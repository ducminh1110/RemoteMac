//! The Settings window (launcher button, or Ctrl+Alt+Shift+P in any remote window), after
//! System Settings on macOS 26 (settingsui.rs draws it and says what clicks and keys do; this
//! puts it in a window with no Windows title bar, frame.rs). A change applies at once.

use crate::settings::{Settings, WORKSPACES};
use crate::settingsui::{Act, SettingsView};
use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmSettings");
const TIMER: usize = 7;
const WM_MOUSE_LEAVE: u32 = 0x02A3;

struct Ui {
    hwnd: HWND,
    view: SettingsView,
    on_save: Rc<dyn Fn(Settings)>,
    timer: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// The window's state (None while it is in use further up: a message sent from inside).
fn with<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|u| u.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn register(hinst: HINSTANCE) {
    unsafe {
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst, lpszClassName: CLASS, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
    }
}

/// The Mac screen sizes offered: this PC's screen at each step of "more space".
fn workspaces() -> Vec<String> {
    let (spx, sds) = (crate::net::screen_px(), crate::net::display_scale());
    (0..WORKSPACES as u8)
        .map(|k| {
            let (w, h) = Settings::points_at(if spx.0 > 0 { spx } else { (1920, 1080) }, sds, k);
            let what = ["as large as this screen", "more space", "even more space", "most space", "pixel for pixel: sharpest, smallest text"][k as usize];
            format!("{w} × {h} ({what})")
        })
        .collect()
}

/// Open the window (or bring it forward). `on_save` gets the settings each time one changes.
pub fn show(hinst: HINSTANCE, owner: Option<HWND>, current: Settings, on_save: impl Fn(Settings) + 'static) {
    if let Some(h) = with(|u| u.hwnd) {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        UI.with(|u| *u.borrow_mut() = Some(Ui { hwnd: HWND::default(), view: SettingsView::new(current, workspaces()), on_save: Rc::new(on_save), timer: false }));
        let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME;
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge Settings"), style, CW_USEDEFAULT, CW_USEDEFAULT, 720, 560, owner, None, Some(hinst), None) else {
            UI.with(|u| u.borrow_mut().take());
            return;
        };
        with(|u| u.hwnd = hwnd);
        crate::frame::adopt(hwnd);
        // its size at this scale, over its owner (or the middle of the screen)
        let s = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let (ww, wh) = ((crate::settingsui::W * s).round() as i32, (crate::settingsui::H * s).round() as i32);
        let mut area = RECT { left: 0, top: 0, right: GetSystemMetrics(SM_CXSCREEN), bottom: GetSystemMetrics(SM_CYSCREEN) };
        if let Some(o) = owner {
            let _ = GetWindowRect(o, &mut area);
        }
        let (x, y) = (area.left + (area.right - area.left - ww) / 2, area.top + ((area.bottom - area.top - wh) / 3).max(0));
        let _ = SetWindowPos(hwnd, None, x, y, ww, wh, SWP_NOZORDER);
        restyle(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn restyle(hwnd: HWND) {
    let (dark, _, level) = crate::surface::glass_look();
    crate::frame::appearance(hwnd, dark);
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    let s = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    with(|u| u.view.set_window(rc.right.max(1) as usize, rc.bottom.max(1) as usize, s, dark, level));
    redraw(hwnd);
}

fn redraw(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Frames while something moves (a switch's knob, a menu opening).
fn animate(hwnd: HWND) {
    redraw(hwnd);
    let start = with(|u| !std::mem::replace(&mut u.timer, true)).unwrap_or(false);
    if start {
        unsafe {
            SetTimer(Some(hwnd), TIMER, 16, None);
        }
    }
}

/// What a click or a key asked for: a change applied at once (outside the window's state: the
/// viewer's state may be touched), a window button.
fn act(hwnd: HWND, a: Option<Act>) {
    match a {
        Some(Act::Changed(s)) => {
            if let Some(f) = with(|u| u.on_save.clone()) {
                f(s);
            }
        }
        Some(Act::Window(l)) => crate::frame::press(hwnd, l),
        None => {}
    }
    animate(hwnd);
}

fn close(hwnd: HWND) {
    UI.with(|u| u.borrow_mut().take());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn xy(lp: LPARAM) -> (f32, f32) {
    ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32)
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE => crate::frame::calc_size(hwnd, wp, lp),
        WM_NCHITTEST => with(|u| crate::frame::hit_test(hwnd, lp, false, |x, y| u.view.is_caption(x, y))).unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp)),
        WM_NCACTIVATE => {
            with(|u| u.view.set_active(wp.0 != 0));
            redraw(hwnd);
            DefWindowProcW(hwnd, msg, wp, LPARAM(-1))
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some(c) = with(|u| u.view.render(Instant::now())) {
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: c.w as i32, biHeight: -(c.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                    ..Default::default()
                };
                StretchDIBits(hdc, 0, 0, c.w as i32, c.h as i32, 0, 0, c.w as i32, c.h as i32, Some(c.px.as_ptr() as *const c_void), &bi, DIB_RGB_COLORS, SRCCOPY);
            }
            let _ = EndPaint(hwnd, &ps);
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
            if !with(|u| u.view.busy(Instant::now())).unwrap_or(false) {
                with(|u| u.timer = false);
                let _ = KillTimer(Some(hwnd), TIMER);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            let a = with(|u| u.view.mouse_move(x, y)).flatten();
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            act(hwnd, a);
            LRESULT(0)
        }
        WM_MOUSE_LEAVE => {
            with(|u| u.view.mouse_leave());
            redraw(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lp);
            SetCapture(hwnd);
            let a = with(|u| u.view.mouse_down(x, y)).flatten();
            act(hwnd, a);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lp);
            let _ = ReleaseCapture();
            let a = with(|u| u.view.mouse_up(x, y)).flatten();
            act(hwnd, a);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_UP => {
                    with(|u| u.view.key_up_down(false));
                    act(hwnd, None);
                }
                VK_DOWN => {
                    with(|u| u.view.key_up_down(true));
                    act(hwnd, None);
                }
                VK_RETURN => {
                    let a = with(|u| u.view.key_enter()).flatten();
                    act(hwnd, a);
                }
                VK_ESCAPE => {
                    // Escape closes an open menu, else the window
                    if with(|u| u.view.key_escape()) == Some(false) {
                        close(hwnd);
                    } else {
                        act(hwnd, None);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            close(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
