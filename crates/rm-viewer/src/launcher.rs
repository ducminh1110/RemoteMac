//! The launcher: the Mac's apps, after MobileLab's look (its Xcode 26 layout, see
//! docs/design/xcode-interface.md there): a softly tinted window; a toolbar with the MacBridge
//! mark, a status capsule (which Mac, connected how) and a round Settings button; and one floating
//! rounded panel holding a large title, a search field, the grid of the Mac's app icons and a
//! status strip. Everything is drawn here with paint.rs (no common controls), so it looks the same
//! at every scale, in light and dark. Click an app (or Enter) to open it; type to search; arrow
//! keys move; files dropped on it open on the Mac.
//!
//! Only one viewer process runs: a second invocation (e.g. a Start-menu shortcut) forwards its
//! `--app` to this window with WM_COPYDATA and exits.

use crate::launchview::{self, Grid, Nav, Tile};
use crate::look::{self, theme};
use crate::motion::{tokens, Anim, Curve};
use crate::paint::{Canvas, Rgba};
use std::cell::RefCell;
use std::collections::HashMap;
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

type Mask = (Vec<u8>, usize, usize);

/// What a click on the launcher asks for.
pub enum Act {
    Launch(String),
    Settings,
}

pub struct Launcher {
    pub hwnd: HWND,
    /// Application ids in the Mac's order.
    pub ids: Vec<String>,
    tiles: Vec<Tile>,
    icons: HashMap<String, Canvas>,
    query: String,
    /// the tiles shown (indexes into `tiles`) for the query
    shown: Vec<usize>,
    sel: Option<usize>,
    hover: Option<usize>,
    hover_t: Anim,
    /// a tile left by the pointer, fading out
    left: Option<(usize, Anim)>,
    pressed: Option<usize>,
    press_t: Anim,
    gear_hover: bool,
    gear_down: bool,
    scroll: Anim,
    /// "Connected", "Connecting…" and how (this network, the relay…)
    state: String,
    route: String,
    mac: String,
    since: Instant,
    /// drawn layers kept between frames: the window (background, panel), each tile, text
    base: Option<((i32, i32, u32, bool), Canvas)>,
    tile_px: HashMap<String, (u32, bool, Canvas)>,
    texts: RefCell<HashMap<(String, i32, i32, usize), Mask>>,
    timer: bool,
}

fn dark_mode() -> bool {
    crate::surface::dark_mode()
}

impl Launcher {
    pub fn create(hinst: HINSTANCE, show: bool) -> Option<Self> {
        unsafe {
            // 920 x 640 DIPs on the primary monitor
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let s = crate::surface::scale_at(pt.x, pt.y);
            let (w, h) = ((920.0 * s) as i32, (640.0 * s) as i32);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("MacBridge"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, w, h, None, None, Some(hinst), None).ok()?;
            // files dropped here open on the Mac with their default app
            windows::Win32::UI::Shell::DragAcceptFiles(hwnd, true);
            let mut l = Self {
                hwnd,
                ids: vec![],
                tiles: vec![],
                icons: HashMap::new(),
                query: String::new(),
                shown: vec![],
                sel: None,
                hover: None,
                hover_t: Anim::at(0.0),
                left: None,
                pressed: None,
                press_t: Anim::at(1.0),
                gear_hover: false,
                gear_down: false,
                scroll: Anim::at(0.0),
                state: "Connecting…".into(),
                route: String::new(),
                mac: "Your Mac".into(),
                since: Instant::now(),
                base: None,
                tile_px: HashMap::new(),
                texts: RefCell::new(HashMap::new()),
                timer: false,
            };
            l.frame_colours();
            l.animate();
            if show {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            Some(l)
        }
    }

    /// The title bar in the window's tint (Windows 11), dark with dark mode.
    fn frame_colours(&self) {
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
        let dark = dark_mode();
        let t = theme(dark);
        let c = t.win[0];
        let rgb = |c: Rgba| (c.r * 255.0) as u32 | ((c.g * 255.0) as u32) << 8 | ((c.b * 255.0) as u32) << 16;
        unsafe {
            let on: i32 = dark as i32;
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(20), &on as *const _ as *const c_void, 4); // immersive dark mode
            let cap = rgb(c);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(35), &cap as *const _ as *const c_void, 4); // caption colour
            let txt = rgb(t.text);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(36), &txt as *const _ as *const c_void, 4); // caption text
        }
    }

    /// Which Mac it is and how it is connected (the capsule and the status strip).
    pub fn set_connection(&mut self, mac: &str, state: &str, route: &str) {
        (self.mac, self.state, self.route) = (mac.into(), state.into(), route.into());
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
        if st != self.state || route != self.route {
            (self.state, self.route) = (st.into(), route);
            self.redraw();
        }
    }

    pub fn fit(&mut self) {
        self.base = None;
        self.redraw();
    }

    /// The theme changed (dark mode): everything drawn again.
    pub fn restyle(&mut self) {
        self.base = None;
        self.tile_px.clear();
        self.texts.borrow_mut().clear();
        self.frame_colours();
        self.redraw();
    }

    fn redraw(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn scale(&self) -> f32 {
        (unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(self.hwnd) } as f32 / 96.0).max(1.0)
    }

    fn size(&self) -> (i32, i32) {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        (rc.right.max(1), rc.bottom.max(1))
    }

    pub fn set_apps(&mut self, apps: &[(String, String, bool)]) {
        let open: Vec<String> = self.tiles.iter().filter(|t| t.open).map(|t| t.id.clone()).collect();
        self.tiles = apps.iter().map(|(id, name, available)| Tile { id: id.clone(), name: name.clone(), available: *available, open: open.contains(id) }).collect();
        self.ids = self.tiles.iter().map(|t| t.id.clone()).collect();
        if self.state.starts_with("Connecting") {
            self.state = "Connected".into();
        }
        self.refilter();
    }

    /// The app's icon (straight-alpha RGBA, `size` square).
    pub fn set_icon(&mut self, app: &str, size: u32, rgba: &[u8]) {
        if rgba.len() == (size * size * 4) as usize {
            self.icons.insert(app.to_string(), Canvas::from_rgba(size as usize, size as usize, rgba));
            self.tile_px.remove(app);
            self.redraw();
        }
    }

    /// Which apps have a window open here (a dot under their icon, as in the Dock).
    pub fn set_open(&mut self, open: &[String]) {
        let mut changed = false;
        for t in self.tiles.iter_mut() {
            let now = open.contains(&t.id);
            if t.open != now {
                t.open = now;
                changed = true;
                self.tile_px.remove(&t.id);
            }
        }
        if changed {
            self.redraw();
        }
    }

    pub fn count(&self) -> usize {
        self.tiles.len()
    }

    fn refilter(&mut self) {
        self.shown = launchview::filter(&self.tiles, &self.query);
        self.sel = if self.query.is_empty() { self.sel.filter(|s| *s < self.shown.len()) } else { (!self.shown.is_empty()).then_some(0) };
        self.hover = None;
        self.scroll = Anim::at(0.0);
        self.redraw();
    }

    // ---------------------------------------------------------------- geometry (pixels)

    fn panel(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (w, h) = self.size();
        let g = launchview::GAP * s;
        (g, launchview::TOOLBAR * s, w as f32 - 2.0 * g, h as f32 - launchview::TOOLBAR * s - g)
    }

    /// The grid's viewport (x, y, w, h).
    fn viewport(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (px, py, pw, ph) = self.panel();
        let side = (launchview::PAD - 10.0) * s;
        (px + side, py + launchview::HEADER * s, pw - 2.0 * side, ph - (launchview::HEADER + launchview::FOOTER) * s)
    }

    fn grid(&self) -> Grid {
        let (vx, vy, vw, _) = self.viewport();
        Grid::new(self.shown.len(), vx, vy + 4.0 * self.scale(), vw, self.scale())
    }

    fn gear(&self) -> (f32, f32, f32) {
        let s = self.scale();
        let (w, _) = self.size();
        (w as f32 - 16.0 * s - 15.0 * s, 26.0 * s, 15.0 * s)
    }

    fn search_box(&self) -> (f32, f32, f32, f32) {
        let s = self.scale();
        let (px, py, pw, _) = self.panel();
        let w = 240.0 * s;
        (px + pw - launchview::PAD * s - w, py + 30.0 * s, w, 30.0 * s)
    }

    // ---------------------------------------------------------------- input

    /// The pointer moved to (x, y) (client pixels).
    pub fn mouse_move(&mut self, x: i32, y: i32) {
        let (fx, fy) = (x as f32, y as f32);
        let (gx, gy, gr) = self.gear();
        let gear = (fx - gx).powi(2) + (fy - gy).powi(2) <= gr * gr;
        if gear != self.gear_hover {
            self.gear_hover = gear;
            self.redraw();
        }
        let (_, vy, _, vh) = self.viewport();
        let over = if fy >= vy && fy < vy + vh { self.grid().at(self.shown.len(), fx, fy, self.scroll.value()) } else { None };
        if over != self.hover {
            if let Some(old) = self.hover {
                let v = self.hover_t.value();
                self.left = Some((old, Anim::new(v, 0.0, tokens::pick(tokens::POPOVER, 0.6), Curve::Accelerate)));
            }
            self.hover = over;
            self.hover_t = Anim::new(0.0, 1.0, tokens::pick(tokens::PRESS, 0.6), Curve::Decelerate);
            self.animate();
        }
    }

    pub fn mouse_leave(&mut self) {
        self.mouse_move(-1000, -1000);
        self.gear_down = false;
    }

    pub fn mouse_down(&mut self, x: i32, y: i32) {
        self.mouse_move(x, y);
        if self.gear_hover {
            self.gear_down = true;
            self.redraw();
            return;
        }
        if let Some(k) = self.hover {
            self.pressed = Some(k);
            self.sel = Some(k);
            self.press_t.retarget(0.92, tokens::pick(tokens::PRESS, 0.3), Curve::Decelerate);
            self.animate();
        }
    }

    /// The button came up at (x, y): what to do.
    pub fn mouse_up(&mut self, x: i32, y: i32) -> Option<Act> {
        self.mouse_move(x, y);
        if std::mem::take(&mut self.gear_down) {
            self.redraw();
            return self.gear_hover.then_some(Act::Settings);
        }
        let pressed = self.pressed.take();
        self.press_t.retarget(1.0, tokens::pick(tokens::POPOVER, 0.4), Curve::SNAP);
        self.animate();
        let k = pressed.filter(|p| Some(*p) == self.hover)?;
        self.open(k)
    }

    fn open(&self, k: usize) -> Option<Act> {
        let t = self.tiles.get(*self.shown.get(k)?)?;
        t.available.then(|| Act::Launch(t.id.clone()))
    }

    /// The wheel turned by `notches` (120 a notch, up positive).
    pub fn wheel(&mut self, delta: i32) {
        let (_, _, _, vh) = self.viewport();
        let max = self.grid().max_scroll(vh);
        let to = (self.scroll.target() - delta as f32 / 120.0 * 72.0 * self.scale()).clamp(0.0, max);
        self.scroll.retarget(to, tokens::pick(tokens::POPOVER, 0.8), Curve::Decelerate);
        self.animate();
    }

    /// A key went down: Enter opens the selection, arrows move it, Escape clears the search.
    pub fn key(&mut self, vk: u16) -> Option<Act> {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        let nav = match VIRTUAL_KEY(vk) {
            VK_LEFT => Some(Nav::Left),
            VK_RIGHT => Some(Nav::Right),
            VK_UP => Some(Nav::Up),
            VK_DOWN => Some(Nav::Down),
            VK_HOME => Some(Nav::Home),
            VK_END => Some(Nav::End),
            _ => None,
        };
        if let Some(n) = nav {
            let g = self.grid();
            self.sel = launchview::step(self.sel, self.shown.len(), g.cols, n);
            if let Some(k) = self.sel {
                let (_, _, _, vh) = self.viewport();
                let to = g.reveal(k, self.scroll.target(), vh);
                self.scroll.retarget(to, tokens::pick(tokens::POPOVER, 0.6), Curve::Decelerate);
                self.animate();
            }
            self.redraw();
            return None;
        }
        match VIRTUAL_KEY(vk) {
            VK_RETURN => self.sel.or((!self.shown.is_empty()).then_some(0)).and_then(|k| self.open(k)),
            VK_ESCAPE if !self.query.is_empty() => {
                self.query.clear();
                self.refilter();
                None
            }
            _ => None,
        }
    }

    /// Typed text goes to the search.
    pub fn char(&mut self, c: char) {
        match c {
            '\u{8}' => {
                self.query.pop();
            }
            c if !c.is_control() && self.query.chars().count() < 40 => self.query.push(c),
            _ => return,
        }
        self.refilter();
    }

    /// One animation frame: false once nothing moves (the timer stops).
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        if self.left.as_ref().is_some_and(|(_, a)| a.done_at(now)) {
            self.left = None;
        }
        let busy = !self.hover_t.done_at(now) || self.left.is_some() || !self.press_t.done_at(now) || !self.scroll.done_at(now) || self.tiles.is_empty();
        self.redraw();
        busy
    }

    fn animate(&mut self) {
        if !self.timer {
            self.timer = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER, 16, None);
            }
        }
    }

    /// The timer fired: keep it while something moves.
    pub fn on_timer(&mut self) {
        if !self.tick() {
            self.timer = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER);
            }
        }
    }

    // ---------------------------------------------------------------- drawing

    fn text(&self, s: &str, px: f32, weight: i32, max_w: f32) -> Mask {
        let key = (s.to_string(), px.round() as i32, weight, max_w.max(1.0) as usize);
        if let Some(m) = self.texts.borrow().get(&key) {
            return m.clone();
        }
        let m = crate::surface::text_mask(s, key.1, weight, key.3);
        let mut t = self.texts.borrow_mut();
        if t.len() > 600 {
            t.clear();
        }
        t.insert(key, m.clone());
        m
    }

    fn put(&self, c: &mut Canvas, m: &Mask, x: f32, y: f32, color: Rgba) {
        c.fill_mask(&m.0, m.1, x.round() as isize, y.round() as isize, color);
    }

    /// The window without what moves: the tinted background, the toolbar's capsule and button
    /// shadows, the panel with its shadow.
    fn base(&mut self, w: i32, h: i32, s: f32, dark: bool) -> Canvas {
        let key = (w, h, (s * 100.0) as u32, dark);
        if let Some((k, c)) = self.base.as_ref() {
            if *k == key {
                return c.clone();
            }
        }
        let t = theme(dark);
        let mut c = Canvas::new(w as usize, h as usize);
        // MobileLab's tinted window, and the panel floating on it
        look::tint(&mut c, &t);
        let (px, py, pw, ph) = {
            let g = launchview::GAP * s;
            (g, launchview::TOOLBAR * s, w as f32 - 2.0 * g, h as f32 - launchview::TOOLBAR * s - g)
        };
        look::panel(&mut c, px, py, pw, ph, launchview::RADIUS * s, s, &t);
        // the footer strip's hairline
        let fy = py + ph - launchview::FOOTER * s;
        c.fill_round_rect(px, fy, pw, (0.5 * s).max(1.0), 0.0, t.divider);
        // the capsule and the round button get their shadow here (their content moves)
        let (cx0, cy0, cw, ch) = self.capsule_rect(w, s);
        c.shadow(cx0, cy0, cw, ch, ch / 2.0, 3.0 * s, 0.5 * s, Rgba::BLACK.alpha(if dark { 0.5 } else { 0.16 }));
        c.fill_round_rect(cx0, cy0, cw, ch, ch / 2.0, t.capsule);
        let (gx, gy, gr) = (w as f32 - 16.0 * s - 15.0 * s, 26.0 * s, 15.0 * s);
        c.shadow(gx - gr, gy - gr, 2.0 * gr, 2.0 * gr, gr, 3.0 * s, 0.5 * s, Rgba::BLACK.alpha(if dark { 0.5 } else { 0.16 }));
        self.base = Some((key, c.clone()));
        c
    }

    fn capsule_rect(&self, w: i32, s: f32) -> (f32, f32, f32, f32) {
        let cw = (520.0 * s).min(w as f32 - 2.0 * 190.0 * s).max(240.0 * s);
        ((w as f32 - cw) / 2.0, 9.0 * s, cw, 34.0 * s)
    }

    /// One tile (icon with its shadow, name, the open dot), drawn once per scale and theme.
    fn tile(&mut self, i: usize, cw: f32, ch: f32, s: f32, dark: bool) -> Canvas {
        let t = self.tiles[i].clone();
        let key = ((s * 100.0) as u32, dark);
        if let Some((sk, dk, c)) = self.tile_px.get(&t.id) {
            if (*sk, *dk) == key {
                return c.clone();
            }
        }
        let th = theme(dark);
        let mut c = Canvas::new(cw.ceil() as usize, ch.ceil() as usize);
        let ic = launchview::ICON * s;
        let (ix, iy) = ((cw - ic) / 2.0, 10.0 * s);
        match self.icons.get(&t.id) {
            Some(icon) => {
                // the icon's own shape casts a soft shadow, as icons on macOS do
                let mut sh = Canvas::new(c.w, c.h);
                sh.draw(icon, ix, iy + 2.0 * s, ic, ic, 0.0, 1.0);
                sh.blur((3.0 * s).round().max(1.0) as usize, 3);
                let m: Vec<u8> = sh.px.iter().map(|p| (p[3] as f32 * if dark { 0.5 } else { 0.28 }) as u8).collect();
                c.fill_mask(&m, c.w, 0, 0, Rgba::BLACK);
                c.draw(icon, ix, iy, ic, ic, 0.0, 1.0);
            }
            None => {
                // until the icon comes: a rounded square in the panel's grey
                c.fill_round_rect(ix + 4.0 * s, iy + 4.0 * s, ic - 8.0 * s, ic - 8.0 * s, 13.0 * s, th.field);
            }
        }
        let name = if t.available { t.name.clone() } else { format!("{} (not installed)", t.name) };
        let m = self.text(&name, 12.5 * s, 500, cw - 12.0 * s);
        let lx = (cw - m.1 as f32) / 2.0;
        let ly = iy + ic + 7.0 * s;
        self.put(&mut c, &m, lx, ly, if t.available { th.text } else { th.text3 });
        if t.open {
            c.fill_circle(cw / 2.0, ly + m.2 as f32 + 5.0 * s, 2.2 * s, th.text2);
        }
        if !t.available {
            for p in c.px.iter_mut() {
                for v in p.iter_mut() {
                    *v = (*v as f32 * 0.55) as u8;
                }
            }
        }
        self.tile_px.insert(t.id.clone(), (key.0, key.1, c.clone()));
        c
    }

    fn gear_glyph(c: &mut Canvas, x: f32, y: f32, r: f32, col: Rgba) {
        // a gear: teeth around a ring, a hole in the middle
        for i in 0..8 {
            let a = i as f32 / 8.0 * std::f32::consts::TAU;
            let (sn, cs) = a.sin_cos();
            c.fill_capsule(x + cs * r * 0.55, y + sn * r * 0.55, x + cs * r * 0.95, y + sn * r * 0.95, r * 0.19, col);
        }
        c.arc(x, y, r * 0.55, r * 0.26, 0.0, std::f32::consts::TAU, |_| col);
    }

    fn spinner(c: &mut Canvas, cx: f32, cy: f32, s: f32, col: Rgba, t: f32) {
        let lead = (t * 12.0) % 12.0;
        for i in 0..12 {
            let a = i as f32 / 12.0 * std::f32::consts::TAU;
            let (sn, cs) = a.sin_cos();
            let age = (lead - i as f32).rem_euclid(12.0) / 12.0;
            c.fill_capsule(cx + sn * 6.0 * s, cy - cs * 6.0 * s, cx + sn * 11.0 * s, cy - cs * 11.0 * s, 1.2 * s, col.fade(1.0 - 0.8 * age));
        }
    }

    /// The whole window as it is now.
    pub fn render(&mut self) -> Canvas {
        let (w, h) = self.size();
        let s = self.scale();
        let dark = dark_mode();
        let t = theme(dark);
        let now = Instant::now();
        let mut c = self.base(w, h, s, dark);
        // ---- toolbar: mark and name, capsule, Settings
        let ty = 13.0 * s;
        look::mark(&mut c, 16.0 * s, ty, 26.0 * s, t.accent);
        let m = self.text("MacBridge", 13.0 * s, 700, 200.0 * s);
        self.put(&mut c, &m, 50.0 * s, ty + (26.0 * s - m.2 as f32) / 2.0, t.text);
        let (cx0, cy0, cw, ch) = self.capsule_rect(w, s);
        let connected = self.state == "Connected";
        let dot = if connected { t.pass } else if self.state.starts_with("Connecting") || self.state.starts_with("Reconnecting") { t.warn } else { t.fail };
        c.fill_circle(cx0 + 18.0 * s, cy0 + ch / 2.0, 4.0 * s, dot);
        let mac = self.text(&self.mac, 12.0 * s, 600, cw * 0.45);
        self.put(&mut c, &mac, cx0 + 30.0 * s, cy0 + (ch - mac.2 as f32) / 2.0, t.text);
        let st = self.text(&self.state, 12.0 * s, 600, cw * 0.25);
        let rt = self.text(&self.route, 12.0 * s, 400, cw * 0.3);
        let right = cx0 + cw - 16.0 * s;
        let (rx, sx) = (right - rt.1 as f32, right - rt.1 as f32 - if rt.1 > 1 { 18.0 * s } else { 0.0 } - st.1 as f32);
        self.put(&mut c, &st, sx, cy0 + (ch - st.2 as f32) / 2.0, t.text);
        if rt.1 > 1 {
            c.fill_round_rect(rx - 9.5 * s, cy0 + 10.0 * s, (0.8 * s).max(1.0), ch - 20.0 * s, 0.0, t.divider);
            self.put(&mut c, &rt, rx, cy0 + (ch - rt.2 as f32) / 2.0, t.text2);
        }
        let (gx, gy, gr) = self.gear();
        let face = if self.gear_down { t.capsule.lerp(Rgba::BLACK, 0.08) } else if self.gear_hover { t.capsule.lerp(t.accent, 0.08) } else { t.capsule };
        c.fill_circle(gx, gy, gr, face);
        Self::gear_glyph(&mut c, gx, gy, 8.5 * s, if self.gear_hover { t.accent } else { t.text2 });
        // ---- the panel's header: title, what to do, the search field
        let (px, py, pw, ph) = self.panel();
        let pad = launchview::PAD * s;
        let title = self.text("Applications", 28.0 * s, 400, pw * 0.5);
        self.put(&mut c, &title, px + pad, py + 24.0 * s, t.text);
        let hint = if self.tiles.is_empty() { "Waiting for the Mac's list of apps…".to_string() } else { format!("On {} · click an app to open it, or type to search", self.mac) };
        let sub = self.text(&hint, 13.0 * s, 400, pw - 2.0 * pad - 260.0 * s);
        self.put(&mut c, &sub, px + pad, py + 24.0 * s + title.2 as f32 + 2.0 * s, t.text2);
        let (bx, by, bw, bh) = self.search_box();
        c.fill_round_rect(bx, by, bw, bh, 8.0 * s, t.field);
        if !self.query.is_empty() {
            c.stroke_round_rect_with(bx - 1.0 * s, by - 1.0 * s, bw + 2.0 * s, bh + 2.0 * s, 9.0 * s, 2.0 * s, |_, _| t.accent.alpha(0.55));
        }
        let (mx, my) = (bx + 15.0 * s, by + bh / 2.0 - 1.0 * s);
        c.arc(mx, my, 4.6 * s, 1.5 * s, 0.0, std::f32::consts::TAU, |_| t.text2);
        c.fill_capsule(mx + 3.6 * s, my + 3.6 * s, mx + 7.0 * s, my + 7.0 * s, 0.8 * s, t.text2);
        let q = if self.query.is_empty() { self.text("Search apps", 13.0 * s, 400, bw - 40.0 * s) } else { self.text(&self.query, 13.0 * s, 400, bw - 40.0 * s) };
        let qx = bx + 28.0 * s;
        self.put(&mut c, &q, qx, by + (bh - q.2 as f32) / 2.0, if self.query.is_empty() { t.text3 } else { t.text });
        if !self.query.is_empty() {
            c.fill_round_rect(qx + q.1 as f32 + 1.0 * s, by + 7.0 * s, (1.2 * s).max(1.0), bh - 14.0 * s, 0.0, t.accent);
        }
        // ---- the grid, in its viewport (scrolled)
        let (vx, vy, vw, vh) = self.viewport();
        let mut view = Canvas::new(vw.max(1.0) as usize, vh.max(1.0) as usize);
        let g = self.grid();
        let scroll = self.scroll.value_at(now);
        let first = ((scroll / g.cell_h).floor() as usize) * g.cols;
        let last = ((((scroll + vh) / g.cell_h).ceil() as usize + 1) * g.cols).min(self.shown.len());
        for k in first..last {
            let i = self.shown[k];
            let (x, y, cw, ch) = g.cell(k, scroll);
            let (lx, ly) = (x - vx, y - vy);
            // the hover (fading in, or out for the tile just left) and the pressed highlight
            let lit = if self.hover == Some(k) { self.hover_t.value_at(now) } else if self.left.as_ref().is_some_and(|(j, _)| *j == k) { self.left.as_ref().map_or(0.0, |(_, a)| a.value_at(now)) } else { 0.0 };
            let inset = 5.0 * s;
            if lit > 0.01 {
                let col = if self.pressed == Some(k) { t.pressed } else { t.hover };
                view.fill_round_rect(lx + inset, ly + inset, cw - 2.0 * inset, ch - 2.0 * inset, 12.0 * s, col.alpha(col.a * lit));
            }
            if self.sel == Some(k) && self.hover.is_none() {
                view.stroke_round_rect_with(lx + inset, ly + inset, cw - 2.0 * inset, ch - 2.0 * inset, 12.0 * s, 2.0 * s, |_, _| t.accent);
            }
            let tile = self.tile(i, cw, ch, s, dark);
            let k_press = if self.pressed == Some(k) || !self.press_t.done_at(now) && self.sel == Some(k) { self.press_t.value_at(now) } else { 1.0 };
            if (k_press - 1.0).abs() > 0.002 {
                let (tw, th) = (tile.w as f32 * k_press, tile.h as f32 * k_press);
                view.draw(&tile, lx + (cw - tw) / 2.0, ly + (ch - th) / 2.0, tw, th, 0.0, 1.0);
            } else {
                view.composite(&tile, lx.round() as isize, ly.round() as isize, 1.0);
            }
        }
        // an empty grid says why
        if self.tiles.is_empty() {
            Self::spinner(&mut view, vw / 2.0, vh / 2.0 - 16.0 * s, s, t.text2, self.since.elapsed().as_secs_f32());
            let m = self.text(if self.state == "Connected" { "Getting the Mac's apps…" } else { "Connecting to your Mac…" }, 13.0 * s, 500, vw);
            self.put(&mut view, &m, (vw - m.1 as f32) / 2.0, vh / 2.0 + 6.0 * s, t.text2);
        } else if self.shown.is_empty() {
            let m = self.text(&format!("No apps match “{}”", self.query), 15.0 * s, 500, vw);
            self.put(&mut view, &m, (vw - m.1 as f32) / 2.0, vh / 2.0 - 20.0 * s, t.text2);
            let m2 = self.text("Escape clears the search", 12.0 * s, 400, vw);
            self.put(&mut view, &m2, (vw - m2.1 as f32) / 2.0, vh / 2.0 + 4.0 * s, t.text3);
        }
        // the grid fades at the edges it scrolls under
        let fade = 14.0 * s;
        for (yy, row) in view.px.chunks_exact_mut(view.w).enumerate() {
            let y = yy as f32;
            let k = if scroll > 0.5 && y < fade { y / fade } else if g.height() - scroll > vh + 0.5 && y > vh - fade { (vh - y) / fade } else { 1.0 };
            if k < 1.0 {
                for p in row.iter_mut() {
                    for v in p.iter_mut() {
                        *v = (*v as f32 * k.max(0.0)) as u8;
                    }
                }
            }
        }
        c.composite(&view, vx.round() as isize, vy.round() as isize, 1.0);
        // ---- the status strip
        let (state, detail) = launchview::footer(&self.state, &self.route, &self.tiles);
        let a = self.text(&state, 11.5 * s, 600, pw * 0.3);
        let b = self.text(&detail, 11.5 * s, 400, pw * 0.6);
        let total = a.1 as f32 + 21.0 * s + b.1 as f32;
        let fy = py + ph - launchview::FOOTER * s;
        let fx = px + (pw - total) / 2.0;
        let ycen = fy + (launchview::FOOTER * s - a.2 as f32) / 2.0;
        self.put(&mut c, &a, fx, ycen, t.text);
        c.fill_round_rect(fx + a.1 as f32 + 10.0 * s, fy + 9.0 * s, (0.8 * s).max(1.0), launchview::FOOTER * s - 18.0 * s, 0.0, t.divider);
        self.put(&mut c, &b, fx + a.1 as f32 + 21.0 * s, ycen, t.text2);
        c
    }

    /// Draw into `hdc` (WM_PAINT, WM_PRINTCLIENT).
    pub fn paint(&mut self, hdc: HDC) {
        let c = self.render();
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
