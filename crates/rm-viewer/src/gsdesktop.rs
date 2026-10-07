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
use rm_protocol::Message;
use std::sync::{Arc, Mutex};

struct State {
    /// which start this is (frames of an earlier session are dropped)
    session: u64,
    window_id: u64,
    tunnel: Arc<ClientTunnel>,
    /// the desktop's size in Mac points (pointer positions are in this space)
    points: (u32, u32),
    /// picture size, from the stream's SPS (for the decoder's crop)
    pixels: (u16, u16),
    /// GameStream's pictures are arriving: the usual ones of this window are dropped
    streaming: bool,
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

/// Whether GameStream carries `window_id`'s picture now (its usual frames are then dropped).
pub fn owns(window_id: u64) -> bool {
    STATE.lock().unwrap().as_ref().is_some_and(|s| s.window_id == window_id && s.streaming)
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

/// What the Moonlight thread does, in order: a stop always ends the session before it before the
/// next one connects (moonlight-common-c keeps one connection per process).
enum Op {
    Connect { session: u64, params: moonlight::Params, sink: Box<dyn Fn(rm_protocol::VideoFrame) + Send>, on_streaming: Box<dyn FnOnce() + Send> },
    Stop,
}

static OPS: Mutex<Option<std::sync::mpsc::Sender<Op>>> = Mutex::new(None);
static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn ops() -> Option<std::sync::mpsc::Sender<Op>> {
    let mut g = OPS.lock().unwrap();
    if g.is_none() {
        let (tx, rx) = std::sync::mpsc::channel::<Op>();
        std::thread::Builder::new().name("rm-moonlight".into()).spawn(move || run_ops(rx)).ok()?;
        *g = Some(tx);
    }
    g.clone()
}

fn run_ops(rx: std::sync::mpsc::Receiver<Op>) {
    let mut connected = false;
    for op in rx {
        match op {
            Op::Stop => {
                if std::mem::take(&mut connected) {
                    moonlight::stop();
                }
            }
            Op::Connect { session, params, sink, on_streaming } => {
                // a stop already queued for this session: do not connect at all
                if STATE.lock().unwrap().as_ref().map(|s| s.session) != Some(session) {
                    continue;
                }
                let on_streaming = Mutex::new(Some(on_streaming));
                let r = moonlight::connect(params, move |data, idr| {
                    let (id, (w, h), first) = {
                        let mut st = STATE.lock().unwrap();
                        let Some(s) = st.as_mut().filter(|s| s.session == session) else { return };
                        // GameStream takes over at a keyframe (the decoder starts there)
                        if !s.streaming && !idr {
                            return;
                        }
                        let first = !s.streaming;
                        s.streaming = true;
                        // the picture size comes with every IDR's SPS (GameStream sends no size)
                        if idr {
                            if let Some((w, h)) = rm_gamestream::sps::h264_size(&data) {
                                s.pixels = (w.min(65535) as u16, h.min(65535) as u16);
                            }
                        }
                        (s.window_id, s.pixels, first)
                    };
                    if first {
                        eprintln!("Mac Desktop: GameStream pictures arriving; it carries the desktop now");
                        if let Some(f) = on_streaming.lock().unwrap().take() {
                            f();
                        }
                    }
                    sink(rm_protocol::VideoFrame { window_id: id, pts_us: 0, keyframe: idr, codec: rm_protocol::CODEC_H264, width: w, height: h, data });
                });
                match r {
                    Ok(()) => {
                        connected = true;
                        eprintln!("Mac Desktop streams over GameStream (Moonlight's client core)");
                    }
                    Err(e) => {
                        // the desktop keeps its usual stream
                        eprintln!("Mac Desktop GameStream: could not connect ({e}); the desktop stays on the usual stream");
                        let mut st = STATE.lock().unwrap();
                        if st.as_ref().is_some_and(|s| s.session == session) {
                            st.take();
                        }
                    }
                }
            }
        }
    }
}

/// Start Moonlight's client for the Mac Desktop window `window_id` (`points` in Mac points,
/// `pixels` the expected picture size). `to_host` carries the tunnel; `sink` gets frames;
/// `on_streaming` is called once, at the first picture (the Mac then stops the usual stream,
/// which shows the desktop until then).
pub fn start(window_id: u64, key: [u8; 16], points: (u32, u32), pixels: (u16, u16), to_host: impl Fn(ToHost) + Send + Sync + 'static, sink: impl Fn(rm_protocol::VideoFrame) + Send + 'static, on_streaming: impl FnOnce() + Send + 'static) -> Result<(), String> {
    // a session left over (its window went away while it was still connecting) ends first
    stop();
    let ops = ops().ok_or("no thread for Moonlight")?;
    let tunnel = ClientTunnel::start(Arc::new(to_host)).map_err(|e| e.to_string())?;
    let port = tunnel.rtsp_port;
    let session = NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    *STATE.lock().unwrap() = Some(State { session, window_id, tunnel, points, pixels, streaming: false });
    let st = crate::settings::Settings::load();
    let bitrate = if st.bitrate_mbps > 0 { st.bitrate_mbps * 1000 } else { 40_000 };
    let params = moonlight::Params { rtsp_port: port, key, width: pixels.0 as u32, height: pixels.1 as u32, fps: st.fps, bitrate_kbps: bitrate, packet_size: 1200, remote: true };
    ops.send(Op::Connect { session, params, sink: Box::new(sink), on_streaming: Box::new(on_streaming) }).map_err(|e| e.to_string())
}

/// End the session. Never blocks the caller: a connect in progress is interrupted, and the stop
/// runs on the Moonlight thread, after it.
pub fn stop() {
    if STATE.lock().unwrap().take().is_some() {
        moonlight::interrupt();
        if let Some(ops) = ops() {
            let _ = ops.send(Op::Stop);
        }
    }
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
            // and over RemoteMac's own input path too (resent after 40 ms, not after ENet's
            // retransmission timeout): a press and release over a lossy link otherwise reach the
            // Mac far apart and the Dock takes the click for a press-and-hold (Options / Quit).
            // The Mac acts on whichever copy of each press and release comes first.
            return false;
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
        // keys and text go over RemoteMac's own input path, as for app windows: every key there
        // (GameStream's key codes leave some out), resent after 40 ms, typed by the Mac as is
        Message::Key { window_id, .. } | Message::TextInput { window_id, .. } if *window_id == id => return false,
        Message::RequestKeyframe { window_id } if *window_id == id => moonlight::request_idr(),
        _ => return false,
    }
    true
}
