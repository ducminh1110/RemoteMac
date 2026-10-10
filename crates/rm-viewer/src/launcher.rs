//! The launcher window: the Mac's apps, after Apple's Screen Sharing on macOS 26 (launchui.rs
//! draws it and says what clicks and keys do; this puts it in a Windows window). The window has
//! no Windows title bar: its toolbar is the title bar, with the window buttons in it (frame.rs).
//! Click an app (or Enter) to open it; type to search; arrow keys move; files dropped on it open
//! on the Mac.
//!
//! Only one viewer process runs: a second invocation (e.g. a Start-menu shortcut) forwards its
//! `--app` to this window with WM_COPYDATA and exits.

use crate::glass::Level;
use crate::launchui::{Key, View};
use std::ffi::c_void;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmLauncher");
/// COPYDATASTRUCT.dwData tag for "launch this application id".
pub const COPYDATA_LAUNCH: usize = 0x524D_4C31; // "RML1"
/// The animation timer (WM_TIMER id) while something moves.
pub const TIMER: usize = 41;
/// The smallest the window gets (DIPs).
pub const MIN_W: f32 = 560.0;
pub const MIN_H: f32 = 400.0;

/// What a click on the launcher asks for (the window's own buttons are done here).
pub enum Act {
    Launch(String),
    Settings,
}

pub struct Launcher {
    pub hwnd: HWND,
    /// Application ids in the Mac's order.
    pub ids: Vec<String>,
    view: View,
    level: Level,
    timer: bool,
}

impl Launcher {
    pub fn create(hinst: HINSTANCE, show: bool) -> Option<Self> {
        unsafe {
            // 920 x 620 DIPs on the monitor with the pointer
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let s = crate::surface::scale_at(pt.x, pt.y);
            let (w, h) = ((920.0 * s) as i32, (620.0 * s) as i32);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, w, h, None, None, Some(hinst), None).ok()?;
            // files dropped here open on the Mac with their default app
            windows::Win32::UI::Shell::DragAcceptFiles(hwnd, true);
            crate::frame::adopt(hwnd);
            let mut l = Self { hwnd, ids: vec![], view: View::new(), level: Level::Full, timer: false };
            l.restyle();
            if show {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            Some(l)
        }
    }

    /// Which Mac it is and how it is connected (the toolbar's title and subtitle).
    pub fn set_connection(&mut self, mac: &str, state: &str, route: &str) {
        self.view.set_connection(mac, state, route);
        self.redraw();
    }

    /// The connection's phase now (connecting again, lost…): shown when it changes.
    pub fn poll(&mut self) {
        use crate::lifecycle::Phase;
        let st = match crate::lifecycle::current() {
            Phase::Idle => return,
            Phase::Connected => "Connected",
            Phase::Reconnecting => "Connecting again…",
            Phase::Disconnecting | Phase::Disconnected => "Disconnected",
            Phase::Error(_) => "Not connected",
            _ => "Connecting…",
        };
        let route = crate::net::route_label();
        if st != self.view.state || route != self.view.route {
            let mac = self.view.mac.clone();
            self.view.set_connection(&mac, st, &route);
            self.redraw();
        }
    }

    /// The window changed size (or was maximized, or restored).
    pub fn fit(&mut self) {
        self.sync();
        self.redraw();
    }

    /// The theme or the scale changed: everything drawn again.
    pub fn restyle(&mut self) {
        let (dark, _, level) = crate::surface::glass_look();
        self.level = level;
        crate::frame::appearance(self.hwnd, dark);
        self.sync();
        self.animate();
    }

    /// The window's size, scale and appearance, to the view.
    fn sync(&mut self) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        let s = (unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.hwnd) } as f32 / 96.0).max(1.0);
        self.view.set_window(rc.right.max(1) as usize, rc.bottom.max(1) as usize, s, crate::surface::dark_mode(), self.level);
        self.view.set_maximized(crate::frame::maximized(self.hwnd));
    }

    /// The window became the active one, or stopped being it (its buttons go grey).
    pub fn set_active(&mut self, active: bool) {
        self.view.set_active(active);
        self.redraw();
    }

    /// WM_NCHITTEST: the toolbar's empty part moves the window.
    pub fn hit_test(&self, lp: LPARAM) -> LRESULT {
        crate::frame::hit_test(self.hwnd, lp, true, |x, y| self.view.is_caption(x, y))
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn set_apps(&mut self, apps: &[(String, String, bool)]) {
        self.view.set_apps(apps);
        self.ids = self.view.tiles().iter().map(|t| t.id.clone()).collect();
        self.animate();
    }

    /// The app's icon (straight-alpha RGBA, `size` square).
    pub fn set_icon(&mut self, app: &str, size: u32, rgba: &[u8]) {
        self.view.set_icon(app, size, rgba);
        self.redraw();
    }

    /// Which apps have a window open here (a dot under their icon, as in the Dock).
    pub fn set_open(&mut self, open: &[String]) {
        if self.view.set_open(open) {
            self.redraw();
        }
    }

    pub fn count(&self) -> usize {
        self.view.tiles().len()
    }

    // ---------------------------------------------------------------- input

    pub fn mouse_move(&mut self, x: i32, y: i32) {
        let before = self.view.hot();
        self.view.mouse_move(x as f32, y as f32);
        if self.view.hot() != before {
            self.animate();
        }
    }

    pub fn mouse_leave(&mut self) {
        self.view.mouse_leave();
        self.animate();
    }

    pub fn mouse_down(&mut self, x: i32, y: i32) {
        self.view.mouse_down(x as f32, y as f32);
        self.animate();
    }

    /// The button came up at (x, y): what to do.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<Act> {
        let act = self.view.mouse_up(x as f32, y as f32);
        self.animate();
        self.act(act)
    }

    fn act(&mut self, act: Option<crate::launchui::Act>) -> Option<Act> {
        match act? {
            crate::launchui::Act::Launch(id) => Some(Act::Launch(id)),
            crate::launchui::Act::Settings => Some(Act::Settings),
            crate::launchui::Act::Window(l) => {
                crate::frame::press(self.hwnd, l);
                None
            }
        }
    }

    /// The wheel turned by `delta` (120 a notch, up positive).
    pub fn wheel(&mut self, delta: i32) {
        self.view.wheel(delta);
        self.animate();
    }

    /// A key went down: Enter opens the selection, arrows move it, Escape clears the search.
    pub fn key(&mut self, vk: u16) -> Option<Act> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let k = match VIRTUAL_KEY(vk) {
            VK_LEFT => Key::Left,
            VK_RIGHT => Key::Right,
            VK_UP => Key::Up,
            VK_DOWN => Key::Down,
            VK_HOME => Key::Home,
            VK_END => Key::End,
            VK_RETURN => Key::Enter,
            VK_ESCAPE => Key::Escape,
            _ => return None,
        };
        let act = self.view.key(k);
        self.animate();
        self.act(act)
    }

    /// Typed text goes to the search.
    pub fn char(&mut self, c: char) {
        self.view.char(c);
        self.animate();
    }

    fn animate(&mut self) {
        self.redraw();
        if !self.timer {
            self.timer = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER, 16, None);
            }
        }
    }

    /// The timer fired: draw, and keep it while something moves.
    pub fn on_timer(&mut self) {
        self.redraw();
        if !self.view.busy(Instant::now()) {
            self.timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER);
            }
        }
    }

    // ---------------------------------------------------------------- drawing

    /// Draw into `hdc` (WM_PAINT, WM_PRINTCLIENT).
    pub fn paint(&mut self, hdc: HDC) {
        let c = self.view.render(Instant::now());
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: c.w as i32, biHeight: -(c.h as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        unsafe {
            StretchDIBits(hdc, 0, 0, c.w as i32, c.h as i32, 0, 0, c.w as i32, c.h as i32, Some(c.px.as_ptr() as *const c_void), &bi, DIB_RGB_COLORS, SRCCOPY);
        }
    }
}

/// If another viewer is already running, hand it `app` and return true (this process should exit).
pub fn forward_to_running_instance(app: Option<&str>) -> bool {
    unsafe {
        let Ok(existing) = FindWindowW(CLASS, PCWSTR::null()) else { return false };
        if existing.0.is_null() {
            return false;
        }
        if let Some(app) = app {
            let bytes = app.as_bytes();
            let cds = COPYDATASTRUCT { dwData: COPYDATA_LAUNCH, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut c_void };
            SendMessageW(existing, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&cds as *const _ as isize)));
        } else {
            let _ = ShowWindow(existing, SW_SHOW);
        }
        let _ = SetForegroundWindow(existing);
        true
    }
}

/// Decode a WM_COPYDATA launch request.
pub fn copydata_app(lp: LPARAM) -> Option<String> {
    unsafe {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData != COPYDATA_LAUNCH || cds.lpData.is_null() || cds.cbData > 256 {
            return None;
        }
        let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
        std::str::from_utf8(bytes).ok().filter(|s| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')).map(String::from)
    }
}
