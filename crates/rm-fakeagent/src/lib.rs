//! A scripted stand-in for `remote-agent-mac`: same wire protocol, real H.264 video, and the
//! observable behaviour of the test apps (each window title mirrors "[N chars]"). It has two apps,
//! so clients can be tested with several apps and windows at once, an Open panel (Cmd+O) that is
//! a child window, and file uploads. Lets clients be tested without a Mac.

pub mod replay;
pub mod udp_agent;

use openh264::encoder::Encoder;
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use rm_protocol::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const WIDTH: usize = 480;
pub const HEIGHT: usize = 352;
const PANEL: (usize, usize) = (320, 240);
/// the fake Mac's screen
const DESKTOP: (usize, usize) = (960, 600);
pub const UPLOAD_DIR: &str = "/Users/runner/Downloads/RemoteMac Uploads";

/// "desktop" is the whole Mac (one window showing the screen); input on it goes to the window
/// under the pointer, like on the real agent.
const APPS: [(&str, &str); 3] = [("desktop", "Mac Desktop"), ("testapp", "RM Test App"), ("notes", "Notes Test")];

fn caps() -> CapabilityReport {
    let ok = || Capability::Available { detail: "fake agent".into() };
    CapabilityReport { gui_session: ok(), capture: ok(), input: ok(), accessibility: ok(), hardware_encode: Capability::Unavailable { reason: "software".into() } }
}

fn animated_frame(w: usize, h: usize, t: usize, hue: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 4);
    let bar = (t * 6) % w;
    for y in 0..h {
        for x in 0..w {
            let in_bar = y >= h - 24 && (x + w - bar) % w < 40;
            let (r, g, b) = if in_bar { (255, 40, 40) } else { ((x * 255 / w) as u8, (y * 255 / h) as u8, hue) };
            v.extend_from_slice(&[r, g, b, 255]);
        }
    }
    v
}

struct Win {
    app: String,
    title_base: String,
    text: String,
    all_selected: bool,
    role: WindowRole,
    parent: Option<u64>,
    stop: Arc<AtomicBool>,
    video: Option<std::thread::JoinHandle<()>>,
}

struct Upload {
    name: String,
    size: u64,
    data: Vec<u8>,
}

#[derive(Default)]
struct State {
    windows: HashMap<u64, Win>,
    next_id: u64,
    clipboard: String,
    clip_seq: u64,
    uploads: HashMap<u64, Upload>,
    /// remote_path -> size of files that finished uploading
    files: HashMap<String, usize>,
    /// virtual display (points), once the client configured one
    display: Option<(u32, u32)>,
    /// window sizes (last requested), and sizes before fullscreen
    sizes: HashMap<u64, (u32, u32)>,
    before_fullscreen: HashMap<u64, (u32, u32)>,
    /// where each window is (screen points), for the desktop's pointer
    rects: HashMap<u64, Rect>,
    desktop: Option<u64>,
    /// the window that last got a click on the desktop (keyboard focus there)
    front: Option<u64>,
    /// UDP video path (when served through a relay)
    udp: Option<Arc<udp_agent::AgentUdp>>,
    /// windows whose next frame must be a keyframe (the client lost one)
    key_requests: std::collections::HashSet<u64>,
}

type Writer<W> = Arc<Mutex<W>>;

fn send<W: Write>(w: &Writer<W>, m: &Message) -> Result<(), ProtocolError> {
    write_message(&mut *w.lock().unwrap(), m)
}

fn title(base: &str, n: usize) -> String {
    format!("{base} [{n} chars]")
}

/// Typing replaces a select-all selection, like a text view.
fn type_text(w: &mut Win, t: &str) {
    if w.all_selected {
        w.text.clear();
        w.all_selected = false;
    }
    w.text.push_str(t);
}

fn open_window<W: Write + Send + 'static>(
    writer: &Writer<W>,
    st: &Arc<Mutex<State>>,
    app: &str,
    title_base: &str,
    role: WindowRole,
    parent: Option<u64>,
) -> Result<u64, ProtocolError> {
    let (w, h) = if app == "desktop" { DESKTOP } else if role == WindowRole::Window { (WIDTH, HEIGHT) } else { PANEL };
    let stop = Arc::new(AtomicBool::new(false));
    let id = {
        let mut s = st.lock().unwrap();
        s.next_id += 1;
        let id = s.next_id;
        s.windows.insert(id, Win { app: app.into(), title_base: title_base.into(), text: String::new(), all_selected: false, role, parent, stop: stop.clone(), video: None });
        id
    };
    let t = if role == WindowRole::Window { title(title_base, 0) } else { title_base.to_string() };
    let x = if app == "desktop" { 0 } else { 200 + 40 * id as i32 };
    let bounds = Rect { x, y: if app == "desktop" { 0 } else { 216 }, w: w as u32, h: h as u32 };
    st.lock().unwrap().rects.insert(id, bounds);
    if app == "desktop" {
        st.lock().unwrap().desktop = Some(id);
    }
    send(writer, &Message::WindowCreated { window_id: id, application_id: app.into(), title: t, bounds, parent_id: parent, role })?;
    let wr = writer.clone();
    let hue = (60 * id % 256) as u8;
    let st2 = st.clone();
    let handle = std::thread::spawn(move || video_loop(wr, st2, id, w, h, hue, stop));
    st.lock().unwrap().windows.get_mut(&id).unwrap().video = Some(handle);
    Ok(id)
}

fn close_window<W: Write>(writer: &Writer<W>, st: &Arc<Mutex<State>>, id: u64) -> Result<(), ProtocolError> {
    // children first, like the window server would
    let children: Vec<u64> = st.lock().unwrap().windows.iter().filter(|(_, w)| w.parent == Some(id)).map(|(k, _)| *k).collect();
    for c in children {
        close_window(writer, st, c)?;
    }
    {
        let mut s = st.lock().unwrap();
        s.rects.remove(&id);
        if s.desktop == Some(id) {
            s.desktop = None;
        }
        if s.front == Some(id) {
            s.front = None;
        }
    }
    let Some(mut w) = st.lock().unwrap().windows.remove(&id) else { return Ok(()) };
    w.stop.store(true, Ordering::SeqCst);
    if let Some(v) = w.video.take() {
        let _ = v.join();
    }
    send(writer, &Message::WindowDestroyed { window_id: id })?;
    let app_still_open = st.lock().unwrap().windows.values().any(|x| x.app == w.app);
    if !app_still_open {
        send(writer, &Message::AppExited { application_id: w.app, code: None })?;
    }
    Ok(())
}

fn retitle<W: Write>(writer: &Writer<W>, st: &Arc<Mutex<State>>, id: u64) -> Result<(), ProtocolError> {
    let t = st.lock().unwrap().windows.get(&id).map(|w| title(&w.title_base, w.text.chars().count()));
    if let Some(title) = t {
        send(writer, &Message::WindowTitleChanged { window_id: id, title })?;
    }
    Ok(())
}

fn menu_item(title: &str, shortcut: &str) -> MenuNode {
    MenuNode { title: title.into(), enabled: true, shortcut: Some(shortcut.into()), ..Default::default() }
}

/// The test apps' menu bar (the Apple menu is never reported).
pub fn menu_bar(app_name: &str) -> Vec<MenuNode> {
    let top = |t: &str, children| MenuNode { title: t.into(), enabled: true, children, ..Default::default() };
    vec![
        top(app_name, vec![menu_item(&format!("About {app_name}"), "Cmd+I")]),
        top("File", vec![menu_item("Open…", "Cmd+O"), MenuNode { separator: true, ..Default::default() }, menu_item("Close Window", "Cmd+W")]),
        top("Edit", vec![menu_item("Select All", "Cmd+A"), menu_item("Copy", "Cmd+C"), menu_item("Paste", "Cmd+V")]),
    ]
}

/// The key a menu item's shortcut presses.
fn menu_key(path: &[u32]) -> Option<&'static str> {
    Some(match path {
        [0, 0] => "KeyI",
        [1, 0] => "KeyO",
        [1, 2] => "KeyW",
        [2, 0] => "KeyA",
        [2, 1] => "KeyC",
        [2, 2] => "KeyV",
        _ => return None,
    })
}

/// Serve one client.
pub fn serve<S: Read + Write + Send + 'static>(reader: S, writer: S) -> Result<(), ProtocolError> {
    serve_with(reader, writer, None)
}

/// [`serve`], sending video over `udp` while the client's reports come in.
pub fn serve_with<S: Read + Write + Send + 'static>(mut reader: S, writer: S, udp: Option<Arc<udp_agent::AgentUdp>>) -> Result<(), ProtocolError> {
    let writer: Writer<S> = Arc::new(Mutex::new(writer));
    match read_message(&mut reader)? {
        Some(Message::ClientHello(_)) => {}
        _ => return Ok(()),
    }
    send(&writer, &Message::ServerHello(Hello::ours("rm-fakeagent", &["h264"], &["control", "video", "files"])))?;
    send(&writer, &Message::CapabilityReport(caps()))?;
    let st = Arc::new(Mutex::new(State { udp, ..Default::default() }));

    while let Some(msg) = read_message(&mut reader)? {
        // A menu command does what its keyboard shortcut does, in the app's main window.
        let msg = match msg {
            Message::MenuInvoke { application_id, path } => {
                let main = st.lock().unwrap().windows.iter().find(|(_, w)| w.app == application_id && w.role == WindowRole::Window).map(|(k, _)| *k);
                match (main, menu_key(&path)) {
                    (Some(window_id), Some(key)) => Message::Key { window_id, physical_key: key.into(), modifiers: vec![Modifier::Command], down: true },
                    _ => Message::Error { code: "menu_invoke_failed".into(), message: format!("{application_id} {path:?}") },
                }
            }
            // Mac Desktop: a click picks the window under the pointer; keys go to it
            Message::MouseButton { window_id, down: true, x, y, .. } if st.lock().unwrap().desktop == Some(window_id) => {
                let mut s = st.lock().unwrap();
                let hit = s.rects.iter().filter(|(id, _)| Some(**id) != s.desktop).find(|(_, r)| x >= r.x as f64 && y >= r.y as f64 && x < (r.x + r.w as i32) as f64 && y < (r.y + r.h as i32) as f64).map(|(id, _)| *id);
                s.front = hit;
                continue;
            }
            Message::TextInput { window_id, text } if st.lock().unwrap().desktop == Some(window_id) => match st.lock().unwrap().front {
                Some(front) => Message::TextInput { window_id: front, text },
                None => continue,
            },
            Message::Key { window_id, physical_key, modifiers, down } if st.lock().unwrap().desktop == Some(window_id) => match st.lock().unwrap().front {
                Some(front) => Message::Key { window_id: front, physical_key, modifiers, down },
                None => continue,
            },
            m => m,
        };
        match msg {
            Message::GetMenuBar { application_id } => {
                let name = APPS.iter().find(|a| a.0 == application_id).map(|a| a.1).unwrap_or("App");
                send(&writer, &Message::MenuBar { application_id, menus: menu_bar(name) })?;
            }
            Message::ListApps => {
                let apps = APPS.iter().map(|(id, name)| AppInfo { id: (*id).into(), name: (*name).into(), available: true, version: None }).collect();
                send(&writer, &Message::Apps { apps })?;
            }
            Message::AppLaunch { application_id, .. } => match APPS.iter().find(|(id, _)| *id == application_id) {
                Some((id, name)) => {
                    send(&writer, &Message::AppLaunched { application_id: (*id).into(), pid: 4000 + APPS.iter().position(|a| a.0 == *id).unwrap() as u32 })?;
                    open_window(&writer, &st, id, name, WindowRole::Window, None)?;
                }
                None => send(&writer, &Message::Error { code: "launch_rejected".into(), message: format!("unknown application '{application_id}'") })?,
            },
            Message::TextInput { window_id, text } => {
                if let Some(w) = st.lock().unwrap().windows.get_mut(&window_id) {
                    type_text(w, &text);
                }
                retitle(&writer, &st, window_id)?;
            }
            Message::ClipboardSet { text, .. } => st.lock().unwrap().clipboard = text,
            Message::GetAppIcon { application_id } => {
                let size = 64u32;
                let tint = if application_id == "notes" { 60 } else { 200 };
                let px: Vec<u8> = (0..size * size).flat_map(|i| [(i % size * 4) as u8, (i / size * 4) as u8, tint, 255]).collect();
                send(&writer, &Message::AppIcon { application_id, size, rgba_base64: base64_encode(&px) })?;
            }
            Message::Key { window_id, physical_key, modifiers, down: true } => {
                let cmd = modifiers.contains(&Modifier::Command);
                enum After { Nothing, Copied(u64, String), OpenPanel(String), Dialog(String), Close }
                let after = {
                    let mut s = st.lock().unwrap();
                    let clip = s.clipboard.clone();
                    let mut after = After::Nothing;
                    if let Some(w) = s.windows.get_mut(&window_id) {
                        match (physical_key.as_str(), cmd) {
                            ("KeyA", true) => w.all_selected = true,
                            ("KeyC", true) => after = After::Copied(0, w.text.clone()),
                            ("KeyV", true) => type_text(w, &clip),
                            ("KeyO", true) if w.role == WindowRole::Window => after = After::OpenPanel(w.app.clone()),
                            ("KeyI", true) if w.role == WindowRole::Window => after = After::Dialog(w.app.clone()),
                            ("KeyW", true) => after = After::Close,
                            ("Backspace", _) => {
                                if w.all_selected { w.text.clear() } else { w.text.pop(); }
                                w.all_selected = false
                            }
                            (k, false) if k.starts_with("Key") && k.len() == 4 => type_text(w, &k[3..].to_ascii_lowercase()),
                            _ => {}
                        }
                    }
                    if let After::Copied(_, t) = &after {
                        s.clip_seq += 1;
                        s.clipboard = t.clone();
                        after = After::Copied(s.clip_seq, t.clone());
                    }
                    after
                };
                match after {
                    After::Copied(seq, text) => send(&writer, &Message::ClipboardSet { seq, text })?,
                    After::OpenPanel(app) => {
                        open_window(&writer, &st, &app, "Open", WindowRole::OpenPanel, Some(window_id))?;
                    }
                    After::Dialog(app) => {
                        open_window(&writer, &st, &app, "About", WindowRole::Dialog, Some(window_id))?;
                    }
                    After::Close => close_window(&writer, &st, window_id)?,
                    After::Nothing => {}
                }
                retitle(&writer, &st, window_id)?;
            }
            Message::FileUploadBegin { transfer_id, name, size } => {
                if size > MAX_UPLOAD {
                    send(&writer, &Message::FileUploadFailed { transfer_id, reason: "file too large".into() })?;
                } else {
                    st.lock().unwrap().uploads.insert(transfer_id, Upload { name: sanitize_upload_name(&name), size, data: Vec::new() });
                }
            }
            Message::FileUploadChunk { transfer_id, offset, data_base64 } => {
                let bad = {
                    let mut s = st.lock().unwrap();
                    match (s.uploads.get_mut(&transfer_id), base64_decode(&data_base64)) {
                        (Some(u), Ok(bytes)) if offset == u.data.len() as u64 && u.data.len() as u64 + bytes.len() as u64 <= u.size => {
                            u.data.extend(bytes);
                            None
                        }
                        (Some(_), _) => Some("out-of-order or oversized chunk"),
                        (None, _) => Some("unknown transfer"),
                    }
                };
                if let Some(reason) = bad {
                    st.lock().unwrap().uploads.remove(&transfer_id);
                    send(&writer, &Message::FileUploadFailed { transfer_id, reason: reason.into() })?;
                }
            }
            Message::FileUploadEnd { transfer_id } => {
                let done = st.lock().unwrap().uploads.remove(&transfer_id);
                match done {
                    Some(u) if u.data.len() as u64 == u.size => {
                        let remote_path = format!("{UPLOAD_DIR}/{}", u.name);
                        st.lock().unwrap().files.insert(remote_path.clone(), u.data.len());
                        send(&writer, &Message::FileUploaded { transfer_id, remote_path })?;
                    }
                    _ => send(&writer, &Message::FileUploadFailed { transfer_id, reason: "incomplete".into() })?,
                }
            }
            Message::PanelChooseFile { window_id, remote_path } => {
                let (parent, size) = {
                    let s = st.lock().unwrap();
                    (s.windows.get(&window_id).filter(|w| w.role == WindowRole::OpenPanel).and_then(|w| w.parent), s.files.get(&remote_path).copied())
                };
                if let (Some(p), Some(size)) = (parent, size) {
                    close_window(&writer, &st, window_id)?;
                    let name = remote_path.rsplit('/').next().unwrap_or("").to_string();
                    let t = st.lock().unwrap().windows.get(&p).map(|w| format!("{} [opened {name} {size} bytes]", w.title_base));
                    if let Some(title) = t {
                        send(&writer, &Message::WindowTitleChanged { window_id: p, title })?;
                    }
                } else {
                    send(&writer, &Message::Error { code: "panel_choose_failed".into(), message: format!("{window_id} {remote_path}") })?;
                }
            }
            Message::PanelCancel { window_id } => close_window(&writer, &st, window_id)?,
            Message::WindowResizeRequest { window_id, width, height } => {
                st.lock().unwrap().sizes.insert(window_id, (width, height));
                st.lock().unwrap().rects.insert(window_id, Rect { x: 200, y: 216, w: width, h: height });
                send(&writer, &Message::WindowMoved { window_id, bounds: Rect { x: 200, y: 216, w: width, h: height } })?;
            }
            Message::WindowClose { window_id } => close_window(&writer, &st, window_id)?,
            Message::DisplayConfigure { width, height, scale } => {
                let (w, h) = (width / scale.max(1), height / scale.max(1));
                st.lock().unwrap().display = Some((w, h));
                send(&writer, &Message::DisplayStatus { available: true, display_id: 77, width: w, height: h, reason: None })?;
            }
            Message::WindowFullscreen { window_id, on } => {
                let (w, h) = {
                    let mut s = st.lock().unwrap();
                    let current = s.sizes.get(&window_id).copied().unwrap_or((WIDTH as u32, HEIGHT as u32));
                    if on {
                        s.before_fullscreen.insert(window_id, current);
                        s.display.unwrap_or((1440, 900))
                    } else {
                        s.before_fullscreen.remove(&window_id).unwrap_or(current)
                    }
                };
                st.lock().unwrap().sizes.insert(window_id, (w, h));
                // the window now has that size: its video restarts at it, as the real agent's does
                let old = st.lock().unwrap().windows.get_mut(&window_id).map(|win| {
                    win.stop.store(true, Ordering::SeqCst);
                    let fresh = Arc::new(AtomicBool::new(false));
                    win.stop = fresh.clone();
                    (win.video.take(), fresh)
                });
                if let Some((handle, stop)) = old {
                    if let Some(hd) = handle {
                        let _ = hd.join();
                    }
                    let bounds = Rect { x: if on { 1920 } else { 200 }, y: if on { 25 } else { 216 }, w, h };
                    st.lock().unwrap().rects.insert(window_id, bounds);
                    send(&writer, &Message::WindowMoved { window_id, bounds })?;
                    let wr = writer.clone();
                    let hue = (60 * window_id % 256) as u8;
                    let st2 = st.clone();
                    let handle = std::thread::spawn(move || video_loop(wr, st2, window_id, w as usize, h as usize, hue, stop));
                    if let Some(win) = st.lock().unwrap().windows.get_mut(&window_id) {
                        win.video = Some(handle);
                    }
                }
            }
            Message::AppTerminate { application_id } => {
                let ids: Vec<u64> = st.lock().unwrap().windows.iter().filter(|(_, w)| w.app == application_id && w.parent.is_none()).map(|(k, _)| *k).collect();
                for id in ids {
                    close_window(&writer, &st, id)?;
                }
            }
            Message::Ping { nonce } => send(&writer, &Message::Pong { nonce })?,
            Message::RequestKeyframe { window_id } => {
                st.lock().unwrap().key_requests.insert(window_id);
            }
            _ => {}
        }
    }
    let ids: Vec<u64> = st.lock().unwrap().windows.keys().copied().collect();
    for id in ids {
        if let Some(mut w) = st.lock().unwrap().windows.remove(&id) {
            w.stop.store(true, Ordering::SeqCst);
            if let Some(v) = w.video.take() {
                let _ = v.join();
            }
        }
    }
    Ok(())
}

fn video_loop<W: Write>(w: Writer<W>, st: Arc<Mutex<State>>, id: u64, width: usize, height: usize, hue: u8, stop: Arc<AtomicBool>) {
    let Ok(mut enc) = Encoder::new() else { return };
    let mut t = 0usize;
    let mut on_udp = false;
    while !stop.load(Ordering::SeqCst) {
        let (udp, asked) = {
            let mut s = st.lock().unwrap();
            (s.udp.clone().filter(|u| u.alive()), s.key_requests.remove(&id))
        };
        // a keyframe when asked, and when switching transport (the client resyncs on it)
        if asked || udp.is_some() != on_udp {
            enc.force_intra_frame();
        }
        on_udp = udp.is_some();
        let rgba = animated_frame(width, height, t, hue);
        let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (width, height)));
        let Ok(bs) = enc.encode(&yuv) else { continue };
        let data = bs.to_vec();
        let keyframe = data.windows(5).any(|x| x[..4] == [0, 0, 0, 1] && x[4] & 0x1f == 5);
        if !data.is_empty() {
            let f = VideoFrame { window_id: id, pts_us: udp_agent::clock_us(), keyframe, codec: CODEC_H264, width: width as u16, height: height as u16, data };
            if let Some(u) = &udp {
                u.send(&f);
            } else {
                let Ok(bytes) = encode_video(&f) else { continue };
                if w.lock().unwrap().write_all(&bytes).is_err() {
                    return;
                }
            }
        }
        t += 1;
        std::thread::sleep(Duration::from_millis(33));
    }
}

/// Bind to a relay as the agent and serve one client.
pub fn serve_via_relay(relay: &str, session: &str, token: &str) -> Result<(), String> {
    let s = rm_relay::join(relay, session, rm_relay::Role::Agent, token).map_err(|e| e.to_string())?;
    let w: TcpStream = s.try_clone().map_err(|e| e.to_string())?;
    // UDP video unless RM_NO_UDP is set (tests of the TCP path)
    let udp = if std::env::var_os("RM_NO_UDP").is_some() { None } else { udp_agent::AgentUdp::start(relay, session, token).ok() };
    serve_with(s, w, udp).map_err(|e| e.to_string())
}
