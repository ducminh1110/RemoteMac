//! A scripted stand-in for `remote-agent-mac`: same wire protocol, a synthetic animated
//! window encoded with a real H.264 encoder, and the observable behaviour of the test app
//! (its title mirrors "[N chars]"). Lets clients be tested without a Mac.

use openh264::encoder::Encoder;
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use rm_protocol::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const WIDTH: usize = 480;
pub const HEIGHT: usize = 352;
const WINDOW_ID: u64 = 1;

fn caps() -> CapabilityReport {
    let ok = || Capability::Available { detail: "fake agent".into() };
    CapabilityReport { gui_session: ok(), capture: ok(), input: ok(), accessibility: ok(), hardware_encode: Capability::Unavailable { reason: "software".into() } }
}

fn animated_frame(t: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(WIDTH * HEIGHT * 4);
    let bar = (t * 6) % WIDTH;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let in_bar = y >= HEIGHT - 24 && (x + WIDTH - bar) % WIDTH < 40;
            let (r, g, b) = if in_bar { (255, 40, 40) } else { ((x * 255 / WIDTH) as u8, (y * 255 / HEIGHT) as u8, 200) };
            v.extend_from_slice(&[r, g, b, 255]);
        }
    }
    v
}

struct State {
    chars: usize,
    all_selected: bool,
    window_open: bool,
}

type Writer<W> = Arc<Mutex<W>>;

fn send<W: Write>(w: &Writer<W>, m: &Message) -> Result<(), ProtocolError> {
    write_message(&mut *w.lock().unwrap(), m)
}

/// Serve one client. `clone` produces a second handle for the video thread.
pub fn serve<S: Read + Write + Send + 'static>(mut reader: S, writer: S) -> Result<(), ProtocolError> {
    let writer: Writer<S> = Arc::new(Mutex::new(writer));
    match read_message(&mut reader)? {
        Some(Message::ClientHello(_)) => {}
        _ => return Ok(()),
    }
    send(&writer, &Message::ServerHello(Hello::ours("rm-fakeagent", &["h264"], &["control", "video"])))?;
    send(&writer, &Message::CapabilityReport(caps()))?;

    let state = Arc::new(Mutex::new(State { chars: 0, all_selected: false, window_open: false }));
    let stop = Arc::new(AtomicBool::new(false));
    let mut video: Option<std::thread::JoinHandle<()>> = None;

    let title = |n: usize| format!("RM Test App [{n} chars]");
    while let Some(msg) = read_message(&mut reader)? {
        match msg {
            Message::ListApps => send(&writer, &Message::Apps { apps: vec![AppInfo { id: "testapp".into(), name: "RM Test App".into(), available: true, version: None }] })?,
            Message::AppLaunch { application_id, .. } if application_id == "testapp" => {
                send(&writer, &Message::AppLaunched { application_id, pid: 4242 })?;
                state.lock().unwrap().window_open = true;
                send(&writer, &Message::WindowCreated { window_id: WINDOW_ID, application_id: "testapp".into(), title: title(0),
                    bounds: Rect { x: 200, y: 216, w: WIDTH as u32, h: HEIGHT as u32 }, parent_id: None })?;
                let (w, st, stop2) = (writer.clone(), state.clone(), stop.clone());
                video = Some(std::thread::spawn(move || video_loop(w, st, stop2)));
            }
            Message::AppLaunch { application_id, .. } => send(&writer, &Message::Error { code: "launch_rejected".into(), message: format!("unknown application '{application_id}'") })?,
            Message::TextInput { text, .. } => {
                let n = { let mut s = state.lock().unwrap(); s.chars += text.chars().count(); s.all_selected = false; s.chars };
                send(&writer, &Message::WindowTitleChanged { window_id: WINDOW_ID, title: title(n) })?;
            }
            Message::Key { physical_key, modifiers, down: true, .. } => {
                let cmd = modifiers.contains(&Modifier::Command);
                let n = {
                    let mut s = state.lock().unwrap();
                    match (physical_key.as_str(), cmd) {
                        ("KeyA", true) => s.all_selected = true,
                        ("KeyA", false) => { s.chars += 1; s.all_selected = false }
                        ("Backspace", _) => { if s.all_selected { s.chars = 0 } else { s.chars = s.chars.saturating_sub(1) } s.all_selected = false }
                        _ => {}
                    }
                    s.chars
                };
                send(&writer, &Message::WindowTitleChanged { window_id: WINDOW_ID, title: title(n) })?;
            }
            Message::WindowResizeRequest { window_id, width, height } => {
                send(&writer, &Message::WindowMoved { window_id, bounds: Rect { x: 200, y: 216, w: width, h: height } })?;
            }
            Message::WindowClose { .. } | Message::AppTerminate { .. } => close_window(&writer, &state, &stop, &mut video)?,
            Message::Ping { nonce } => send(&writer, &Message::Pong { nonce })?,
            _ => {}
        }
    }
    stop.store(true, Ordering::SeqCst);
    if let Some(v) = video { let _ = v.join(); }
    Ok(())
}

fn close_window<W: Write>(w: &Writer<W>, st: &Arc<Mutex<State>>, stop: &Arc<AtomicBool>, video: &mut Option<std::thread::JoinHandle<()>>) -> Result<(), ProtocolError> {
    if !std::mem::replace(&mut st.lock().unwrap().window_open, false) { return Ok(()); }
    stop.store(true, Ordering::SeqCst);
    if let Some(v) = video.take() { let _ = v.join(); }
    send(w, &Message::WindowDestroyed { window_id: WINDOW_ID })?;
    send(w, &Message::AppExited { application_id: "testapp".into(), code: None })
}

fn video_loop<W: Write>(w: Writer<W>, st: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    let Ok(mut enc) = Encoder::new() else { return };
    let start = Instant::now();
    let mut t = 0usize;
    while !stop.load(Ordering::SeqCst) && st.lock().unwrap().window_open {
        let rgba = animated_frame(t);
        let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (WIDTH, HEIGHT)));
        let Ok(bs) = enc.encode(&yuv) else { continue };
        let data = bs.to_vec();
        let keyframe = data.windows(5).any(|x| x[..4] == [0, 0, 0, 1] && x[4] & 0x1f == 5);
        if !data.is_empty() {
            let f = VideoFrame { window_id: WINDOW_ID, pts_us: start.elapsed().as_micros() as u64, keyframe, codec: CODEC_H264, width: WIDTH as u16, height: HEIGHT as u16, data };
            let Ok(bytes) = encode_video(&f) else { continue };
            if w.lock().unwrap().write_all(&bytes).is_err() { return; }
        }
        t += 1;
        std::thread::sleep(Duration::from_millis(33));
    }
}

/// Bind to a relay as the agent and serve one client.
pub fn serve_via_relay(relay: &str, session: &str, token: &str) -> Result<(), String> {
    let s = rm_relay::join(relay, session, rm_relay::Role::Agent, token).map_err(|e| e.to_string())?;
    let w: TcpStream = s.try_clone().map_err(|e| e.to_string())?;
    serve(s, w).map_err(|e| e.to_string())
}
