//! Surfaces: borderless layered windows MacBridge draws itself (paint.rs) — the app loading
//! window, glass menus, the search palette, the reconnect banner, the Dock. A surface keeps one
//! DIB the size of the window; a frame is painted into a `Canvas`, copied in (only the part
//! that changed, when told) and shown with per-pixel alpha by `UpdateLayeredWindowIndirect`.
//!
//! What is behind a glass surface is captured just before it shows (a still backdrop: glass
//! never re-renders continuously). Surfaces stay visible to screenshots and screen readers'
//! magnifiers like any window. Text is drawn by GDI into a grey coverage mask (no ClearType:
//! it has no alpha) and painted through it.

use crate::paint::{Canvas, Rgba};
use std::ffi::c_void;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Surface {
    pub hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: usize,
    pub h: usize,
}

impl Surface {
    /// A hidden layered popup of class `class` (registered by the caller), never activated by a
    /// click unless `activates`.
    pub fn new(hinst: HINSTANCE, class: PCWSTR, title: &str, owner: Option<HWND>, topmost: bool, activates: bool) -> Option<Surface> {
        unsafe {
            let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
            if topmost {
                ex |= WS_EX_TOPMOST;
            }
            if !activates {
                ex |= WS_EX_NOACTIVATE;
            }
            let hwnd = CreateWindowExW(ex, class, &HSTRING::from(title), WS_POPUP, 0, 0, 1, 1, owner, None, Some(hinst), None).ok()?;
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            Some(Surface { hwnd, dc, bmp: HBITMAP::default(), old: HGDIOBJ::default(), bits: std::ptr::null_mut(), w: 0, h: 0 })
        }
    }

    fn ensure(&mut self, w: usize, h: usize) -> bool {
        if w == self.w && h == self.h && !self.bits.is_null() {
            return true;
        }
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(bmp) = CreateDIBSection(Some(self.dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                self.bits = std::ptr::null_mut();
                return false;
            };
            self.old = SelectObject(self.dc, bmp.into());
            self.bmp = bmp;
            self.bits = bits as *mut u8;
            self.w = w;
            self.h = h;
        }
        true
    }

    /// Show `c` at (x, y) (screen pixels), fading the whole surface by `alpha` (255: as drawn).
    /// `dirty`: only this part of `c` changed (x, y, w, h); None: all of it.
    pub fn present(&mut self, c: &Canvas, x: i32, y: i32, alpha: u8, dirty: Option<(usize, usize, usize, usize)>) {
        if c.w == 0 || c.h == 0 || !self.ensure(c.w, c.h) {
            return;
        }
        let (dx, dy, dw, dh) = dirty.map(|(a, b, w, h)| (a.min(c.w), b.min(c.h), w, h)).unwrap_or((0, 0, c.w, c.h));
        let (dw, dh) = (dw.min(c.w - dx), dh.min(c.h - dy));
        unsafe {
            let _ = GdiFlush();
            for row in dy..dy + dh {
                let src = &c.px[row * c.w + dx..row * c.w + dx + dw];
                std::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, self.bits.add((row * c.w + dx) * 4), dw * 4);
            }
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
            let pos = POINT { x, y };
            let size = SIZE { cx: c.w as i32, cy: c.h as i32 };
            let src = POINT::default();
            let rect = RECT { left: dx as i32, top: dy as i32, right: (dx + dw) as i32, bottom: (dy + dh) as i32 };
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: HDC::default(),
                pptDst: &pos,
                psize: &size,
                hdcSrc: self.dc,
                pptSrc: &src,
                crKey: COLORREF(0),
                pblend: &blend,
                dwFlags: ULW_ALPHA,
                prcDirty: if dirty.is_some() { &rect } else { std::ptr::null() },
            };
            if !UpdateLayeredWindowIndirect(self.hwnd, &info).as_bool() && dirty.is_some() {
                // a dirty rectangle is refused when the size or position changed: all of it
                let full = UPDATELAYEREDWINDOWINFO { prcDirty: std::ptr::null(), ..info };
                let _ = UpdateLayeredWindowIndirect(self.hwnd, &full);
            }
        }
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.bmp.is_invalid() {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            let _ = DeleteDC(self.dc);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// What the screen shows at (x, y, w, h) (screen pixels): the backdrop of a glass surface,
/// taken before the surface shows. None when the screen cannot be read (a secure desktop).
pub fn capture(x: i32, y: i32, w: usize, h: usize) -> Option<Canvas> {
    if w == 0 || h == 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => {
                let old = SelectObject(dc, bmp.into());
                let ok = BitBlt(dc, 0, 0, w as i32, h as i32, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
                let _ = GdiFlush();
                let r = ok.then(|| {
                    let raw = std::slice::from_raw_parts(bits as *const [u8; 4], w * h);
                    Canvas { w, h, px: raw.iter().map(|p| [p[0], p[1], p[2], 255]).collect() }
                });
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                r
            }
            Err(_) => None,
        };
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Text as a coverage mask: (mask, width, height) at `px` pixels high, `weight` (400, 600…),
/// at most `max_w` wide (cut with an ellipsis), one line.
pub fn text_mask(text: &str, px: i32, weight: i32, max_w: usize) -> (Vec<u8>, usize, usize) {
    unsafe {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        ReleaseDC(None, screen);
        let font = CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, 0, &HSTRING::from(crate::native::ui_face(weight)));
        let oldf = SelectObject(dc, font.into());
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut r = RECT { left: 0, top: 0, right: max_w as i32, bottom: px * 2 };
        DrawTextW(dc, &mut wide, &mut r, DT_SINGLELINE | DT_NOPREFIX | DT_CALCRECT | DT_END_ELLIPSIS);
        let (w, h) = ((r.right.clamp(1, max_w.max(1) as i32)) as usize, (r.bottom.max(1)) as usize);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w as i32, biHeight: -(h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let mut mask = vec![0u8; w * h];
        if let Ok(bmp) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) {
            let oldb = SelectObject(dc, bmp.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, COLORREF(0x00FF_FFFF));
            let mut r = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
            let mut wide: Vec<u16> = text.encode_utf16().collect();
            DrawTextW(dc, &mut wide, &mut r, DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
            let _ = GdiFlush();
            let raw = std::slice::from_raw_parts(bits as *const [u8; 4], w * h);
            for (m, p) in mask.iter_mut().zip(raw) {
                *m = p[1];
            }
            SelectObject(dc, oldb);
            let _ = DeleteObject(bmp.into());
        }
        SelectObject(dc, oldf);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(dc);
        (mask, w, h)
    }
}

/// A line of text painted in `color` inside (x, w) at top `y`, aligned.
#[allow(clippy::too_many_arguments)]
pub fn text(c: &mut Canvas, s: &str, x: f32, y: f32, w: f32, px: i32, weight: i32, color: Rgba, align: Align) {
    let (mask, mw, _) = text_mask(s, px, weight, w.max(1.0) as usize);
    let left = match align {
        Align::Left => x,
        Align::Center => x + (w - mw as f32) / 2.0,
        Align::Right => x + w - mw as f32,
    };
    c.fill_mask(&mask, mw, left.round() as isize, y.round() as isize, color);
}

/// Windows uses dark mode for apps.
pub fn dark_mode() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut v: u32 = 1;
    let mut n = 4u32;
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), &HSTRING::from("AppsUseLightTheme"), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut c_void), Some(&mut n))
    };
    r.is_ok() && v == 0
}

/// The user's accent colour (Windows settings), else system blue.
pub fn accent() -> Rgba {
    let mut c = 0u32;
    let mut opaque = windows::core::BOOL(0);
    if unsafe { windows::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut c, &mut opaque) }.is_ok() && c != 0 {
        return Rgba::rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
    }
    Rgba::rgb(10, 132, 255)
}

/// The display scale of the monitor at a screen point.
pub fn scale_at(x: i32, y: i32) -> f32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        dx.max(96) as f32 / 96.0
    }
}

/// The work area of the monitor at a screen point.
pub fn work_area_at(x: i32, y: i32) -> RECT {
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        info.rcWork
    }
}

/// Register a window class for surfaces with `proc`.
pub fn register(hinst: HINSTANCE, class: PCWSTR, proc: WNDPROC, cursor: PCWSTR) {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: proc, hInstance: hinst, lpszClassName: class, hCursor: LoadCursorW(None, cursor).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
    }
}
