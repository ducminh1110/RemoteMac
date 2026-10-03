//! "Connect to your Mac" window: the Mac prints an ID and a password, the user types them here.
//! A plain Win32 window (Inter, light Mac-like colours) with its own small message loop; it
//! returns once the user presses Connect (or closes it).

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("RmConnect");
const ID_EDIT: i32 = 101;
const PW_EDIT: i32 = 102;
const BG: (u8, u8, u8) = (247, 247, 248);
const W: i32 = 420;
const H: i32 = 330;

struct State {
    id: HWND,
    pw: HWND,
    error: HWND,
    done: Option<Option<(String, String)>>,
    heading: HFONT,
    body: HFONT,
    bg: HBRUSH,
}

thread_local! { static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) }; }

fn rgb((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn font(face: &str, px: i32, weight: i32) -> HFONT {
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from(face)) }
}

fn text_of(h: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 128];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize]).trim().to_string()
    }
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

/// Show the window and connect from it: `try_connect(id, password)` runs on a background
/// thread while the window stays responsive ("Connecting…"); a failure is shown in red and the
/// user can try again. `id` prefills the ID, `error` starts with a message (a lost connection).
/// Returns what `try_connect` returned, or None if the user closed the window.
pub fn connect_window<T: Send>(id: Option<&str>, error: Option<&str>, try_connect: impl Fn(&str, &str) -> Result<T, String> + Sync) -> Option<T> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc); // a second call fails harmlessly (already registered)
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("RemoteMac"), style, CW_USEDEFAULT, CW_USEDEFAULT, W, H, None, None, Some(hinst), None).ok()?;
        let s = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
        let px = |v: i32| (v as f64 * s).round() as i32;
        // size the client area, centre on the screen
        let mut r = RECT { left: 0, top: 0, right: px(W), bottom: px(H) };
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let _ = SetWindowPos(hwnd, None, (sw - ww) / 2, (sh - wh) / 3, ww, wh, SWP_NOZORDER);

        let body = font(crate::native::ui_face(400), px(14), 400);
        let heading = font(crate::native::ui_face(600), px(22), 600);
        let mono = font(crate::native::mono_face(), px(16), 400);
        let child = |class: PCWSTR, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: i32, f: HFONT| {
            let c = CreateWindowExW(WINDOW_EX_STYLE(ex), class, &HSTRING::from(text), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style), px(x), px(y), px(w), px(h),
                Some(hwnd), Some(HMENU(id as isize as *mut c_void)), Some(hinst), None).unwrap_or_default();
            SendMessageW(c, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
            c
        };
        let edit_style = WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32;
        child(w!("STATIC"), "ID", 0, 0, 32, 92, 356, 18, 0, body);
        let id_edit = child(w!("EDIT"), id.unwrap_or(""), edit_style, 0, 32, 112, 356, 30, ID_EDIT, mono);
        child(w!("STATIC"), "Password", 0, 0, 32, 152, 356, 18, 0, body);
        let pw_edit = child(w!("EDIT"), "", edit_style | ES_PASSWORD as u32, 0, 32, 172, 356, 30, PW_EDIT, mono);
        let error_label = child(w!("STATIC"), error.unwrap_or(""), 0, 0, 32, 212, 356, 38, 0, body);
        child(w!("BUTTON"), "Connect", WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, 0, 268, 266, 120, 34, IDOK.0, body);
        STATE.with(|st| {
            *st.borrow_mut() = Some(State { id: id_edit, pw: pw_edit, error: error_label, done: None, heading, body, bg: CreateSolidBrush(rgb(BG)) });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(if id.is_some() { pw_edit } else { id_edit }));

        let button = GetDlgItem(Some(hwnd), IDOK.0).unwrap_or_default();
        let mut msg = MSG::default();
        let result = std::thread::scope(|scope| {
            let mut attempt: Option<(String, std::thread::ScopedJoinHandle<'_, Result<T, String>>)> = None;
            loop {
                // a connection attempt finished?
                if attempt.as_ref().is_some_and(|(_, h)| h.is_finished()) {
                    let (typed, h) = attempt.take().unwrap();
                    match h.join().unwrap_or_else(|_| Err("internal error while connecting".into())) {
                        Ok(t) => {
                            remember_id(&typed);
                            break Some(t);
                        }
                        Err(e) => {
                            set_text(error_of(), &e);
                            let _ = EnableWindow(button, true);
                            let _ = EnableWindow(edit_of(true), true);
                            let _ = EnableWindow(edit_of(false), true);
                            let _ = SetFocus(Some(edit_of(false)));
                        }
                    }
                }
                match STATE.with(|st| st.borrow_mut().as_mut().and_then(|s| s.done.take())) {
                    Some(None) => break None,
                    Some(Some((typed, pw))) if attempt.is_none() => {
                        let _ = EnableWindow(button, false);
                        let _ = EnableWindow(edit_of(true), false);
                        let _ = EnableWindow(edit_of(false), false);
                        let f = &try_connect;
                        let t2 = typed.clone();
                        attempt = Some((typed, scope.spawn(move || f(&t2, &pw))));
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
                // Tab between fields, Enter = Connect
                if !IsDialogMessageW(hwnd, &msg).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });
        let _ = DestroyWindow(hwnd);
        if let Some(s) = STATE.with(|st| st.borrow_mut().take()) {
            let _ = DeleteObject(s.heading.into());
            let _ = DeleteObject(s.body.into());
            let _ = DeleteObject(s.bg.into());
        }
        let _ = DeleteObject(mono.into());
        result
    }
}

fn set_text(h: HWND, t: &str) {
    unsafe {
        let _ = SetWindowTextW(h, &HSTRING::from(t));
    }
}

fn error_of() -> HWND {
    STATE.with(|st| st.borrow().as_ref().map(|s| s.error)).unwrap_or_default()
}

fn edit_of(id: bool) -> HWND {
    STATE.with(|st| st.borrow().as_ref().map(|s| if id { s.id } else { s.pw })).unwrap_or_default()
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let s = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
            let px = |v: i32| (v as f64 * s).round() as i32;
            STATE.with(|st| {
                if let Some(st) = st.borrow().as_ref() {
                    FillRect(hdc, &rc, st.bg);
                    SetBkMode(hdc, TRANSPARENT);
                    let old = SelectObject(hdc, st.heading.into());
                    SetTextColor(hdc, rgb((28, 28, 30)));
                    let mut t = RECT { left: px(32), top: px(26), right: rc.right - px(32), bottom: px(56) };
                    let mut h: Vec<u16> = "Connect to your Mac".encode_utf16().collect();
                    DrawTextW(hdc, &mut h, &mut t, DT_LEFT | DT_SINGLELINE);
                    SelectObject(hdc, st.body.into());
                    SetTextColor(hdc, rgb((110, 110, 115)));
                    let mut t = RECT { left: px(32), top: px(58), right: rc.right - px(32), bottom: px(80) };
                    let mut h: Vec<u16> = "Run ./remotemac on the Mac, then type its ID and password.".encode_utf16().collect();
                    DrawTextW(hdc, &mut h, &mut t, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);
                    SelectObject(hdc, old);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut c_void);
            let ctl = HWND(lp.0 as *mut c_void);
            STATE.with(|st| match st.borrow().as_ref() {
                Some(st) => {
                    SetBkColor(hdc, rgb(BG));
                    SetTextColor(hdc, if ctl == st.error { rgb((200, 40, 40)) } else { rgb((60, 60, 64)) });
                    LRESULT(st.bg.0 as isize)
                }
                None => DefWindowProcW(hwnd, msg, wp, lp),
            })
        }
        WM_COMMAND if (wp.0 & 0xFFFF) as i32 == IDOK.0 => {
            // read under a short borrow; every Win32 call below may re-enter this procedure
            let Some((id_h, pw_h, err_h)) = STATE.with(|st| st.borrow().as_ref().map(|s| (s.id, s.pw, s.error))) else { return LRESULT(0) };
            let (id, pw) = (text_of(id_h), text_of(pw_h));
            match rm_protocol::session::normalize_id(&id) {
                None => {
                    set_text(err_h, "The ID is the 9 digits shown on the Mac.");
                    let _ = SetFocus(Some(id_h));
                }
                Some(_) if pw.is_empty() => {
                    set_text(err_h, "Type the password shown on the Mac.");
                    let _ = SetFocus(Some(pw_h));
                }
                Some(id) => {
                    set_text(err_h, "Connecting…");
                    STATE.with(|st| {
                        if let Some(s) = st.borrow_mut().as_mut() {
                            s.done = Some(Some((id, pw)));
                        }
                    });
                    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            STATE.with(|st| {
                if let Some(st) = st.borrow_mut().as_mut() {
                    st.done = Some(None);
                }
            });
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xFFFF) as i32 == IDCANCEL.0 => {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
