//! The title bar of a Mac app's window here (MacBridge's frame), after macOS 26: the window's
//! own surface with the red, yellow and green buttons, the app's menus beside them (the app's
//! name in bold) and the window's title in the middle of the room left. Its text is set as the
//! rest of MacBridge's (text.rs: Inter, Apple's tracking), its buttons are the launcher's
//! (look.rs), light or dark as Windows is; the menu title under the pointer, or open, sits on a
//! small glass pill that comes in with a quick fade. Drawn into a canvas (previewed in the
//! tests); ui.rs puts it on the window and opens the menus (glassmenu.rs).

use crate::look::{self, Light};
use crate::paint::{Canvas, Rgba};
use crate::text::{self, Style};

/// Height in DIPs (1 DIP = 1 Mac point); the same as `chrome::TITLE_H`.
pub const H: f32 = 40.0;
/// The first button's centre from the left (DIPs), as on the Mac.
pub const LIGHTS_X: f32 = 20.0;
/// Room on each side of a menu title (DIPs).
const PAD: f32 = 8.0;
/// The text size (points).
const SIZE: f32 = 13.0;

/// What the bar shows.
pub struct Bar<'a> {
    /// width in pixels, and pixels per DIP
    pub w: usize,
    pub scale: f32,
    pub active: bool,
    pub dark: bool,
    /// a dialog: no yellow or green button to use (shown grey)
    pub dialog: bool,
    pub maximized: bool,
    pub title: &'a str,
    /// the menus' titles (the first is the app's name) and whether each can be opened
    pub menus: &'a [(String, bool)],
    /// the pointer is over the buttons (their marks show), and the one pressed
    pub hover_lights: bool,
    pub pressed: Option<Light>,
    /// the menu title lit: which, how much (0..1, it fades in), and whether its menu is open
    pub lit: Option<(usize, f32, bool)>,
}

/// Where things are (pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// each menu title's span (those that fit, as on the Mac)
    pub menus: Vec<(f32, f32)>,
    /// the title's span, when there is room for it
    pub title: Option<(f32, f32)>,
}

fn menu_style(i: usize, s: f32) -> Style {
    Style::dip(SIZE, if i == 0 { 700 } else { 400 }, s)
}

fn title_style(s: f32) -> Style {
    Style::dip(SIZE, 600, s)
}

/// Where the menus start: right of the three buttons.
fn lights_end(s: f32) -> f32 {
    (LIGHTS_X + 2.0 * look::LIGHT_STEP + look::LIGHT_D / 2.0 + 12.0) * s
}

pub fn layout(b: &Bar) -> Layout {
    let (s, w) = (b.scale, b.w as f32);
    let mut x = lights_end(s) - PAD * s / 2.0;
    let mut menus = vec![];
    for (i, (t, _)) in b.menus.iter().enumerate() {
        let span = (x, x + text::width(t, menu_style(i, s)) + 2.0 * PAD * s);
        if span.1 > w - 8.0 * s {
            break; // as on the Mac, menus that do not fit are left out
        }
        menus.push(span);
        x = span.1;
    }
    let end = menus.last().map_or(lights_end(s), |m| m.1);
    let gap = 16.0 * s;
    let tw = text::width(b.title, title_style(s));
    let centred = (w - tw) / 2.0;
    let title = if b.title.is_empty() {
        None
    } else if centred > end + gap {
        Some((centred, centred + tw))
    } else if w - gap - (end + gap) > 40.0 * s {
        Some((end + gap, w - gap))
    } else {
        None
    };
    Layout { menus, title }
}

/// The menu title at `x` (pixels), if any.
pub fn menu_at(l: &Layout, x: f32) -> Option<usize> {
    l.menus.iter().position(|(a, b)| x >= *a && x < *b)
}

/// The bar's colours: (surface, menu text, menu text that cannot be used, title, the line under it).
pub fn colors(dark: bool, active: bool) -> (Rgba, Rgba, Rgba, Rgba, Rgba) {
    match (dark, active) {
        (false, true) => (Rgba::rgb(250, 250, 250), Rgba::rgba(0, 0, 0, 222), Rgba::rgba(0, 0, 0, 66), Rgba::rgba(0, 0, 0, 200), Rgba::rgb(222, 222, 224)),
        (false, false) => (Rgba::rgb(244, 244, 245), Rgba::rgba(0, 0, 0, 110), Rgba::rgba(0, 0, 0, 50), Rgba::rgba(0, 0, 0, 90), Rgba::rgb(222, 222, 224)),
        (true, true) => (Rgba::rgb(42, 42, 45), Rgba::rgba(255, 255, 255, 230), Rgba::rgba(255, 255, 255, 70), Rgba::rgba(255, 255, 255, 210), Rgba::rgb(18, 18, 20)),
        (true, false) => (Rgba::rgb(34, 34, 37), Rgba::rgba(255, 255, 255, 110), Rgba::rgba(255, 255, 255, 50), Rgba::rgba(255, 255, 255, 100), Rgba::rgb(18, 18, 20)),
    }
}

/// The bar, `w` x `H * scale` pixels.
pub fn draw(b: &Bar) -> Canvas {
    let s = b.scale;
    let h = (H * s).round() as usize;
    let (bg, fg, fg_off, title_fg, line) = colors(b.dark, b.active);
    let mut c = Canvas::filled(b.w.max(1), h.max(1), bg);
    let cy = h as f32 / 2.0;
    look::traffic_lights(&mut c, LIGHTS_X * s, cy, s, b.active, b.hover_lights, b.pressed, b.maximized, b.dark);
    if b.dialog {
        // a dialog's yellow and green buttons cannot be used: grey, as on the Mac
        let (rim, fill) = if b.dark { (Rgba::rgb(0x52, 0x52, 0x57), Rgba::rgb(0x46, 0x46, 0x4b)) } else { (Rgba::rgb(0xcb, 0xcb, 0xcf), Rgba::rgb(0xdc, 0xdc, 0xde)) };
        let r = look::LIGHT_D * s / 2.0;
        for i in 1..3 {
            let x = (LIGHTS_X + i as f32 * look::LIGHT_STEP) * s;
            c.fill_circle(x, cy, r + 0.5, bg);
            c.fill_circle(x, cy, r, rim);
            c.fill_circle(x, cy, r - 0.5 * s.max(1.0), fill);
        }
    }
    let l = layout(b);
    for (i, ((t, enabled), (x0, x1))) in b.menus.iter().zip(&l.menus).enumerate() {
        if let Some((_, amount, open)) = b.lit.filter(|lit| lit.0 == i && *enabled) {
            pill(&mut c, *x0 + 2.0 * s, cy - 13.0 * s, x1 - x0 - 4.0 * s, 26.0 * s, s, b.dark, amount, open);
        }
        let (mask, mw, mh) = text::line(t, menu_style(i, s), x1 - x0);
        let col = if *enabled { fg } else { fg_off };
        c.fill_mask(&mask, mw, (x0 + PAD * s).round() as isize, ((h as f32 - mh as f32) / 2.0).round() as isize, col);
    }
    if let Some((x0, x1)) = l.title {
        let (mask, mw, mh) = text::line(b.title, title_style(s), x1 - x0);
        // centred in its span (a title cut short fills it)
        let x = x0 + ((x1 - x0) - mw as f32).max(0.0) / 2.0;
        c.fill_mask(&mask, mw, x.round() as isize, ((h as f32 - mh as f32) / 2.0).round() as isize, title_fg);
    }
    c.fill_round_rect(0.0, h as f32 - s.max(1.0).round(), b.w as f32, s.max(1.0).round(), 0.0, line);
    c
}

/// The glass pill under a menu title: a tint of the bar, a light rim along its top edge and a
/// soft shade along its bottom, as Liquid Glass on a flat surface; stronger while open.
#[allow(clippy::too_many_arguments)]
fn pill(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, s: f32, dark: bool, amount: f32, open: bool) {
    let k = amount.clamp(0.0, 1.0);
    let r = 8.0 * s;
    let (tint, rim, shade) = match (dark, open) {
        (false, false) => (Rgba::BLACK.alpha(0.055), Rgba::WHITE.alpha(0.95), Rgba::BLACK.alpha(0.05)),
        (false, true) => (Rgba::BLACK.alpha(0.10), Rgba::WHITE.alpha(0.95), Rgba::BLACK.alpha(0.08)),
        (true, false) => (Rgba::WHITE.alpha(0.09), Rgba::WHITE.alpha(0.20), Rgba::BLACK.alpha(0.25)),
        (true, true) => (Rgba::WHITE.alpha(0.15), Rgba::WHITE.alpha(0.26), Rgba::BLACK.alpha(0.30)),
    };
    c.fill_round_rect(x, y, w, h, r, tint.fade(k));
    let edge = s.max(1.0);
    c.stroke_round_rect_with(x, y, w, h, r, edge, |_, py| {
        // light from above: the rim bright at the top, gone by the middle; a shade at the bottom
        let t = ((py - y) / h).clamp(0.0, 1.0);
        if t < 0.5 {
            rim.fade(k * (1.0 - t * 2.0))
        } else {
            shade.fade(k * (t - 0.5) * 2.0)
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menus() -> Vec<(String, bool)> {
        ["TextEdit", "File", "Edit", "Format", "View", "Window", "Help"].iter().map(|t| (t.to_string(), true)).collect()
    }

    fn bar<'a>(w: usize, menus: &'a [(String, bool)], title: &'a str) -> Bar<'a> {
        Bar { w, scale: 1.0, active: true, dark: false, dialog: false, maximized: false, title, menus, hover_lights: false, pressed: None, lit: None }
    }

    #[test]
    fn menus_follow_the_buttons_and_the_title_is_centred_when_there_is_room() {
        let m = menus();
        let l = layout(&bar(1100, &m, "Untitled"));
        assert_eq!(l.menus.len(), 7);
        assert!(l.menus[0].0 > 70.0 && l.menus[0].0 < 80.0, "{:?}", l.menus[0]);
        for p in l.menus.windows(2) {
            assert_eq!(p[0].1, p[1].0, "menus side by side");
        }
        let (t0, t1) = l.title.unwrap();
        assert!(((t0 + t1) / 2.0 - 550.0).abs() < 1.0, "centred: {t0}..{t1}");
        assert_eq!(menu_at(&l, l.menus[2].0 + 1.0), Some(2));
        assert_eq!(menu_at(&l, 10.0), None);
    }

    #[test]
    fn a_narrow_window_leaves_menus_out_and_the_title_moves_or_goes() {
        let m = menus();
        let l = layout(&bar(330, &m, "A rather long document name.txt"));
        assert!(l.menus.len() < 7 && !l.menus.is_empty());
        assert!(l.menus.last().unwrap().1 <= 322.0);
        let wide = layout(&bar(560, &m, "Untitled"));
        let (t0, _) = wide.title.unwrap();
        assert!(t0 >= wide.menus.last().unwrap().1, "never over the menus");
        assert!(layout(&bar(900, &m, "")).title.is_none());
    }

    #[test]
    fn it_draws_in_every_state() {
        let m = menus();
        for dark in [false, true] {
            for active in [false, true] {
                let mut b = bar(700, &m, "Untitled");
                b.scale = 1.5;
                b.w = 1050;
                b.dark = dark;
                b.active = active;
                b.lit = Some((2, 1.0, true));
                b.dialog = true;
                let c = draw(&b);
                assert_eq!((c.w, c.h), (1050, 60));
                assert_eq!(c.px.iter().any(|p| p[2] > 200 && p[1] < 120), active, "the red button only while active");
            }
        }
    }

    #[test]
    fn a_lit_menu_title_sits_on_a_pill() {
        let m = menus();
        let mut b = bar(900, &m, "Untitled");
        let plain = draw(&b);
        b.lit = Some((1, 1.0, true));
        let lit = draw(&b);
        let (x0, x1) = layout(&b).menus[1];
        let changed = (x0 as usize..x1 as usize).filter(|&x| plain.get(x, 10) != lit.get(x, 10)).count();
        assert!(changed > 10, "the pill shows under File");
    }
}

/// Pictures of the bar (RM_PREVIEW=dir cargo test -p rm-viewer titlebar::preview).
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
        let m: Vec<(String, bool)> = ["TextEdit", "File", "Edit", "Format", "View", "Window", "Help"].iter().map(|t| (t.to_string(), *t != "Window")).collect();
        let w = (820.0 * s) as usize;
        for (name, dark, active, lit, hover) in [("titlebar-light", false, true, Some((2, 1.0, true)), false), ("titlebar-hover", false, true, Some((3, 1.0, false)), true), ("titlebar-dark", true, true, Some((1, 1.0, true)), false), ("titlebar-inactive", false, false, None, false)] {
            let b = Bar { w, scale: s, active, dark, dialog: false, maximized: false, title: "Untitled 2 — Edited", menus: &m, hover_lights: hover, pressed: None, lit };
            save(&draw(&b), &dir.join(format!("{name}.ppm")));
        }
    }
}
