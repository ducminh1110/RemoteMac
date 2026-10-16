//! The connect window as it looks and behaves, after Apple's Screen Sharing on macOS 26. It opens
//! on the Macs kept on this PC, one row each (its name, its ID or address, its relay); a click on
//! one connects. With none kept it says so ("No Computers") and the + adds one. Adding (or
//! editing, with the row's ⓘ) slides in a sheet: a Mac at the top, a segmented control (find the
//! Mac by its ID, or type its address), the name, the ID and the password, and "Have a custom
//! relay?" for a Mac reached through a relay of one's own (its address and its key). Save keeps
//! it and goes back to the list. The fields are Taro's own (field.rs): typing, selecting, words,
//! paste, a secure password field (cleared when focused, so a password left from before is not
//! sent again by mistake).
//!
//! Portable: it draws into a Canvas and takes plain input; connect.rs puts it in a window and
//! keeps the list (the password and the relay key protected by Windows).

use crate::field::{self, Field, Move};
use crate::glass::{self, Kind, Level, Material};
use crate::look::{self, theme, Light, Symbol, Theme};
use crate::motion::{Anim, Curve};
use crate::paint::{Canvas, Rgba};
use crate::text::{self, Style};
use std::time::{Duration, Instant};

/// The window's size (DIPs).
pub const W: f32 = 460.0;
pub const H: f32 = 600.0;
// the sheet
const FX: f32 = 48.0;
const FW: f32 = 364.0;
const FH: f32 = 42.0;
const FIELD_Y0: f32 = 222.0;
const FIELD_STEP: f32 = 50.0;
const SEG: (f32, f32, f32, f32) = (80.0, 172.0, 300.0, 32.0);
const STATUS_Y: f32 = 506.0;
const BUTTON_Y: f32 = 548.0;
const BUTTON_H: f32 = 36.0;
const CONNECT_W: f32 = 120.0;
const CANCEL_W: f32 = 100.0;
// the list
const ROW_X: f32 = 16.0;
const ROW_Y0: f32 = 76.0;
const ROW_H: f32 = 64.0;
const ROW_GAP: f32 = 8.0;
const LIST_BOTTOM: f32 = H - 70.0;
const ADD: (f32, f32, f32, f32) = (W - 52.0, 8.0, 32.0, 28.0);
const EMPTY_ADD: (f32, f32, f32, f32) = ((W - 160.0) / 2.0, 372.0, 160.0, 36.0);
/// a password dot's step (DIPs)
const BULLET: f32 = 10.0;

/// How the Mac is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// found by its ID on this network, else through this relay ("" the one built into Taro)
    Id(String),
    /// straight to this address (IP or host name, :port when not 7471)
    Address(String),
    /// the demo Mac inside Taro (the guided tour's)
    Demo,
}

/// A Mac kept on this PC.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Computer {
    /// what the user calls it ("" shows the ID or the address)
    pub name: String,
    /// its ID (9 digits) when it is found by its ID
    pub id: String,
    /// its address when it is reached straight
    pub address: String,
    pub by_address: bool,
    /// a relay of one's own ("" the one built into Taro), and that relay's key
    pub relay: String,
    pub key: String,
    pub password: String,
}

impl Computer {
    /// The row's title.
    pub fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else if self.by_address {
            self.address.clone()
        } else {
            format!("Mac {}", rm_protocol::session::display_id(&self.id))
        }
    }

    /// The row's second line: how it is reached.
    pub fn subtitle(&self) -> String {
        let how = match self.by_address {
            // (with no name its address is already the title)
            true if self.name.trim().is_empty() => "Straight to the Mac, no relay".to_string(),
            true => self.address.clone(),
            false => format!("ID {}", rm_protocol::session::display_id(&self.id)),
        };
        if !self.by_address && !self.relay.is_empty() {
            format!("{how} · via {}", self.relay)
        } else {
            how
        }
    }

    /// How it is reached, as connecting needs it.
    pub fn via(&self) -> Via {
        if self.by_address {
            Via::Address(self.address.clone())
        } else {
            Via::Id(self.relay.clone())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// connect to this Mac; `key`: the relay's key when it is a relay of one's own ("" none)
    Connect { id: String, password: String, via: Via, key: String },
    /// the list changed: keep it
    Save(Vec<Computer>),
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
    /// Ctrl+N
    New,
}

/// Where the keyboard is, in Tab's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Field(usize),
    Segment,
    /// "Have a custom relay?"
    Link,
    /// Save (the sheet)
    Connect,
    Cancel,
    /// the + (the list)
    Add,
    Row(usize),
    /// the demo Mac (during the guided tour)
    Demo,
}

/// The fields: the ID, the password, the relay (by ID), the address (by address), the name, the
/// relay's key.
pub const ID: usize = 0;
pub const PASSWORD: usize = 1;
pub const RELAY: usize = 2;
pub const ADDRESS: usize = 3;
pub const NAME: usize = 4;
pub const KEY: usize = 5;
const FIELDS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hot {
    None,
    Lights(Option<Light>),
    Segment(bool),
    Field(usize),
    Link,
    Connect,
    Cancel,
    Add,
    Row(usize),
    RowInfo(usize),
    RowRemove(usize),
    Demo,
}

/// Which page is shown: the list, or the sheet adding a Mac (None) or editing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    List,
    Edit(Option<usize>),
}

/// The places and states the guided tour (tour.rs) looks at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TourProbe {
    /// the demo Mac's row (zero when it is not shown)
    pub demo: (f32, f32, f32, f32),
    pub add: (f32, f32, f32, f32),
    pub id: (f32, f32, f32, f32),
    pub password: (f32, f32, f32, f32),
    /// Save on the sheet
    pub save: (f32, f32, f32, f32),
    /// the first Mac of the list (zero when there is none)
    pub first: (f32, f32, f32, f32),
    pub editing: bool,
    pub computers: usize,
    pub id_len: usize,
    pub password_len: usize,
    pub busy: bool,
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
/// the sheet sliding in and out
const SLIDE: Curve = Curve::Spring { response: 0.42, damping: 0.9 };

type Mask = (Vec<u8>, usize, usize);
/// What a drawn background depends on: size, scale, dark, the page (0 list, 1 add, 2 edit), by
/// address.
type BgKey = (usize, usize, u32, bool, u8, bool);

pub struct ConnectView {
    computers: Vec<Computer>,
    page: Page,
    /// 0: the list, 1: the sheet (sliding)
    flip: Anim,
    fields: [Field; FIELDS],
    by_address: bool,
    custom_relay: bool,
    /// the relay's rows fading in
    reveal: Anim,
    /// the password field shows a kept password: focusing it clears it
    pw_kept: bool,
    focus: Focus,
    status: Status,
    busy: bool,
    /// the row connecting, and the row whose attempt failed (with why)
    busy_row: Option<usize>,
    row_error: Option<(usize, String)>,
    /// the row whose ⓧ was pressed once: a second press removes it
    confirm: Option<usize>,
    /// the list scrolled by (px)
    scroll: f32,
    /// the keyboard was used last (the list shows where it is only then, as macOS does)
    keys: bool,
    /// the demo Mac is offered, above the list (during the guided tour)
    demo: bool,
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
    shift: [f32; FIELDS],
    w: usize,
    h: usize,
    s: f32,
    dark: bool,
    active: bool,
    level: Level,
    bg: [Option<(BgKey, Canvas)>; 2],
    glass: Vec<(u64, Canvas)>,
}

impl ConnectView {
    /// The Macs kept on this PC.
    pub fn new(computers: Vec<Computer>) -> ConnectView {
        let mut v = ConnectView {
            fields: [Field::new("", false, 32), Field::new("", true, 128), Field::new("", false, 255), Field::new("", false, 255), Field::new("", false, 64), Field::new("", true, 255)],
            by_address: false,
            custom_relay: false,
            reveal: Anim::at(0.0),
            pw_kept: false,
            focus: Focus::Add,
            page: Page::List,
            flip: Anim::at(0.0),
            computers,
            status: Status::None,
            busy: false,
            busy_row: None,
            row_error: None,
            confirm: None,
            scroll: 0.0,
            keys: false,
            demo: false,
            hot: Hot::None,
            down: Hot::None,
            seg: Anim::at(0.0),
            ring: Anim::at(1.0),
            press: Anim::at(1.0),
            blink: Instant::now(),
            since: Instant::now(),
            drag: None,
            shift: [0.0; FIELDS],
            w: W as usize,
            h: H as usize,
            s: 1.0,
            dark: false,
            active: true,
            level: Level::Full,
            bg: [None, None],
            glass: vec![],
        };
        if !v.computers.is_empty() {
            v.focus = Focus::Row(0);
        }
        v
    }

    pub fn set_window(&mut self, w: usize, h: usize, s: f32, dark: bool, level: Level) {
        (self.w, self.h, self.s, self.dark, self.level) = (w.max(1), h.max(1), s.max(0.5), dark, level);
    }

    pub fn set_active(&mut self, a: bool) {
        self.active = a;
    }

    /// Offer the demo Mac (the guided tour does, at its start).
    pub fn set_demo(&mut self, on: bool) {
        self.demo = on;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    pub fn computers(&self) -> &[Computer] {
        &self.computers
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// The step the connection is at (a spinner beside it).
    pub fn set_step(&mut self, label: &str) {
        self.status = Status::Busy(label.into());
    }

    /// It did not connect: say why. A password that did not match opens the Mac's sheet with the
    /// password field empty, to type it again; anything else is said on the Mac's row.
    pub fn fail(&mut self, why: &str) {
        self.busy = false;
        self.status = Status::Error(why.into());
        let Some(i) = self.busy_row.take() else { return };
        if why.to_lowercase().contains("password") {
            self.open(Some(i));
            self.status = Status::Error(why.into());
            self.fields[PASSWORD].set_text("");
            self.pw_kept = false;
            self.set_focus(Focus::Field(PASSWORD));
        } else {
            self.row_error = Some((i, why.into()));
            self.set_focus(Focus::Row(i));
        }
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

    /// The keyboard is in a text field (Space types there).
    pub fn in_field(&self) -> bool {
        matches!(self.focus, Focus::Field(_))
    }

    fn set_focus(&mut self, f: Focus) {
        if f != self.focus {
            self.focus = f;
            self.ring = Anim::new(0.0, 1.0, Duration::from_millis(260), FLOW);
            // a kept password is not edited but typed again: focusing it empties it
            if f == Focus::Field(PASSWORD) && self.pw_kept {
                self.fields[PASSWORD].set_text("");
                self.pw_kept = false;
            }
        }
        self.blink = Instant::now();
    }

    // ------------------------------------------------------------------ pages

    /// The sheet slides in, for a new Mac (None) or Mac `i`.
    pub fn open(&mut self, i: Option<usize>) {
        let c = i.and_then(|i| self.computers.get(i)).cloned().unwrap_or_default();
        self.page = Page::Edit(i.filter(|&i| i < self.computers.len()));
        self.flip.retarget(1.0, Duration::from_millis(420), SLIDE);
        self.by_address = c.by_address;
        self.seg = Anim::at(if c.by_address { 1.0 } else { 0.0 });
        self.custom_relay = !c.relay.is_empty();
        self.reveal = Anim::at(if self.custom_relay { 1.0 } else { 0.0 });
        for (f, t) in [(NAME, &c.name), (ID, &c.id), (ADDRESS, &c.address), (RELAY, &c.relay), (KEY, &c.key), (PASSWORD, &c.password)] {
            let t = if f == ID && !t.is_empty() { rm_protocol::session::display_id(t) } else { t.clone() };
            self.fields[f].set_text(&t);
        }
        self.shift = [0.0; FIELDS];
        self.pw_kept = !c.password.is_empty();
        self.status = Status::None;
        self.confirm = None;
        // the keyboard to the first field still empty (the name is optional)
        let first = self.rows().iter().copied().find(|&f| f != NAME && f != KEY && self.fields[f].is_empty());
        self.focus = Focus::Cancel; // so that set_focus below rings
        self.set_focus(Focus::Field(first.unwrap_or(if i.is_some() { NAME } else { ID })));
    }

    /// Back to the list (Mac `select` chosen).
    fn close(&mut self, select: Option<usize>) {
        self.page = Page::List;
        self.flip.retarget(0.0, Duration::from_millis(420), SLIDE);
        self.drag = None;
        if let Status::Error(_) = self.status {
            self.status = Status::None;
        }
        let f = match select {
            Some(i) if i < self.computers.len() => Focus::Row(i),
            _ if !self.computers.is_empty() => Focus::Row(0),
            _ => Focus::Add,
        };
        self.set_focus(f);
    }

    /// Keep what the sheet says, if it can be kept: else say what is missing (and go to it).
    fn save(&mut self) -> Option<Act> {
        let Page::Edit(index) = self.page else { return None };
        let password = self.fields[PASSWORD].text().to_string();
        let mut c = Computer { name: self.fields[NAME].text().trim().to_string(), password, by_address: self.by_address, ..Default::default() };
        if self.by_address {
            let address = self.fields[ADDRESS].text().trim().to_string();
            if let Err(e) = rm_relay::lan::parse_address(&address) {
                self.status = Status::Error(if address.is_empty() { "Type the Mac's IP address or name.".into() } else { e });
                self.set_focus(Focus::Field(ADDRESS));
                return None;
            }
            c.address = address;
        } else {
            let Some(id) = rm_protocol::session::normalize_id(self.fields[ID].text()) else {
                self.status = Status::Error("The ID is the 9 digits shown on the Mac.".into());
                self.set_focus(Focus::Field(ID));
                return None;
            };
            c.id = id;
            if self.custom_relay {
                let relay = self.fields[RELAY].text().trim().to_string();
                if relay.is_empty() {
                    self.status = Status::Error("Type the address of your relay (host:port).".into());
                    self.set_focus(Focus::Field(RELAY));
                    return None;
                }
                // the relay built into Taro typed out is the built-in one (with its own key)
                if Some(relay.as_str()) != rm_protocol::session::default_relay() {
                    c.relay = relay;
                    c.key = self.fields[KEY].text().trim().to_string();
                }
            }
        }
        if c.password.is_empty() {
            self.status = Status::Error("Type the password Taro shows on the Mac.".into());
            self.set_focus(Focus::Field(PASSWORD));
            return None;
        }
        let at = match index {
            Some(i) => {
                self.computers[i] = c;
                i
            }
            None => {
                self.computers.push(c);
                self.computers.len() - 1
            }
        };
        if self.row_error.as_ref().is_some_and(|(i, _)| *i == at) {
            self.row_error = None;
        }
        self.close(Some(at));
        Some(Act::Save(self.computers.clone()))
    }

    /// Connect to Mac `i` (its sheet first when it has no password).
    fn connect(&mut self, i: usize) -> Option<Act> {
        if self.busy {
            return None;
        }
        let c = self.computers.get(i)?.clone();
        if c.password.is_empty() {
            self.open(Some(i));
            self.status = Status::Error("Type the password Taro shows on the Mac.".into());
            self.set_focus(Focus::Field(PASSWORD));
            return None;
        }
        self.busy = true;
        self.busy_row = Some(i);
        self.row_error = None;
        self.confirm = None;
        self.since = Instant::now();
        self.status = Status::Busy("Connecting…".into());
        Some(Act::Connect { id: if c.by_address { String::new() } else { c.id.clone() }, password: c.password.clone(), via: c.via(), key: c.key.clone() })
    }

    /// Connect to the demo Mac.
    fn connect_demo(&mut self) -> Option<Act> {
        if self.busy || !self.demo {
            return None;
        }
        self.busy = true;
        self.busy_row = None;
        self.since = Instant::now();
        self.status = Status::Busy("Starting the demo Mac…".into());
        Some(Act::Connect { id: String::new(), password: String::new(), via: Via::Demo, key: String::new() })
    }

    /// The ⓧ of Mac `i`: once to ask, twice to remove it.
    fn remove(&mut self, i: usize) -> Option<Act> {
        if self.busy || i >= self.computers.len() {
            return None;
        }
        if self.confirm != Some(i) {
            self.confirm = Some(i);
            return None;
        }
        self.confirm = None;
        self.computers.remove(i);
        self.row_error = match self.row_error.take() {
            Some((r, _)) if r == i => None,
            Some((r, e)) if r > i => Some((r - 1, e)),
            other => other,
        };
        self.scroll = self.scroll.min(self.max_scroll());
        let f = if self.computers.is_empty() { Focus::Add } else { Focus::Row(i.min(self.computers.len() - 1)) };
        self.set_focus(f);
        Some(Act::Save(self.computers.clone()))
    }

    // ------------------------------------------------------------------ geometry (px)

    fn p(&self, v: f32) -> f32 {
        v * self.s
    }

    fn lights(&self) -> (f32, f32) {
        (self.p(20.0 + look::LIGHT_D / 2.0), self.p(22.0))
    }

    fn editing(&self) -> bool {
        matches!(self.page, Page::Edit(_))
    }

    /// The sheet's fields, top to bottom.
    pub fn rows(&self) -> &'static [usize] {
        if self.by_address {
            &[NAME, ADDRESS, PASSWORD]
        } else if self.custom_relay {
            &[NAME, ID, PASSWORD, RELAY, KEY]
        } else {
            &[NAME, ID, PASSWORD]
        }
    }

    /// Tab's order: on the list the + and the Macs; on the sheet the fields, "custom relay", the
    /// segmented control, Save, Cancel.
    fn order(&self) -> Vec<Focus> {
        if !self.editing() {
            let mut o = vec![Focus::Add];
            if self.demo {
                o.push(Focus::Demo);
            }
            o.extend((0..self.computers.len()).map(Focus::Row));
            return o;
        }
        let mut o: Vec<Focus> = self.rows().iter().map(|&i| Focus::Field(i)).collect();
        if !self.by_address {
            o.push(Focus::Link);
        }
        o.extend([Focus::Segment, Focus::Connect, Focus::Cancel]);
        o
    }

    fn field_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let row = self.rows().iter().position(|&r| r == i).unwrap_or(0);
        (self.p(FX), self.p(FIELD_Y0 + row as f32 * FIELD_STEP), self.p(FW), self.p(FH))
    }

    /// "Have a custom relay?", under the fields.
    fn link_rect(&self) -> (f32, f32, f32, f32) {
        let n = self.rows().len() as f32;
        (self.p(FX), self.p(FIELD_Y0 + (n - 1.0) * FIELD_STEP + FH + 10.0), self.p(210.0), self.p(22.0))
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

    fn add_rect(&self) -> (f32, f32, f32, f32) {
        if self.computers.is_empty() {
            (self.p(EMPTY_ADD.0), self.p(EMPTY_ADD.1), self.p(EMPTY_ADD.2), self.p(EMPTY_ADD.3))
        } else {
            (self.p(ADD.0), self.p(ADD.1), self.p(ADD.2), self.p(ADD.3))
        }
    }

    /// The toolbar's + (also there when the list is empty, beside the big button).
    fn plus_rect(&self) -> (f32, f32, f32, f32) {
        (self.p(ADD.0), self.p(ADD.1), self.p(ADD.2), self.p(ADD.3))
    }

    /// Where the first Mac's row is (lower when the demo Mac is offered above it).
    fn rows_y0(&self) -> f32 {
        ROW_Y0 + if self.demo { ROW_H + ROW_GAP } else { 0.0 }
    }

    fn row_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        (self.p(ROW_X), self.p(self.rows_y0() + i as f32 * (ROW_H + ROW_GAP)) - self.scroll, self.p(W - 2.0 * ROW_X), self.p(ROW_H))
    }

    /// The demo Mac's row (it does not scroll).
    fn demo_rect(&self) -> (f32, f32, f32, f32) {
        (self.p(ROW_X), self.p(ROW_Y0), self.p(W - 2.0 * ROW_X), self.p(ROW_H))
    }

    /// The ⓘ and ⓧ of a row (their centres) and the radius they answer within.
    fn row_buttons(&self, i: usize) -> ((f32, f32), (f32, f32), f32) {
        let (x, y, w, h) = self.row_rect(i);
        let cy = y + h / 2.0;
        ((x + w - self.p(66.0), cy), (x + w - self.p(32.0), cy), self.p(14.0))
    }

    /// The "Remove" pill a ⓧ turns into.
    fn confirm_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = self.row_rect(i);
        (x + w - self.p(96.0), y + (h - self.p(26.0)) / 2.0, self.p(80.0), self.p(26.0))
    }

    fn max_scroll(&self) -> f32 {
        let n = self.computers.len() as f32;
        let content = self.rows_y0() + n * (ROW_H + ROW_GAP) - ROW_GAP;
        self.p((content - LIST_BOTTOM).max(0.0))
    }

    /// Mac `i`'s row is at least partly in the list's view.
    fn row_shown(&self, i: usize) -> bool {
        let (_, y, _, h) = self.row_rect(i);
        y + h > self.p(self.rows_y0() - 8.0) && y < self.p(LIST_BOTTOM)
    }

    /// The list scrolled so Mac `i` is in view.
    fn reveal_row(&mut self, i: usize) {
        let (_, y, _, h) = self.row_rect(i);
        if y < self.p(self.rows_y0()) {
            self.scroll -= self.p(self.rows_y0()) - y;
        } else if y + h > self.p(LIST_BOTTOM) {
            self.scroll += y + h - self.p(LIST_BOTTOM);
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// What the guided tour points at and watches (client px).
    pub fn probe(&self) -> TourProbe {
        let typed = if self.by_address { ADDRESS } else { ID };
        let first = if self.computers.is_empty() { (0.0, 0.0, 0.0, 0.0) } else { self.row_rect(0) };
        TourProbe {
            demo: if self.demo { self.demo_rect() } else { (0.0, 0.0, 0.0, 0.0) },
            add: if self.computers.is_empty() { self.add_rect() } else { self.plus_rect() },
            id: self.field_rect(typed),
            password: self.field_rect(PASSWORD),
            save: self.connect_rect(),
            first,
            editing: self.editing(),
            computers: self.computers.len(),
            id_len: self.fields[typed].len(),
            password_len: self.fields[PASSWORD].len(),
            busy: self.busy,
        }
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
        if !self.editing() {
            if inside(self.add_rect()) || inside(self.plus_rect()) {
                return Hot::Add;
            }
            if self.demo && inside(self.demo_rect()) {
                return Hot::Demo;
            }
            if y < self.p(self.rows_y0() - 8.0) || y >= self.p(LIST_BOTTOM) {
                return Hot::None;
            }
            for i in 0..self.computers.len() {
                if !self.row_shown(i) || !inside(self.row_rect(i)) {
                    continue;
                }
                if self.confirm == Some(i) {
                    if inside(self.confirm_rect(i)) {
                        return Hot::RowRemove(i);
                    }
                } else {
                    let (info, remove, r) = self.row_buttons(i);
                    let near = |c: (f32, f32)| (x - c.0).powi(2) + (y - c.1).powi(2) <= r * r;
                    if near(info) {
                        return Hot::RowInfo(i);
                    }
                    if near(remove) {
                        return Hot::RowRemove(i);
                    }
                }
                return Hot::Row(i);
            }
            return Hot::None;
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
        if !self.by_address && inside(self.link_rect()) {
            return Hot::Link;
        }
        if inside(self.connect_rect()) {
            return Hot::Connect;
        }
        if inside(self.cancel_rect()) {
            return Hot::Cancel;
        }
        Hot::None
    }

    /// The window is moved by dragging anywhere that is not a control (as a Mac window).
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
        self.keys = false;
        self.down = self.hot;
        match self.hot {
            Hot::Field(i) => {
                self.set_focus(Focus::Field(i));
                let c = self.char_at(i, x);
                self.fields[i].place(c, shift);
                self.drag = Some(i);
            }
            Hot::Connect | Hot::Cancel | Hot::Add => self.press.retarget(0.96, Duration::from_millis(120), GIVE),
            _ => {}
        }
        if !matches!(self.hot, Hot::RowRemove(_)) {
            self.confirm = None;
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
        if matches!(down, Hot::Connect | Hot::Cancel | Hot::Add) {
            self.press.retarget(1.0, Duration::from_millis(400), BACK);
        }
        if down != self.hot {
            return None;
        }
        match down {
            Hot::Lights(Some(l)) => Some(Act::Window(l)),
            Hot::Segment(addr) => {
                self.switch(addr);
                None
            }
            Hot::Link => {
                self.toggle_relay();
                None
            }
            Hot::Connect => self.save(),
            Hot::Cancel => {
                self.close(match self.page {
                    Page::Edit(i) => i,
                    _ => None,
                });
                None
            }
            Hot::Add if !self.busy => {
                self.open(None);
                None
            }
            Hot::Row(i) => {
                self.set_focus(Focus::Row(i));
                self.connect(i)
            }
            Hot::RowInfo(i) if !self.busy => {
                self.open(Some(i));
                None
            }
            Hot::RowRemove(i) => self.remove(i),
            Hot::Demo => self.connect_demo(),
            _ => None,
        }
    }

    /// The list scrolls (a wheel's notches, positive up).
    pub fn wheel(&mut self, notches: f32) {
        if !self.editing() {
            self.scroll = (self.scroll - notches * self.p(48.0)).clamp(0.0, self.max_scroll());
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
            let first = self.rows()[1];
            if self.fields[first].is_empty() {
                self.set_focus(Focus::Field(first));
            } else if !self.rows().contains(&f) {
                self.set_focus(Focus::Field(PASSWORD));
            }
        } else if self.focus == Focus::Link && by_address {
            self.set_focus(Focus::Segment);
        }
        self.clear_error();
    }

    /// "Have a custom relay?": its address and key come in (or go, and the built-in relay is used).
    fn toggle_relay(&mut self) {
        self.custom_relay = !self.custom_relay;
        self.reveal = Anim::new(if self.custom_relay { 0.0 } else { 1.0 }, if self.custom_relay { 1.0 } else { 0.0 }, Duration::from_millis(260), Curve::Decelerate);
        if self.custom_relay {
            self.set_focus(Focus::Field(RELAY));
        } else if matches!(self.focus, Focus::Field(RELAY) | Focus::Field(KEY)) {
            self.set_focus(Focus::Link);
        }
        self.clear_error();
    }

    pub fn key(&mut self, k: Key, shift: bool, ctrl: bool) -> Option<Act> {
        self.blink = Instant::now();
        self.keys = true;
        if k != Key::Delete && k != Key::Backspace {
            self.confirm = None;
        }
        match k {
            Key::Tab => {
                let order = self.order();
                let i = order.iter().position(|f| *f == self.focus).unwrap_or(0);
                let n = order.len();
                let next = if shift { order[(i + n - 1) % n] } else { order[(i + 1) % n] };
                self.set_focus(next);
                match next {
                    Focus::Field(f) => self.fields[f].select_all(),
                    Focus::Row(r) => self.reveal_row(r),
                    _ => {}
                }
                return None;
            }
            Key::New if !self.busy => {
                self.open(None);
                return None;
            }
            _ => {}
        }
        if !self.editing() {
            let n = self.computers.len();
            match (k, self.focus) {
                (Key::Down, Focus::Add) if self.demo => self.set_focus(Focus::Demo),
                (Key::Down, Focus::Demo) if n > 0 => self.set_focus(Focus::Row(0)),
                (Key::Up, Focus::Demo) => self.set_focus(Focus::Add),
                (Key::Up, Focus::Row(0)) if self.demo => self.set_focus(Focus::Demo),
                (Key::Enter | Key::Space, Focus::Demo) => return self.connect_demo(),
                (Key::Down, Focus::Add) if n > 0 => self.set_focus(Focus::Row(0)),
                (Key::Down, Focus::Row(r)) if r + 1 < n => self.set_focus(Focus::Row(r + 1)),
                (Key::Up, Focus::Row(0)) => self.set_focus(Focus::Add),
                (Key::Up, Focus::Row(r)) => self.set_focus(Focus::Row(r - 1)),
                (Key::Home, _) if n > 0 => self.set_focus(Focus::Row(0)),
                (Key::End, _) if n > 0 => self.set_focus(Focus::Row(n - 1)),
                (Key::Enter | Key::Space, Focus::Row(r)) => return self.connect(r),
                (Key::Enter | Key::Space, Focus::Add) if !self.busy => self.open(None),
                (Key::Delete | Key::Backspace, Focus::Row(r)) => return self.remove(r),
                (Key::Escape, _) => self.confirm = None,
                _ => {}
            }
            if let Focus::Row(r) = self.focus {
                self.reveal_row(r);
            }
            return None;
        }
        match k {
            Key::Enter if self.focus != Focus::Cancel && self.focus != Focus::Link => return self.save(),
            Key::Escape => {
                self.close(match self.page {
                    Page::Edit(i) => i,
                    _ => None,
                });
                return None;
            }
            _ => {}
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
            Focus::Link if matches!(k, Key::Space | Key::Enter) => self.toggle_relay(),
            Focus::Connect if k == Key::Space => return self.save(),
            Focus::Cancel if matches!(k, Key::Space | Key::Enter) => {
                self.close(match self.page {
                    Page::Edit(i) => i,
                    _ => None,
                });
            }
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
        if c.is_control() || !self.editing() {
            return;
        }
        if let Focus::Field(i) = self.focus {
            self.fields[i].insert(&c.to_string());
            self.blink = Instant::now();
            self.clear_error();
        }
    }

    pub fn paste(&mut self, s: &str) {
        if !self.editing() {
            return;
        }
        if let Focus::Field(i) = self.focus {
            // an ID pasted with its spaces or dashes is kept as the digits
            let s = if i == PASSWORD || i == KEY { s.trim_end_matches(['\r', '\n']).to_string() } else { s.trim().to_string() };
            self.fields[i].insert(&s);
            self.clear_error();
        }
    }

    /// When the next frame is due: soon while something moves, at the caret's next blink, or
    /// never (nothing changes until the user does something).
    pub fn next_frame(&self, now: Instant) -> Option<Duration> {
        let moving = !self.seg.done_at(now) || !self.ring.done_at(now) || !self.press.done_at(now) || !self.flip.done_at(now) || !self.reveal.done_at(now) || matches!(self.status, Status::Busy(_));
        if moving {
            return Some(Duration::from_millis(16));
        }
        if matches!(self.focus, Focus::Field(_)) && self.active && self.editing() {
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

    /// What does not move on a page: the tint and, on the sheet, the Mac and its title.
    fn background(&mut self, sheet: bool) -> Canvas {
        let kind = match self.page {
            Page::Edit(Some(_)) if sheet => 2,
            _ if sheet => 1,
            _ => 0,
        };
        let key = (self.w, self.h, (self.s * 100.0) as u32, self.dark, kind, self.by_address && sheet);
        let slot = sheet as usize;
        if let Some((k, c)) = &self.bg[slot] {
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
        let s = self.s;
        if sheet {
            look::mac_icon(&mut c, self.w as f32 / 2.0, self.p(72.0), self.p(78.0), self.dark);
            let title = self.text(if kind == 2 { "Edit Mac" } else { "Add a Mac" }, 20.0, 700, self.p(380.0));
            Self::put(&mut c, &title, (self.w as f32 - title.1 as f32) / 2.0, self.p(108.0), t.text);
            let what = if self.by_address { "Type the Mac's IP address or name and the password Taro shows on it." } else { "Open Taro on the Mac, then type the ID and password it shows." };
            for (n, l) in text::wrap(what, Style::dip(13.0, 400, s), self.p(330.0), 2).iter().enumerate() {
                let m = self.text(l, 13.0, 400, self.p(340.0));
                Self::put(&mut c, &m, (self.w as f32 - m.1 as f32) / 2.0, self.p(136.0 + n as f32 * 17.0), t.text2);
            }
        } else {
            let title = self.text("Taro", 13.5, 700, self.p(200.0));
            Self::put(&mut c, &title, (self.w as f32 - title.1 as f32) / 2.0, self.p(22.0) - title.2 as f32 / 2.0, t.text);
        }
        self.bg[slot] = Some((key, c.clone()));
        c
    }

    /// A glass control (the segmented track, a button) of what is under it.
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

    /// A glass button with a label (`k`: its press scale).
    #[allow(clippy::too_many_arguments)]
    fn button(&mut self, c: &mut Canvas, slot: usize, hot: Hot, label: &str, kind: Kind, rect: (f32, f32, f32, f32), k: f32, dim: bool) {
        let t = theme(self.dark);
        let (bx, by, bw, bh) = rect;
        let (bw2, bh2) = (bw * k, bh * k);
        let (bx2, by2) = (bx + (bw - bw2) / 2.0, by + (bh - bh2) / 2.0);
        let pressed = self.down == hot && self.hot == hot;
        self.glass(c, slot, bx2, by2, bw2, bh2, kind, pressed);
        if self.hot == hot && !pressed && !dim {
            c.fill_round_rect(bx2, by2, bw2, bh2, bh2 / 2.0, Rgba::WHITE.alpha(if kind == Kind::Accent { 0.1 } else { 0.18 }));
        }
        let fg = if kind == Kind::Accent { Rgba::WHITE.fade(if dim { 0.75 } else { 1.0 }) } else { t.text };
        let m = self.text(label, 13.5, 600, bw);
        Self::put(c, &m, bx2 + (bw2 - m.1 as f32) / 2.0, by2 + (bh2 - m.2 as f32) / 2.0, fg);
    }

    fn press_of(&self, hot: Hot, now: Instant) -> f32 {
        if self.hot == hot && (self.down == hot || (self.down == Hot::None && !self.press.done_at(now))) {
            self.press.value_at(now)
        } else {
            1.0
        }
    }

    pub fn render(&mut self, now: Instant) -> Canvas {
        let k = self.flip.value_at(now).clamp(0.0, 1.0);
        let mut c = if k <= 0.002 {
            self.render_list(now)
        } else if k >= 0.998 {
            self.render_sheet(now)
        } else {
            // pushed as macOS pushes a view: the sheet comes in from the right edge over the
            // list, which steps back a third of the way and dims, a soft shadow at the seam
            let list = self.render_list(now);
            let sheet = self.render_sheet(now);
            let (w, h) = (self.w as f32, self.h as f32);
            let mut c = Canvas::new(self.w, self.h);
            look::tint(&mut c, &theme(self.dark));
            c.composite(&list, (-k * w * 0.3).round() as isize, 0, 1.0);
            c.fill_round_rect(0.0, 0.0, w, h, 0.0, Rgba::BLACK.alpha(0.12 * k));
            let x = ((1.0 - k) * w).round();
            let edge = self.p(18.0);
            c.fill_round_rect_with(x - edge, 0.0, edge, h, 0.0, |px, _| Rgba::BLACK.alpha(0.16 * ((px - (x - edge)) / edge).clamp(0.0, 1.0).powi(2)));
            c.composite(&sheet, x as isize, 0, 1.0);
            c
        };
        // the window buttons stay put (zoom grey: this window keeps its size)
        let (lx, ly) = self.lights();
        let hot_lights = matches!(self.hot, Hot::Lights(_));
        let down = match self.down {
            Hot::Lights(Some(l)) if self.hot == self.down => Some(l),
            _ => None,
        };
        look::traffic_lights(&mut c, lx, ly, self.s, self.active, hot_lights, down, false, self.dark);
        let zx = lx + 2.0 * look::LIGHT_STEP * self.s;
        c.fill_circle(zx, ly, look::LIGHT_D * self.s / 2.0, if self.dark { Rgba::rgb(0x46, 0x46, 0x4b) } else { Rgba::rgb(0xdc, 0xdc, 0xde) });
        c
    }

    /// The list of Macs (or "No Computers").
    fn render_list(&mut self, now: Instant) -> Canvas {
        let t = theme(self.dark);
        let s = self.s;
        let mut c = self.background(false);
        let on_list = !self.editing();
        // the toolbar's +
        let (ax, ay, aw, ah) = self.plus_rect();
        let k = if on_list && !self.computers.is_empty() { self.press_of(Hot::Add, now) } else { 1.0 };
        let (aw2, ah2) = (aw * k, ah * k);
        let (ax2, ay2) = (ax + (aw - aw2) / 2.0, ay + (ah - ah2) / 2.0);
        let pressed = on_list && self.down == Hot::Add && self.hot == Hot::Add;
        self.glass(&mut c, 3, ax2, ay2, aw2, ah2, Kind::Control, pressed);
        if on_list && self.hot == Hot::Add && !pressed && !self.computers.is_empty() {
            c.fill_round_rect(ax2, ay2, aw2, ah2, ah2 / 2.0, Rgba::WHITE.alpha(0.18));
        }
        look::symbol(&mut c, Symbol::Plus, ax + aw / 2.0, ay + ah / 2.0, self.p(14.0), t.text);
        if on_list && self.keys && self.focus == Focus::Add && !self.computers.is_empty() {
            self.focus_ring(&mut c, ax, ay, aw, ah, ah / 2.0, now, &t);
        }
        if self.demo {
            self.draw_demo(&mut c, now, &t);
        }
        if self.computers.is_empty() {
            let cx = self.w as f32 / 2.0;
            look::mac_icon(&mut c, cx, self.p(228.0), self.p(118.0), self.dark);
            let m = self.text("No Computers", 20.0, 700, self.p(380.0));
            Self::put(&mut c, &m, cx - m.1 as f32 / 2.0, self.p(296.0), t.text);
            for (n, l) in text::wrap("Click + to add your Mac: its ID and the password Taro shows on it.", Style::dip(13.0, 400, s), self.p(320.0), 2).iter().enumerate() {
                let m = self.text(l, 13.0, 400, self.p(340.0));
                Self::put(&mut c, &m, cx - m.1 as f32 / 2.0, self.p(326.0 + n as f32 * 17.0), t.text2);
            }
            let r = self.add_rect();
            let k = if on_list { self.press_of(Hot::Add, now) } else { 1.0 };
            self.button(&mut c, 4, Hot::Add, "Add a Mac…", Kind::Accent, r, k, false);
            if on_list && self.keys && self.focus == Focus::Add {
                self.focus_ring(&mut c, r.0, r.1, r.2, r.3, r.3 / 2.0, now, &t);
            }
        } else {
            // the rows, clipped to the list
            let (top, bottom) = (self.p(self.rows_y0() - 8.0), self.p(LIST_BOTTOM));
            let mut list = Canvas::new(self.w, (bottom - top).ceil().max(1.0) as usize);
            for i in 0..self.computers.len() {
                if self.row_shown(i) {
                    self.draw_row(&mut list, i, top, now, &t);
                }
            }
            // under the list, what to do (or what went wrong)
            c.composite(&list, 0, top.round() as isize, 1.0);
            let y = self.p(H - 52.0);
            match self.status.clone() {
                Status::Error(why) if self.busy_row.is_none() && !self.editing() => self.error_line(&mut c, &why, y, &t),
                _ => {
                    let hint = if self.busy { "Connecting to your Mac…" } else { "Click a Mac to connect. Point at it to edit or remove it." };
                    let m = self.text(hint, 12.0, 400, self.p(FW));
                    Self::put(&mut c, &m, (self.w as f32 - m.1 as f32) / 2.0, y, t.text3);
                }
            }
        }
        c
    }

    /// Mac `i`'s row, into `list` (which starts `top` px down the window).
    fn draw_row(&mut self, list: &mut Canvas, i: usize, top: f32, now: Instant, t: &Theme) {
        let (x, y, w, h) = self.row_rect(i);
        let y = y - top;
        let r = self.p(14.0);
        let on_list = !self.editing();
        let hot = on_list && matches!(self.hot, Hot::Row(j) | Hot::RowInfo(j) | Hot::RowRemove(j) if j == i);
        let pressed = on_list && self.down == Hot::Row(i) && self.hot == Hot::Row(i);
        let busy = self.busy_row == Some(i);
        let (fill, line) = if self.dark { (Rgba::WHITE.alpha(if hot { 0.11 } else { 0.07 }), Rgba::WHITE.alpha(0.12)) } else { (Rgba::WHITE.alpha(if hot { 0.98 } else { 0.86 }), Rgba::rgba(20, 40, 80, 26)) };
        list.shadow(x, y, w, h, r, self.p(8.0), self.p(2.0), Rgba::BLACK.alpha(if self.dark { 0.28 } else { 0.06 }));
        list.fill_round_rect(x - 0.5 * self.s, y - 0.5 * self.s, w + self.s, h + self.s, r + 0.5 * self.s, line);
        list.fill_round_rect(x, y, w, h, r, if pressed { fill.lerp(t.accent, 0.12) } else { fill });
        if busy {
            list.fill_round_rect(x, y, w, h, r, t.accent.alpha(0.08));
        }
        let c = self.computers[i].clone();
        look::mac_icon(list, x + self.p(40.0), y + h / 2.0 - self.p(3.0), self.p(52.0), self.dark);
        let tx = x + self.p(78.0);
        let room = w - self.p(78.0) - self.p(if hot || busy { 104.0 } else { 20.0 });
        let name = self.text(&c.title(), 14.5, 600, room);
        Self::put(list, &name, tx, y + self.p(13.0), t.text);
        let sub_y = y + self.p(35.0);
        if busy {
            let label = match &self.status {
                Status::Busy(l) => l.clone(),
                _ => "Connecting…".into(),
            };
            look::spinner(list, tx + self.p(7.0), sub_y + self.p(8.0), self.p(14.0), t.text2, now.saturating_duration_since(self.since).as_secs_f32());
            let m = self.text(&label, 12.0, 500, room - self.p(22.0));
            Self::put(list, &m, tx + self.p(20.0), sub_y, t.text2);
        } else if let Some((_, why)) = self.row_error.as_ref().filter(|(j, _)| *j == i) {
            look::symbol(list, Symbol::Warning, tx + self.p(6.0), sub_y + self.p(8.0), self.p(13.0), t.fail);
            let m = self.text(why, 12.0, 500, room - self.p(18.0));
            Self::put(list, &m, tx + self.p(18.0), sub_y, t.fail);
        } else {
            let m = self.text(&c.subtitle(), 12.0, 400, room);
            Self::put(list, &m, tx, sub_y, t.text2);
        }
        // the row's own buttons, while the pointer is over it
        if self.confirm == Some(i) {
            let (bx, by, bw, bh) = self.confirm_rect(i);
            let by = by - top;
            let red = t.fail;
            let over = on_list && self.hot == Hot::RowRemove(i);
            list.fill_round_rect(bx, by, bw, bh, bh / 2.0, if over { red } else { red.fade(0.88) });
            let m = self.text("Remove", 12.5, 600, bw);
            Self::put(list, &m, bx + (bw - m.1 as f32) / 2.0, by + (bh - m.2 as f32) / 2.0, Rgba::WHITE);
        } else if hot && !busy {
            let (info, remove, rad) = self.row_buttons(i);
            for (centre, sym, which) in [(info, Symbol::Info, Hot::RowInfo(i)), (remove, Symbol::Clear, Hot::RowRemove(i))] {
                let cy = centre.1 - top;
                if self.hot == which {
                    list.fill_circle(centre.0, cy, rad, if self.dark { Rgba::WHITE.alpha(0.12) } else { Rgba::BLACK.alpha(0.06) });
                }
                let col = if sym == Symbol::Clear { t.text3 } else { t.text2 };
                look::symbol(list, sym, centre.0, cy, self.p(17.0), col);
            }
        }
        if on_list && self.keys && self.focus == Focus::Row(i) && !hot {
            self.focus_ring(list, x, y, w, h, r, now, t);
        }
    }

    /// The demo Mac's row: an accent card that says what it is.
    fn draw_demo(&mut self, c: &mut Canvas, now: Instant, t: &Theme) {
        let (x, y, w, h) = self.demo_rect();
        let r = self.p(14.0);
        let on_list = !self.editing();
        let hot = on_list && self.hot == Hot::Demo;
        let busy = self.busy && self.busy_row.is_none();
        c.shadow(x, y, w, h, r, self.p(10.0), self.p(3.0), t.accent.alpha(if self.dark { 0.35 } else { 0.22 }));
        let (a, b) = (t.accent.lerp(Rgba::WHITE, if hot { 0.12 } else { 0.0 }), t.accent.lerp(Rgba::rgb(0x2b, 0x7b, 0xff), 0.45));
        c.fill_round_rect_with(x, y, w, h, r, |px, _| a.lerp(b, ((px - x) / w).clamp(0.0, 1.0)));
        look::mac_icon(c, x + self.p(40.0), y + h / 2.0 - self.p(3.0), self.p(52.0), true);
        let tx = x + self.p(78.0);
        let room = w - self.p(98.0);
        let name = self.text("Demo Mac", 14.5, 700, room);
        Self::put(c, &name, tx, y + self.p(13.0), Rgba::WHITE);
        let sub_y = y + self.p(35.0);
        if busy {
            let label = match &self.status {
                Status::Busy(l) => l.clone(),
                _ => "Starting…".into(),
            };
            look::spinner(c, tx + self.p(7.0), sub_y + self.p(8.0), self.p(14.0), Rgba::WHITE.alpha(0.85), now.saturating_duration_since(self.since).as_secs_f32());
            let m = self.text(&label, 12.0, 500, room - self.p(22.0));
            Self::put(c, &m, tx + self.p(20.0), sub_y, Rgba::WHITE.alpha(0.9));
        } else {
            let m = self.text("Try Taro on a pretend Mac, no Mac needed", 12.0, 500, room);
            Self::put(c, &m, tx, sub_y, Rgba::WHITE.alpha(0.88));
        }
        if on_list && self.keys && self.focus == Focus::Demo {
            self.focus_ring(c, x, y, w, h, r, now, t);
        }
    }

    fn error_line(&self, c: &mut Canvas, why: &str, y: f32, t: &Theme) {
        let lines = text::wrap(why, Style::dip(13.0, 500, self.s), self.p(FW - 40.0), 2);
        let ms: Vec<Mask> = lines.iter().map(|l| self.text(l, 13.0, 500, self.p(FW - 40.0))).collect();
        let widest = ms.iter().map(|m| m.1).max().unwrap_or(0) as f32;
        let x0 = (self.w as f32 - widest - self.p(24.0)) / 2.0;
        for (n, m) in ms.iter().enumerate() {
            let yy = y + n as f32 * self.p(17.0);
            if n == 0 {
                look::symbol(c, Symbol::Warning, x0 + self.p(8.0), yy + m.2 as f32 / 2.0, self.p(15.0), t.fail);
            }
            Self::put(c, m, x0 + self.p(24.0), yy, t.fail);
        }
    }

    /// The sheet adding or editing a Mac.
    fn render_sheet(&mut self, now: Instant) -> Canvas {
        let t = theme(self.dark);
        let mut c = self.background(true);
        let on_sheet = self.editing();
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
        if on_sheet && self.focus == Focus::Segment {
            self.focus_ring(&mut c, sx, sy, sw, sh, sh / 2.0, now, &t);
        }
        // the fields (the relay's two fade in when they come)
        let labels: [(&str, Symbol); FIELDS] = [
            ("Mac ID (9 digits)", Symbol::Number),
            ("Password", Symbol::Lock),
            ("Relay server, e.g. relay.example.com:7470", Symbol::Globe),
            ("IP address or name, e.g. 192.168.1.20", Symbol::Display),
            ("Name (optional), e.g. Studio Mac", Symbol::Window),
            ("Relay key (RM_RELAY_KEY), if it has one", Symbol::Lock),
        ];
        let reveal = self.reveal.value_at(now).clamp(0.0, 1.0);
        for &i in self.rows() {
            let (placeholder, sym) = labels[i];
            if (i == RELAY || i == KEY) && reveal < 0.999 {
                let mut layer = Canvas::new(self.w, self.h);
                self.draw_field(&mut layer, i, placeholder, sym, now, &t);
                c.composite(&layer, 0, ((1.0 - reveal) * -self.p(10.0)).round() as isize, reveal);
            } else {
                self.draw_field(&mut c, i, placeholder, sym, now, &t);
            }
        }
        // "Have a custom relay?" (by ID), or a word about going straight (by address)
        let (lx, ly, lw, lh) = self.link_rect();
        if self.by_address {
            let m = self.text("Straight to the Mac, no relay (port 7471 unless given).", 11.5, 400, self.p(FW - 16.0));
            Self::put(&mut c, &m, lx + self.p(6.0), ly + (lh - m.2 as f32) / 2.0, t.text3);
        } else {
            let label = if self.custom_relay { "Use Taro's relay" } else { "Have a custom relay?" };
            let over = on_sheet && self.hot == Hot::Link;
            let col = if over { t.accent.lerp(t.text, 0.25) } else { t.accent };
            look::symbol(&mut c, Symbol::Globe, lx + self.p(13.0), ly + lh / 2.0, self.p(13.0), col);
            let m = self.text(label, 13.0, 500, lw);
            Self::put(&mut c, &m, lx + self.p(26.0), ly + (lh - m.2 as f32) / 2.0, col);
            if over {
                c.fill_round_rect(lx + self.p(26.0), ly + (lh + m.2 as f32) / 2.0, m.1 as f32, self.p(1.0), 0.0, col.fade(0.6));
            }
            if on_sheet && self.focus == Focus::Link {
                self.focus_ring(&mut c, lx, ly, m.1 as f32 + self.p(34.0), lh, lh / 2.0, now, &t);
            }
        }
        // what went wrong
        if let Status::Error(why) = self.status.clone() {
            self.error_line(&mut c, &why, self.p(STATUS_Y), &t);
        }
        // Cancel and Save
        for (slot, (hot, label, kind, rect)) in [(Hot::Cancel, "Cancel", Kind::Control, self.cancel_rect()), (Hot::Connect, "Save", Kind::Accent, self.connect_rect())].into_iter().enumerate() {
            let k = if on_sheet { self.press_of(hot, now) } else { 1.0 };
            self.button(&mut c, 1 + slot, hot, label, kind, rect, k, false);
            let f = if hot == Hot::Connect { Focus::Connect } else { Focus::Cancel };
            if on_sheet && self.focus == f {
                let (bx, by, bw, bh) = rect;
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
        // a hairline and the fill (white in light, a raised grey in dark)
        let (fill, line) = if self.dark { (Rgba::WHITE.alpha(0.075), Rgba::WHITE.alpha(0.13)) } else { (Rgba::WHITE.alpha(0.9), Rgba::rgba(20, 40, 80, 30)) };
        c.shadow(x, y, w, h, r, self.p(4.0), self.p(1.0), Rgba::BLACK.alpha(if self.dark { 0.25 } else { 0.035 }));
        c.fill_round_rect(x - 0.5 * s, y - 0.5 * s, w + s, h + s, r + 0.5 * s, line);
        c.fill_round_rect(x, y, w, h, r, fill);
        let focused = self.editing() && self.focus == Focus::Field(i);
        if focused {
            self.focus_ring(c, x, y, w, h, r, now, t);
        }
        look::symbol(c, sym, x + self.p(20.0), y + h / 2.0, self.p(15.0), if focused { t.accent } else { t.text3 });
        let st = self.text_style();
        let tx = self.text_x(i);
        let room = self.text_room(i);
        let f = &self.fields[i];
        if f.is_empty() {
            let m = self.text(placeholder, 14.0, 400, room);
            Self::put(c, &m, tx, y + (h - m.2 as f32) / 2.0, t.text3);
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
                    c.fill_circle(tx + dx, ty + line_h / 2.0, self.p(3.4), t.text);
                }
            }
        } else if !shown.is_empty() {
            let mut clip = Canvas::new(room.ceil() as usize + 2, m.2);
            clip.fill_mask(&m.0, m.1, (-shift).round() as isize, 0, t.text);
            c.composite(&clip, tx.round() as isize, ty.round() as isize, 1.0);
        }
        // the caret, blinking
        if focused && a == b && self.active {
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

    fn typed(v: &mut ConnectView, s: &str) {
        for c in s.chars() {
            v.char(c);
        }
    }

    fn click(v: &mut ConnectView, r: (f32, f32, f32, f32)) -> Option<Act> {
        let (x, y) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        v.mouse_down(x, y, false);
        v.mouse_up(x, y)
    }

    fn mac(id: &str, pw: &str) -> Computer {
        Computer { id: id.into(), password: pw.into(), ..Default::default() }
    }

    #[test]
    fn with_none_kept_it_says_so_and_the_plus_adds_one() {
        let mut v = ConnectView::new(vec![]);
        v.set_window(460, 600, 1.0, false, Level::Full);
        assert_eq!((v.page(), v.focus()), (Page::List, Focus::Add));
        let _ = v.render(later());
        let r = v.add_rect();
        assert_eq!(click(&mut v, r), None);
        assert_eq!(v.page(), Page::Edit(None));
        assert_eq!(v.focus(), Focus::Field(ID), "the ID first (the name is optional)");
        typed(&mut v, "482 913 027");
        v.key(Key::Tab, false, false);
        assert_eq!(v.focus(), Focus::Field(PASSWORD));
        typed(&mut v, "pa55");
        let saved = v.key(Key::Enter, false, false);
        let want = vec![Computer { id: "482913027".into(), password: "pa55".into(), ..Default::default() }];
        assert_eq!(saved, Some(Act::Save(want.clone())));
        assert_eq!((v.page(), v.focus()), (Page::List, Focus::Row(0)));
        assert_eq!(v.computers(), &want[..]);
        let _ = v.render(later());
    }

    #[test]
    fn a_click_on_a_mac_connects_and_a_wrong_password_opens_its_sheet() {
        let mut v = ConnectView::new(vec![mac("123456789", "old")]);
        v.set_window(460, 600, 1.0, false, Level::Full);
        let row = v.row_rect(0);
        let (x, y) = (row.0 + 120.0, row.1 + row.3 / 2.0);
        v.mouse_down(x, y, false);
        assert_eq!(v.mouse_up(x, y), Some(Act::Connect { id: "123456789".into(), password: "old".into(), via: Via::Id(String::new()), key: String::new() }));
        assert!(v.is_busy());
        let _ = v.render(later());
        v.mouse_down(x, y, false);
        assert_eq!(v.mouse_up(x, y), None, "one attempt at a time");
        v.fail("The password did not match.");
        assert_eq!(v.page(), Page::Edit(Some(0)));
        assert_eq!((v.focus(), v.fields[PASSWORD].text()), (Focus::Field(PASSWORD), ""), "typed again, not sent again");
        typed(&mut v, "new");
        assert!(matches!(v.key(Key::Enter, false, false), Some(Act::Save(l)) if l[0].password == "new"));
        // anything else is said on the row, the list stays
        assert_eq!(v.key(Key::Enter, false, false), Some(Act::Connect { id: "123456789".into(), password: "new".into(), via: Via::Id(String::new()), key: String::new() }));
        v.fail("The Mac did not answer.");
        assert_eq!(v.page(), Page::List);
        assert!(v.row_error.is_some());
    }

    #[test]
    fn a_custom_relay_and_its_key() {
        let mut v = ConnectView::new(vec![]);
        v.open(None);
        typed(&mut v, "123456789");
        v.set_focus(Focus::Field(PASSWORD));
        typed(&mut v, "pw");
        assert_eq!(v.rows(), &[NAME, ID, PASSWORD], "the built-in relay: no relay fields");
        v.set_focus(Focus::Link);
        v.key(Key::Space, false, false);
        assert_eq!(v.rows(), &[NAME, ID, PASSWORD, RELAY, KEY]);
        assert_eq!(v.focus(), Focus::Field(RELAY));
        assert_eq!(v.key(Key::Enter, false, false), None, "a custom relay needs its address");
        assert!(matches!(v.status(), Status::Error(e) if e.contains("relay")));
        v.paste("relay.example.com:7470\r\n");
        v.key(Key::Tab, false, false);
        assert_eq!(v.focus(), Focus::Field(KEY));
        typed(&mut v, "s3cret");
        let Some(Act::Save(list)) = v.key(Key::Enter, false, false) else { panic!() };
        assert_eq!((list[0].relay.as_str(), list[0].key.as_str()), ("relay.example.com:7470", "s3cret"));
        assert_eq!(list[0].subtitle(), "ID 123 456 789 · via relay.example.com:7470");
        assert_eq!(v.key(Key::Enter, false, false), Some(Act::Connect { id: "123456789".into(), password: "pw".into(), via: Via::Id("relay.example.com:7470".into()), key: "s3cret".into() }));
    }

    #[test]
    fn by_its_address_only_the_address_and_the_password() {
        let mut v = ConnectView::new(vec![]);
        v.open(None);
        v.switch(true);
        assert_eq!(v.rows(), &[NAME, ADDRESS, PASSWORD], "no ID, no relay");
        assert_eq!(v.focus(), Focus::Field(ADDRESS), "the address first");
        v.set_focus(Focus::Field(PASSWORD));
        v.char('p');
        assert_eq!(v.key(Key::Enter, false, false), None);
        assert_eq!(v.focus(), Focus::Field(ADDRESS));
        assert!(matches!(v.status(), Status::Error(e) if e.contains("address")));
        typed(&mut v, "192.168.1.20");
        let Some(Act::Save(list)) = v.key(Key::Enter, false, false) else { panic!() };
        assert!(list[0].by_address && list[0].address == "192.168.1.20" && list[0].id.is_empty());
        assert_eq!(v.key(Key::Enter, false, false), Some(Act::Connect { id: String::new(), password: "p".into(), via: Via::Address("192.168.1.20".into()), key: String::new() }));
    }

    #[test]
    fn a_kept_password_is_cleared_when_its_field_is_focused() {
        let mut v = ConnectView::new(vec![Computer { name: "Studio".into(), ..mac("123456789", "kept") }]);
        v.open(Some(0));
        assert_eq!(v.focus(), Focus::Field(NAME), "all filled: the name first");
        assert_eq!(v.fields[PASSWORD].text(), "kept");
        // saving without touching it keeps it
        let Some(Act::Save(list)) = v.key(Key::Enter, false, false) else { panic!() };
        assert_eq!(list[0].password, "kept");
        v.open(Some(0));
        v.key(Key::Tab, false, false);
        v.key(Key::Tab, false, false);
        assert_eq!((v.focus(), v.fields[PASSWORD].text()), (Focus::Field(PASSWORD), ""));
        // Cancel leaves the Mac as it was
        v.key(Key::Escape, false, false);
        assert_eq!((v.page(), v.computers()[0].password.as_str()), (Page::List, "kept"));
    }

    #[test]
    fn a_mac_is_removed_on_the_second_press() {
        let mut v = ConnectView::new(vec![mac("111111111", "a"), mac("222222222", "b")]);
        v.set_window(460, 600, 1.0, false, Level::Full);
        let (_, remove, _) = v.row_buttons(0);
        v.mouse_move(remove.0, remove.1);
        assert_eq!(v.hot, Hot::RowRemove(0));
        v.mouse_down(remove.0, remove.1, false);
        assert_eq!(v.mouse_up(remove.0, remove.1), None, "asked first");
        let _ = v.render(later());
        let r = v.confirm_rect(0);
        assert_eq!(click(&mut v, r), Some(Act::Save(vec![mac("222222222", "b")])));
        // by keyboard: Delete twice
        v.set_focus(Focus::Row(0));
        assert_eq!(v.key(Key::Delete, false, false), None);
        assert_eq!(v.key(Key::Delete, false, false), Some(Act::Save(vec![])));
        assert_eq!(v.focus(), Focus::Add);
    }

    #[test]
    fn the_keyboard_on_the_list() {
        let mut v = ConnectView::new(vec![mac("111111111", "a"), mac("222222222", "")]);
        assert_eq!(v.focus(), Focus::Row(0));
        v.key(Key::Down, false, false);
        assert_eq!(v.focus(), Focus::Row(1));
        // no password kept: its sheet, the password asked
        assert_eq!(v.key(Key::Enter, false, false), None);
        assert_eq!((v.page(), v.focus()), (Page::Edit(Some(1)), Focus::Field(PASSWORD)));
        v.key(Key::Escape, false, false);
        assert_eq!((v.page(), v.focus()), (Page::List, Focus::Row(1)));
        v.key(Key::Up, false, false);
        v.key(Key::Up, false, false);
        assert_eq!(v.focus(), Focus::Add);
        v.key(Key::New, false, true);
        assert_eq!(v.page(), Page::Edit(None));
    }

    #[test]
    fn the_demo_mac_is_offered_during_the_tour() {
        let mut v = ConnectView::new(vec![mac("111111111", "a")]);
        v.set_window(460, 600, 1.0, false, Level::Full);
        let before = v.row_rect(0);
        v.set_demo(true);
        assert!(v.row_rect(0).1 > before.1, "the Macs move down under it");
        let r = v.probe().demo;
        assert_eq!(click(&mut v, r), Some(Act::Connect { id: String::new(), password: String::new(), via: Via::Demo, key: String::new() }));
        assert!(v.is_busy());
        let _ = v.render(later());
        v.fail("The demo Mac did not start.");
        assert_eq!(v.page(), Page::List);
        // by keyboard
        v.set_focus(Focus::Add);
        v.key(Key::Down, false, false);
        assert_eq!(v.focus(), Focus::Demo);
        assert_eq!(v.key(Key::Enter, false, false).map(|a| matches!(a, Act::Connect { via: Via::Demo, .. })), Some(true));
    }

    #[test]
    fn a_long_list_scrolls() {
        let list = (0..12).map(|i| mac(&format!("{i:09}"), "p")).collect();
        let mut v = ConnectView::new(list);
        v.set_window(460, 600, 1.0, false, Level::Full);
        assert!(!v.row_shown(11));
        v.wheel(-20.0);
        assert!(v.row_shown(11) && !v.row_shown(0));
        v.key(Key::Home, false, false);
        assert!(v.row_shown(0), "the focused row is brought into view");
    }

    #[test]
    fn clicks_and_the_clipboard() {
        let mut v = ConnectView::new(vec![]);
        v.set_window(460, 600, 1.0, false, Level::Full);
        v.open(None);
        v.toggle_relay();
        let (x, y, _, h) = v.field_rect(RELAY);
        v.mouse_down(x + 60.0, y + h / 2.0, false);
        v.mouse_up(x + 60.0, y + h / 2.0);
        assert_eq!(v.focus(), Focus::Field(RELAY));
        v.paste("relay.example.com\r\n");
        assert_eq!(v.fields[RELAY].text(), "relay.example.com");
        v.key(Key::SelectAll, false, true);
        assert_eq!(v.key(Key::Copy, false, true), Some(Act::Copy("relay.example.com".into())));
        // Cancel goes back to the list; the red light closes; the zoom light does nothing
        let r = v.cancel_rect();
        assert_eq!(click(&mut v, r), None);
        assert_eq!(v.page(), Page::List);
        let (lx, ly) = v.lights();
        v.mouse_down(lx, ly, false);
        assert_eq!(v.mouse_up(lx, ly), Some(Act::Window(Light::Close)));
        v.mouse_down(lx + 40.0, ly, false);
        assert_eq!(v.mouse_up(lx + 40.0, ly), None);
        assert!(v.is_caption(5.0, 300.0));
        assert!(v.next_frame(later()).is_none() || v.next_frame(later()).is_some_and(|d| d <= Duration::from_millis(530)));
    }

    #[test]
    fn it_draws_at_every_scale() {
        for (s, dark) in [(1.0, false), (1.5, true), (2.0, false)] {
            let mut v = ConnectView::new(vec![mac("123456789", "x"), Computer { name: "Studio".into(), relay: "r:1".into(), ..mac("987654321", "y") }]);
            v.set_window((W * s) as usize, (H * s) as usize, s, dark, Level::Full);
            v.set_error("The Mac did not answer.");
            let c = v.render(later());
            assert_eq!(c.w, (W * s) as usize);
            v.open(Some(1));
            let mid = Instant::now() + Duration::from_millis(80);
            let _ = v.render(mid);
            let c = v.render(later());
            assert_eq!(c.h, (H * s) as usize);
            let _ = ConnectView::new(vec![]).render(later());
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
        let size = |v: &mut ConnectView, dark: bool| v.set_window((W * s) as usize, (H * s) as usize, s, dark, Level::Full);
        // no Macs yet
        let mut v = ConnectView::new(vec![]);
        size(&mut v, false);
        save(&v.render(later), &dir.join("connect-empty.ppm"));
        // a few Macs, one connecting, the pointer over another
        let macs = vec![
            Computer { name: "Studio Mac".into(), ..Computer { id: "482913027".into(), password: "x".into(), ..Default::default() } },
            Computer { name: "MacBook Pro".into(), relay: "relay.example.com:7470".into(), key: "k".into(), ..Computer { id: "120554731".into(), password: "x".into(), ..Default::default() } },
            Computer { by_address: true, address: "192.168.1.20".into(), password: "x".into(), ..Default::default() },
        ];
        let mut v = ConnectView::new(macs.clone());
        size(&mut v, false);
        let r = v.row_rect(1);
        v.mouse_move(r.0 + 100.0, r.1 + 20.0);
        save(&v.render(later), &dir.join("connect-list.ppm"));
        let mut v = ConnectView::new(macs.clone());
        size(&mut v, true);
        let _ = v.key(Key::Enter, false, false);
        v.set_step("Checking the password…");
        save(&v.render(later), &dir.join("connect-list-dark.ppm"));
        // adding one, with a custom relay
        let mut v = ConnectView::new(macs);
        size(&mut v, false);
        v.open(None);
        for c in "482 913 027".chars() {
            v.char(c);
        }
        v.toggle_relay();
        for c in "relay.example.com:7470".chars() {
            v.char(c);
        }
        save(&v.render(later), &dir.join("connect-add.ppm"));
        // the guided tour's demo Mac
        let mut v = ConnectView::new(vec![]);
        size(&mut v, false);
        v.set_demo(true);
        save(&v.render(later), &dir.join("connect-demo.ppm"));
        // the sheet half way in
        let mut v = ConnectView::new(vec![]);
        size(&mut v, true);
        v.open(None);
        save(&v.render(Instant::now() + Duration::from_millis(90)), &dir.join("connect-slide.ppm"));
    }
}
