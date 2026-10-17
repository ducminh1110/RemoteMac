//! Minimising as the Mac does: the window's picture shrinks and fades toward the side of the
//! Dock (the bottom when there is none) while the real window goes to the taskbar. The picture
//! is a copy of what the screen shows of the window, in a see-through window of its own.
use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::dock::Edge;

const LENGTH: Duration = Duration::from_millis(260);

/// Where the shrinking window ends (screen px): the Dock's side of the work area, near its end.
fn target(work: RECT, edge: Option<Edge>) -> (f32, f32) {
    let low = (work.bottom - (work.bottom - work.top) / 12) as f32;
    match edge {
        Some(Edge::Left) => (work.left as f32, low),
        Some(Edge::Right) => (work.right as f32, low),
        None => ((work.left + work.right) as f32 / 2.0, work.bottom as f32),
    }
}

fn ease(t: f32) -> f32 {
    // slow at first, then quicker, as the Mac's
    t * t * (3.0 - 2.0 * t)
}

/// Minimise `frame` with the animation (plain minimise when it cannot be done).
pub fn minimize(frame: HWND, edge: Option<Edge>) {
    unsafe {
        let mut r = RECT::default();
        let (w, h) = {
            let _ = GetWindowRect(frame, &mut r);
            (r.right - r.left, r.bottom - r.top)
        };
        if w < 40 || h < 40 || !IsWindowVisible(frame).as_bool() || IsIconic(frame).as_bool() {
            let _ = ShowWindow(frame, SW_MINIMIZE);
            return;
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let copied = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY | CAPTUREBLT).is_ok();
        let ghost = if copied {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                w!("STATIC"),
                PCWSTR::null(),
                WS_POPUP,
                r.left,
                r.top,
                w,
                h,
                None,
                None,
                GetModuleHandleW(None).ok().map(|m| m.into()),
                None,
            )
            .ok()
        } else {
            None
        };
        let present = |x: i32, y: i32, dw: i32, dh: i32, alpha: u8| {
            let Some(g) = ghost else { return };
            let dc = CreateCompatibleDC(Some(screen));
            let b = CreateCompatibleBitmap(screen, dw, dh);
            let prev = SelectObject(dc, b.into());
            SetStretchBltMode(dc, HALFTONE);
            let _ = SetBrushOrgEx(dc, 0, 0, None);
            let _ = StretchBlt(dc, 0, 0, dw, dh, Some(mem), 0, 0, w, h, SRCCOPY);
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
            let _ = UpdateLayeredWindow(g, Some(screen), Some(&POINT { x, y }), Some(&SIZE { cx: dw, cy: dh }), Some(dc), Some(&POINT { x: 0, y: 0 }), COLORREF(0), Some(&blend), ULW_ALPHA);
            SelectObject(dc, prev);
            let _ = DeleteObject(b.into());
            let _ = DeleteDC(dc);
        };
        if let Some(g) = ghost {
            present(r.left, r.top, w, h, 255);
            let _ = ShowWindow(g, SW_SHOWNOACTIVATE);
        }
        // the window itself goes at once (without Windows' own animation: ours is on show)
        let off = BOOL(1);
        let _ = DwmSetWindowAttribute(frame, DWMWA_TRANSITIONS_FORCEDISABLED, &off as *const BOOL as *const c_void, std::mem::size_of::<BOOL>() as u32);
        let _ = ShowWindow(frame, SW_MINIMIZE);
        let on = BOOL(0);
        let _ = DwmSetWindowAttribute(frame, DWMWA_TRANSITIONS_FORCEDISABLED, &on as *const BOOL as *const c_void, std::mem::size_of::<BOOL>() as u32);
        if ghost.is_some() {
            let mon = MonitorFromWindow(frame, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut info);
            let (tx, ty) = target(info.rcWork, edge);
            let (cx, cy) = ((r.left + r.right) as f32 / 2.0, (r.top + r.bottom) as f32 / 2.0);
            let start = Instant::now();
            loop {
                let t = (start.elapsed().as_secs_f32() / LENGTH.as_secs_f32()).min(1.0);
                let e = ease(t);
                let k = 1.0 - 0.92 * e; // ends at a twelfth of its size
                let (dw, dh) = (((w as f32 * k) as i32).max(2), ((h as f32 * k) as i32).max(2));
                let (x, y) = (cx + (tx - cx) * e, cy + (ty - cy) * e);
                present((x - dw as f32 / 2.0) as i32, (y - dh as f32 / 2.0) as i32, dw, dh, (255.0 * (1.0 - 0.85 * e)) as u8);
                if t >= 1.0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        }
        if let Some(g) = ghost {
            let _ = DestroyWindow(g);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
}
