//! Stage Manager, as on the Mac: the window in use stands in the middle and the windows of the
//! other apps rest at the left as pictures, newest first; a click on one brings it forward.
//! This module keeps the pictures and the strip that shows them; ui.rs decides what is on stage.
use std::cell::RefCell;
use std::sync::Mutex;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The strip's width at 96 dpi.
const STRIP: i32 = 176;
const KEY: COLORREF = COLORREF(0x00FF_00FF);

struct Item {
    hwnd: isize,
    bmp: HBITMAP,
    w: i32,
    h: i32,
}

#[derive(Default)]
struct Stage {
    items: Vec<Item>,
    strip: Option<HWND>,
    /// each picture's place in the strip (client px)
    spots: Vec<(isize, RECT)>,
}

thread_local! {
    static STAGE: RefCell<Stage> = RefCell::new(Stage::default());
}

/// What a click on a picture does (set once by ui.rs).
static PICK: Mutex<Option<fn(isize)>> = Mutex::new(None);

pub fn set_picker(f: fn(isize)) {
    *PICK.lock().unwrap() = Some(f);
}

fn px(v: i32) -> i32 {
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() } as i32;
    v * dpi.max(96) / 96
}

/// The work area of the main monitor, as the stage's: its left part is the strip.
pub fn work() -> RECT {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        info.rcWork
    }
}

/// The part of the work area where the window on stage stands.
pub fn stage_area() -> RECT {
    let mut r = work();
    r.left += px(STRIP);
    r
}

pub fn is_stashed(hwnd: HWND) -> bool {
    STAGE.with(|s| s.borrow().items.iter().any(|i| i.hwnd == hwnd.0 as isize))
}

pub fn stashed() -> Vec<isize> {
    STAGE.with(|s| s.borrow().items.iter().map(|i| i.hwnd).collect())
}

/// Take a picture of `hwnd`, hide it, and put the picture in the strip (at the top).
pub fn stash(hwnd: HWND) {
    unsafe {
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w < 40 || h < 40 || !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        // the window's own picture (it may be behind others); the screen's when that fails
        if !PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(2)).as_bool() {
            let _ = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY);
        }
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let _ = ShowWindow(hwnd, SW_HIDE);
        STAGE.with(|s| {
            let mut s = s.borrow_mut();
            if let Some(i) = s.items.iter().position(|i| i.hwnd == hwnd.0 as isize) {
                let gone = s.items.remove(i);
                let _ = DeleteObject(gone.bmp.into());
            }
            s.items.insert(0, Item { hwnd: hwnd.0 as isize, bmp, w, h });
        });
    }
    redraw();
}

/// `hwnd` is on stage (or gone): its picture leaves the strip.
pub fn unstash(hwnd: HWND) {
    let had = STAGE.with(|s| {
        let mut s = s.borrow_mut();
        match s.items.iter().position(|i| i.hwnd == hwnd.0 as isize) {
            Some(i) => {
                let gone = s.items.remove(i);
                unsafe {
                    let _ = DeleteObject(gone.bmp.into());
                }
                true
            }
            None => false,
        }
    });
    if had {
        redraw();
    }
}

/// Stage Manager off: the strip goes; the windows that were in it are returned (to be shown).
pub fn clear() -> Vec<isize> {
    let all = stashed();
    for h in &all {
        unstash(HWND(*h as *mut _));
    }
    all
}

fn ensure_strip() -> Option<HWND> {
    if let Some(h) = STAGE.with(|s| s.borrow().strip) {
        return Some(h);
    }
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("TaroStageStrip");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc); // (again: refused, and the class is there)
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, class, PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None).ok()?;
        STAGE.with(|s| s.borrow_mut().strip = Some(hwnd));
        Some(hwnd)
    }
}

/// The strip drawn again from its pictures (hidden when there are none).
fn redraw() {
    let empty = STAGE.with(|s| s.borrow().items.is_empty());
    if empty {
        if let Some(h) = STAGE.with(|s| s.borrow().strip) {
            unsafe {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
        STAGE.with(|s| s.borrow_mut().spots.clear());
        return;
    }
    let Some(strip) = ensure_strip() else { return };
    unsafe {
        let wk = work();
        let (sw, sh) = (px(STRIP), wk.bottom - wk.top);
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let canvas = CreateCompatibleBitmap(screen, sw, sh);
        let old = SelectObject(dc, canvas.into());
        let key = CreateSolidBrush(KEY);
        FillRect(dc, &RECT { left: 0, top: 0, right: sw, bottom: sh }, key);
        let _ = DeleteObject(key.into());
        let edge = CreateSolidBrush(COLORREF(0x0070_5a50)); // a thin violet-grey frame
        SetStretchBltMode(dc, HALFTONE);
        let _ = SetBrushOrgEx(dc, 0, 0, None);
        let (maxw, maxh) = (sw - px(36), (sh - px(40)) / 4);
        let mut y = px(28);
        let mut spots = vec![];
        STAGE.with(|s| {
            let s = s.borrow();
            let src = CreateCompatibleDC(Some(screen));
            for it in s.items.iter() {
                if y + px(40) > sh {
                    break;
                }
                let k = (maxw as f32 / it.w as f32).min(maxh as f32 / it.h as f32).min(1.0);
                let (tw, th) = (((it.w as f32 * k) as i32).max(8), ((it.h as f32 * k) as i32).max(8));
                let x = (sw - tw) / 2;
                let prev = SelectObject(src, it.bmp.into());
                FillRect(dc, &RECT { left: x - 2, top: y - 2, right: x + tw + 2, bottom: y + th + 2 }, edge);
                let _ = StretchBlt(dc, x, y, tw, th, Some(src), 0, 0, it.w, it.h, SRCCOPY);
                SelectObject(src, prev);
                spots.push((it.hwnd, RECT { left: x, top: y, right: x + tw, bottom: y + th }));
                y += th + px(26);
            }
            let _ = DeleteDC(src);
        });
        let _ = DeleteObject(edge.into());
        STAGE.with(|s| s.borrow_mut().spots = spots);
        let _ = UpdateLayeredWindow(strip, Some(screen), Some(&POINT { x: wk.left, y: wk.top }), Some(&SIZE { cx: sw, cy: sh }), Some(dc), Some(&POINT { x: 0, y: 0 }), KEY, None, ULW_COLORKEY);
        let _ = SetWindowPos(strip, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SelectObject(dc, old);
        let _ = DeleteObject(canvas.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
            let hit = STAGE.with(|s| s.borrow().spots.iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(h, _)| *h));
            if let (Some(h), Some(f)) = (hit, *PICK.lock().unwrap()) {
                f(h);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
