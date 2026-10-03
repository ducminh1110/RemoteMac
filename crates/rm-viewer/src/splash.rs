//! The "opening an app" card: when a Mac app is launched from Windows a small window shows the
//! app's icon and name, what is happening right now and how far along it is, until the app's
//! first picture is on screen (or the Mac reports an error).
//!
//!  1 sending the launch to the Mac   2 the app is starting on the Mac
//!  3 its window is being set up       4 the stream is connecting (first picture)

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmSplash");
pub const STEPS: u32 = 4;
const TIMER: usize = 1;
const W: i32 = 380;
const H: i32 = 132;

struct Card {
    hwnd: HWND,
    name: String,
    icon: Option<HICON>,
    step: u32,
    text: String,
    error: bool,
    since: Instant,
    close_at: Option<Instant>,
}

thread_local! {
    static CARDS: RefCell<HashMap<String, Card>> = RefCell::new(HashMap::new());
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

fn font(px: i32, weight: i32) -> HFONT {
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, &HSTRING::from(crate::native::ui_face(weight))) }
}

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_APPSTARTING).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
    }
}

fn step_text(step: u32, name: &str) -> String {
    match step {
        1 => "Sending the launch to the Mac…".into(),
        2 => format!("Starting {name} on the Mac…"),
        3 => "Setting up the window…".into(),
        _ => "Connecting the stream…".into(),
    }
}

/// Show the card for `app` (step 1).
pub fn show(hinst: HINSTANCE, app: &str, name: &str, icon: Option<HICON>) {
    if CARDS.with(|c| c.borrow().contains_key(app)) {
        return;
    }
    unsafe {
        let scale = crate::native::dpi_scale(HWND::default()).max(1.0);
        let (w, h) = ((W as f64 * scale) as i32, (H as f64 * scale) as i32);
        let mut wa = RECT::default();
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut wa as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        let (x, y) = (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2);
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, CLASS, &HSTRING::from(format!("Opening {name}")), WS_POPUP, x, y, w, h, None, None, Some(hinst), None) else { return };
        crate::native::round_corners(hwnd);
        CARDS.with(|c| {
            c.borrow_mut().insert(app.to_string(), Card { hwnd, name: name.to_string(), icon, step: 1, text: step_text(1, name), error: false, since: Instant::now(), close_at: None })
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER, 30, None);
    }
}

/// Move `app`'s card on to `step` (never back).
pub fn step(app: &str, step: u32) {
    CARDS.with(|c| {
        if let Some(card) = c.borrow_mut().get_mut(app) {
            if step > card.step && !card.error {
                card.step = step;
                card.text = step_text(step, &card.name);
                unsafe { let _ = InvalidateRect(Some(card.hwnd), None, false); }
            }
        }
    });
}

pub fn showing(app: &str) -> bool {
    CARDS.with(|c| c.borrow().contains_key(app))
}

/// The app's first picture is on screen: fill the bar and close.
pub fn done(app: &str) {
    CARDS.with(|c| {
        if let Some(card) = c.borrow_mut().get_mut(app) {
            card.step = STEPS + 1;
            card.text = "Ready".into();
            card.close_at = Some(Instant::now() + Duration::from_millis(250));
            unsafe { let _ = InvalidateRect(Some(card.hwnd), None, false); }
        }
    });
}

/// Launching failed: say why, then close.
pub fn fail(app: &str, why: &str) {
    CARDS.with(|c| {
        if let Some(card) = c.borrow_mut().get_mut(app) {
            card.error = true;
            card.text = why.to_string();
            card.close_at = Some(Instant::now() + Duration::from_secs(4));
            unsafe { let _ = InvalidateRect(Some(card.hwnd), None, false); }
        }
    });
}

fn close(hwnd: HWND) {
    CARDS.with(|c| c.borrow_mut().retain(|_, card| card.hwnd != hwnd));
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER);
        let _ = DestroyWindow(hwnd);
    }
}

fn paint(hwnd: HWND) {
    let snapshot = CARDS.with(|c| c.borrow().values().find(|k| k.hwnd == hwnd).map(|k| (k.name.clone(), k.icon, k.step, k.text.clone(), k.error, k.since)));
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let (w, h) = (rc.right, rc.bottom);
        // double buffered
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp.into());
        let s = |v: i32| (v as f64 * crate::native::dpi_scale(hwnd).max(1.0)) as i32;
        let bg = CreateSolidBrush(rgb(250, 250, 251));
        FillRect(mem, &rc, bg);
        let _ = DeleteObject(bg.into());
        if let Some((name, icon, step, text, error, since)) = snapshot {
            if let Some(i) = icon {
                let _ = DrawIconEx(mem, s(20), s(22), i, s(56), s(56), 0, None, DI_NORMAL);
            }
            SetBkMode(mem, TRANSPARENT);
            let title = font(s(18), 600);
            let small = font(s(13), 400);
            let o = SelectObject(mem, title.into());
            SetTextColor(mem, rgb(28, 28, 30));
            let mut r = RECT { left: s(92), top: s(24), right: w - s(18), bottom: s(50) };
            let mut t: Vec<u16> = name.encode_utf16().collect();
            DrawTextW(mem, &mut t, &mut r, DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(mem, small.into());
            SetTextColor(mem, if error { rgb(200, 40, 40) } else { rgb(95, 95, 100) });
            let shown = if error { text } else { format!("{text}   ({}/{STEPS})", step.min(STEPS)) };
            let mut r = RECT { left: s(92), top: s(54), right: w - s(18), bottom: s(76) };
            let mut t: Vec<u16> = shown.encode_utf16().collect();
            DrawTextW(mem, &mut t, &mut r, DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
            // progress: finished steps solid, the current one filling slowly (it has no real
            // percentage of its own), and a light sweep so it reads as alive
            let bar = RECT { left: s(92), top: s(88), right: w - s(18), bottom: s(94) };
            let track = CreateSolidBrush(rgb(228, 228, 232));
            FillRect(mem, &bar, track);
            let _ = DeleteObject(track.into());
            let width = (bar.right - bar.left) as f64;
            let secs = since.elapsed().as_secs_f64();
            let within = 1.0 - (-secs / 3.0).exp(); // eases toward the step's end, never reaches it
            let frac = if step > STEPS { 1.0 } else { ((step - 1) as f64 + 0.85 * within) / STEPS as f64 };
            let fill = RECT { right: bar.left + (width * frac.clamp(0.0, 1.0)) as i32, ..bar };
            let col = CreateSolidBrush(if error { rgb(200, 40, 40) } else { rgb(10, 132, 255) });
            FillRect(mem, &fill, col);
            let _ = DeleteObject(col.into());
            if !error && step <= STEPS {
                let pos = ((secs * 0.8).fract() * width) as i32;
                let sweep = RECT { left: (bar.left + pos).min(fill.right), right: (bar.left + pos + s(40)).min(fill.right), ..bar };
                let light = CreateSolidBrush(rgb(110, 180, 255));
                FillRect(mem, &sweep, light);
                let _ = DeleteObject(light.into());
            }
            SelectObject(mem, o);
            let _ = DeleteObject(title.into());
            let _ = DeleteObject(small.into());
        }
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER => {
            let close_now = CARDS.with(|c| c.borrow().values().find(|k| k.hwnd == hwnd).map(|k| k.close_at.is_some_and(|t| Instant::now() >= t) || k.since.elapsed() > Duration::from_secs(90)));
            match close_now {
                Some(true) | None => close(hwnd),
                Some(false) => {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        // a click dismisses it (the app keeps opening)
        WM_LBUTTONUP => {
            close(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
