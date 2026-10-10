//! The Settings window as it looks and behaves, after System Settings on macOS 26: the window
//! buttons, a sidebar of Liquid Glass with the sections (each with its coloured icon), and the
//! section's settings in rounded groups: menus that open as the Mac's do (the choice ticked),
//! switches whose knob springs across, a volume slider, a word under a group where one helps.
//! A change applies at once, as on the Mac: there is no Save.
//!
//! Portable: it draws into a Canvas and takes plain input; settings_ui.rs puts it in a window.

use crate::glass::{self, Kind, Level, Material};
use crate::look::{self, theme, Light, Symbol};
use crate::motion::{Anim, Curve};
use crate::paint::{Canvas, Rgba};
use crate::settings::{Settings, BITRATES, DECODERS, DESKTOP_SCALES, FPS, GLASS_LEVELS, KEYBOARD_MODES, MOTION_LEVELS, QUALITY, WINDOW_FRAMES};
use crate::text::{self, Style};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The window's size (DIPs).
pub const W: f32 = 720.0;
pub const H: f32 = 560.0;
const SIDEBAR: f32 = 220.0;
const INSET: f32 = 10.0;
const ROW: f32 = 44.0;
const GROUP_R: f32 = 12.0;

/// One setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Fps,
    Bitrate,
    Sharpness,
    Workspace,
    DesktopScale,
    Decoder,
    Pacing,
    Audio,
    Volume,
    Keyboard,
    Cursor,
    Glass,
    Motion,
    Frame,
    Fusion,
}

enum Control {
    Menu,
    Switch,
    Slider,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Fps => "Frame rate",
            Field::Bitrate => "Bitrate",
            Field::Sharpness => "Sharpness",
            Field::Workspace => "Mac screen size",
            Field::DesktopScale => "Mac Desktop scale",
            Field::Decoder => "Video decoder",
            Field::Pacing => "Frame pacing",
            Field::Audio => "Play the Mac's sound here",
            Field::Volume => "Volume",
            Field::Keyboard => "Keyboard",
            Field::Cursor => "Use this PC's pointer",
            Field::Glass => "Glass",
            Field::Motion => "Animations",
            Field::Frame => "Mac windows",
            Field::Fusion => "Desktop Fusion",
        }
    }

    fn control(self) -> Control {
        match self {
            Field::Pacing | Field::Audio | Field::Cursor | Field::Fusion => Control::Switch,
            Field::Volume => Control::Slider,
            _ => Control::Menu,
        }
    }
}

/// A section of the sidebar: its name, icon and colour, and its groups (fields, a word under).
struct Section {
    name: &'static str,
    icon: Symbol,
    colour: Rgba,
    groups: &'static [(&'static [Field], &'static str)],
}

const SECTIONS: [Section; 5] = [
    Section {
        name: "Video",
        icon: Symbol::Display,
        colour: Rgba::rgb(0x0a, 0x84, 0xff),
        groups: &[
            (&[Field::Fps, Field::Bitrate, Field::Sharpness], "Automatic bitrate follows the connection. Ultra draws the Mac's windows at twice the pixels and scales them down here: the sharpest text."),
            (&[Field::Workspace, Field::DesktopScale], ""),
            (&[Field::Decoder, Field::Pacing], "Frame pacing shows pictures on the display's refresh: smoother motion, up to one frame more delay. The decoder and pacing apply from the next connection."),
        ],
    },
    Section { name: "Sound", icon: Symbol::Speaker, colour: Rgba::rgb(0xff, 0x37, 0x5f), groups: &[(&[Field::Audio, Field::Volume], "Ctrl+Alt+Shift+M mutes and unmutes at any time.")] },
    Section {
        name: "Keyboard & Pointer",
        icon: Symbol::Keyboard,
        colour: Rgba::rgb(0x8e, 0x8e, 0x93),
        groups: &[(&[Field::Keyboard, Field::Cursor], "Windows: Ctrl acts as ⌘ Command. Mac: the keys of a Mac keyboard (Win is ⌘). Fusion: Windows' shortcuts and text keys too. Ctrl+Alt+Shift+C swaps the pointer.")],
    },
    Section {
        name: "Appearance",
        icon: Symbol::Appearance,
        colour: Rgba::rgb(0x5e, 0x5c, 0xe6),
        groups: &[(&[Field::Frame], "The app's menus sit in each window's title bar, beside its buttons. As the Mac draws them (experimental): the Mac's own title bars, and its menu bar at the top of the screen. Applies from the next connection."), (&[Field::Glass, Field::Motion], "")],
    },
    Section { name: "Desktop Fusion", icon: Symbol::Dock, colour: Rgba::rgb(0xaf, 0x52, 0xde), groups: &[(&[Field::Fusion], "Experimental: the Mac's own Dock at the bottom of this PC's screen, on this PC's wallpaper (the Mac takes it while connected and gets its own back after).")] },
];

/// What the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hot {
    None,
    Lights(Option<Light>),
    Section(usize),
    Field(Field),
    /// an item of the open menu
    Item(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// the settings changed: apply them
    Changed(Settings),
    Window(Light),
}

/// A menu open on a field: its items and the one under the pointer.
struct Menu {
    field: Field,
    items: Vec<String>,
    hover: Option<usize>,
    open: Anim,
}

const SPRING: Curve = Curve::Spring { response: 0.32, damping: 0.78 };

type Mask = (Vec<u8>, usize, usize);
/// x, y, width, height (px).
type Rect = (f32, f32, f32, f32);
/// Each field's row, and each group's box with the lines of its note.
type Layout = (Vec<(Field, Rect)>, Vec<(Rect, Vec<String>)>);

pub struct SettingsView {
    pub settings: Settings,
    workspaces: Vec<String>,
    section: usize,
    hot: Hot,
    down: Hot,
    menu: Option<Menu>,
    knobs: HashMap<Field, Anim>,
    dragging: bool,
    w: usize,
    h: usize,
    s: f32,
    dark: bool,
    active: bool,
    level: Level,
    texts: HashMap<(String, u32, i32, u32), Mask>,
    glass: Option<(u64, Canvas)>,
}

/// The part of an option shown on its menu button: before " — " or " (".
fn short(s: &str) -> &str {
    let s = s.split(" — ").next().unwrap_or(s);
    s.split(" (").next().unwrap_or(s)
}

impl SettingsView {
    /// `workspaces`: the Mac screen sizes offered (worked out from this PC's screen).
    pub fn new(settings: Settings, workspaces: Vec<String>) -> SettingsView {
        let mut knobs = HashMap::new();
        for f in [Field::Pacing, Field::Audio, Field::Cursor, Field::Fusion] {
            knobs.insert(f, Anim::at(if Self::switch_of(&settings, f) { 1.0 } else { 0.0 }));
        }
        SettingsView { settings, workspaces, section: 0, hot: Hot::None, down: Hot::None, menu: None, knobs, dragging: false, w: W as usize, h: H as usize, s: 1.0, dark: false, active: true, level: Level::Full, texts: HashMap::new(), glass: None }
    }

    pub fn set_window(&mut self, w: usize, h: usize, s: f32, dark: bool, level: Level) {
        if (s - self.s).abs() > 0.001 || dark != self.dark {
            self.texts.clear();
        }
        (self.w, self.h, self.s, self.dark, self.level) = (w.max(1), h.max(1), s.max(0.5), dark, level);
    }

    pub fn set_active(&mut self, a: bool) {
        self.active = a;
    }

    pub fn section(&self) -> usize {
        self.section
    }

    // ------------------------------------------------------------------ the settings

    fn switch_of(s: &Settings, f: Field) -> bool {
        match f {
            Field::Pacing => s.pacing,
            Field::Audio => s.audio,
            Field::Cursor => s.local_cursor,
            Field::Fusion => s.dock,
            _ => false,
        }
    }

    fn options(&self, f: Field) -> Vec<String> {
        let own = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        match f {
            Field::Fps => FPS.iter().map(|f| format!("{f} fps")).collect(),
            Field::Bitrate => BITRATES.iter().map(|b| if *b == 0 { "Automatic (follows the connection)".into() } else { format!("{b} Mbit/s") }).collect(),
            Field::Sharpness => own(&QUALITY),
            Field::Workspace => self.workspaces.clone(),
            Field::DesktopScale => own(&DESKTOP_SCALES),
            Field::Decoder => own(&DECODERS),
            Field::Keyboard => own(&KEYBOARD_MODES),
            Field::Glass => own(&GLASS_LEVELS),
            Field::Motion => own(&MOTION_LEVELS),
            Field::Frame => own(&WINDOW_FRAMES),
            _ => vec![],
        }
    }

    fn chosen(&self, f: Field) -> usize {
        let s = &self.settings;
        match f {
            Field::Fps => FPS.iter().position(|x| *x == s.fps).unwrap_or(1),
            Field::Bitrate => BITRATES.iter().position(|x| *x == s.bitrate_mbps).unwrap_or(0),
            Field::Sharpness => s.quality as usize,
            Field::Workspace => s.workspace as usize,
            Field::DesktopScale => s.desktop_2x as usize,
            Field::Decoder => s.decoder as usize,
            Field::Keyboard => s.keyboard as usize,
            Field::Glass => s.glass as usize,
            Field::Motion => s.motion as usize,
            Field::Frame => s.frame as usize,
            _ => 0,
        }
    }

    fn choose(&mut self, f: Field, i: usize) {
        let s = &mut self.settings;
        match f {
            Field::Fps => s.fps = FPS[i.min(FPS.len() - 1)],
            Field::Bitrate => s.bitrate_mbps = BITRATES[i.min(BITRATES.len() - 1)],
            Field::Sharpness => s.quality = i.min(QUALITY.len() - 1) as u8,
            Field::Workspace => s.workspace = i.min(self.workspaces.len().max(1) - 1) as u8,
            Field::DesktopScale => s.desktop_2x = i == 1,
            Field::Decoder => s.decoder = i.min(DECODERS.len() - 1) as u8,
            Field::Keyboard => s.keyboard = i.min(KEYBOARD_MODES.len() - 1) as u8,
            Field::Glass => s.glass = i.min(GLASS_LEVELS.len() - 1) as u8,
            Field::Motion => s.motion = i.min(MOTION_LEVELS.len() - 1) as u8,
            Field::Frame => s.frame = i.min(WINDOW_FRAMES.len() - 1) as u8,
            _ => {}
        }
    }

    fn toggle(&mut self, f: Field) {
        let s = &mut self.settings;
        let v = match f {
            Field::Pacing => {
                s.pacing = !s.pacing;
                s.pacing
            }
            Field::Audio => {
                s.audio = !s.audio;
                s.audio
            }
            Field::Cursor => {
                s.local_cursor = !s.local_cursor;
                s.local_cursor
            }
            Field::Fusion => {
                s.dock = !s.dock;
                s.dock
            }
            _ => return,
        };
        self.knobs.entry(f).or_insert_with(|| Anim::at(0.0)).retarget(if v { 1.0 } else { 0.0 }, Duration::from_millis(400), SPRING);
    }

    // ------------------------------------------------------------------ geometry (px)

    fn p(&self, v: f32) -> f32 {
        v * self.s
    }

    fn lights(&self) -> (f32, f32) {
        (self.p(INSET + 18.0 + look::LIGHT_D / 2.0), self.p(INSET + 22.0))
    }

    fn sidebar(&self) -> (f32, f32, f32, f32) {
        (self.p(INSET), self.p(INSET), self.p(SIDEBAR), self.h as f32 - self.p(2.0 * INSET))
    }

    fn section_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (x, y, w, _) = self.sidebar();
        (x + self.p(10.0), y + self.p(56.0) + i as f32 * self.p(34.0), w - self.p(20.0), self.p(30.0))
    }

    fn content_x(&self) -> f32 {
        self.p(INSET + SIDEBAR + 24.0)
    }

    fn content_w(&self) -> f32 {
        self.w as f32 - self.content_x() - self.p(24.0)
    }

    /// Each field of the section shown: its row (x, y, w, h); and each group's box and note.
    fn layout(&mut self) -> Layout {
        let (x, w) = (self.content_x(), self.content_w());
        let mut y = self.p(70.0);
        let mut rows = vec![];
        let mut groups = vec![];
        for (fields, note) in SECTIONS[self.section].groups {
            let top = y;
            for f in fields.iter() {
                rows.push((*f, (x, y, w, self.p(ROW))));
                y += self.p(ROW);
            }
            let lines = if note.is_empty() { vec![] } else { text::wrap(note, Style::dip(11.5, 400, self.s), w - self.p(16.0), 4) };
            groups.push(((x, top, w, y - top), lines.clone()));
            y += self.p(8.0) + lines.len() as f32 * self.p(15.0) + self.p(if lines.is_empty() { 10.0 } else { 16.0 });
        }
        (rows, groups)
    }

    /// The open menu's box (x, y, w, h) and its row height.
    fn menu_rect(&mut self) -> Option<((f32, f32, f32, f32), f32)> {
        let m = self.menu.as_ref()?;
        let (field, n) = (m.field, m.items.len());
        let widest = m.items.iter().map(|i| text::width(i, Style::dip(13.0, 400, self.s))).fold(0.0f32, f32::max);
        let (rows, _) = self.layout();
        let (_, (rx, ry, rw, rh)) = *rows.iter().find(|(f, _)| *f == field)?;
        let ih = self.p(24.0);
        let mw = (widest + self.p(48.0)).max(self.p(180.0)).min(self.w as f32 - self.p(40.0));
        let mh = ih * n as f32 + self.p(10.0);
        let mx = (rx + rw - mw).max(self.p(10.0));
        let below = ry + rh - self.p(4.0);
        let my = if below + mh < self.h as f32 - self.p(10.0) { below } else { (ry - mh + self.p(4.0)).max(self.p(10.0)) };
        Some(((mx, my, mw, mh), ih))
    }

    fn slider_track(&self, row: (f32, f32, f32, f32)) -> (f32, f32, f32) {
        let (x, y, w, h) = row;
        let tw = self.p(200.0).min(w * 0.5);
        (x + w - self.p(16.0) - tw, y + h / 2.0, tw)
    }

    fn hit(&mut self, x: f32, y: f32) -> Hot {
        let inside = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        if let Some(((mx, my, mw, mh), ih)) = self.menu_rect() {
            if inside((mx, my, mw, mh)) {
                let i = ((y - my - self.p(5.0)) / ih).floor();
                let n = self.menu.as_ref().map_or(0, |m| m.items.len());
                return if i >= 0.0 && (i as usize) < n { Hot::Item(i as usize) } else { Hot::None };
            }
            return Hot::None;
        }
        let (lx, ly) = self.lights();
        if y < self.p(44.0) {
            if let Some(l) = look::light_at(x, y, lx, ly, self.s) {
                return Hot::Lights((l != Light::Zoom).then_some(l));
            }
        }
        for i in 0..SECTIONS.len() {
            if inside(self.section_rect(i)) {
                return Hot::Section(i);
            }
        }
        let (rows, _) = self.layout();
        for (f, r) in rows {
            if inside(r) {
                return Hot::Field(f);
            }
        }
        Hot::None
    }

    /// The window is moved by dragging what is not a control.
    pub fn is_caption(&mut self, x: f32, y: f32) -> bool {
        self.menu.is_none() && self.hit(x, y) == Hot::None
    }

    // ------------------------------------------------------------------ input

    fn volume_at(&mut self, x: f32) {
        let (rows, _) = self.layout();
        if let Some((_, r)) = rows.iter().find(|(f, _)| *f == Field::Volume) {
            let (tx, _, tw) = self.slider_track(*r);
            self.settings.volume = (((x - tx) / tw).clamp(0.0, 1.0) * 100.0).round() as u8;
        }
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) -> Option<Act> {
        if self.dragging {
            self.volume_at(x);
            return Some(Act::Changed(self.settings));
        }
        self.hot = self.hit(x, y);
        if let (Some(m), Hot::Item(i)) = (self.menu.as_mut(), self.hot) {
            m.hover = Some(i);
        }
        None
    }

    pub fn mouse_leave(&mut self) {
        if !self.dragging {
            self.hot = Hot::None;
        }
    }

    pub fn mouse_down(&mut self, x: f32, y: f32) -> Option<Act> {
        self.hot = self.hit(x, y);
        self.down = self.hot;
        if self.menu.is_some() && !matches!(self.hot, Hot::Item(_)) {
            // a click outside an open menu closes it, and does nothing else
            self.menu = None;
            self.down = Hot::None;
            return None;
        }
        if self.hot == Hot::Field(Field::Volume) {
            self.dragging = true;
            self.volume_at(x);
            return Some(Act::Changed(self.settings));
        }
        None
    }

    pub fn mouse_up(&mut self, x: f32, y: f32) -> Option<Act> {
        if std::mem::take(&mut self.dragging) {
            self.volume_at(x);
            self.down = Hot::None;
            return Some(Act::Changed(self.settings));
        }
        let down = std::mem::replace(&mut self.down, Hot::None);
        self.hot = self.hit(x, y);
        if down != self.hot {
            return None;
        }
        match down {
            Hot::Lights(Some(l)) => Some(Act::Window(l)),
            Hot::Section(i) => {
                self.section = i;
                None
            }
            Hot::Item(i) => {
                let f = self.menu.take()?.field;
                self.choose(f, i);
                Some(Act::Changed(self.settings))
            }
            Hot::Field(f) => match f.control() {
                Control::Switch => {
                    self.toggle(f);
                    Some(Act::Changed(self.settings))
                }
                Control::Menu => {
                    let items = self.options(f);
                    let hover = Some(self.chosen(f));
                    self.menu = Some(Menu { field: f, items, hover, open: Anim::new(0.0, 1.0, Duration::from_millis(200), Curve::Decelerate) });
                    None
                }
                Control::Slider => None,
            },
            _ => None,
        }
    }

    /// Up and Down move between the sections (or in an open menu), Enter chooses, Escape closes
    /// the menu.
    pub fn key_up_down(&mut self, down: bool) {
        match self.menu.as_mut() {
            Some(m) => {
                let n = m.items.len();
                let i = m.hover.unwrap_or(0);
                m.hover = Some(if down { (i + 1).min(n.saturating_sub(1)) } else { i.saturating_sub(1) });
            }
            None => self.section = if down { (self.section + 1).min(SECTIONS.len() - 1) } else { self.section.saturating_sub(1) },
        }
    }

    pub fn key_enter(&mut self) -> Option<Act> {
        let m = self.menu.take()?;
        self.choose(m.field, m.hover?);
        Some(Act::Changed(self.settings))
    }

    /// Escape: true when it closed a menu (else the window may close).
    pub fn key_escape(&mut self) -> bool {
        self.menu.take().is_some()
    }

    pub fn busy(&self, now: Instant) -> bool {
        self.knobs.values().any(|a| !a.done_at(now)) || self.menu.as_ref().is_some_and(|m| !m.open.done_at(now))
    }

    // ------------------------------------------------------------------ drawing

    fn text(&mut self, s: &str, size: f32, weight: i32, max_w: f32) -> Mask {
        let key = (s.to_string(), (size * 100.0) as u32, weight, max_w.max(1.0) as u32);
        if let Some(m) = self.texts.get(&key) {
            return m.clone();
        }
        let m = text::line(s, Style::dip(size, weight, self.s), max_w);
        if self.texts.len() > 600 {
            self.texts.clear();
        }
        self.texts.insert(key, m.clone());
        m
    }

    fn put(c: &mut Canvas, m: &Mask, x: f32, y: f32, col: Rgba) {
        c.fill_mask(&m.0, m.1, x.round() as isize, y.round() as isize, col);
    }

    pub fn render(&mut self, now: Instant) -> Canvas {
        let t = theme(self.dark);
        let s = self.s;
        let mut c = Canvas::new(self.w, self.h);
        look::tint(&mut c, &t);
        // the sidebar: a pane of glass over the window's tint
        let (sx, sy, sw, sh) = self.sidebar();
        let r = self.p(18.0);
        let m = Material::for_kind(Kind::Sheet, self.dark, t.accent);
        let (wi, hi) = (sw.round() as usize, sh.round() as usize);
        let reach = glass::margin(&m, s, self.level, wi, hi, r);
        let key = (wi as u64) << 32 | (hi as u64) << 8 | (self.dark as u64) << 1 | (self.level as u64) << 4 | ((s * 100.0) as u64) << 48;
        let pane = match &self.glass {
            Some((k, p)) if *k == key => p.clone(),
            _ => {
                let back = c.crop(sx as isize - reach as isize, sy as isize - reach as isize, wi + 2 * reach, hi + 2 * reach);
                let p = glass::render(Some(&back), reach, wi, hi, r, &m, self.level, s);
                self.glass = Some((key, p.clone()));
                p
            }
        };
        c.shadow(sx, sy, sw, sh, r, self.p(16.0), self.p(4.0), Rgba::BLACK.alpha(if self.dark { 0.35 } else { 0.08 }));
        c.composite(&pane, sx.round() as isize, sy.round() as isize, 1.0);
        let (lx, ly) = self.lights();
        let down = match self.down {
            Hot::Lights(Some(l)) if self.hot == self.down => Some(l),
            _ => None,
        };
        look::traffic_lights(&mut c, lx, ly, s, self.active, matches!(self.hot, Hot::Lights(_)), down, false, self.dark);
        c.fill_circle(lx + 2.0 * look::LIGHT_STEP * s, ly, look::LIGHT_D * s / 2.0, if self.dark { Rgba::rgb(0x46, 0x46, 0x4b) } else { Rgba::rgb(0xdc, 0xdc, 0xde) });
        for (i, sec) in SECTIONS.iter().enumerate() {
            let (x, y, w, h) = self.section_rect(i);
            let chosen = i == self.section;
            if chosen {
                c.fill_round_rect(x, y, w, h, self.p(8.0), if self.active { t.accent } else if self.dark { Rgba::WHITE.alpha(0.12) } else { Rgba::BLACK.alpha(0.08) });
            } else if self.hot == Hot::Section(i) {
                c.fill_round_rect(x, y, w, h, self.p(8.0), t.hover);
            }
            let ic = self.p(20.0);
            let (ix, iy) = (x + self.p(8.0), y + (h - ic) / 2.0);
            c.fill_round_rect_with(ix, iy, ic, ic, self.p(5.5), |_, py| sec.colour.shade(0.12).lerp(sec.colour.shade(-0.08), ((py - iy) / ic).clamp(0.0, 1.0)));
            look::symbol(&mut c, sec.icon, ix + ic / 2.0, iy + ic / 2.0, self.p(12.5), Rgba::WHITE);
            let m = self.text(sec.name, 13.0, if chosen { 600 } else { 500 }, w - self.p(40.0));
            let fg = if chosen && self.active { Rgba::WHITE } else { t.text };
            Self::put(&mut c, &m, ix + ic + self.p(9.0), y + (h - m.2 as f32) / 2.0, fg);
        }
        // the section: its name, then its groups
        let title = self.text(SECTIONS[self.section].name, 20.0, 700, self.content_w());
        Self::put(&mut c, &title, self.content_x(), self.p(26.0), if self.active { t.text } else { t.text2 });
        let (rows, groups) = self.layout();
        let (fill, line) = if self.dark { (Rgba::WHITE.alpha(0.06), Rgba::WHITE.alpha(0.08)) } else { (Rgba::WHITE.alpha(0.82), Rgba::rgba(20, 40, 80, 22)) };
        for ((gx, gy, gw, gh), note) in &groups {
            c.shadow(*gx, *gy, *gw, *gh, self.p(GROUP_R), self.p(6.0), self.p(1.0), Rgba::BLACK.alpha(if self.dark { 0.2 } else { 0.04 }));
            c.fill_round_rect(*gx - 0.5 * s, *gy - 0.5 * s, *gw + s, *gh + s, self.p(GROUP_R) + 0.5 * s, line);
            c.fill_round_rect(*gx, *gy, *gw, *gh, self.p(GROUP_R), fill);
            for (n, l) in note.iter().enumerate() {
                let m = self.text(l, 11.5, 400, *gw);
                Self::put(&mut c, &m, *gx + self.p(8.0), *gy + *gh + self.p(7.0) + n as f32 * self.p(15.0), t.text2);
            }
        }
        for (k, (f, row)) in rows.iter().enumerate() {
            let (x, y, w, h) = *row;
            // a hairline between rows of a group
            if k > 0 && rows[k - 1].1 .1 + rows[k - 1].1 .3 == y {
                c.fill_round_rect(x + self.p(16.0), y, w - self.p(32.0), (0.75 * s).max(1.0), 0.0, t.divider);
            }
            let m = self.text(f.label(), 13.0, 400, w * 0.45);
            Self::put(&mut c, &m, x + self.p(16.0), y + (h - m.2 as f32) / 2.0, t.text);
            match f.control() {
                Control::Menu => {
                    let opts = self.options(*f);
                    let v = opts.get(self.chosen(*f)).map(|o| short(o).to_string()).unwrap_or_default();
                    let vm = self.text(&v, 13.0, 400, w * 0.5);
                    let right = x + w - self.p(16.0);
                    let hot = self.hot == Hot::Field(*f) || self.menu.as_ref().is_some_and(|m| m.field == *f);
                    let vx = right - self.p(16.0) - vm.1 as f32;
                    if hot {
                        c.fill_round_rect(vx - self.p(8.0), y + self.p(8.0), right - vx + self.p(10.0), h - self.p(16.0), self.p(7.0), t.hover);
                    }
                    Self::put(&mut c, &vm, vx, y + (h - vm.2 as f32) / 2.0, t.text2);
                    look::symbol(&mut c, Symbol::UpDown, right - self.p(5.0), y + h / 2.0, self.p(12.0), t.text2);
                }
                Control::Switch => {
                    let (kw, kh) = (self.p(38.0), self.p(22.0));
                    let (kx, ky) = (x + w - self.p(16.0) - kw, y + (h - kh) / 2.0);
                    let on = self.knobs.get(f).map_or(0.0, |a| a.value_at(now)).clamp(-0.1, 1.1);
                    let off = if self.dark { Rgba::rgb(0x39, 0x39, 0x3d) } else { Rgba::rgb(0xe3, 0xe3, 0xe8) };
                    c.fill_round_rect(kx, ky, kw, kh, kh / 2.0, off.lerp(t.accent, on.clamp(0.0, 1.0)));
                    let d = kh - self.p(4.0);
                    let cx = kx + self.p(2.0) + d / 2.0 + on * (kw - d - self.p(4.0));
                    c.shadow(cx - d / 2.0, ky + self.p(2.0), d, d, d / 2.0, self.p(3.0), self.p(1.0), Rgba::BLACK.alpha(0.18));
                    c.fill_circle(cx, ky + kh / 2.0, d / 2.0, Rgba::WHITE);
                }
                Control::Slider => {
                    let (tx, ty, tw) = self.slider_track(*row);
                    let v = self.settings.volume as f32 / 100.0;
                    look::symbol(&mut c, Symbol::Speaker, tx - self.p(16.0), ty, self.p(12.0), t.text3);
                    c.fill_capsule(tx, ty, tx + tw, ty, self.p(2.0), if self.dark { Rgba::WHITE.alpha(0.18) } else { Rgba::BLACK.alpha(0.1) });
                    c.fill_capsule(tx, ty, tx + tw * v, ty, self.p(2.0), t.accent);
                    let kx = tx + tw * v;
                    c.shadow(kx - self.p(10.0), ty - self.p(10.0), self.p(20.0), self.p(20.0), self.p(10.0), self.p(4.0), self.p(1.0), Rgba::BLACK.alpha(0.2));
                    c.fill_circle(kx, ty, self.p(10.0), Rgba::WHITE);
                    let vm = self.text(&format!("{} %", self.settings.volume), 12.0, 400, self.p(60.0));
                    Self::put(&mut c, &vm, tx - self.p(28.0) - vm.1 as f32, ty - vm.2 as f32 / 2.0, t.text2);
                }
            }
        }
        // the open menu, over everything
        if let Some(((mx, my, mw, mh), ih)) = self.menu_rect() {
            let open = self.menu.as_ref().map_or(1.0, |m| m.open.value_at(now)).clamp(0.0, 1.0);
            let mr = self.p(10.0);
            let mat = Material::for_kind(Kind::Sheet, self.dark, t.accent);
            let (wi, hi) = (mw.round() as usize, mh.round() as usize);
            let reach = glass::margin(&mat, s, self.level, wi, hi, mr);
            let back = c.crop(mx as isize - reach as isize, my as isize - reach as isize, wi + 2 * reach, hi + 2 * reach);
            let body = glass::render(Some(&back), reach, wi, hi, mr, &mat, self.level, s);
            c.shadow(mx, my, mw, mh, mr, self.p(14.0), self.p(5.0), Rgba::BLACK.alpha(if self.dark { 0.45 } else { 0.18 }));
            c.composite(&body, mx.round() as isize, (my - (1.0 - open) * self.p(6.0)).round() as isize, open);
            let (items, hover, chosen) = {
                let m = self.menu.as_ref().unwrap();
                (m.items.clone(), m.hover, self.chosen(m.field))
            };
            for (i, item) in items.iter().enumerate() {
                let iy = my + self.p(5.0) + i as f32 * ih;
                let lit = hover == Some(i);
                if lit {
                    c.fill_round_rect(mx + self.p(5.0), iy, mw - self.p(10.0), ih, self.p(6.0), t.accent.fade(open));
                }
                let fg = if lit { Rgba::WHITE } else { t.text };
                if i == chosen {
                    look::symbol(&mut c, Symbol::Check, mx + self.p(17.0), iy + ih / 2.0, self.p(11.0), fg.fade(open));
                }
                let m = self.text(item, 13.0, 400, mw - self.p(40.0));
                Self::put(&mut c, &m, mx + self.p(30.0), iy + (ih - m.2 as f32) / 2.0, fg.fade(open));
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> SettingsView {
        let mut v = SettingsView::new(Settings::default(), vec!["1280 × 800 (as large as this screen)".into(), "1440 × 900 (more space)".into()]);
        v.set_window(720, 560, 1.0, false, Level::Full);
        v
    }

    fn click(v: &mut SettingsView, x: f32, y: f32) -> Option<Act> {
        v.mouse_move(x, y);
        v.mouse_down(x, y);
        v.mouse_up(x, y)
    }

    fn row(v: &mut SettingsView, f: Field) -> (f32, f32, f32, f32) {
        v.layout().0.into_iter().find(|(g, _)| *g == f).unwrap().1
    }

    #[test]
    fn a_menu_opens_and_its_choice_applies_at_once() {
        let mut v = view();
        let (x, y, w, h) = row(&mut v, Field::Fps);
        assert_eq!(click(&mut v, x + w - 30.0, y + h / 2.0), None, "the menu opens");
        let ((mx, my, _, _), ih) = v.menu_rect().unwrap();
        // the fourth item: 120 fps
        match click(&mut v, mx + 40.0, my + 5.0 + 3.5 * ih) {
            Some(Act::Changed(s)) => assert_eq!(s.fps, 120),
            a => panic!("{a:?}"),
        }
        assert!(v.menu.is_none());
        // a click outside an open menu only closes it
        click(&mut v, x + w - 30.0, y + h / 2.0);
        assert_eq!(click(&mut v, 300.0, 540.0), None);
        assert!(v.menu.is_none() && v.settings.fps == 120);
        // by keyboard
        click(&mut v, x + w - 30.0, y + h / 2.0);
        v.key_up_down(false);
        assert!(matches!(v.key_enter(), Some(Act::Changed(s)) if s.fps == 90));
    }

    #[test]
    fn switches_slide_and_the_slider_drags() {
        let mut v = view();
        let (sx, sy, sw, sh) = v.section_rect(1);
        click(&mut v, sx + sw / 2.0, sy + sh / 2.0);
        assert_eq!(v.section(), 1, "Sound");
        let (x, y, w, h) = row(&mut v, Field::Audio);
        assert!(matches!(click(&mut v, x + w - 30.0, y + h / 2.0), Some(Act::Changed(s)) if !s.audio));
        assert!(v.busy(Instant::now()), "the knob slides");
        let r = row(&mut v, Field::Volume);
        let (tx, ty, tw) = v.slider_track(r);
        v.mouse_move(tx + tw * 0.5, ty);
        v.mouse_down(tx + tw * 0.5, ty);
        v.mouse_move(tx + tw * 0.25, ty);
        assert!(matches!(v.mouse_up(tx + tw * 0.25, ty), Some(Act::Changed(s)) if s.volume == 25));
        // the lights: close; zoom does nothing (the window keeps its size)
        let (lx, ly) = v.lights();
        assert_eq!(click(&mut v, lx, ly), Some(Act::Window(Light::Close)));
        assert_eq!(click(&mut v, lx + 40.0, ly), None);
        v.key_up_down(true);
        assert_eq!(v.section(), 2);
    }

    #[test]
    fn every_section_draws() {
        for (s, dark) in [(1.0, false), (1.5, true)] {
            let mut v = view();
            v.set_window((W * s) as usize, (H * s) as usize, s, dark, Level::Full);
            for i in 0..SECTIONS.len() {
                v.section = i;
                let c = v.render(Instant::now() + Duration::from_secs(2));
                assert_eq!(c.w, (W * s) as usize);
            }
        }
    }
}

/// Pictures of the Settings window (RM_PREVIEW=dir cargo test -p rm-viewer settingsui::preview).
#[cfg(test)]
mod preview {
    use super::*;

    #[test]
    fn pictures() {
        let Some(dir) = std::env::var_os("RM_PREVIEW") else { return };
        let dir = std::path::PathBuf::from(dir);
        let s: f32 = std::env::var("RM_PREVIEW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
        let save = |c: &Canvas, name: &str| {
            let mut ppm = format!("P6 {} {} 255\n", c.w, c.h).into_bytes();
            for p in &c.px {
                ppm.extend_from_slice(&[p[2], p[1], p[0]]);
            }
            std::fs::write(dir.join(name), ppm).unwrap();
        };
        let later = Instant::now() + Duration::from_secs(3);
        for dark in [false, true] {
            let mut v = SettingsView::new(Settings::default(), vec!["1440 × 900 (as large as this screen)".into(), "1620 × 1012 (more space)".into(), "1800 × 1125 (even more space)".into()]);
            v.set_window((W * s) as usize, (H * s) as usize, s, dark, Level::Full);
            save(&v.render(later), &format!("settings-{}.ppm", if dark { "dark" } else { "light" }));
            if !dark {
                // the Sharpness menu open
                let (x, y, w, h) = v.layout().0.into_iter().find(|(f, _)| *f == Field::Sharpness).unwrap().1;
                v.mouse_move(x + w - 30.0, y + h / 2.0);
                v.mouse_down(x + w - 30.0, y + h / 2.0);
                v.mouse_up(x + w - 30.0, y + h / 2.0);
                v.key_up_down(true);
                save(&v.render(later), "settings-menu.ppm");
                v.key_escape();
                v.section = 1;
                save(&v.render(later), "settings-sound.ppm");
            }
        }
    }
}
