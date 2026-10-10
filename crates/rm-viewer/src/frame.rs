//! MacBridge's own windows drawn as macOS draws a window: no Windows title bar, the window's
//! own toolbar at the top with the three window buttons in it. The window stays a normal
//! resizable Windows window underneath (snapping, Win+arrows, the taskbar, the shadow and
//! rounded corners of Windows 11): only the title bar is taken away, and its jobs (moving the
//! window, resizing it at its edges, double-clicking to zoom) are answered here.

use crate::look::Light;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Take the title bar away (call once the window exists): the frame stays for its shadow.
pub fn adopt(hwnd: HWND) {
    unsafe {
        let m = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
        let round: u32 = 2; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const std::ffi::c_void, 4);
        // the frame changed: Windows asks for the client area again
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// The light or dark frame (Windows 11 tints the window border with it).
pub fn appearance(hwnd: HWND, dark: bool) {
    unsafe {
        let on: i32 = dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &on as *const _ as *const std::ffi::c_void, 4);
    }
}

fn frame_px(hwnd: HWND) -> i32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CXFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

/// WM_NCCALCSIZE: the whole window is the client area (inside the screen when maximized).
pub fn calc_size(hwnd: HWND, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if wp.0 != 0 && IsZoomed(hwnd).as_bool() {
            let p = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
            let f = frame_px(hwnd);
            p.rgrc[0].left += f;
            p.rgrc[0].top += f;
            p.rgrc[0].right -= f;
            p.rgrc[0].bottom -= f;
        }
        LRESULT(0)
    }
}

/// WM_NCHITTEST at the screen point in `lp`: the resize edges (when `resizable`), the window's
/// draggable parts (`caption(x, y)` in client pixels), else the client.
pub fn hit_test(hwnd: HWND, lp: LPARAM, resizable: bool, caption: impl Fn(f32, f32) -> bool) -> LRESULT {
    unsafe {
        let (sx, sy) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (x, y) = (sx - r.left, sy - r.top);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if resizable && !IsZoomed(hwnd).as_bool() {
            let b = (frame_px(hwnd) * 3 / 4).max(4);
            let (l, rt, t, bt) = (x < b, x >= w - b, y < b, y >= h - b);
            let code = match (l, rt, t, bt) {
                (true, _, true, _) => HTTOPLEFT,
                (_, true, true, _) => HTTOPRIGHT,
                (true, _, _, true) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, ..) => HTLEFT,
                (_, true, ..) => HTRIGHT,
                (_, _, true, _) => HTTOP,
                (.., true) => HTBOTTOM,
                _ => HTNOWHERE,
            };
            if code != HTNOWHERE {
                return LRESULT(code as isize);
            }
        }
        let mut pt = POINT { x: sx, y: sy };
        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
        LRESULT(if caption(pt.x as f32, pt.y as f32) { HTCAPTION } else { HTCLIENT } as isize)
    }
}

/// What a window button does here: red closes, yellow minimizes, green zooms (maximizes, or
/// back).
pub fn press(hwnd: HWND, l: Light) {
    unsafe {
        match l {
            Light::Close => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            Light::Minimize => {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            Light::Zoom => {
                let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
            }
        }
    }
}

pub fn maximized(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
}

/// WM_GETMINMAXINFO: no smaller than `w` x `h` DIPs.
pub fn min_size(hwnd: HWND, lp: LPARAM, w: f32, h: f32) {
    unsafe {
        let s = GetDpiForWindow(hwnd) as f32 / 96.0;
        let mm = &mut *(lp.0 as *mut MINMAXINFO);
        mm.ptMinTrackSize = POINT { x: (w * s) as i32, y: (h * s) as i32 };
    }
}
