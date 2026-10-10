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
/// The Dock's window id and size (as the Mac's: Fusion.swift).
const DOCK_ID: u64 = 0x7FFF_0002;
const DOCK: (usize, usize) = (480, 64);
/// the Mac's menu bar, as Desktop Fusion's exact windows stream it (scripted strip)
const MENUBAR_ID: u64 = 0x7FFF_0003;
const MENUBAR: (usize, usize) = (1280, 24);

const APPS: [(&str, &str); 4] = [("desktop", "Mac Desktop"), ("testapp", "RM Test App"), ("notes", "Notes Test"), ("textedit", "TextEdit")];

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
    /// the Mac Desktop in full GameStream mode: its host session behind the tunnel
    gs: Option<Arc<rm_gamestream::tunnel::HostTunnel>>,
    /// the viewer shows GameStream's pictures: the desktop goes only that way (until then also
    /// the usual way, as the Mac app does)
    gs_ready: bool,
    /// sound is being sent (a 440 Hz tone): set to stop it
    audio: Option<Arc<AtomicBool>>,
    /// the Dock is streamed: set to stop it
    dock: Option<Arc<AtomicBool>>,
    /// windows are shown as the Mac draws them ("window_style"): their title bars are described
    exact: bool,
    /// the menu bar is streamed: set to stop it
    menubar: Option<Arc<AtomicBool>>,
    /// the wallpaper set from the PC (path or colour), if any
    wallpaper: Option<String>,
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
    if app != "desktop" {
        send_shape(writer, id, w, h, 10.0)?;
        // an exact window's title bar: a 28-point band with the three buttons (scripted)
        if role != WindowRole::Popup && st.lock().unwrap().exact {
            let light = |x: i32| Some(Rect { x, y: 8, w: 12, h: 12 });
            send(writer, &Message::WindowChrome { window_id: id, title_height: 28, close: light(8), minimize: light(28), zoom: light(48), controls: vec![] })?;
        }
    }
    let wr = writer.clone();
    let hue = (60 * id % 256) as u8;
    let st2 = st.clone();
    let handle = std::thread::spawn(move || video_loop(wr, st2, id, w, h, hue, stop));
    st.lock().unwrap().windows.get_mut(&id).unwrap().video = Some(handle);
    Ok(id)
}

/// A `w` x `h` alpha mask with corners rounded by `r` pixels (anti-aliased), as the Mac's
/// windows have.
pub fn rounded_mask(w: usize, h: usize, r: f32) -> Vec<u8> {
    let mut m = vec![255u8; w * h];
    if r <= 0.0 {
        return m;
    }
    for y in 0..h {
        for x in 0..w {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let (cx, cy) = (px.clamp(r, w as f32 - r), py.clamp(r, h as f32 - r));
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            m[y * w + x] = ((r + 0.5 - d).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    m
}

/// The shape of window `id`'s pictures (`w` x `h`), rounded like a Mac window's (scripted).
fn send_shape<W: Write>(writer: &Writer<W>, id: u64, w: usize, h: usize, r: f32) -> Result<(), ProtocolError> {
    let m = rounded_mask(w, h, r);
    let rle = if r <= 0.0 { String::new() } else { base64_encode(&mask::encode(&m)) };
    send(writer, &Message::WindowMask { window_id: id, width: w as u32, height: h as u32, rle })
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
            s.gs_ready = false;
            if let Some(g) = s.gs.take() {
                g.session.stop();
            }
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
    // exact windows only when asked for (RM_FAKE_EXACT=1): the viewer's smoke test checks its own frame
    let exact = std::env::var_os("RM_FAKE_EXACT").is_some_and(|v| v != "0");
    let features: &[&str] = if exact { &["control", "video", "files", "audio", "open_file", "fusion", "mask", "exact"] } else { &["control", "video", "files", "audio", "open_file", "fusion", "mask"] };
    send(&writer, &Message::ServerHello(Hello::ours("rm-fakeagent", &["h264"], features)))?;
    send(&writer, &Message::CapabilityReport(caps()))?;
    // TCP messages and input that came over UDP (the direct path) go through one loop
    let (tx, rx) = std::sync::mpsc::channel::<Result<Option<Message>, ProtocolError>>();
    if let Some(u) = &udp {
        if u.p2p.load(std::sync::atomic::Ordering::Relaxed) {
            send(&writer, &u.offer())?;
        }
        let (itx, irx) = std::sync::mpsc::channel::<Message>();
        *u.inputs.lock().unwrap() = Some(itx);
        let t = tx.clone();
        std::thread::spawn(move || {
            for m in irx {
                if t.send(Ok(Some(m))).is_err() {
                    return;
                }
            }
        });
    }
    let gs_tx = tx.clone();
    std::thread::spawn(move || loop {
        let r = read_message(&mut reader);
        let end = !matches!(r, Ok(Some(_)));
        if tx.send(r).is_err() || end {
            return;
        }
    });
    let st = Arc::new(Mutex::new(State { udp, ..Default::default() }));

    while let Some(msg) = rx.recv().map_err(|_| ProtocolError::Io(std::io::Error::other("reader gone")))?? {
        // A menu command does what its keyboard shortcut does, in the app's main window.
        let msg = match msg {
            Message::MenuInvoke { application_id, path } => {
                let main = st.lock().unwrap().windows.iter().find(|(_, w)| w.app == application_id && w.role == WindowRole::Window).map(|(k, _)| *k);
                match (main, menu_key(&path)) {
                    (Some(window_id), Some(key)) => Message::Key { window_id, physical_key: key.into(), modifiers: vec![Modifier::Command], down: true },
                    _ => Message::Error { code: "menu_invoke_failed".into(), message: format!("{application_id} {path:?}") },
                }
            }
            // the menu bar: a click opens a menu under it (a popup of the bar), Escape closes it
            Message::MouseButton { window_id: MENUBAR_ID, down: true, .. } => {
                if st.lock().unwrap().menubar.is_some() {
                    open_window(&writer, &st, "menubar", "Menu", WindowRole::Popup, Some(MENUBAR_ID))?;
                }
                continue;
            }
            Message::Key { window_id: MENUBAR_ID, physical_key, down: true, .. } if physical_key == "Escape" => {
                let menus: Vec<u64> = st.lock().unwrap().windows.iter().filter(|(_, w)| w.parent == Some(MENUBAR_ID)).map(|(k, _)| *k).collect();
                for m in menus {
                    close_window(&writer, &st, m)?;
                }
                continue;
            }
            Message::MouseMove { window_id: MENUBAR_ID, .. } | Message::MouseButton { window_id: MENUBAR_ID, .. } | Message::Key { window_id: MENUBAR_ID, .. } => continue,
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
            Message::AppLaunch { application_id, arguments, .. } => match APPS.iter().find(|(id, _)| *id == application_id) {
                Some((id, name)) => {
                    if *id == "desktop" {
                        if let Some(hex) = arguments.iter().find_map(|a| a.strip_prefix("gamestream=")) {
                            start_gamestream(&writer, &st, hex, gs_tx.clone());
                        }
                    }
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
            Message::StreamSettings { .. } => {}
            Message::GsTunnel { id, op, data_base64 } => {
                let gs = st.lock().unwrap().gs.clone();
                if let Some(t) = gs {
                    match op.as_str() {
                        "ready" => st.lock().unwrap().gs_ready = true,
                        "open" => t.tcp_open(id),
                        "data" => t.tcp_data(id, &rm_protocol::base64_decode(&data_base64).unwrap_or_default()),
                        _ => t.tcp_close(id),
                    }
                }
            }
            Message::P2pOffer { secret, candidates } => {
                if let Some(u) = st.lock().unwrap().udp.clone() {
                    u.peer_offer(&secret, &candidates);
                }
            }
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
            // a dropped file, uploaded: it opens in a window of the app asked for (default: notes),
            // whose title says what it opened; anything not uploaded this session is refused
            Message::OpenFile { path, application_id } => {
                let size = st.lock().unwrap().files.get(&path).copied().filter(|_| !rm_protocol::runs_code(&path));
                // the default app: TextEdit for text, else the notes app
                let app = application_id.unwrap_or_else(|| if path.ends_with(".txt") { "textedit" } else { "notes" }.into());
                match (size, APPS.iter().find(|(id, _)| *id == app)) {
                    (Some(size), Some((id, name))) => {
                        let file = path.rsplit('/').next().unwrap_or("").to_string();
                        let w = open_window(&writer, &st, id, name, WindowRole::Window, None)?;
                        send(&writer, &Message::AppLaunched { application_id: (*id).into(), pid: 4242 })?;
                        send(&writer, &Message::WindowTitleChanged { window_id: w, title: format!("{name} [opened {file} {size} bytes]") })?;
                    }
                    _ => send(&writer, &Message::Error { code: "open_rejected".into(), message: path })?,
                }
            }
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
                    send_shape(&writer, window_id, w as usize, h as usize, if on { 0.0 } else { 10.0 })?;
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
            // Desktop Fusion: a strip streamed as the Dock, the wallpaper kept (scripted)
            Message::DockStream { enabled } => {
                let mut s = st.lock().unwrap();
                if let Some(stop) = s.dock.take() {
                    stop.store(true, Ordering::SeqCst);
                }
                if enabled {
                    let stop = Arc::new(AtomicBool::new(false));
                    s.dock = Some(stop.clone());
                    drop(s);
                    send(&writer, &Message::DockStatus { available: true, window_id: DOCK_ID, bounds: Rect { x: 220, y: 1000, w: DOCK.0 as u32, h: DOCK.1 as u32 }, edge: "bottom".into(), reason: None })?;
                    send_shape(&writer, DOCK_ID, DOCK.0, DOCK.1, 18.0)?;
                    let (wr, st2) = (writer.clone(), st.clone());
                    std::thread::spawn(move || video_loop(wr, st2, DOCK_ID, DOCK.0, DOCK.1, 200, stop));
                }
            }
            Message::SetWallpaper { path, style: _, color } => {
                let known = path.as_ref().is_none_or(|p| st.lock().unwrap().files.contains_key(p));
                if known {
                    st.lock().unwrap().wallpaper = Some(path.unwrap_or(color));
                    send(&writer, &Message::WallpaperStatus { applied: true, reason: None })?;
                } else {
                    send(&writer, &Message::WallpaperStatus { applied: false, reason: Some("only an image sent from Windows is used".into()) })?;
                }
            }
            Message::WindowStyle { exact } => st.lock().unwrap().exact = exact,
            Message::MenuBarStream { enabled } => {
                let mut s = st.lock().unwrap();
                if let Some(stop) = s.menubar.take() {
                    stop.store(true, Ordering::SeqCst);
                }
                if enabled {
                    let stop = Arc::new(AtomicBool::new(false));
                    s.menubar = Some(stop.clone());
                    drop(s);
                    send(&writer, &Message::MenuBarStatus { available: true, window_id: MENUBAR_ID, bounds: Rect { x: 0, y: 0, w: MENUBAR.0 as u32, h: MENUBAR.1 as u32 }, reason: None })?;
                    let (wr, st2) = (writer.clone(), st.clone());
                    std::thread::spawn(move || video_loop(wr, st2, MENUBAR_ID, MENUBAR.0, MENUBAR.1, 230, stop));
                }
            }
            Message::RestoreWallpaper => {
                st.lock().unwrap().wallpaper = None;
                send(&writer, &Message::WallpaperStatus { applied: false, reason: None })?;
            }
            Message::AudioControl { enabled } => {
                let mut s = st.lock().unwrap();
                if let Some(stop) = s.audio.take() {
                    stop.store(true, Ordering::SeqCst);
                }
                if enabled {
                    let stop = Arc::new(AtomicBool::new(false));
                    s.audio = Some(stop.clone());
                    let (w, st2) = (writer.clone(), st.clone());
                    std::thread::spawn(move || audio_loop(w, st2, stop));
                }
                drop(s);
                send(&writer, &Message::AudioStatus { state: if enabled { "playing" } else { "stopped" }.into(), reason: None })?;
            }
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

/// A 440 Hz tone in 5 ms packets, over UDP while it is alive, else on the Audio channel.
fn audio_loop<W: Write>(w: Writer<W>, st: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    use rm_protocol::audio::{AudioPacket, PACKET_FRAMES, SAMPLE_RATE};
    let start = std::time::Instant::now();
    let (mut seq, mut phase) = (0u32, 0f64);
    while !stop.load(Ordering::SeqCst) {
        let mut samples = Vec::with_capacity(PACKET_FRAMES * 2);
        for _ in 0..PACKET_FRAMES {
            let v = (phase.sin() * 8000.0) as i16;
            samples.extend([v, v]);
            phase += std::f64::consts::TAU * 440.0 / SAMPLE_RATE as f64;
        }
        let p = AudioPacket { seq, pts_us: udp_agent::clock_us(), channels: 2, samples };
        seq = seq.wrapping_add(1);
        let udp = st.lock().unwrap().udp.clone().filter(|u| u.alive());
        if let Some(u) = udp {
            u.send_audio(&p);
        } else {
            let Ok(bytes) = rm_protocol::encode_audio(&p) else { return };
            if w.lock().unwrap().write_all(&bytes).is_err() {
                return;
            }
        }
        // paced by the clock, as a sound card is
        let due = start + Duration::from_micros(seq as u64 * 5_000);
        if let Some(d) = due.checked_duration_since(std::time::Instant::now()) {
            std::thread::sleep(d);
        }
    }
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
        let (gs, gs_ready) = {
            let s = st.lock().unwrap();
            (s.gs.clone().filter(|_| s.desktop == Some(id)), s.gs_ready)
        };
        let gs_only = gs.is_some() && gs_ready;
        if let (Some(g), false) = (gs, data.is_empty()) {
            g.session.send_frame(&data, keyframe, std::time::Instant::now());
        }
        if !gs_only && !data.is_empty() {
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

/// Mac Desktop in full GameStream mode: a host session (rm-gamestream) behind the tunnel, as
/// the Swift agent runs one. Its input becomes the desktop's own input messages.
fn start_gamestream<W: Write + Send + 'static>(writer: &Writer<W>, st: &Arc<Mutex<State>>, hex: &str, input: std::sync::mpsc::Sender<Result<Option<Message>, ProtocolError>>) {
    use rm_gamestream::tunnel::{HostTunnel, Outbound};
    let (Some(key), Some(udp)) = (rm_protocol::udp::unhex16(hex), st.lock().unwrap().udp.clone()) else { return };
    let (w, u) = (writer.clone(), udp.clone());
    let out = Arc::new(move |o: Outbound| match o {
        Outbound::Udp { kind, data } => u.send_tunnel(kind, data),
        Outbound::TcpData { id, data } => {
            let _ = send(&w, &Message::GsTunnel { id, op: "data".into(), data_base64: base64_encode(data) });
        }
        Outbound::TcpClose { id } => {
            let _ = send(&w, &Message::GsTunnel { id, op: "close".into(), data_base64: String::new() });
        }
    });
    let Ok(t) = HostTunnel::start(key, 20, out) else { return };
    let t2 = t.clone();
    *udp.tunnel.lock().unwrap() = Some(Box::new(move |flow, data| t2.udp_in(flow, data)));
    {
        let mut s = st.lock().unwrap();
        s.gs = Some(t.clone());
        s.gs_ready = false;
    }
    let st2 = st.clone();
    let mut at = (0.0f64, 0.0f64);
    std::thread::spawn(move || loop {
        let e = t.events.lock().unwrap().recv_timeout(Duration::from_millis(500));
        let desktop = st2.lock().unwrap().desktop;
        match e {
            // the pointer, from Moonlight's reference space to desktop points
            Ok(rm_gamestream::Event::Input(rm_gamestream::Input::MouseAbs { x, y, width, height })) if width > 0 && height > 0 => {
                at = (x as f64 * DESKTOP.0 as f64 / width as f64, y as f64 * DESKTOP.1 as f64 / height as f64);
            }
            Ok(rm_gamestream::Event::Input(rm_gamestream::Input::Button { button, down })) => {
                if let Some(d) = desktop {
                    let button = match button { 3 => MouseButton::Right, 2 => MouseButton::Middle, _ => MouseButton::Left };
                    let _ = input.send(Ok(Some(Message::MouseButton { window_id: d, button, down, x: at.0, y: at.1 })));
                }
            }
            Ok(rm_gamestream::Event::RequestIdr) => {
                if let Some(d) = desktop {
                    st2.lock().unwrap().key_requests.insert(d);
                }
            }
            Ok(rm_gamestream::Event::Input(rm_gamestream::Input::Text(text))) => {
                if let Some(d) = desktop {
                    let _ = input.send(Ok(Some(Message::TextInput { window_id: d, text })));
                }
            }
            Ok(rm_gamestream::Event::Ended) => return,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if st2.lock().unwrap().gs.is_none() {
                    return;
                }
            }
            Err(_) => return,
        }
    });
}

/// Wait at the relay as the Mac of `session`, then run the end-to-end handshake with the
/// session secret `secret`, as the Mac does.
pub fn secure_join(relay: &str, session: &str, secret: &str) -> Result<(rm_protocol::secure::SecureStream<TcpStream>, rm_protocol::secure::Keys), String> {
    let s = rm_relay::join(relay, session, rm_relay::Role::Agent, &rm_protocol::session::relay_token(session)).map_err(|e| e.to_string())?;
    rm_protocol::secure::agent_tcp(s, session, secret).map_err(|e| e.to_string())
}

/// Bind to a relay as the agent and serve one client.
pub fn serve_via_relay(relay: &str, session: &str, token: &str) -> Result<(), String> {
    serve_via_relay_with(relay, session, token, true)
}

/// [`serve_via_relay`]; `p2p: false` keeps all UDP on the relay (no direct path).
pub fn serve_via_relay_with(relay: &str, session: &str, token: &str, p2p: bool) -> Result<(), String> {
    let (s, keys) = secure_join(relay, session, token)?;
    let w = s.try_clone().map_err(|e| e.to_string())?;
    // UDP video unless RM_NO_UDP is set (tests of the TCP path)
    let udp = if std::env::var_os("RM_NO_UDP").is_some() { None } else { udp_agent::AgentUdp::start(relay, session, &rm_protocol::session::relay_token(session), Some(&keys)).ok() };
    if let Some(u) = &udp {
        u.p2p.store(p2p, std::sync::atomic::Ordering::Relaxed);
    }
    serve_with(s, w, udp).map_err(|e| e.to_string())
}
