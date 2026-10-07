//! The Mac Desktop's navigation ball: in fullscreen a small floating disc stays over the picture
//! instead of a bar that slides in at the top edge (which got in the way of the Mac's own menu
//! bar there). Drag it anywhere (it settles at the nearest side, as a phone's assistive button
//! does); click it for the menu. It is an owned layered window: above its fullscreen window,
//! never taking the focus from it, hidden with it.

use crate::chrome;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: windows::core::PCWSTR = w!("RmNavBall");
/// Diameter in DIPs.
const SIZE: f64 = 46.0;
/// How far the pointer moves before a press is a drag, not a click (px).
const DRAG: i32 = 5;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Opens the ball's menu for its window at a screen point.
pub type OnClick = fn(HWND, POINT);

struct Ball {
    frame: HWND,
    size: i32,
    lit: bool,
    /// pointer pressed at (screen), window there at
    press: Option<(POINT, POINT)>,
    dragged: bool,
}

thread_local! {
    static BALLS: RefCell<HashMap<isize, Ball>> = RefCell::new(HashMap::new());
    /// what the ball's click does (set by the UI): opens its menu at the given screen point
    static ON_CLICK: std::cell::Cell<Option<OnClick>> = const { std::cell::Cell::new(None) };
}

/// Where the ball sits on its monitor (parts of the width and height, centre), kept while the
/// viewer runs: the ball comes back where it was left.
static PLACE: std::sync::Mutex<(f64, f64)> = std::sync::Mutex::new((1.0, 0.28));

pub fn register(hinst: HINSTANCE, on_click: OnClick) {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_HAND).ok().unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
    }
    ON_CLICK.with(|c| c.set(Some(on_click)));
}

fn ball_of(frame: HWND) -> Option<HWND> {
    BALLS.with(|b| b.borrow().iter().find(|(_, v)| v.frame == frame).map(|(k, _)| HWND(*k as *mut c_void)))
}

fn monitor_of(hwnd: HWND) -> RECT {
    unsafe {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
        info.rcMonitor
    }
}

/// Show the ball over `frame` (fullscreen), where it was last left.
pub fn show(frame: HWND) {
    if ball_of(frame).is_some() {
        return;
    }
    unsafe {
        let hinst: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map(Into::into).unwrap_or_default();
        let size = (SIZE * GetDpiForWindow(frame).max(96) as f64 / 96.0).round() as i32;
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            CLASS,
            w!("Mac Desktop"),
            WS_POPUP,
            0,
            0,
            size,
            size,
            Some(frame),
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };
        BALLS.with(|b| b.borrow_mut().insert(hwnd.0 as isize, Ball { frame, size, lit: false, press: None, dragged: false }));
        let m = monitor_of(frame);
        let (fx, fy) = *PLACE.lock().unwrap();
        let at = place(m, size, fx, fy);
        paint(hwnd, Some(at));
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

pub fn hide(frame: HWND) {
    if let Some(b) = ball_of(frame) {
        unsafe {
            let _ = DestroyWindow(b);
        }
    }
}

/// Top-left for a ball of `size` px whose centre is at parts (`fx`, `fy`) of monitor `m`,
/// kept a little inside its edges.
fn place(m: RECT, size: i32, fx: f64, fy: f64) -> POINT {
    let margin = size / 4;
    let x = m.left + ((m.right - m.left) as f64 * fx).round() as i32 - size / 2;
    let y = m.top + ((m.bottom - m.top) as f64 * fy).round() as i32 - size / 2;
    POINT { x: x.clamp(m.left + margin, m.right - size - margin), y: y.clamp(m.top + margin, m.bottom - size - margin) }
}

/// Draw the ball (premultiplied alpha), and move it to `at` when given.
fn paint(hwnd: HWND, at: Option<POINT>) {
    let Some((size, lit)) = BALLS.with(|b| b.borrow().get(&(hwnd.0 as isize)).map(|v| (v.size, v.lit))) else { return };
    let px = chrome::nav_ball(size as usize, lit);
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u8, px.len());
            let old = SelectObject(mem, bmp.into());
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let sz = SIZE { cx: size, cy: size };
            let src = POINT::default();
            let _ = UpdateLayeredWindow(hwnd, Some(screen), at.as_ref().map(|p| p as *const POINT), Some(&sz), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}

fn set_lit(hwnd: HWND, lit: bool) {
    let changed = BALLS.with(|b| b.borrow_mut().get_mut(&(hwnd.0 as isize)).map(|v| std::mem::replace(&mut v.lit, lit) != lit)).unwrap_or(false);
    if changed {
        paint(hwnd, None);
    }
}

fn window_pos(hwnd: HWND) -> POINT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    POINT { x: r.left, y: r.top }
}

/// After a drag: to the nearest left or right side, at the height it was left, and remember it.
fn settle(hwnd: HWND) {
    let Some(size) = BALLS.with(|b| b.borrow().get(&(hwnd.0 as isize)).map(|v| v.size)) else { return };
    let m = monitor_of(hwnd);
    let p = window_pos(hwnd);
    let (w, h) = ((m.right - m.left).max(1) as f64, (m.bottom - m.top).max(1) as f64);
    let cx = (p.x + size / 2 - m.left) as f64 / w;
    let fy = ((p.y + size / 2 - m.top) as f64 / h).clamp(0.0, 1.0);
    let fx = if cx < 0.5 { 0.0 } else { 1.0 };
    *PLACE.lock().unwrap() = (fx, fy);
    let to = place(m, size, fx, fy);
    // a short glide to the side
    const STEPS: i32 = 8;
    for i in 1..=STEPS {
        let t = chrome::ease_out(i as f64 / STEPS as f64);
        let x = p.x + ((to.x - p.x) as f64 * t).round() as i32;
        let y = p.y + ((to.y - p.y) as f64 * t).round() as i32;
        unsafe {
            let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        std::thread::sleep(std::time::Duration::from_millis(12));
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let key = hwnd.0 as isize;
    let cursor = || {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        p
    };
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            set_lit(hwnd, true);
            let now = cursor();
            let to = BALLS.with(|b| {
                let mut b = b.borrow_mut();
                let v = b.get_mut(&key)?;
                let (from, start) = v.press?;
                let (dx, dy) = (now.x - from.x, now.y - from.y);
                if !v.dragged && dx.abs().max(dy.abs()) < DRAG {
                    return None;
                }
                v.dragged = true;
                Some(POINT { x: start.x + dx, y: start.y + dy })
            });
            if let Some(p) = to {
                let _ = SetWindowPos(hwnd, None, p.x, p.y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            set_lit(hwnd, false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (now, at) = (cursor(), window_pos(hwnd));
            BALLS.with(|b| b.borrow_mut().get_mut(&key).map(|v| { v.press = Some((now, at)); v.dragged = false }));
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let dragged = BALLS.with(|b| b.borrow_mut().get_mut(&key).map(|v| { v.press = None; v.dragged })).unwrap_or(false);
            if dragged {
                settle(hwnd);
            } else {
                click(hwnd);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            click(hwnd);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            BALLS.with(|b| b.borrow_mut().get_mut(&key).map(|v| v.press = None));
            LRESULT(0)
        }
        WM_DESTROY => {
            BALLS.with(|b| b.borrow_mut().remove(&key));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// The menu opens beside the ball, on the side away from the screen edge.
fn click(hwnd: HWND) {
    let Some(frame) = BALLS.with(|b| b.borrow().get(&(hwnd.0 as isize)).map(|v| v.frame)) else { return };
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    let m = monitor_of(hwnd);
    let left_side = (r.left + r.right) / 2 < (m.left + m.right) / 2;
    let at = POINT { x: if left_side { r.right + 6 } else { r.left - 6 }, y: r.top };
    set_lit(hwnd, true);
    if let Some(f) = ON_CLICK.with(|c| c.get()) {
        f(frame, at);
    }
    if ball_of(frame) == Some(hwnd) {
        set_lit(hwnd, false);
    }
}

/// Whether the menu should open to the left of `at` (the ball is on the right side).
pub fn opens_left(frame: HWND, at: POINT) -> bool {
    let m = monitor_of(frame);
    at.x > (m.left + m.right) / 2
}
