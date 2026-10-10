//! "Connect to your Mac" window: the Mac prints an ID and a password, the user types them here.
//! A Mac on the same network is found by its ID; for one elsewhere the relay server is used (the
//! one built into this build, or one the user types; it is remembered). Or the user types the
//! Mac's address (an IP or a host name, any network that reaches it). While it connects, the
//! steps show as they happen (finding the Mac, checking the password, setting up, video).
//! It looks like the launcher (MobileLab's look, look.rs): a tinted window, one floating rounded
//! panel with the mark and a large title, filled fields, a segmented "Find it by its ID / Type
//! its address" control and an accent Connect button; the fields are Windows' own edit controls
//! (typing, IME, Tab and Enter as everywhere). It has its own small message loop and returns once
//! the user presses Connect (or closes it).

use std::ffi::c_void;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("RmConnect");
const ID_EDIT: i32 = 101;
const PW_EDIT: i32 = 102;
const RELAY_EDIT: i32 = 103;
const VIA_ID: i32 = 104;
const VIA_ADDRESS: i32 = 105;
const W: i32 = 440;
const H: i32 = 548;
/// Layout (DIPs): the panel, the fields' left edge and width, each field's top.
const PANEL: (i32, i32, i32, i32) = (16, 16, 408, 516);
const FX: i32 = 40;
const FW: i32 = 360;
const FH: i32 = 36;
const ID_Y: i32 = 176;
const PW_Y: i32 = 242;
const SEG_Y: i32 = 298;
const RELAY_Y: i32 = 368;
const STATUS_Y: i32 = 414;
const BUTTON: (i32, i32, i32, i32) = (264, 474, 136, 36);

/// How the Mac is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// found by its ID on this network, else through this relay ("" none)
    Id(String),
    /// straight to this address (IP or host name, :port when not 7471)
    Address(String),
}

struct State {
    id: HWND,
    pw: HWND,
    relay: HWND,
    error: HWND,
    field_label: HWND,
    by_address: bool,
    /// what the other mode's field held (kept while the user switches)
    other: String,
    done: Option<Option<(String, String, Via)>>,
    heading: HFONT,
    body: HFONT,
    /// the panel's and the fields' colours as brushes (for the edit controls' backgrounds)
    panel: HBRUSH,
    field: HBRUSH,
    dark: bool,
}

thread_local! { static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) }; }

fn rgb((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn colour(c: crate::paint::Rgba) -> COLORREF {
    rgb(((c.r * 255.0) as u8, (c.g * 255.0) as u8, (c.b * 255.0) as u8))
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
/// thread while the window stays responsive ("Connecting…"); a failure is shown in red and the
/// user can try again. `id` prefills the ID, `error` starts with a message (a lost connection).
/// Returns what `try_connect` returned, or None if the user closed the window.
pub fn connect_window<T: Send>(id: Option<&str>, error: Option<&str>, try_connect: impl Fn(&str, &str, &Via) -> Result<T, String> + Sync) -> Option<T> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc); // a second call fails harmlessly (already registered)
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        let dark = crate::surface::dark_mode();
        let t = crate::look::theme(dark);
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge"), style, CW_USEDEFAULT, CW_USEDEFAULT, W, H, None, None, Some(hinst), None).ok()?;
        let s = GetDpiForWindow(hwnd).max(96) as f64 / 96.0;
        let px = |v: i32| (v as f64 * s).round() as i32;
        // size the client area, centre on the screen
        let mut r = RECT { left: 0, top: 0, right: px(W), bottom: px(H) };
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let _ = SetWindowPos(hwnd, None, (sw - ww) / 2, (sh - wh) / 3, ww, wh, SWP_NOZORDER);
        frame_colours(hwnd, dark);

        let body = font(crate::native::ui_face(400), px(14), 400);
        let heading = font(crate::native::ui_face(600), px(22), 600);
        let mono = font(crate::native::mono_face(), px(16), 400);
        let child = |class: PCWSTR, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: i32, f: HFONT| {
            let c = CreateWindowExW(WINDOW_EX_STYLE(ex), class, &HSTRING::from(text), WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style), px(x), px(y), px(w), px(h),
                Some(hwnd), Some(HMENU(id as isize as *mut c_void)), Some(hinst), None).unwrap_or_default();
            SendMessageW(c, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
            c
        };
        // the edits sit in filled fields drawn behind them (no border of their own)
        let edit_style = WS_TABSTOP.0 | ES_AUTOHSCROLL as u32;
        let inner = |y: i32| (FX + 12, y + (FH - 22) / 2, FW - 24, 22);
        let (ex, ey, ew, eh) = inner(ID_Y);
        let id_edit = child(w!("EDIT"), id.unwrap_or(""), edit_style, 0, ex, ey, ew, eh, ID_EDIT, mono);
        let (ex, ey, ew, eh) = inner(PW_Y);
        let pw_edit = child(w!("EDIT"), "", edit_style | ES_PASSWORD as u32, 0, ex, ey, ew, eh, PW_EDIT, mono);
        // how to reach it: by its ID (this network, else a relay), or at an address typed here
        // (a segmented control: two buttons drawn here)
        let (address, by_address) = last_address();
        let half = FW / 2;
        child(w!("BUTTON"), "Find it by its ID", WS_TABSTOP.0 | WS_GROUP.0 | BS_OWNERDRAW as u32, 0, FX, SEG_Y, half, 32, VIA_ID, body);
        child(w!("BUTTON"), "Type its address", WS_TABSTOP.0 | BS_OWNERDRAW as u32, 0, FX + half, SEG_Y, half, 32, VIA_ADDRESS, body);
        let field_label = child(w!("STATIC"), field_text(by_address), 0, 0, FX, RELAY_Y - 20, FW, 18, 0, body);
        let (shown, other) = if by_address { (address, last_relay()) } else { (last_relay(), address) };
        let (ex, ey, ew, eh) = inner(RELAY_Y);
        let relay_edit = child(w!("EDIT"), &shown, edit_style | WS_GROUP.0, 0, ex, ey, ew, eh, RELAY_EDIT, mono);
        let error_label = child(w!("STATIC"), error.unwrap_or(""), 0, 0, FX, STATUS_Y, FW, 50, 0, body);
        let (bx, by, bw, bh) = BUTTON;
        child(w!("BUTTON"), "Connect", WS_TABSTOP.0 | WS_GROUP.0 | BS_OWNERDRAW as u32, 0, bx, by, bw, bh, IDOK.0, body);
        STATE.with(|st| {
            *st.borrow_mut() = Some(State { id: id_edit, pw: pw_edit, relay: relay_edit, error: error_label, field_label, by_address, other, done: None, heading, body, panel: CreateSolidBrush(colour(t.panel)), field: CreateSolidBrush(colour(t.field)), dark });
        });
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(if id.is_some() { pw_edit } else { id_edit }));

        let button = GetDlgItem(Some(hwnd), IDOK.0).unwrap_or_default();
        let mut msg = MSG::default();
        let result = std::thread::scope(|scope| {
            // (ID, way) of the attempt running, and its thread
            type Attempt<'s, T> = ((String, Via), std::thread::ScopedJoinHandle<'s, Result<T, String>>);
            let mut attempt: Option<Attempt<'_, T>> = None;
            let mut shown_step = String::new();
            let started = std::time::Instant::now();
            loop {
                // the step the attempt is at, with a spinner (as the Mac's progress indicator)
                if attempt.is_some() {
                    let p = crate::lifecycle::current();
                    if p.busy() {
                        const SPIN: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
                        let frame = (started.elapsed().as_millis() / 90) as usize % SPIN.len();
                        let t = format!("{}  {}", SPIN[frame], p.label());
                        if t != shown_step {
                            set_text(error_of(), &t);
                            shown_step = t;
                        }
                    }
                }
                // a connection attempt finished?
                if attempt.as_ref().is_some_and(|(_, h)| h.is_finished()) {
                    let ((typed, via), h) = attempt.take().unwrap();
                    match h.join().unwrap_or_else(|_| Err("internal error while connecting".into())) {
                        Ok(t) => {
                            remember_id(&typed);
                            remember_via(&via);
                            break Some(t);
                        }
                        Err(e) => {
                            set_text(error_of(), &e);
                            let _ = EnableWindow(button, true);
                            let _ = EnableWindow(edit_of(true), true);
                            let _ = EnableWindow(edit_of(false), true);
                            let _ = EnableWindow(relay_of(), true);
                            let _ = SetFocus(Some(edit_of(false)));
                        }
                    }
                }
                match STATE.with(|st| st.borrow_mut().as_mut().and_then(|s| s.done.take())) {
                    Some(None) => break None,
                    Some(Some((typed, pw, via))) if attempt.is_none() => {
                        let _ = EnableWindow(button, false);
                        let _ = EnableWindow(edit_of(true), false);
                        let _ = EnableWindow(edit_of(false), false);
                        let _ = EnableWindow(relay_of(), false);
                        let f = &try_connect;
                        let (t2, v2) = (typed.clone(), via.clone());
                        attempt = Some(((typed, via), scope.spawn(move || f(&t2, &pw, &v2))));
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
            let _ = DeleteObject(s.panel.into());
            let _ = DeleteObject(s.field.into());
        }
        let _ = DeleteObject(mono.into());
        result
    }
}

fn field_text(by_address: bool) -> &'static str {
    if by_address {
        "Mac's address — IP or name, e.g. 192.168.1.20 or mac.example.com:7471"
    } else {
        "Relay server (only for a Mac on another network)"
    }
}

/// The user switched between the ID and the address: the field shows the other value.
fn switch_via(by_address: bool) {
    let Some((field, label, other, was)) = STATE.with(|st| st.borrow().as_ref().map(|s| (s.relay, s.field_label, s.other.clone(), s.by_address))) else { return };
    if was == by_address {
        return;
    }
    let now = text_of(field);
    set_text(field, &other);
    set_text(label, field_text(by_address));
    STATE.with(|st| {
        if let Some(s) = st.borrow_mut().as_mut() {
            s.other = now;
            s.by_address = by_address;
        }
    });
    unsafe {
        if let Ok(p) = GetParent(field) {
            let _ = InvalidateRect(Some(p), None, false);
            for id in [VIA_ID, VIA_ADDRESS] {
                if let Ok(b) = GetDlgItem(Some(p), id) {
                    let _ = InvalidateRect(Some(b), None, false);
                }
            }
        }
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

fn relay_of() -> HWND {
    STATE.with(|st| st.borrow().as_ref().map(|s| s.relay)).unwrap_or_default()
}

fn edit_of(id: bool) -> HWND {
    STATE.with(|st| st.borrow().as_ref().map(|s| if id { s.id } else { s.pw })).unwrap_or_default()
}

/// The title bar in the window's tint (Windows 11), dark with dark mode.
fn frame_colours(hwnd: HWND, dark: bool) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    let t = crate::look::theme(dark);
    unsafe {
        let on: i32 = dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &on as *const _ as *const c_void, 4);
        let cap = colour(t.win[0]).0;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &cap as *const _ as *const c_void, 4);
        let txt = colour(t.text).0;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), &txt as *const _ as *const c_void, 4);
    }
}

/// Put a canvas on `hdc` at (x, y).
fn blit(hdc: HDC, c: &crate::paint::Canvas, x: i32, y: i32) {
    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: c.w as i32, biHeight: -(c.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
        ..Default::default()
    };
    unsafe {
        StretchDIBits(hdc, x, y, c.w as i32, c.h as i32, 0, 0, c.w as i32, c.h as i32, Some(c.px.as_ptr() as *const c_void), &bi, DIB_RGB_COLORS, SRCCOPY);
    }
}

/// The window behind its controls: the tint, the panel, the mark and title, the field labels,
/// the filled fields (an accent ring on the one with the focus).
fn paint(hwnd: HWND, hdc: HDC) {
    use crate::paint::Canvas;
    use crate::surface::{text, Align};
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let s = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let p = |v: i32| v as f32 * s;
    let Some((dark, ids)) = STATE.with(|st| st.borrow().as_ref().map(|st| (st.dark, [st.id, st.pw, st.relay]))) else { return };
    let t = crate::look::theme(dark);
    let mut c = Canvas::new(rc.right.max(1) as usize, rc.bottom.max(1) as usize);
    crate::look::tint(&mut c, &t);
    let (px0, py0, pw, ph) = PANEL;
    crate::look::panel(&mut c, p(px0), p(py0), p(pw), p(ph), 14.0 * s, s, &t);
    crate::look::mark(&mut c, p(FX), p(42), 40.0 * s, t.accent);
    text(&mut c, "Connect to your Mac", p(FX), p(94), p(FW), (22.0 * s).round() as i32, 600, t.text, Align::Left);
    text(&mut c, "Open MacBridge on the Mac, then type its ID and password.", p(FX), p(126), p(FW), (13.0 * s).round() as i32, 400, t.text2, Align::Left);
    for (label, y) in [("ID", ID_Y), ("Password", PW_Y)] {
        text(&mut c, label, p(FX), p(y - 20), p(FW), (12.0 * s).round() as i32, 600, t.text2, Align::Left);
    }
    let focus = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() };
    for (h, y) in ids.iter().zip([ID_Y, PW_Y, RELAY_Y]) {
        if *h == focus {
            c.stroke_round_rect_with(p(FX) - 2.0 * s, p(y) - 2.0 * s, p(FW) + 4.0 * s, p(FH) + 4.0 * s, 10.0 * s, 2.5 * s, |_, _| t.accent.alpha(0.6));
        }
        c.fill_round_rect(p(FX), p(y), p(FW), p(FH), 8.0 * s, t.field);
    }
    blit(hdc, &c, 0, 0);
}

/// The segmented control's halves and the Connect button.
fn draw_button(d: &DRAWITEMSTRUCT) {
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{text, Align};
    let Some((dark, by_address)) = STATE.with(|st| st.borrow().as_ref().map(|st| (st.dark, st.by_address))) else { return };
    let t = crate::look::theme(dark);
    let r = d.rcItem;
    let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
    let s = unsafe { GetDpiForWindow(d.hwndItem) }.max(96) as f32 / 96.0;
    let mut c = Canvas::filled(w as usize, h as usize, t.panel);
    let (fw, fh) = (w as f32, h as f32);
    let pressed = d.itemState.0 & ODS_SELECTED.0 != 0;
    let disabled = d.itemState.0 & ODS_DISABLED.0 != 0;
    let focused = d.itemState.0 & ODS_FOCUS.0 != 0;
    let px = |v: f32| (v * s).round() as i32;
    match d.CtlID as i32 {
        VIA_ID | VIA_ADDRESS => {
            // one half of a filled capsule; the chosen half a raised white pill
            let left = d.CtlID as i32 == VIA_ID;
            let rad = 9.0 * s;
            let (x0, ww) = if left { (0.0, fw + rad) } else { (-rad, fw + rad) };
            c.fill_round_rect(x0, 0.0, ww, fh, rad, t.field);
            let chosen = left != by_address;
            let label = if left { "Find it by its ID" } else { "Type its address" };
            if chosen {
                let (ix, iy, iw, ih) = (3.0 * s, 3.0 * s, fw - 6.0 * s, fh - 6.0 * s);
                c.shadow(ix, iy, iw, ih, 7.0 * s, 2.0 * s, 0.5 * s, Rgba::BLACK.alpha(if dark { 0.45 } else { 0.14 }));
                c.fill_round_rect(ix, iy, iw, ih, 7.0 * s, t.capsule);
            }
            if focused {
                c.stroke_round_rect_with(2.0 * s, 2.0 * s, fw - 4.0 * s, fh - 4.0 * s, 8.0 * s, 1.5 * s, |_, _| t.accent.alpha(0.7));
            }
            let (_, _, mh) = crate::surface::text_mask(label, px(13.0), if chosen { 600 } else { 500 }, w as usize);
            text(&mut c, label, 0.0, (fh - mh as f32) / 2.0, fw, px(13.0), if chosen { 600 } else { 500 }, if chosen { t.text } else { t.text2 }, Align::Center);
        }
        _ => {
            // Connect: an accent capsule (darker pressed, faded while connecting)
            let base = if pressed { t.accent.shade(-0.15) } else { t.accent };
            let col = if disabled { base.alpha(0.45) } else { base };
            if focused && !disabled {
                c.stroke_round_rect_with(0.5 * s, 0.5 * s, fw - s, fh - s, fh / 2.0, 2.0 * s, |_, _| t.accent.alpha(0.45));
            }
            c.fill_round_rect(2.5 * s, 2.5 * s, fw - 5.0 * s, fh - 5.0 * s, (fh - 5.0 * s) / 2.0, col);
            let (_, _, mh) = crate::surface::text_mask("Connect", px(14.0), 600, w as usize);
            text(&mut c, "Connect", 0.0, (fh - mh as f32) / 2.0, fw, px(14.0), 600, Rgba::WHITE, Align::Center);
        }
    }
    blit(d.hDC, &c, r.left, r.top);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_PRINTCLIENT => {
            paint(hwnd, HDC(wp.0 as *mut c_void));
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // the edits on their filled fields, the labels on the panel
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut c_void);
            let ctl = HWND(lp.0 as *mut c_void);
            STATE.with(|st| match st.borrow().as_ref() {
                Some(st) => {
                    let t = crate::look::theme(st.dark);
                    let edit = ctl == st.id || ctl == st.pw || ctl == st.relay;
                    SetBkColor(hdc, colour(if edit { t.field } else { t.panel }));
                    // the status line: the step under way in grey, a failure in red
                    let busy = crate::lifecycle::current().busy();
                    let fg = if ctl == st.error && !busy { t.fail } else if ctl == st.error || ctl == st.field_label { t.text2 } else { t.text };
                    SetTextColor(hdc, colour(fg));
                    LRESULT(if edit { st.field.0 as isize } else { st.panel.0 as isize })
                }
                None => DefWindowProcW(hwnd, msg, wp, lp),
            })
        }
        WM_DRAWITEM => {
            draw_button(&*(lp.0 as *const DRAWITEMSTRUCT));
            LRESULT(1)
        }
        // a field gets or loses the focus: its accent ring
        WM_COMMAND if matches!(((wp.0 >> 16) & 0xffff) as u32, EN_SETFOCUS | EN_KILLFOCUS) => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_COMMAND if matches!((wp.0 & 0xFFFF) as i32, VIA_ID | VIA_ADDRESS) => {
            switch_via((wp.0 & 0xFFFF) as i32 == VIA_ADDRESS);
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xFFFF) as i32 == IDOK.0 => {
            // read under a short borrow; every Win32 call below may re-enter this procedure
            let Some((id_h, pw_h, relay_h, err_h, by_address)) = STATE.with(|st| st.borrow().as_ref().map(|s| (s.id, s.pw, s.relay, s.error, s.by_address))) else { return LRESULT(0) };
            let (id, pw, relay) = (text_of(id_h), text_of(pw_h), text_of(relay_h));
            if by_address {
                if let Err(e) = rm_relay::lan::parse_address(&relay) {
                    set_text(err_h, &e);
                    let _ = SetFocus(Some(relay_h));
                    return LRESULT(0);
                }
            }
            let via = if by_address { Via::Address(relay) } else { Via::Id(relay) };
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
                    set_text(err_h, &crate::lifecycle::Phase::Connecting.label());
                    STATE.with(|st| {
                        if let Some(s) = st.borrow_mut().as_mut() {
                            s.done = Some(Some((id, pw, via)));
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
