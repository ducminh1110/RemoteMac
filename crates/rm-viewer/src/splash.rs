//! The app loading window. When a Mac app is opened from Windows, a window the shape of a Mac
//! window appears at once (loadview.rs draws it): the app's name in its title bar, a blur of
//! the app's icon in its dominant colour, the icon, the name and the Mac's spinning activity
//! indicator, with what is happening under it:
//!
//!  1 sending the launch to the Mac   2 the app is starting on the Mac
//!  3 its window is being set up       4 the stream is connecting (first picture)
//!
//! When the app's real window arrives, the loading window moves and resizes onto it (a spring),
//! then fades out as the first picture shows. An error is shown in the same window. It never
//! takes the focus; a click dismisses it (the app keeps opening).

use crate::loadview::{self, Frame, Look, TextMask, Texts};
use crate::motion::{tokens, Anim, Curve};
use crate::paint::Canvas;
use crate::surface::{self, Surface};
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const CLASS: PCWSTR = w!("RmLoading");
pub const STEPS: u32 = 4;
const TIMER: usize = 1;
/// Logical size of the loading window before the real one is known.
const W: f32 = 720.0;
const H: f32 = 460.0;

struct Card {
    surf: Surface,
    look: Look,
    name: String,
    texts: Texts,
    status: String,
    scale: f32,
    /// the window's rectangle on the screen (device px), animated
    x: Anim,
    y: Anim,
    w: Anim,
    h: Anim,
    /// 0..1 the whole surface
    alpha: Anim,
    /// appearing: a little smaller, growing to its size
    pop: Anim,
    step: u32,
    error: bool,
    since: Instant,
    close_at: Option<Instant>,
    closing: bool,
    arrived: bool,
    shadow: (Canvas, usize),
    /// the frame without the spinner, for the rectangle it was drawn at
    still: Option<((i32, i32, i32), Canvas)>,
    frame: Canvas,
    last_turn: i64,
}

thread_local! {
    static CARDS: RefCell<HashMap<String, Card>> = RefCell::new(HashMap::new());
}

pub fn register(hinst: HINSTANCE) {
    surface::register(hinst, CLASS, Some(proc), IDC_APPSTARTING);
}

fn step_text(step: u32, name: &str) -> String {
    match step {
        1 => format!("Opening {name}…"),
        2 => format!("Starting {name}…"),
        3 => "Preparing the window…".into(),
        _ => "Almost ready…".into(),
    }
}

fn mask(text: &str, px: f32, weight: i32, max_w: f32) -> Option<TextMask> {
    (!text.is_empty()).then(|| surface::text_mask(text, px.round() as i32, weight, max_w.max(8.0) as usize))
}

fn icon_canvas(icon: Option<&(u32, Vec<u8>)>) -> Option<Canvas> {
    icon.filter(|(s, px)| px.len() == (*s * *s * 4) as usize).map(|(s, px)| Canvas::from_rgba(*s as usize, *s as usize, px))
}

/// Show the loading window for `app` (step 1). `icon`: its square RGBA icon, when known.
pub fn show(hinst: HINSTANCE, app: &str, name: &str, icon: Option<(u32, Vec<u8>)>) {
    if CARDS.with(|c| c.borrow().contains_key(app)) {
        return;
    }
    let mut cur = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cur);
    }
    let scale = surface::scale_at(cur.x, cur.y);
    let wa = surface::work_area_at(cur.x, cur.y);
    let (aw, ah) = ((wa.right - wa.left) as f32, (wa.bottom - wa.top) as f32);
    let (w, h) = ((W * scale).min(aw * 0.8), (H * scale).min(ah * 0.8));
    let n = CARDS.with(|c| c.borrow().len()) as f32;
    let (x, y) = (wa.left as f32 + (aw - w) / 2.0 + n * 28.0 * scale, wa.top as f32 + (ah - h) * 0.42 + n * 28.0 * scale);
    let Some(surf) = Surface::new(hinst, CLASS, &format!("Opening {name}"), None, true, false) else { return };
    let look = Look::new(icon_canvas(icon.as_ref()).as_ref(), surface::accent());
    let status = step_text(1, name);
    let texts = Texts { title: mask(name, 13.0 * scale, 600, w * 0.5), name: mask(name, 20.0 * scale, 600, w * 0.8), status: mask(&status, 12.0 * scale, 400, w * 0.8) };
    let window = tokens::pick(tokens::WINDOW, 0.6);
    let card = Card {
        surf,
        look,
        name: name.to_string(),
        texts,
        status,
        scale,
        x: Anim::at(x),
        y: Anim::at(y),
        w: Anim::at(w),
        h: Anim::at(h),
        alpha: Anim::new(0.0, 1.0, tokens::pick(tokens::WINDOW, 0.2), Curve::Decelerate),
        pop: Anim::new(0.94, 1.0, window, Curve::ARRIVE),
        step: 1,
        error: false,
        since: Instant::now(),
        close_at: None,
        closing: false,
        arrived: false,
        shadow: loadview::shadow_source(scale),
        still: None,
        frame: Canvas::new(0, 0),
        last_turn: -1,
    };
    let hwnd = card.surf.hwnd;
    CARDS.with(|c| c.borrow_mut().insert(app.to_string(), card));
    tick(hwnd);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER, 16, None);
    }
}

fn with_card(app: &str, f: impl FnOnce(&mut Card)) {
    CARDS.with(|c| {
        if let Some(card) = c.borrow_mut().get_mut(app) {
            f(card)
        }
    });
}

fn set_status(card: &mut Card, text: String) {
    if card.status != text {
        let w = card.w.target();
        card.texts.status = mask(&text, 12.0 * card.scale, 400, w * 0.8);
        card.status = text;
        card.still = None;
    }
}

/// Move `app`'s loading window on to `step` (never back).
pub fn step(app: &str, step: u32) {
    with_card(app, |card| {
        if step > card.step && !card.error {
            card.step = step;
            set_status(card, step_text(step, &card.name));
        }
    });
}

/// Where `app`'s loading window is (for screenshots).
pub fn rect(app: &str) -> Option<RECT> {
    CARDS.with(|c| c.borrow().get(app).and_then(|k| surface::window_rect(k.surf.hwnd)))
}

pub fn showing(app: &str) -> bool {
    CARDS.with(|c| c.borrow().contains_key(app))
}

/// The app's icon arrived: the loading window takes its colours.
pub fn set_icon(app: &str, size: u32, rgba: &[u8]) {
    with_card(app, |card| {
        if card.look.icon.is_none() {
            card.look = Look::new(icon_canvas(Some(&(size, rgba.to_vec()))).as_ref(), surface::accent());
            card.still = None;
        }
    });
}

/// The app's window is on the screen at `r` (screen px): the loading window moves onto it.
pub fn arrive(app: &str, r: RECT) {
    with_card(app, |card| {
        if card.arrived || r.right - r.left < 40 || r.bottom - r.top < 40 {
            return;
        }
        card.arrived = true;
        eprintln!("{app}: its window came {} ms after opening", card.since.elapsed().as_millis());
        let (d, c) = (tokens::pick(tokens::WINDOW, 0.8), Curve::ARRIVE);
        card.x.retarget(r.left as f32, d, c);
        card.y.retarget(r.top as f32, d, c);
        card.w.retarget((r.right - r.left) as f32, d, c);
        card.h.retarget((r.bottom - r.top) as f32, d, c);
    });
}

/// The app's first picture is on the screen: fade out. Called for every picture while the
/// window shows, so only the first call counts (a later one would put the fade off for good).
pub fn done(app: &str) {
    with_card(app, |card| {
        if card.close_at.is_some() {
            return;
        }
        eprintln!("{app}: its first picture came {} ms after opening", card.since.elapsed().as_millis());
        set_status(card, "Ready".into());
        card.close_at = Some(Instant::now() + Duration::from_millis(if card.arrived { 120 } else { 250 }));
    });
}

/// Opening failed: say why in the window, then close.
pub fn fail(app: &str, why: &str) {
    with_card(app, |card| {
        card.error = true;
        set_status(card, why.to_string());
        card.close_at = Some(Instant::now() + Duration::from_secs(6));
    });
}

fn start_closing(card: &mut Card) {
    if !card.closing {
        card.closing = true;
        card.alpha.retarget(0.0, tokens::pick(tokens::POPOVER, 0.5), Curve::Accelerate);
    }
}

/// One frame: animate, draw, show. Removes the card once it has faded out.
fn tick(hwnd: HWND) {
    let gone = CARDS.with(|c| {
        let mut map = c.borrow_mut();
        let key = map.iter().find(|(_, k)| k.surf.hwnd == hwnd).map(|(k, _)| k.clone())?;
        let card = map.get_mut(&key)?;
        let now = Instant::now();
        if card.close_at.is_some_and(|t| now >= t) || card.since.elapsed() > Duration::from_secs(90) {
            start_closing(card);
        }
        if card.closing && card.alpha.done_at(now) {
            return Some(key);
        }
        draw(card, now);
        None
    });
    if let Some(key) = gone {
        // out of the map first: destroying the window sends it messages
        let card = CARDS.with(|c| c.borrow_mut().remove(&key));
        if let Some(card) = card {
            unsafe {
                let _ = KillTimer(Some(card.surf.hwnd), TIMER);
            }
            drop(card);
        }
    }
}

fn draw(card: &mut Card, now: Instant) {
    let s = card.scale;
    let m = loadview::margin(s) as f32;
    let (x, y, w, h) = (card.x.value_at(now), card.y.value_at(now), card.w.value_at(now).max(80.0), card.h.value_at(now).max(60.0));
    let k = card.pop.value_at(now);
    let (pw, ph) = (w * k, h * k);
    let (cw, ch) = ((w + 2.0 * m).round() as usize, (h + 2.0 * m).round() as usize);
    let f = Frame { panel: (m + (w - pw) / 2.0, m + (h - ph) / 2.0, pw, ph), scale: s, phase: card.since.elapsed().as_secs_f32() % 1.0, error: card.error };
    let key = (pw.round() as i32, ph.round() as i32, cw as i32 * 7919 + ch as i32);
    let turn = (card.since.elapsed().as_millis() / 83) as i64;
    let alpha = (card.alpha.value_at(now).clamp(0.0, 1.0) * 255.0).round() as u8;
    let moving = !(card.x.done_at(now) && card.y.done_at(now) && card.w.done_at(now) && card.h.done_at(now) && card.pop.done_at(now) && card.alpha.done_at(now));
    let fresh = card.still.as_ref().is_some_and(|(k, _)| *k == key);
    if !moving && fresh && turn == card.last_turn {
        return; // nothing changed since the last frame
    }
    card.last_turn = turn;
    if !fresh {
        let mut still = Canvas::new(cw, ch);
        loadview::draw_static(&mut still, &card.look, &f, &card.texts, &card.shadow);
        card.still = Some((key, still));
    }
    let still = &card.still.as_ref().unwrap().1;
    let (ax, ay, aw, ah) = loadview::spinner_area(&f);
    let area = (ax.max(0.0) as usize, ay.max(0.0) as usize, (aw.ceil() as usize).min(cw), (ah.ceil() as usize).min(ch));
    let dirty = if fresh && !moving && card.frame.w == cw && card.frame.h == ch {
        // only the spinner turned: put back what is under it, draw it again
        for row in area.1..(area.1 + area.3).min(ch) {
            let (a, b) = (row * cw + area.0, row * cw + (area.0 + area.2).min(cw));
            card.frame.px[a..b].copy_from_slice(&still.px[a..b]);
        }
        Some(area)
    } else {
        card.frame = still.clone();
        None
    };
    loadview::draw_spinner(&mut card.frame, &card.look, &f, &card.texts);
    card.surf.present(&card.frame, (x - m).round() as i32, (y - m).round() as i32, alpha, dirty);
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // a click dismisses it (the app keeps opening)
        WM_LBUTTONUP => {
            CARDS.with(|c| {
                if let Some(card) = c.borrow_mut().values_mut().find(|k| k.surf.hwnd == hwnd) {
                    start_closing(card);
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
