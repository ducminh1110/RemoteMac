//! The Demo Mac that ships inside Taro: a few Mac apps (Notes, TextEdit, Calculator) drawn here
//! and served by rm-fakeagent's demo mode behind a relay on this PC, so a new user can try Taro
//! without a Mac. The viewer connects to it like to any Mac ([`start`] says how); its windows
//! come as H.264 like a real Mac's, title bar and buttons included.
//!
//! The pictures are drawn with the viewer's own painter (paint.rs, text.rs, look.rs) after
//! macOS 26, and are portable: they are tested and previewed on any system.

use crate::look::{self, Symbol};
use crate::paint::{Canvas, Rgba};
use crate::text::{self, Style};
use rm_fakeagent::demo::{Demo, DemoApp, Input, Scene};
use rm_protocol::MenuNode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the viewer connects to: `net::connect_with(Some(&relay), &session, &token, None, false, wake)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DemoMac {
    pub relay: String,
    pub session: String,
    pub token: String,
}

/// The demo's apps (id, name), in Dock order.
pub const APPS: [(&str, &str); 3] = [("notes", "Notes"), ("textedit", "TextEdit"), ("calculator", "Calculator")];

/// Demo sessions are named so the viewer can tell one ("Demo Mac" rather than "Your Mac").
pub const SESSION_PREFIX: &str = "taro-demo-";

pub fn is_demo(session: &str) -> bool {
    session.starts_with(SESSION_PREFIX)
}

/// Start the Demo Mac (once per process; later calls return the same one): a relay on
/// 127.0.0.1 and the demo agent waiting at it under a fresh session. It waits again after each
/// viewer leaves, so a reconnect finds it.
pub fn start() -> Result<DemoMac, String> {
    static RUNNING: Mutex<Option<DemoMac>> = Mutex::new(None);
    let mut running = RUNNING.lock().unwrap();
    if let Some(d) = running.as_ref() {
        return Ok(d.clone());
    }
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("demo relay: {e}"))?;
    let relay = l.local_addr().map_err(|e| e.to_string())?.to_string();
    std::thread::Builder::new().name("taro-demo-relay".into()).spawn(move || rm_relay::serve(l, Default::default())).map_err(|e| e.to_string())?;
    let mac = DemoMac { relay, session: format!("{SESSION_PREFIX}{}", random_hex(8)), token: random_hex(24) };
    let m = mac.clone();
    std::thread::Builder::new()
        .name("taro-demo-mac".into())
        .spawn(move || loop {
            let began = Instant::now();
            if let Err(e) = rm_fakeagent::demo::serve_demo_via_relay(&m.relay, &m.session, &m.token, demo()) {
                eprintln!("demo Mac: {e}");
            }
            // waiting again at once for the next viewer, but never spinning on a relay that fails
            if began.elapsed() < Duration::from_secs(1) {
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .map_err(|e| e.to_string())?;
    *running = Some(mac.clone());
    Ok(mac)
}

/// `n` random bytes as hex (the session's password only guards a relay on this PC; the
/// standard library's random hash keys are enough for it).
fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    while out.len() < n * 2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(t ^ out.len() as u64);
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(n * 2);
    out
}

// ------------------------------------------------------------------ the apps

/// Title bars and toolbars (points): Notes' toolbar is part of its title band.
const NOTES_BAR: u32 = 52;
const BAR: u32 = 28;

fn item(title: &str, shortcut: Option<&str>) -> MenuNode {
    MenuNode { title: title.into(), enabled: true, shortcut: shortcut.map(String::from), ..Default::default() }
}

fn sep() -> MenuNode {
    MenuNode { separator: true, ..Default::default() }
}

fn menu(title: &str, children: Vec<MenuNode>) -> MenuNode {
    MenuNode { title: title.into(), enabled: true, children, ..Default::default() }
}

/// A Mac app's menu bar: its own menu, File (its items), Edit, View (its items), Window, Help.
fn menus(name: &str, file: Vec<MenuNode>, view: Vec<MenuNode>) -> Vec<MenuNode> {
    vec![
        menu(name, vec![item(&format!("About {name}"), None), sep(), item("Settings…", Some("Cmd+,")), sep(), item(&format!("Hide {name}"), Some("Cmd+H")), item(&format!("Quit {name}"), Some("Cmd+Q"))]),
        menu("File", file),
        menu("Edit", vec![item("Undo", Some("Cmd+Z")), item("Redo", Some("Shift+Cmd+Z")), sep(), item("Cut", Some("Cmd+X")), item("Copy", Some("Cmd+C")), item("Paste", Some("Cmd+V")), item("Select All", Some("Cmd+A"))]),
        menu("View", view),
        menu("Window", vec![item("Minimize", Some("Cmd+M")), item("Zoom", None)]),
        menu("Help", vec![item(&format!("{name} Help"), None)]),
    ]
}

fn apps() -> Vec<DemoApp> {
    let app = |id: &str, name: &str, title: &str, size, title_height, corner, resizable, caret, menus| DemoApp { id: id.into(), name: name.into(), title: title.into(), size, title_height, corner, resizable, caret, menus };
    vec![
        app(
            "notes",
            "Notes",
            "Notes",
            (720, 480),
            NOTES_BAR,
            16.0,
            true,
            true,
            menus("Notes", vec![item("New Note", Some("Cmd+N")), item("New Folder", Some("Shift+Cmd+N")), sep(), item("Close", Some("Cmd+W"))], vec![item("as List", None), item("as Gallery", None), sep(), item("Show Folders", None), sep(), item("Zoom In", Some("Cmd++")), item("Zoom Out", Some("Cmd+-"))]),
        ),
        app(
            "textedit",
            "TextEdit",
            "Untitled",
            (720, 480),
            BAR,
            12.0,
            true,
            true,
            menus("TextEdit", vec![item("New", Some("Cmd+N")), item("Open…", Some("Cmd+O")), sep(), item("Close", Some("Cmd+W")), item("Save…", Some("Cmd+S"))], vec![item("Actual Size", Some("Cmd+0")), item("Zoom In", Some("Cmd+>")), item("Zoom Out", Some("Cmd+<")), sep(), item("Show Ruler", None)]),
        ),
        app(
            "calculator",
            "Calculator",
            "Calculator",
            (CALC_W as u32, CALC_H as u32),
            BAR,
            20.0,
            false,
            false,
            menus("Calculator", vec![item("Close", Some("Cmd+W"))], vec![item("Basic", Some("Cmd+1")), item("Scientific", Some("Cmd+2")), item("Programmer", Some("Cmd+3")), sep(), item("Show Thousands Separators", None)]),
        ),
    ]
}

/// The Demo Mac's apps and how they look, for rm-fakeagent's demo mode.
pub fn demo() -> Demo {
    Demo { apps: apps(), paint: Arc::new(paint), icon: Arc::new(icon), input: Some(Arc::new(input)) }
}

/// Input an app takes itself: Calculator's keys, clicked or typed.
fn input(app: &str, i: &Input) -> Option<String> {
    if app != "calculator" {
        return None;
    }
    match i {
        Input::Click { x, y } => calc_key_at(*x as f32, *y as f32).map(|k| k.code.to_string()),
        Input::Key("Escape") => Some("c".into()),
        Input::Key("Backspace") | Input::Key("Delete") => Some("d".into()),
        Input::Key("Enter") | Input::Key("NumpadEnter") => Some("=".into()),
        Input::Key(_) => None,
        Input::Text(t) => Some(t.chars().filter_map(|c| match c {
            '0'..='9' | '.' | '+' | '-' | '*' | '/' | '=' | '%' => Some(c),
            ',' => Some('.'),
            'x' | 'X' | '×' => Some('*'),
            '÷' => Some('/'),
            '−' => Some('-'),
            'c' | 'C' => Some('c'),
            _ => None,
        }).collect()),
    }
}

// ------------------------------------------------------------------ drawing helpers

/// The system's colours (light appearance), as macOS 26 draws them.
const INK: Rgba = Rgba::rgb(0x1d, 0x1d, 0x1f);
const INK2: Rgba = Rgba::rgb(0x6e, 0x6e, 0x73);
const INK3: Rgba = Rgba::rgb(0xa1, 0xa1, 0xa6);
const HAIRLINE: Rgba = Rgba::rgba(0, 0, 0, 26);
const BLUE: Rgba = Rgba::rgb(0x00, 0x7a, 0xff);
const SELECTION: Rgba = Rgba::rgb(0xb3, 0xd7, 0xff);
const SELECTION_OFF: Rgba = Rgba::rgb(0xdc, 0xdc, 0xdc);
const NOTES_YELLOW: Rgba = Rgba::rgb(0xff, 0xd5, 0x4a);
const NOTES_CARET: Rgba = Rgba::rgb(0xe8, 0xa8, 0x00);
const ORANGE: Rgba = Rgba::rgb(0xff, 0x9f, 0x0a);

/// Draws in points on a canvas `s` pixels per point.
struct Pen<'a> {
    c: &'a mut Canvas,
    s: f32,
}

impl Pen<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, col: Rgba) {
        let s = self.s;
        self.c.fill_round_rect(x * s, y * s, w * s, h * s, r * s, col);
    }

    /// A hairline: one device pixel, wherever it falls.
    fn hline(&mut self, x: f32, y: f32, w: f32, col: Rgba) {
        let s = self.s;
        self.c.fill_round_rect(x * s, (y * s).round(), w * s, 1.0, 0.0, col);
    }

    fn vline(&mut self, x: f32, y: f32, h: f32, col: Rgba) {
        let s = self.s;
        self.c.fill_round_rect((x * s).round(), y * s, 1.0, h * s, 0.0, col);
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, col: Rgba) {
        let s = self.s;
        self.c.stroke_round_rect_with(x * s, y * s, w * s, h * s, r * s, (width * s).max(1.0), |_, _| col);
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, col: Rgba) {
        let s = self.s;
        self.c.fill_capsule(x0 * s, y0 * s, x1 * s, y1 * s, width * s / 2.0, col);
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, col: Rgba) {
        let s = self.s;
        self.c.fill_circle(cx * s, cy * s, r * s, col);
    }

    fn style(&self, size: f32, weight: i32) -> Style {
        Style::dip(size, weight, self.s)
    }

    fn width(&self, t: &str, size: f32, weight: i32) -> f32 {
        text::width(t, self.style(size, weight)) / self.s
    }

    /// `t` with its baseline at `y`, starting at `x` (cut with an ellipsis at `max_w`); its width.
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, t: &str, size: f32, weight: i32, x: f32, y: f32, col: Rgba, max_w: f32) -> f32 {
        if t.is_empty() {
            return 0.0;
        }
        let s = self.s;
        let st = self.style(size, weight);
        let asc = text::layout("", st).ascent.round();
        let m = text::line(t, st, max_w * s);
        self.c.fill_mask(&m.0, m.1, (x * s).round() as isize, ((y * s).round() - asc) as isize, col);
        m.1 as f32 / s
    }

    #[allow(clippy::too_many_arguments)]
    fn text_right(&mut self, t: &str, size: f32, weight: i32, right: f32, y: f32, col: Rgba) {
        let w = self.width(t, size, weight);
        self.text(t, size, weight, right - w, y, col, w + 4.0);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_center(&mut self, t: &str, size: f32, weight: i32, cx: f32, y: f32, col: Rgba) {
        let w = self.width(t, size, weight);
        self.text(t, size, weight, cx - w / 2.0, y, col, w + 4.0);
    }

    /// A soft shadow under a rounded rectangle (points).
    #[allow(clippy::too_many_arguments)]
    fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, spread: f32, dy: f32, col: Rgba) {
        let s = self.s;
        self.c.shadow(x * s, y * s, w * s, h * s, r * s, spread * s, dy * s, col);
    }

    /// The window's buttons where rm-fakeagent's demo mode says they are (`demo::lights`).
    fn lights(&mut self, title_height: u32, active: bool, dark: bool) {
        let r = rm_fakeagent::demo::lights(title_height)[0];
        let s = self.s;
        let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
        look::traffic_lights(self.c, cx * s, cy * s, s, active, false, None, false, dark);
    }
}

/// Text broken into lines of at most `max_w` points: at the line breaks typed, then between
/// words (spaces kept, so the caret stands after them), a word too long cut where it fills.
fn wrap(t: &str, size: f32, weight: i32, s: f32, max_w: f32) -> Vec<String> {
    type Key = (String, u32, i32, u32, u32);
    thread_local! {
        /// paragraphs already broken (a window is drawn again on every key and blink; only the
        /// paragraph being typed changes)
        static DONE: std::cell::RefCell<std::collections::HashMap<Key, Vec<String>>> = Default::default();
    }
    let mut out = vec![];
    for para in t.split('\n') {
        let key = (para.to_string(), size.to_bits(), weight, s.to_bits(), max_w.to_bits());
        let lines = DONE.with(|d| d.borrow().get(&key).cloned()).unwrap_or_else(|| {
            let l = wrap_paragraph(para, size, weight, s, max_w);
            DONE.with(|d| {
                let mut d = d.borrow_mut();
                if d.len() > 512 {
                    d.clear();
                }
                d.insert(key, l.clone());
            });
            l
        });
        out.extend(lines);
    }
    out
}

fn wrap_paragraph(para: &str, size: f32, weight: i32, s: f32, max_w: f32) -> Vec<String> {
    let st = Style::dip(size, weight, s);
    let fits = |l: &str| text::width(l, st) / s <= max_w;
    let mut out = vec![];
    {
        let mut line = String::new();
        // words with the spaces after them
        let mut words: Vec<String> = vec![];
        for c in para.chars() {
            let space_before = words.last().is_some_and(|w| w.ends_with(' '));
            if c != ' ' && (words.is_empty() || space_before) {
                words.push(String::new());
            }
            if words.is_empty() {
                words.push(String::new());
            }
            words.last_mut().unwrap().push(c);
        }
        for w in words {
            let next = format!("{line}{w}");
            if line.is_empty() || fits(next.trim_end()) {
                line = next;
            } else {
                out.push(std::mem::take(&mut line));
                line = w;
            }
            // one word wider than the line: cut it where it no longer fits
            while !fits(line.trim_end()) && line.chars().count() > 1 {
                let mut cut = String::new();
                for c in line.chars() {
                    if !cut.is_empty() && !fits(&format!("{cut}{c}")) {
                        break;
                    }
                    cut.push(c);
                }
                let rest = line[cut.len()..].to_string();
                if rest.is_empty() {
                    break;
                }
                out.push(cut);
                line = rest;
            }
        }
        out.push(line);
    }
    out
}

/// The caret is shown: in the active window, the first half of each blink.
fn caret_on(sc: &Scene) -> bool {
    sc.active && !sc.selected && ((sc.since_input / rm_fakeagent::demo::BLINK) as u64).is_multiple_of(2)
}

/// Lines of what was typed, the last ones only when there are very many (the view follows the
/// caret, so earlier ones are scrolled away anyway).
fn typed_tail(t: &str) -> &str {
    const KEEP: usize = 4000;
    if t.len() <= KEEP {
        return t;
    }
    let mut start = t.len() - KEEP;
    while !t.is_char_boundary(start) {
        start += 1;
    }
    let from = t[start..].find('\n').map_or(start, |i| start + i + 1);
    &t[from..]
}

/// Lines of body text from `top`, scrolled up so the last stays above `bottom`, with what was
/// selected highlighted and the caret after the last character. Returns nothing.
#[allow(clippy::too_many_arguments)]
fn body(p: &mut Pen, lines: &[(String, f32, i32, Rgba, bool)], x: f32, top: f32, bottom: f32, lead: f32, sc: &Scene, caret: Rgba) {
    // (text, size, weight, colour, typed): each line `lead` × its size below the one before
    let height: f32 = lines.iter().map(|l| l.1 * lead).sum();
    let shift = (top + height - bottom).max(0.0);
    let mut y = top - shift;
    let mut end = (x, top, 13.0);
    for (i, (t, size, weight, col, typed)) in lines.iter().enumerate() {
        let h = size * lead;
        // what is scrolled away is not drawn
        if y + h > top - 40.0 && y < bottom + 40.0 {
            let base = y + size * 1.02;
            if sc.selected && *typed {
                let w = p.width(t, *size, *weight).max(4.0);
                p.rect(x - 1.0, y + 1.0, w + 2.0, h, 0.0, if sc.active { SELECTION } else { SELECTION_OFF });
            }
            p.text(t, *size, *weight, x, base, *col, 2000.0);
        }
        if i + 1 == lines.len() {
            end = (x + p.width(t, *size, *weight), y, *size);
        }
        y += h;
    }
    if caret_on(sc) {
        let (cx, cy, size) = end;
        p.rect(cx + 0.5, cy + size * 0.12, 1.6, size * 1.22, 0.8, caret);
    }
}

/// A Mac window's edge: a hairline just inside it.
fn rim(p: &mut Pen, w: f32, h: f32, r: f32, col: Rgba) {
    p.stroke(0.0, 0.0, w, h, r, 1.0 / p.s, col);
}

// ------------------------------------------------------------------ the windows

/// A window's picture (straight RGBA, as rm-fakeagent takes it).
pub fn paint(sc: &Scene) -> Vec<u8> {
    rgba(&draw(sc))
}

/// A window's picture as a canvas (premultiplied BGRA).
pub fn draw(sc: &Scene) -> Canvas {
    let s = sc.scale.max(0.5);
    let (w, h) = (sc.width as f32 / s, sc.height as f32 / s);
    // what does not change as the window is typed into is drawn once (per size, state)
    let calc = (sc.app == "calculator").then(|| Calc::run(sc.text));
    let variant = match &calc {
        Some(c) => format!("{} {:?}", c.typing, c.pending()),
        None => (sc.text.is_empty()).to_string(),
    };
    let key = format!("{} {}x{} {s} {} {variant}", sc.app, sc.width, sc.height, sc.active);
    let mut c = background(key, || {
        let mut c = Canvas::filled(sc.width, sc.height, Rgba::WHITE);
        let mut p = Pen { c: &mut c, s };
        match sc.app {
            "notes" => notes_back(&mut p, w, h, sc.active),
            "textedit" => textedit_back(&mut p, w, h, sc.active, !sc.text.is_empty()),
            "calculator" => calculator_back(&mut p, w, h, sc.active, calc.as_ref().unwrap_or(&Calc::default())),
            _ => {}
        }
        c
    });
    let mut p = Pen { c: &mut c, s };
    match (sc.app, &calc) {
        ("notes", _) => notes_text(&mut p, w, h, sc),
        ("textedit", _) => textedit_text(&mut p, w, h, sc),
        (_, Some(calc)) => calculator_display(&mut p, w, calc),
        _ => {}
    }
    c
}

/// A window's unchanging part, drawn once by `make` and kept (a few, for this thread: each
/// Demo Mac session draws on its own).
fn background(key: String, make: impl FnOnce() -> Canvas) -> Canvas {
    thread_local! {
        static KEPT: std::cell::RefCell<Vec<(String, Canvas)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    if let Some(c) = KEPT.with(|k| k.borrow().iter().find(|(x, _)| *x == key).map(|(_, c)| c.clone())) {
        return c;
    }
    let c = make();
    KEPT.with(|k| {
        let mut k = k.borrow_mut();
        if k.len() >= 12 {
            k.remove(0);
        }
        k.push((key, c.clone()));
    });
    c
}

fn rgba(c: &Canvas) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.w * c.h * 4);
    for p in &c.px {
        let a = p[3] as u32;
        if a == 255 {
            // a window is opaque all over: nothing to divide
            out.extend_from_slice(&[p[2], p[1], p[0], 255]);
            continue;
        }
        let un = |v: u8| ((v as u32 * 255 + a / 2).checked_div(a).unwrap_or(0)).min(255) as u8;
        out.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), p[3]]);
    }
    out
}

/// The demo notes: title, when, the first line after the title.
const NOTES: [(&str, &str, &str); 5] = [
    ("Welcome to Taro", "9:41 AM", "Your Mac's apps, each in its own window"),
    ("Groceries", "Yesterday", "Oat milk, apples, coffee beans, basil"),
    ("Trip to Kyoto", "Tuesday", "Cherry blossoms at the Philosopher's Path"),
    ("Book club", "Sunday", "The Design of Everyday Things, ch. 4"),
    ("Ideas", "10/9/26", "A tiny garden on the balcony"),
];

/// The welcome note's text after its title (an empty line between paragraphs).
const WELCOME: [&str; 5] = [
    "This is a Demo Mac, built into Taro. Its windows come to this PC as video, the way your own Mac's apps will.",
    "",
    "Try it: click here and type. What you type goes to the Mac, and the Mac's picture comes back.",
    "",
    "TextEdit and Calculator are in the Dock too.",
];

/// Notes' floating sidebar (x, width).
const SIDEBAR: (f32, f32) = (8.0, 236.0);

fn notes_back(p: &mut Pen, w: f32, h: f32, active: bool) {
    let bar = NOTES_BAR as f32;
    // the editor's page (the canvas is white), the floating sidebar on it
    let (sx, sw) = SIDEBAR;
    let (sy, sh) = (8.0, h - 16.0);
    p.shadow(sx, sy, sw, sh, 18.0, 10.0, 1.0, Rgba::BLACK.alpha(0.08));
    p.rect(sx, sy, sw, sh, 18.0, Rgba::rgb(0xf3, 0xf3, 0xf5));
    p.stroke(sx, sy, sw, sh, 18.0, 1.0 / p.s, Rgba::BLACK.alpha(0.06));
    p.lights(NOTES_BAR, active, false);
    // the list's own button (list / gallery), at the sidebar's top right
    capsule(p, sx + sw - 46.0, 9.0, 34.0, 34.0);
    list_glyph(p, sx + sw - 29.0, 26.0, INK);
    // the notes, newest first, the welcome note selected
    let x = sx + 14.0;
    let mut y = bar + 18.0;
    p.text("Today", 11.0, 650, x + 2.0, y, INK2, sw);
    y += 8.0;
    for (i, (title, when, first)) in NOTES.iter().enumerate() {
        if i == 1 {
            y += 22.0;
            p.text("Previous 7 Days", 11.0, 650, x + 2.0, y, INK2, sw);
            y += 8.0;
        }
        let row = 50.0;
        if y + row > h - 12.0 {
            break;
        }
        if i == 0 {
            p.rect(sx + 6.0, y, sw - 12.0, row, 10.0, if active { NOTES_YELLOW } else { Rgba::rgb(0xe2, 0xe2, 0xe5) });
        } else if i != 1 {
            p.hline(x + 2.0, y, sw - 34.0, HAIRLINE);
        }
        p.text(&crate::brand::named(title), 13.0, 650, x + 2.0, y + 21.0, INK, sw - 32.0);
        let tw = p.text(when, 11.5, 500, x + 2.0, y + 38.0, if i == 0 { INK } else { INK2 }, 80.0);
        p.text(first, 11.5, 400, x + 2.0 + tw + 6.0, y + 38.0, if i == 0 { INK.alpha(0.7) } else { INK2 }, sw - 40.0 - tw);
        y += row;
    }
    // the editor's toolbar: Liquid Glass capsules floating at the top right
    let right = w - 12.0;
    let tools = [(Tool::Search, 34.0), (Tool::Compose, 34.0), (Tool::Share, 34.0)];
    let mut tx = right;
    for (tool, tw) in tools {
        tx -= tw;
        capsule(p, tx, 9.0, tw, 34.0);
        tool_glyph(p, tool, tx + tw / 2.0, 26.0);
        tx -= 8.0;
    }
    let group = [Tool::Format, Tool::Checklist, Tool::Table];
    let gw = 36.0 * group.len() as f32 + 8.0;
    tx -= gw;
    capsule(p, tx, 9.0, gw, 34.0);
    for (i, tool) in group.into_iter().enumerate() {
        tool_glyph(p, tool, tx + 4.0 + 18.0 + 36.0 * i as f32, 26.0);
    }
}

/// The note: when it was edited, its title, its text, and what was typed after it.
fn notes_text(p: &mut Pen, w: f32, h: f32, sc: &Scene) {
    let bar = NOTES_BAR as f32;
    let ex = SIDEBAR.0 + SIDEBAR.1 + 28.0;
    let ew = w - ex - 28.0;
    p.text_center("October 16, 2026 at 9:41 AM", 11.0, 500, ex + ew / 2.0, bar + 14.0, INK3);
    // the title, a little room under it, the text
    let mut lines: Vec<(String, f32, i32, Rgba, bool)> = vec![(crate::brand::named(NOTES[0].0).into_owned(), 22.0, 700, INK, false), (String::new(), 4.0, 400, INK, false)];
    for para in WELCOME {
        for l in wrap(&crate::brand::named(para), 13.5, 400, p.s, ew) {
            lines.push((l, 13.5, 400, INK, false));
        }
    }
    if !sc.text.is_empty() {
        lines.push((String::new(), 13.5, 400, INK, false));
        for l in wrap(typed_tail(sc.text), 13.5, 400, p.s, ew) {
            lines.push((l, 13.5, 400, INK, true));
        }
    }
    body(p, &lines, ex, bar + 24.0, h - 18.0, 1.42, sc, NOTES_CARET);
}

/// A Liquid Glass capsule on a light window: white, a hairline rim, a soft shadow.
fn capsule(p: &mut Pen, x: f32, y: f32, w: f32, h: f32) {
    p.shadow(x, y, w, h, h / 2.0, 6.0, 1.5, Rgba::BLACK.alpha(0.10));
    p.rect(x, y, w, h, h / 2.0, Rgba::WHITE);
    p.stroke(x, y, w, h, h / 2.0, 1.0 / p.s, Rgba::BLACK.alpha(0.07));
}

#[derive(Clone, Copy)]
enum Tool {
    Format,
    Checklist,
    Table,
    Share,
    Compose,
    Search,
}

/// The list button's glyph ("list.bullet.rectangle", simplified).
fn list_glyph(p: &mut Pen, cx: f32, cy: f32, col: Rgba) {
    for i in -1..=1 {
        let y = cy + i as f32 * 4.5;
        p.circle(cx - 6.0, y, 1.2, col);
        p.line(cx - 2.5, y, cx + 7.0, y, 1.5, col);
    }
}

/// SF Symbols' toolbar glyphs, drawn small (about 16 points).
fn tool_glyph(p: &mut Pen, t: Tool, cx: f32, cy: f32) {
    let col = Rgba::rgb(0x3a, 0x3a, 0x3c);
    match t {
        Tool::Format => {
            p.text_center("Aa", 14.0, 500, cx, cy + 5.0, col);
        }
        Tool::Checklist => {
            for (i, dy) in [-4.0f32, 4.0].into_iter().enumerate() {
                let y = cy + dy;
                p.stroke(cx - 8.0, y - 2.6, 5.2, 5.2, 2.6, 1.2, col);
                if i == 0 {
                    p.line(cx - 6.6, y, cx - 5.6, y + 1.0, 1.1, col);
                    p.line(cx - 5.6, y + 1.0, cx - 3.9, y - 1.2, 1.1, col);
                }
                p.line(cx - 0.5, y, cx + 8.0, y, 1.5, col);
            }
        }
        Tool::Table => {
            p.stroke(cx - 8.0, cy - 6.5, 16.0, 13.0, 2.5, 1.4, col);
            p.hline(cx - 8.0, cy - 2.0, 16.0, col);
            p.hline(cx - 8.0, cy + 2.4, 16.0, col);
            p.vline(cx - 2.6, cy - 6.5, 13.0, col);
            p.vline(cx + 2.6, cy - 6.5, 13.0, col);
        }
        Tool::Share => {
            p.stroke(cx - 6.0, cy - 2.0, 12.0, 10.0, 2.0, 1.4, col);
            p.rect(cx - 2.6, cy - 2.6, 5.2, 2.0, 0.0, Rgba::WHITE);
            p.line(cx, cy - 8.0, cx, cy + 2.6, 1.5, col);
            p.line(cx - 3.0, cy - 5.2, cx, cy - 8.2, 1.5, col);
            p.line(cx + 3.0, cy - 5.2, cx, cy - 8.2, 1.5, col);
        }
        Tool::Compose => {
            p.stroke(cx - 7.5, cy - 5.5, 13.0, 13.0, 3.0, 1.4, col);
            p.rect(cx + 1.0, cy - 7.0, 6.5, 6.5, 0.0, Rgba::WHITE);
            p.line(cx - 1.5, cy + 1.5, cx + 6.5, cy - 6.5, 1.6, col);
        }
        Tool::Search => {
            let s = p.s;
            look::symbol(p.c, Symbol::Search, cx * s, cy * s, 15.0 * s, col);
        }
    }
}

/// TextEdit's bars (points): the format bar and the ruler under the title bar; the text's margin.
const FORMAT_H: f32 = 32.0;
const RULER_H: f32 = 22.0;
const MARGIN: f32 = 26.0;

fn textedit_back(p: &mut Pen, w: f32, h: f32, active: bool, edited: bool) {
    let bar = BAR as f32;
    let (format_h, ruler_h, margin) = (FORMAT_H, RULER_H, MARGIN);
    let top = Rgba::rgb(0xf4, 0xf4, 0xf6);
    // the title bar and the format bar, one light band
    p.rect(0.0, 0.0, w, bar + format_h, 0.0, top);
    p.lights(BAR, active, false);
    let title_col = if active { INK } else { INK3 };
    if !edited {
        p.text_center("Untitled", 13.0, 650, w / 2.0, 18.5, title_col);
    } else {
        // edited since it was saved: said after the name, in grey
        let (a, b) = ("Untitled", " — Edited");
        let (wa, wb) = (p.width(a, 13.0, 650), p.width(b, 13.0, 400));
        let x = w / 2.0 - (wa + wb) / 2.0;
        p.text(a, 13.0, 650, x, 18.5, title_col, wa + 4.0);
        p.text(b, 13.0, 400, x + wa, 18.5, INK3, wb + 4.0);
    }
    format_bar(p, w, bar + 3.0, active);
    // the ruler
    let ry = bar + format_h;
    p.hline(0.0, ry, w, HAIRLINE);
    p.rect(0.0, ry, w, ruler_h, 0.0, Rgba::rgb(0xfa, 0xfa, 0xfb));
    let mut i = 0;
    loop {
        let x = margin + i as f32 * 9.0;
        if x > w - 10.0 {
            break;
        }
        let tick = match i % 8 {
            0 => 0.0,
            4 => 5.0,
            _ => 3.0,
        };
        if i % 8 == 0 {
            if i > 0 {
                p.text_center(&(i / 8).to_string(), 9.0, 500, x, ry + 10.5, INK2);
            }
            p.vline(x, ry + 13.0, ruler_h - 13.0, INK2.alpha(0.6));
        } else {
            p.vline(x, ry + ruler_h - tick, tick, INK3.alpha(0.8));
        }
        i += 1;
    }
    // the margins' markers
    for mx in [margin, w - margin] {
        let s = p.s;
        let (cx, cy) = (mx * s, (ry + 16.0) * s);
        for k in 0..(5.0 * s) as i32 {
            let half = (5.0 * s - k as f32) * 0.8;
            p.c.fill_round_rect(cx - half, cy + k as f32, half * 2.0, 1.0, 0.0, Rgba::rgb(0x8e, 0x8e, 0x93));
        }
    }
    p.hline(0.0, ry + ruler_h, w, HAIRLINE);
    // the page is the canvas's white
    rim(p, w, h, 12.0, Rgba::BLACK.alpha(0.12));
}

/// What was typed on TextEdit's page (Helvetica 12 there; the viewer's text face here).
fn textedit_text(p: &mut Pen, w: f32, h: f32, sc: &Scene) {
    let page_top = BAR as f32 + FORMAT_H + RULER_H + 1.0;
    let lines: Vec<(String, f32, i32, Rgba, bool)> = wrap(typed_tail(sc.text), 13.0, 400, p.s, w - MARGIN * 2.0).into_iter().map(|l| (l, 13.0, 400, Rgba::BLACK, true)).collect();
    body(p, &lines, MARGIN + 1.0, page_top + 8.0, h - 12.0, 1.3, sc, BLUE);
}

/// TextEdit's format bar: paragraph style, font, typeface, size, colour, B I U, alignment,
/// spacing, lists.
fn format_bar(p: &mut Pen, w: f32, y: f32, active: bool) {
    let h = 24.0;
    let control = |p: &mut Pen, x: f32, cw: f32| {
        p.shadow(x, y, cw, h, 6.0, 2.0, 0.5, Rgba::BLACK.alpha(0.10));
        p.rect(x, y, cw, h, 6.0, Rgba::WHITE);
        p.stroke(x, y, cw, h, 6.0, 1.0 / p.s, Rgba::BLACK.alpha(0.10));
    };
    let ink = if active { INK } else { INK2 };
    let up_down = |p: &mut Pen, x: f32| {
        let s = p.s;
        look::symbol(p.c, Symbol::UpDown, x * s, (y + h / 2.0) * s, 11.0 * s, INK2);
    };
    let mut x = 10.0;
    for (label, cw) in [("¶", 40.0), ("Helvetica", 116.0), ("Regular", 88.0), ("12", 44.0)] {
        if x + cw > w - 8.0 {
            return;
        }
        control(p, x, cw);
        p.text(label, 12.0, 450, x + 9.0, y + 16.5, ink, cw - 26.0);
        up_down(p, x + cw - 11.0);
        x += cw + 8.0;
    }
    // the text colour well
    if x + 26.0 > w - 8.0 {
        return;
    }
    control(p, x, 26.0);
    p.rect(x + 5.0, y + 5.0, 16.0, 14.0, 3.0, Rgba::BLACK);
    x += 34.0;
    // bold, italic, underline; then the four alignments; each a segmented control
    let segments = |p: &mut Pen, x: f32, n: usize, seg: f32| {
        control(p, x, n as f32 * seg);
        for i in 1..n {
            p.vline(x + i as f32 * seg, y + 5.0, h - 10.0, HAIRLINE);
        }
    };
    if x + 84.0 > w - 8.0 {
        return;
    }
    segments(p, x, 3, 28.0);
    p.text_center("B", 13.0, 750, x + 14.0, y + 17.0, ink);
    // italic: the face has no italic, so its I is slanted by hand
    p.line(x + 43.6, y + 7.4, x + 40.6, y + 16.6, 1.3, ink);
    p.text_center("U", 13.0, 400, x + 70.0, y + 17.0, ink);
    p.hline(x + 66.0, y + 18.5, 8.0, ink);
    x += 92.0;
    if x + 104.0 > w - 8.0 {
        return;
    }
    segments(p, x, 4, 26.0);
    for k in 0..4 {
        let cx = x + 13.0 + k as f32 * 26.0;
        for (j, lw) in [10.0f32, 7.0, 10.0, 6.0].into_iter().enumerate() {
            let ly = y + 7.5 + j as f32 * 3.0;
            let lw = if k == 3 { 10.0 } else { lw };
            let lx = match k {
                0 => cx - 5.0,
                1 => cx - lw / 2.0,
                2 => cx + 5.0 - lw,
                _ => cx - 5.0,
            };
            p.line(lx, ly, lx + lw, ly, 1.2, ink);
        }
    }
    x += 112.0;
    if x + 52.0 > w - 8.0 {
        return;
    }
    control(p, x, 52.0);
    p.text("1.0", 12.0, 450, x + 9.0, y + 16.5, ink, 30.0);
    up_down(p, x + 41.0);
    x += 60.0;
    if x + 46.0 > w - 8.0 {
        return;
    }
    control(p, x, 46.0);
    for j in 0..3 {
        let ly = y + 8.0 + j as f32 * 4.0;
        p.circle(x + 10.0, ly, 1.1, ink);
        p.line(x + 14.0, ly, x + 24.0, ly, 1.2, ink);
    }
    up_down(p, x + 35.0);
}

// ------------------------------------------------------------------ Calculator

const CALC_W: f32 = 232.0;
const CALC_H: f32 = 380.0;
/// its keys: diameter and gap (points), the keypad's top
const KEY_D: f32 = 44.0;
const KEY_GAP: f32 = 8.0;
const PAD_TOP: f32 = 116.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyKind {
    Function,
    Digit,
    Operator,
}

#[derive(Clone, Copy, Debug)]
struct CalcKey {
    label: &'static str,
    /// what it types into the window (see `Calc::run`)
    code: char,
    kind: KeyKind,
}

const fn key(label: &'static str, code: char, kind: KeyKind) -> CalcKey {
    CalcKey { label, code, kind }
}

/// macOS 26's basic keypad, row by row ("⌫" stands for "AC" when nothing is being typed; the
/// first key of the last row is the mode switch, which does nothing here).
const KEYS: [[CalcKey; 4]; 5] = {
    use KeyKind::*;
    [
        [key("⌫", 'k', Function), key("±", 'n', Function), key("%", '%', Function), key("÷", '/', Operator)],
        [key("7", '7', Digit), key("8", '8', Digit), key("9", '9', Digit), key("×", '*', Operator)],
        [key("4", '4', Digit), key("5", '5', Digit), key("6", '6', Digit), key("−", '-', Operator)],
        [key("1", '1', Digit), key("2", '2', Digit), key("3", '3', Digit), key("+", '+', Operator)],
        [key("mode", ' ', Digit), key("0", '0', Digit), key(".", '.', Digit), key("=", '=', Operator)],
    ]
};

fn key_center(row: usize, col: usize) -> (f32, f32) {
    let left = (CALC_W - 4.0 * KEY_D - 3.0 * KEY_GAP) / 2.0;
    (left + KEY_D / 2.0 + col as f32 * (KEY_D + KEY_GAP), PAD_TOP + KEY_D / 2.0 + row as f32 * (KEY_D + KEY_GAP))
}

/// The key under (x, y) (points of the whole window).
fn calc_key_at(x: f32, y: f32) -> Option<CalcKey> {
    for (r, row) in KEYS.iter().enumerate() {
        for (c, k) in row.iter().enumerate() {
            let (cx, cy) = key_center(r, c);
            if (x - cx).powi(2) + (y - cy).powi(2) <= (KEY_D / 2.0 + 2.0).powi(2) && k.code != ' ' {
                return Some(*k);
            }
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(String),
    Op(char),
}

/// The calculator's state, from the keys pressed since the window opened.
#[derive(Default, Debug)]
struct Calc {
    toks: Vec<Tok>,
    /// a number is being typed (the last token)
    typing: bool,
    /// after "=": the sum shown and its result
    done: Option<(String, Option<f64>)>,
}

impl Calc {
    fn run(keys: &str) -> Calc {
        let mut c = Calc::default();
        for k in keys.chars() {
            c.press(k);
        }
        c
    }

    fn press(&mut self, k: char) {
        match k {
            '0'..='9' | '.' => {
                if self.done.is_some() {
                    *self = Calc::default();
                }
                match self.toks.last_mut() {
                    Some(Tok::Num(n)) if self.typing => {
                        let digits = n.chars().filter(char::is_ascii_digit).count();
                        if (k == '.' && n.contains('.')) || digits >= 9 {
                            return;
                        }
                        if n == "0" && k != '.' {
                            n.clear();
                        } else if n == "-0" && k != '.' {
                            n.truncate(1);
                        }
                        n.push(k);
                    }
                    _ => {
                        if matches!(self.toks.last(), Some(Tok::Num(_))) {
                            self.toks.pop();
                        }
                        self.toks.push(Tok::Num(if k == '.' { "0.".into() } else { k.to_string() }));
                        self.typing = true;
                    }
                }
            }
            '+' | '-' | '*' | '/' => {
                self.continue_from_result();
                match self.toks.last() {
                    None => self.toks.push(Tok::Num("0".into())),
                    Some(Tok::Op(_)) => {
                        self.toks.pop();
                    }
                    _ => {}
                }
                self.toks.push(Tok::Op(k));
                self.typing = false;
            }
            '=' | '\n' => {
                if self.done.is_some() || self.toks.is_empty() {
                    return;
                }
                if matches!(self.toks.last(), Some(Tok::Op(_))) {
                    self.toks.pop();
                }
                let sum = self.sum();
                let v = eval(&self.toks);
                self.done = Some((sum, v));
                self.typing = false;
            }
            'c' => *self = Calc::default(),
            // the top-left key: deletes a digit while one is typed, else clears all
            'k' | 'd' => {
                if self.typing {
                    if let Some(Tok::Num(n)) = self.toks.last_mut() {
                        n.pop();
                        if n.is_empty() || n == "-" {
                            self.toks.pop();
                            self.typing = false;
                        }
                    }
                } else if k == 'k' {
                    *self = Calc::default();
                }
            }
            'n' | '%' => {
                self.continue_from_result();
                if let Some(Tok::Num(n)) = self.toks.last_mut() {
                    if k == 'n' {
                        if let Some(rest) = n.strip_prefix('-') {
                            *n = rest.to_string();
                        } else {
                            n.insert(0, '-');
                        }
                    } else if let Ok(v) = n.parse::<f64>() {
                        *n = number(v / 100.0);
                        self.typing = false;
                    }
                }
            }
            _ => {}
        }
    }

    /// After "=", the result is where the next sum starts.
    fn continue_from_result(&mut self) {
        if let Some((_, v)) = self.done.take() {
            self.toks = vec![Tok::Num(v.map_or("0".into(), number))];
            self.typing = false;
        }
    }

    /// The sum as it is shown ("12×3+4").
    fn sum(&self) -> String {
        self.toks
            .iter()
            .map(|t| match t {
                Tok::Num(n) => group(n),
                Tok::Op(o) => match o {
                    '*' => "×".into(),
                    '/' => "÷".into(),
                    '-' => "−".into(),
                    o => o.to_string(),
                },
            })
            .collect()
    }

    /// What the display shows: the sum above (after "="), the big line.
    fn display(&self) -> (String, String) {
        match &self.done {
            Some((sum, Some(v))) => (sum.clone(), group(&number(*v))),
            Some((sum, None)) => (sum.clone(), "Error".into()),
            None if self.toks.is_empty() => (String::new(), "0".into()),
            None => (String::new(), self.sum()),
        }
    }

    /// The operator waiting for its second number (its key is lit).
    fn pending(&self) -> Option<char> {
        match (self.toks.last(), &self.done) {
            (Some(Tok::Op(o)), None) => Some(*o),
            _ => None,
        }
    }
}

/// The sum's value, × and ÷ before + and − (None: divided by zero).
fn eval(toks: &[Tok]) -> Option<f64> {
    let mut terms: Vec<f64> = vec![];
    let mut op = '+';
    for t in toks {
        match t {
            Tok::Num(n) => {
                let v: f64 = n.parse().unwrap_or(0.0);
                match op {
                    '*' => *terms.last_mut()? *= v,
                    '/' => {
                        if v == 0.0 {
                            return None;
                        }
                        *terms.last_mut()? /= v
                    }
                    '-' => terms.push(-v),
                    _ => terms.push(v),
                }
            }
            Tok::Op(o) => op = *o,
        }
    }
    let v: f64 = terms.iter().sum();
    v.is_finite().then_some(v)
}

/// A number as the calculator shows it: nine digits at most, then in scientific notation.
fn number(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-8..1e9).contains(&a) {
        let s = format!("{v:.6e}");
        // "1.234500e10" -> "1.2345e10"
        if let Some((m, e)) = s.split_once('e') {
            let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
            return format!("{m}e{e}");
        }
        return s;
    }
    let int_digits = (a.log10().floor() as i32 + 1).max(1);
    let decimals = if a < 1.0 { 9 } else { (9 - int_digits).clamp(0, 8) as usize };
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Thousands separators in a number's whole part ("1234567.5" -> "1,234,567.5").
fn group(n: &str) -> String {
    if n.contains('e') {
        return n.to_string();
    }
    let (sign, rest) = n.strip_prefix('-').map_or(("", n), |r| ("-", r));
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(a, b)| (a, Some(b)));
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

/// Calculator's window and keys (the top-left key and a lit operator follow `calc`).
fn calculator_back(p: &mut Pen, w: f32, h: f32, active: bool, calc: &Calc) {
    // dark glass: a deep grey, a little lighter at the top
    let s = p.s;
    let (top, bottom) = (Rgba::rgb(0x34, 0x34, 0x37), Rgba::rgb(0x1f, 0x1f, 0x21));
    let hp = h * s;
    p.c.fill_round_rect_with(0.0, 0.0, w * s, hp, 0.0, |_, y| top.lerp(bottom, (y / hp).clamp(0.0, 1.0)));
    p.lights(BAR, active, true);
    let pending = calc.pending();
    for (r, row) in KEYS.iter().enumerate() {
        for (c, k) in row.iter().enumerate() {
            let (cx, cy) = key_center(r, c);
            let lit = k.kind == KeyKind::Operator && pending == Some(k.code);
            let (fill, ink) = match k.kind {
                KeyKind::Function => (Rgba::rgb(0x5c, 0x5c, 0x60), Rgba::WHITE),
                KeyKind::Digit => (Rgba::rgb(0x3b, 0x3b, 0x3e), Rgba::WHITE),
                KeyKind::Operator if lit => (Rgba::rgb(0xfc, 0xfc, 0xfc), ORANGE),
                KeyKind::Operator => (ORANGE, Rgba::WHITE),
            };
            let rad = KEY_D / 2.0;
            p.c.fill_round_rect_with((cx - rad) * s, (cy - rad) * s, KEY_D * s, KEY_D * s, rad * s, |_, y| fill.shade(0.06 * (1.0 - ((y / s - (cy - rad)) / KEY_D).clamp(0.0, 1.0))));
            p.stroke(cx - rad, cy - rad, KEY_D, KEY_D, rad, 1.0 / s, Rgba::WHITE.alpha(0.06));
            match k.label {
                "⌫" if !calc.typing => p.text_center("AC", 19.0, 500, cx, cy + 7.0, ink),
                "⌫" => backspace_glyph(p, cx, cy, ink),
                "mode" => mode_glyph(p, cx, cy, ink),
                "." => p.circle(cx, cy + 1.0, 2.6, ink),
                l if k.kind == KeyKind::Operator => p.text_center(l, 26.0, 450, cx, cy + 9.0, ink),
                l if k.kind == KeyKind::Function => p.text_center(l, 20.0, 450, cx, cy + 7.0, ink),
                l => p.text_center(l, 22.0, 400, cx, cy + 8.0, ink),
            }
        }
    }
    rim(p, w, h, 20.0, Rgba::WHITE.alpha(0.10));
}

/// Calculator's display: the sum above (after "="), the number or result.
fn calculator_display(p: &mut Pen, w: f32, calc: &Calc) {
    let (sum, big) = calc.display();
    let right = w - (w - 4.0 * KEY_D - 3.0 * KEY_GAP) / 2.0 - 6.0;
    let room = right - 18.0;
    if !sum.is_empty() {
        let size = fit(p, &sum, 17.0, 400, room);
        p.text_right(&sum, size, 400, right, 60.0, Rgba::rgb(0x98, 0x98, 0x9d));
    }
    let size = fit(p, &big, 46.0, 300, room);
    p.text_right(&big, size, 300, right, 101.0, Rgba::WHITE);
}

/// The largest size up to `size` at which `t` fits in `room` points (at least 14).
fn fit(p: &Pen, t: &str, size: f32, weight: i32, room: f32) -> f32 {
    let w = p.width(t, size, weight);
    if w <= room {
        size
    } else {
        (size * room / w).floor().max(14.0)
    }
}

/// "delete.left": a tag pointing left with an x in it.
fn backspace_glyph(p: &mut Pen, cx: f32, cy: f32, col: Rgba) {
    let (l, r, t, b) = (cx - 10.0, cx + 9.0, cy - 6.5, cy + 6.5);
    p.line(l, cy, l + 6.0, t, 1.6, col);
    p.line(l, cy, l + 6.0, b, 1.6, col);
    p.line(l + 6.0, t, r, t, 1.6, col);
    p.line(l + 6.0, b, r, b, 1.6, col);
    p.line(r, t, r, b, 1.6, col);
    p.line(cx - 1.0, cy - 3.0, cx + 5.0, cy + 3.0, 1.5, col);
    p.line(cx - 1.0, cy + 3.0, cx + 5.0, cy - 3.0, 1.5, col);
}

/// The mode switch: a small calculator.
fn mode_glyph(p: &mut Pen, cx: f32, cy: f32, col: Rgba) {
    p.stroke(cx - 6.5, cy - 8.5, 13.0, 17.0, 2.5, 1.4, col);
    p.rect(cx - 3.8, cy - 5.8, 7.6, 3.2, 0.8, col);
    for r in 0..2 {
        for c in 0..2 {
            p.circle(cx - 2.2 + c as f32 * 4.4, cy + 1.5 + r as f32 * 3.8, 1.0, col);
        }
    }
}

// ------------------------------------------------------------------ icons

/// An app's icon, `size` px square, straight RGBA.
pub fn icon(app: &str, size: u32) -> Vec<u8> {
    rgba(&icon_canvas(app, size as usize))
}

/// An app's icon as macOS 26 draws app icons: a rounded square a little inside the canvas, a
/// soft shadow under it.
pub fn icon_canvas(app: &str, size: usize) -> Canvas {
    let mut c = Canvas::new(size, size);
    let k = size as f32 / 100.0;
    let (x, y, d, r) = (10.0 * k, 9.0 * k, 80.0 * k, 18.5 * k);
    c.shadow(x, y, d, d, r, 4.0 * k, 1.5 * k, Rgba::BLACK.alpha(0.28));
    let at = |v: f32| v * k;
    match app {
        "notes" => {
            // a yellow pad: the yellow top, its torn edge, ruled lines on white
            c.fill_round_rect_with(x, y, d, d, r, |_, py| Rgba::WHITE.lerp(Rgba::rgb(0xf1, 0xf1, 0xf3), ((py - y) / d).clamp(0.0, 1.0)));
            let band = y + 27.0 * k;
            let mut top = Canvas::new(size, size);
            top.fill_round_rect_with(x, y, d, d, r, |_, py| Rgba::rgb(0xff, 0xd8, 0x4c).lerp(Rgba::rgb(0xf7, 0xbd, 0x1d), ((py - y) / (band - y)).clamp(0.0, 1.0)));
            let clip = top.crop(0, 0, size, band.round() as usize);
            c.composite(&clip, 0, 0, 1.0);
            c.fill_round_rect(x, band, d, 1.6 * k, 0.0, Rgba::rgb(0xd9, 0xa4, 0x10));
            for i in 0..9 {
                c.fill_circle(x + at(9.0) + i as f32 * at(7.8), band + at(4.5), at(1.3), Rgba::rgb(0xd6, 0xd6, 0xda));
            }
            for i in 0..5 {
                let ly = band + at(14.0) + i as f32 * at(9.5);
                c.fill_round_rect(x + at(9.0), ly, d - at(18.0), at(1.4), at(0.7), Rgba::rgb(0xcf, 0xcf, 0xd4));
            }
        }
        "textedit" => {
            // a sheet of paper with writing, and a fountain pen across it
            c.fill_round_rect_with(x, y, d, d, r, |_, py| Rgba::rgb(0xf6, 0xf6, 0xf8).lerp(Rgba::rgb(0xdd, 0xdd, 0xe3), ((py - y) / d).clamp(0.0, 1.0)));
            let (px, py, pw, ph) = (x + at(16.0), y + at(12.0), at(48.0), at(58.0));
            c.shadow(px, py, pw, ph, at(2.5), at(2.5), at(1.0), Rgba::BLACK.alpha(0.22));
            c.fill_round_rect(px, py, pw, ph, at(2.5), Rgba::WHITE);
            for i in 0..7 {
                let lw = if i % 3 == 2 { at(24.0) } else { at(36.0) };
                c.fill_round_rect(px + at(6.0), py + at(9.0) + i as f32 * at(6.6), lw, at(2.0), at(1.0), Rgba::rgb(0x9a, 0x9a, 0xa2));
            }
            // the pen: a dark barrel, a gold band and nib, from the bottom right towards the page
            let (x0, y0, x1, y1) = (x + at(74.0), y + at(76.0), x + at(46.0), y + at(40.0));
            c.fill_capsule(x0, y0, x1, y1, at(5.2), Rgba::rgb(0x2c, 0x2c, 0x30));
            c.fill_capsule(x0 - at(2.0), y0 - at(2.6), x1 + at(6.0), y1 + at(7.8), at(1.4), Rgba::WHITE.alpha(0.18));
            let (bx, by) = (x1 + (x0 - x1) * 0.12, y1 + (y0 - y1) * 0.12);
            c.fill_capsule(bx - at(0.8), by - at(1.0), bx + at(0.8), by + at(1.0), at(5.6), Rgba::rgb(0xd8, 0xb0, 0x5a));
            c.fill_capsule(x1, y1, x1 - at(7.0), y1 - at(9.0), at(2.4), Rgba::rgb(0xc9, 0x9a, 0x3c));
            c.fill_capsule(x1 - at(2.0), y1 - at(2.6), x1 - at(8.6), y1 - at(11.0), at(0.9), Rgba::rgb(0x6b, 0x4f, 0x16));
        }
        _ => {
            // Calculator: dark, with its four keys
            c.fill_round_rect_with(x, y, d, d, r, |_, py| Rgba::rgb(0x4a, 0x4a, 0x4e).lerp(Rgba::rgb(0x1f, 0x1f, 0x22), ((py - y) / d).clamp(0.0, 1.0)));
            let kr = at(14.5);
            let centers = [(x + at(25.0), y + at(25.0)), (x + at(55.0), y + at(25.0)), (x + at(25.0), y + at(55.0)), (x + at(55.0), y + at(55.0))];
            let fills = [Rgba::rgb(0x8e, 0x8e, 0x93), ORANGE, Rgba::rgb(0x8e, 0x8e, 0x93), ORANGE];
            for (i, ((cx, cy), f)) in centers.into_iter().zip(fills).enumerate() {
                let (cx, cy) = (cx + at(0.0), cy + at(0.0));
                c.fill_round_rect_with(cx - kr, cy - kr, kr * 2.0, kr * 2.0, kr, |_, yy| f.shade(0.12 * (1.0 - ((yy - (cy - kr)) / (2.0 * kr)).clamp(0.0, 1.0))));
                let (m, wd) = (at(6.0), at(1.7));
                match i {
                    0 => c.fill_capsule(cx - m, cy, cx + m, cy, wd, Rgba::WHITE),
                    1 => {
                        c.fill_capsule(cx - m, cy, cx + m, cy, wd, Rgba::WHITE);
                        c.fill_capsule(cx, cy - m, cx, cy + m, wd, Rgba::WHITE);
                    }
                    2 => {
                        let q = m * 0.75;
                        c.fill_capsule(cx - q, cy - q, cx + q, cy + q, wd, Rgba::WHITE);
                        c.fill_capsule(cx - q, cy + q, cx + q, cy - q, wd, Rgba::WHITE);
                    }
                    _ => {
                        c.fill_capsule(cx - m, cy - at(2.6), cx + m, cy - at(2.6), wd, Rgba::WHITE);
                        c.fill_capsule(cx - m, cy + at(2.6), cx + m, cy + at(2.6), wd, Rgba::WHITE);
                    }
                }
            }
        }
    }
    // the rim light all of macOS 26's icons have
    c.stroke_round_rect_with(x, y, d, d, r, (0.8 * k).max(1.0), |_, py| Rgba::WHITE.alpha(0.5 * (1.0 - ((py - y) / d).clamp(0.0, 1.0))));
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene<'a>(app: &'a str, text: &'a str, s: f32) -> Scene<'a> {
        let a = apps().into_iter().find(|a| a.id == app).unwrap();
        Scene { app, width: (a.size.0 as f32 * s) as usize, height: (a.size.1 as f32 * s) as usize, scale: s, since_input: 0.1, text, selected: false, active: true }
    }

    /// How many distinct colours a picture has (a blank one has one).
    fn colours(px: &[u8]) -> usize {
        let mut set = std::collections::HashSet::new();
        for p in px.chunks(4) {
            set.insert([p[0] >> 3, p[1] >> 3, p[2] >> 3]);
        }
        set.len()
    }

    /// Whether the picture has dark text-like ink in (x, y, w, h) (points at scale 1).
    fn ink_in(px: &[u8], width: usize, (x, y, w, h): (usize, usize, usize, usize), dark: bool) -> bool {
        (y..y + h).any(|yy| (x..x + w).any(|xx| {
            let p = &px[(yy * width + xx) * 4..];
            let l = (p[0] as u32 + p[1] as u32 + p[2] as u32) / 3;
            if dark { l < 90 } else { l > 200 }
        }))
    }

    #[test]
    fn every_window_is_drawn_whole_with_its_text() {
        for (app, _) in APPS {
            let sc = scene(app, "Hello", 1.0);
            let px = paint(&sc);
            assert_eq!(px.len(), sc.width * sc.height * 4, "{app}");
            assert!(px.chunks(4).all(|p| p[3] == 255), "{app}: opaque (the mask makes its corners)");
            assert!(colours(&px) > 40, "{app}: drawn, not blank ({} colours)", colours(&px));
        }
        // what was typed is drawn: Notes and TextEdit change with it
        for app in ["notes", "textedit"] {
            assert_ne!(paint(&scene(app, "", 1.0)), paint(&scene(app, "Hello there", 1.0)), "{app}");
        }
        // TextEdit's page holds the text, under the ruler
        let px = paint(&scene("textedit", "Hello there", 1.0));
        assert!(ink_in(&px, 720, (26, 92, 120, 20), true), "typed text on the page");
        assert!(!ink_in(&px, 720, (26, 200, 400, 100), true), "the rest of the page is blank");
        // Calculator's display shows the sum's result in white
        let px = paint(&scene("calculator", "12+30=", 1.0));
        assert!(ink_in(&px, 232, (120, 80, 90, 36), false), "the result");
        let blank = paint(&scene("calculator", "", 1.0));
        assert_ne!(px, blank);
    }

    #[test]
    fn pictures_follow_the_scale_and_the_caret_blinks() {
        let sc = scene("textedit", "x", 2.0);
        assert_eq!(paint(&sc).len(), 1440 * 960 * 4);
        let on = paint(&Scene { since_input: 0.1, ..scene("textedit", "x", 1.0) });
        let off = paint(&Scene { since_input: 0.7, ..scene("textedit", "x", 1.0) });
        let inactive = paint(&Scene { active: false, ..scene("textedit", "x", 1.0) });
        assert_ne!(on, off, "the caret blinks");
        assert_ne!(off, inactive, "an inactive window's buttons are grey");
    }

    #[test]
    fn icons_are_drawn_in_their_square() {
        for (app, _) in APPS {
            let px = icon(app, 64);
            assert_eq!(px.len(), 64 * 64 * 4);
            assert_eq!(px[3], 0, "{app}: the corner is clear");
            assert_eq!(px[(32 * 64 + 32) * 4 + 3], 255, "{app}: the middle is solid");
            assert!(colours(&px) > 10, "{app}");
        }
    }

    #[test]
    fn the_calculator_adds_up() {
        let d = |k: &str| Calc::run(k).display();
        assert_eq!(d(""), ("".into(), "0".into()));
        assert_eq!(d("12+7"), ("".into(), "12+7".into()));
        assert_eq!(d("12+7="), ("12+7".into(), "19".into()));
        assert_eq!(d("2+3*4="), ("2+3×4".into(), "14".into()));
        assert_eq!(d("1234567*100="), ("1,234,567×100".into(), "123,456,700".into()));
        assert_eq!(d("1234567*1000="), ("1,234,567×1,000".into(), "1.234567e9".into()), "past nine digits");
        assert_eq!(d("1/3="), ("1÷3".into(), "0.333333333".into()));
        assert_eq!(d("5/0="), ("5÷0".into(), "Error".into()));
        assert_eq!(d("9=+1="), ("9+1".into(), "10".into()), "a result goes on into the next sum");
        assert_eq!(d("12k"), ("".into(), "1".into()), "the top-left key deletes a digit while typing");
        assert_eq!(d("12+k"), ("".into(), "0".into()), "…and clears all otherwise");
        assert_eq!(d("5n"), ("".into(), "-5".into()));
        assert_eq!(d("50%"), ("".into(), "0.5".into()));
        assert_eq!(d("7c3"), ("".into(), "3".into()));
        assert_eq!(Calc::run("3+").pending(), Some('+'));
        assert_eq!(input("calculator", &Input::Text("2x3")).as_deref(), Some("2*3"));
        assert_eq!(input("notes", &Input::Text("2x3")), None);
        // the keys are where they are drawn
        let (cx, cy) = key_center(4, 3);
        assert_eq!(calc_key_at(cx, cy).map(|k| k.code), Some('='));
        let (cx, cy) = key_center(0, 0);
        assert_eq!(calc_key_at(cx + 10.0, cy - 10.0).map(|k| k.code), Some('k'));
        assert!(calc_key_at(5.0, 50.0).is_none());
    }

    #[test]
    fn text_wraps_between_words_and_keeps_line_breaks() {
        let l = wrap("one two three four five six seven", 13.0, 400, 1.0, 80.0);
        assert!(l.len() >= 3, "{l:?}");
        assert!(l.iter().all(|x| text::width(x.trim_end(), Style::dip(13.0, 400, 1.0)) <= 80.5), "{l:?}");
        assert_eq!(l.concat(), "one two three four five six seven");
        assert_eq!(wrap("a\n\nb", 13.0, 400, 1.0, 80.0), ["a", "", "b"]);
        assert_eq!(wrap("", 13.0, 400, 1.0, 80.0), [""]);
        let long = wrap(&"x".repeat(60), 13.0, 400, 1.0, 80.0);
        assert!(long.len() >= 3 && long.concat().len() == 60, "{long:?}");
    }

    #[test]
    fn a_demo_session_is_told_apart() {
        assert!(is_demo(&format!("{SESSION_PREFIX}0011")));
        assert!(!is_demo("rm-123456789"));
        assert_ne!(random_hex(8), random_hex(8));
        assert_eq!(random_hex(24).len(), 48);
    }
}

/// Writes the demo's windows and icons as pictures (RM_PREVIEW=dir cargo test -p rm-viewer
/// demo::preview), each window on a desktop with its shadow and rounded corners.
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

    /// The window as the viewer shows an exact one: through its mask, with a shadow, on a
    /// soft desktop.
    fn on_desktop(app: &str, text: &str, s: f32, active: bool, since: f64, framed: bool) -> Canvas {
        let a = apps().into_iter().find(|x| x.id == app).unwrap();
        let (w, h) = ((a.size.0 as f32 * s) as usize, (a.size.1 as f32 * s) as usize);
        let sc = Scene { app, width: w, height: h, scale: s, since_input: since, text, selected: false, active };
        let mut win = draw(&sc);
        if framed {
            // as the viewer's own frame shows it: the Mac's title band cut off, a bar of Taro's above
            let cut = (a.title_height as f32 * s) as usize;
            let mut f = Canvas::filled(w, h, Rgba::rgb(0xec, 0xec, 0xee));
            f.composite(&win.crop(0, cut as isize, w, h - cut), 0, (40.0 * s) as isize, 1.0);
            win = f.crop(0, 0, w, h - cut + (40.0 * s) as usize);
        }
        let h = win.h;
        win.mask(&rm_fakeagent::rounded_mask(w, h, a.corner * s));
        let pad = (40.0 * s) as usize;
        let mut c = Canvas::new(w + 2 * pad, h + 2 * pad);
        let (cw, ch) = (c.w as f32, c.h as f32);
        c.fill_round_rect_with(0.0, 0.0, cw, ch, 0.0, |x, y| Rgba::rgb(0x8f, 0xa8, 0xd8).lerp(Rgba::rgb(0xc9, 0xa7, 0xd9), (x / cw * 0.6 + y / ch * 0.4).clamp(0.0, 1.0)));
        c.shadow(pad as f32, pad as f32, w as f32, h as f32, a.corner * s, 24.0 * s, 10.0 * s, Rgba::BLACK.alpha(0.35));
        c.composite(&win, pad as isize, pad as isize, 1.0);
        c
    }

    #[test]
    fn pictures() {
        let Some(dir) = std::env::var_os("RM_PREVIEW") else { return };
        let dir = std::path::PathBuf::from(dir);
        let s: f32 = std::env::var("RM_PREVIEW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
        save(&on_desktop("notes", "Hello from Windows! This is typed on the PC and drawn by the Mac.", s, true, 0.1, false), &dir.join("demo-notes.ppm"));
        save(&on_desktop("textedit", "Dear Taro,\n\nThis text was typed on Windows, sent to the Mac and streamed back as video. The caret blinks while the window is active.", s, true, 0.1, false), &dir.join("demo-textedit.ppm"));
        save(&on_desktop("textedit", "", s, false, 0.1, false), &dir.join("demo-textedit-inactive.ppm"));
        save(&on_desktop("calculator", "1234*56+7=", s, true, 0.1, false), &dir.join("demo-calculator.ppm"));
        save(&on_desktop("calculator", "12+3", s, true, 0.1, false), &dir.join("demo-calculator-typing.ppm"));
        save(&on_desktop("notes", "", s, true, 0.1, true), &dir.join("demo-notes-framed.ppm"));
        // the icons, at the Dock's size and large
        let mut c = Canvas::filled(3 * 140 + 3 * 72 + 40, 160, Rgba::rgb(0xe8, 0xe8, 0xec));
        for (i, (app, _)) in APPS.iter().enumerate() {
            let big = icon_canvas(app, 128);
            c.composite(&big, 10 + i as isize * 140, 16, 1.0);
            let small = icon_canvas(app, 64);
            c.composite(&small, 440 + i as isize * 72, 48, 1.0);
        }
        save(&c, &dir.join("demo-icons.ppm"));
    }
}
