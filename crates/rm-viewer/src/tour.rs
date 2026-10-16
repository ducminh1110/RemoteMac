//! The guided tour, the first time Taro runs after an update. The screen dims a little and an
//! arrow points at what to do; do what the card says and it answers ("Great!"), Next goes on,
//! Skip ends it. The control under the arrow stays usable: the dimming has a hole there.
//!
//! It has two parts. First a demo Mac inside Taro (demo.rs): the card points at it in the
//! connect window, Taro connects to it as to any Mac, and the tour goes on over the session (the
//! launcher: open an app; the screen's edge: push the pointer for the Dock; the search). Then
//! Taro starts again from the beginning (ui.rs) and the second part shows adding one's own Mac:
//! the +, the ID, the password, Save, and the click that connects.
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::connectui::TourProbe;
use crate::paint::{coverage, sdf_round_rect, Canvas, Rgba};
use crate::surface::{self, Align, Surface};
use crate::text::{self, Style};

const CLASS: PCWSTR = w!("RmTour");
const TIMER: usize = 1;

type Rect = (f32, f32, f32, f32);
/// What the dimming was made for: the step, the hole, how dark.
type BaseKey = (usize, Option<[i32; 4]>, u32);

#[derive(Clone, Copy, PartialEq)]
enum Target {
    None,
    /// the demo Mac's row in the connect window
    Demo,
    /// the launcher's tile of the app to open
    App,
    /// the screen's edge where the Dock comes out
    Edge,
    Add,
    Id,
    Password,
    Save,
    /// one's own Mac, first in the list
    First,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// the connect window is up
    Connect,
    /// the demo Mac's session
    Session,
}

struct Step {
    title: &'static str,
    body: &'static str,
    target: Target,
    great: &'static str,
    phase: Phase,
}

/// Connecting to the demo Mac.
const DEMO_STEP: usize = 1;
/// The demo's last card: Taro goes back to the start from it.
const DEMO_END: usize = 5;
/// The second part's first card (adding one's own Mac).
const OWN_FIRST: usize = 6;
/// Connecting to one's own Mac: the tour ends with it.
const CONNECT_STEP: usize = 10;

const STEPS: [Step; 11] = [
    Step { title: "Welcome to Taro", body: "A quick tour: first you try Taro on a demo Mac, then you add your own. It takes about a minute; Skip ends it at any time.", target: Target::None, great: "", phase: Phase::Connect },
    Step { title: "Try the demo Mac", body: "Click Demo Mac. It is a pretend Mac inside Taro, so you can see how everything works before you connect yours.", target: Target::Demo, great: "Connecting…", phase: Phase::Connect },
    Step { title: "Your Mac's apps", body: "This is the launcher: every app on the Mac. Click Notes to open it. Each Mac app gets a window of its own on Windows.", target: Target::App, great: "Great! That is a Mac app, in its own window.", phase: Phase::Session },
    Step { title: "The Dock", body: "Push the pointer against the edge of the screen and keep pushing: the Mac's Dock slides out. A click on an icon opens that app.", target: Target::Edge, great: "Great! That is the Mac's Dock.", phase: Phase::Session },
    Step { title: "Search", body: "Ctrl+Alt+Space finds any app on the Mac from anywhere. Settings (in the launcher) has Stage Manager, sound and updates.", target: Target::None, great: "", phase: Phase::Session },
    Step { title: "That was the demo", body: "Next, Taro goes back to the start, where you add your own Mac. The demo's windows close.", target: Target::None, great: "", phase: Phase::Session },
    Step { title: "Now your Mac", body: "Click + to add the Mac you want to reach. Taro keeps it here, so next time one click connects.", target: Target::Add, great: "Great!", phase: Phase::Connect },
    Step { title: "Your Mac's ID", body: "Start Taro on the Mac (./taro --password …): it shows an ID. Type it here (or the Mac's address, under By Address).", target: Target::Id, great: "Great! That is the ID.", phase: Phase::Connect },
    Step { title: "The password", body: "Type the password you gave Taro on the Mac. Your own relay? Have a custom relay? takes its address and key.", target: Target::Password, great: "Great!", phase: Phase::Connect },
    Step { title: "Save", body: "Save keeps the Mac on this PC, its password protected for your Windows account.", target: Target::Save, great: "Saved.", phase: Phase::Connect },
    Step { title: "Connect", body: "Click your Mac. Taro finds it and brings its apps here.", target: Target::First, great: "Connecting…", phase: Phase::Connect },
];

/// What the demo session shows the tour (ui.rs, screen px).
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionProbe {
    /// the launcher's tile of the app to open, while it is shown
    pub app: Option<Rect>,
    /// a window of that app is open
    pub app_open: bool,
    /// where the Dock comes out: a strip along the screen's edge
    pub edge: Rect,
    /// the Dock is out
    pub dock_out: bool,
}

type ProbeFn = fn() -> Option<SessionProbe>;
/// what the tour looks at, and how Taro goes back to the start
type Hooks = (ProbeFn, fn());
static SESSION: Mutex<Option<Hooks>> = Mutex::new(None);

/// The session's side (ui.rs): what the tour looks at, and how Taro goes back to the start.
pub fn set_session_hooks(probe: ProbeFn, back_to_start: fn()) {
    *SESSION.lock().unwrap() = Some((probe, back_to_start));
}

struct Tour {
    surf: Surface,
    step: usize,
    started: Instant,
    /// the step's goal was reached (and when)
    done_at: Option<Instant>,
    /// the whole tour ended (connecting to one's own Mac), closing soon
    ending: Option<Instant>,
    /// connected to the demo Mac: the tour goes on when the connect window goes
    demo_live: bool,
    mon: RECT,
    scale: f32,
    /// the dimming with its hole, and what it was made for
    base: Option<(BaseKey, Canvas)>,
    next: Rect,
    skip: Rect,
}

thread_local! {
    static TOUR: RefCell<Option<Tour>> = const { RefCell::new(None) };
}

/// This version as one number (1.3.0 -> 1_003_000).
fn code() -> u32 {
    let mut n = env!("CARGO_PKG_VERSION").split(['.', '-']).filter_map(|p| p.parse::<u32>().ok());
    let (a, b, c) = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
    a * 1_000_000 + b * 1_000 + c
}

fn mark_seen() {
    let mut s = crate::settings::Settings::load();
    if s.tour_seen < code() {
        s.tour_seen = code();
        let _ = s.save();
    }
}

/// The demo part is behind: when Taro starts again the tour goes on with adding one's own Mac.
fn mark_demo_done() {
    let mut s = crate::settings::Settings::load();
    s.tour_demo = code();
    let _ = s.save();
}

/// Whether the tour will run.
pub fn wanted() -> bool {
    std::env::var_os("RM_NO_TOUR").is_none() && crate::settings::Settings::load().tour_seen < code()
}

/// Whether the tour is at its demo part (the connect window offers the demo Mac then).
pub fn offers_demo() -> bool {
    wanted() && crate::settings::Settings::load().tour_demo < code()
}

/// Begin the tour when this version's has not been shown yet (and the connect window is up).
pub fn start(hinst: HINSTANCE) {
    if !wanted() || TOUR.with(|t| t.borrow().is_some()) {
        return;
    }
    let Some((owner, _, _)) = crate::connect::tour_probe() else { return };
    surface::register(hinst, CLASS, Some(proc), IDC_ARROW);
    let Some(surf) = Surface::new(hinst, CLASS, "Taro tour", None, true, false) else { return };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info);
    }
    let mon = info.rcMonitor;
    let scale = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(owner) }.max(96) as f32 / 96.0;
    let hwnd = surf.hwnd;
    let step = if offers_demo() { 0 } else { OWN_FIRST };
    TOUR.with(|t| *t.borrow_mut() = Some(Tour { surf, step, started: Instant::now(), done_at: None, ending: None, demo_live: false, mon, scale, base: None, next: (0.0, 0.0, 0.0, 0.0), skip: (0.0, 0.0, 0.0, 0.0) }));
    tick();
    TOUR.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            t.surf.show();
        }
    });
    unsafe {
        SetTimer(Some(hwnd), TIMER, 33, None);
    }
}

/// Taro is connected to the demo Mac (ui.rs): the tour goes on over its session.
pub fn demo_connected() {
    TOUR.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t.demo_live = true;
        }
    });
}

/// The connect window is gone. The tour goes on over the demo Mac's session, else it goes (not
/// counted as seen unless it was finished).
pub fn close() {
    let demo = TOUR.with(|t| {
        let mut b = t.borrow_mut();
        let t = b.as_mut()?;
        if t.demo_live && t.step <= DEMO_STEP {
            t.step = DEMO_STEP + 1;
            t.done_at = None;
            t.base = None;
            return Some(true);
        }
        Some(false)
    });
    if demo != Some(true) {
        destroy();
    }
}

fn destroy() {
    let t = TOUR.with(|t| t.borrow_mut().take());
    if let Some(t) = t {
        unsafe {
            let _ = KillTimer(Some(t.surf.hwnd), TIMER);
            let _ = DestroyWindow(t.surf.hwnd);
        }
    }
}

fn finish() {
    mark_seen();
    destroy();
}

/// Out of the demo: back to the start (`own`: to go on with adding one's own Mac).
fn leave_demo(own: bool) {
    if own {
        mark_demo_done();
    } else {
        mark_seen();
    }
    destroy();
    let back = SESSION.lock().unwrap().map(|(_, b)| b);
    if let Some(back) = back {
        back();
    }
}

/// What the arrow points at, on the overlay (px from the monitor's corner).
fn target_rect(kind: Target, conn: Option<&(HWND, (i32, i32), TourProbe)>, session: Option<&SessionProbe>, mon: RECT) -> Option<Rect> {
    let screen = |r: Rect| Some((r.0 - mon.left as f32, r.1 - mon.top as f32, r.2, r.3));
    if let Some((_, origin, p)) = conn {
        let r = match kind {
            Target::None | Target::App | Target::Edge => return None,
            Target::Demo if p.demo.2 <= 0.0 => return None,
            Target::Demo => p.demo,
            // the sheet's controls while it is open; before it is, the + that opens it
            Target::Id | Target::Password | Target::Save if !p.editing => p.add,
            Target::Add if p.editing => p.id,
            Target::Add => p.add,
            Target::Id => p.id,
            Target::Password => p.password,
            Target::Save => p.save,
            Target::First if p.computers == 0 => p.add,
            Target::First => p.first,
        };
        return screen((r.0 + origin.0 as f32, r.1 + origin.1 as f32, r.2, r.3));
    }
    let s = session?;
    match kind {
        Target::App => s.app.and_then(screen),
        Target::Edge => screen(s.edge),
        _ => None,
    }
}

fn goal_reached(step: usize, conn: Option<&TourProbe>, session: Option<&SessionProbe>) -> bool {
    match (step, conn, session) {
        (DEMO_STEP, Some(p), _) => p.busy,
        (2, _, Some(s)) => s.app_open,
        (3, _, Some(s)) => s.dock_out,
        (6, Some(p), _) => p.editing || p.computers > 0,
        (7, Some(p), _) => p.editing && p.id_len > 0,
        (8, Some(p), _) => p.editing && p.password_len > 0,
        (9, Some(p), _) => !p.editing && p.computers > 0,
        (CONNECT_STEP, Some(p), _) => p.busy,
        _ => false,
    }
}

/// The dimming with a hole (and a ring) where the arrow points.
fn dimming(w: usize, h: usize, s: f32, hole: Option<Rect>, accent: Rgba, dim: f32) -> Canvas {
    let mut c = Canvas::filled(w, h, Rgba::BLACK.alpha(dim));
    if let Some((x, y, ww, hh)) = hole {
        let pad = 8.0 * s;
        let (hx, hy, hw, hh) = (x - pad, y - pad, ww + 2.0 * pad, hh + 2.0 * pad);
        let r = 12.0 * s;
        let (x0, y0) = ((hx - 2.0).floor().max(0.0) as usize, (hy - 2.0).floor().max(0.0) as usize);
        let (x1, y1) = (((hx + hw + 2.0).ceil().max(0.0) as usize).min(w), ((hy + hh + 2.0).ceil().max(0.0) as usize).min(h));
        for py in y0..y1 {
            for px in x0..x1 {
                let cov = coverage(sdf_round_rect(px as f32 + 0.5, py as f32 + 0.5, hx, hy, hw, hh, r));
                if cov > 0.0 {
                    c.px[py * w + px] = [0, 0, 0, ((dim * (1.0 - cov)) * 255.0).round() as u8];
                }
            }
        }
        c.stroke_round_rect_with(hx, hy, hw, hh, r, (2.5 * s).max(2.0), |_, _| accent);
    }
    c
}

/// A filled triangle (the arrow's head).
fn triangle(c: &mut Canvas, a: (f32, f32), b: (f32, f32), d: (f32, f32), color: Rgba) {
    let (x0, x1) = (a.0.min(b.0).min(d.0).floor().max(0.0) as usize, (a.0.max(b.0).max(d.0).ceil().max(0.0) as usize).min(c.w));
    let (y0, y1) = (a.1.min(b.1).min(d.1).floor().max(0.0) as usize, (a.1.max(b.1).max(d.1).ceil().max(0.0) as usize).min(c.h));
    let edge = |p: (f32, f32), q: (f32, f32), x: f32, y: f32| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (e1, e2, e3) = (edge(a, b, fx, fy), edge(b, d, fx, fy), edge(d, a, fx, fy));
            if (e1 >= 0.0 && e2 >= 0.0 && e3 >= 0.0) || (e1 <= 0.0 && e2 <= 0.0 && e3 <= 0.0) {
                c.blend(x, y, color, 1.0);
            }
        }
    }
}

/// The steps of the part the tour is in (the dots under the card).
fn part(step: usize) -> std::ops::Range<usize> {
    if step < OWN_FIRST {
        0..OWN_FIRST
    } else {
        OWN_FIRST..STEPS.len()
    }
}

enum Arrow {
    /// up or down to a control (`x` along the card, `edge` the target's near side)
    Up { x: f32, below: bool, edge: f32 },
    /// sideways to the screen's edge
    Side { left: bool, edge: f32, y: f32 },
}

fn tick() {
    let phase = TOUR.with(|t| t.borrow().as_ref().map(|t| STEPS[t.step].phase));
    let Some(phase) = phase else { return };
    let conn = crate::connect::tour_probe();
    let session = if phase == Phase::Session { SESSION.lock().unwrap().and_then(|(p, _)| p()) } else { None };
    if phase == Phase::Connect && conn.is_none() {
        // (the window went: connecting closes it, and close() says what becomes of the tour)
        return;
    }
    let mut leave = false;
    TOUR.with(|t| {
        let mut b = t.borrow_mut();
        let Some(t) = b.as_mut() else { return };
        let s = t.scale;
        let (w, h) = ((t.mon.right - t.mon.left) as usize, (t.mon.bottom - t.mon.top) as usize);
        let step = &STEPS[t.step];
        // the goal of the step
        if t.done_at.is_none() && goal_reached(t.step, conn.as_ref().map(|c| &c.2), session.as_ref()) {
            t.done_at = Some(Instant::now());
            if t.step == CONNECT_STEP {
                t.ending = Some(Instant::now());
            }
        }
        if t.ending.is_some_and(|e| e.elapsed() > Duration::from_millis(900)) {
            leave = true;
            return;
        }
        let accent = surface::accent();
        let target = target_rect(step.target, conn.as_ref(), session.as_ref(), t.mon);
        // over the demo's session the dimming is lighter, and gone once the goal is reached
        // (the app's window, the Dock: they are what the user should look at)
        let dim = match (phase, t.done_at.is_some()) {
            (Phase::Session, true) => 0.0,
            (Phase::Session, false) => 0.42,
            _ => 0.58,
        };
        let key = (t.step, target.map(|r| [r.0 as i32, r.1 as i32, r.2 as i32, r.3 as i32]), (dim * 100.0) as u32);
        if t.base.as_ref().is_none_or(|(k, _)| *k != key) {
            t.base = Some((key, dimming(w, h, s, if dim > 0.0 { target } else { None }, accent, dim)));
        }
        let mut c = t.base.as_ref().unwrap().1.clone();
        let clock = t.started.elapsed().as_secs_f32();
        // the card
        let cw = 360.0 * s;
        let pad = 20.0 * s;
        let body_style = Style::dip(13.5, 400, s);
        let lines = text::wrap(step.body, body_style, cw - 2.0 * pad, 6);
        let done = t.done_at.is_some() && !step.great.is_empty();
        let ch = pad + 24.0 * s + 8.0 * s + lines.len() as f32 * 19.0 * s + if done { 30.0 * s } else { 0.0 } + 16.0 * s + 34.0 * s + pad;
        let (cx, cy, arrow) = match target {
            None => ((w as f32 - cw) / 2.0, (h as f32 - ch) / 2.0, None),
            // a strip along the left or right edge: the card beside it, the arrow sideways
            Some((tx, ty, tw, th)) if th > h as f32 * 0.5 => {
                let left = tx < w as f32 / 2.0;
                let x = if left { tx + tw + 90.0 * s } else { tx - 90.0 * s - cw };
                let y = ty + (th - ch) / 2.0;
                (x, y, Some(Arrow::Side { left, edge: if left { tx + tw + 10.0 * s } else { tx - 10.0 * s }, y: y + ch / 2.0 }))
            }
            Some((tx, ty, tw, th)) => {
                let mid = tx + tw / 2.0;
                let x = (mid - cw / 2.0).clamp(24.0 * s, w as f32 - cw - 24.0 * s);
                let below = ty + th + 8.0 * s + 52.0 * s + ch < h as f32;
                let y = if below { ty + th + 8.0 * s + 52.0 * s } else { ty - 8.0 * s - 52.0 * s - ch };
                (x, y, Some(Arrow::Up { x: mid.clamp(x + 30.0 * s, x + cw - 30.0 * s), below, edge: if below { ty + th + 8.0 * s } else { ty - 8.0 * s } }))
            }
        };
        let (r, shadow) = (18.0 * s, Rgba::BLACK.alpha(0.35));
        c.shadow(cx, cy, cw, ch, r, 18.0 * s, 8.0 * s, shadow);
        let paper = Rgba::rgba(250, 250, 253, 250);
        c.fill_round_rect(cx, cy, cw, ch, r, paper);
        // the arrow: from the card to the target, bobbing
        let bob = (clock * 5.0).sin() * 5.0 * s;
        let half = 14.0 * s;
        match arrow {
            Some(Arrow::Up { x, below, edge }) => {
                let (tip, base) = if below { (edge + bob, cy + 2.0 * s) } else { (edge - bob, cy + ch - 2.0 * s) };
                triangle(&mut c, (x, tip), (x - half, base), (x + half, base), paper);
            }
            Some(Arrow::Side { left, edge, y }) => {
                // towards the edge and back, as a push
                let tip = if left { edge - bob.abs() } else { edge + bob.abs() };
                let head = if left { tip + 2.0 * half } else { tip - 2.0 * half };
                let base = if left { cx + 2.0 * s } else { cx + cw - 2.0 * s };
                c.fill_capsule(base, y, head, y, 5.0 * s, paper);
                triangle(&mut c, (tip, y), (head, y - half), (head, y + half), paper);
            }
            None => {}
        }
        let ink = Rgba::rgb(0x1d, 0x1d, 0x1f);
        let grey = Rgba::rgb(0x6e, 0x6e, 0x73);
        let px = |v: f32| (v * s).round() as i32;
        let title = if t.step == 0 { format!("{} {}", step.title, env!("CARGO_PKG_VERSION")) } else { step.title.to_string() };
        surface::text(&mut c, &title, cx + pad, cy + pad, cw - 2.0 * pad, px(18.0), 700, ink, Align::Left);
        let mut y = cy + pad + 24.0 * s + 8.0 * s;
        for l in &lines {
            surface::text(&mut c, l, cx + pad, y, cw - 2.0 * pad, px(13.5), 400, grey, Align::Left);
            y += 19.0 * s;
        }
        if done {
            surface::text(&mut c, &format!("✓ {}", step.great), cx + pad, y + 4.0 * s, cw - 2.0 * pad, px(14.0), 700, Rgba::rgb(0x1f, 0x9d, 0x55), Align::Left);
        }
        // the buttons: Skip (not on the last card), Next (Done on the last)
        let by = cy + ch - pad - 34.0 * s;
        let last = t.step == STEPS.len() - 1;
        let label = match t.step {
            DEMO_END => "Back to Start",
            _ if last => "Done",
            _ => "Next",
        };
        let bw = (text::width(label, Style::dip(14.0, 600, s)) + 40.0 * s).max(84.0 * s);
        let next = (cx + cw - pad - bw, by, bw, 34.0 * s);
        let emphasised = last || t.done_at.is_some() || step.target == Target::None;
        let fill = if emphasised { accent } else { Rgba::rgba(0, 0, 0, 20) };
        c.fill_capsule(next.0 + next.3 / 2.0, next.1 + next.3 / 2.0, next.0 + next.2 - next.3 / 2.0, next.1 + next.3 / 2.0, next.3 / 2.0, fill);
        surface::text(&mut c, label, next.0, next.1 + 8.0 * s, next.2, px(14.0), 600, if emphasised { Rgba::WHITE } else { ink }, Align::Center);
        t.next = next;
        t.skip = if last { (0.0, 0.0, 0.0, 0.0) } else { (cx + pad, by, 80.0 * s, 34.0 * s) };
        if !last {
            surface::text(&mut c, "Skip", t.skip.0, t.skip.1 + 8.0 * s, t.skip.2, px(14.0), 500, grey, Align::Left);
        }
        // the part's dots
        let steps = part(t.step);
        let dots = steps.len();
        let dx = cx + cw / 2.0 - (dots as f32 * 12.0 * s) / 2.0;
        for (n, i) in steps.enumerate() {
            c.fill_circle(dx + n as f32 * 12.0 * s + 4.0 * s, by + 17.0 * s, 3.0 * s, if i == t.step { accent } else { Rgba::rgba(0, 0, 0, 40) });
        }
        // in: fading
        let a = ((t.started.elapsed().as_secs_f32() / 0.35).min(1.0) * 255.0) as u8;
        t.surf.present(&c, t.mon.left, t.mon.top, a, None);
    });
    if leave {
        finish();
    }
}

/// Next (or Skip) was clicked at (x, y) of the overlay.
fn click(x: f32, y: f32) {
    let inside = |r: Rect| r.2 > 0.0 && x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3;
    enum Do {
        Nothing,
        Next,
        Skip,
    }
    let (what, step) = TOUR.with(|t| {
        t.borrow().as_ref().map_or((Do::Nothing, 0), |t| {
            let d = if inside(t.next) {
                Do::Next
            } else if inside(t.skip) {
                Do::Skip
            } else {
                Do::Nothing
            };
            (d, t.step)
        })
    });
    let in_demo = (DEMO_STEP + 1..=DEMO_END).contains(&step);
    match what {
        // Skip over the demo still leaves it (its windows are a pretend Mac's)
        Do::Skip if in_demo => leave_demo(false),
        Do::Skip => finish(),
        Do::Next if step == DEMO_END => leave_demo(true),
        Do::Next => {
            let last = TOUR.with(|t| {
                let mut b = t.borrow_mut();
                let t = b.as_mut()?;
                if t.step + 1 >= STEPS.len() {
                    return Some(true);
                }
                // the demo is reached by clicking it, not by Next
                if t.step != DEMO_STEP {
                    t.step += 1;
                    t.done_at = None;
                }
                Some(false)
            });
            if last == Some(true) {
                finish();
            } else {
                tick();
            }
        }
        Do::Nothing => {}
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER => {
            tick();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            click((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
