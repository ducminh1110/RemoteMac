//! "Connect to your Mac" window: it opens on the Macs kept on this PC (a click connects; + adds
//! one with the ID and the password the Mac shows, or its address). A Mac on the same network is
//! found by its ID; for one elsewhere the relay server is used (the one built into this build, or
//! one's own with its key). While it connects, the steps show as they happen (finding the Mac,
//! checking the password, setting up, video). It looks like Apple's Screen Sharing (connectui.rs
//! draws it; its fields are Taro's own). It has its own small message loop and returns once the
//! user connects (or closes it).

use crate::connectui::{Act, Computer, ConnectView, Key};
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

/// What to connect to: the ID, the password, how, the relay's key.
type Target = (String, String, Via, String);

struct Shell {
    view: ConnectView,
    done: Option<Option<Target>>,
}

thread_local! { static STATE: std::cell::RefCell<Option<Shell>> = const { std::cell::RefCell::new(None) }; }

/// The connect window, for the guided tour: where the controls are on the screen, what is typed.
pub fn tour_probe() -> Option<(HWND, (i32, i32), crate::connectui::TourProbe)> {
    let hwnd = HWND(CONNECT_HWND.with(|h| h.get()) as *mut c_void);
    if hwnd.0.is_null() {
        return None;
    }
    let mut origin = POINT::default();
    unsafe {
        let _ = ClientToScreen(hwnd, &mut origin);
    }
    with(|sh| sh.view.probe()).map(|p| (hwnd, (origin.x, origin.y), p))
}

thread_local! { static CONNECT_HWND: std::cell::Cell<isize> = const { std::cell::Cell::new(0) }; }

/// The window's state (None while it is in use further up: a message sent from inside).
fn with<R>(f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
    STATE.with(|st| st.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Where Taro keeps what it remembers on this PC.
fn dir() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("RemoteMac"))
}

/// The Macs kept on this PC; their passwords and relay keys are protected for this Windows user
/// (DPAPI), so another user of the PC, or a copy of the file, cannot read them.
fn computers_path() -> Option<std::path::PathBuf> {
    dir().map(|d| d.join("computers.json"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// A secret, protected for this user ("" stays "").
fn protect(secret: &str) -> String {
    use windows::Win32::Security::Cryptography::*;
    if secret.is_empty() {
        return String::new();
    }
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: secret.len() as u32, pbData: secret.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        if CryptProtectData(&input, w!("Taro"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).is_err() {
            return String::new();
        }
        let sealed = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut c_void)));
        format!("dpapi:{}", hex(&sealed))
    }
}

/// A secret [`protect`] kept (None when it cannot be read: another user's, or damaged).
fn unprotect(kept: &str) -> Option<String> {
    use windows::Win32::Security::Cryptography::*;
    if kept.is_empty() {
        return Some(String::new());
    }
    let sealed = unhex(kept.strip_prefix("dpapi:")?)?;
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: sealed.len() as u32, pbData: sealed.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
        let plain = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut c_void)));
        String::from_utf8(plain).ok()
    }
}

/// The Macs kept on this PC (on the first run of this version, the Mac typed last before it).
pub fn load_computers() -> Vec<Computer> {
    let Some(p) = computers_path() else { return vec![] };
    let Ok(text) = std::fs::read_to_string(&p) else { return migrated() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let field = |m: &serde_json::Value, k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    v.get("computers")
        .and_then(|l| l.as_array())
        .map(|l| {
            l.iter()
                .map(|m| Computer {
                    name: field(m, "name"),
                    id: field(m, "id"),
                    address: field(m, "address"),
                    by_address: m.get("by_address").and_then(|x| x.as_bool()).unwrap_or(false),
                    relay: field(m, "relay"),
                    // a secret that cannot be read is asked for again
                    key: unprotect(&field(m, "key")).unwrap_or_default(),
                    password: unprotect(&field(m, "password")).unwrap_or_default(),
                })
                .filter(|c| if c.by_address { !c.address.is_empty() } else { c.id.len() == 9 })
                .collect()
        })
        .unwrap_or_default()
}

pub fn save_computers(list: &[Computer]) {
    let Some(p) = computers_path() else { return };
    let macs: Vec<serde_json::Value> = list
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name, "id": c.id, "address": c.address, "by_address": c.by_address,
                "relay": c.relay, "key": protect(&c.key), "password": protect(&c.password),
            })
        })
        .collect();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "version": 1, "computers": macs })).unwrap_or_default();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    // written whole then moved over the old one: a crash mid-write leaves the old list
    let tmp = p.with_extension("json.new");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// Before 1.3 Taro kept only the ID, the relay and the address typed last: they become the first
/// Macs of the list (without a password, asked for at the first click).
fn migrated() -> Vec<Computer> {
    let read = |name: &str| dir().and_then(|d| std::fs::read_to_string(d.join(name)).ok()).map(|s| s.trim().to_string()).unwrap_or_default();
    let (id, relay, address) = (read("last-id"), read("last-relay"), read("last-address"));
    let built_in = rm_protocol::session::default_relay().unwrap_or_default();
    let mut list = vec![];
    if rm_protocol::session::normalize_id(&id).is_some() {
        let relay = if relay == built_in { String::new() } else { relay };
        list.push(Computer { id: rm_protocol::session::normalize_id(&id).unwrap_or_default(), relay, ..Default::default() });
    }
    let (address, used_last) = match address.strip_prefix('*') {
        Some(a) => (a.to_string(), true),
        None => (address, false),
    };
    if !address.is_empty() {
        let mac = Computer { by_address: true, address, ..Default::default() };
        if used_last {
            list.insert(0, mac);
        } else {
            list.push(mac);
        }
    }
    list
}

/// Show the window and connect from it: `try_connect(id, password, via, key)` runs on a
/// background thread while the window stays responsive (the steps with a spinner); a failure is
/// shown in red and the user can try again. `error` starts with a message (a lost connection).
/// Returns what `try_connect` returned, or None if the user closed the window.
pub fn connect_window<T: Send>(error: Option<&str>, try_connect: impl Fn(&str, &str, &Via, &str) -> Result<T, String> + Sync) -> Option<T> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc); // a second call fails harmlessly (already registered)
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME;
        let mut view = ConnectView::new(load_computers());
        // the guided tour's first part: the demo Mac, above the list
        view.set_demo(crate::tour::offers_demo());
        if let Some(e) = error {
            view.set_error(e);
        }
        STATE.with(|st| *st.borrow_mut() = Some(Shell { view, done: None }));
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, &windows::core::HSTRING::from(crate::brand::name()), style, CW_USEDEFAULT, CW_USEDEFAULT, 460, 600, None, None, Some(hinst), None).ok()?;
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
        CONNECT_HWND.with(|h| h.set(hwnd.0 as isize));
        crate::tour::start(hinst);

        let mut msg = MSG::default();
        let result = std::thread::scope(|scope| {
            type Attempt<'s, T> = std::thread::ScopedJoinHandle<'s, Result<T, String>>;
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
                if attempt.as_ref().is_some_and(|h| h.is_finished()) {
                    let h = attempt.take().unwrap();
                    match h.join().unwrap_or_else(|_| Err("internal error while connecting".into())) {
                        Ok(t) => break Some(t),
                        Err(e) => {
                            with(|sh| sh.view.fail(&e));
                            redraw(hwnd);
                            schedule(hwnd);
                        }
                    }
                }
                match with(|sh| sh.done.take()).flatten() {
                    Some(None) => break None,
                    Some(Some((id, pw, via, key))) if attempt.is_none() => {
                        let f = &try_connect;
                        attempt = Some(scope.spawn(move || f(&id, &pw, &via, &key)));
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
        crate::tour::close();
        CONNECT_HWND.with(|h| h.set(0));
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
        Some(Act::Connect { id, password, via, key }) => {
            with(|sh| sh.done = Some(Some((id, password, via, key))));
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
        Some(Act::Save(list)) => save_computers(&list),
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
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
            with(|sh| sh.view.wheel(notches));
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
                v if ctrl && v == VIRTUAL_KEY(b'N' as u16) => Some(Key::New),
                _ => None,
            };
            if let Some(k) = k {
                // Space types a space in a field (WM_CHAR); elsewhere it presses
                let in_field = with(|sh| sh.view.in_field()).unwrap_or(false);
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
