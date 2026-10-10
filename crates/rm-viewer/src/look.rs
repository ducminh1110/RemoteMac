//! MacBridge's own look, after MobileLab's interface (MIT; its Xcode 26 layout and colour tokens,
//! docs/design/xcode-interface.md there): the colour tokens in light and dark, the softly tinted
//! window, the floating rounded panel, the round accent mark. The launcher and the connect window
//! draw with these (paint.rs), so both look the same.

use crate::paint::{Canvas, Rgba};

/// MobileLab's colour tokens.
pub struct Theme {
    /// the window's three tints, along its 155-degree gradient
    pub win: [Rgba; 3],
    pub panel: Rgba,
    pub text: Rgba,
    pub text2: Rgba,
    pub text3: Rgba,
    pub accent: Rgba,
    pub field: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    pub capsule: Rgba,
    pub divider: Rgba,
    /// the panel's hairline and shadow
    pub ring: Rgba,
    pub shade: Rgba,
    pub pass: Rgba,
    pub warn: Rgba,
    pub fail: Rgba,
}

pub fn theme(dark: bool) -> Theme {
    if dark {
        Theme {
            win: [Rgba::rgb(0x17, 0x1b, 0x23), Rgba::rgb(0x1e, 0x22, 0x2b), Rgba::rgb(0x1a, 0x1e, 0x27)],
            panel: Rgba::rgb(0x1f, 0x1f, 0x24),
            text: Rgba::rgb(0xec, 0xec, 0xee),
            text2: Rgba::rgb(0xa0, 0xa0, 0xa8),
            text3: Rgba::rgb(0x6f, 0x6f, 0x78),
            accent: Rgba::rgb(0x0a, 0x84, 0xff),
            field: Rgba::rgb(0x33, 0x33, 0x38),
            hover: Rgba::WHITE.alpha(0.07),
            pressed: Rgba::WHITE.alpha(0.12),
            capsule: Rgba::rgb(0x3b, 0x3b, 0x3f),
            divider: Rgba::rgb(0x3a, 0x3a, 0x3f),
            ring: Rgba::WHITE.alpha(0.07),
            shade: Rgba::BLACK.alpha(0.4),
            pass: Rgba::rgb(0x32, 0xd1, 0x5b),
            warn: Rgba::rgb(0xff, 0xb3, 0x40),
            fail: Rgba::rgb(0xff, 0x45, 0x3a),
        }
    } else {
        Theme {
            win: [Rgba::rgb(0xd5, 0xe6, 0xf8), Rgba::rgb(0xe6, 0xee, 0xf8), Rgba::rgb(0xd9, 0xe3, 0xf2)],
            panel: Rgba::WHITE,
            text: Rgba::rgb(0x1d, 0x1d, 0x1f),
            text2: Rgba::rgb(0x6c, 0x6c, 0x72),
            text3: Rgba::rgb(0xa1, 0xa1, 0xa8),
            accent: Rgba::rgb(0x0a, 0x7a, 0xff),
            field: Rgba::rgb(0xef, 0xef, 0xf1),
            hover: Rgba::BLACK.alpha(0.05),
            pressed: Rgba::BLACK.alpha(0.09),
            capsule: Rgba::WHITE,
            divider: Rgba::rgb(0xe4, 0xe4, 0xe8),
            ring: Rgba::rgba(30, 50, 90, 31),
            shade: Rgba::rgba(20, 40, 80, 26),
            pass: Rgba::rgb(0x30, 0xb3, 0x56),
            warn: Rgba::rgb(0xff, 0x9f, 0x0a),
            fail: Rgba::rgb(0xff, 0x3b, 0x30),
        }
    }
}

/// The window's background: three tints along a 155-degree gradient, as MobileLab's.
pub fn tint(c: &mut Canvas, t: &Theme) {
    let (w, h) = (c.w as f32, c.h as f32);
    let (dx, dy) = (155f32.to_radians().sin(), -155f32.to_radians().cos());
    let span = (w * dx).abs() + (h * dy).abs();
    let (cx, cy) = (w / 2.0, h / 2.0);
    for y in 0..c.h {
        for x in 0..c.w {
            let u = (((x as f32 - cx) * dx + (y as f32 - cy) * dy) / span.max(1.0) + 0.5).clamp(0.0, 1.0);
            let col = if u < 0.48 { t.win[0].lerp(t.win[1], u / 0.48) } else { t.win[1].lerp(t.win[2], (u - 0.48) / 0.52) };
            c.blend(x, y, col, 1.0);
        }
    }
}

/// A floating panel at (x, y, w, h), corner `r`: a hairline ring, a soft near shadow and a wide
/// far one, filled with the panel colour (`s` pixels per DIP).
#[allow(clippy::too_many_arguments)]
pub fn panel(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, s: f32, t: &Theme) {
    c.shadow(x, y, w, h, r, 28.0 * s, 10.0 * s, t.shade.alpha(t.shade.a * 0.7));
    c.shadow(x, y, w, h, r, 3.0 * s, 1.0 * s, t.shade);
    c.fill_round_rect(x - 0.5 * s, y - 0.5 * s, w + s, h + s, r + 0.5 * s, t.ring);
    c.fill_round_rect(x, y, w, h, r, t.panel);
}

/// MacBridge's mark, `d` pixels square at (x, y): an accent rounded square with a white display.
pub fn mark(c: &mut Canvas, x: f32, y: f32, d: f32, accent: Rgba) {
    let k = d / 26.0;
    c.fill_round_rect_with(x, y, d, d, 6.5 * k, |_, py| accent.lerp(accent.shade(-0.18), ((py - y) / d).clamp(0.0, 1.0)));
    c.stroke_round_rect_with(x + 6.0 * k, y + 7.0 * k, 14.0 * k, 9.5 * k, 2.0 * k, 1.5 * k, |_, _| Rgba::WHITE);
    c.fill_capsule(x + d / 2.0 - 3.5 * k, y + 19.5 * k, x + d / 2.0 + 3.5 * k, y + 19.5 * k, 0.9 * k, Rgba::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_tint_runs_from_its_first_tint_to_its_last() {
        let t = theme(false);
        let mut c = Canvas::new(40, 30);
        tint(&mut c, &t);
        let at = |x: usize, y: usize| c.get(x, y);
        let near = |p: [u8; 4], col: Rgba| (p[2] as f32 / 255.0 - col.r).abs() < 0.03 && (p[1] as f32 / 255.0 - col.g).abs() < 0.03;
        assert!(near(at(0, 0), t.win[0]), "top left: {:?}", at(0, 0));
        assert!(near(at(39, 29), t.win[2]), "bottom right: {:?}", at(39, 29));
        assert!(at(20, 15)[3] == 255, "opaque");
    }

    #[test]
    fn a_panel_is_filled_inside_and_shaded_outside() {
        let t = theme(false);
        let mut c = Canvas::filled(80, 80, t.win[1]);
        panel(&mut c, 10.0, 10.0, 60.0, 50.0, 8.0, 1.0, &t);
        assert_eq!(c.get(40, 30), [255, 255, 255, 255], "the panel colour inside");
        let below = c.get(40, 66);
        assert!(below[0] < c.get(40, 78)[0] || below[1] < c.get(40, 78)[1], "a shadow under it: {below:?}");
    }
}
