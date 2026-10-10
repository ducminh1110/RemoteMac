//! The launcher as it looks and moves, after Apple's Screen Sharing on macOS 26: one window, its
//! title bar and toolbar in one (the window buttons, the Mac's name with how it is connected,
//! Liquid Glass controls: the Mac Desktop and Settings, a search field), and the Mac's apps
//! edge to edge under it in a Finder-like icon grid. What scrolls under the toolbar softens into
//! it (the scroll edge effect). Everything moves on springs: icons lift under the pointer and
//! give when pressed, the grid flows to its new places as the search narrows it, and the apps
//! come in one after another when the Mac's list arrives.
//!
//! Portable: it draws into a Canvas and takes plain input, so it is tested (and previewed) on
//! any system; launcher.rs puts it in a Windows window.

use crate::glass::{self, Kind, Level, Material};
use crate::launchview::{self, Grid, Nav, Tile};
use crate::look::{self, theme, Light, Symbol, Theme};
use crate::motion::{Anim, Curve};
use crate::paint::{Canvas, Rgba};
use crate::text::{self, Style};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The toolbar's height and its parts (DIPs).
pub const TOOLBAR: f32 = 52.0;
const LIGHTS_X: f32 = 20.0;
const CONTROL_H: f32 = 34.0;
const BUTTON_W: f32 = 40.0;
const SEARCH_W: f32 = 230.0;
const EDGE: f32 = 14.0;
const SIDE: f32 = 22.0;

/// What a click or a key asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Launch(String),
    Settings,
    Window(Light),
}

/// Keys the launcher uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Escape,
}

/// What is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hot {
    None,
    Lights(Option<Light>),
    Desktop,
    Settings,
    Search,
    Clear,
    /// a tile, by its index in the tiles
    Tile(usize),
}

/// A tile's place in the grid (before scrolling) and whether it is shown, all animated.
struct Spot {
    x: Anim,
    y: Anim,
    show: Anim,
    /// a later start (the apps come in one after another)
    delay: Duration,
}

/// The spring things move on: quick, the slightest settle (as SwiftUI's default).
const FLOW: Curve = Curve::Spring { response: 0.42, damping: 0.86 };
const LIFT: Curve = Curve::Spring { response: 0.3, damping: 0.78 };
const GIVE: Curve = Curve::Spring { response: 0.18, damping: 1.0 };
const BACK: Curve = Curve::Spring { response: 0.36, damping: 0.62 };

/// A line of text: its coverage, width and height.
type Mask = (Vec<u8>, usize, usize);

fn d(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

pub struct View {
    tiles: Vec<Tile>,
    icons: HashMap<String, Canvas>,
    query: String,
    shown: Vec<usize>,
    /// the keyboard's selection (a position in `shown`)
    sel: Option<usize>,
    hot: Hot,
    down: Hot,
    lift: HashMap<usize, Anim>,
    press: Anim,
    pressed: Option<usize>,
    scroll: Anim,
    spots: HashMap<usize, Spot>,
    pub mac: String,
    pub state: String,
    pub route: String,
    since: Instant,
    // the window
    w: usize,
    h: usize,
    s: f32,
    dark: bool,
    active: bool,
    maximized: bool,
    level: Level,
    searching: bool,
    apps_came: bool,
    // drawn once, kept
    bg: Option<((usize, usize, u32, bool), Canvas)>,
    icon_px: HashMap<(String, u32, bool), Canvas>,
    texts: HashMap<(String, u32, i32, u32), Mask>,
    glass: HashMap<u8, (u64, Canvas)>,
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}

impl View {
    pub fn new() -> View {
        View {
            tiles: vec![],
            icons: HashMap::new(),
            query: String::new(),
            shown: vec![],
            sel: None,
            hot: Hot::None,
            down: Hot::None,
            lift: HashMap::new(),
            press: Anim::at(1.0),
            pressed: None,
            scroll: Anim::at(0.0),
            spots: HashMap::new(),
            mac: "Your Mac".into(),
            state: "Connecting…".into(),
            route: String::new(),
            since: Instant::now(),
            w: 920,
            h: 640,
            s: 1.0,
            dark: false,
            active: true,
            maximized: false,
            level: Level::Full,
            searching: false,
            apps_came: false,
            bg: None,
            icon_px: HashMap::new(),
            texts: HashMap::new(),
            glass: HashMap::new(),
        }
    }

    // ------------------------------------------------------------------ the window around it

    /// The window's size (px), scale (px per DIP), appearance and glass level.
    pub fn set_window(&mut self, w: usize, h: usize, s: f32, dark: bool, level: Level) {
        let resized = (w, h) != (self.w, self.h) || (s - self.s).abs() > 0.001;
        if (s - self.s).abs() > 0.001 || dark != self.dark {
            self.icon_px.clear();
            self.texts.clear();
        }
        (self.w, self.h, self.s, self.dark, self.level) = (w.max(1), h.max(1), s.max(0.5), dark, level);
        if resized {
            self.relayout(false);
            let max = self.max_scroll();
            if self.scroll.target() > max {
                self.scroll = Anim::at(max);
            }
        }
    }

    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    pub fn set_maximized(&mut self, m: bool) {
        self.maximized = m;
    }

    pub fn set_connection(&mut self, mac: &str, state: &str, route: &str) {
        (self.mac, self.state, self.route) = (mac.into(), state.into(), route.into());
    }

    // ------------------------------------------------------------------ the apps

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn set_apps(&mut self, apps: &[(String, String, bool)]) {
        let open: Vec<String> = self.tiles.iter().filter(|t| t.open).map(|t| t.id.clone()).collect();
        let first = self.tiles.is_empty() && !apps.is_empty();
        self.tiles = apps.iter().map(|(id, name, available)| Tile { id: id.clone(), name: name.clone(), available: *available, open: open.contains(id) }).collect();
        if self.state.starts_with("Connecting") {
            self.state = "Connected".into();
        }
        self.spots.clear();
        self.lift.clear();
        self.apps_came = first;
        self.refilter();
        self.apps_came = false;
    }

    /// An app's icon (straight-alpha RGBA, `size` square).
    pub fn set_icon(&mut self, app: &str, size: u32, rgba: &[u8]) {
        if rgba.len() == (size * size * 4) as usize {
            self.icons.insert(app.to_string(), Canvas::from_rgba(size as usize, size as usize, rgba));
            self.icon_px.retain(|k, _| k.0 != app);
        }
    }

    pub fn set_icon_canvas(&mut self, app: &str, icon: Canvas) {
        self.icons.insert(app.to_string(), icon);
        self.icon_px.retain(|k, _| k.0 != app);
    }

    /// Which apps have a window open here (a dot under them, as in the Dock): true if it changed.
    pub fn set_open(&mut self, open: &[String]) -> bool {
        let mut changed = false;
        for t in self.tiles.iter_mut() {
            let now = open.contains(&t.id);
            changed |= t.open != now;
            t.open = now;
        }
        changed
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn refilter(&mut self) {
        self.shown = launchview::filter(&self.tiles, &self.query);
        self.sel = if self.query.is_empty() { self.sel.filter(|s| *s < self.shown.len()) } else { (!self.shown.is_empty()).then_some(0) };
        self.relayout(true);
        let max = self.max_scroll();
        if self.scroll.target() > max {
            self.scroll.retarget(max, d(300), FLOW);
        }
    }

    // ------------------------------------------------------------------ geometry (px)

    fn tb(&self) -> f32 {
        TOOLBAR * self.s
    }

    fn grid(&self) -> Grid {
        let s = self.s;
        Grid::new(self.shown.len(), SIDE * s, self.tb() + 6.0 * s, self.w as f32 - 2.0 * SIDE * s, s)
    }

    fn view_h(&self) -> f32 {
        self.h as f32 - self.tb() - 6.0 * self.s
    }

    fn max_scroll(&self) -> f32 {
        (self.grid().height() + 16.0 * self.s - self.view_h()).max(0.0)
    }

    /// The first light's centre.
    fn lights(&self) -> (f32, f32) {
        (LIGHTS_X * self.s + look::LIGHT_D * self.s / 2.0, self.tb() / 2.0)
    }

    fn search_rect(&self) -> (f32, f32, f32, f32) {
        let s = self.s;
        let w = (SEARCH_W * s).min((self.w as f32 - 470.0 * s).max(130.0 * s));
        (self.w as f32 - EDGE * s - w, (self.tb() - CONTROL_H * s) / 2.0, w, CONTROL_H * s)
    }

    fn group_rect(&self) -> (f32, f32, f32, f32) {
        let s = self.s;
        let (sx, sy, _, sh) = self.search_rect();
        let w = 2.0 * BUTTON_W * s;
        (sx - 10.0 * s - w, sy, w, sh)
    }

    fn clear_spot(&self) -> (f32, f32) {
        let (x, y, w, h) = self.search_rect();
        (x + w - 16.0 * self.s, y + h / 2.0)
    }

    /// Where the tiles go now (relative to the grid, before scrolling): animated to, or at once.
    fn relayout(&mut self, animated: bool) {
        let g = self.grid();
        let now = Instant::now();
        let shown: std::collections::HashSet<usize> = self.shown.iter().copied().collect();
        for (k, &i) in self.shown.iter().enumerate() {
            let (x, y, _, _) = g.cell(k, 0.0);
            match self.spots.get_mut(&i) {
                Some(sp) if animated => {
                    let was_shown = sp.show.target() > 0.5;
                    if was_shown {
                        sp.x.retarget_at(x, d(400), FLOW, now);
                        sp.y.retarget_at(y, d(400), FLOW, now);
                    } else {
                        // back in: where it belongs now, growing in
                        (sp.x, sp.y) = (Anim::at(x), Anim::at(y));
                    }
                    sp.show.retarget_at(1.0, d(360), FLOW, now);
                    sp.delay = Duration::ZERO;
                }
                Some(sp) => {
                    (sp.x, sp.y) = (Anim::at(x), Anim::at(y));
                }
                None => {
                    let delay = if self.apps_came { d((k as u64 * 18).min(360)) } else { Duration::ZERO };
                    let show = if animated { Anim::new(0.0, 1.0, d(420), FLOW) } else { Anim::at(1.0) };
                    self.spots.insert(i, Spot { x: Anim::at(x), y: Anim::at(y), show, delay });
                }
            }
        }
        for (i, sp) in self.spots.iter_mut() {
            if !shown.contains(i) && sp.show.target() > 0.0 {
                sp.show.retarget_at(0.0, d(140), Curve::Accelerate, now);
                sp.delay = Duration::ZERO;
            }
        }
    }

    // ------------------------------------------------------------------ input (px)

    fn hit(&self, x: f32, y: f32) -> Hot {
        let s = self.s;
        let (lx, ly) = self.lights();
        if x < lx + 2.0 * look::LIGHT_STEP * s + look::LIGHT_STEP * s / 2.0 && y < self.tb() {
            if let Some(l) = look::light_at(x, y, lx, ly, s) {
                return Hot::Lights(Some(l));
            }
            if (y - ly).abs() < look::LIGHT_STEP * s / 2.0 && x >= lx - look::LIGHT_STEP * s / 2.0 {
                return Hot::Lights(None);
            }
        }
        let inside = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        let g = self.group_rect();
        if inside(g) {
            return if x < g.0 + g.2 / 2.0 { Hot::Desktop } else { Hot::Settings };
        }
        if inside(self.search_rect()) {
            let (cx, cy) = self.clear_spot();
            if !self.query.is_empty() && (x - cx).powi(2) + (y - cy).powi(2) < (11.0 * s).powi(2) {
                return Hot::Clear;
            }
            return Hot::Search;
        }
        if y >= self.tb() {
            let gr = self.grid();
            if let Some(k) = gr.at(self.shown.len(), x, y, self.scroll.value()) {
                // the icon and its name, not the space around them
                let (cx, cy, cw, _) = gr.cell(k, self.scroll.value());
                let ic = launchview::ICON * s;
                if (x - (cx + cw / 2.0)).abs() < ic / 2.0 + 14.0 * s && y > cy + 4.0 * s && y < cy + 14.0 * s + ic + 26.0 * s {
                    return Hot::Tile(self.shown[k]);
                }
            }
        }
        Hot::None
    }

    /// The pointer is over the toolbar's empty part: the window is moved by dragging it there.
    pub fn is_caption(&self, x: f32, y: f32) -> bool {
        y < self.tb() && self.hit(x, y) == Hot::None
    }

    pub fn hot(&self) -> Hot {
        self.hot
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        let h = self.hit(x, y);
        if h == self.hot {
            return;
        }
        let now = Instant::now();
        if let Hot::Tile(i) = self.hot {
            if let Some(a) = self.lift.get_mut(&i) {
                a.retarget_at(0.0, d(300), LIFT, now);
            }
        }
        if let Hot::Tile(i) = h {
            self.lift.entry(i).or_insert_with(|| Anim::at(0.0)).retarget_at(1.0, d(300), LIFT, now);
        }
        self.hot = h;
    }

    pub fn mouse_leave(&mut self) {
        self.mouse_move(-1e6, -1e6);
        self.down = Hot::None;
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) {
        self.mouse_move(x, y);
        self.down = self.hot;
        self.searching = matches!(self.hot, Hot::Search | Hot::Clear) || (self.searching && !matches!(self.hot, Hot::None));
        if let Hot::Tile(i) = self.hot {
            self.pressed = Some(i);
            if let Some(k) = self.shown.iter().position(|&j| j == i) {
                self.sel = Some(k);
            }
            self.press.retarget(0.9, d(160), GIVE);
        }
    }

    pub fn mouse_up(&mut self, x: f32, y: f32) -> Option<Act> {
        self.mouse_move(x, y);
        let down = std::mem::replace(&mut self.down, Hot::None);
        let pressed = self.pressed.take();
        if pressed.is_some() {
            self.press.retarget(1.0, d(500), BACK);
        }
        if down != self.hot {
            return None;
        }
        match down {
            Hot::Lights(Some(l)) => Some(Act::Window(l)),
            Hot::Desktop => Some(Act::Launch("desktop".into())),
            Hot::Settings => Some(Act::Settings),
            Hot::Clear => {
                self.query.clear();
                self.refilter();
                None
            }
            Hot::Tile(i) if pressed == Some(i) => self.tiles.get(i).filter(|t| t.available).map(|t| Act::Launch(t.id.clone())),
            _ => None,
        }
    }

    /// The wheel turned (120 a notch, up positive).
    pub fn wheel(&mut self, delta: i32) {
        let to = (self.scroll.target() - delta as f32 / 120.0 * 80.0 * self.s).clamp(0.0, self.max_scroll());
        self.scroll.retarget(to, d(320), Curve::Decelerate);
    }

    pub fn key(&mut self, k: Key) -> Option<Act> {
        let nav = match k {
            Key::Left => Some(Nav::Left),
            Key::Right => Some(Nav::Right),
            Key::Up => Some(Nav::Up),
            Key::Down => Some(Nav::Down),
            Key::Home => Some(Nav::Home),
            Key::End => Some(Nav::End),
            _ => None,
        };
        if let Some(n) = nav {
            let g = self.grid();
            self.sel = launchview::step(self.sel, self.shown.len(), g.cols, n);
            if let Some(k) = self.sel {
                let top = g.cell(k, 0.0).1 - g.y;
                let vh = self.view_h();
                let sc = self.scroll.target();
                let to = if top < sc { top } else if top + g.cell_h > sc + vh { top + g.cell_h - vh + 8.0 * self.s } else { sc };
                self.scroll.retarget(to.clamp(0.0, self.max_scroll()), d(300), FLOW);
            }
            return None;
        }
        match k {
            Key::Enter => {
                let k = self.sel.or((!self.shown.is_empty()).then_some(0))?;
                let t = self.tiles.get(*self.shown.get(k)?)?;
                t.available.then(|| Act::Launch(t.id.clone()))
            }
            Key::Escape if !self.query.is_empty() => {
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
        self.searching = true;
        self.refilter();
    }

    /// Something still moves at `now` (keep drawing frames).
    pub fn busy(&self, now: Instant) -> bool {
        self.tiles.is_empty()
            || !self.press.done_at(now)
            || !self.scroll.done_at(now)
            || self.lift.values().any(|a| !a.done_at(now))
            || self.spots.values().any(|s| !s.show.done_at(now - s.delay.min(now - self.since)) || !s.x.done_at(now) || !s.y.done_at(now))
    }

    // ------------------------------------------------------------------ drawing

    fn text(&mut self, s: &str, size: f32, weight: i32, max_w: f32) -> Mask {
        let key = (s.to_string(), (size * 100.0) as u32, weight, max_w.max(1.0) as u32);
        if let Some(m) = self.texts.get(&key) {
            return m.clone();
        }
        let m = text::line(s, Style::dip(size, weight, self.s), max_w);
        if self.texts.len() > 800 {
            self.texts.clear();
        }
        self.texts.insert(key, m.clone());
        m
    }

    fn put(c: &mut Canvas, m: &Mask, x: f32, y: f32, col: Rgba) {
        c.fill_mask(&m.0, m.1, x.round() as isize, y.round() as isize, col);
    }

    fn background(&mut self) -> Canvas {
        let key = (self.w, self.h, (self.s * 100.0) as u32, self.dark);
        if let Some((k, c)) = &self.bg {
            if *k == key {
                return c.clone();
            }
        }
        let t = theme(self.dark);
        let mut c = Canvas::new(self.w, self.h);
        look::tint(&mut c, &t);
        // a soft light along the top, where the toolbar is
        let tb = self.tb() * 1.6;
        let glow = if self.dark { Rgba::WHITE.alpha(0.025) } else { Rgba::WHITE.alpha(0.35) };
        c.fill_round_rect_with(0.0, 0.0, self.w as f32, tb, 0.0, |_, y| glow.fade(1.0 - y / tb));
        self.bg = Some((key, c.clone()));
        c
    }

    /// An app's icon with its soft shadow (as icons on macOS cast), `pad` px around it.
    fn icon_layer(&mut self, i: usize) -> (Canvas, f32) {
        let t = &self.tiles[i];
        let key = (t.id.clone(), (self.s * 100.0) as u32, self.dark);
        let s = self.s;
        let ic = launchview::ICON * s;
        let pad = (10.0 * s).ceil();
        if let Some(c) = self.icon_px.get(&key) {
            return (c.clone(), pad);
        }
        let size = (ic + 2.0 * pad).ceil() as usize;
        let mut c = Canvas::new(size, size);
        let icon = self.icons.get(&t.id).cloned().unwrap_or_else(|| placeholder(&t.id, &t.name, (ic.round() as usize).max(8), s));
        let mut sh = Canvas::new(size, size);
        sh.draw(&icon, pad, pad + 2.0 * s, ic, ic, 0.0, 1.0);
        sh.blur((3.0 * s).round().max(1.0) as usize, 3);
        let m: Vec<u8> = sh.px.iter().map(|p| (p[3] as f32 * if self.dark { 0.55 } else { 0.3 }) as u8).collect();
        c.fill_mask(&m, size, 0, 0, Rgba::BLACK);
        c.draw(&icon, pad, pad, ic, ic, 0.0, 1.0);
        self.icon_px.insert(key, c.clone());
        (c, pad)
    }

    /// The whole window at `now`.
    pub fn render(&mut self, now: Instant) -> Canvas {
        let t = theme(self.dark);
        let s = self.s;
        let tb = self.tb();
        let mut c = self.background();
        let scroll = self.scroll.value_at(now);
        let g = self.grid();
        // ---- the grid
        let mut under = false;
        let order: Vec<usize> = (0..self.tiles.len()).collect();
        for i in order {
            let Some(sp) = self.spots.get(&i) else { continue };
            let since = now.saturating_duration_since(self.since);
            let show = sp.show.value_at(now - sp.delay.min(since)).clamp(0.0, 1.2);
            if show < 0.01 {
                continue;
            }
            let (x, y) = (sp.x.value_at(now), sp.y.value_at(now) - scroll);
            if y > self.h as f32 || y + g.cell_h < 0.0 {
                continue;
            }
            under |= y < tb + 12.0 * s;
            let sel = self.sel.and_then(|k| self.shown.get(k)).is_some_and(|&j| j == i);
            self.draw_tile(&mut c, i, x, y + (1.0 - show.min(1.0)) * 10.0 * s, g.cell_w, show, sel, now, &t);
        }
        // ---- what there is to say instead of apps
        if self.tiles.is_empty() {
            let (cx, cy) = (self.w as f32 / 2.0, tb + (self.h as f32 - tb) / 2.0 - 30.0 * s);
            look::spinner(&mut c, cx, cy, 28.0 * s, t.text2, now.saturating_duration_since(self.since).as_secs_f32());
            let title = if self.state == "Connected" { "Getting the Mac’s apps…".to_string() } else { format!("Connecting to {}…", self.mac) };
            let m = self.text(&title, 17.0, 700, self.w as f32 * 0.8);
            Self::put(&mut c, &m, cx - m.1 as f32 / 2.0, cy + 30.0 * s, t.text);
            let m2 = self.text("The apps appear here as soon as the Mac sends them.", 13.0, 400, self.w as f32 * 0.8);
            Self::put(&mut c, &m2, cx - m2.1 as f32 / 2.0, cy + 30.0 * s + m.2 as f32 + 4.0 * s, t.text2);
        } else if self.shown.is_empty() {
            let (cx, cy) = (self.w as f32 / 2.0, tb + (self.h as f32 - tb) / 2.0 - 40.0 * s);
            look::symbol(&mut c, Symbol::Search, cx, cy, 44.0 * s, t.text3);
            let m = self.text(&format!("No Results for “{}”", self.query), 17.0, 700, self.w as f32 * 0.8);
            Self::put(&mut c, &m, cx - m.1 as f32 / 2.0, cy + 36.0 * s, t.text);
            let m2 = self.text("Check the spelling or try a new search.", 13.0, 400, self.w as f32 * 0.8);
            Self::put(&mut c, &m2, cx - m2.1 as f32 / 2.0, cy + 36.0 * s + m.2 as f32 + 4.0 * s, t.text2);
        }
        // ---- the scroll edge effect: what is under the toolbar softens into it
        if under {
            self.soften_top(&mut c);
        }
        // ---- the toolbar
        self.draw_toolbar(&mut c, now, &t);
        c
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_tile(&mut self, c: &mut Canvas, i: usize, x: f32, y: f32, cw: f32, show: f32, sel: bool, now: Instant, t: &Theme) {
        let s = self.s;
        let ic = launchview::ICON * s;
        let (icx, icy) = (x + cw / 2.0, y + 12.0 * s + ic / 2.0);
        let lift = self.lift.get(&i).map_or(0.0, |a| a.value_at(now));
        let press = if self.pressed == Some(i) || (self.sel.and_then(|k| self.shown.get(k)) == Some(&i) && !self.press.done_at(now)) { self.press.value_at(now) } else { 1.0 };
        let tile = self.tiles[i].clone();
        let dim = if tile.available { 1.0 } else { 0.42 };
        // the selection, as Finder shows it: a soft square behind the icon, the name on accent
        if sel {
            let b = ic + 14.0 * s;
            let col = if self.dark { Rgba::WHITE.alpha(0.11) } else { Rgba::BLACK.alpha(0.075) };
            c.fill_round_rect(icx - b / 2.0, icy - b / 2.0, b, b, 11.0 * s, col.fade(show.min(1.0)));
        }
        let (layer, pad) = self.icon_layer(i);
        let k = (0.86 + 0.14 * show.min(1.0)) * (1.0 + 0.055 * lift) * press;
        let lw = layer.w as f32 * k;
        let lift_y = -2.5 * s * lift;
        let op = show.min(1.0) * dim;
        if (k - 1.0).abs() < 0.002 && lift_y.abs() < 0.05 {
            c.composite(&layer, (icx - ic / 2.0 - pad).round() as isize, (icy - ic / 2.0 - pad).round() as isize, op);
        } else {
            c.draw(&layer, icx - lw / 2.0, icy - lw / 2.0 + lift_y, lw, lw, 0.0, op);
        }
        if press < 0.99 {
            // pressed: the icon darkens a little, as a pressed icon does on the Mac
            let b = ic * k;
            c.fill_round_rect(icx - b / 2.0, icy - b / 2.0, b, b, 14.0 * s * k, Rgba::BLACK.alpha(0.16 * (1.0 - press) / 0.1));
        }
        // the name, in up to two lines (as Finder's icon view)
        let max_w = cw - 14.0 * s;
        let lines = self.lines(&tile.name, max_w);
        let ms: Vec<_> = lines.iter().map(|l| self.text(l, 12.0, 500, max_w)).collect();
        let lh = ms.first().map_or(0.0, |m| m.2 as f32) - 1.0 * s;
        let widest = ms.iter().map(|m| m.1).max().unwrap_or(0) as f32;
        let ly = icy + ic / 2.0 + 8.0 * s;
        let mut fg = if tile.available { t.text } else { t.text3 };
        if sel {
            let (pw, ph) = (widest + 12.0 * s, lh * ms.len() as f32 + 3.0 * s);
            let bg = if self.active { t.accent } else if self.dark { Rgba::WHITE.alpha(0.16) } else { Rgba::BLACK.alpha(0.1) };
            let r = if ms.len() > 1 { 6.0 * s } else { ph / 2.0 };
            c.fill_round_rect(icx - pw / 2.0, ly - 1.0 * s, pw, ph, r, bg.fade(show.min(1.0)));
            if self.active {
                fg = Rgba::WHITE;
            }
        }
        for (n, m) in ms.iter().enumerate() {
            Self::put(c, m, icx - m.1 as f32 / 2.0, ly + n as f32 * lh, fg.fade(show.min(1.0)));
        }
        if tile.open {
            c.fill_circle(icx, ly + lh * ms.len() as f32 + 6.0 * s, 2.0 * s, t.text2.fade(show.min(1.0)));
        }
    }

    fn lines(&mut self, name: &str, max_w: f32) -> Vec<String> {
        let key = (format!("\u{1}{name}"), 1200, 500, max_w as u32);
        if let Some(m) = self.texts.get(&key) {
            return String::from_utf8_lossy(&m.0).split('\n').map(String::from).collect();
        }
        let l = text::wrap(name, Style::dip(12.0, 500, self.s), max_w, 2);
        self.texts.insert(key, (l.join("\n").into_bytes(), 0, 0));
        l
    }

    /// Content under the toolbar blurs and fades into the window's tint (macOS 26's scroll edge).
    fn soften_top(&mut self, c: &mut Canvas) {
        let s = self.s;
        let tb = self.tb();
        let band = (tb + 16.0 * s).ceil() as usize;
        let band = band.min(c.h);
        let bg = self.background();
        let mut soft = c.crop(0, 0, c.w, band);
        soft.blur((7.0 * s).round().max(1.0) as usize, 2);
        for y in 0..band {
            let fy = y as f32;
            // fully softened under the toolbar, easing out below it
            let k = if fy < tb - 8.0 * s { 1.0 } else { let u = ((fy - (tb - 8.0 * s)) / (band as f32 - (tb - 8.0 * s))).clamp(0.0, 1.0); 1.0 - u * u * (3.0 - 2.0 * u) };
            if k <= 0.0 {
                continue;
            }
            let veil = 0.78 * k;
            for x in 0..c.w {
                let i = y * c.w + x;
                let (p, q, b) = (c.px[i], soft.px[i], bg.px[i]);
                let mut o = [0u8; 4];
                for ch in 0..4 {
                    let mixed = p[ch] as f32 * (1.0 - k) + q[ch] as f32 * k;
                    o[ch] = (mixed * (1.0 - veil) + b[ch] as f32 * veil).round() as u8;
                }
                c.px[i] = o;
            }
        }
    }

    /// A Liquid Glass control at (x, y, w, h) with corner `r`, of what is under it now.
    #[allow(clippy::too_many_arguments)]
    fn glass(&mut self, c: &mut Canvas, id: u8, x: f32, y: f32, w: f32, h: f32, r: f32, pressed: bool) {
        let s = self.s;
        let accent = theme(self.dark).accent;
        let mut m = Material::for_kind(Kind::Control, self.dark, accent);
        if pressed {
            m = m.pressed();
        }
        let (wi, hi) = (w.round() as usize, h.round() as usize);
        let reach = glass::margin(&m, s, self.level, wi, hi, r);
        let (bx, by) = (x.round() as isize - reach as isize, y.round() as isize - reach as isize);
        let back = c.crop(bx, by, wi + 2 * reach, hi + 2 * reach);
        let key = {
            // what is behind and how it is drawn decide the picture
            let mut hsh: u64 = 0xcbf2_9ce4_8422_2325;
            for p in back.px.iter().step_by(3) {
                hsh = (hsh ^ u32::from_le_bytes(*p) as u64).wrapping_mul(0x100_0000_01b3);
            }
            hsh ^ ((wi as u64) << 40) ^ ((hi as u64) << 28) ^ (pressed as u64) << 1 ^ (self.dark as u64) ^ ((self.level as u64) << 3)
        };
        // the soft shadow under the glass
        c.shadow(x, y, w, h, r, 10.0 * s, 2.0 * s, Rgba::BLACK.alpha(if self.dark { 0.32 } else { 0.07 }));
        c.shadow(x, y, w, h, r, 2.0 * s, 0.5 * s, Rgba::BLACK.alpha(if self.dark { 0.3 } else { 0.06 }));
        let body = match self.glass.get(&id) {
            Some((k, b)) if *k == key => b.clone(),
            _ => {
                let b = glass::render(Some(&back), reach, wi, hi, r, &m, self.level, s);
                self.glass.insert(id, (key, b.clone()));
                b
            }
        };
        c.composite(&body, x.round() as isize, y.round() as isize, 1.0);
    }

    fn draw_toolbar(&mut self, c: &mut Canvas, _now: Instant, t: &Theme) {
        let s = self.s;
        let tb = self.tb();
        // the window buttons
        let (lx, ly) = self.lights();
        let lights_hot = matches!(self.hot, Hot::Lights(_));
        let down = match self.down {
            Hot::Lights(Some(l)) if self.hot == self.down => Some(l),
            _ => None,
        };
        look::traffic_lights(c, lx, ly, s, self.active, lights_hot, down, self.maximized, self.dark);
        // the Mac's name and how it is connected
        let tx = lx + 2.0 * look::LIGHT_STEP * s + look::LIGHT_D * s / 2.0 + 20.0 * s;
        let (gx, gy, gw, gh) = self.group_rect();
        let room = (gx - tx - 16.0 * s).max(40.0 * s);
        let mac = self.mac.clone();
        let title = self.text(&mac, 15.0, 650, room);
        let connected = self.state == "Connected";
        let apps = self.tiles.iter().filter(|t| t.available && t.id != "desktop").count();
        let mut sub = self.state.clone();
        if !self.route.is_empty() && connected {
            sub = format!("{sub} · {}", self.route);
        }
        if apps > 0 {
            sub = format!("{sub} · {apps} app{}", if apps == 1 { "" } else { "s" });
        }
        let subm = self.text(&sub, 11.5, 450, room - 12.0 * s);
        let total = title.2 as f32 + subm.2 as f32 - 2.0 * s;
        let ty = (tb - total) / 2.0;
        Self::put(c, &title, tx, ty, if self.active { t.text } else { t.text2 });
        let dot = if connected { t.pass } else if self.state.starts_with("Connecting") { t.warn } else { t.fail };
        let sy = ty + title.2 as f32 - 2.0 * s;
        c.fill_circle(tx + 3.5 * s, sy + subm.2 as f32 / 2.0, 3.2 * s, dot);
        Self::put(c, &subm, tx + 11.0 * s, sy, t.text2);
        // the Mac Desktop and Settings, one glass capsule
        let pressed_group = matches!(self.down, Hot::Desktop | Hot::Settings) && self.hot == self.down;
        self.glass(c, 1, gx, gy, gw, gh, gh / 2.0, pressed_group);
        let half = gw / 2.0;
        for (n, (hot, sym)) in [(Hot::Desktop, Symbol::Display), (Hot::Settings, Symbol::Gear)].into_iter().enumerate() {
            let bx = gx + n as f32 * half;
            if self.hot == hot {
                let col = if self.down == hot { t.pressed } else { t.hover };
                let inset = 3.0 * s;
                c.fill_round_rect(bx + inset, gy + inset, half - 2.0 * inset, gh - 2.0 * inset, (gh - 2.0 * inset) / 2.0, col);
            }
            look::symbol(c, sym, bx + half / 2.0, gy + gh / 2.0, 16.0 * s, t.text);
        }
        c.fill_round_rect(gx + half - 0.5 * s, gy + 9.0 * s, (0.75 * s).max(1.0), gh - 18.0 * s, 0.0, t.divider);
        // the search field
        let (sx, sy2, sw, sh) = self.search_rect();
        self.glass(c, 2, sx, sy2, sw, sh, sh / 2.0, false);
        if self.searching && !self.query.is_empty() {
            c.stroke_round_rect_with(sx - 2.0 * s, sy2 - 2.0 * s, sw + 4.0 * s, sh + 4.0 * s, sh / 2.0 + 2.0 * s, 3.0 * s, |_, _| t.accent.alpha(0.42));
        }
        look::symbol(c, Symbol::Search, sx + 17.0 * s, sy2 + sh / 2.0, 14.0 * s, t.text2);
        let qx = sx + 32.0 * s;
        let q = if self.query.is_empty() { self.text("Search", 13.0, 400, sw - 48.0 * s) } else { let q = self.query.clone(); self.text(&q, 13.0, 400, sw - 58.0 * s) };
        let qy = sy2 + (sh - q.2 as f32) / 2.0;
        Self::put(c, &q, qx, qy, if self.query.is_empty() { t.text3 } else { t.text });
        if !self.query.is_empty() {
            if self.searching {
                c.fill_round_rect(qx + q.1 as f32 + 1.0 * s, sy2 + 9.0 * s, (1.5 * s).max(1.0), sh - 18.0 * s, 0.75 * s, t.accent);
            }
            let (cx, cy) = self.clear_spot();
            let col = if self.hot == Hot::Clear { t.text2 } else { t.text3 };
            look::symbol(c, Symbol::Clear, cx, cy, 15.0 * s, col);
        }
    }
}

/// An icon to show until the Mac's comes: a rounded square in a colour of the name, its first
/// letter in white (the Mac Desktop: a display).
fn placeholder(id: &str, name: &str, size: usize, s: f32) -> Canvas {
    let mut c = Canvas::new(size, size);
    let f = size as f32;
    let hue = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    let (top, bottom) = if id == "desktop" { (Rgba::rgb(0x5a, 0xc8, 0xfa), Rgba::rgb(0x0a, 0x6c, 0xf0)) } else { (hsv(hue as f32, 0.45, 0.98), hsv(hue as f32, 0.75, 0.82)) };
    let inset = f * 0.09;
    c.fill_round_rect_with(inset, inset, f - 2.0 * inset, f - 2.0 * inset, f * 0.2, |_, y| top.lerp(bottom, (y / f).clamp(0.0, 1.0)));
    if id == "desktop" {
        look::symbol(&mut c, Symbol::Display, f / 2.0, f / 2.0, f * 0.46, Rgba::WHITE);
    } else if let Some(ch) = name.chars().next() {
        let m = text::line(&ch.to_uppercase().to_string(), Style::dip(f / s * 0.42, 600, s), f);
        c.fill_mask(&m.0, m.1, ((f - m.1 as f32) / 2.0).round() as isize, ((f - m.2 as f32) / 2.0).round() as isize, Rgba::WHITE);
    }
    c
}

fn hsv(h: f32, s: f32, v: f32) -> Rgba {
    let c = v * s;
    let hp = (h / 60.0) % 6.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Rgba { r: r + m, g: g + m, b: b + m, a: 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> Vec<(String, String, bool)> {
        ["desktop:Mac Desktop", "safari:Safari", "notes:Notes", "textedit:TextEdit", "xcode:Xcode", "terminal:Terminal", "music:Music", "photos:Photos", "maps:Maps", "calendar:Calendar", "old:Old App"]
            .iter()
            .map(|s| {
                let (id, name) = s.split_once(':').unwrap();
                (id.to_string(), name.to_string(), id != "old")
            })
            .collect()
    }

    fn view() -> View {
        let mut v = View::new();
        v.set_window(920, 640, 1.0, false, Level::Full);
        v.set_apps(&apps());
        v.set_connection("Studio", "Connected", "This network");
        v
    }

    #[test]
    fn clicking_an_app_opens_it_and_the_toolbar_does_what_it_shows() {
        let mut v = view();
        let later = Instant::now() + Duration::from_secs(5);
        let _ = v.render(later);
        let g = v.grid();
        let (x, y, w, _) = g.cell(1, 0.0);
        let (cx, cy) = (x + w / 2.0, y + 40.0);
        v.mouse_down(cx, cy);
        assert_eq!(v.mouse_up(cx, cy), Some(Act::Launch("safari".into())));
        // the lights, the Mac Desktop, Settings
        let (lx, ly) = v.lights();
        v.mouse_down(lx, ly);
        assert_eq!(v.mouse_up(lx, ly), Some(Act::Window(Light::Close)));
        v.mouse_down(lx + 40.0, ly);
        assert_eq!(v.mouse_up(lx + 40.0, ly), Some(Act::Window(Light::Zoom)));
        let (gx, gy, gw, gh) = v.group_rect();
        v.mouse_down(gx + 5.0, gy + gh / 2.0);
        assert_eq!(v.mouse_up(gx + 5.0, gy + gh / 2.0), Some(Act::Launch("desktop".into())));
        v.mouse_down(gx + gw - 5.0, gy + gh / 2.0);
        assert_eq!(v.mouse_up(gx + gw - 5.0, gy + gh / 2.0), Some(Act::Settings));
        // an app that is not installed does nothing
        let k = v.shown.iter().position(|&i| v.tiles[i].id == "old").unwrap();
        let (x, y, w, _) = g.cell(k, 0.0);
        v.mouse_down(x + w / 2.0, y + 40.0);
        assert_eq!(v.mouse_up(x + w / 2.0, y + 40.0), None);
        // the toolbar's empty part moves the window; its controls do not
        assert!(v.is_caption(400.0, 10.0));
        assert!(!v.is_caption(lx, ly));
        assert!(!v.is_caption(gx + 5.0, gy + 5.0));
        assert!(!v.is_caption(400.0, 300.0));
    }

    #[test]
    fn typing_searches_and_the_grid_flows() {
        let mut v = view();
        for c in "te".chars() {
            v.char(c);
        }
        assert_eq!(v.shown.iter().map(|&i| v.tiles[i].id.as_str()).collect::<Vec<_>>(), vec!["notes", "textedit", "terminal"]);
        assert!(v.busy(Instant::now()), "the tiles move to their new places");
        assert!(!v.busy(Instant::now() + Duration::from_secs(3)));
        assert_eq!(v.key(Key::Enter), Some(Act::Launch("notes".into())));
        v.key(Key::Right);
        assert_eq!(v.key(Key::Enter), Some(Act::Launch("textedit".into())));
        v.char('\u{8}');
        v.char('\u{8}');
        assert_eq!(v.shown.len(), 11);
        v.char('z');
        assert!(v.shown.is_empty());
        let _ = v.render(Instant::now() + Duration::from_secs(2));
        v.key(Key::Escape);
        assert!(v.query().is_empty());
    }

    #[test]
    fn it_draws_at_every_scale_and_in_both_appearances() {
        for (s, dark) in [(1.0, false), (1.5, true), (2.0, false)] {
            let mut v = view();
            v.set_window((920.0 * s) as usize, (640.0 * s) as usize, s, dark, Level::Full);
            let c = v.render(Instant::now() + Duration::from_secs(3));
            assert_eq!((c.w, c.h), ((920.0 * s) as usize, (640.0 * s) as usize));
            assert!(c.px.iter().all(|p| p[3] == 255), "opaque");
        }
        // no apps yet, and a tiny window
        let mut v = View::new();
        v.set_window(300, 200, 1.0, false, Level::Off);
        let _ = v.render(Instant::now());
    }
}

/// Pictures of the launcher (RM_PREVIEW=dir cargo test -p rm-viewer launchui::preview).
#[cfg(test)]
mod preview {
    use super::*;

    /// Icons in the manner of the Mac's (for the pictures only).
    fn icon(id: &str, n: usize) -> Canvas {
        let mut c = Canvas::new(n, n);
        let f = n as f32;
        let i = f * 0.09;
        let (a, b) = match id {
            "safari" => (Rgba::rgb(0x6c, 0xd3, 0xff), Rgba::rgb(0x1a, 0x6f, 0xf0)),
            "notes" => (Rgba::rgb(0xff, 0xe1, 0x6b), Rgba::rgb(0xf7, 0xc1, 0x2c)),
            "textedit" => (Rgba::rgb(0xff, 0xff, 0xff), Rgba::rgb(0xe8, 0xe8, 0xec)),
            "xcode" => (Rgba::rgb(0x4f, 0xb4, 0xff), Rgba::rgb(0x14, 0x5a, 0xd8)),
            "terminal" => (Rgba::rgb(0x3a, 0x3a, 0x40), Rgba::rgb(0x12, 0x12, 0x16)),
            "music" => (Rgba::rgb(0xff, 0x6b, 0x81), Rgba::rgb(0xf5, 0x2d, 0x4a)),
            "photos" => (Rgba::rgb(0xff, 0xff, 0xff), Rgba::rgb(0xf2, 0xf2, 0xf4)),
            "maps" => (Rgba::rgb(0x8c, 0xe0, 0x8a), Rgba::rgb(0x3c, 0xb3, 0x5c)),
            "calendar" => (Rgba::rgb(0xff, 0xff, 0xff), Rgba::rgb(0xf0, 0xf0, 0xf2)),
            _ => (Rgba::rgb(0xb0, 0xb4, 0xbc), Rgba::rgb(0x80, 0x86, 0x90)),
        };
        c.fill_round_rect_with(i, i, f - 2.0 * i, f - 2.0 * i, f * 0.2, |_, y| a.lerp(b, (y / f).clamp(0.0, 1.0)));
        let k = f / 64.0;
        match id {
            "safari" => {
                c.arc(f / 2.0, f / 2.0, 19.0 * k, 2.0 * k, 0.0, std::f32::consts::TAU, |_| Rgba::WHITE.alpha(0.9));
                c.fill_capsule(f / 2.0 - 9.0 * k, f / 2.0 + 9.0 * k, f / 2.0 + 9.0 * k, f / 2.0 - 9.0 * k, 3.0 * k, Rgba::rgb(0xff, 0x3b, 0x30));
            }
            "notes" => {
                c.fill_round_rect(i, i, f - 2.0 * i, 13.0 * k, f * 0.2, Rgba::rgb(0xfb, 0xfb, 0xf6));
                for r in 0..4 {
                    c.fill_round_rect(14.0 * k, 26.0 * k + r as f32 * 8.0 * k, 36.0 * k, 1.2 * k, 0.0, Rgba::rgb(0xd6, 0xb0, 0x3a));
                }
            }
            "textedit" => {
                for r in 0..5 {
                    c.fill_round_rect(16.0 * k, 17.0 * k + r as f32 * 7.0 * k, (32.0 - (r % 2) as f32 * 9.0) * k, 2.0 * k, 1.0 * k, Rgba::rgb(0x9a, 0x9a, 0xa2));
                }
            }
            "xcode" => {
                c.fill_capsule(20.0 * k, 44.0 * k, 44.0 * k, 20.0 * k, 4.0 * k, Rgba::WHITE);
                c.fill_capsule(22.0 * k, 22.0 * k, 30.0 * k, 30.0 * k, 3.0 * k, Rgba::WHITE.alpha(0.8));
            }
            "terminal" => {
                c.fill_capsule(16.0 * k, 22.0 * k, 24.0 * k, 28.0 * k, 2.0 * k, Rgba::rgb(0xe8, 0xe8, 0xe8));
                c.fill_capsule(24.0 * k, 28.0 * k, 16.0 * k, 34.0 * k, 2.0 * k, Rgba::rgb(0xe8, 0xe8, 0xe8));
                c.fill_capsule(28.0 * k, 36.0 * k, 40.0 * k, 36.0 * k, 2.0 * k, Rgba::rgb(0xe8, 0xe8, 0xe8));
            }
            "music" => {
                c.fill_circle(26.0 * k, 42.0 * k, 6.0 * k, Rgba::WHITE);
                c.fill_circle(42.0 * k, 38.0 * k, 6.0 * k, Rgba::WHITE);
                c.fill_capsule(31.0 * k, 42.0 * k, 31.0 * k, 18.0 * k, 1.6 * k, Rgba::WHITE);
                c.fill_capsule(47.0 * k, 38.0 * k, 47.0 * k, 15.0 * k, 1.6 * k, Rgba::WHITE);
                c.fill_capsule(31.0 * k, 18.0 * k, 47.0 * k, 15.0 * k, 2.4 * k, Rgba::WHITE);
            }
            "photos" => {
                let cols = [(0xff, 0x95, 0x00), (0xff, 0xcc, 0x00), (0x34, 0xc7, 0x59), (0x5a, 0xc8, 0xfa), (0x00, 0x7a, 0xff), (0xaf, 0x52, 0xde), (0xff, 0x2d, 0x55), (0xff, 0x3b, 0x30)];
                for (n, (r, g, b)) in cols.iter().enumerate() {
                    let a = n as f32 / 8.0 * std::f32::consts::TAU;
                    let (sn, cs) = a.sin_cos();
                    c.fill_capsule(f / 2.0 + cs * 6.0 * k, f / 2.0 + sn * 6.0 * k, f / 2.0 + cs * 15.0 * k, f / 2.0 + sn * 15.0 * k, 6.0 * k, Rgba::rgb(*r, *g, *b).alpha(0.82));
                }
            }
            "maps" => {
                c.fill_capsule(10.0 * k, 50.0 * k, 54.0 * k, 14.0 * k, 4.0 * k, Rgba::rgb(0xff, 0xd7, 0x5e));
                c.fill_circle(40.0 * k, 26.0 * k, 6.0 * k, Rgba::rgb(0x0a, 0x84, 0xff));
            }
            "calendar" => {
                let m = text::line("17", Style::dip(30.0 * k, 400, 1.0), f);
                c.fill_mask(&m.0, m.1, ((f - m.1 as f32) / 2.0) as isize, (24.0 * k) as isize, Rgba::rgb(0x1d, 0x1d, 0x1f));
                let d = text::line("FRI", Style::dip(10.0 * k, 600, 1.0), f);
                c.fill_mask(&d.0, d.1, ((f - d.1 as f32) / 2.0) as isize, (11.0 * k) as isize, Rgba::rgb(0xff, 0x3b, 0x30));
            }
            _ => {}
        }
        c
    }

    fn save(c: &Canvas, path: &std::path::Path) {
        let mut ppm = format!("P6 {} {} 255\n", c.w, c.h).into_bytes();
        for p in &c.px {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(path, ppm).unwrap();
    }

    #[test]
    fn pictures() {
        let Some(dir) = std::env::var_os("RM_PREVIEW") else { return };
        let dir = std::path::PathBuf::from(dir);
        let s: f32 = std::env::var("RM_PREVIEW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
        for dark in [false, true] {
            let mut v = View::new();
            v.set_window((920.0 * s) as usize, (600.0 * s) as usize, s, dark, Level::Full);
            v.set_connection("Minh’s MacBook Pro", "Connected", "This network");
            let mut apps = super::tests_apps();
            if !dark {
                for n in ["App Store", "Books", "Contacts", "FaceTime", "Mail", "Messages", "News", "Podcasts", "Preview", "Reminders", "Shortcuts", "Stocks", "System Settings", "Weather", "Audio MIDI Setup", "Activity Monitor"] {
                    apps.push((n.to_lowercase().replace(' ', ""), n.to_string(), true));
                }
            }
            v.set_apps(&apps);
            for (id, _, _) in super::tests_apps() {
                if id != "desktop" && id != "old" {
                    v.set_icon_canvas(&id, icon(&id, 128));
                }
            }
            v.set_open(&["safari".into(), "notes".into()]);
            let later = Instant::now() + Duration::from_secs(5);
            // the pointer over Notes, Safari selected
            v.sel = Some(1);
            let g = v.grid();
            let (x, y, w, _) = g.cell(2, 0.0);
            v.mouse_move(x + w / 2.0, y + 40.0 * s);
            let c = v.render(later);
            save(&c, &dir.join(format!("launcher-{}.ppm", if dark { "dark" } else { "light" })));
            // scrolled a little under the toolbar, searching
            if !dark {
                v.mouse_move(-1.0, -1.0);
                v.set_window((920.0 * s) as usize, (430.0 * s) as usize, s, dark, Level::Full);
                v.wheel(-150);
                let c = v.render(later);
                save(&c, &dir.join("launcher-scrolled.ppm"));
                for ch in "o".chars() {
                    v.char(ch);
                }
                let c = v.render(later + Duration::from_secs(5));
                save(&c, &dir.join("launcher-search.ppm"));
            }
        }
        let mut v = View::new();
        v.set_window((920.0 * s) as usize, (600.0 * s) as usize, s, false, Level::Full);
        v.set_connection("Studio", "Connecting…", "");
        let c = v.render(Instant::now() + Duration::from_millis(300));
        save(&c, &dir.join("launcher-connecting.ppm"));
    }
}

#[cfg(test)]
fn tests_apps() -> Vec<(String, String, bool)> {
    ["desktop:Mac Desktop", "safari:Safari", "notes:Notes", "textedit:TextEdit", "xcode:Xcode", "terminal:Terminal", "music:Music", "photos:Photos", "maps:Maps", "calendar:Calendar", "old:Old App"]
        .iter()
        .map(|s| {
            let (id, name) = s.split_once(':').unwrap();
            (id.to_string(), name.to_string(), id != "old")
        })
        .collect()
}
