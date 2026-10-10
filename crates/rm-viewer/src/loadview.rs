//! What the app loading window looks like (drawn here, shown by splash.rs): instead of a card,
//! a window the shape of a Mac window opens at once — traffic lights and the app's name in its
//! title bar, its content a soft blur of the app's own icon in the icon's dominant colour, the
//! icon and the name in the middle and the Mac's spinning activity indicator under them, with
//! what is happening in small text. When the real window arrives the loading window moves onto
//! it, and fades away once the app's first picture is shown.
//!
//! Portable (no Win32): text comes in as coverage masks, so the look can be checked anywhere.

use crate::paint::{dominant, Canvas, Rgba};

/// The colours and the blurred background of one app's loading window, made once.
pub struct Look {
    /// a small, heavily blurred picture of the icon on its dominant colour, scaled up to fill
    pub bg: Canvas,
    pub dominant: Rgba,
    /// light background: dark text
    pub dark_text: bool,
    pub icon: Option<Canvas>,
}

impl Look {
    /// From the app's icon (straight RGBA, square), else from `fallback` (no icon yet).
    pub fn new(icon: Option<&Canvas>, fallback: Rgba) -> Look {
        let dom = icon.map(dominant).unwrap_or(fallback);
        // keep it rich but never glaring or muddy
        let dom = {
            let l = dom.luma();
            if l > 0.85 {
                dom.shade(-0.18)
            } else if l < 0.12 {
                dom.shade(0.12)
            } else {
                dom
            }
        };
        let (w, h) = (96usize, 64usize);
        let mut bg = Canvas::new(w, h);
        let (top, bottom) = (dom.shade(0.22), dom.shade(-0.38));
        bg.fill_round_rect_with(0.0, 0.0, w as f32, h as f32, 0.0, |_, y| top.lerp(bottom, y / h as f32));
        if let Some(i) = icon {
            // the icon, very large and very soft: the colours of the app, not its picture
            let s = h as f32 * 1.5;
            bg.draw(i, (w as f32 - s) / 2.0 + 8.0, -s * 0.18, s, s, 0.0, 0.55);
            bg.draw(i, -s * 0.45, h as f32 * 0.35, s * 0.8, s * 0.8, 0.0, 0.35);
        }
        bg.blur(5, 3);
        bg.saturate(1.25);
        let dark_text = top.lerp(bottom, 0.5).luma() > 0.6;
        Look { bg, dominant: dom, dark_text, icon: icon.cloned() }
    }

    pub fn text_color(&self) -> Rgba {
        if self.dark_text {
            Rgba::rgba(0, 0, 0, 220)
        } else {
            Rgba::rgba(255, 255, 255, 242)
        }
    }
}

/// A line of text already drawn as a coverage mask (mask, width, height).
pub type TextMask = (Vec<u8>, usize, usize);

#[derive(Default)]
pub struct Texts {
    pub title: Option<TextMask>,
    pub name: Option<TextMask>,
    pub status: Option<TextMask>,
}

/// One frame: where the window is inside the canvas, at what display scale, the spinner's turn.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// the window (x, y, w, h) inside the canvas, device pixels
    pub panel: (f32, f32, f32, f32),
    pub scale: f32,
    /// 0..1, one turn of the spinner
    pub phase: f32,
    pub error: bool,
}

/// Room around the window for its shadow (device px).
pub fn margin(scale: f32) -> usize {
    (44.0 * scale).ceil() as usize
}

/// The shadow under a window, made once per scale and stretched to any size (nine-slice).
pub fn shadow_source(scale: f32) -> (Canvas, usize) {
    let m = margin(scale);
    let core = (40.0 * scale) as usize;
    let size = core + 2 * m;
    let mut c = Canvas::new(size, size);
    c.shadow(m as f32, m as f32, core as f32, core as f32, 12.0 * scale, 26.0 * scale, 10.0 * scale, Rgba::rgba(0, 0, 0, 120));
    c.shadow(m as f32, m as f32, core as f32, core as f32, 12.0 * scale, 2.0 * scale, 1.0 * scale, Rgba::rgba(0, 0, 0, 60));
    (c, m + (16.0 * scale) as usize)
}

/// Draw everything but the spinner and the status line (what does not change while waiting).
pub fn draw_static(c: &mut Canvas, look: &Look, f: &Frame, t: &Texts, shadow: &(Canvas, usize)) {
    let s = f.scale;
    let (x, y, w, h) = f.panel;
    let r = 12.0 * s;
    // the shadow, stretched around the window
    let m = margin(s) as f32;
    let (sx, sy) = ((x - m).round() as isize, (y - m).round() as isize);
    c.nine_slice(&shadow.0, shadow.1, sx, sy, (w + 2.0 * m).round() as usize, (h + 2.0 * m).round() as usize, 1.0);
    // the window: the blurred colours of the app
    c.draw(&look.bg, x, y, w, h, r, 1.0);
    // a hairline around it (as a Mac window's edge)
    c.stroke_round_rect_with(x, y, w, h, r, s.max(1.0), |_, _| Rgba::rgba(0, 0, 0, 46));
    c.stroke_round_rect_with(x + s, y + s, w - 2.0 * s, h - 2.0 * s, r - s, s.max(1.0), |_, py| Rgba::WHITE.alpha(0.16 * (1.0 - ((py - y) / h).min(1.0))));
    // the title bar: a lighter band, a separator, the traffic lights, the title
    let bar = 28.0 * s;
    let band = if look.dark_text { Rgba::WHITE.alpha(0.22) } else { Rgba::WHITE.alpha(0.10) };
    // (the window's rounded top corners, square at the separator)
    c.fill_round_rect_with(x, y, w, bar + r, r, |_, py| if py < y + bar { band } else { Rgba::CLEAR });
    c.fill_round_rect(x + s, y + bar, w - 2.0 * s, s.max(1.0), 0.0, if look.dark_text { Rgba::rgba(0, 0, 0, 30) } else { Rgba::rgba(0, 0, 0, 60) });
    for (i, col) in [Rgba::rgb(255, 95, 87), Rgba::rgb(254, 188, 46), Rgba::rgb(40, 200, 64)].into_iter().enumerate() {
        let (cx, cy) = (x + (20.0 + 20.0 * i as f32) * s, y + bar / 2.0);
        c.fill_circle(cx, cy, 6.0 * s, col);
        c.stroke_round_rect_with(cx - 6.0 * s, cy - 6.0 * s, 12.0 * s, 12.0 * s, 6.0 * s, 0.5 * s, |_, _| Rgba::rgba(0, 0, 0, 40));
    }
    let tc = look.text_color();
    if let Some((mask, mw, mh)) = &t.title {
        c.fill_mask(mask, *mw, (x + (w - *mw as f32) / 2.0).round() as isize, (y + (bar - *mh as f32) / 2.0).round() as isize, tc.fade(0.85));
    }
    // the icon, with a soft shadow, and the name under it
    let (icon_s, cy) = content_layout(f);
    if let Some(icon) = &look.icon {
        let ix = x + (w - icon_s) / 2.0;
        c.shadow(ix + icon_s * 0.1, cy, icon_s * 0.8, icon_s * 0.9, icon_s * 0.22, 14.0 * s, 8.0 * s, Rgba::rgba(0, 0, 0, 70));
        c.draw(icon, ix, cy, icon_s, icon_s, 0.0, 1.0);
    }
    if let Some((mask, mw, _)) = &t.name {
        c.fill_mask(mask, *mw, (x + (w - *mw as f32) / 2.0).round() as isize, (cy + icon_s + 14.0 * s).round() as isize, tc);
    }
}

/// The icon's size and top, for the window's size.
fn content_layout(f: &Frame) -> (f32, f32) {
    let s = f.scale;
    let (_, y, _, h) = f.panel;
    let icon_s = (112.0 * s).min(h * 0.32).max(24.0 * s);
    let bar = 28.0 * s;
    // icon + name + spinner + status, centred in the content area
    let block = icon_s + 14.0 * s + 26.0 * s + 22.0 * s + 28.0 * s + 18.0 * s;
    let top = y + bar + ((h - bar) - block).max(0.0) * 0.45;
    (icon_s, top)
}

/// Where the spinner and the status line go: (x, y, w, h) to redraw each turn.
pub fn spinner_area(f: &Frame) -> (f32, f32, f32, f32) {
    let s = f.scale;
    let (x, _, w, _) = f.panel;
    let (icon_s, top) = content_layout(f);
    let y0 = top + icon_s + 14.0 * s + 26.0 * s + 8.0 * s;
    (x + 8.0 * s, y0, w - 16.0 * s, 76.0 * s)
}

/// The Mac's activity indicator (twelve spokes, the brightest going round) or, after an error,
/// a warning sign; and the status line under it.
pub fn draw_spinner(c: &mut Canvas, look: &Look, f: &Frame, t: &Texts) {
    let s = f.scale;
    let (ax, ay, aw, _) = spinner_area(f);
    let (cx, cy) = (ax + aw / 2.0, ay + 16.0 * s);
    let tc = look.text_color();
    if f.error {
        c.fill_circle(cx, cy, 11.0 * s, Rgba::rgb(255, 69, 58));
        c.fill_capsule(cx, cy - 5.5 * s, cx, cy + 1.0 * s, 1.4 * s, Rgba::WHITE);
        c.fill_circle(cx, cy + 5.0 * s, 1.5 * s, Rgba::WHITE);
    } else {
        let lead = (f.phase.rem_euclid(1.0) * 12.0).floor();
        for i in 0..12 {
            let a = i as f32 / 12.0 * std::f32::consts::TAU;
            let (sn, cs) = a.sin_cos();
            let (r0, r1) = (7.0 * s, 14.0 * s);
            let age = ((lead - i as f32).rem_euclid(12.0)) / 12.0;
            c.fill_capsule(cx + sn * r0, cy - cs * r0, cx + sn * r1, cy - cs * r1, 1.35 * s, tc.fade(1.0 - 0.8 * age));
        }
    }
    if let Some((mask, mw, _)) = &t.status {
        c.fill_mask(mask, *mw, (ax + (aw - *mw as f32) / 2.0).round() as isize, (cy + 26.0 * s).round() as isize, tc.fade(0.78));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn teal_icon() -> Canvas {
        let mut i = Canvas::new(64, 64);
        i.fill_round_rect(4.0, 4.0, 56.0, 56.0, 14.0, Rgba::rgb(30, 150, 160));
        i.fill_circle(32.0, 32.0, 14.0, Rgba::WHITE);
        i
    }

    #[test]
    fn the_look_takes_the_icons_colour() {
        let l = Look::new(Some(&teal_icon()), Rgba::rgb(10, 132, 255));
        assert!(l.dominant.g > l.dominant.r && l.dominant.b > l.dominant.r, "{:?}", l.dominant);
        let c = l.bg.get(48, 32);
        assert!(c[1] > c[2].saturating_sub(40) && c[1] > c[2] / 2 && c[1] as i32 - (c[2] as i32) < 120, "{c:?}");
        assert!(!l.dark_text, "white text on teal");
        let pale = Look::new(None, Rgba::rgb(240, 240, 240));
        assert!(pale.dominant.luma() < 0.85 && pale.dark_text);
    }

    #[test]
    fn a_frame_has_a_window_with_lights_and_a_turning_spinner() {
        let s = 1.0;
        let look = Look::new(Some(&teal_icon()), Rgba::rgb(10, 132, 255));
        let m = margin(s) as f32;
        let (w, h) = (480.0, 320.0);
        let mut c = Canvas::new((w + 2.0 * m) as usize, (h + 2.0 * m) as usize);
        let f = Frame { panel: (m, m, w, h), scale: s, phase: 0.0, error: false };
        draw_static(&mut c, &look, &f, &Texts::default(), &shadow_source(s));
        draw_spinner(&mut c, &look, &f, &Texts::default());
        assert!(c.get(5, 5)[3] < 40, "only shadow far outside");
        assert_eq!(c.get((m + 20.0) as usize, (m + 14.0) as usize)[2], 255, "the red light");
        let (ax, ay, aw, _) = spinner_area(&f);
        let mut later = c.clone();
        draw_spinner(&mut later, &look, &Frame { phase: 0.5, ..f }, &Texts::default());
        let area = |k: &Canvas| (0..30).flat_map(|dy| (0..30).map(move |dx| (dx, dy))).map(|(dx, dy)| k.get((ax + aw / 2.0 - 15.0) as usize + dx, ay as usize + dy)[1] as u32).sum::<u32>();
        assert_ne!(area(&c), area(&later), "the spinner turns");
    }

    /// A picture of the loading window (for eyes): `RM_LOADING_PROOF=out.ppm cargo test -p
    /// rm-viewer loading_proof -- --ignored`. Text is drawn by Windows, so here it is bars.
    #[test]
    #[ignore]
    fn loading_proof() {
        let s = 1.0;
        let mut icon = Canvas::new(128, 128);
        icon.fill_round_rect(8.0, 8.0, 112.0, 112.0, 26.0, Rgba::rgb(22, 140, 150));
        icon.fill_round_rect(30.0, 34.0, 68.0, 52.0, 8.0, Rgba::rgb(240, 248, 250));
        icon.fill_round_rect(40.0, 92.0, 48.0, 8.0, 4.0, Rgba::rgb(240, 248, 250));
        let look = Look::new(Some(&icon), Rgba::rgb(10, 132, 255));
        let m = margin(s) as f32;
        let (w, h) = (720.0, 460.0);
        let mut c = Canvas::filled((w + 2.0 * m) as usize, (h + 2.0 * m) as usize, Rgba::rgb(236, 236, 240));
        let f = Frame { panel: (m, m, w, h), scale: s, phase: 0.3, error: false };
        let bar = |len: usize, hh: usize| -> TextMask { (vec![255; len * hh], len, hh) };
        let t = Texts { title: Some(bar(40, 9)), name: Some(bar(90, 16)), status: Some(bar(110, 9)) };
        draw_static(&mut c, &look, &f, &t, &shadow_source(s));
        draw_spinner(&mut c, &look, &f, &t);
        let Some(path) = std::env::var_os("RM_LOADING_PROOF") else { return };
        let mut ppm = format!("P6 {} {} 255\n", c.w, c.h).into_bytes();
        for p in &c.px {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(path, ppm).unwrap();
    }
}
