//! The connect window as it looks and behaves, after the sign-in sheet of Apple's Screen Sharing
//! on macOS 26: the window buttons, a Mac at the top, a bold title and what to do, a segmented
//! control (find the Mac by its ID, or type its address), rounded fields with their symbols,
//! the step under way (a spinner) or what went wrong (in red), and Cancel / Connect. By its ID:
//! the ID, the password and (for a Mac elsewhere) a relay. By its address: only the address and
//! the password, straight to the Mac as Moonlight goes to Sunshine (the Mac says which it is).
//! The fields are MacBridge's own (field.rs): typing, selecting, words, paste, a secure password
//! field.
//!
//! Portable: it draws into a Canvas and takes plain input; connect.rs puts it in a window.

use crate::field::{self, Field, Move};
use crate::glass::{self, Kind, Level, Material};
use crate::look::{self, theme, Light, Symbol, Theme};
use crate::motion::{Anim, Curve};
use crate::paint::{Canvas, Rgba};
use crate::text::{self, Style};
use std::time::{Duration, Instant};

/// The window's size (DIPs).
pub const W: f32 = 460.0;
pub const H: f32 = 572.0;
const FX: f32 = 48.0;
const FW: f32 = 364.0;
const FH: f32 = 42.0;
const FIELD_Y: [f32; 3] = [276.0, 328.0, 380.0];
const SEG: (f32, f32, f32, f32) = (80.0, 222.0, 300.0, 32.0);
const BUTTON_Y: f32 = 508.0;
const BUTTON_H: f32 = 36.0;
const CONNECT_W: f32 = 120.0;
const CANCEL_W: f32 = 100.0;
/// a password dot's step (DIPs)
const BULLET: f32 = 10.0;

/// How the Mac is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// found by its ID on this network, else through this relay ("" none)
    Id(String),
    /// straight to this address (IP or host name, :port when not 7471)
    Address(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Connect { id: String, password: String, via: Via },
    Cancel,
    Window(Light),
    /// put this on the clipboard (Copy, Cut)
    Copy(String),
}

/// Keys the window uses (with Shift and Ctrl as given).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    Enter,
    Escape,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Backspace,
    Delete,
    /// Ctrl+A, Ctrl+C, Ctrl+X
    SelectAll,
    Copy,
    Cut,
}

/// Where the keyboard is, in Tab's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Field(usize),
    Segment,
    Connect,
    Cancel,
}

/// The fields: the ID, the password, the relay (by ID), the address (by address).
pub const ID: usize = 0;
pub const PASSWORD: usize = 1;
pub const RELAY: usize = 2;
pub const ADDRESS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hot {
    None,
    Lights(Option<Light>),
    Segment(bool),
    Field(usize),
    Connect,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    None,
    Busy(String),
    Error(String),
}

const FLOW: Curve = Curve::Spring { response: 0.36, damping: 0.86 };
const GIVE: Curve = Curve::Spring { response: 0.16, damping: 1.0 };
const BACK: Curve = Curve::Spring { response: 0.34, damping: 0.66 };

type Mask = (Vec<u8>, usize, usize);
/// What the drawn background depends on: size, scale, dark, by address.
type BgKey = (usize, usize, u32, bool, bool);

pub struct ConnectView {
    fields: [Field; 4],
    by_address: bool,
    focus: Focus,
    status: Status,
    busy: bool,
    hot: Hot,
    down: Hot,
    seg: Anim,
    ring: Anim,
    press: Anim,
    blink: Instant,
    since: Instant,
    /// a drag selecting in this field
    drag: Option<usize>,
    /// horizontal scroll of each field's text (px)
    shift: [f32; 4],
    w: usize,
    h: usize,
    s: f32,
    dark: bool,
    active: bool,
    level: Level,
    bg: Option<(BgKey, Canvas)>,
    glass: Vec<(u64, Canvas)>,
}

impl ConnectView {
    /// `id`, the relay and the address typed last, and whether the address was used last.
    pub fn new(id: &str, relay: &str, address: &str, by_address: bool) -> ConnectView {
        let first = if by_address { (ADDRESS, address) } else { (ID, id) };
        ConnectView {
            fields: [Field::new(id, false, 32), Field::new("", true, 128), Field::new(relay, false, 255), Field::new(address, false, 255)],
            by_address,
            focus: Focus::Field(if first.1.is_empty() { first.0 } else { PASSWORD }),
            status: Status::None,
            busy: false,
            hot: Hot::None,
            down: Hot::None,
            seg: Anim::at(if by_address { 1.0 } else { 0.0 }),
            ring: Anim::at(1.0),
            press: Anim::at(1.0),
            blink: Instant::now(),
            since: Instant::now(),
            drag: None,
            shift: [0.0; 4],
            w: W as usize,
            h: H as usize,
            s: 1.0,
            dark: false,
            active: true,
            level: Level::Full,
            bg: None,
            glass: vec![],
        }
    }

    pub fn set_window(&mut self, w: usize, h: usize, s: f32, dark: bool, level: Level) {
        (self.w, self.h, self.s, self.dark, self.level) = (w.max(1), h.max(1), s.max(0.5), dark, level);
    }

    pub fn set_active(&mut self, a: bool) {
        self.active = a;
    }

    /// The step the connection is at (a spinner beside it).
    pub fn set_step(&mut self, label: &str) {
        self.status = Status::Busy(label.into());
    }

    /// It did not connect: say why, and let the user try again (in the password field).
    pub fn fail(&mut self, why: &str) {
        self.busy = false;
        self.status = Status::Error(why.into());
        self.set_focus(Focus::Field(PASSWORD));
        self.fields[PASSWORD].select_all();
    }

    /// Start with this message (a lost connection, say).
    pub fn set_error(&mut self, why: &str) {
        self.status = if why.is_empty() { Status::None } else { Status::Error(why.into()) };
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn by_address(&self) -> bool {
        self.by_address
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    fn set_focus(&mut self, f: Focus) {
        if f != self.focus {
            self.focus = f;
            self.ring = Anim::new(0.0, 1.0, Duration::from_millis(260), FLOW);
        }
        self.blink = Instant::now();
    }

    // ------------------------------------------------------------------ geometry (px)

    fn p(&self, v: f32) -> f32 {
        v * self.s
    }

    fn lights(&self) -> (f32, f32) {
        (self.p(20.0 + look::LIGHT_D / 2.0), self.p(22.0))
    }

    /// The fields shown, top to bottom.
    pub fn rows(&self) -> &'static [usize] {
        if self.by_address {
            &[ADDRESS, PASSWORD]
        } else {
            &[ID, PASSWORD, RELAY]
        }
    }

    /// Tab's order: the fields, the segmented control, Connect, Cancel.
    fn order(&self) -> Vec<Focus> {
        let mut o: Vec<Focus> = self.rows().iter().map(|&i| Focus::Field(i)).collect();
        o.extend([Focus::Segment, Focus::Connect, Focus::Cancel]);
        o
    }

    fn field_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let row = self.rows().iter().position(|&r| r == i).unwrap_or(0);
        (self.p(FX), self.p(FIELD_Y[row]), self.p(FW), self.p(FH))
    }

    fn connect_rect(&self) -> (f32, f32, f32, f32) {
        (self.p(FX + FW - CONNECT_W), self.p(BUTTON_Y), self.p(CONNECT_W), self.p(BUTTON_H))
    }

    fn cancel_rect(&self) -> (f32, f32, f32, f32) {
        (self.p(FX + FW - CONNECT_W - 12.0 - CANCEL_W), self.p(BUTTON_Y), self.p(CANCEL_W), self.p(BUTTON_H))
    }

    fn seg_rect(&self) -> (f32, f32, f32, f32) {
        (self.p(SEG.0), self.p(SEG.1), self.p(SEG.2), self.p(SEG.3))
    }

    fn text_style(&self) -> Style {
        Style::dip(14.0, 400, self.s)
    }

    /// Where field `i`'s text starts (px).
    fn text_x(&self, i: usize) -> f32 {
        self.field_rect(i).0 + self.p(40.0)
    }

    fn text_room(&self, i: usize) -> f32 {
        self.field_rect(i).2 - self.p(52.0)
    }

    fn hit(&self, x: f32, y: f32) -> Hot {
        let inside = |r: (f32, f32, f32, f32)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
        let (lx, ly) = self.lights();
        if y < self.p(44.0) {
            if let Some(l) = look::light_at(x, y, lx, ly, self.s) {
                // the window is not resizable: zoom does nothing (grey, as on the Mac)
                return Hot::Lights((l != Light::Zoom).then_some(l));
            }
        }
        let sr = self.seg_rect();
        if inside(sr) {
            return Hot::Segment(x >= sr.0 + sr.2 / 2.0);
        }
        for &i in self.rows() {
            if inside(self.field_rect(i)) {
                return Hot::Field(i);
            }
        }
        if inside(self.connect_rect()) {
            return Hot::Connect;
        }
        if inside(self.cancel_rect()) {
            return Hot::Cancel;
        }
        Hot::None
    }

    /// The window is moved by dragging anywhere that is not a control (as a Mac sheet's window).
    pub fn is_caption(&self, x: f32, y: f32) -> bool {
        self.hit(x, y) == Hot::None
    }

    /// Where a caret can stand in field `i` (px from its text's start): the text's own places,
    /// or even steps for a password's dots.
    fn carets(&self, i: usize) -> Vec<f32> {
        let f = &self.fields[i];
        if f.secure {
            return (0..=f.len()).map(|n| n as f32 * self.p(BULLET)).collect();
        }
        text::carets(f.text(), self.text_style())
    }

    /// The character in field `i` nearest `x` (px).
    fn char_at(&self, i: usize, x: f32) -> usize {
        let xs = self.carets(i);
        field::nearest(&xs, x - self.text_x(i) + self.shift[i])
    }

    // ------------------------------------------------------------------ input

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        self.hot = self.hit(x, y);
        if let Some(i) = self.drag {
            let c = self.char_at(i, x);
            self.fields[i].place(c, true);
            self.blink = Instant::now();
        }
    }

    pub fn mouse_leave(&mut self) {
        if self.drag.is_none() {
            self.hot = Hot::None;
        }
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, shift: bool) {
        self.mouse_move(x, y);
        self.down = self.hot;
        match self.hot {
            Hot::Field(i) if !self.busy => {
                self.set_focus(Focus::Field(i));
                let c = self.char_at(i, x);
                self.fields[i].place(c, shift);
                self.drag = Some(i);
            }
            Hot::Connect | Hot::Cancel => self.press.retarget(0.96, Duration::from_millis(120), GIVE),
            _ => {}
        }
    }

    pub fn double_click(&mut self, x: f32, y: f32) {
        if let Hot::Field(i) = self.hit(x, y) {
            let c = self.char_at(i, x);
            self.fields[i].select_word(c);
        }
    }

    pub fn mouse_up(&mut self, x: f32, y: f32) -> Option<Act> {
        self.mouse_move(x, y);
        self.drag = None;
        let down = std::mem::replace(&mut self.down, Hot::None);
        if matches!(down, Hot::Connect | Hot::Cancel) {
            self.press.retarget(1.0, Duration::from_millis(400), BACK);
        }
        if down != self.hot {
            return None;
        }
        match down {
            Hot::Lights(Some(l)) => Some(Act::Window(l)),
            Hot::Segment(addr) if !self.busy => {
                self.switch(addr);
                None
            }
            Hot::Connect => self.submit(),
            Hot::Cancel => Some(Act::Cancel),
            _ => None,
        }
    }

    /// Find it by its ID (false) or type its address (true): the fields change (each keeps what
    /// was typed in it), the keyboard to the first one still empty.
    fn switch(&mut self, by_address: bool) {
        if by_address == self.by_address {
            return;
        }
        self.by_address = by_address;
        self.seg.retarget(if by_address { 1.0 } else { 0.0 }, Duration::from_millis(320), FLOW);
        if let Focus::Field(f) = self.focus {
            let first = self.rows()[0];
            if self.fields[first].is_empty() {
                self.set_focus(Focus::Field(first));
            } else if !self.rows().contains(&f) {
                self.set_focus(Focus::Field(PASSWORD));
            }
        }
        if let Status::Error(_) = self.status {
            self.status = Status::None;
        }
    }

    /// Connect, if what is typed can be: else say what is missing (and go to it).
    fn submit(&mut self) -> Option<Act> {
        if self.busy {
            return None;
        }
        let password = self.fields[PASSWORD].text().to_string();
        let (id, via) = if self.by_address {
            // straight to the Mac at the address: no ID (the Mac says it), no relay
            let address = self.fields[ADDRESS].text().trim().to_string();
            if let Err(e) = rm_relay::lan::parse_address(&address) {
                self.status = Status::Error(if address.is_empty() { "Type the Mac's IP address or name.".into() } else { e });
                self.set_focus(Focus::Field(ADDRESS));
                return None;
            }
            (String::new(), Via::Address(address))
        } else {
            let Some(id) = rm_protocol::session::normalize_id(self.fields[ID].text()) else {
                self.status = Status::Error("The ID is the 9 digits shown on the Mac.".into());
                self.set_focus(Focus::Field(ID));
                return None;
            };
            (id, Via::Id(self.fields[RELAY].text().trim().to_string()))
        };
        if password.is_empty() {
            self.status = Status::Error("Type the password shown on the Mac.".into());
            self.set_focus(Focus::Field(PASSWORD));
            return None;
        }
        self.busy = true;
        self.status = Status::Busy("Connecting…".into());
        Some(Act::Connect { id, password, via })
    }

    pub fn key(&mut self, k: Key, shift: bool, ctrl: bool) -> Option<Act> {
        self.blink = Instant::now();
        match k {
            Key::Tab => {
                let order = self.order();
                let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
                let n = order.len();
                let next = if shift { order[(i + n - 1) % n] } else { order[(i + 1) % n] };
                self.set_focus(next);
                if let Focus::Field(f) = next {
                    self.fields[f].select_all();
                }
                return None;
            }
            Key::Enter => return self.submit(),
            Key::Escape => return Some(Act::Cancel),
            _ => {}
        }
        if self.busy {
            return None;
        }
        match self.focus {
            Focus::Field(i) => {
                let f = &mut self.fields[i];
                match k {
                    Key::Left => f.go(if ctrl { Move::WordLeft } else { Move::Left }, shift),
                    Key::Right => f.go(if ctrl { Move::WordRight } else { Move::Right }, shift),
                    Key::Home | Key::Up => f.go(Move::Home, shift),
                    Key::End | Key::Down => f.go(Move::End, shift),
                    Key::Backspace => f.backspace(ctrl),
                    Key::Delete => f.delete(ctrl),
                    Key::SelectAll => f.select_all(),
                    Key::Copy => return f.copied().map(Act::Copy),
                    Key::Cut => return f.cut().map(Act::Copy),
                    _ => {}
                }
                if matches!(k, Key::Backspace | Key::Delete | Key::Cut) {
                    self.clear_error();
                }
            }
            Focus::Segment => match k {
                Key::Left => self.switch(false),
                Key::Right => self.switch(true),
                Key::Space => self.switch(!self.by_address),
                _ => {}
            },
            Focus::Connect if k == Key::Space => return self.submit(),
            Focus::Cancel if k == Key::Space => return Some(Act::Cancel),
            _ => {}
        }
        None
    }

    fn clear_error(&mut self) {
        if let Status::Error(_) = self.status {
            self.status = Status::None;
        }
    }

    /// Typed text goes to the field with the focus.
    pub fn char(&mut self, c: char) {
        if self.busy || c.is_control() {
            return;
        }
        if let Focus::Field(i) = self.focus {
            self.fields[i].insert(&c.to_string());
            self.blink = Instant::now();
            self.clear_error();
        }
    }

    pub fn paste(&mut self, s: &str) {
        if self.busy {
            return;
        }
        if let Focus::Field(i) = self.focus {
            // an ID pasted with its spaces or dashes is kept as the digits
            let s = if i == PASSWORD { s.trim_end_matches(['\r', '\n']).to_string() } else { s.trim().to_string() };
            self.fields[i].insert(&s);
            self.clear_error();
        }
    }

    /// When the next frame is due: soon while something moves, at the caret's next blink, or
    /// never (nothing changes until the user does something).
    pub fn next_frame(&self, now: Instant) -> Option<Duration> {
        let moving = !self.seg.done_at(now) || !self.ring.done_at(now) || !self.press.done_at(now) || matches!(self.status, Status::Busy(_));
        if moving {
            return Some(Duration::from_millis(16));
        }
        if matches!(self.focus, Focus::Field(_)) && self.active && !self.busy {
            let t = now.saturating_duration_since(self.blink).as_millis() as u64;
            return Some(Duration::from_millis(530 - t % 530));
        }
        None
    }

    // ------------------------------------------------------------------ drawing

    fn text(&self, s: &str, size: f32, weight: i32, max_w: f32) -> Mask {
        text::line(s, Style::dip(size, weight, self.s), max_w)
    }

    fn put(c: &mut Canvas, m: &Mask, x: f32, y: f32, col: Rgba) {
        c.fill_mask(&m.0, m.1, x.round() as isize, y.round() as isize, col);
    }

    fn background(&mut self) -> Canvas {
        let key = (self.w, self.h, (self.s * 100.0) as u32, self.dark, self.by_address);
        if let Some((k, c)) = &self.bg {
            if *k == key {
                return c.clone();
            }
        }
        let t = theme(self.dark);
        let mut c = Canvas::new(self.w, self.h);
        look::tint(&mut c, &t);
        let glow = if self.dark { Rgba::WHITE.alpha(0.03) } else { Rgba::WHITE.alpha(0.4) };
        let gh = self.p(220.0);
        c.fill_round_rect_with(0.0, 0.0, self.w as f32, gh, 0.0, |_, y| glow.fade(1.0 - y / gh));
        // the Mac, and what this window is for
        let s = self.s;
        look::mac_icon(&mut c, self.w as f32 / 2.0, self.p(92.0), self.p(104.0), self.dark);
        let title = self.text("Connect to Your Mac", 20.0, 700, self.p(380.0));
        Self::put(&mut c, &title, (self.w as f32 - title.1 as f32) / 2.0, self.p(150.0), t.text);
        let what = if self.by_address { "Type the Mac's IP address or name and the password MacBridge shows on it." } else { "Open MacBridge on the Mac, then type the ID and password it shows." };
        for (n, l) in text::wrap(what, Style::dip(13.0, 400, s), self.p(330.0), 2).iter().enumerate() {
            let m = self.text(l, 13.0, 400, self.p(340.0));
            Self::put(&mut c, &m, (self.w as f32 - m.1 as f32) / 2.0, self.p(180.0 + n as f32 * 17.0), t.text2);
        }
        self.bg = Some((key, c.clone()));
        c
    }

    /// A glass control (the segmented track, Cancel) of what is under it.
    #[allow(clippy::too_many_arguments)]
    fn glass(&mut self, c: &mut Canvas, slot: usize, x: f32, y: f32, w: f32, h: f32, kind: Kind, pressed: bool) {
        let t = theme(self.dark);
        let mut m = Material::for_kind(kind, self.dark, t.accent);
        if pressed {
            m = m.pressed();
        }
        let (wi, hi) = (w.round().max(1.0) as usize, h.round().max(1.0) as usize);
        let r = h / 2.0;
        let reach = glass::margin(&m, self.s, self.level, wi, hi, r);
        let back = c.crop(x.round() as isize - reach as isize, y.round() as isize - reach as isize, wi + 2 * reach, hi + 2 * reach);
        let mut key: u64 = 0xcbf2_9ce4_8422_2325 ^ (pressed as u64) ^ ((kind as u64) << 2) ^ ((self.level as u64) << 5);
        for p in back.px.iter().step_by(5) {
            key = (key ^ u32::from_le_bytes(*p) as u64).wrapping_mul(0x100_0000_01b3);
        }
        let shadow = if kind == Kind::Accent { t.accent.alpha(if self.dark { 0.35 } else { 0.28 }) } else { Rgba::BLACK.alpha(if self.dark { 0.3 } else { 0.07 }) };
        c.shadow(x, y, w, h, r, self.p(10.0), self.p(3.0), shadow);
        while self.glass.len() <= slot {
            self.glass.push((0, Canvas::new(1, 1)));
        }
        if self.glass[slot].0 != key {
            self.glass[slot] = (key, glass::render(Some(&back), reach, wi, hi, r, &m, self.level, self.s));
        }
        let body = self.glass[slot].1.clone();
        c.composite(&body, x.round() as isize, y.round() as isize, 1.0);
    }

    pub fn render(&mut self, now: Instant) -> Canvas {
        let t = theme(self.dark);
        let s = self.s;
        let mut c = self.background();
        // the window buttons (zoom grey: this window keeps its size)
        let (lx, ly) = self.lights();
        let hot_lights = matches!(self.hot, Hot::Lights(_));
        let down = match self.down {
            Hot::Lights(Some(l)) if self.hot == self.down => Some(l),
            _ => None,
        };
        look::traffic_lights(&mut c, lx, ly, s, self.active, hot_lights, down, false, self.dark);
        let zx = lx + 2.0 * look::LIGHT_STEP * s;
        c.fill_circle(zx, ly, look::LIGHT_D * s / 2.0, if self.dark { Rgba::rgb(0x46, 0x46, 0x4b) } else { Rgba::rgb(0xdc, 0xdc, 0xde) });
        // the segmented control: a glass track, the chosen side a raised pill that slides
        let (sx, sy, sw, sh) = self.seg_rect();
        self.glass(&mut c, 0, sx, sy, sw, sh, Kind::Tabs, false);
        let k = self.seg.value_at(now).clamp(-0.1, 1.1);
        let half = sw / 2.0;
        let inset = self.p(3.0);
        let (px, pw) = (sx + inset + k * (half - inset), half - inset);
        c.shadow(px, sy + inset, pw, sh - 2.0 * inset, (sh - 2.0 * inset) / 2.0, self.p(3.0), self.p(1.0), Rgba::BLACK.alpha(if self.dark { 0.4 } else { 0.12 }));
        c.fill_round_rect(px, sy + inset, pw, sh - 2.0 * inset, (sh - 2.0 * inset) / 2.0, if self.dark { Rgba::rgb(0x5a, 0x5a, 0x62) } else { Rgba::WHITE });
        for (n, label) in ["By ID", "By Address"].iter().enumerate() {
            let chosen = (n == 1) == self.by_address;
            let m = self.text(label, 13.0, if chosen { 600 } else { 500 }, half);
            let col = if chosen { t.text } else { t.text2 };
            Self::put(&mut c, &m, sx + n as f32 * half + (half - m.1 as f32) / 2.0, sy + (sh - m.2 as f32) / 2.0, col);
        }
        if self.focus == Focus::Segment {
            self.focus_ring(&mut c, sx, sy, sw, sh, sh / 2.0, now, &t);
        }
        // the fields
        let labels: [(&str, Symbol); 4] = [("Mac ID (9 digits)", Symbol::Number), ("Password", Symbol::Lock), ("Relay server (optional)", Symbol::Globe), ("IP address or name, e.g. 192.168.1.20", Symbol::Display)];
        for &i in self.rows() {
            let (placeholder, sym) = labels[i];
            self.draw_field(&mut c, i, placeholder, sym, now, &t);
        }
        // a word under the last field
        let hint = if self.by_address { "Straight to the Mac, no relay (port 7471 unless given)." } else { "Only needed for a Mac on another network." };
        let hm = self.text(hint, 11.5, 400, self.p(FW - 16.0));
        Self::put(&mut c, &hm, self.p(FX + 6.0), self.p(FIELD_Y[self.rows().len() - 1] + FH + 8.0), t.text3);
        // the step under way, or what went wrong
        let line_y = self.p(452.0);
        match self.status.clone() {
            Status::None => {}
            Status::Busy(label) => {
                let m = self.text(&label, 13.0, 500, self.p(FW - 30.0));
                let total = m.1 as f32 + self.p(26.0);
                let x0 = (self.w as f32 - total) / 2.0;
                look::spinner(&mut c, x0 + self.p(8.0), line_y + m.2 as f32 / 2.0, self.p(16.0), t.text2, now.saturating_duration_since(self.since).as_secs_f32());
                Self::put(&mut c, &m, x0 + self.p(26.0), line_y, t.text2);
            }
            Status::Error(why) => {
                let lines = text::wrap(&why, Style::dip(13.0, 500, s), self.p(FW - 40.0), 2);
                let ms: Vec<Mask> = lines.iter().map(|l| self.text(l, 13.0, 500, self.p(FW - 40.0))).collect();
                let widest = ms.iter().map(|m| m.1).max().unwrap_or(0) as f32;
                let x0 = (self.w as f32 - widest - self.p(24.0)) / 2.0;
                for (n, m) in ms.iter().enumerate() {
                    let y = line_y + n as f32 * self.p(17.0);
                    if n == 0 {
                        look::symbol(&mut c, Symbol::Warning, x0 + self.p(8.0), y + m.2 as f32 / 2.0, self.p(15.0), t.fail);
                    }
                    Self::put(&mut c, m, x0 + self.p(24.0), y, t.fail);
                }
            }
        }
        // Cancel and Connect
        let press = self.press.value_at(now);
        for (slot, (hot, label, kind, rect)) in [(Hot::Cancel, "Cancel", Kind::Control, self.cancel_rect()), (Hot::Connect, if self.busy { "Connecting…" } else { "Connect" }, Kind::Accent, self.connect_rect())].into_iter().enumerate() {
            let (bx, by, bw, bh) = rect;
            let k = if self.hot == hot && (self.down == hot || (self.down == Hot::None && !self.press.done_at(now))) { press } else { 1.0 };
            let (bw2, bh2) = (bw * k, bh * k);
            let (bx2, by2) = (bx + (bw - bw2) / 2.0, by + (bh - bh2) / 2.0);
            let pressed = self.down == hot && self.hot == hot;
            self.glass(&mut c, 1 + slot, bx2, by2, bw2, bh2, kind, pressed);
            if self.hot == hot && !pressed && !(hot == Hot::Connect && self.busy) {
                c.fill_round_rect(bx2, by2, bw2, bh2, bh2 / 2.0, Rgba::WHITE.alpha(if kind == Kind::Accent { 0.1 } else { 0.18 }));
            }
            let fg = if kind == Kind::Accent { Rgba::WHITE.fade(if self.busy { 0.75 } else { 1.0 }) } else { t.text };
            let m = self.text(label, 13.5, 600, bw);
            Self::put(&mut c, &m, bx2 + (bw2 - m.1 as f32) / 2.0, by2 + (bh2 - m.2 as f32) / 2.0, fg);
            let f = if hot == Hot::Connect { Focus::Connect } else { Focus::Cancel };
            if self.focus == f {
                self.focus_ring(&mut c, bx, by, bw, bh, bh / 2.0, now, &t);
            }
        }
        c
    }

    /// macOS's focus ring: the accent, softly around the control, growing in when it comes.
    #[allow(clippy::too_many_arguments)]
    fn focus_ring(&self, c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, now: Instant, t: &Theme) {
        if !self.active {
            return;
        }
        let k = self.ring.value_at(now).clamp(0.0, 1.2);
        let grow = self.p(3.5) + self.p(6.0) * (1.0 - k.min(1.0));
        let g = grow;
        c.stroke_round_rect_with(x - g + self.p(1.75), y - g + self.p(1.75), w + 2.0 * g - self.p(3.5), h + 2.0 * g - self.p(3.5), r + g - self.p(1.75), self.p(3.5), |_, _| t.accent.alpha(0.5 * k.min(1.0)));
    }

    fn draw_field(&mut self, c: &mut Canvas, i: usize, placeholder: &str, sym: Symbol, now: Instant, t: &Theme) {
        let (x, y, w, h) = self.field_rect(i);
        let r = self.p(11.0);
        let s = self.s;
        let dim = if self.busy { 0.55 } else { 1.0 };
        // a hairline and the fill (white in light, a raised grey in dark)
        let (fill, line) = if self.dark { (Rgba::WHITE.alpha(0.075), Rgba::WHITE.alpha(0.13)) } else { (Rgba::WHITE.alpha(0.9), Rgba::rgba(20, 40, 80, 30)) };
        c.shadow(x, y, w, h, r, self.p(4.0), self.p(1.0), Rgba::BLACK.alpha(if self.dark { 0.25 } else { 0.035 }));
        c.fill_round_rect(x - 0.5 * s, y - 0.5 * s, w + s, h + s, r + 0.5 * s, line);
        c.fill_round_rect(x, y, w, h, r, fill);
        let focused = self.focus == Focus::Field(i);
        if focused {
            self.focus_ring(c, x, y, w, h, r, now, t);
        }
        look::symbol(c, sym, x + self.p(20.0), y + h / 2.0, self.p(15.0), (if focused { t.accent } else { t.text3 }).fade(dim));
        let st = self.text_style();
        let tx = self.text_x(i);
        let room = self.text_room(i);
        let f = &self.fields[i];
        if f.is_empty() {
            let m = self.text(placeholder, 14.0, 400, room);
            Self::put(c, &m, tx, y + (h - m.2 as f32) / 2.0, t.text3.fade(dim));
        }
        let secure = f.secure;
        let shown = if secure { String::new() } else { f.text().to_string() };
        let xs = self.carets(i);
        let f = &self.fields[i];
        // keep the caret in sight
        let cx = xs.get(f.caret()).copied().unwrap_or(0.0);
        let mut shift = self.shift[i];
        if cx - shift > room - self.p(2.0) {
            shift = cx - room + self.p(2.0);
        }
        if cx - shift < 0.0 {
            shift = cx;
        }
        shift = shift.clamp(0.0, (xs.last().copied().unwrap_or(0.0) - room + self.p(2.0)).max(0.0));
        self.shift[i] = shift;
        let (a, b) = f.selection();
        let n = f.len();
        let caret = f.caret();
        let m = text::line(if secure { "X" } else { &shown }, st, 1e6);
        let line_h = m.2 as f32;
        let ty = y + (h - line_h) / 2.0;
        // the selection behind the text
        if a != b && focused {
            let (sa, sb) = (xs[a] - shift, xs[b] - shift);
            let (sa, sb) = (sa.max(0.0), sb.min(room));
            let col = if self.active { t.accent.alpha(if self.dark { 0.45 } else { 0.28 }) } else { Rgba::BLACK.alpha(0.1) };
            c.fill_round_rect(tx + sa, ty + self.p(1.0), (sb - sa).max(0.0), line_h - self.p(2.0), self.p(2.0), col);
        }
        // the text, clipped to the field (a password: a dot a letter)
        if secure {
            for x0 in xs.iter().take(n) {
                let dx = x0 - shift + self.p(BULLET) / 2.0;
                if dx > 0.0 && dx < room {
                    c.fill_circle(tx + dx, ty + line_h / 2.0, self.p(3.4), t.text.fade(dim));
                }
            }
        } else if !shown.is_empty() {
            let mut clip = Canvas::new(room.ceil() as usize + 2, m.2);
            clip.fill_mask(&m.0, m.1, (-shift).round() as isize, 0, t.text.fade(dim));
            c.composite(&clip, tx.round() as isize, ty.round() as isize, 1.0);
        }
        // the caret, blinking
        if focused && a == b && self.active && !self.busy {
            let on = (now.saturating_duration_since(self.blink).as_millis() / 530).is_multiple_of(2);
            if on {
                let cx = xs.get(caret).copied().unwrap_or(0.0);
                c.fill_round_rect(tx + cx - shift - self.p(0.5), ty + self.p(1.0), self.p(2.0), line_h - self.p(2.0), self.p(1.0), t.accent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }

    #[test]
    fn typing_the_id_and_password_connects_by_id() {
        let mut v = ConnectView::new("", "relay.example.com:7470", "", false);
        assert_eq!(v.focus(), Focus::Field(ID));
        for c in "123 456 789".chars() {
            v.char(c);
        }
        assert_eq!(v.key(Key::Tab, false, false), None);
        assert_eq!(v.focus(), Focus::Field(PASSWORD));
        for c in "pa55".chars() {
            v.char(c);
        }
        match v.key(Key::Enter, false, false) {
            Some(Act::Connect { id, password, via }) => {
                assert_eq!(id, "123456789");
                assert_eq!(password, "pa55");
                assert_eq!(via, Via::Id("relay.example.com:7470".into()));
            }
            a => panic!("{a:?}"),
        }
        assert!(v.is_busy());
        v.char('x');
        assert_eq!(v.fields[PASSWORD].text(), "pa55", "nothing is typed while it connects");
        v.fail("Wrong password.");
        assert!(!v.is_busy() && v.focus() == Focus::Field(PASSWORD));
        assert_eq!(v.fields[PASSWORD].selection(), (0, 4), "the password selected, to type it again");
        let _ = v.render(later());
    }

    #[test]
    fn what_is_missing_is_said_and_focused() {
        let mut v = ConnectView::new("", "", "", false);
        assert_eq!(v.key(Key::Enter, false, false), None);
        assert_eq!(v.focus(), Focus::Field(ID));
        assert!(matches!(v.status(), Status::Error(e) if e.contains("9 digits")));
        for c in "123456789".chars() {
            v.char(c);
        }
        assert!(matches!(v.status(), Status::None), "typing clears the message");
        assert_eq!(v.key(Key::Enter, false, false), None);
        assert_eq!(v.focus(), Focus::Field(PASSWORD));
    }

    #[test]
    fn by_its_address_only_the_address_and_the_password() {
        let mut v = ConnectView::new("123456789", "relay:1", "", false);
        v.switch(true);
        assert_eq!(v.rows(), &[ADDRESS, PASSWORD], "no ID, no relay");
        assert_eq!(v.focus(), Focus::Field(ADDRESS), "the address first");
        v.set_focus(Focus::Field(PASSWORD));
        v.char('p');
        assert_eq!(v.key(Key::Enter, false, false), None);
        assert_eq!(v.focus(), Focus::Field(ADDRESS));
        assert!(matches!(v.status(), Status::Error(e) if e.contains("address")));
        for c in "192.168.1.20".chars() {
            v.char(c);
        }
        assert_eq!(v.key(Key::Tab, false, false), None);
        assert_eq!(v.focus(), Focus::Field(PASSWORD));
        assert_eq!(v.key(Key::Enter, false, false), Some(Act::Connect { id: String::new(), password: "p".into(), via: Via::Address("192.168.1.20".into()) }));
    }

    #[test]
    fn switching_keeps_what_each_mode_had() {
        let mut v = ConnectView::new("123456789", "relay:1", "mac.local", false);
        assert_eq!(v.focus(), Focus::Field(PASSWORD), "with the ID known, the password comes first");
        assert_eq!(v.rows(), &[ID, PASSWORD, RELAY]);
        v.switch(true);
        assert_eq!(v.fields[ADDRESS].text(), "mac.local");
        v.switch(false);
        assert_eq!((v.fields[ID].text(), v.fields[RELAY].text()), ("123456789", "relay:1"));
        // the segment by keyboard
        v.set_focus(Focus::Segment);
        v.key(Key::Right, false, false);
        assert!(v.by_address());
        v.key(Key::Space, false, false);
        assert!(!v.by_address());
        // a new window by address starts on the address (or the password when it is known)
        assert_eq!(ConnectView::new("", "", "", true).focus(), Focus::Field(ADDRESS));
        assert_eq!(ConnectView::new("", "", "mac.local", true).focus(), Focus::Field(PASSWORD));
    }

    #[test]
    fn clicks_and_the_clipboard() {
        let mut v = ConnectView::new("", "", "", false);
        v.set_window(460, 572, 1.0, false, Level::Full);
        let (x, y, _, h) = v.field_rect(RELAY);
        v.mouse_down(x + 60.0, y + h / 2.0, false);
        v.mouse_up(x + 60.0, y + h / 2.0);
        assert_eq!(v.focus(), Focus::Field(RELAY));
        v.paste("relay.example.com\r\n");
        assert_eq!(v.fields[RELAY].text(), "relay.example.com");
        v.key(Key::SelectAll, false, true);
        assert_eq!(v.key(Key::Copy, false, true), Some(Act::Copy("relay.example.com".into())));
        // Cancel, the red light; the zoom light does nothing
        let (cx, cy, cw, ch) = v.cancel_rect();
        v.mouse_down(cx + cw / 2.0, cy + ch / 2.0, false);
        assert_eq!(v.mouse_up(cx + cw / 2.0, cy + ch / 2.0), Some(Act::Cancel));
        let (lx, ly) = v.lights();
        v.mouse_down(lx, ly, false);
        assert_eq!(v.mouse_up(lx, ly), Some(Act::Window(Light::Close)));
        v.mouse_down(lx + 40.0, ly, false);
        assert_eq!(v.mouse_up(lx + 40.0, ly), None);
        assert!(v.is_caption(5.0, 300.0) && !v.is_caption(x + 5.0, y + 5.0));
        assert_eq!(v.next_frame(later()).map(|d| d <= Duration::from_millis(530)), Some(true), "the caret blinks");
    }

    #[test]
    fn it_draws_at_every_scale() {
        for (s, dark) in [(1.0, false), (1.5, true), (2.0, false)] {
            let mut v = ConnectView::new("123456789", "", "", false);
            v.set_window((W * s) as usize, (H * s) as usize, s, dark, Level::Full);
            v.set_error("The Mac did not answer.");
            let c = v.render(later());
            assert_eq!(c.w, (W * s) as usize);
        }
    }
}

/// Pictures of the connect window (RM_PREVIEW=dir cargo test -p rm-viewer connectui::preview).
#[cfg(test)]
mod preview {
    use super::*;

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
        let later = Instant::now() + Duration::from_secs(3);
        // light: the ID typed, the password field focused, a message
        let mut v = ConnectView::new("482 913 027", "", "", false);
        v.set_window((W * s) as usize, (H * s) as usize, s, false, Level::Full);
        v.char('h');
        v.char('u');
        v.set_error("The password did not match. Check it on the Mac and try again.");
        save(&v.render(later), &dir.join("connect-light.ppm"));
        // dark: connecting
        let mut v = ConnectView::new("482913027", "relay.example.com", "", false);
        v.set_window((W * s) as usize, (H * s) as usize, s, true, Level::Full);
        for c in "secret".chars() {
            v.char(c);
        }
        let _ = v.key(Key::Enter, false, false);
        v.set_step("Checking the password…");
        save(&v.render(later), &dir.join("connect-dark.ppm"));
        // by address, the address being typed
        let mut v = ConnectView::new("", "", "192.168.1.", true);
        v.set_window((W * s) as usize, (H * s) as usize, s, false, Level::Full);
        v.set_focus(Focus::Field(ADDRESS));
        save(&v.render(later), &dir.join("connect-address.ppm"));
    }
}
