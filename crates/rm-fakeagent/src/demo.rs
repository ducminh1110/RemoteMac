//! The Demo Mac: a scripted Mac whose apps are pictures drawn by the caller, so Taro can be
//! tried without a Mac (the viewer's guided tour runs one inside Taro.exe). It speaks the same
//! protocol as the real agent: its apps are listed and launched, its Dock is told, its windows
//! are streamed as H.264 with their title bar and buttons (exact windows) or without them (the
//! viewer's own frame), and what is typed into a window is kept and drawn into it.
//!
//! It is light, as it runs in the viewer's own process: a window is drawn and encoded again only
//! when it changes (typing, its caret blinking, a click) and once a second besides, as the real
//! agent sends an idle window. Video goes over the TCP connection only (it is all on this PC).

use super::{caps, send, Writer};
use openh264::encoder::{Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use openh264::OpenH264API;
use rm_protocol::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// One app of the Demo Mac.
#[derive(Clone, Debug)]
pub struct DemoApp {
    pub id: String,
    pub name: String,
    /// its window's title ("Untitled" for TextEdit)
    pub title: String,
    /// its window's size in points, title bar included
    pub size: (u32, u32),
    /// the band at the top the window is dragged by (title bar and toolbar, points): described
    /// to the viewer for an exact window, cut off under the viewer's own title bar otherwise
    pub title_height: u32,
    /// the window's corner radius (points)
    pub corner: f32,
    pub resizable: bool,
    /// a text caret blinks in it (it is drawn again on the blink while the window is active)
    pub caret: bool,
    /// its menu bar (the Apple menu left out, as the real agent sends it)
    pub menus: Vec<MenuNode>,
}

/// What one picture of a window shows.
#[derive(Clone, Debug)]
pub struct Scene<'a> {
    pub app: &'a str,
    /// the picture in pixels (the whole window, title bar included)
    pub width: usize,
    pub height: usize,
    /// pixels per point (the viewer's display scale)
    pub scale: f32,
    /// seconds since the window last took input (or opened): a caret blinks from it
    pub since_input: f64,
    /// what was typed into the window ('\n' for Return)
    pub text: &'a str,
    /// all of it is selected (Cmd+A)
    pub selected: bool,
    /// it is the active window (coloured buttons, a caret)
    pub active: bool,
}

/// Input an app may take itself, before the usual text editing.
#[derive(Clone, Debug, PartialEq)]
pub enum Input<'a> {
    /// a click at (x, y), points from the window's top-left (title bar included)
    Click { x: f64, y: f64 },
    /// a key pressed without Command ("Escape", "Enter", …)
    Key(&'a str),
    /// text typed
    Text(&'a str),
}

/// A window's picture: straight RGBA, `width` x `height` x 4 bytes.
pub type PaintFn = Arc<dyn Fn(&Scene) -> Vec<u8> + Send + Sync>;
/// An app's icon `size` px square: straight RGBA.
pub type IconFn = Arc<dyn Fn(&str, u32) -> Vec<u8> + Send + Sync>;
/// Input an app handles itself (a Calculator key clicked): what it types into the window
/// ('\u{8}' deletes the last character), or None for the usual handling.
pub type InputFn = Arc<dyn Fn(&str, &Input) -> Option<String> + Send + Sync>;

/// The Demo Mac's apps (in Dock order) and how they look.
#[derive(Clone)]
pub struct Demo {
    pub apps: Vec<DemoApp>,
    pub paint: PaintFn,
    pub icon: IconFn,
    pub input: Option<InputFn>,
}

/// Where a window's three buttons are (points from its top-left), as the demo windows draw
/// them: 12-point lights 20 points apart, the first centred 20 points in, all centred in the
/// title band.
pub fn lights(title_height: u32) -> [Rect; 3] {
    let cy = title_height as i32 / 2;
    std::array::from_fn(|i| Rect { x: 14 + 20 * i as i32, y: cy - 6, w: 12, h: 12 })
}

/// The pixel size of a picture `points` big at `scale` (even sides: H.264's 4:2:0).
fn pixels(points: u32, scale: f32) -> usize {
    (((points as f32 * scale).round() as usize) & !1).max(2)
}

/// The shape of a window's picture: corners rounded by `r` px (only the bottom ones under the
/// viewer's own title bar, which covers the top).
fn window_mask(w: usize, h: usize, r: f32, top: bool) -> Vec<u8> {
    let mut m = super::rounded_mask(w, h, r);
    if !top {
        let rows = (r.ceil() as usize + 1).min(h);
        m[..rows * w].fill(255);
    }
    m
}

/// How long text may grow in one window (characters).
const MAX_TEXT: usize = 20_000;
/// An idle window's picture is sent again this often (as the real agent sends at 1 fps).
const KEEP_ALIVE: Duration = Duration::from_secs(1);
/// macOS's caret: on and off this long each.
pub const BLINK: f64 = 0.53;

struct Win {
    app: usize,
    /// size in points, title bar included; before full screen
    size: (u32, u32),
    before_full: Option<(u32, u32)>,
    pos: (i32, i32),
    /// shown with its title bar (the viewer asked for exact windows before it opened)
    exact: bool,
    text: String,
    selected: bool,
    last_input: Instant,
    /// its picture must be drawn again / must be a keyframe
    dirty: bool,
    keyframe: bool,
}

impl Win {
    /// The part of the window the viewer sees, in points: (top, height).
    fn shown(&self, app: &DemoApp) -> (u32, u32) {
        let cut = if self.exact { 0 } else { app.title_height.min(self.size.1.saturating_sub(1)) };
        (cut, self.size.1 - cut)
    }

    fn bounds(&self, app: &DemoApp) -> Rect {
        let (cut, h) = self.shown(app);
        Rect { x: self.pos.0, y: self.pos.1 + cut as i32, w: self.size.0, h }
    }

    fn type_text(&mut self, t: &str) {
        for c in t.chars() {
            if c == '\u{8}' {
                self.backspace();
                continue;
            }
            if c.is_control() && c != '\n' && c != '\t' {
                continue;
            }
            if self.selected {
                self.text.clear();
                self.selected = false;
            }
            if self.text.chars().count() < MAX_TEXT {
                self.text.push(c);
            }
        }
        self.touch();
    }

    fn backspace(&mut self) {
        if self.selected {
            self.text.clear();
            self.selected = false;
        } else {
            self.text.pop();
        }
        self.touch();
    }

    fn touch(&mut self) {
        self.last_input = Instant::now();
        self.dirty = true;
    }
}

struct State {
    demo: Demo,
    windows: BTreeMap<u64, Win>,
    next_id: u64,
    /// the active window
    front: Option<u64>,
    /// the viewer shows windows as the Mac draws them (asked before they open)
    exact: bool,
    /// pixels per point: the viewer's display scale (`VideoDecoder`) and what its settings ask
    /// for (`StreamSettings`); the pictures take the smaller, never more pixels than are shown
    /// (a Mac captures at 2x for sharpness; drawn here, 1x text is as sharp, at a quarter of
    /// the work)
    display_scale: Option<f32>,
    asked_scale: Option<f32>,
    clipboard: String,
    clip_seq: u64,
    display: Option<(u32, u32)>,
    /// the session is over: the render thread stops
    stop: bool,
}

impl State {
    fn scale(&self) -> f32 {
        let s = match (self.display_scale, self.asked_scale) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(1.0),
        };
        s.clamp(1.0, 3.0)
    }
}

type Shared = Arc<(Mutex<State>, Condvar)>;

fn wake(sh: &Shared) {
    sh.1.notify_all();
}

/// Serve one viewer as the Demo Mac (see the module's notes).
pub fn serve_demo<S: Read + Write + Send + 'static>(mut reader: S, writer: S, demo: Demo) -> Result<(), ProtocolError> {
    let writer: Writer<S> = Arc::new(Mutex::new(writer));
    match read_message(&mut reader)? {
        Some(Message::ClientHello(_)) => {}
        _ => return Ok(()),
    }
    let agent = format!("taro-demo {} Demo Mac", env!("CARGO_PKG_VERSION"));
    send(&writer, &Message::ServerHello(Hello::ours(&agent, &["h264"], &["control", "video", "mask", "exact", "dock"])))?;
    send(&writer, &Message::CapabilityReport(caps()))?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<Option<Message>, ProtocolError>>();
    std::thread::spawn(move || loop {
        let r = read_message(&mut reader);
        let end = !matches!(r, Ok(Some(_)));
        if tx.send(r).is_err() || end {
            return;
        }
    });
    let st = State { demo, windows: BTreeMap::new(), next_id: 0, front: None, exact: false, display_scale: None, asked_scale: None, clipboard: String::new(), clip_seq: 0, display: None, stop: false };
    let sh: Shared = Arc::new((Mutex::new(st), Condvar::new()));
    let render = {
        let (sh, w) = (sh.clone(), writer.clone());
        std::thread::spawn(move || render_loop(sh, w))
    };
    let r = (|| -> Result<(), ProtocolError> {
        while let Some(msg) = rx.recv().map_err(|_| ProtocolError::Io(std::io::Error::other("reader gone")))?? {
            handle(&writer, &sh, msg)?;
        }
        Ok(())
    })();
    sh.0.lock().unwrap().stop = true;
    wake(&sh);
    let _ = render.join();
    r
}

/// Bind to a relay as the Demo Mac of `session` and serve one viewer; returns when it leaves.
pub fn serve_demo_via_relay(relay: &str, session: &str, token: &str, demo: Demo) -> Result<(), String> {
    let (s, _keys) = super::secure_join(relay, session, token)?;
    let w = s.try_clone().map_err(|e| e.to_string())?;
    serve_demo(s, w, demo).map_err(|e| e.to_string())
}

fn app_index(st: &State, id: &str) -> Option<usize> {
    st.demo.apps.iter().position(|a| a.id == id)
}

fn handle<W: Write + Send + 'static>(w: &Writer<W>, sh: &Shared, msg: Message) -> Result<(), ProtocolError> {
    match msg {
        Message::ListApps => {
            let apps = sh.0.lock().unwrap().demo.apps.iter().map(|a| AppInfo { id: a.id.clone(), name: a.name.clone(), available: true, version: None }).collect();
            send(w, &Message::Apps { apps })?;
        }
        Message::AppLaunch { application_id, .. } => launch(w, sh, &application_id)?,
        Message::DockOpen { id } => launch(w, sh, id.strip_prefix("app:").unwrap_or(&id))?,
        Message::AppTerminate { application_id } => quit(w, sh, &application_id)?,
        Message::DockList => {
            let items = {
                let st = sh.0.lock().unwrap();
                let running: HashSet<usize> = st.windows.values().map(|x| x.app).collect();
                st.demo.apps.iter().enumerate().map(|(i, a)| DockEntry { id: format!("app:{}", a.id), kind: "app".into(), title: a.name.clone(), app_id: Some(a.id.clone()), running: running.contains(&i) }).collect()
            };
            send(w, &Message::DockItems { items })?;
        }
        Message::DockMenuGet { id } => {
            let menus = dock_menu(&sh.0.lock().unwrap(), &id);
            send(w, &Message::DockMenu { id, menus })?;
        }
        Message::DockMenuInvoke { id, path } => {
            let chosen = path.first().and_then(|&i| dock_menu(&sh.0.lock().unwrap(), &id).get(i as usize).map(|n| n.title.clone()));
            let app = id.strip_prefix("app:").unwrap_or(&id).to_string();
            match chosen.as_deref() {
                Some("Open") | Some("Show All Windows") => launch(w, sh, &app)?,
                Some("Quit") => quit(w, sh, &app)?,
                _ => {}
            }
        }
        Message::GetAppIcon { application_id } => {
            let icon = {
                let st = sh.0.lock().unwrap();
                app_index(&st, &application_id).map(|_| st.demo.icon.clone())
            };
            match icon {
                Some(f) => {
                    let size = 128u32;
                    let px = f(&application_id, size);
                    if px.len() == (size * size * 4) as usize {
                        send(w, &Message::AppIcon { application_id, size, rgba_base64: base64_encode(&px) })?;
                    }
                }
                None => send(w, &Message::Error { code: "unknown_app".into(), message: application_id })?,
            }
        }
        Message::GetMenuBar { application_id } => {
            let menus = {
                let st = sh.0.lock().unwrap();
                app_index(&st, &application_id).map(|i| st.demo.apps[i].menus.clone())
            };
            if let Some(menus) = menus {
                send(w, &Message::MenuBar { application_id, menus })?;
            }
        }
        Message::MenuInvoke { application_id, path } => {
            let (title, win) = {
                let st = sh.0.lock().unwrap();
                let title = app_index(&st, &application_id).and_then(|i| menu_title(&st.demo.apps[i].menus, &path));
                (title, main_window(&st, &application_id))
            };
            let Some(title) = title else { return Ok(()) };
            // a menu command does what its shortcut does
            let key = if title.starts_with("Quit") {
                "KeyQ"
            } else if title.starts_with("Close") {
                "KeyW"
            } else if title == "Select All" {
                "KeyA"
            } else if title == "Copy" {
                "KeyC"
            } else if title == "Cut" {
                "KeyX"
            } else if title == "Paste" {
                "KeyV"
            } else if title.starts_with("New") {
                if win.is_none() {
                    launch(w, sh, &application_id)?;
                }
                return Ok(());
            } else {
                return Ok(());
            };
            match win {
                Some(id) => command(w, sh, id, key)?,
                None if key == "KeyQ" => quit(w, sh, &application_id)?,
                None => {}
            }
        }
        Message::TextInput { window_id, text } => {
            let mut st = sh.0.lock().unwrap();
            let typed = {
                let State { windows, demo, .. } = &mut *st;
                windows.get_mut(&window_id).map(|win| {
                    // an app may take what is typed itself (Calculator's keys)
                    let own = demo.input.as_ref().and_then(|f| f(&demo.apps[win.app].id, &Input::Text(&text)));
                    win.type_text(own.as_deref().unwrap_or(&text));
                })
            };
            if typed.is_some() {
                st.front = Some(window_id);
            }
            drop(st);
            wake(sh);
        }
        Message::Key { window_id, physical_key, modifiers, down: true } => {
            if modifiers.contains(&Modifier::Command) {
                command(w, sh, window_id, &physical_key)?;
            } else {
                let mut st = sh.0.lock().unwrap();
                let State { windows, demo, .. } = &mut *st;
                if let Some(win) = windows.get_mut(&window_id) {
                    match demo.input.as_ref().and_then(|f| f(&demo.apps[win.app].id, &Input::Key(&physical_key))) {
                        Some(t) => win.type_text(&t),
                        None => match physical_key.as_str() {
                            "Backspace" | "Delete" => win.backspace(),
                            "Enter" | "NumpadEnter" => win.type_text("\n"),
                            "Tab" => win.type_text("\t"),
                            _ => {}
                        },
                    }
                }
                drop(st);
                wake(sh);
            }
        }
        Message::MouseButton { window_id, button: MouseButton::Left, down: true, x, y } => {
            let mut st = sh.0.lock().unwrap();
            let State { windows, demo, front, .. } = &mut *st;
            if let Some(win) = windows.get_mut(&window_id) {
                *front = Some(window_id);
                win.dirty = true;
                let app = &demo.apps[win.app];
                let (cut, _) = win.shown(app);
                if let Some(t) = demo.input.as_ref().and_then(|f| f(&app.id, &Input::Click { x, y: y + cut as f64 })) {
                    win.type_text(&t);
                }
            }
            drop(st);
            wake(sh);
        }
        Message::WindowFocus { window_id } => {
            let mut st = sh.0.lock().unwrap();
            if st.windows.contains_key(&window_id) && st.front != Some(window_id) {
                st.front = Some(window_id);
                st.windows.values_mut().for_each(|x| x.dirty = true);
            }
            drop(st);
            wake(sh);
        }
        Message::WindowClose { window_id } => close(w, sh, window_id)?,
        Message::WindowResizeRequest { window_id, width, height } => {
            let moved = {
                let mut st = sh.0.lock().unwrap();
                let State { windows, demo, .. } = &mut *st;
                windows.get_mut(&window_id).map(|win| {
                    let app = &demo.apps[win.app];
                    if app.resizable {
                        let (cut, _) = win.shown(app);
                        win.size = (width.clamp(300, 3000), (height + cut).clamp(app.title_height + 120, 2000));
                        win.dirty = true;
                    }
                    win.bounds(app)
                })
            };
            wake(sh);
            if let Some(bounds) = moved {
                send(w, &Message::WindowMoved { window_id, bounds })?;
            }
        }
        Message::WindowFullscreen { window_id, on } => {
            let moved = {
                let mut st = sh.0.lock().unwrap();
                let screen = st.display.unwrap_or((1440, 900));
                let State { windows, demo, .. } = &mut *st;
                windows.get_mut(&window_id).filter(|win| demo.apps[win.app].resizable).map(|win| {
                    if on && win.before_full.is_none() {
                        win.before_full = Some(win.size);
                        win.size = screen;
                    } else if let (false, Some(s)) = (on, win.before_full.take()) {
                        win.size = s;
                    }
                    win.dirty = true;
                    win.bounds(&demo.apps[win.app])
                })
            };
            wake(sh);
            if let Some(bounds) = moved {
                send(w, &Message::WindowMoved { window_id, bounds })?;
            }
        }
        Message::RequestKeyframe { window_id } => {
            if let Some(win) = sh.0.lock().unwrap().windows.get_mut(&window_id) {
                win.keyframe = true;
            }
            wake(sh);
        }
        Message::VideoDecoder { scale: Some(s), .. } | Message::StreamSettings { scale: Some(s), .. } => {
            let mut st = sh.0.lock().unwrap();
            if matches!(msg, Message::VideoDecoder { .. }) {
                st.display_scale = Some(s as f32);
            } else {
                st.asked_scale = Some(s as f32);
            }
            st.windows.values_mut().for_each(|x| x.dirty = true);
            drop(st);
            wake(sh);
        }
        Message::WindowStyle { exact } => sh.0.lock().unwrap().exact = exact,
        Message::ClipboardSet { text, .. } => sh.0.lock().unwrap().clipboard = text,
        Message::DisplayConfigure { width, height, scale } => {
            let (dw, dh) = (width / scale.max(1), height / scale.max(1));
            sh.0.lock().unwrap().display = Some((dw, dh));
            send(w, &Message::DisplayStatus { available: true, display_id: 1, width: dw, height: dh, reason: None })?;
        }
        Message::Ping { nonce } => send(w, &Message::Pong { nonce })?,
        _ => {}
    }
    Ok(())
}

/// The app's first window (its main one).
fn main_window(st: &State, app: &str) -> Option<u64> {
    let i = app_index(st, app)?;
    st.windows.iter().find(|(_, x)| x.app == i).map(|(k, _)| *k)
}

/// The title of the menu item at `path` (indices from the top of the bar).
fn menu_title(menus: &[MenuNode], path: &[u32]) -> Option<String> {
    let (first, rest) = path.split_first()?;
    let node = menus.get(*first as usize)?;
    if rest.is_empty() {
        Some(node.title.clone())
    } else {
        menu_title(&node.children, rest)
    }
}

fn dock_menu(st: &State, id: &str) -> Vec<MenuNode> {
    let node = |t: &str| MenuNode { title: t.into(), enabled: true, ..Default::default() };
    let app = id.strip_prefix("app:").unwrap_or(id);
    match main_window(st, app) {
        Some(_) => vec![node("Show All Windows"), MenuNode { separator: true, ..Default::default() }, node("Quit")],
        None if app_index(st, app).is_some() => vec![node("Open")],
        None => vec![],
    }
}

/// A Command shortcut in window `id`.
fn command<W: Write + Send + 'static>(w: &Writer<W>, sh: &Shared, id: u64, key: &str) -> Result<(), ProtocolError> {
    enum After {
        Nothing,
        Copied(u64, String),
        Close,
        Quit(String),
    }
    let after = {
        let mut st = sh.0.lock().unwrap();
        let clip = st.clipboard.clone();
        let State { windows, demo, clip_seq, clipboard, .. } = &mut *st;
        let Some(win) = windows.get_mut(&id) else { return Ok(()) };
        let mut copy = |t: String| {
            *clip_seq += 1;
            *clipboard = t.clone();
            After::Copied(*clip_seq, t)
        };
        let after = match key {
            "KeyA" => {
                win.selected = !win.text.is_empty();
                win.touch();
                After::Nothing
            }
            "KeyC" if win.selected => copy(win.text.clone()),
            "KeyX" if win.selected => {
                let a = copy(win.text.clone());
                win.backspace();
                a
            }
            "KeyV" => {
                win.type_text(&clip);
                After::Nothing
            }
            "KeyW" => After::Close,
            "KeyQ" => After::Quit(demo.apps[win.app].id.clone()),
            _ => After::Nothing,
        };
        drop(st);
        wake(sh);
        after
    };
    match after {
        After::Copied(seq, text) => send(w, &Message::ClipboardSet { seq, text }),
        After::Close => close(w, sh, id),
        After::Quit(app) => quit(w, sh, &app),
        After::Nothing => Ok(()),
    }
}

/// Open the app (one window) or bring its window to the front.
fn launch<W: Write + Send + 'static>(w: &Writer<W>, sh: &Shared, id: &str) -> Result<(), ProtocolError> {
    let opened = {
        let mut st = sh.0.lock().unwrap();
        let Some(i) = app_index(&st, id) else {
            drop(st);
            return send(w, &Message::Error { code: "launch_rejected".into(), message: format!("unknown application '{id}'") });
        };
        match main_window(&st, id) {
            Some(win) => {
                // already open: in front, with a fresh picture (the viewer's launch card waits for one)
                st.front = Some(win);
                st.windows.values_mut().for_each(|x| x.dirty = true);
                if let Some(x) = st.windows.get_mut(&win) {
                    x.keyframe = true;
                }
                (i, None)
            }
            None => {
                st.next_id += 1;
                let wid = st.next_id;
                let app = st.demo.apps[i].clone();
                // the apps open where they would on a 1440 x 900 screen, not on top of each other
                let pos = (140 + 170 * i as i32, 90 + 50 * i as i32);
                let win = Win { app: i, size: app.size, before_full: None, pos, exact: st.exact, text: String::new(), selected: false, last_input: Instant::now(), dirty: true, keyframe: true };
                let created = Message::WindowCreated { window_id: wid, application_id: app.id.clone(), title: app.title.clone(), bounds: win.bounds(&app), parent_id: None, role: WindowRole::Window };
                let chrome = win.exact.then(|| {
                    let [close, minimize, zoom] = lights(app.title_height);
                    Message::WindowChrome { window_id: wid, title_height: app.title_height, close: Some(close), minimize: Some(minimize), zoom: Some(zoom), controls: vec![] }
                });
                st.windows.insert(wid, win);
                st.front = Some(wid);
                st.windows.values_mut().for_each(|x| x.dirty = true);
                (i, Some((created, chrome)))
            }
        }
    };
    send(w, &Message::AppLaunched { application_id: id.into(), pid: 500 + opened.0 as u32 })?;
    // announced before the render thread can send its first picture (it waits for the lock)
    if let Some((created, chrome)) = opened.1 {
        send(w, &created)?;
        if let Some(c) = chrome {
            send(w, &c)?;
        }
    }
    wake(sh);
    Ok(())
}

fn close<W: Write + Send + 'static>(w: &Writer<W>, sh: &Shared, id: u64) -> Result<(), ProtocolError> {
    let gone = {
        let mut st = sh.0.lock().unwrap();
        let Some(win) = st.windows.remove(&id) else { return Ok(()) };
        if st.front == Some(id) {
            st.front = st.windows.keys().next_back().copied();
        }
        let still = st.windows.values().any(|x| x.app == win.app);
        (!still).then(|| st.demo.apps[win.app].id.clone())
    };
    send(w, &Message::WindowDestroyed { window_id: id })?;
    // a demo app quits with its last window (its dot leaves the Dock)
    if let Some(app) = gone {
        send(w, &Message::AppExited { application_id: app, code: Some(0) })?;
    }
    wake(sh);
    Ok(())
}

fn quit<W: Write + Send + 'static>(w: &Writer<W>, sh: &Shared, app: &str) -> Result<(), ProtocolError> {
    let ids: Vec<u64> = {
        let st = sh.0.lock().unwrap();
        let Some(i) = app_index(&st, app) else { return Ok(()) };
        st.windows.iter().filter(|(_, x)| x.app == i).map(|(k, _)| *k).collect()
    };
    for id in ids {
        close(w, sh, id)?;
    }
    Ok(())
}

/// One window's stream: its encoder and what it last sent.
struct Stream {
    enc: Encoder,
    size: (usize, usize),
    rgba: Vec<u8>,
    yuv: Option<YUVBuffer>,
    sent: Instant,
    /// the caret's blink phase it was drawn at
    phase: Option<u64>,
}

fn encoder() -> Option<Encoder> {
    // screen content at a steady quality, no rate control: text stays sharp, few frames are
    // sent anyway, and nothing is skipped (the options screen content cannot take are off, so
    // openh264 does not warn about them in the viewer's log)
    let cfg = EncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Off)
        .max_frame_rate(FrameRate::from_hz(30.0))
        .skip_frames(false)
        .adaptive_quantization(false)
        .background_detection(false);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).ok()
}

/// What one window needs drawn, read under the lock.
struct Job {
    id: u64,
    app: String,
    paint: PaintFn,
    /// the picture's size (px), the part of the window it shows (top px) and the window's height (px)
    size: (usize, usize),
    cut: usize,
    full_h: usize,
    scale: f32,
    corner: f32,
    exact: bool,
    text: String,
    selected: bool,
    active: bool,
    since: f64,
    phase: Option<u64>,
    redraw: bool,
    keyframe: bool,
}

/// Draws and sends the windows' pictures: each when it changed, its caret blinked, or a second
/// went by.
fn render_loop<W: Write>(sh: Shared, w: Writer<W>) {
    let mut streams: HashMap<u64, Stream> = HashMap::new();
    loop {
        let jobs = {
            let mut st = sh.0.lock().unwrap();
            loop {
                if st.stop {
                    return;
                }
                let now = Instant::now();
                streams.retain(|id, _| st.windows.contains_key(id));
                let mut next = now + KEEP_ALIVE;
                let mut jobs = vec![];
                let (scale, front) = (st.scale(), st.front);
                let State { windows, demo, .. } = &mut *st;
                for (&id, win) in windows.iter_mut() {
                    let app = &demo.apps[win.app];
                    let active = front == Some(id);
                    let since = now.duration_since(win.last_input).as_secs_f64();
                    // which half of its blink the caret is in (it blinks in the active window only)
                    let phase = (app.caret && active).then(|| (since / BLINK) as u64);
                    let (cut, h) = win.shown(app);
                    let size = (pixels(win.size.0, scale), pixels(h, scale));
                    let last = streams.get(&id).map(|s| (s.sent, s.size, s.phase));
                    let resized = last.is_none_or(|l| l.1 != size);
                    let blinked = last.is_some_and(|l| l.2 != phase);
                    let stale = last.is_some_and(|l| now >= l.0 + KEEP_ALIVE);
                    if win.dirty || win.keyframe || resized || blinked || stale {
                        jobs.push(Job {
                            id,
                            app: app.id.clone(),
                            paint: demo.paint.clone(),
                            size,
                            cut: (cut as f32 * scale).round() as usize,
                            full_h: (win.size.1 as f32 * scale).round() as usize,
                            scale,
                            corner: app.corner,
                            exact: win.exact,
                            text: win.text.clone(),
                            selected: win.selected,
                            active,
                            since,
                            phase,
                            redraw: win.dirty || resized || blinked,
                            keyframe: win.keyframe,
                        });
                        win.dirty = false;
                        win.keyframe = false;
                    } else {
                        if let Some(l) = last {
                            next = next.min(l.0 + KEEP_ALIVE);
                        }
                        if let Some(p) = phase {
                            next = next.min(win.last_input + Duration::from_secs_f64((p + 1) as f64 * BLINK));
                        }
                    }
                }
                if !jobs.is_empty() {
                    break jobs;
                }
                let wait = next.saturating_duration_since(Instant::now()).max(Duration::from_millis(5));
                st = sh.1.wait_timeout(st, wait).unwrap().0;
            }
        };
        for job in jobs {
            if render(&w, &mut streams, job).is_err() {
                return;
            }
        }
    }
}

/// Draw (when it changed) and send one window's picture.
fn render<W: Write>(w: &Writer<W>, streams: &mut HashMap<u64, Stream>, job: Job) -> Result<(), ProtocolError> {
    let (pw, ph) = job.size;
    let fresh = streams.get(&job.id).is_none_or(|s| s.size != job.size);
    if fresh {
        let enc = encoder().ok_or_else(|| ProtocolError::Io(std::io::Error::other("no H.264 encoder")))?;
        streams.insert(job.id, Stream { enc, size: job.size, rgba: vec![], yuv: None, sent: Instant::now(), phase: None });
        // the shape of pictures of this size, before the first of them
        let m = window_mask(pw, ph, job.corner * job.scale, job.exact);
        send(w, &Message::WindowMask { window_id: job.id, width: pw as u32, height: ph as u32, rle: base64_encode(&mask::encode(&m)) })?;
    }
    let s = streams.get_mut(&job.id).expect("stream");
    let mut changed = false;
    s.phase = job.phase;
    if job.redraw || fresh {
        let scene = Scene { app: &job.app, width: pw, height: job.full_h.max(ph + job.cut), scale: job.scale, since_input: job.since, text: &job.text, selected: job.selected, active: job.active };
        let full = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.paint)(&scene))).unwrap_or_default();
        let mut rgba = full.get(job.cut * pw * 4..(job.cut + ph) * pw * 4).map(<[u8]>::to_vec).unwrap_or_default();
        if rgba.len() != pw * ph * 4 {
            // a painter that drew nothing (or the wrong size): a plain window, never a broken stream
            rgba = [236u8, 236, 236, 255].repeat(pw * ph);
        }
        if rgba != s.rgba {
            s.yuv = Some(YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (pw, ph))));
            s.rgba = rgba;
            changed = true;
        }
    }
    // unchanged and sent a moment ago: nothing to send
    if !changed && !job.keyframe && !fresh && s.sent.elapsed() < KEEP_ALIVE {
        return Ok(());
    }
    let Some(yuv) = s.yuv.as_ref() else { return Ok(()) };
    if job.keyframe || fresh {
        s.enc.force_intra_frame();
    }
    let Ok(bs) = s.enc.encode(yuv) else { return Ok(()) };
    let data = bs.to_vec();
    s.sent = Instant::now();
    if data.is_empty() {
        return Ok(());
    }
    let keyframe = data.windows(5).any(|x| x[..4] == [0, 0, 0, 1] && x[4] & 0x1f == 5);
    let f = VideoFrame { window_id: job.id, pts_us: super::udp_agent::clock_us(), keyframe, codec: CODEC_H264, width: pw as u16, height: ph as u16, data };
    let bytes = encode_video(&f)?;
    w.lock().unwrap().write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn app(id: &str, size: (u32, u32), title_height: u32) -> DemoApp {
        DemoApp { id: id.into(), name: id.into(), title: id.into(), size, title_height, corner: 12.0, resizable: true, caret: true, menus: vec![] }
    }

    /// Fine dark detail on white, as text is (what the encoder must keep sharp), and a bar as
    /// long as what was typed.
    fn page(s: &Scene) -> Vec<u8> {
        let mut v = vec![255u8; s.width * s.height * 4];
        for y in 0..s.height {
            for x in 0..s.width {
                let ink = ((y / 3) % 4 == 0 && (x * 7 + y * 3) % 11 < 5) || (s.text.len() * 40 + 20 > x && y > 200 && y < 210);
                if ink {
                    v[(y * s.width + x) * 4..][..3].copy_from_slice(&[20, 20, 30]);
                }
            }
        }
        v
    }

    fn demo() -> Demo {
        Demo { apps: vec![app("notes", (480, 320), 52), app("calc", (232, 380), 28)], paint: Arc::new(page), icon: Arc::new(|_, s| vec![200; (s * s * 4) as usize]), input: None }
    }

    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        (a, l.accept().unwrap().0)
    }

    fn launch(app: &str) -> Message {
        Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() }
    }

    #[test]
    fn the_demo_mac_lists_launches_streams_and_takes_typing() {
        let (mut c, srv) = pair();
        let s2 = srv.try_clone().unwrap();
        std::thread::spawn(move || serve_demo(srv, s2, demo()));
        let mut c2 = c.try_clone().unwrap();
        write_message(&mut c2, &Message::ClientHello(Hello::ours("test", &["h264"], &["dock", "exact", "mask"]))).unwrap();
        write_message(&mut c2, &Message::WindowStyle { exact: false }).unwrap();
        write_message(&mut c2, &Message::ListApps).unwrap();
        write_message(&mut c2, &launch("notes")).unwrap();
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let (mut apps, mut win, mut mask, mut dock, mut typed) = (vec![], None, None, None, false);
        let (mut frames, mut last) = (0, vec![]);
        let deadline = Instant::now() + Duration::from_secs(10);
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        while Instant::now() < deadline && !(frames >= 3 && dock.is_some()) {
            match read_frame(&mut c).unwrap() {
                Some(Frame::Msg(Message::Apps { apps: a })) => apps = a.into_iter().map(|a| a.id).collect(),
                Some(Frame::Msg(Message::WindowCreated { window_id, bounds, .. })) => {
                    // the viewer's own title bar: the Mac's (52 points) is cut off
                    assert_eq!((bounds.w, bounds.h), (480, 268));
                    win = Some(window_id);
                    write_message(&mut c2, &Message::DockList).unwrap();
                }
                Some(Frame::Msg(Message::WindowMask { width, height, .. })) => mask = Some((width, height)),
                Some(Frame::Msg(Message::DockItems { items })) => dock = Some(items),
                Some(Frame::Video(v)) => {
                    assert_eq!((v.width, v.height), (480, 268));
                    if let Ok(Some(yuv)) = dec.decode(&v.data) {
                        last.resize(480 * 268 * 3, 0);
                        yuv.write_rgb8(&mut last);
                        frames += 1;
                    }
                    if !typed {
                        write_message(&mut c2, &Message::TextInput { window_id: win.unwrap(), text: "hi".into() }).unwrap();
                        typed = true;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(apps, ["notes", "calc"]);
        assert_eq!(mask, Some((480, 268)));
        assert!(frames >= 3, "decoded frames: {frames}");
        // the last picture is what was drawn after the typing, close enough to keep text legible
        let want = page(&Scene { app: "notes", width: 480, height: 320, scale: 1.0, since_input: 0.0, text: "hi", selected: false, active: true });
        let sum: f64 = last.chunks(3).zip(want[52 * 480 * 4..].chunks(4)).map(|(a, b)| (0..3).map(|i| (a[i] as f64 - b[i] as f64).abs()).sum::<f64>()).sum();
        let err = sum / (480.0 * 268.0 * 3.0);
        assert!(err < 4.0, "mean error per channel after typing: {err}");
        let dock = dock.expect("the Dock");
        let dock: Vec<(String, bool)> = dock.into_iter().map(|d| (d.app_id.unwrap(), d.running)).collect();
        assert_eq!(dock, [("notes".to_string(), true), ("calc".to_string(), false)]);
    }

    #[test]
    fn an_idle_window_sends_about_once_a_second() {
        let (mut c, srv) = pair();
        let s2 = srv.try_clone().unwrap();
        let mut d = demo();
        d.apps[1].caret = false;
        std::thread::spawn(move || serve_demo(srv, s2, d));
        let mut c2 = c.try_clone().unwrap();
        write_message(&mut c2, &Message::ClientHello(Hello::ours("test", &["h264"], &[]))).unwrap();
        write_message(&mut c2, &Message::WindowStyle { exact: true }).unwrap();
        write_message(&mut c2, &launch("calc")).unwrap();
        let start = Instant::now();
        let (mut frames, mut chrome) = (0, None);
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        while start.elapsed() < Duration::from_millis(2500) {
            match read_frame(&mut c).unwrap() {
                Some(Frame::Video(v)) => {
                    assert_eq!((v.width, v.height), (232, 380), "an exact window: all of it");
                    frames += 1;
                }
                Some(Frame::Msg(Message::WindowChrome { title_height, close, .. })) => chrome = Some((title_height, close)),
                _ => {}
            }
        }
        assert_eq!(chrome, Some((28, Some(Rect { x: 14, y: 8, w: 12, h: 12 }))));
        assert!((2..=5).contains(&frames), "frames in 2.5 s: {frames}");
    }
}
