//! The reconnect banner: when the connection to the Mac is lost, a small glass capsule at the
//! top of the screen says so, with the Mac's spinner and the step the new attempt is at, while
//! MacBridge connects again by itself. Once connected it shows a check mark for a moment and
//! fades. It never takes the focus; a click hides it (connecting goes on).

use crate::motion::{tokens, Anim, Curve};
use crate::paint::{Canvas, Rgba};
use crate::surface::{self, Surface};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS: PCWSTR = w!("RmBanner");
const TIMER: usize = 1;

type Mask = (Vec<u8>, usize, usize);

struct Banner {
    surf: Surface,
    scale: f32,
    pos: (i32, i32),
    /// the glass capsule with its shadow, and the margin around it
    base: Canvas,
    margin: usize,
    w: f32,
    h: f32,
    title: Mask,
    step: Option<(String, Mask)>,
    alpha: Anim,
    drop: Anim,
    since: Instant,
    /// connected again: a check mark until then, then fading
    done_at: Option<Instant>,
    closing: bool,
    dark: bool,
}

thread_local! {
    static BANNER: RefCell<Option<Banner>> = const { RefCell::new(None) };
}

pub fn register(hinst: HINSTANCE) {
    surface::register(hinst, CLASS, Some(proc), IDC_ARROW);
}

/// Show "connection lost, connecting again" at the top of the monitor at (x, y).
pub fn show(hinst: HINSTANCE, x: i32, y: i32) {
    if BANNER.with(|b| b.borrow().is_some()) {
        return;
    }
    let scale = surface::scale_at(x, y);
    let wa = surface::work_area_at(x, y);
    let (w, h) = (460.0 * scale, 52.0 * scale);
    let pos = (wa.left + ((wa.right - wa.left) as f32 - w) as i32 / 2, wa.top + (14.0 * scale) as i32);
    let (base, margin) = surface::glass_panel(pos.0, pos.1, w as usize, h as usize, h / 2.0, crate::glass::Kind::Sheet, scale);
    let Some(surf) = Surface::new(hinst, CLASS, "MacBridge — connecting again", None, true, false) else { return };
    let (dark, _, _) = surface::glass_look();
    let title = surface::text_mask("Connection lost — connecting again…", (14.0 * scale).round() as i32, 600, (w * 0.75) as usize);
    let hwnd = surf.hwnd;
    BANNER.with(|b| {
        *b.borrow_mut() = Some(Banner {
            surf,
            scale,
            pos,
            base,
            margin,
            w,
            h,
            title,
            step: None,
            alpha: Anim::new(0.0, 1.0, tokens::pick(tokens::POPOVER, 0.6), Curve::Decelerate),
            drop: Anim::new(-12.0 * scale, 0.0, tokens::pick(tokens::WINDOW, 0.3), Curve::ARRIVE),
            since: Instant::now(),
            done_at: None,
            closing: false,
            dark,
        })
    });
    draw();
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER, 16, None);
    }
}

/// Connected again: a check mark, then gone.
pub fn connected() {
    BANNER.with(|b| {
        if let Some(m) = b.borrow_mut().as_mut() {
            m.done_at = Some(Instant::now());
            m.title = surface::text_mask("Connected again", (14.0 * m.scale).round() as i32, 600, (m.w * 0.75) as usize);
            m.step = None;
        }
    });
}

/// Where it is (for screenshots).
pub fn rect() -> Option<RECT> {
    BANNER.with(|b| b.borrow().as_ref().and_then(|m| surface::window_rect(m.surf.hwnd)))
}

/// Gone at once (connecting again failed: a message says why).
pub fn hide() {
    let b = BANNER.with(|b| b.borrow_mut().take());
    drop(b);
}

fn start_closing(m: &mut Banner) {
    if !m.closing {
        m.closing = true;
        m.alpha.retarget(0.0, tokens::pick(tokens::POPOVER, 0.5), Curve::Accelerate);
    }
}

fn draw() {
    BANNER.with(|b| {
        let mut b = b.borrow_mut();
        let Some(m) = b.as_mut() else { return };
        let s = m.scale;
        let now = Instant::now();
        // the step the attempt is at, under the title
        let phase = crate::lifecycle::current();
        if m.done_at.is_none() && phase.busy() && phase != crate::lifecycle::Phase::Reconnecting {
            let label = phase.label();
            if m.step.as_ref().is_none_or(|(l, _)| *l != label) {
                let mask = surface::text_mask(&label, (12.0 * s).round() as i32, 400, (m.w * 0.75) as usize);
                m.step = Some((label, mask));
            }
        }
        let mut c = m.base.clone();
        let (mx, my) = (m.margin as f32, m.margin as f32);
        let (fg, fg2) = surface::glass_text(m.dark);
        let (cx, cy) = (mx + 30.0 * s, my + m.h / 2.0);
        if m.done_at.is_some() {
            c.fill_circle(cx, cy, 11.0 * s, Rgba::rgb(48, 209, 88));
            c.fill_capsule(cx - 5.0 * s, cy, cx - 1.5 * s, cy + 4.0 * s, 1.3 * s, Rgba::WHITE);
            c.fill_capsule(cx - 1.5 * s, cy + 4.0 * s, cx + 5.5 * s, cy - 4.5 * s, 1.3 * s, Rgba::WHITE);
        } else {
            let lead = ((m.since.elapsed().as_millis() / 83) % 12) as f32;
            for i in 0..12 {
                let a = i as f32 / 12.0 * std::f32::consts::TAU;
                let (sn, cs) = a.sin_cos();
                let age = (lead - i as f32).rem_euclid(12.0) / 12.0;
                c.fill_capsule(cx + sn * 5.0 * s, cy - cs * 5.0 * s, cx + sn * 10.0 * s, cy - cs * 10.0 * s, 1.1 * s, fg.fade(1.0 - 0.8 * age));
            }
        }
        let tx = mx + 54.0 * s;
        let (mask, mw, mh) = &m.title;
        let ty = if m.step.is_some() { my + m.h / 2.0 - *mh as f32 + 1.0 * s } else { my + (m.h - *mh as f32) / 2.0 };
        c.fill_mask(mask, *mw, tx as isize, ty.round() as isize, fg);
        if let Some((_, (mask, sw, _))) = &m.step {
            c.fill_mask(mask, *sw, tx as isize, (my + m.h / 2.0 + 1.0 * s).round() as isize, fg2);
        }
        let a = (m.alpha.value_at(now).clamp(0.0, 1.0) * 255.0).round() as u8;
        let dy = m.drop.value_at(now).round() as i32;
        m.surf.present(&c, m.pos.0 - m.margin as i32, m.pos.1 - m.margin as i32 + dy, a, None);
    });
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            let gone = BANNER.with(|b| {
                let mut b = b.borrow_mut();
                let m = b.as_mut()?;
                if m.done_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1400)) {
                    start_closing(m);
                }
                Some(m.closing && m.alpha.done())
            });
            if gone == Some(true) {
                hide();
            } else {
                draw();
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            BANNER.with(|b| b.borrow_mut().as_mut().map(start_closing));
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}
