//! "Connect to your Mac" window: the Mac prints an ID and a password, the user types them here.
//! A Mac on the same network is found by its ID; for one elsewhere the relay server is used (the
//! one built into this build, or one the user types; it is remembered). Or the user types the
//! Mac's address (an IP or a host name, any network that reaches it). While it connects, the
//! steps show as they happen (finding the Mac, checking the password, setting up, video).
//! It looks like the sign-in of Apple's Screen Sharing (connectui.rs draws it; its fields are
//! MacBridge's own). It has its own small message loop and returns once the user connects (or
//! closes it).

use crate::connectui::{Act, ConnectView, Key};
use std::ffi::c_void;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub use crate::connectui::Via;

const CLASS: PCWSTR = w!("RmConnect");
const TIMER: usize = 1;
const WM_MOUSE_LEAVE: u32 = 0x02A3;

struct Shell {
    view: ConnectView,
    done: Option<Option<(String, String, Via)>>,
}

thread_local! { static STATE: std::cell::RefCell<Option<Shell>> = const { std::cell::RefCell::new(None) }; }

/// The window's state (None while it is in use further up: a message sent from inside).
fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    STATE.with(|st| st.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Last ID used on this PC (so next time only the password is typed).
fn last_id_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("RemoteMac").join("last-id"))
}

pub fn remember_id(id: &str) {
    if let Some(p) = last_id_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, id);
    }
}

pub fn last_id() -> Option<String> {
    last_id_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The relay server last typed (blank when none was).
fn last_relay_path() -> Option<std::path::PathBuf> {
    last_id_path().map(|p| p.with_file_name("last-relay"))
}

pub fn remember_relay(relay: &str) {
    if let Some(p) = last_relay_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, relay.trim());
    }
}

/// The Mac address typed last (blank when none was), and whether it was used last.
fn last_address_path() -> Option<std::path::PathBuf> {
    last_id_path().map(|p| p.with_file_name("last-address"))
}

pub fn remember_via(via: &Via) {
    let Some(p) = last_address_path() else { return };
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    match via {
        Via::Id(relay) => {
            remember_relay(relay);
            // the address stays remembered, but the ID mode is the one used now
            if let Ok(a) = std::fs::read_to_string(&p) {
                let _ = std::fs::write(&p, a.trim_start_matches('*'));
            }
        }
        Via::Address(a) => {
            let _ = std::fs::write(&p, format!("*{}", a.trim()));
        }
    }
}

/// (address typed last, used last)
pub fn last_address() -> (String, bool) {
    let s = last_address_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let s = s.trim();
    match s.strip_prefix('*') {
        Some(a) => (a.to_string(), true),
        None => (s.to_string(), false),
    }
}

/// What the relay field starts with: what was typed last, else the relay built into this build.
pub fn last_relay() -> String {
    last_relay_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| rm_protocol::session::default_relay().map(String::from))
        .unwrap_or_default()
}

/// Show the window and connect from it: `try_connect(id, password, via)` runs on a background
/// thread while the window stays responsive (the steps with a spinner); a failure is shown in
/// red and the user can try again. `id` prefills the ID, `error` starts with a message (a lost
/// connection). Returns what `try_connect` returned, or None if the user closed the window.
pub fn connect_window<T: Send>(id: Option<&str>, error: Option<&str>, try_connect: impl Fn(&str, &str, &Via) -> Result<T, String> + Sync) -> Option<T> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc); // a second call fails harmlessly (already registered)
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME;
        let (address, by_address) = last_address();
        let mut view = ConnectView::new(id.unwrap_or(""), &last_relay(), &address, by_address);
        if let Some(e) = error {
            view.set_error(e);
        }
        STATE.with(|st| *st.borrow_mut() = Some(Shell { view, done: None }));
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge"), style, CW_USEDEFAULT, CW_USEDEFAULT, 460, 572, None, None, Some(hinst), None).ok()?;
        crate::frame::adopt(hwnd);
        // its size at this scale, a little above the middle of the screen
        let s = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let (ww, wh) = ((crate::connectui::W * s).round() as i32, (crate::connectui::H * s).round() as i32);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let _ = SetWindowPos(hwnd, None, (sw - ww) / 2, (sh - wh) / 3, ww, wh, SWP_NOZORDER);
        restyle(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        schedule(hwnd);

        let mut msg = MSG::default();
        let result = std::thread::scope(|scope| {
            type Attempt<'s, T> = ((String, Via), std::thread::ScopedJoinHandle<'s, Result<T, String>>);
            let mut attempt: Option<Attempt<'_, T>> = None;
            loop {
                // the step the attempt is at
                if attempt.is_some() {
                    let p = crate::lifecycle::current();
                    if p.busy() {
                        let label = p.label();
                        let changed = with(|sh| {
                            let now = matches!(sh.view.status(), crate::connectui::Status::Busy(l) if *l == label);
                            if !now {
                                sh.view.set_step(&label);
                            }
                            !now
                        });
                        if changed == Some(true) {
                            redraw(hwnd);
                        }
                    }
                }
                // a connection attempt finished?
                if attempt.as_ref().is_some_and(|(_, h)| h.is_finished()) {
                    let ((typed, via), h) = attempt.take().unwrap();
                    match h.join().unwrap_or_else(|_| Err("internal error while connecting".into())) {
                        Ok(t) => {
                            // (by address no ID was typed: the one remembered stays)
                            if !typed.is_empty() {
                                remember_id(&typed);
                            }
                            remember_via(&via);
                            break Some(t);
                        }
                        Err(e) => {
                            with(|sh| sh.view.fail(&e));
                            redraw(hwnd);
                            schedule(hwnd);
                        }
                    }
                }
                match with(|sh| sh.done.take()).flatten() {
                    Some(None) => break None,
                    Some(Some((typed, pw, via))) if attempt.is_none() => {
                        let f = &try_connect;
                        let (t2, v2) = (typed.clone(), via.clone());
                        attempt = Some(((typed, via), scope.spawn(move || f(&t2, &pw, &v2))));
                        schedule(hwnd);
                    }
                    _ => {}
                }
                // pump without blocking for long, so the attempt is noticed when it ends
                if attempt.is_some() {
                    if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = MsgWaitForMultipleObjects(None, false, 50, QS_ALLINPUT);
                        continue;
                    }
                    if msg.message == WM_QUIT {
                        break None;
                    }
                } else if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break None;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        let _ = DestroyWindow(hwnd);
        STATE.with(|st| st.borrow_mut().take());
        result
    }
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// The next frame when something moves (or the caret blinks); none while nothing does.
fn schedule(hwnd: HWND) {
    let next = with(|sh| sh.view.next_frame(Instant::now())).flatten();
    unsafe {
        match next {
            Some(d) => {
                SetTimer(Some(hwnd), TIMER, (d.as_millis() as u32).clamp(10, 1000), None);
            }
            None => {
                let _ = KillTimer(Some(hwnd), TIMER);
            }
        }
    }
}

/// The size, scale and appearance, to the view.
fn restyle(hwnd: HWND) {
    let (dark, _, level) = crate::surface::glass_look();
    crate::frame::appearance(hwnd, dark);
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let s = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    with(|sh| sh.view.set_window(rc.right.max(1) as usize, rc.bottom.max(1) as usize, s, dark, level));
    redraw(hwnd);
}

fn paint(hdc: HDC) {
    let Some(c) = with(|sh| sh.view.render(Instant::now())) else { return };
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: c.w as i32, biHeight: -(c.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    unsafe {
        StretchDIBits(hdc, 0, 0, c.w as i32, c.h as i32, 0, 0, c.w as i32, c.h as i32, Some(c.px.as_ptr() as *const c_void), &bi, DIB_RGB_COLORS, SRCCOPY);
    }
}

/// What a click or a key asked for.
fn act(hwnd: HWND, a: Option<Act>) {
    match a {
        Some(Act::Connect { id, password, via }) => {
            with(|sh| sh.done = Some(Some((id, password, via))));
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            }
        }
        Some(Act::Cancel) => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Some(Act::Window(l)) => crate::frame::press(hwnd, l),
        Some(Act::Copy(s)) => {
            crate::native::set_clipboard_text(hwnd, &s);
        }
        None => {}
    }
    redraw(hwnd);
    schedule(hwnd);
}

fn xy(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // no Windows title bar: the window is drawn whole, as a Mac window (frame.rs)
        WM_NCCALCSIZE => crate::frame::calc_size(hwnd, wp, lp),
        WM_NCHITTEST => with(|sh| crate::frame::hit_test(hwnd, lp, false, |x, y| sh.view.is_caption(x, y))).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
        WM_NCACTIVATE => {
            with(|sh| sh.view.set_active(wp.0 != 0));
            redraw(hwnd);
            schedule(hwnd);
            DefWindowProcW(hwnd, msg, wp, LPARAM(-1))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint(HDC(wp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            restyle(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            restyle(hwnd);
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            restyle(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == TIMER => {
            redraw(hwnd);
            schedule(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            with(|sh| sh.view.mouse_move(x, y));
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            redraw(hwnd);
            LRESULT(0)
        }
        WM_MOUSE_LEAVE => {
            with(|sh| sh.view.mouse_leave());
            redraw(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            SetCapture(hwnd);
            let _ = SetFocus(Some(hwnd));
            let shift = wp.0 & 0x0004 != 0; // MK_SHIFT
            with(|sh| sh.view.mouse_down(x, y, shift));
            act(hwnd, None);
            LRESULT(0)
        }
        WM_LBUTTONDBLCLK => {
            let (x, y) = xy(lp);
            with(|sh| sh.view.double_click(x, y));
            act(hwnd, None);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            let _ = ReleaseCapture();
            let a = with(|sh| sh.view.mouse_up(x, y)).flatten();
            act(hwnd, a);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let (shift, ctrl) = (down(VK_SHIFT), down(VK_CONTROL));
            let vk = VIRTUAL_KEY(wp.0 as u16);
            if ctrl && vk == VIRTUAL_KEY(b'V' as u16) {
                if let Some(t) = crate::native::clipboard_text(hwnd) {
                    with(|sh| sh.view.paste(&t));
                }
                act(hwnd, None);
                return LRESULT(0);
            }
            let k = match vk {
                VK_TAB => Some(Key::Tab),
                VK_RETURN => Some(Key::Enter),
                VK_ESCAPE => Some(Key::Escape),
                VK_SPACE => Some(Key::Space),
                VK_LEFT => Some(Key::Left),
                VK_RIGHT => Some(Key::Right),
                VK_UP => Some(Key::Up),
                VK_DOWN => Some(Key::Down),
                VK_HOME => Some(Key::Home),
                VK_END => Some(Key::End),
                VK_BACK => Some(Key::Backspace),
                VK_DELETE => Some(Key::Delete),
                v if ctrl && v == VIRTUAL_KEY(b'A' as u16) => Some(Key::SelectAll),
                v if ctrl && v == VIRTUAL_KEY(b'C' as u16) => Some(Key::Copy),
                v if ctrl && v == VIRTUAL_KEY(b'X' as u16) => Some(Key::Cut),
                _ => None,
            };
            if let Some(k) = k {
                // Space types a space in a field (WM_CHAR); elsewhere it presses
                let in_field = with(|sh| matches!(sh.view.focus(), crate::connectui::Focus::Field(_))).unwrap_or(false);
                if !(k == Key::Space && in_field) {
                    let a = with(|sh| sh.view.key(k, shift, ctrl)).flatten();
                    act(hwnd, a);
                }
            }
            LRESULT(0)
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(wp.0 as u32) {
                if !c.is_control() {
                    with(|sh| sh.view.char(c));
                    act(hwnd, None);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            with(|sh| sh.done = Some(None));
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
