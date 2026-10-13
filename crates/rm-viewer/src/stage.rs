//! Stage Manager, as on the Mac: the window in use stands in the middle and the windows of the
//! other apps rest at the left as live cards, newest first; a click on one brings it forward.
//!
//! The way of doing it follows StageManagerForWindows (MIT, Andreas Wäscher and depoledna): a
//! window that leaves the stage is not hidden but parked past the edge of the screens, so that
//! DWM goes on composing it and its card stays live; the card is a DWM thumbnail. The strip slides
//! down out of the way while the Mac's Dock is out (`lower`), and comes back after.
//! ui.rs decides what is on stage; this module keeps the parked windows and the strip.
use std::cell::RefCell;
use std::sync::Mutex;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The strip's width at 96 dpi.
const STRIP: i32 = 176;
const KEY: COLORREF = COLORREF(0x00FF_00FF);
const TIMER: usize = 1;
const SLIDE_MS: f32 = 260.0;

struct Item {
    hwnd: isize,
    /// where the window stood before it was parked
    home: RECT,
    thumb: isize,
    w: i32,
    h: i32,
}

#[derive(Default)]
struct Stage {
    items: Vec<Item>,
    strip: Option<HWND>,
    /// each card's place in the strip (client px)
    spots: Vec<(isize, RECT)>,
    /// the strip's slide: 0 in place, 1 down out of the way; where it is, where it goes, since when
    drop: f32,
    to: f32,
    from: f32,
    since: Option<std::time::Instant>,
}

thread_local! {
    static STAGE: RefCell<Stage> = RefCell::new(Stage::default());
}

/// What a click on a card does (set once by ui.rs).
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

/// Park `hwnd` past the edge of the screens (still composed, so its card is live) and put its
/// card at the top of the strip.
pub fn stash(hwnd: HWND) {
    if is_stashed(hwnd) {
        return;
    }
    unsafe {
        let mut r = RECT::default();
        let _ = GetWindowRect(hwnd, &mut r);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w < 40 || h < 40 || !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        let Some(strip) = ensure_strip() else { return };
        let Ok(thumb) = DwmRegisterThumbnail(strip, hwnd) else { return };
        let away = GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) + 64;
        let _ = SetWindowPos(hwnd, None, away, r.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        STAGE.with(|s| s.borrow_mut().items.insert(0, Item { hwnd: hwnd.0 as isize, home: r, thumb, w, h }));
    }
    layout();
}

/// `hwnd` is on stage (or gone): its card leaves the strip and the window goes back where it was.
pub fn unstash(hwnd: HWND) {
    let gone = STAGE.with(|s| {
        let mut s = s.borrow_mut();
        s.items.iter().position(|i| i.hwnd == hwnd.0 as isize).map(|i| s.items.remove(i))
    });
    if let Some(it) = gone {
        unsafe {
            let _ = DwmUnregisterThumbnail(it.thumb);
            let _ = SetWindowPos(hwnd, None, it.home.left, it.home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        layout();
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

/// The strip slides down out of the way (`on`: the Mac's Dock is out), or back up.
pub fn lower(on: bool) {
    let Some(strip) = STAGE.with(|s| {
        let mut s = s.borrow_mut();
        let to = if on { 1.0 } else { 0.0 };
        if s.to == to {
            return None;
        }
        s.from = s.drop;
        s.to = to;
        s.since = Some(std::time::Instant::now());
        s.strip
    }) else {
        return;
    };
    unsafe {
        SetTimer(Some(strip), TIMER, 15, None);
    }
}

fn ensure_strip() -> Option<HWND> {
    if let Some(h) = STAGE.with(|s| s.borrow().strip) {
        return Some(h);
    }
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let class = w!("TaroStageStrip");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), hbrBackground: CreateSolidBrush(KEY), ..Default::default() };
        RegisterClassW(&wc); // (again: refused, and the class is there)
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, class, PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None).ok()?;
        let _ = SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY);
        STAGE.with(|s| s.borrow_mut().strip = Some(hwnd));
        Some(hwnd)
    }
}

/// Where the strip is now (it slides by `drop` of its height).
fn place(strip: HWND, drop: f32) {
    let wk = work();
    let h = wk.bottom - wk.top;
    unsafe {
        let _ = SetWindowPos(strip, Some(HWND_TOPMOST), wk.left, wk.top + (h as f32 * drop) as i32, px(STRIP), h, SWP_NOACTIVATE);
    }
}

/// The cards put in their places: each card's thumbnail, the strip shown (hidden with none).
fn layout() {
    let Some(strip) = STAGE.with(|s| s.borrow().strip) else { return };
    if STAGE.with(|s| s.borrow().items.is_empty()) {
        unsafe {
            let _ = ShowWindow(strip, SW_HIDE);
        }
        STAGE.with(|s| s.borrow_mut().spots.clear());
        return;
    }
    let wk = work();
    let (sw, sh) = (px(STRIP), wk.bottom - wk.top);
    let (maxw, maxh) = (sw - px(40), (sh - px(40)) / 4);
    let mut y = px(28);
    let mut spots = vec![];
    STAGE.with(|s| {
        for it in s.borrow().items.iter() {
            if y + px(40) > sh {
                // no room: not shown
                let none = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_VISIBLE, fVisible: BOOL(0), ..Default::default() };
                unsafe {
                    let _ = DwmUpdateThumbnailProperties(it.thumb, &none);
                }
                continue;
            }
            let k = (maxw as f32 / it.w as f32).min(maxh as f32 / it.h as f32).min(1.0);
            let (tw, th) = (((it.w as f32 * k) as i32).max(8), ((it.h as f32 * k) as i32).max(8));
            let x = (sw - tw) / 2;
            let rc = RECT { left: x, top: y, right: x + tw, bottom: y + th };
            let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY, rcDestination: rc, opacity: 255, fVisible: BOOL(1), ..Default::default() };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(it.thumb, &props);
            }
            spots.push((it.hwnd, rc));
            y += th + px(26);
        }
    });
    STAGE.with(|s| s.borrow_mut().spots = spots);
    let drop = STAGE.with(|s| s.borrow().drop);
    place(strip, drop);
    unsafe {
        let _ = ShowWindow(strip, SW_SHOWNOACTIVATE);
        let _ = InvalidateRect(Some(strip), None, true);
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let card = CreateSolidBrush(COLORREF(0x002c_2622));
        let old = SelectObject(dc, card.into());
        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let pad = px(5);
        STAGE.with(|s| {
            for (_, r) in s.borrow().spots.iter() {
                let _ = RoundRect(dc, r.left - pad, r.top - pad, r.right + pad, r.bottom + pad, px(14), px(14));
            }
        });
        SelectObject(dc, pen);
        SelectObject(dc, old);
        let _ = DeleteObject(card.into());
        let _ = EndPaint(hwnd, &ps);
    }
}

fn slide(hwnd: HWND) {
    let (drop, done) = STAGE.with(|s| {
        let mut s = s.borrow_mut();
        let t = s.since.map_or(1.0, |t| (t.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS).min(1.0));
        let e = t * t * (3.0 - 2.0 * t);
        s.drop = s.from + (s.to - s.from) * e;
        (s.drop, t >= 1.0)
    });
    place(hwnd, drop);
    if done {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => DefWindowProcW(hwnd, msg, wp, lp),
        WM_TIMER if wp.0 == TIMER => {
            slide(hwnd);
            LRESULT(0)
        }
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
