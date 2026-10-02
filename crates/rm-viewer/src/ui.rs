//! Win32 presentation: one native top-level window per remote window, pictures drawn with GDI
//! (`StretchDIBits`). GDI is the portable baseline that also works on CI machines without a GPU;
//! a Direct3D 11 swap-chain presenter is a drop-in replacement for `paint`.
//!
//! Threading: everything here runs on the UI thread. The network thread only posts `WM_UI_EVENT`.

use crate::keymap::*;
use crate::native;
use crate::d3d;
use crate::launcher::{self, Launcher};
use rm_protocol::WindowRole;
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
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Options {
    pub relay: String,
    pub session: String,
    pub token: String,
    pub app: Option<String>,
    pub ctrl_as_command: bool,
    pub smoke: bool,
    pub clipboard: bool,
    /// Prefer Direct3D 11 (falls back to GDI when no device can be created).
    pub d3d: bool,
    /// Replace the Mac's file Open panel with the Windows one (uploading the chosen file).
    pub windows_file_picker: bool,
}

const WM_UI_EVENT: u32 = WM_APP + 1;
/// wParam = remote id of an open panel to replace with the Windows file picker.
const WM_PICK_FILE: u32 = WM_APP + 2;
const TIMER_ID: usize = 1;

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
    picture: Option<Picture>,
    frames: u32,
    high_surrogate: Option<u16>,
}

struct App {
    link: Link,
    rx: Receiver<UiEvent>,
    remotes: HashMap<isize, Remote>,
    by_id: HashMap<u64, isize>,
    ctrl_as_command: bool,
    smoke: Option<Smoke>,
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
    launcher: Option<Launcher>,
    /// Remote open panels we replaced with the Windows picker: panel id -> parent window id.
    panels: HashMap<u64, Option<u64>>,
    /// Upload transfer id -> panel id waiting for the uploaded file.
    uploads: HashMap<u64, u64>,
    next_transfer: u64,
    redirect_panels: bool,
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
        if !opts.smoke && launcher::forward_to_running_instance(opts.app.as_deref()) {
            return 0;
        }
        let hinst: HINSTANCE = GetModuleHandleW(None).expect("module handle").into();
        let cursor = LoadCursorW(None, IDC_ARROW).ok().unwrap_or_default();
        for (name, proc) in [(w!("RmController"), Some(controller_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)), (w!("RmRemoteWindow"), Some(remote_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT)),
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
        let (link, rx) = match net::connect(&opts.relay, &opts.session, &opts.token, opts.app.as_deref(), wake) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("connect failed: {e}");
                return 1;
            }
        };
        eprintln!("connected; waiting for windows");
        link.send(&Message::ListApps);
        let launcher = Launcher::create(hinst, opts.app.is_none() && !opts.smoke);
        if launcher.is_none() {
            eprintln!("warning: launcher window could not be created");
        }
        let smoke = opts.smoke.then(Smoke::new);
        APP.with(|a| {
            *a.borrow_mut() = Some(App { link, rx, remotes: HashMap::new(), by_id: HashMap::new(), ctrl_as_command: opts.ctrl_as_command, smoke, exit: None, hinst: hinst.0 as isize, controller: ctl,
                icons: HashMap::new(), icons_requested: Default::default(), clipboard: opts.clipboard, clip_applied: None, clip_seq: 0, d3d: opts.d3d,
                launcher, panels: HashMap::new(), uploads: HashMap::new(), next_transfer: 1, redirect_panels: opts.windows_file_picker })
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

fn quit(code: i32) {
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
            smoke_tick();
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

fn drain_events() {
    let events: Vec<UiEvent> = with_app(|a| a.rx.try_iter().collect()).unwrap_or_default();
    for ev in events {
        handle_event(ev);
    }
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
                Some((key, r.scale, r.maximized))
            })
            .flatten();
            if let Some((key, scale, maximized)) = target {
                if !maximized {
                    resize_client(hwnd_of(key), (w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
                }
            }
        }
        UiEvent::Frame { id, picture } => {
            let key = with_app(|a| {
                let key = *a.by_id.get(&id)?;
                let r = a.remotes.get_mut(&key)?;
                r.frames += 1;
                let gpu_ok = match r.presenter.as_mut() {
                    Some(p) => p.present(&picture),
                    None => false,
                };
                if !gpu_ok && r.presenter.take().is_some() {
                    eprintln!("Direct3D presenter failed; falling back to GDI for window {id}");
                }
                r.picture = Some(picture);
                Some((key, gpu_ok))
            })
            .flatten();
            if let Some((k, false)) = key {
                unsafe { let _ = InvalidateRect(Some(hwnd_of(k)), None, false); }
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
            let idle = with_app(|a| a.smoke.is_none() && a.remotes.is_empty() && !a.launcher.as_ref().is_some_and(|l| unsafe { IsWindowVisible(l.hwnd).as_bool() })).unwrap_or(false);
            if idle {
                quit(0);
            }
        }
        UiEvent::AppExited(app) => eprintln!("remote app exited: {app}"),
        UiEvent::Notice(n) => eprintln!("notice: {n}"),
        UiEvent::Disconnected(why) => {
            eprintln!("disconnected: {why}");
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
        let style = if owned { WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_THICKFRAME.0) } else { WS_OVERLAPPEDWINDOW };
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("RmRemoteWindow"), &HSTRING::from(title), style, CW_USEDEFAULT, CW_USEDEFAULT,
            w as i32, h as i32, if owned { owner.map(hwnd_of) } else { None }, None, Some(HINSTANCE(hinst as *mut c_void)), None);
        let Ok(hwnd) = hwnd else {
            eprintln!("CreateWindowExW failed for remote window {id}");
            return;
        };
        let aumid = format!("RemoteMac.{}", app.replace(|c: char| !c.is_ascii_alphanumeric(), "_"));
        if !owned && !native::set_app_user_model_id(hwnd, &aumid) {
            // Taskbar identity before the window is shown: own group + icon per remote application.
            eprintln!("warning: could not set AppUserModelID {aumid}");
        }
        let scale = native::dpi_scale(hwnd);
        let (cached, parent_origin) = with_app(|a| {
            a.remotes.insert(hwnd.0 as isize, Remote { id, app: app.into(), role, parent, rx: x, ry: y, rw: w, rh: h, scale, maximized: false, presenter: None, picture: None, frames: 0, high_surrogate: None });
            a.by_id.insert(id, hwnd.0 as isize);
            let cached = a.icons.get(app).copied();
            if cached.is_none() && a.icons_requested.insert(app.to_string()) {
                a.link.send(&Message::GetAppIcon { application_id: app.into() });
            }
            let parent_origin = owner.and_then(|o| a.remotes.get(&o)).map(|p| (p.rx, p.ry));
            (cached, parent_origin)
        })
        .unwrap_or((None, None));
        if let Some(icon) = cached {
            native::set_window_icon(hwnd, HICON(icon as *mut c_void));
        }
        resize_client(hwnd, (w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
        if let (true, Some(o), Some((px, py))) = (owned, owner, parent_origin) {
            // keep the dialog where the Mac put it relative to its parent
            let mut orc = RECT::default();
            let _ = GetWindowRect(hwnd_of(o), &mut orc);
            let nx = orc.left + ((x - px) as f64 * scale).round() as i32;
            let ny = orc.top + ((y - py) as f64 * scale).round() as i32;
            let _ = SetWindowPos(hwnd, None, nx, ny, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let presenter = if with_app(|a| a.d3d).unwrap_or(false) { d3d::Presenter::new(hwnd, w, h) } else { None };
        let renderer = presenter.as_ref().map(|p| p.kind).unwrap_or("gdi");
        with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| r.presenter = presenter));
        eprintln!("window created id={id} app={app} role={role:?} parent={parent:?} renderer={renderer} {w}x{h}pt scale={scale} title={title:?}");
    }
}

/// Current client size of the window expressed in Mac points.
fn client_points(hwnd: HWND) -> (u32, u32, f64) {
    let (cw, ch) = client_size(hwnd);
    let scale = with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.scale)).flatten().unwrap_or(1.0);
    ((cw as f64 / scale).round().max(1.0) as u32, (ch as f64 / scale).round().max(1.0) as u32, scale)
}

fn request_remote_resize(hwnd: HWND) {
    let (pw, ph, _) = client_points(hwnd);
    send_for(hwnd, |r, _| (pw.abs_diff(r.rw) > 2 || ph.abs_diff(r.rh) > 2).then_some(Message::WindowResizeRequest { window_id: r.id, width: pw, height: ph }));
}

fn resize_client(hwnd: HWND, w: i32, h: i32) {
    unsafe {
        let mut cur = RECT::default();
        let _ = GetClientRect(hwnd, &mut cur);
        if (cur.right - w).abs() <= 2 && (cur.bottom - h).abs() <= 2 {
            return;
        }
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

// ------------------------------------------------------------------ remote windows

fn send_for(hwnd: HWND, f: impl FnOnce(&Remote, bool) -> Option<Message>) {
    with_app(|a| {
        if let Some(r) = a.remotes.get(&(hwnd.0 as isize)) {
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
fn repaint_gpu(hwnd: HWND) -> bool {
    with_app(|a| {
        let r = a.remotes.get_mut(&(hwnd.0 as isize))?;
        let (p, pic) = (r.presenter.as_mut()?, r.picture.as_ref()?);
        Some(p.present(pic))
    })
    .flatten()
    .unwrap_or(false)
}

fn paint_into(hwnd: HWND, hdc: HDC) {
    let (cw, ch) = client_size(hwnd);
    unsafe {
        let drew = with_app(|a| {
            let Some(p) = a.remotes.get(&(hwnd.0 as isize)).and_then(|r| r.picture.as_ref()) else { return false };
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
            FillRect(hdc, &RECT { left: 0, top: 0, right: cw, bottom: ch }, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        }
    }
}

unsafe extern "system" fn remote_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !repaint_gpu(hwnd) {
                paint_into(hwnd, hdc);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint_into(hwnd, HDC(wp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            let (x, y) = lp_xy(lp);
            let cs = client_size(hwnd);
            send_for(hwnd, |r, _| {
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
            if down { SetCapture(hwnd); } else { let _ = ReleaseCapture(); }
            send_for(hwnd, |r, _| {
                let (px, py) = scale_point(x, y, cs, (r.rw, r.rh));
                Some(Message::MouseButton { window_id: r.id, button, down, x: px, y: py })
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16 as f64 / 120.0 * 40.0;
            send_for(hwnd, |r, _| {
                Some(if msg == WM_MOUSEWHEEL { Message::Scroll { window_id: r.id, dx: 0.0, dy: delta } } else { Message::Scroll { window_id: r.id, dx: delta, dy: 0.0 } })
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wp.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            if (msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP) && vk == VK_F4.0 as u32 {
                return DefWindowProcW(hwnd, msg, wp, lp); // Alt+F4 closes the local window as usual
            }
            if is_modifier_vk(vk) {
                return LRESULT(0);
            }
            let mods = current_mods();
            if sends_as_text(vk, mods) {
                return LRESULT(0); // WM_CHAR delivers the character, layout-correct
            }
            if let Some(name) = vk_to_physical(vk) {
                send_for(hwnd, |r, cc| Some(Message::Key { window_id: r.id, physical_key: name.into(), modifiers: map_modifiers(mods, cc), down }));
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
                    let r = a.remotes.get_mut(&(hwnd.0 as isize))?;
                    push_utf16(&mut r.high_surrogate, unit).map(|t| (r.id, t))
                })
                .flatten();
                if let Some((id, text)) = text {
                    with_app(|a| a.link.send(&Message::TextInput { window_id: id, text }));
                }
            }
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            request_remote_resize(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            // Maximize / restore do not go through a size-move loop.
            let kind = wp.0 as u32;
            let was = with_app(|a| a.remotes.get_mut(&(hwnd.0 as isize)).map(|r| std::mem::replace(&mut r.maximized, kind == SIZE_MAXIMIZED))).flatten();
            if let Some(was_max) = was {
                if kind == SIZE_MAXIMIZED || (kind == SIZE_RESTORED && was_max) {
                    request_remote_resize(hwnd);
                }
            }
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
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing the local window asks the remote window to close; we disappear when it does.
            send_for(hwnd, |r, _| Some(Message::WindowClose { window_id: r.id }));
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(|a| {
                if let Some(r) = a.remotes.remove(&(hwnd.0 as isize)) {
                    a.by_id.remove(&r.id);
                }
            });
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

fn painted_colors(hwnd: HWND) -> usize {
    unsafe {
        let (w, h) = client_size(hwnd);
        if w <= 0 || h <= 0 {
            return 0;
        }
        let hdc = GetDC(Some(hwnd));
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp.into());
        let _ = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(0x2)); // PW_RENDERFULLCONTENT
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), hdc);
        Picture { width: w as usize, height: h as usize, bgra: buf }.distinct_colors()
    }
}

fn sendmsg(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    unsafe { SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp))) };
}

fn type_char(hwnd: HWND, c: char) {
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
            let renderer = with_app(|a| a.remotes.get(&(hwnd.0 as isize)).map(|r| r.presenter.as_ref().map(|p| p.kind).unwrap_or("gdi"))).flatten().unwrap_or("?");
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
            let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
            let sc = native::dpi_scale(hwnd);
            let mut r = RECT { left: 0, top: 0, right: (640.0 * sc).round() as i32, bottom: (400.0 * sc).round() as i32 };
            let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
            let _ = SetWindowPos(hwnd, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
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
            if let Some((2, ids)) = ids {
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
                    finish("input goes to the right app when several run", tm.contains("[17 chars]"), format!("notes={tn:?} testapp={tm:?}"), 33);
                }
            }
        }
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
            sendmsg(hwnd, WM_CLOSE, 0, 0);
            with_app(|a| a.smoke.as_mut().map(|s| s.stage = 10));
        }
        (10, _) if !destroyed.is_empty() && find_window("testapp", WindowRole::Window).is_none() => {
            let notes_alive = find_window("notes", WindowRole::Window).is_some();
            finish("closing one app's window leaves the other app running", notes_alive, format!("destroyed={destroyed:?}"), 100);
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
