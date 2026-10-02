//! Win32 presentation: one native top-level window per remote window, pictures drawn with GDI
//! (`StretchDIBits`). GDI is the portable baseline that also works on CI machines without a GPU;
//! a Direct3D 11 swap-chain presenter is a drop-in replacement for `paint`.
//!
//! Threading: everything here runs on the UI thread. The network thread only posts `WM_UI_EVENT`.

use crate::keymap::*;
use crate::native;
use crate::comp;
use crate::d3d;
use crate::launcher::{self, Launcher};
use crate::chrome;
use crate::menu;
use crate::shortcuts;
use rm_protocol::{MenuNode, WindowRole};
use crate::net::{self, Link, UiEvent};
use rm_decode::Picture;
use rm_protocol::{Message, MouseButton};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Options {
    pub relay: String,
    pub session: String,
    pub token: String,
    /// Ask for the Mac's ID and password in a window (session/token are derived from them).
    pub prompt: bool,
    pub app: Option<String>,
    pub ctrl_as_command: bool,
    pub smoke: bool,
    pub clipboard: bool,
    /// Prefer Direct3D 11 (falls back to GDI when no device can be created).
    pub d3d: bool,
    /// Replace the Mac's file Open panel with the Windows one (uploading the chosen file).
    pub windows_file_picker: bool,
    /// Put each Mac app in the Start menu (and so in Windows Search) while connected.
    pub shortcuts: bool,
    /// Open these apps one after another and save screenshots of their windows, then exit.
    pub showcase: Option<ShowcaseOptions>,
}

pub struct ShowcaseOptions {
    pub apps: Vec<String>,
    pub dir: std::path::PathBuf,
    /// How long an app's windows are left to settle before the screenshot.
    pub settle: Duration,
    /// Give up on an app that shows no window by then.
    pub app_timeout: Duration,
    /// Typed into each app's window once it is drawn (keyboard path: WM_CHAR -> Mac).
    pub type_text: Option<String>,
}

const WM_UI_EVENT: u32 = WM_APP + 1;
/// wParam = remote id of an open panel to replace with the Windows file picker.
const WM_PICK_FILE: u32 = WM_APP + 2;
const TIMER_ID: usize = 1;
const WM_MOUSE_LEAVE: u32 = 0x02A3;

struct Remote {
    id: u64,
    app: String,
    role: WindowRole,
    parent: Option<u64>,
    /// Remote window origin (Mac screen points), used to place owned dialogs relative to their parent.
    rx: i32,
    ry: i32,
    /// Remote window size in Mac points.
    rw: u32,
    rh: u32,
    /// Windows DIP scale of the monitor the window is on (1 Mac point = 1 DIP).
    scale: f64,
    maximized: bool,
    /// GPU presenter; `None` means GDI.
    presenter: Option<d3d::Presenter>,
    /// DirectComposition surface (rounded window, chrome + picture); replaces `presenter`.
    comp: Option<comp::Comp>,
    picture: Option<Picture>,
    frames: u32,
    high_surrogate: Option<u16>,
    /// Menu command id -> path in the Mac app's menu bar (top-level windows only).
    cmds: HashMap<u16, Vec<u32>>,
    /// Child window showing the picture, under the Mac chrome.
    content: isize,
    /// HMENU of the menu bar (0: none); its titles are drawn in the chrome's menu strip.
    menu: isize,
    /// Pixel span of each menu title in the strip (from the last paint), for clicks.
    menu_x: Vec<(i32, i32)>,
    open_menu: Option<usize>,
    /// Pointer over the traffic lights (they show their glyphs).
    hover: bool,
    pressed: Option<chrome::Light>,
    active: bool,
    /// Owned by its parent window (a dialog or panel with a parent): no menu, only the red light.
    owned: bool,
    /// Fullscreen (green light / F11): covers the monitor, chrome hidden; `saved` is the window
    /// rect to return to; `reveal` while the pointer is at the top edge (the bar slides in).
    fullscreen: bool,
    saved: RECT,
    reveal: bool,
}

struct App {
    link: Link,
    rx: Receiver<UiEvent>,
    remotes: HashMap<isize, Remote>,
    by_id: HashMap<u64, isize>,
    ctrl_as_command: bool,
    smoke: Option<Smoke>,
    showcase: Option<Showcase>,
    exit: Option<i32>,
    hinst: isize,
    controller: isize,
    /// HICON per remote application id, shared by all its windows.
    icons: HashMap<String, isize>,
    icons_requested: std::collections::HashSet<String>,
    clipboard: bool,
    /// Text we just put on the Windows clipboard ourselves (its change notification is not echoed).
    clip_applied: Option<String>,
    clip_seq: u64,
    d3d: bool,
    /// Windows are composition windows (rounded, anti-aliased): decided once at start.
    comp: bool,
    launcher: Option<Launcher>,
    /// Remote open panels we replaced with the Windows picker: panel id -> parent window id.
    panels: HashMap<u64, Option<u64>>,
    /// Upload transfer id -> panel id waiting for the uploaded file.
    uploads: HashMap<u64, u64>,
    next_transfer: u64,
    redirect_panels: bool,
    /// Start-menu folder for this Mac's app shortcuts; they exist only while the Mac is connected.
    shortcut_dir: Option<std::path::PathBuf>,
    app_names: Vec<(String, String)>,
    icon_rgba: HashMap<String, (u32, Vec<u8>)>,
    /// Last menu bar seen per remote application; new windows of the app start with it.
    menus: HashMap<String, Vec<MenuNode>>,
    /// Virtual display last asked of the Mac (DisplayConfigure), and what it answered.
    display_req: Option<(u32, u32, u32)>,
    display: Option<(u32, u32)>,
    stats: Stats,
}

/// Stream health, logged every few seconds (pictures shown, skipped as stale, bytes).
struct Stats {
    shown: u64,
    skipped: u64,
    udp: u64,
    bytes: u64,
    decode_us: u64,
    /// capture -> received, agent clock (ms), summed over `lat_n` pictures
    lat_ms: f64,
    lat_n: u64,
    since: Instant,
}

impl Default for Stats {
    fn default() -> Self {
        Self { shown: 0, skipped: 0, udp: 0, bytes: 0, decode_us: 0, lat_ms: 0.0, lat_n: 0, since: Instant::now() }
    }
}

impl Stats {
    fn note(&mut self, m: &net::FrameMeta) {
        self.shown += 1;
        self.udp += m.via_udp as u64;
        self.bytes += m.bytes as u64;
        self.decode_us += m.decode_us as u64;
        if let Some(r) = m.received_agent_us {
            let ms = (r - m.pts_us as i64) as f64 / 1000.0;
            if (0.0..5000.0).contains(&ms) {
                self.lat_ms += ms;
                self.lat_n += 1;
            }
        }
    }
}

thread_local! { static APP: RefCell<Option<App>> = const { RefCell::new(None) }; }

/// Run `f` on the app state if it is not already borrowed (re-entrant window messages fall back to defaults).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn hwnd_of(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

pub fn run(opts: Options) -> i32 {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        // One viewer per user session: a second launch (Start menu, Search, taskbar) hands its app over.
        if !opts.smoke && opts.showcase.is_none() && launcher::forward_to_running_instance(opts.app.as_deref()) {
            return 0;
        }
        let hinst: HINSTANCE = GetModuleHandleW(None).expect("module handle").into();
        let cursor = LoadCursorW(None, IDC_ARROW).ok().unwrap_or_default();
        for (name, proc) in [(w!("RmController"), Some(controller_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)), (w!("RmRemoteWindow"), Some(remote_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (w!("RmContent"), Some(content_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
            (launcher::CLASS, Some(launcher_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT))] {
            let wc = WNDCLASSW { lpfnWndProc: proc, hInstance: hinst, lpszClassName: name, hCursor: cursor, ..Default::default() };
            if RegisterClassW(&wc) == 0 {
                eprintln!("RegisterClassW failed");
                return 1;
            }
        }
        let controller = match CreateWindowExW(WINDOW_EX_STYLE(0), w!("RmController"), w!("rm-controller"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst), None) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("controller window: {e}");
                return 1;
            }
        };
        let ctl = controller.0 as isize;
        let wake = move || {
            let _ = PostMessageW(Some(hwnd_of(ctl)), WM_UI_EVENT, WPARAM(0), LPARAM(0));
        };
        native::load_fonts();
        // decided before any window exists: composition windows, and with them GPU pictures
        let use_comp = opts.d3d && comp::available();
        net::set_decoder(choose_decoder(use_comp));
        eprintln!("video decoder: {:?}", net::decoder_kind());
        let mut opts = opts;
        let (link, rx) = if opts.prompt {
            // ID + password window; a failed attempt shows why and asks again
            let (mut id, mut error) = (crate::connect::last_id(), None::<String>);
            loop {
                let Some((typed, password)) = crate::connect::ask(id.as_deref(), error.as_deref()) else { return 0 };
                let (session, token) = (rm_protocol::session::relay_session(&typed), rm_protocol::session::token(&typed, &password));
                match net::connect_with(&opts.relay, &session, &token, opts.app.as_deref(), false, wake) {
                    Ok(x) => {
                        crate::connect::remember_id(&typed);
                        opts.session = rm_protocol::session::display_id(&typed);
                        break x;
                    }
                    Err(e) => {
                        eprintln!("connect failed: {e}");
                        error = Some(net::friendly_error(&e));
                        id = Some(rm_protocol::session::display_id(&typed));
                    }
                }
            }
        } else {
            match net::connect(&opts.relay, &opts.session, &opts.token, opts.app.as_deref(), wake) {
                Ok(x) => x,
                Err(e) => {
                    eprintln!("connect failed: {e}");
                    return 1;
                }
            }
        };
        eprintln!("connected; waiting for windows");
        // Shortcuts left behind by a crash point at a Mac that may be gone: clean them first.
        let shortcut_dir = if opts.shortcuts { shortcuts::folder() } else { None };
        if let Some(d) = &shortcut_dir {
            shortcuts::remove_all(d);
        }
        link.send(&Message::ListApps);
        let mut launcher = Launcher::create(hinst, opts.app.is_none() && !opts.smoke);
        if let Some(l) = launcher.as_mut() {
            l.status(&format!("Mac {} · relay {}", opts.session, opts.relay));
        }
        if launcher.is_none() {
            eprintln!("warning: launcher window could not be created");
        }
        let smoke = opts.smoke.then(Smoke::new);
        eprintln!("window surfaces: {}", if use_comp { "DirectComposition (rounded corners)" } else if opts.d3d { "Direct3D 11" } else { "GDI" });
        let showcase = opts.showcase.map(Showcase::new);
        APP.with(|a| {
            *a.borrow_mut() = Some(App { link, rx, remotes: HashMap::new(), by_id: HashMap::new(), ctrl_as_command: opts.ctrl_as_command, smoke, showcase, exit: None, hinst: hinst.0 as isize, controller: ctl,
                icons: HashMap::new(), icons_requested: Default::default(), clipboard: opts.clipboard, clip_applied: None, clip_seq: 0, d3d: opts.d3d, comp: use_comp,
                launcher, panels: HashMap::new(), uploads: HashMap::new(), next_transfer: 1, redirect_panels: opts.windows_file_picker,
                shortcut_dir, app_names: vec![], icon_rgba: HashMap::new(), menus: HashMap::new(), display_req: None, display: None, stats: Stats::default() })
        });
        SetTimer(Some(controller), TIMER_ID, 100, None);
        if opts.clipboard && !native::listen_clipboard(controller) {
            eprintln!("warning: clipboard listener unavailable; clipboard sync off");
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        with_app(|a| a.exit.unwrap_or(0)).unwrap_or(0)
    }
}

/// Best decoder this PC has: the GPU (DXVA through Media Foundation, pictures stay in video
/// memory; needs composition windows), else Media Foundation in software, else openh264.
/// RM_DECODER=hardware|platform|software forces one.
fn choose_decoder(use_comp: bool) -> net::DecoderKind {
    use net::DecoderKind::*;
    let forced = match std::env::var("RM_DECODER").ok().as_deref() {
        Some("hardware") => Some(Hardware),
        Some("platform") => Some(Platform),
        Some("software") => Some(Software),
        _ => None,
    };
    if let Some(k) = forced {
        return k;
    }
    // probe on a thread of its own (Media Foundation wants a multithreaded COM apartment)
    std::thread::spawn(move || {
        let gpu = crate::gpu::shared().filter(|g| g.hardware);
        if use_comp && gpu.is_some() && crate::mfdec::MfDecoder::new(gpu).is_ok() {
            Hardware
        } else if crate::mfdec::MfDecoder::new(None).is_ok() {
            Platform
        } else {
            Software
        }
    })
    .join()
    .unwrap_or(Software)
}

/// The Mac is no longer reachable from this viewer: its apps leave the Start menu and Search.
fn on_mac_gone() {
    if let Some(d) = with_app(|a| a.shortcut_dir.clone()).flatten() {
        shortcuts::remove_all(&d);
    }
}

fn sync_shortcuts() {
    let Some((dir, apps)) = with_app(|a| {
        let dir = a.shortcut_dir.clone()?;
        let apps: Vec<shortcuts::AppEntry> = a.app_names.iter().map(|(id, n)| (id.clone(), n.clone(), a.icon_rgba.get(id).cloned())).collect();
        Some((dir, apps))
    })
    .flatten() else { return };
    let Ok(exe) = std::env::current_exe() else { return };
    for r in shortcuts::sync(&dir, &exe, &apps) {
        if let Err(e) = r {
            eprintln!("warning: shortcut not written: {e}");
        }
    }
}

fn quit(code: i32) {
    on_mac_gone();
    with_app(|a| a.exit = Some(code));
    unsafe { PostQuitMessage(code) };
}

// ------------------------------------------------------------------ controller window

unsafe extern "system" fn controller_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_UI_EVENT => {
            drain_events();
            LRESULT(0)
        }
        WM_TIMER => {
            stats_tick();
            smoke_tick();
            showcase_tick();
            LRESULT(0)
        }
        WM_CLIPBOARDUPDATE => {
            on_local_clipboard(hwnd);
            LRESULT(0)
        }
        WM_PICK_FILE => {
            pick_file_for_panel(wp.0 as u64);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------------ launcher

unsafe extern "system" fn launcher_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            with_app(|a| a.launcher.as_ref().map(|l| l.paint(hdc)));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            with_app(|a| a.launcher.as_ref().map(|l| l.fit()));
            LRESULT(0)
        }
        WM_NOTIFY => {
            if let Some(app) = with_app(|a| a.launcher.as_ref().and_then(|l| l.activated(lp))).flatten() {
                launch_app(&app);
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            if let Some(app) = launcher::copydata_app(lp) {
                launch_app(&app);
            }
            LRESULT(1)
        }
        WM_CLOSE => {
            // Closing the launcher keeps running apps open; the viewer exits with the last window.
            if with_app(|a| a.remotes.is_empty()).unwrap_or(true) {
                quit(0);
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Launch an app, or bring its existing main window to the front (like clicking a running app).
fn launch_app(app: &str) {
    let existing = with_app(|a| a.remotes.iter().find(|(_, r)| r.app == app && r.parent.is_none()).map(|(k, _)| *k)).flatten();
    match existing {
        Some(h) => unsafe {
            let _ = ShowWindow(hwnd_of(h), SW_RESTORE);
            let _ = SetForegroundWindow(hwnd_of(h));
        },
        None => {
            with_app(|a| a.link.send(&Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() }));
        }
    }
}

// ------------------------------------------------------------------ file panels

/// The Mac app opened a file panel: show the Windows picker instead, upload the chosen file and
/// make the Mac panel open it. Cancelling the Windows picker cancels the Mac panel.
fn pick_file_for_panel(panel: u64) {
    let Some((owner, smoke)) = with_app(|a| {
        let parent = a.panels.get(&panel).copied()?;
        let owner = parent.and_then(|p| a.by_id.get(&p).copied()).or_else(|| a.launcher.as_ref().map(|l| l.hwnd.0 as isize)).unwrap_or(0);
        Some((owner, a.smoke.is_some()))
    })
    .flatten() else { return };
    let picked = if smoke {
        std::env::var_os("RM_SMOKE_PICK_FILE").map(std::path::PathBuf::from)
    } else {
        native::pick_open_file(hwnd_of(owner), "Open on Mac")
    };
    let Some(path) = picked else {
        with_app(|a| {
            a.panels.remove(&panel);
            a.link.send(&Message::PanelCancel { window_id: panel })
        });
        return;
    };
    let Some((link, tid)) = with_app(|a| {
        let tid = a.next_transfer;
        a.next_transfer += 1;
        a.uploads.insert(tid, panel);
        (a.link.clone(), tid)
    }) else { return };
    eprintln!("uploading {} for Mac open panel {panel}", path.display());
    std::thread::spawn(move || {
        if let Err(e) = net::upload_file(&link, tid, &path) {
            eprintln!("upload failed: {e}");
            link.send(&Message::PanelCancel { window_id: panel });
        }
    });
}

/// Windows clipboard changed: forward it unless we caused the change ourselves.
fn on_local_clipboard(owner: HWND) {
    let Some(text) = native::clipboard_text(owner) else { return };
    with_app(|a| {
        if !a.clipboard || a.clip_applied.as_deref() == Some(text.as_str()) {
            return;
        }
        a.clip_seq += 1;
        a.link.send(&Message::ClipboardSet { seq: a.clip_seq, text });
    });
}

fn stats_tick() {
    with_app(|a| {
        let secs = a.stats.since.elapsed().as_secs_f64();
        if secs >= 5.0 {
            if a.stats.shown > 0 {
                let st = &a.stats;
                let lat = if st.lat_n > 0 { format!("{:.1} ms", st.lat_ms / st.lat_n as f64) } else { "-".into() };
                eprintln!("stream: {:.0} fps shown ({} over UDP), {:.1} Mbit/s, capture->received {lat}, decode {:.1} ms, {} stale pictures skipped",
                    st.shown as f64 / secs, st.udp, st.bytes as f64 * 8.0 / secs / 1e6, st.decode_us as f64 / st.shown.max(1) as f64 / 1000.0, st.skipped);
            }
            a.stats = Stats::default();
        }
    });
}

fn drain_events() {
    let events: Vec<UiEvent> = with_app(|a| a.rx.try_iter().collect()).unwrap_or_default();
    for ev in latest_frames_only(events) {
        handle_event(ev);
    }
}

/// Of several pictures queued for one window only the newest is shown (the others would only
/// add latency); everything else keeps its order.
fn latest_frames_only(events: Vec<UiEvent>) -> Vec<UiEvent> {
    let mut last: HashMap<u64, usize> = HashMap::new();
    for (i, e) in events.iter().enumerate() {
        if let UiEvent::Frame { id, .. } = e {
            last.insert(*id, i);
        }
    }
    let skipped = events.iter().enumerate().filter(|(i, e)| matches!(e, UiEvent::Frame { id, .. } if last.get(id) != Some(i))).count();
    if skipped > 0 {
        with_app(|a| a.stats.skipped += skipped as u64);
    }
    events.into_iter().enumerate().filter(|(i, e)| !matches!(e, UiEvent::Frame { id, .. } if last.get(id) != Some(i))).map(|(_, e)| e).collect()
}

fn handle_event(ev: UiEvent) {
    match ev {
        UiEvent::WindowCreated { id, app, title, x, y, w, h, parent, role } => {
            let redirect = role == WindowRole::OpenPanel && with_app(|a| a.redirect_panels).unwrap_or(false);
            if redirect {
                let ctl = with_app(|a| {
                    a.panels.insert(id, parent);
                    a.controller
                })
                .unwrap_or(0);
                unsafe { let _ = PostMessageW(Some(hwnd_of(ctl)), WM_PICK_FILE, WPARAM(id as usize), LPARAM(0)); }
            } else {
                create_remote_window(id, &app, &title, (x, y, w, h), parent, role);
            }
        }
        UiEvent::Apps(apps) => {
            with_app(|a| {
                let rows: Vec<(String, String, bool)> = apps.iter().map(|x| (x.id.clone(), x.name.clone(), x.available)).collect();
                a.app_names = apps.iter().filter(|x| x.available).map(|x| (x.id.clone(), x.name.clone())).collect();
                if let Some(l) = a.launcher.as_mut() {
                    l.set_apps(&rows);
                }
                for x in &apps {
                    if let Some(icon) = a.icons.get(&x.id) {
                        if let Some(l) = a.launcher.as_ref() {
                            l.set_icon(&x.id, HICON(*icon as *mut c_void));
                        }
                    } else if a.icons_requested.insert(x.id.clone()) {
                        a.link.send(&Message::GetAppIcon { application_id: x.id.clone() });
                    }
                }
            });
            sync_shortcuts();
        }
        UiEvent::MenuBar { app, menus } => {
            let windows = with_app(|a| {
                if a.menus.get(&app) == Some(&menus) {
                    return vec![]; // unchanged (it is re-sent on every focus): no rebuild, no flicker
                }
                a.menus.insert(app.clone(), menus.clone());
                a.remotes.iter().filter(|(_, r)| r.app == app && !r.owned).map(|(k, _)| *k).collect()
            })
            .unwrap_or_default();
            for w in windows {
                set_window_menu(hwnd_of(w), &menus);
            }
        }
        UiEvent::Display { available, width, height, reason } => {
            eprintln!("virtual display on the Mac: available={available} {width}x{height}pt reason={reason:?}");
            with_app(|a| a.display = available.then_some((width, height)));
        }
        UiEvent::Uploaded { transfer_id, remote_path } => {
            with_app(|a| {
                if let Some(panel) = a.uploads.remove(&transfer_id) {
                    a.panels.remove(&panel);
                    a.link.send(&Message::PanelChooseFile { window_id: panel, remote_path });
                }
            });
        }
        UiEvent::UploadFailed { transfer_id, reason } => {
            eprintln!("upload {transfer_id} failed on the Mac: {reason}");
            with_app(|a| {
                if let Some(panel) = a.uploads.remove(&transfer_id) {
                    a.panels.remove(&panel);
                    a.link.send(&Message::PanelCancel { window_id: panel });
                }
            });
        }
        UiEvent::Icon { app, size, rgba } => {
            let Some(icon) = native::make_icon(size, &rgba) else { return };
            let known = with_app(|a| {
                a.icon_rgba.insert(app.clone(), (size, rgba.clone()));
                a.app_names.iter().any(|(id, _)| *id == app)
            })
            .unwrap_or(false);
            if known {
                sync_shortcuts();
            }
            let windows: Vec<isize> = with_app(|a| {
                a.icons.insert(app.clone(), icon.0 as isize);
                if let Some(l) = a.launcher.as_ref() {
                    l.set_icon(&app, icon);
                }
                a.remotes.iter().filter(|(_, r)| r.app == app).map(|(k, _)| *k).collect()
            })
            .unwrap_or_default();
            for w in windows {
                native::set_window_icon(hwnd_of(w), icon);
            }
        }
        UiEvent::Clipboard(text) => {
            let owner = with_app(|a| a.clipboard.then(|| {
                a.clip_applied = Some(text.clone());
                a.controller
            }))
            .flatten();
            if let Some(owner) = owner {
                if !native::set_clipboard_text(hwnd_of(owner), &text) {
                    eprintln!("warning: could not set the Windows clipboard");
                }
            }
        }
        UiEvent::Title { id, title } => {
            if let Some(h) = with_app(|a| a.by_id.get(&id).copied()).flatten() {
                unsafe { let _ = SetWindowTextW(hwnd_of(h), &HSTRING::from(title)); }
            }
        }
        UiEvent::Resized { id, w, h } => {
            let target = with_app(|a| {
                let key = *a.by_id.get(&id)?;
                let r = a.remotes.get_mut(&key)?;
                r.rw = w;
                r.rh = h;
                Some((key, r.scale, r.maximized || r.fullscreen))
            })
            .flatten();
            if let Some((key, scale, maximized)) = target {
                if !maximized {
                    resize_content(hwnd_of(key), (w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
                }
            }
        }
        UiEvent::Frame { id, picture, meta } => {
            let key = with_app(|a| {
                let key = *a.by_id.get(&id)?;
                let r = a.remotes.get_mut(&key)?;
                r.frames += 1;
                a.stats.note(&meta);
                let gpu_ok = match (&picture, r.comp.as_mut(), r.presenter.as_mut()) {
                    (net::Pic::Gpu(g), Some(c), _) => c.present_gpu(g),
                    (net::Pic::Cpu(p), Some(c), _) => c.present(p),
                    (net::Pic::Cpu(p), None, Some(d)) => d.present(p),
                    _ => false,
                };
                if !gpu_ok && r.presenter.take().is_some() {
                    eprintln!("Direct3D presenter failed; falling back to GDI for window {id}");
                }
                if let net::Pic::Cpu(p) = picture {
                    r.picture = Some(p);
                }
                Some((key, gpu_ok))
            })
            .flatten();
            if let Some((k, false)) = key {
                if let Some(c) = content_of(hwnd_of(k)) {
                    unsafe { let _ = InvalidateRect(Some(c), None, false); }
                }
            }
        }
        UiEvent::Destroyed { id } => {
            if let Some(h) = with_app(|a| a.by_id.remove(&id)).flatten() {
                with_app(|a| a.remotes.remove(&h));
                unsafe { let _ = DestroyWindow(hwnd_of(h)); }
            }
            with_app(|a| a.panels.remove(&id));
            smoke_note_destroyed(id);
            // Last remote window gone and the launcher is not on screen: the viewer is done.
            let idle = with_app(|a| a.smoke.is_none() && a.showcase.is_none() && a.remotes.is_empty() && !a.launcher.as_ref().is_some_and(|l| unsafe { IsWindowVisible(l.hwnd).as_bool() })).unwrap_or(false);
            if idle {
                quit(0);
            }
        }
        UiEvent::AppExited(app) => eprintln!("remote app exited: {app}"),
        UiEvent::Notice(n) => eprintln!("notice: {n}"),
        UiEvent::Disconnected(why) => {
            eprintln!("disconnected: {why}");
            on_mac_gone();
            quit(if with_app(|a| a.smoke.is_some()).unwrap_or(false) { 1 } else { 0 });
        }
    }
}

fn create_remote_window(id: u64, app: &str, title: &str, (x, y, w, h): (i32, i32, u32, u32), parent: Option<u64>, role: WindowRole) {
    unsafe {
        let (hinst, owner) = with_app(|a| (a.hinst, parent.and_then(|p| a.by_id.get(&p).copied()))).unwrap_or((0, None));
        // Dialogs, sheets and panels become *owned* windows: they stay above their parent,
        // minimise with it and do not get their own taskbar button, as on the Mac.
        let owned = owner.is_some() && role != WindowRole::Window;
        // No Windows caption: the viewer draws a Mac title bar (traffic lights) itself; the thick
        // frame keeps resizing, snapping and the shadow.
        let mut style = WS_POPUP.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;
        if !owned {
            style |= WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;
        }
        let hinst = HINSTANCE(hinst as *mut c_void);
        let use_comp = with_app(|a| a.comp).unwrap_or(false);
        let ex = if use_comp { WS_EX_NOREDIRECTIONBITMAP } else { WINDOW_EX_STYLE(0) };
        let hwnd = CreateWindowExW(ex, w!("RmRemoteWindow"), &HSTRING::from(title), WINDOW_STYLE(style), 40 + x.max(0), 40 + y.max(0),
            w as i32, h as i32, if owned { owner.map(hwnd_of) } else { None }, None, Some(hinst), None);
        let Ok(hwnd) = hwnd else {
            eprintln!("CreateWindowExW failed for remote window {id}");
            return;
        };
        let content = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RmContent"), w!(""), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0), 0, 0, w as i32, h as i32,
            Some(hwnd), None, Some(hinst), None);
        let Ok(content) = content else {
            eprintln!("content window failed for remote window {id}");
            let _ = DestroyWindow(hwnd);
            return;
        };
        if use_comp {
            // the composition clip makes the (larger, anti-aliased) rounded corners; DWM's own
            // rounding would add its 1px highlight in the transparent corner
            native::round_corners_off(hwnd);
            native::no_border(hwnd);
            // the frame extended 1px into the client: DWM then renders its top frame line as
            // part of the (transparent) composition instead of drawing it in the corners
            native::corner_remedy(hwnd, "extend-frame", 0);
        } else {
            native::round_corners(hwnd);
        }
        let aumid = format!("RemoteMac.{}", app.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
        if !owned && !native::set_app_user_model_id(hwnd, &aumid) {
            // Taskbar identity before the window is shown: own group + icon per remote application.
            eprintln!("warning: could not set AppUserModelID {aumid}");
        }
        let scale = native::dpi_scale(hwnd);
        let (cached, parent_origin) = with_app(|a| {
            a.remotes.insert(hwnd.0 as isize, Remote { id, app: app.into(), role, parent, rx: x, ry: y, rw: w, rh: h, scale, maximized: false, presenter: None, comp: None, picture: None, frames: 0,
                high_surrogate: None, cmds: HashMap::new(), content: content.0 as isize, menu: 0, menu_x: vec![], open_menu: None, hover: false, pressed: None, active: false, owned, fullscreen: false, saved: RECT::default(), reveal: false });
            a.by_id.insert(id, hwnd.0 as isize);
            let cached = a.icons.get(app).copied();
            if cached.is_none() && a.icons_requested.insert(app.to_string()) {
                a.link.send(&Message::GetAppIcon { application_id: app.into() });
            }
            let parent_origin = owner.and_then(|o| a.remotes.get(&o)).map(|p| (p.rx, p.ry));
            (cached, parent_origin)
        })
        .unwrap_or((None, None));
        if !owned {
            // The Mac app's menu bar comes down into its window, like on the Mac but per window.
            let menus = with_app(|a| {
                let m = a.menus.get(app).cloned();
                if m.is_none() {
                    a.link.send(&Message::GetMenuBar { application_id: app.into() });
                }
                m
            })
            .flatten();
            if let Some(m) = menus {
                set_window_menu(hwnd, &m);
            }
        }
        if let Some(icon) = cached {
            native::set_window_icon(hwnd, HICON(icon as *mut c_void));
        }
        resize_content(hwnd, (w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
        if let (true, Some(o), Some((px, py))) = (owned, owner, parent_origin) {
            // keep the dialog where the Mac put it relative to its parent
            let mut orc = RECT::default();
            let _ = GetWindowRect(hwnd_of(o), &mut orc);
            let nx = orc.left + ((x - px) as f64 * scale).round() as i32;
            let ny = orc.top + ((y - py) as f64 * scale).round() as i32;
            let _ = SetWindowPos(hwnd, None, nx, ny, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        }
        layout(hwnd);
        keep_on_screen(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let compositor = if use_comp { comp::Comp::new(hwnd) } else { None };
        let presenter = if compositor.is_none() && with_app(|a| a.d3d).unwrap_or(false) { d3d::Presenter::new(content, w, h) } else { None };
        let renderer = compositor.as_ref().map(|c| c.kind).or(presenter.as_ref().map(|p| p.kind)).unwrap_or("gdi");
        with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| { r.presenter = presenter; r.comp = compositor }));
        layout(hwnd);
        let _ = InvalidateRect(Some(hwnd), None, false);
        eprintln!("window created id={id} app={app} role={role:?} parent={parent:?} renderer={renderer} {w}x{h}pt scale={scale} title={title:?}");
        if app == DESKTOP_APP {
            // the whole Mac: straight to fullscreen, as a remote desktop is used
            toggle_fullscreen(hwnd);
        }
    }
}

/// Move a window (keeping its size) so its top-left part is inside the monitor's work area:
/// Mac screen positions can be off a smaller Windows desktop.
fn keep_on_screen(hwnd: HWND) {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut r = RECT::default();
        if !GetMonitorInfoW(mon, &mut info).as_bool() || GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let w = info.rcWork;
        let x = r.left.min(w.right - (r.right - r.left)).max(w.left);
        let y = r.top.min(w.bottom - (r.bottom - r.top)).max(w.top);
        if (x, y) != (r.left, r.top) {
            let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// The child window showing the remote picture (everything under the Mac chrome).
fn content_of(frame: HWND) -> Option<HWND> {
    with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| hwnd_of(r.content))).flatten()
}

/// Height of the chrome (title bar, plus the menu strip when the app has a menu bar).
fn bar_px(frame: HWND) -> i32 {
    with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| if r.fullscreen && !r.reveal { 0 } else { chrome::bar_height(r.scale, r.menu != 0) })).flatten().unwrap_or(0)
}

fn is_fullscreen(frame: HWND) -> bool {
    with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.fullscreen)).flatten().unwrap_or(false)
}

fn monitor_rect(hwnd: HWND) -> RECT {
    unsafe {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
        info.rcMonitor
    }
}

/// Slide/zoom the window from `a` to `b` (screen rects) like the Mac's fullscreen transition.
fn animate(frame: HWND, a: RECT, b: RECT) {
    const STEPS: u32 = 14;
    for i in 1..=STEPS {
        let (l, t, r, btm) = chrome::lerp_rect((a.left, a.top, a.right, a.bottom), (b.left, b.top, b.right, b.bottom), i as f64 / STEPS as f64);
        unsafe {
            let _ = SetWindowPos(frame, Some(HWND_TOP), l, t, r - l, btm - t, SWP_NOACTIVATE | SWP_NOCOPYBITS);
            let _ = UpdateWindow(frame);
        }
        std::thread::sleep(Duration::from_millis(14));
    }
}

/// Green light / F11: fullscreen on this monitor, with the Mac app sized to it exactly (on the
/// Mac's virtual display of this monitor's size), and back.
/// The remote "application" that is the whole Mac screen.
const DESKTOP_APP: &str = "desktop";

fn toggle_fullscreen(frame: HWND) {
    let Some((on, saved, id, scale, desktop)) = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| (r.fullscreen, r.saved, r.id, r.scale, r.app == DESKTOP_APP))).flatten() else { return };
    unsafe {
        if !on {
            let mut from = RECT::default();
            let _ = GetWindowRect(frame, &mut from);
            let mon = monitor_rect(frame);
            // the Mac gets a display like this monitor (once; again if the monitor changed)
            let req = chrome::display_request(mon.right - mon.left, mon.bottom - mon.top, scale);
            with_app(|a| {
                // the Mac Desktop is the Mac's own screen: it is only scaled, nothing to resize there
                if !desktop && a.display_req != Some(req) {
                    a.display_req = Some(req);
                    a.link.send(&Message::DisplayConfigure { width: req.0, height: req.1, scale: req.2 });
                }
                if let Some(r) = a.remotes.get_mut(&(frame.0 as isize)) {
                    r.fullscreen = true;
                    r.reveal = false;
                    r.saved = from;
                }
            });
            native::round_corners_off(frame);
            let _ = SetWindowPos(frame, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            animate(frame, from, mon);
            layout(frame);
            if !desktop {
                with_app(|a| a.link.send(&Message::WindowFullscreen { window_id: id, on: true }));
            }
        } else {
            let mut from = RECT::default();
            let _ = GetWindowRect(frame, &mut from);
            with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).map(|r| { r.reveal = true }));
            layout(frame);
            animate(frame, from, saved);
            with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).map(|r| { r.fullscreen = false; r.reveal = false }));
            let _ = SetWindowPos(frame, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = SetWindowPos(frame, None, saved.left, saved.top, saved.right - saved.left, saved.bottom - saved.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if !with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.comp.is_some())).flatten().unwrap_or(false) {
                native::round_corners(frame);
            }
            layout(frame);
            if !desktop {
                with_app(|a| a.link.send(&Message::WindowFullscreen { window_id: id, on: false }));
            }
        }
        let _ = InvalidateRect(Some(frame), None, false);
    }
}

/// In fullscreen the bar slides in while the pointer is at the top edge, as the Mac's menu bar does.
fn set_reveal(frame: HWND, reveal: bool) {
    let changed = with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).and_then(|r| (r.fullscreen && r.reveal != reveal).then(|| r.reveal = reveal))).flatten().is_some();
    if changed {
        layout(frame);
        invalidate_chrome(frame);
    }
}

/// Place the picture window under the chrome.
fn layout(frame: HWND) {
    let Some(content) = content_of(frame) else { return };
    let (cw, ch) = client_size(frame);
    let bar = bar_px(frame);
    unsafe { let _ = MoveWindow(content, 0, bar, cw, (ch - bar).max(1), true); }
    let square = unsafe { IsZoomed(frame).as_bool() } || is_fullscreen(frame);
    with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).map(|r| {
        let radius = if square { 0.0 } else { (chrome::CORNER_RADIUS * r.scale) as f32 };
        if let Some(c) = r.comp.as_mut() {
            c.layout(cw, ch, bar, radius);
        }
    }));
    if bar == 0 {
        with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).and_then(|r| r.comp.as_mut()).map(|c| c.set_chrome(0, 0, &[])));
    }
}

/// Current picture size of the window expressed in Mac points.
fn client_points(frame: HWND) -> (u32, u32, f64) {
    let (cw, ch) = content_of(frame).map(client_size).unwrap_or_else(|| client_size(frame));
    let scale = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.scale)).flatten().unwrap_or(1.0);
    ((cw as f64 / scale).round().max(1.0) as u32, (ch as f64 / scale).round().max(1.0) as u32, scale)
}

fn request_remote_resize(frame: HWND) {
    if is_fullscreen(frame) {
        return; // the Mac sizes a fullscreen window itself (to the virtual display)
    }
    let (pw, ph, _) = client_points(frame);
    send_for(frame, |r, _| (pw.abs_diff(r.rw) > 2 || ph.abs_diff(r.rh) > 2).then_some(Message::WindowResizeRequest { window_id: r.id, width: pw, height: ph }));
}

/// Size the window so its picture area is `w`x`h` pixels.
fn resize_content(frame: HWND, w: i32, h: i32) {
    let h = h + bar_px(frame);
    unsafe {
        let mut cur = RECT::default();
        let _ = GetClientRect(frame, &mut cur);
        if (cur.right - w).abs() <= 2 && (cur.bottom - h).abs() <= 2 {
            return;
        }
        let (ow, oh) = outer_for_client(frame, w, h);
        let _ = SetWindowPos(frame, None, 0, 0, ow, oh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Outer window size giving a `w`x`h` client area, from the window's current frame.
fn outer_for_client(hwnd: HWND, w: i32, h: i32) -> (i32, i32) {
    let (mut wr, mut cr) = (RECT::default(), RECT::default());
    unsafe {
        let _ = GetWindowRect(hwnd, &mut wr);
        let _ = GetClientRect(hwnd, &mut cr);
    }
    (w + (wr.right - wr.left) - cr.right, h + (wr.bottom - wr.top) - cr.bottom)
}

fn build_popup(nodes: &[MenuNode], cc: bool, ids: &mut std::vec::IntoIter<(u16, Vec<u32>)>) -> HMENU {
    unsafe {
        let m = CreatePopupMenu().unwrap_or_default();
        for n in nodes {
            if n.separator {
                let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                continue;
            }
            let text = HSTRING::from(menu::label(n, cc));
            let grayed = if n.enabled { MENU_ITEM_FLAGS(0) } else { MF_GRAYED };
            if n.children.is_empty() {
                let id = ids.next().map(|(id, _)| id).unwrap_or(0);
                let _ = AppendMenuW(m, MF_STRING | grayed, id as usize, &text);
            } else {
                let sub = build_popup(&n.children, cc, ids);
                let _ = AppendMenuW(m, MF_POPUP | grayed, sub.0 as usize, &text);
            }
        }
        m
    }
}

/// Give a top-level window the Mac app's menu bar (drawn in the strip under the title bar,
/// each title opening a native popup menu), keeping the picture size.
fn set_window_menu(frame: HWND, menus: &[MenuNode]) {
    let cc = with_app(|a| a.ctrl_as_command).unwrap_or(true);
    let table = menu::commands(menus);
    unsafe {
        let content = content_of(frame).map(client_size).unwrap_or((0, 0));
        let bar = CreateMenu().unwrap_or_default();
        // ids are handed out in the same depth-first order `menu::commands` uses
        let mut ids = table.clone().into_iter();
        for top in menus {
            let sub = build_popup(&top.children, cc, &mut ids);
            let grayed = if top.enabled { MENU_ITEM_FLAGS(0) } else { MF_GRAYED };
            let _ = AppendMenuW(bar, MF_POPUP | grayed, sub.0 as usize, &HSTRING::from(top.title.replace('&', "&&")));
        }
        let old = with_app(|a| {
            let r = a.remotes.get_mut(&(frame.0 as isize))?;
            r.cmds = table.into_iter().collect();
            r.menu_x.clear();
            Some(std::mem::replace(&mut r.menu, bar.0 as isize))
        })
        .flatten();
        if let Some(old) = old.filter(|o| *o != 0) {
            let _ = DestroyMenu(HMENU(old as *mut c_void));
        }
        if content.0 > 0 && content.1 > 0 && !IsZoomed(frame).as_bool() {
            resize_content(frame, content.0, content.1);
        }
        layout(frame);
        let _ = InvalidateRect(Some(frame), None, false);
    }
}

/// A menu item was chosen: the Mac app runs it.
fn invoke_menu_cmd(frame: HWND, id: u16) {
    with_app(|a| {
        if let Some(r) = a.remotes.get(&(frame.0 as isize)) {
            if let Some(path) = r.cmds.get(&id) {
                a.link.send(&Message::MenuInvoke { application_id: r.app.clone(), path: path.clone() });
            }
        }
    });
}

/// Open menu `i` of the strip as a popup under its title (modal until an item is chosen).
fn open_menu_popup(frame: HWND, i: usize) {
    let Some((menu, (x0, _), bar)) = with_app(|a| {
        let r = a.remotes.get_mut(&(frame.0 as isize))?;
        let span = *r.menu_x.get(i)?;
        r.open_menu = Some(i);
        Some((r.menu, span, chrome::bar_height(r.scale, true)))
    })
    .flatten() else { return };
    unsafe {
        let _ = InvalidateRect(Some(frame), None, false);
        let _ = UpdateWindow(frame);
        let mut pt = POINT { x: x0, y: bar };
        let _ = ClientToScreen(frame, &mut pt);
        let sub = GetSubMenu(HMENU(menu as *mut c_void), i as i32);
        let cmd = if sub.is_invalid() { 0 } else { TrackPopupMenuEx(sub, (TPM_LEFTALIGN | TPM_TOPALIGN | TPM_RETURNCMD).0, pt.x, pt.y, frame, None).0 };
        with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).map(|r| r.open_menu = None));
        let _ = InvalidateRect(Some(frame), None, false);
        if cmd > 0 {
            invoke_menu_cmd(frame, cmd as u16);
        }
    }
}

// ------------------------------------------------------------------ remote windows

fn send_for(frame: HWND, f: impl FnOnce(&Remote, bool) -> Option<Message>) {
    with_app(|a| {
        if let Some(r) = a.remotes.get(&(frame.0 as isize)) {
            if let Some(m) = f(r, a.ctrl_as_command) {
                a.link.send(&m);
            }
        }
    });
}

fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rc = RECT::default();
    unsafe { let _ = GetClientRect(hwnd, &mut rc); }
    (rc.right, rc.bottom)
}

fn lp_xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

fn current_mods() -> Mods {
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) } < 0;
    Mods { ctrl: down(VK_CONTROL), alt: down(VK_MENU), shift: down(VK_SHIFT), win: down(VK_LWIN) || down(VK_RWIN) }
}

/// Re-present the last picture through the GPU presenter. True if handled.
fn repaint_gpu(frame: HWND) -> bool {
    with_app(|a| {
        let r = a.remotes.get_mut(&(frame.0 as isize))?;
        let (p, pic) = (r.presenter.as_mut()?, r.picture.as_ref()?);
        Some(p.present(pic))
    })
    .flatten()
    .unwrap_or(false)
}

fn paint_into(frame: HWND, hdc: HDC, (cw, ch): (i32, i32)) {
    unsafe {
        let drew = with_app(|a| {
            let Some(p) = a.remotes.get(&(frame.0 as isize)).and_then(|r| r.picture.as_ref()) else { return false };
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: p.width as i32, biHeight: -(p.height as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            SetStretchBltMode(hdc, HALFTONE);
            StretchDIBits(hdc, 0, 0, cw, ch, 0, 0, p.width as i32, p.height as i32, Some(p.bgra.as_ptr() as *const c_void), &bmi, DIB_RGB_COLORS, SRCCOPY);
            true
        })
        .unwrap_or(false);
        if !drew {
            FillRect(hdc, &RECT { left: 0, top: 0, right: cw, bottom: ch }, HBRUSH(GetStockObject(WHITE_BRUSH).0));
        }
    }
}

fn rgb((r, g, b): chrome::Rgb) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn fill(hdc: HDC, rc: RECT, c: chrome::Rgb) {
    unsafe {
        let b = CreateSolidBrush(rgb(c));
        FillRect(hdc, &rc, b);
        let _ = DeleteObject(b.into());
    }
}

/// Inter at `weight` (bundled; Segoe UI if it could not be loaded).
fn ui_font(px: i32, weight: i32) -> HFONT {
    let face = native::ui_face(weight);
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from(face)) }
}

/// Paint the Mac chrome (title bar with traffic lights and title; menu strip) into `hdc`.
fn paint_chrome(frame: HWND, hdc: HDC) {
    struct View { scale: f64, active: bool, hover: bool, dialog: bool, menu: isize, open: Option<usize> }
    let Some(v) = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| View { scale: r.scale, active: r.active, hover: r.hover, dialog: r.owned, menu: r.menu, open: r.open_menu })).flatten() else { return };
    let (cw, _) = client_size(frame);
    let bar = bar_px(frame);
    if cw <= 0 || bar <= 0 {
        return;
    }
    unsafe {
        // double-buffered: draw into a bitmap, then one blit
        let mem = CreateCompatibleDC(Some(hdc));
        let dib = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: cw, biHeight: -bar, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(bmp) = CreateDIBSection(Some(mem), &dib, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            return;
        };
        let old = SelectObject(mem, bmp.into());
        let tbg = chrome::title_bg(v.active);
        fill(mem, RECT { left: 0, top: 0, right: cw, bottom: bar }, tbg);
        // traffic lights (a dialog's minimise/zoom are greyed out, as on the Mac)
        let d = chrome::light_size(v.scale);
        for l in chrome::LIGHTS {
            let enabled = !(v.dialog && l != chrome::Light::Close);
            let px = chrome::light_sprite(d, l, v.active && enabled, v.hover && enabled, tbg);
            let (ox, oy) = chrome::light_origin(l, v.scale);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: d, biHeight: -d, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            SetDIBitsToDevice(mem, ox, oy, d as u32, d as u32, 0, 0, 0, d as u32, px.as_ptr() as *const c_void, &bmi, DIB_RGB_COLORS);
        }
        SetBkMode(mem, TRANSPARENT);
        let bold = ui_font((13.0 * v.scale).round() as i32, 600);
        let regular = ui_font((13.0 * v.scale).round() as i32, 400);
        let oldf = SelectObject(mem, bold.into());
        // menus right after the lights: the app's name in bold, then its menus
        let lw = chrome::lights_width(v.scale);
        let mut spans = vec![];
        let mut menus_end = lw;
        if v.menu != 0 {
            let hm = HMENU(v.menu as *mut c_void);
            let pad = (8.0 * v.scale).round() as i32;
            let mut x = lw - pad / 2;
            for i in 0..GetMenuItemCount(Some(hm)).max(0) {
                let mut t = [0u16; 128];
                let len = GetMenuStringW(hm, i as u32, Some(&mut t), MF_BYPOSITION).max(0) as usize;
                let text: Vec<u16> = String::from_utf16_lossy(&t[..len]).replace("&&", "&").encode_utf16().collect();
                SelectObject(mem, if i == 0 { bold.into() } else { regular.into() });
                let mut sz = SIZE::default();
                let _ = GetTextExtentPoint32W(mem, &text, &mut sz);
                let span = (x, x + sz.cx + 2 * pad);
                if span.1 > cw - (8.0 * v.scale) as i32 {
                    break; // like the Mac, menus that do not fit are left out
                }
                let open = v.open == Some(i as usize);
                if open {
                    let b = CreateSolidBrush(rgb(chrome::MENU_HIGHLIGHT));
                    let oldb = SelectObject(mem, b.into());
                    let pen = SelectObject(mem, GetStockObject(NULL_PEN));
                    let r = (12.0 * v.scale) as i32;
                    let m = (8.0 * v.scale) as i32;
                    let _ = RoundRect(mem, span.0, m, span.1, bar - m, r, r);
                    SelectObject(mem, pen);
                    SelectObject(mem, oldb);
                    let _ = DeleteObject(b.into());
                }
                let state = GetMenuState(hm, i as u32, MF_BYPOSITION);
                SetTextColor(mem, rgb(chrome::menu_fg(v.active, state & MF_GRAYED.0 == 0)));
                let mut text = text;
                let mut rc = RECT { left: span.0, top: 0, right: span.1, bottom: bar };
                DrawTextW(mem, &mut text, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
                spans.push(span);
                x = span.1;
                menus_end = x;
            }
        }
        // the title, centred on the window when there is room, else in the space left of the menus
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(frame, &mut buf).max(0) as usize;
        SelectObject(mem, bold.into());
        let mut sz = SIZE::default();
        let _ = GetTextExtentPoint32W(mem, &buf[..n], &mut sz);
        let gap = (16.0 * v.scale) as i32;
        let centred = (cw - sz.cx) / 2;
        let mut rc = if centred > menus_end + gap {
            RECT { left: centred, top: 0, right: centred + sz.cx + 2, bottom: bar }
        } else {
            RECT { left: menus_end + gap, top: 0, right: cw - gap, bottom: bar }
        };
        if rc.right - rc.left > (40.0 * v.scale) as i32 {
            SetTextColor(mem, rgb(chrome::title_fg(v.active)));
            DrawTextW(mem, &mut buf[..n], &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
        }
        fill(mem, RECT { left: 0, top: bar - 1, right: cw, bottom: bar }, chrome::HAIRLINE);
        SelectObject(mem, oldf);
        let _ = DeleteObject(bold.into());
        let _ = DeleteObject(regular.into());
        let _ = GdiFlush();
        let composed = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.comp.is_some())).flatten().unwrap_or(false);
        if composed {
            // GDI leaves alpha at 0: the bar is opaque
            let px = std::slice::from_raw_parts_mut(bits as *mut u8, (cw * bar * 4) as usize);
            px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
            let px = px.to_vec();
            with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).and_then(|r| r.comp.as_mut()).map(|c| c.set_chrome(cw, bar, &px)));
        } else {
            let _ = BitBlt(hdc, 0, 0, cw, bar, Some(mem), 0, 0, SRCCOPY);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        with_app(|a| a.remotes.get_mut(&(frame.0 as isize)).map(|r| r.menu_x = spans));
    }
}

fn invalidate_chrome(frame: HWND) {
    let bar = bar_px(frame);
    let (cw, _) = client_size(frame);
    unsafe { let _ = InvalidateRect(Some(frame), Some(&RECT { left: 0, top: 0, right: cw, bottom: bar }), false); }
}

/// Resize border thickness (pixels) at this window's DPI.
fn frame_border(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

fn light_action(frame: HWND, l: chrome::Light) {
    let dialog = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.owned)).flatten().unwrap_or(false);
    unsafe {
        match l {
            chrome::Light::Close => { let _ = PostMessageW(Some(frame), WM_CLOSE, WPARAM(0), LPARAM(0)); }
            _ if dialog => {}
            chrome::Light::Minimize => { let _ = ShowWindow(frame, SW_MINIMIZE); }
            chrome::Light::Zoom => toggle_fullscreen(frame), // like the Mac: green = fullscreen
        }
    }
}

/// Top-level window of a remote window: Mac chrome, window management, focus.
unsafe extern "system" fn remote_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wp.0 != 0 => {
            // Keep the left/right/bottom resize borders; the top belongs to our title bar.
            let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
            let orig = p.rgrc[0];
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if IsZoomed(hwnd).as_bool() || is_fullscreen(hwnd) {
                p.rgrc[0] = orig; // maximised to the work area exactly / fullscreen: no borders
            } else {
                p.rgrc[0].top = orig.top;
            }
            r
        }
        WM_GETMINMAXINFO => {
            let mi = &mut *(lp.0 as *mut MINMAXINFO);
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut info).as_bool() {
                let (w, m) = (info.rcWork, info.rcMonitor);
                mi.ptMaxPosition = POINT { x: w.left - m.left, y: w.top - m.top };
                mi.ptMaxSize = POINT { x: w.right - w.left, y: w.bottom - w.top };
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if r.0 as u32 != HTCLIENT {
                return r;
            }
            let mut pt = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let scale = with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.scale)).flatten().unwrap_or(1.0);
            let (cw, _) = client_size(hwnd);
            let b = frame_border(hwnd);
            let fullscreen = is_fullscreen(hwnd);
            let on_menu = with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.menu_x.iter().any(|(a, b)| pt.x >= *a && pt.x < *b))).flatten().unwrap_or(false);
            let code = if !IsZoomed(hwnd).as_bool() && !fullscreen && pt.y < b {
                if pt.x < 2 * b { HTTOPLEFT } else if pt.x >= cw - 2 * b { HTTOPRIGHT } else { HTTOP }
            } else if pt.y < bar_px(hwnd) && !chrome::over_lights(pt.x, pt.y, scale) && !on_menu && !fullscreen {
                HTCAPTION
            } else {
                HTCLIENT
            };
            LRESULT(code as isize)
        }
        WM_NCACTIVATE => {
            let active = wp.0 != 0;
            with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| r.active = active));
            invalidate_chrome(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_SETTEXT => {
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            invalidate_chrome(hwnd);
            r
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_chrome(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_chrome(hwnd, HDC(wp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = lp_xy(lp);
            let changed = with_app(|a| {
                let r = a.remotes.get_mut(&(hwnd.0 as isize))?;
                let over = chrome::over_lights(x, y, r.scale);
                Some(std::mem::replace(&mut r.hover, over) != over)
            })
            .flatten()
            .unwrap_or(false);
            if changed {
                invalidate_chrome(hwnd);
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut tme);
            }
            LRESULT(0)
        }
        WM_MOUSE_LEAVE => {
            with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| r.hover = false));
            invalidate_chrome(hwnd);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            if pt.y > bar_px(hwnd) || pt.y < 0 {
                set_reveal(hwnd, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = lp_xy(lp);
            let hit = with_app(|a| {
                let r = a.remotes.get_mut(&(hwnd.0 as isize))?;
                if let Some(l) = chrome::hit_light(x, y, r.scale) {
                    r.pressed = Some(l);
                    return Some((Some(l), None));
                }
                let menu = (y < chrome::bar_height(r.scale, r.menu != 0)).then(|| r.menu_x.iter().position(|(a, b)| x >= *a && x < *b)).flatten();
                Some((None, menu))
            })
            .flatten();
            match hit {
                Some((Some(_), _)) => { SetCapture(hwnd); }
                Some((None, Some(i))) => open_menu_popup(hwnd, i),
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = lp_xy(lp);
            let _ = ReleaseCapture();
            let act = with_app(|a| {
                let r = a.remotes.get_mut(&(hwnd.0 as isize))?;
                let pressed = r.pressed.take()?;
                (chrome::hit_light(x, y, r.scale) == Some(pressed)).then_some(pressed)
            })
            .flatten();
            if let Some(l) = act {
                light_action(hwnd, l);
            }
            LRESULT(0)
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR => {
            // keyboard focus normally sits in the picture window; forward if the frame has it
            match content_of(hwnd) {
                Some(c) => SendMessageW(c, msg, Some(wp), Some(lp)),
                None => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        WM_SETFOCUS => {
            if let Some(c) = content_of(hwnd) {
                let _ = SetFocus(Some(c));
            }
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 >> 16) & 0xffff == 0 => {
            invoke_menu_cmd(hwnd, (wp.0 & 0xffff) as u16);
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            request_remote_resize(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            layout(hwnd);
            // Maximize / restore do not go through a size-move loop.
            let kind = wp.0 as u32;
            let was = with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| std::mem::replace(&mut r.maximized, kind == SIZE_MAXIMIZED))).flatten();
            if let Some(was_max) = was {
                if kind == SIZE_MAXIMIZED || (kind == SIZE_RESTORED && was_max) {
                    request_remote_resize(hwnd);
                }
            }
            invalidate_chrome(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xffff) as u32 != WA_INACTIVE {
                send_for(hwnd, |r, _| Some(Message::WindowFocus { window_id: r.id }));
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DPICHANGED => {
            let scale = ((wp.0 & 0xffff) as f64 / 96.0).max(0.5);
            with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| r.scale = scale));
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            layout(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing the local window asks the remote window to close; we disappear when it does.
            send_for(hwnd, |r, _| Some(Message::WindowClose { window_id: r.id }));
            LRESULT(0)
        }
        WM_DESTROY => {
            let menu = with_app(|a| {
                let r = a.remotes.remove(&(hwnd.0 as isize))?;
                a.by_id.remove(&r.id);
                Some(r.menu)
            })
            .flatten();
            if let Some(m) = menu.filter(|m| *m != 0) {
                let _ = DestroyMenu(HMENU(m as *mut c_void));
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The picture of a remote window: input goes to the Mac, frames are drawn here.
unsafe extern "system" fn content_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let frame = GetParent(hwnd).unwrap_or_default();
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let composed = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.comp.is_some())).flatten().unwrap_or(false);
            if !composed && !repaint_gpu(frame) {
                paint_into(frame, hdc, client_size(hwnd));
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_into(frame, HDC(wp.0 as *mut c_void), client_size(hwnd));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = lp_xy(lp);
            if is_fullscreen(frame) {
                // pointer at the top edge: the bar slides in; leaving it: it goes again
                let revealed = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.reveal)).flatten().unwrap_or(false);
                if !revealed && y <= 1 {
                    set_reveal(frame, true);
                } else if revealed && y > 4 {
                    set_reveal(frame, false);
                }
            }
            let cs = client_size(hwnd);
            send_for(frame, |r, _| {
                let (px, py) = scale_point(x, y, cs, (r.rw, r.rh));
                Some(Message::MouseMove { window_id: r.id, x: px, y: py })
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
            let (x, y) = lp_xy(lp);
            let cs = client_size(hwnd);
            let (button, down) = match msg {
                WM_LBUTTONDOWN => (MouseButton::Left, true),
                WM_LBUTTONUP => (MouseButton::Left, false),
                WM_RBUTTONDOWN => (MouseButton::Right, true),
                WM_RBUTTONUP => (MouseButton::Right, false),
                WM_MBUTTONDOWN => (MouseButton::Middle, true),
                _ => (MouseButton::Middle, false),
            };
            if down {
                let _ = SetFocus(Some(hwnd));
                SetCapture(hwnd);
            } else {
                let _ = ReleaseCapture();
            }
            send_for(frame, |r, _| {
                let (px, py) = scale_point(x, y, cs, (r.rw, r.rh));
                Some(Message::MouseButton { window_id: r.id, button, down, x: px, y: py })
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as f64 / 120.0 * 40.0;
            send_for(frame, |r, _| {
                Some(if msg == WM_MOUSEWHEEL { Message::Scroll { window_id: r.id, dx: 0.0, dy: delta } } else { Message::Scroll { window_id: r.id, dx: delta, dy: 0.0 } })
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wp.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            if (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) && vk == VK_F4.0 as u32 {
                if msg == WM_SYSKEYDOWN {
                    let _ = PostMessageW(Some(frame), WM_CLOSE, WPARAM(0), LPARAM(0)); // Alt+F4 closes the window as usual
                }
                return LRESULT(0);
            }
            if is_modifier_vk(vk) {
                return LRESULT(0);
            }
            if vk == VK_F11.0 as u32 {
                if msg == WM_KEYDOWN {
                    toggle_fullscreen(frame); // the Windows fullscreen key; the green light does the same
                }
                return LRESULT(0);
            }
            let mods = current_mods();
            if sends_as_text(vk, mods) {
                return LRESULT(0); // WM_CHAR delivers the character, layout-correct
            }
            if let Some(name) = vk_to_physical(vk) {
                send_for(frame, |r, cc| Some(Message::Key { window_id: r.id, physical_key: name.into(), modifiers: map_modifiers(mods, cc), down }));
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let unit = wp.0 as u16;
            if current_mods().ctrl || current_mods().alt {
                return LRESULT(0);
            }
            if is_text_char(unit) || (0xD800..=0xDFFF).contains(&unit) {
                let text = with_app(|a| {
                    let r = a.remotes.get_mut(&(frame.0 as isize))?;
                    push_utf16(&mut r.high_surrogate, unit).map(|t| (r.id, t))
                })
                .flatten();
                if let Some((id, text)) = text {
                    with_app(|a| a.link.send(&Message::TextInput { window_id: id, text }));
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------------ headless smoke test
//
// `--smoke` drives the real window procedures with synthetic messages and checks, end to end:
// frames are decoded and *painted* (read back with PrintWindow), typing reaches the remote app
// and comes back as a title change, mouse + resize requests are accepted, and closing the
// window destroys it.

struct Smoke {
    stage: u32,
    started: Instant,
    stage_started: Instant,
    destroyed: Vec<u64>,
    results: Vec<(String, bool, String)>,
    resized: bool,
}

impl Smoke {
    fn new() -> Self {
        Self { stage: 0, started: Instant::now(), stage_started: Instant::now(), destroyed: vec![], results: vec![], resized: false }
    }
}

fn smoke_note_destroyed(id: u64) {
    with_app(|a| {
        if let Some(s) = a.smoke.as_mut() {
            s.destroyed.push(id);
        }
    });
}

/// (hwnd, remote id) of the first window matching app/role/top-level.
fn find_window(app: &str, role: WindowRole) -> Option<(HWND, u64)> {
    with_app(|a| a.remotes.iter().find(|(_, r)| r.app == app && r.role == role).map(|(k, r)| (hwnd_of(*k), r.id))).flatten()
}

fn send_key(window_id: u64, key: &str, ctrl: bool) {
    with_app(|a| {
        let mods = map_modifiers(Mods { ctrl, ..Default::default() }, a.ctrl_as_command);
        for down in [true, false] {
            a.link.send(&Message::Key { window_id, physical_key: key.into(), modifiers: mods.clone(), down });
        }
    });
}

fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn painted_colors(frame: HWND) -> usize {
    // the picture area (under the bar)
    let bar = bar_px(frame);
    capture(frame, false).map(|s| {
        let rows = (s.h - bar).max(0);
        let start = (bar * s.w * 4) as usize;
        distinct(&Shot { w: s.w, h: rows, px: s.px[start..].to_vec() })
    }).unwrap_or(0)
}

fn light_center(l: chrome::Light, scale: f64) -> (i32, i32) {
    let (x, y) = chrome::light_origin(l, scale);
    let d = chrome::light_size(scale);
    (x + d / 2, y + d / 2)
}

/// A click on a traffic light, as the window procedure receives it.
fn click_light(frame: HWND, l: chrome::Light) {
    let scale = with_app(|a| a.remotes.get(&(frame.0 as isize)).map(|r| r.scale)).flatten().unwrap_or(1.0);
    let (x, y) = light_center(l, scale);
    let lp = ((y as isize) << 16) | x as isize;
    sendmsg(frame, WM_MOUSEMOVE, 0, lp);
    sendmsg(frame, WM_LBUTTONDOWN, 1, lp);
    sendmsg(frame, WM_LBUTTONUP, 0, lp);
}

pub(crate) struct Shot {
    pub w: i32,
    pub h: i32,
    /// BGRA, top-down
    pub px: Vec<u8>,
}

impl Shot {
    fn pixel(&self, x: i32, y: i32) -> (u8, u8, u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return (0, 0, 0);
        }
        let i = ((y * self.w + x) * 4) as usize;
        (self.px[i + 2], self.px[i + 1], self.px[i])
    }
}

/// The window as it is composed on screen (PrintWindow full content: chrome, picture, Direct3D).
/// `whole`: the outer window (borders included) instead of the client area.
/// What is on screen in the window's client (or whole) rect, brought to the top first. Used when
/// PrintWindow cannot see composition content.
fn capture_screen(hwnd: HWND, whole: bool) -> Option<Shot> {
    unsafe {
        let (x, y, w, h) = if whole {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            (r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            let mut o = POINT::default();
            let _ = ClientToScreen(hwnd, &mut o);
            let (w, h) = client_size(hwnd);
            (o.x, o.y, w, h)
        };
        if w <= 0 || h <= 0 {
            return None;
        }
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        std::thread::sleep(Duration::from_millis(250));
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        Some(Shot { w, h, px })
    }
}

/// Is there a drop shadow under the window (the pixel just below it darker than further away)?
fn shadow_below(hwnd: HWND) -> bool {
    let mut o = POINT::default();
    unsafe { let _ = ClientToScreen(hwnd, &mut o); }
    let (w, h) = client_size(hwnd);
    let Some(s) = screen_pixels(o.x + w / 2, o.y + h + 1, 1, 40) else { return false };
    let lum = |p: (u8, u8, u8)| p.0 as i32 + p.1 as i32 + p.2 as i32;
    lum(s.pixel(0, 0)) + 9 < lum(s.pixel(0, 39))
}

/// Screen pixels of a rectangle, as composed now.
fn screen_pixels(x: i32, y: i32, w: i32, h: i32) -> Option<Shot> {
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        Some(Shot { w, h, px })
    }
}

/// The window's picture: PrintWindow (full content), or the screen when that comes back blank
/// (composition windows).
fn capture(hwnd: HWND, whole: bool) -> Option<Shot> {
    let shot = capture_print(hwnd, whole);
    match shot {
        Some(s) if distinct(&s) > 8 => Some(s),
        _ => capture_screen(hwnd, whole),
    }
}

fn capture_print(hwnd: HWND, whole: bool) -> Option<Shot> {
    unsafe {
        let (w, h) = if whole {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            (r.right - r.left, r.bottom - r.top)
        } else {
            client_size(hwnd)
        };
        if w <= 0 || h <= 0 {
            return None;
        }
        let hdc = GetDC(Some(hwnd));
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp.into());
        let flags = if whole { 0x2 } else { 0x2 | 0x1 }; // PW_RENDERFULLCONTENT (| PW_CLIENTONLY)
        let _ = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(flags));
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), hdc);
        Some(Shot { w, h, px })
    }
}

fn capture_client(hwnd: HWND) -> Option<Shot> {
    capture(hwnd, false)
}

fn menu_text(m: HMENU, pos: u32) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetMenuStringW(m, pos, Some(&mut buf), MF_BYPOSITION) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// (top-level titles, label of File's first item, command id of the first menu's first item).
fn menu_snapshot(hwnd: HWND) -> Option<(Vec<String>, String, u32)> {
    unsafe {
        let bar = HMENU(with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.menu)).flatten().unwrap_or(0) as *mut c_void);
        if bar.is_invalid() {
            return None;
        }
        let n = GetMenuItemCount(Some(bar));
        if n < 2 {
            return None;
        }
        let tops: Vec<String> = (0..n as u32).map(|i| menu_text(bar, i)).collect();
        let file = GetSubMenu(bar, 1);
        let about = GetMenuItemID(GetSubMenu(bar, 0), 0);
        Some((tops, menu_text(file, 0), about))
    }
}

fn sendmsg(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp))) };
}

fn type_char(frame: HWND, c: char) {
    let hwnd = content_of(frame).unwrap_or(frame);
    let vk = c.to_ascii_uppercase() as usize;
    sendmsg(hwnd, WM_KEYDOWN, vk, 0);
    sendmsg(hwnd, WM_CHAR, c as usize, 0);
    sendmsg(hwnd, WM_KEYUP, vk, 0);
}

fn smoke_tick() {
    // Snapshot under a short borrow; all Win32 calls happen outside it (they re-enter our window procs).
    let Some((stage, since_stage, since_start, window, frames, destroyed, resized)) = with_app(|a| {
        let s = a.smoke.as_ref()?;
        let main = a.remotes.iter().find(|(_, r)| r.app == "testapp" && r.role == WindowRole::Window);
        let (key, frames) = main.map(|(k, r)| (Some(*k), r.frames)).unwrap_or((None, 0));
        let dims = key.and_then(|k| a.remotes.get(&k)).map(|r| (r.rw, r.rh));
        Some((s.stage, s.stage_started.elapsed(), s.started.elapsed(), key.map(|k| (hwnd_of(k), dims.unwrap_or((0, 0)))), frames, s.destroyed.clone(), s.resized))
    })
    .flatten() else { return };

    let finish = |name: &str, ok: bool, detail: String, next: u32| {
        eprintln!("[{}] {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        with_app(|a| {
            if let Some(s) = a.smoke.as_mut() {
                s.results.push((name.into(), ok, detail));
                s.stage = if ok { next } else { 99 };
                s.stage_started = Instant::now();
            }
        });
    };
    if since_start > Duration::from_secs(90) || since_stage > Duration::from_secs(20) {
        finish(&format!("stage {stage} timed out"), false, format!("frames={frames}"), 99);
    }

    match (stage, window) {
        (0, Some((hwnd, _))) if frames >= 15 => {
            std::thread::sleep(Duration::from_millis(200));
            let colors = painted_colors(hwnd);
            let renderer = with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.comp.as_ref().map(|c| c.kind).or(r.presenter.as_ref().map(|p| p.kind)).unwrap_or("gdi"))).flatten().unwrap_or("?");
            finish("decoded frames are painted into the native window", colors > 100, format!("renderer={renderer} frames={frames} distinctColors={colors}"), 1);
        }
        (1, Some((hwnd, _))) => {
            "hello".chars().for_each(|c| type_char(hwnd, c));
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 2));
        }
        (2, Some((hwnd, _))) => {
            let t = title_of(hwnd);
            if t.contains("[5 chars]") {
                finish("typing reaches the remote app (WM_CHAR -> TextInput -> title)", true, t, 3);
            }
        }
        (3, Some((hwnd, _))) => {
            let pt: isize = (70 << 16) | 20;
            let hwnd = content_of(hwnd).unwrap_or(hwnd);
            sendmsg(hwnd, WM_MOUSEMOVE, 0, pt);
            sendmsg(hwnd, WM_LBUTTONDOWN, 1, pt);
            sendmsg(hwnd, WM_LBUTTONUP, 0, pt);
            type_char(hwnd, 'X');
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 4));
        }
        (4, Some((hwnd, _))) => {
            let t = title_of(hwnd);
            if t.contains("[6 chars]") {
                finish("keyboard works after a mouse click", true, t, 5);
            }
        }
        (5, Some((hwnd, _))) => {
            let hwnd = content_of(hwnd).unwrap_or(hwnd);
            sendmsg(hwnd, WM_KEYDOWN, VK_BACK.0 as usize, 0);
            sendmsg(hwnd, WM_KEYUP, VK_BACK.0 as usize, 0);
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 6));
        }
        (6, Some((hwnd, _))) => {
            let t = title_of(hwnd);
            if t.contains("[5 chars]") {
                finish("non-text key (Backspace) is sent as a physical key", true, t, 7);
            }
        }
        (7, Some((hwnd, _))) => unsafe {
            // user drags the border to 640x400: client resized, then WM_EXITSIZEMOVE
            let sc = native::dpi_scale(hwnd);
            let (ow, oh) = outer_for_client(hwnd, (640.0 * sc).round() as i32, (400.0 * sc).round() as i32 + bar_px(hwnd));
            let _ = SetWindowPos(hwnd, None, 0, 0, ow, oh, SWP_NOMOVE | SWP_NOZORDER);
            sendmsg(hwnd, WM_EXITSIZEMOVE, 0, 0);
            with_app(|a| a.smoke.as_mut().map(|s| { s.stage = 8; s.resized = true }));
        },
        (8, Some((_, (rw, rh)))) if resized => {
            if (rw, rh) == (640, 400) {
                finish("resize request accepted by the remote side (in Mac points)", true, format!("remote now {rw}x{rh}pt"), 20);
            }
        }
        (20, Some((hwnd, _))) => {
            let aumid = native::get_app_user_model_id(hwnd);
            let icon = native::window_has_icon(hwnd);
            if icon {
                let ok = aumid.as_deref() == Some("RemoteMac.testapp");
                finish("own taskbar identity: AppUserModelID + app icon", ok, format!("aumid={aumid:?} icon={icon}"), 21);
            }
        }
        (21, Some((_, _))) => {
            // user copies on Windows; the remote app pastes it
            let owner = with_app(|a| a.controller).unwrap_or(0);
            native::set_clipboard_text(hwnd_of(owner), "from-windows");
            std::thread::sleep(Duration::from_millis(300));
            // (WM_CLIPBOARDUPDATE is posted to the controller; it forwards ClipboardSet)
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 22));
        }
        (22, Some((hwnd, _))) if since_stage > Duration::from_millis(700) => {
            send_for(hwnd, |r, cc| Some(Message::Key { window_id: r.id, physical_key: "KeyV".into(), modifiers: map_modifiers(Mods { ctrl: true, ..Default::default() }, cc), down: true }));
            send_for(hwnd, |r, cc| Some(Message::Key { window_id: r.id, physical_key: "KeyV".into(), modifiers: map_modifiers(Mods { ctrl: true, ..Default::default() }, cc), down: false }));
            with_app(|a| a.smoke.as_mut().map(|s| { s.stage = 23; s.stage_started = Instant::now() }));
        }
        (23, Some((hwnd, _))) => {
            let t = title_of(hwnd);
            if t.contains("[17 chars]") {
                finish("clipboard Windows->Mac (Ctrl+V pastes 'from-windows')", true, t, 24);
            }
        }
        (24, Some((hwnd, _))) => {
            for k in ["KeyA", "KeyC"] {
                for down in [true, false] {
                    send_for(hwnd, |r, cc| Some(Message::Key { window_id: r.id, physical_key: k.into(), modifiers: map_modifiers(Mods { ctrl: true, ..Default::default() }, cc), down }));
                }
            }
            with_app(|a| a.smoke.as_mut().map(|s| { s.stage = 25; s.stage_started = Instant::now() }));
        }
        (25, Some((_, _))) => {
            let owner = with_app(|a| a.controller).unwrap_or(0);
            let clip = native::clipboard_text(hwnd_of(owner));
            if clip.as_deref() == Some("hellofrom-windows") {
                finish("clipboard Mac->Windows (Ctrl+A, Ctrl+C lands on the Windows clipboard)", true, format!("{clip:?}"), 30);
            }
        }
        (30, Some(_)) => {
            let ids = with_app(|a| a.launcher.as_ref().map(|l| (l.count(), l.ids.clone()))).flatten();
            if let Some((3, ids)) = ids {
                finish("launcher lists the Mac's applications", true, format!("{ids:?}"), 31);
                launch_app("notes"); // same path as a double-click on the "Notes Test" icon
            }
        }
        (31, Some((main, _))) => {
            if let Some((notes, nid)) = find_window("notes", WindowRole::Window) {
                let aumid = native::get_app_user_model_id(notes);
                let both = unsafe { IsWindow(Some(main)).as_bool() };
                finish("second app opens alongside the first, in its own taskbar group", both && aumid.as_deref() == Some("RemoteMac.notes"), format!("aumid={aumid:?} firstStillOpen={both}"), 32);
                with_app(|a| a.link.send(&Message::TextInput { window_id: nid, text: "zz".into() }));
            }
        }
        (32, Some((main, _))) => {
            if let Some((notes, _)) = find_window("notes", WindowRole::Window) {
                let (tn, tm) = (title_of(notes), title_of(main));
                if tn.contains("[2 chars]") {
                    finish("input goes to the right app when several run", tm.contains("[17 chars]"), format!("notes={tn:?} testapp={tm:?}"), 40);
                }
            }
        }
        (40, Some((main, _))) => {
            // the Mac menu bar is in the window: "RM Test App  File  Edit", shortcuts in Windows terms
            let Some((tops, open_label, about)) = menu_snapshot(main) else { return };
            let ok = tops == ["RM Test App", "File", "Edit"] && open_label == "Open…\tCtrl+O";
            finish("Mac menu bar shown as the window's native menu", ok, format!("{tops:?} File[0]={open_label:?}"), 41);
            if ok {
                sendmsg(main, WM_COMMAND, about as usize, 0); // user clicks RM Test App > About
            }
        }
        (41, Some((main, _))) => {
            if let Some((dlg, _)) = find_window("testapp", WindowRole::Dialog) {
                finish("clicking a menu item runs it in the Mac app (About opens its dialog)", true, format!("dialog for {:?}", main.0), 42);
                sendmsg(dlg, WM_CLOSE, 0, 0);
            }
        }
        (42, Some(_)) => {
            if find_window("testapp", WindowRole::Dialog).is_none() {
                with_app(|a| a.smoke.as_mut().map(|s| s.stage = 43));
            }
        }
        (43, Some((main, _))) if since_stage > Duration::from_millis(300) => unsafe {
            // Mac chrome instead of the Windows caption: no WS_CAPTION, our title bar drags the
            // window, the traffic lights are drawn where the Mac has them, the menu strip is under it.
            let style = GetWindowLongW(main, GWL_STYLE) as u32;
            let scale = with_app(|a| a.remotes.get(&(main.0 as isize)).map(|r| r.scale)).flatten().unwrap_or(1.0);
            let mut org = POINT::default();
            let _ = ClientToScreen(main, &mut org);
            let at = |x: i32, y: i32| ((org.y + y) as isize) << 16 | ((org.x + x) as isize & 0xffff);
            let (cw, _) = client_size(main);
            let caption = SendMessageW(main, WM_NCHITTEST, None, Some(LPARAM(at(cw / 2, chrome::title_height(scale) / 2)))).0 as u32;
            let on_light = SendMessageW(main, WM_NCHITTEST, None, Some(LPARAM(at(light_center(chrome::Light::Close, scale).0, light_center(chrome::Light::Close, scale).1)))).0 as u32;
            let shot = capture_client(main);
            let colors: Vec<(u8, u8, u8)> = chrome::LIGHTS.iter().map(|l| { let (x, y) = light_center(*l, scale); shot.as_ref().map(|s| s.pixel(x, y)).unwrap_or((0, 0, 0)) }).collect();
            let active = with_app(|a| a.remotes.get(&(main.0 as isize)).map(|r| r.active)).flatten().unwrap_or(false);
            let want: Vec<(u8, u8, u8)> = chrome::LIGHTS.iter().map(|l| chrome::light_color(*l, active)).collect();
            let near = |a: (u8, u8, u8), b: (u8, u8, u8)| a.0.abs_diff(b.0) < 24 && a.1.abs_diff(b.1) < 24 && a.2.abs_diff(b.2) < 24;
            let lights_ok = colors.iter().zip(&want).all(|(a, b)| near(*a, *b));
            let content_top = content_of(main).map(|c| { let mut r = RECT::default(); let _ = GetWindowRect(c, &mut r); r.top - org.y }).unwrap_or(-1);
            let ok = style & WS_CAPTION.0 != WS_CAPTION.0 && caption == HTCAPTION && on_light == HTCLIENT && lights_ok && content_top == bar_px(main);
            finish("Mac window chrome: traffic lights, own title bar, menu strip above the picture", ok,
                format!("caption={caption} light={on_light} lights={colors:?} want={want:?} active={active} pictureTop={content_top} bar={}", bar_px(main)), 47);
        },
        (47, Some((main, _))) => {
            // rounded, anti-aliased corners: the very corner shows what is behind the window,
            // the bar a few pixels along is the bar
            let composed = with_app(|a| a.remotes.get(&(main.0 as isize)).map(|r| r.comp.is_some())).flatten().unwrap_or(false);
            if composed {
                // what is behind the window at its corner, then the window itself there: a rounded
                // (transparent) corner shows exactly what is behind; the bar shows the bar
                let shot = capture_screen(main, false);
                let mut org = POINT::default();
                unsafe { let _ = ClientToScreen(main, &mut org); }
                let (w, _) = client_size(main);
                unsafe { let _ = ShowWindow(main, SW_HIDE); }
                std::thread::sleep(Duration::from_millis(300));
                let behind = screen_pixels(org.x, org.y, w, 3);
                unsafe { let _ = ShowWindow(main, SW_SHOW); }
                let active = with_app(|a| a.remotes.get(&(main.0 as isize)).map(|r| r.active)).flatten().unwrap_or(false);
                let bg = chrome::title_bg(active);
                let far = |a: (u8, u8, u8), b: (u8, u8, u8)| a.0.abs_diff(b.0) as u32 + a.1.abs_diff(b.1) as u32 + a.2.abs_diff(b.2) as u32;
                let (corner, edge) = shot.as_ref().map(|s| (s.pixel(0, 0), s.pixel(s.w / 2, 2))).unwrap_or_default();
                let behind_corner = behind.as_ref().map(|s| s.pixel(0, 0)).unwrap_or_default();
                let clean = |c: (u8, u8, u8)| far(c, behind_corner) < 12;
                let mut ok = clean(corner) && far(edge, bg) < 12;
                let mut notes = vec![format!("plain: corner={corner:?} behind={behind_corner:?} shadow={}", shadow_below(main))];
                // DWM can draw a 1px frame line along the top, visible only in the transparent
                // corners: try the remedies one after another and report which one works
                if !ok {
                    for remedy in ["extend-frame", "nc-rendering-off", "window-region"] {
                        native::corner_remedy(main, remedy, (chrome::CORNER_RADIUS * native::dpi_scale(main)) as i32);
                        std::thread::sleep(Duration::from_millis(400));
                        let c = capture_screen(main, false).map(|s| s.pixel(0, 0)).unwrap_or_default();
                        let fixed = clean(c);
                        notes.push(format!("{remedy}: corner={c:?} fixed={fixed} shadow={}", shadow_below(main)));
                        if fixed {
                            ok = true;
                            break;
                        }
                    }
                }
                finish("rounded window corners (DirectComposition clip)", ok, format!("barEdge={edge:?} bar={bg:?} | {}", notes.join(" | ")), 44);
            } else {
                finish("square corners with the GDI renderer", true, "gdi".into(), 44);
            }
            click_light(main, chrome::Light::Minimize);
        }
        (44, Some((main, _))) => unsafe {
            if IsIconic(main).as_bool() {
                let _ = ShowWindow(main, SW_RESTORE);
                let _ = SetForegroundWindow(main);
                click_light(main, chrome::Light::Zoom); // green: fullscreen, as on the Mac
                with_app(|a| a.smoke.as_mut().map(|s| { s.stage = 45; s.stage_started = Instant::now() }));
            }
        },
        (45, Some((main, (rw, rh)))) => unsafe {
            let mon = monitor_rect(main);
            let scale = native::dpi_scale(main);
            let (dw, dh, ds) = chrome::display_request(mon.right - mon.left, mon.bottom - mon.top, scale);
            let want = (dw / ds, dh / ds);
            if (rw, rh) == want {
                let mut wr = RECT::default();
                let _ = GetWindowRect(main, &mut wr);
                let covers = (wr.left, wr.top, wr.right, wr.bottom) == (mon.left, mon.top, mon.right, mon.bottom);
                let hidden = bar_px(main) == 0;
                let picture = content_of(main).map(client_size) == Some((mon.right - mon.left, mon.bottom - mon.top));
                // the bar slides in at the top edge and goes again
                let c = content_of(main).unwrap_or(main);
                sendmsg(c, WM_MOUSEMOVE, 0, 0);
                let revealed = bar_px(main) > 0;
                let c = content_of(main).unwrap_or(main);
                sendmsg(c, WM_MOUSEMOVE, 0, (200 << 16) | 200);
                let gone = bar_px(main) == 0;
                let display = with_app(|a| a.display).flatten();
                finish("green light: fullscreen on the monitor, Mac app sized to it on a virtual display", covers && hidden && picture && revealed && gone,
                    format!("window={:?} monitor={:?} remote={rw}x{rh}pt display={display:?} barHidden={hidden} reveal={revealed}/{gone}", (wr.left, wr.top, wr.right, wr.bottom), (mon.left, mon.top, mon.right, mon.bottom)), 46);
                let c = content_of(main).unwrap_or(main);
                sendmsg(c, WM_KEYDOWN, VK_F11.0 as usize, 0); // F11 leaves fullscreen
                sendmsg(c, WM_KEYUP, VK_F11.0 as usize, 0);
            }
        },
        (46, Some((main, (rw, _)))) => unsafe {
            if !is_fullscreen(main) && rw < 1000 {
                let mut wr = RECT::default();
                let _ = GetWindowRect(main, &mut wr);
                let saved = with_app(|a| a.remotes.get(&(main.0 as isize)).map(|r| r.saved)).flatten().unwrap_or_default();
                let back = (wr.left, wr.top, wr.right, wr.bottom) == (saved.left, saved.top, saved.right, saved.bottom) && bar_px(main) > 0;
                // resizing like any window: the edges and corners give the sizing cursors
                let at = |x: i32, y: i32| SendMessageW(main, WM_NCHITTEST, None, Some(LPARAM(((y as isize) << 16) | (x as isize & 0xffff)))).0 as u32;
                let midy = (wr.top + wr.bottom) / 2;
                let (left, right, bottom, corner) = (at(wr.left + 2, midy), at(wr.right - 2, midy), at((wr.left + wr.right) / 2, wr.bottom - 2), at(wr.right - 2, wr.bottom - 2));
                SendMessageW(main, WM_SETCURSOR, Some(WPARAM(main.0 as usize)), Some(LPARAM(((WM_MOUSEMOVE as isize) << 16) | HTLEFT as isize)));
                let cursor = GetCursor();
                let we = LoadCursorW(None, IDC_SIZEWE).unwrap_or_default();
                let edges = left == HTLEFT && right == HTRIGHT && bottom == HTBOTTOM && corner == HTBOTTOMRIGHT;
                finish("leaving fullscreen restores the window; edges resize with sizing cursors", back && edges && cursor == we,
                    format!("restored={back} hit=({left},{right},{bottom},{corner}) cursorIsSizeWE={}", cursor == we), 33);
            }
        },
        (33, Some((_, _))) => {
            if let Some((_, mid)) = find_window("testapp", WindowRole::Window) {
                send_key(mid, "KeyI", true); // opens the app's "About" dialog
                with_app(|a| a.smoke.as_mut().map(|s| s.stage = 34));
            }
        }
        (34, Some((main, _))) => {
            if let Some((dlg, _)) = find_window("testapp", WindowRole::Dialog) {
                let owner = unsafe { GetWindow(dlg, GW_OWNER) }.ok();
                finish("app dialogs become owned windows of their parent", owner == Some(main), format!("owner={:?} parent={:?}", owner.map(|o| o.0), main.0), 35);
                sendmsg(dlg, WM_CLOSE, 0, 0);
            }
        }
        (35, Some((_, _))) => {
            if find_window("testapp", WindowRole::Dialog).is_none() {
                if let Some((_, mid)) = find_window("testapp", WindowRole::Window) {
                    send_key(mid, "KeyO", true); // the app shows its file Open panel
                    with_app(|a| a.smoke.as_mut().map(|s| s.stage = 36));
                }
            }
        }
        (36, Some((main, _))) => {
            let t = title_of(main);
            if t.contains("[opened ") {
                let mac_panel_shown = find_window("testapp", WindowRole::OpenPanel).is_some();
                let ok = t.contains("716800 bytes") && !mac_panel_shown;
                finish("Mac file panel replaced by the Windows picker; chosen file uploaded and opened", ok, format!("{t:?} macPanelShown={mac_panel_shown}"), 9);
            }
        }
        (9, Some((hwnd, _))) => {
            click_light(hwnd, chrome::Light::Close); // the red light closes the window
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 10));
        }
        (10, _) if !destroyed.is_empty() && find_window("testapp", WindowRole::Window).is_none() => {
            let notes_alive = find_window("notes", WindowRole::Window).is_some();
            finish("closing one app's window leaves the other app running", notes_alive, format!("destroyed={destroyed:?}"), 50);
        }
        (50, _) => {
            launch_app(DESKTOP_APP); // the "Mac Desktop" icon in the launcher
            with_app(|a| a.smoke.as_mut().map(|s| { s.stage = 51; s.stage_started = Instant::now() }));
        }
        (51, _) => unsafe {
            if let Some((d, did)) = find_window(DESKTOP_APP, WindowRole::Window) {
                let frames = with_app(|a| a.remotes.get(&(d.0 as isize)).map(|r| r.frames)).flatten().unwrap_or(0);
                if frames >= 5 {
                    let mon = monitor_rect(d);
                    let mut wr = RECT::default();
                    let _ = GetWindowRect(d, &mut wr);
                    let covers = (wr.left, wr.top, wr.right, wr.bottom) == (mon.left, mon.top, mon.right, mon.bottom);
                    let colors = painted_colors(d);
                    finish("Mac Desktop opens fullscreen and shows the whole Mac screen", covers && bar_px(d) == 0 && colors > 100,
                        format!("window={:?} monitor={:?} frames={frames} colors={colors}", (wr.left, wr.top, wr.right, wr.bottom), (mon.left, mon.top, mon.right, mon.bottom)), 52);
                    // a click and a key on the desktop reach the window under the pointer (the notes app)
                    let notes = with_app(|a| a.remotes.values().find(|r| r.app == "notes").map(|r| (r.rx, r.ry))).flatten();
                    if let Some((nx, ny)) = notes {
                        with_app(|a| {
                            for down in [true, false] {
                                a.link.send(&Message::MouseButton { window_id: did, button: MouseButton::Left, down, x: nx as f64 + 30.0, y: ny as f64 + 30.0 });
                            }
                            a.link.send(&Message::TextInput { window_id: did, text: "q".into() });
                        });
                    }
                }
            }
        },
        (52, _) => {
            if let Some((notes, _)) = find_window("notes", WindowRole::Window) {
                let t = title_of(notes);
                if t.contains("[3 chars]") {
                    finish("input on the Mac Desktop reaches the app under the pointer", true, t, 53);
                    with_app(|a| a.link.send(&Message::AppTerminate { application_id: DESKTOP_APP.into() }));
                }
            }
        }
        (53, _) => {
            if find_window(DESKTOP_APP, WindowRole::Window).is_none() {
                finish("closing the Mac Desktop", true, "window gone".into(), 37);
            }
        }
        (37, _) => {
            let Some(dir) = with_app(|a| a.shortcut_dir.clone()).flatten() else {
                finish("Start-menu shortcuts", false, "no shortcut folder".into(), 99);
                return;
            };
            let found: Vec<(String, Option<String>)> = shortcuts::list(&dir).iter().filter_map(|p| shortcuts::read(p)).collect();
            let want = [("--app desktop".to_string(), Some("RemoteMac.desktop".to_string())), ("--app testapp".to_string(), Some("RemoteMac.testapp".to_string())), ("--app notes".to_string(), Some("RemoteMac.notes".to_string()))];
            let icons = dir.join("icons").read_dir().map(|d| d.count()).unwrap_or(0);
            if want.iter().all(|w| found.contains(w)) && icons >= 3 {
                finish("each Mac app is in the Start menu / Windows Search (shortcut + icon + AUMID)", found.len() == 3, format!("{found:?} icons={icons}"), 38);
            }
        }
        (38, _) => {
            on_mac_gone(); // what a disconnect does
            let dir = with_app(|a| a.shortcut_dir.clone()).flatten();
            let left = dir.as_deref().map(shortcuts::list).unwrap_or_default();
            finish("disconnecting the Mac removes its shortcuts", left.is_empty(), format!("left={left:?}"), 100);
        }
        (99, _) | (100, _) => {
            let (ok, n) = with_app(|a| {
                let r = &a.smoke.as_ref().unwrap().results;
                (stage == 100 && r.iter().all(|c| c.1), r.len())
            })
            .unwrap_or((false, 0));
            eprintln!("SMOKE {}: {n} checks", if ok { "PASS" } else { "FAIL" });
            quit(if ok { 0 } else { 1 });
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ showcase
//
// `--showcase DIR` opens the given Mac apps one after another (as a double-click in the launcher
// would), waits for their windows to be drawn and settle, and saves what the user sees: every
// window of each app (Mac chrome included) and the launcher, as BMP files, plus `showcase.txt`.
// `DIR/ready` tells a script that all apps are on screen (for a desktop screenshot); the viewer
// finishes when `DIR/desktop.done` appears (or after a minute).

struct Showcase {
    cfg: ShowcaseOptions,
    next: usize,
    current: Option<(String, Instant, Option<Instant>)>,
    report: Vec<String>,
    failed: bool,
    launcher_saved: bool,
    started: Instant,
    ready_at: Option<Instant>,
    quit_at: Option<Instant>,
}

impl Showcase {
    fn new(cfg: ShowcaseOptions) -> Self {
        let _ = std::fs::create_dir_all(&cfg.dir);
        Self { cfg, next: 0, current: None, report: vec![], failed: false, launcher_saved: false, started: Instant::now(), ready_at: None, quit_at: None }
    }
}

/// 24-bit bottom-up BMP (opens everywhere, converts to PNG with any tool).
fn write_bmp(path: &std::path::Path, s: &Shot) -> std::io::Result<()> {
    let row = (s.w as usize * 3).div_ceil(4) * 4;
    let size = 54 + row * s.h as usize;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&s.w.to_le_bytes());
    b.extend_from_slice(&s.h.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    for y in (0..s.h).rev() {
        let start = b.len();
        for x in 0..s.w {
            let i = ((y * s.w + x) * 4) as usize;
            b.extend_from_slice(&s.px[i..i + 3]);
        }
        b.resize(start + row, 0);
    }
    std::fs::write(path, b)
}

fn distinct(s: &Shot) -> usize {
    Picture { width: s.w as usize, height: s.h as usize, bgra: s.px.clone() }.distinct_colors()
}

fn showcase_note(line: String) {
    eprintln!("[showcase] {line}");
    with_app(|a| a.showcase.as_mut().map(|s| s.report.push(line)));
}

fn showcase_tick() {
    let Some((dir, apps_known, next, current, launcher_saved, ready_at, quit_at, settle, timeout, started)) = with_app(|a| {
        let s = a.showcase.as_ref()?;
        Some((s.cfg.dir.clone(), !a.app_names.is_empty(), s.next, s.current.clone(), s.launcher_saved, s.ready_at, s.quit_at, s.cfg.settle, s.cfg.app_timeout, s.started))
    })
    .flatten() else { return };
    if let Some(t) = quit_at {
        if Instant::now() >= t {
            let (report, failed) = with_app(|a| a.showcase.as_ref().map(|s| (s.report.join("\n"), s.failed))).flatten().unwrap_or_default();
            let _ = std::fs::write(dir.join("showcase.txt"), format!("{report}\n"));
            eprintln!("SHOWCASE {}", if failed { "INCOMPLETE" } else { "DONE" });
            quit(if failed { 1 } else { 0 });
        }
        return;
    }
    if !apps_known {
        if started.elapsed() > Duration::from_secs(60) {
            showcase_note("no app list from the Mac".into());
            with_app(|a| a.showcase.as_mut().map(|s| { s.failed = true; s.quit_at = Some(Instant::now()) }));
        }
        return;
    }
    if !launcher_saved && started.elapsed() > Duration::from_secs(4) {
        // the launcher, with the Mac's app icons, as the user first sees it
        let l = with_app(|a| a.launcher.as_ref().map(|l| (l.hwnd, l.count()))).flatten();
        if let Some((h, n)) = l {
            unsafe { let _ = ShowWindow(h, SW_SHOW); }
            std::thread::sleep(Duration::from_millis(300));
            if let Some(shot) = capture(h, true) {
                let _ = write_bmp(&dir.join("00-launcher.bmp"), &shot);
                showcase_note(format!("launcher: {n} apps listed, {}x{} px", shot.w, shot.h));
            }
        }
        with_app(|a| a.showcase.as_mut().map(|s| s.launcher_saved = true));
        return;
    }
    if let Some(t) = ready_at {
        if dir.join("desktop.done").exists() || t.elapsed() > Duration::from_secs(60) {
            let apps: Vec<String> = with_app(|a| a.showcase.as_ref().map(|s| s.cfg.apps.clone())).flatten().unwrap_or_default();
            for app in apps {
                with_app(|a| a.link.send(&Message::AppTerminate { application_id: app }));
            }
            with_app(|a| a.showcase.as_mut().map(|s| s.quit_at = Some(Instant::now() + Duration::from_secs(3))));
        }
        return;
    }
    match current {
        None => {
            let app = with_app(|a| a.showcase.as_ref().and_then(|s| s.cfg.apps.get(next).cloned())).flatten();
            match app {
                Some(app) => {
                    let known = with_app(|a| a.app_names.iter().any(|(id, _)| *id == app)).unwrap_or(false);
                    if !known {
                        showcase_note(format!("{app}: not available on this Mac"));
                        with_app(|a| a.showcase.as_mut().map(|s| { s.failed = true; s.next += 1 }));
                        return;
                    }
                    showcase_note(format!("{app}: launching"));
                    launch_app(&app); // what a double-click in the launcher does
                    with_app(|a| a.showcase.as_mut().map(|s| s.current = Some((app, Instant::now(), None))));
                }
                None => {
                    // everything is open: let a script take the desktop screenshot
                    let _ = std::fs::write(dir.join("ready"), b"ready\n");
                    showcase_note("all apps open; desktop screenshot can be taken".into());
                    with_app(|a| a.showcase.as_mut().map(|s| s.ready_at = Some(Instant::now())));
                }
            }
        }
        Some((app, launched, seen)) => {
            let drawn = with_app(|a| a.remotes.values().any(|r| r.app == app && r.frames >= 1)).unwrap_or(false);
            if drawn && seen.is_none() {
                showcase_note(format!("{app}: first window drawn after {:.1}s", launched.elapsed().as_secs_f64()));
                let typing = with_app(|a| a.showcase.as_ref().and_then(|s| s.cfg.type_text.clone())).flatten();
                let target = with_app(|a| a.remotes.iter().find(|(_, r)| r.app == app && !r.owned).map(|(k, _)| *k)).flatten();
                if let (Some(text), Some(h)) = (typing, target) {
                    unsafe { let _ = SetForegroundWindow(hwnd_of(h)); }
                    // a click into the picture first, so the Mac app has a focused text view
                    if let Some(c) = content_of(hwnd_of(h)) {
                        let (cw, ch) = client_size(c);
                        let pt = ((ch / 2) as isize) << 16 | (cw / 2) as isize;
                        sendmsg(c, WM_LBUTTONDOWN, 1, pt);
                        sendmsg(c, WM_LBUTTONUP, 0, pt);
                    }
                    text.chars().for_each(|ch| type_char(hwnd_of(h), ch));
                    showcase_note(format!("{app}: typed {text:?} on Windows"));
                }
                with_app(|a| a.showcase.as_mut().map(|s| s.current = Some((app.clone(), launched, Some(Instant::now())))));
                return;
            }
            if let Some(t) = seen {
                if t.elapsed() >= settle {
                    save_app_windows(&app, next, &dir);
                    with_app(|a| a.showcase.as_mut().map(|s| { s.current = None; s.next += 1 }));
                }
            } else if launched.elapsed() > timeout {
                showcase_note(format!("{app}: no window within {}s", timeout.as_secs()));
                with_app(|a| a.showcase.as_mut().map(|s| { s.failed = true; s.current = None; s.next += 1 }));
            }
        }
    }
}

fn save_app_windows(app: &str, index: usize, dir: &std::path::Path) {
    let wins: Vec<(isize, String, String, u32, u32, u32, &'static str)> = with_app(|a| {
        let mut v: Vec<_> = a.remotes.iter().filter(|(_, r)| r.app == app).map(|(k, r)| (*k, format!("{:?}", r.role), r.id.to_string(), r.rw, r.rh, r.frames, r.comp.as_ref().map(|c| c.kind).or(r.presenter.as_ref().map(|p| p.kind)).unwrap_or("gdi"))).collect();
        v.sort_by_key(|w| w.2.clone());
        v
    })
    .unwrap_or_default();
    for (k, (h, role, _, rw, rh, frames, renderer)) in wins.into_iter().enumerate() {
        let hwnd = hwnd_of(h);
        unsafe {
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
        }
        std::thread::sleep(Duration::from_millis(400));
        drain_events(); // keep the picture current
        let title = title_of(hwnd);
        match capture(hwnd, true) {
            Some(shot) => {
                let name = format!("{:02}-{}-{}.bmp", index + 1, app, k + 1);
                let _ = write_bmp(&dir.join(&name), &shot);
                let menus = menu_snapshot(hwnd).map(|m| m.0.join(" | ")).unwrap_or_default();
                showcase_note(format!("{app}: {name} role={role} title={title:?} remote={rw}x{rh}pt window={}x{}px frames={frames} renderer={renderer} colors={} menus=[{menus}]", shot.w, shot.h, distinct(&shot)));
            }
            None => showcase_note(format!("{app}: window {title:?} could not be captured")),
        }
    }
}
