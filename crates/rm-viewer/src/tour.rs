//! The guided tour: the first time Taro runs after an update, before it connects, the screen
//! dims a little and arrows point at what to do in the connect window (the ID, the password,
//! Connect). Do what the card says and it answers "Great!"; Next goes on, Skip ends the tour.
//! The controls under the arrows stay usable: the dimming has a hole where the arrow points.
use std::cell::RefCell;
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
/// What the dimming was made for: the step and the hole.
type BaseKey = (usize, Option<[i32; 4]>);

#[derive(Clone, Copy, PartialEq)]
enum Target {
    None,
    Id,
    Password,
    Connect,
}

struct Step {
    title: &'static str,
    body: &'static str,
    target: Target,
    great: &'static str,
}

const STEPS: [Step; 5] = [
    Step { title: "Welcome to Taro", body: "A quick tour of connecting to your Mac. It takes less than a minute, and you can skip it at any time.", target: Target::None, great: "" },
    Step { title: "Your Mac's ID", body: "Start Taro on the Mac (./taro --password …): it shows an ID. Type it here (or the Mac's address, under By Address).", target: Target::Id, great: "Great! That is the ID." },
    Step { title: "The password", body: "Type the password you gave Taro on the Mac.", target: Target::Password, great: "Great!" },
    Step { title: "Connect", body: "Press Connect. Taro finds the Mac and brings its apps here.", target: Target::Connect, great: "Connecting…" },
    Step { title: "You are all set", body: "Push the pointer against the edge of the screen to bring out the Mac's Dock. Ctrl+Alt+Space searches for an app. Settings has Stage Manager and updates.", target: Target::None, great: "" },
];

struct Tour {
    surf: Surface,
    step: usize,
    started: Instant,
    /// the step's goal was reached (and when)
    done_at: Option<Instant>,
    /// the whole tour ended (Connect pressed), closing soon
    ending: Option<Instant>,
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

/// This version as one number (1.2.9 -> 1_002_009).
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

/// Begin the tour when this version's has not been shown yet (and the connect window is up).
pub fn start(hinst: HINSTANCE) {
    if std::env::var_os("RM_NO_TOUR").is_some() || crate::settings::Settings::load().tour_seen >= code() {
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
    TOUR.with(|t| *t.borrow_mut() = Some(Tour { surf, step: 0, started: Instant::now(), done_at: None, ending: None, mon, scale, base: None, next: (0.0, 0.0, 0.0, 0.0), skip: (0.0, 0.0, 0.0, 0.0) }));
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

/// The tour goes (the connect window is gone). It is not counted as seen unless it was finished.
pub fn close() {
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
    close();
}

fn target_rect(p: &TourProbe, origin: (i32, i32), mon: RECT, kind: Target) -> Option<Rect> {
    let r = match kind {
        Target::None => return None,
        Target::Id => p.id,
        Target::Password => p.password,
        Target::Connect => p.connect,
    };
    Some((r.0 + (origin.0 - mon.left) as f32, r.1 + (origin.1 - mon.top) as f32, r.2, r.3))
}

fn goal_reached(step: usize, p: &TourProbe) -> bool {
    match step {
        1 => p.id_len > 0,
        2 => p.password_len > 0,
        3 => p.busy,
        _ => false,
    }
}

/// The dimming with a hole (and a ring) where the arrow points.
fn dimming(w: usize, h: usize, s: f32, hole: Option<Rect>, accent: Rgba) -> Canvas {
    let dim = 0.58f32;
    let mut c = Canvas::filled(w, h, Rgba::BLACK.alpha(dim));
    if let Some((x, y, ww, hh)) = hole {
        let pad = 8.0 * s;
        let (hx, hy, hw, hh) = (x - pad, y - pad, ww + 2.0 * pad, hh + 2.0 * pad);
        let r = 12.0 * s;
        let (x0, y0) = ((hx - 2.0).floor().max(0.0) as usize, (hy - 2.0).floor().max(0.0) as usize);
        let (x1, y1) = (((hx + hw + 2.0).ceil() as usize).min(w), ((hy + hh + 2.0).ceil() as usize).min(h));
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
    let (x0, x1) = (a.0.min(b.0).min(d.0).floor().max(0.0) as usize, (a.0.max(b.0).max(d.0).ceil() as usize).min(c.w));
    let (y0, y1) = (a.1.min(b.1).min(d.1).floor().max(0.0) as usize, (a.1.max(b.1).max(d.1).ceil() as usize).min(c.h));
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

fn tick() {
    let Some((_, origin, probe)) = crate::connect::tour_probe() else {
        close();
        return;
    };
    let mut leave = false;
    TOUR.with(|t| {
        let mut b = t.borrow_mut();
        let Some(t) = b.as_mut() else { return };
        let s = t.scale;
        let (w, h) = ((t.mon.right - t.mon.left) as usize, (t.mon.bottom - t.mon.top) as usize);
        let step = &STEPS[t.step];
        // the goal of the step
        if t.done_at.is_none() && goal_reached(t.step, &probe) {
            t.done_at = Some(Instant::now());
            if t.step == 3 {
                t.ending = Some(Instant::now());
            }
        }
        if t.ending.is_some_and(|e| e.elapsed() > Duration::from_millis(900)) {
            leave = true;
            return;
        }
        let accent = surface::accent();
        let target = target_rect(&probe, origin, t.mon, step.target);
        // the dimming is made again only when its hole moved
        let key = (t.step, target.map(|r| [r.0 as i32, r.1 as i32, r.2 as i32, r.3 as i32]));
        if t.base.as_ref().is_none_or(|(k, _)| *k != key) {
            t.base = Some((key, dimming(w, h, s, target, accent)));
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
            Some((tx, ty, tw, th)) => {
                let mid = tx + tw / 2.0;
                let x = (mid - cw / 2.0).clamp(24.0 * s, w as f32 - cw - 24.0 * s);
                let below = ty + th + 8.0 * s + 52.0 * s + ch < h as f32;
                let y = if below { ty + th + 8.0 * s + 52.0 * s } else { ty - 8.0 * s - 52.0 * s - ch };
                (x, y, Some((mid.clamp(x + 30.0 * s, x + cw - 30.0 * s), below, if below { ty + th + 8.0 * s } else { ty - 8.0 * s })))
            }
        };
        let (r, shadow) = (18.0 * s, Rgba::BLACK.alpha(0.35));
        c.shadow(cx, cy, cw, ch, r, 18.0 * s, 8.0 * s, shadow);
        c.fill_round_rect(cx, cy, cw, ch, r, Rgba::rgba(250, 250, 253, 250));
        // the arrow: from the card to the target, bobbing
        if let Some((ax, below, edge)) = arrow {
            let bob = (clock * 5.0).sin() * 5.0 * s;
            let (tip, base) = if below { (edge + bob, cy + 2.0 * s) } else { (edge - bob, cy + ch - 2.0 * s) };
            let half = 14.0 * s;
            triangle(&mut c, (ax, tip), (ax - half, base), (ax + half, base), Rgba::rgba(250, 250, 253, 250));
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
        let label = if last { "Done" } else { "Next" };
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
        // the step's dots
        let dots = STEPS.len();
        let dx = cx + cw / 2.0 - (dots as f32 * 12.0 * s) / 2.0;
        for i in 0..dots {
            c.fill_circle(dx + i as f32 * 12.0 * s + 4.0 * s, by + 17.0 * s, 3.0 * s, if i == t.step { accent } else { Rgba::rgba(0, 0, 0, 40) });
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
    let what = TOUR.with(|t| {
        t.borrow().as_ref().map_or(Do::Nothing, |t| {
            if inside(t.next) {
                Do::Next
            } else if inside(t.skip) {
                Do::Skip
            } else {
                Do::Nothing
            }
        })
    });
    match what {
        Do::Skip => finish(),
        Do::Next => {
            let last = TOUR.with(|t| {
                let mut b = t.borrow_mut();
                let t = b.as_mut()?;
                if t.step + 1 >= STEPS.len() {
                    return Some(true);
                }
                t.step += 1;
                t.done_at = None;
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
