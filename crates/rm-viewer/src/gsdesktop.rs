//! Mac Desktop in full GameStream mode: Moonlight's own client core (moonlight-common-c)
//! streams the Mac's screen, exactly as Moonlight does with Sunshine — RTSP handshake,
//! encrypted ENet control and input, RTP video with FEC. It reaches the Mac through
//! [`ClientTunnel`]: RTSP over RemoteMac's authenticated TCP link, the UDP flows over the
//! NAT-punched UDP path. Decoded pictures take the viewer's usual decode and present path.
//!
//! moonlight-common-c keeps one connection per process: one Mac Desktop at a time.

use rm_gamestream::moonlight;
use rm_gamestream::tunnel::{ClientTunnel, ToHost};
use rm_gamestream::Input;
use rm_protocol::{Message, Modifier};
use std::sync::{Arc, Mutex};

struct State {
    window_id: u64,
    tunnel: Arc<ClientTunnel>,
    /// the desktop's size in Mac points (pointer positions are in this space)
    points: (u32, u32),
    /// picture size, from the stream's SPS (for the decoder's crop)
    pixels: (u16, u16),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Full GameStream for the Mac Desktop unless RM_GAMESTREAM=0.
pub fn enabled() -> bool {
    std::env::var("RM_GAMESTREAM").ok().as_deref() != Some("0")
}

/// A fresh session key, and as 32 hex digits for the launch argument.
pub fn new_key() -> ([u8; 16], String) {
    let k = rm_protocol::udp::random_secret();
    (k, rm_protocol::udp::hex(&k))
}

pub fn active_window() -> Option<u64> {
    STATE.lock().unwrap().as_ref().map(|s| s.window_id)
}

/// RTSP bytes from the Mac.
pub fn from_host_tcp(id: u32, op: &str, data: &[u8]) {
    let t = STATE.lock().unwrap().as_ref().map(|s| s.tunnel.clone());
    if let Some(t) = t {
        match op {
            "data" => t.tcp_from_host(id, data),
            "close" => t.tcp_close_from_host(id),
            _ => {}
        }
    }
}

/// A GameStream datagram from the Mac.
pub fn from_host_udp(flow: u8, data: &[u8]) {
    let t = STATE.lock().unwrap().as_ref().map(|s| s.tunnel.clone());
    if let Some(t) = t {
        t.udp_from_host(flow, data);
    }
}

/// Start Moonlight's client for the Mac Desktop window `window_id` (`points` in Mac points,
/// `pixels` the expected picture size). `to_host` carries the tunnel; `sink` gets frames.
pub fn start(window_id: u64, key: [u8; 16], points: (u32, u32), pixels: (u16, u16), to_host: impl Fn(ToHost) + Send + Sync + 'static, sink: impl Fn(rm_protocol::VideoFrame) + Send + 'static) -> Result<(), String> {
    if STATE.lock().unwrap().is_some() {
        return Err("a Mac Desktop GameStream session is already running".into());
    }
    let tunnel = ClientTunnel::start(Arc::new(to_host)).map_err(|e| e.to_string())?;
    let port = tunnel.rtsp_port;
    *STATE.lock().unwrap() = Some(State { window_id, tunnel, points, pixels });
    std::thread::Builder::new()
        .name("rm-moonlight".into())
        .spawn(move || {
            let st = crate::settings::Settings::load();
            let bitrate = if st.bitrate_mbps > 0 { st.bitrate_mbps * 1000 } else { 40_000 };
            let params = moonlight::Params { rtsp_port: port, key, width: pixels.0 as u32, height: pixels.1 as u32, fps: st.fps, bitrate_kbps: bitrate, packet_size: 1200, remote: true };
            let r = moonlight::connect(params, move |data, idr| {
                let (id, (w, h)) = {
                    let mut st = STATE.lock().unwrap();
                    let Some(s) = st.as_mut() else { return };
                    // the picture size comes with every IDR's SPS (GameStream sends no size)
                    if idr {
                        if let Some((w, h)) = rm_gamestream::sps::h264_size(&data) {
                            s.pixels = (w.min(65535) as u16, h.min(65535) as u16);
                        }
                    }
                    (s.window_id, s.pixels)
                };
                sink(rm_protocol::VideoFrame { window_id: id, pts_us: 0, keyframe: idr, codec: rm_protocol::CODEC_H264, width: w, height: h, data });
            });
            match r {
                Ok(()) => eprintln!("Mac Desktop streams over GameStream (Moonlight's client core)"),
                Err(e) => {
                    eprintln!("Mac Desktop GameStream: could not connect ({e})");
                    STATE.lock().unwrap().take();
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn stop() {
    if STATE.lock().unwrap().take().is_some() {
        moonlight::stop();
    }
}

fn vk_for(name: &str) -> Option<u16> {
    (0u16..256).find(|v| rm_gamestream::tunnel::vk_name(*v) == Some(name))
}

/// GameStream modifier bits (shift 1, ctrl 2, alt 4, meta 8): Command travels as meta.
fn mods(m: &[Modifier]) -> u8 {
    m.iter().map(|x| match x {
        Modifier::Shift => 1,
        Modifier::Control => 2,
        Modifier::Option => 4,
        Modifier::Command => 8,
        _ => 0,
    }).fold(0, |a, b| a | b)
}

/// Input for the GameStream desktop goes through Moonlight's input stream (ENet). True when
/// `m` was taken.
pub fn intercept(m: &Message) -> bool {
    let (id, (pw, ph)) = match STATE.lock().unwrap().as_ref() {
        Some(s) => (s.window_id, s.points),
        None => return false,
    };
    let clamp = |v: f64| v.round().clamp(0.0, 32767.0) as i16;
    let at = |x: f64, y: f64| Input::MouseAbs { x: clamp(x), y: clamp(y), width: pw.min(32767) as i16, height: ph.min(32767) as i16 };
    match m {
        Message::MouseMove { window_id, x, y } if *window_id == id => moonlight::send_input(&at(*x, *y)),
        Message::MouseButton { window_id, button, down, x, y } if *window_id == id => {
            moonlight::send_input(&at(*x, *y));
            let button = match button {
                rm_protocol::MouseButton::Left => 1,
                rm_protocol::MouseButton::Middle => 2,
                rm_protocol::MouseButton::Right => 3,
            };
            moonlight::send_input(&Input::Button { button, down: *down });
        }
        // the viewer counts 40 px per wheel notch; GameStream 120 per notch
        Message::Scroll { window_id, dx, dy } if *window_id == id => {
            if *dy != 0.0 {
                moonlight::send_input(&Input::Scroll { amount: (dy * 3.0).round().clamp(-32768.0, 32767.0) as i16 });
            }
            if *dx != 0.0 {
                moonlight::send_input(&Input::HScroll { amount: (dx * 3.0).round().clamp(-32768.0, 32767.0) as i16 });
            }
        }
        Message::Key { window_id, physical_key, modifiers, down } if *window_id == id => {
            if let Some(vk) = vk_for(physical_key) {
                moonlight::send_input(&Input::Key { vk, down: *down, modifiers: mods(modifiers) });
            }
        }
        Message::TextInput { window_id, text } if *window_id == id => moonlight::send_input(&Input::Text(text.clone())),
        Message::RequestKeyframe { window_id } if *window_id == id => moonlight::request_idr(),
        _ => return false,
    }
    true
}
