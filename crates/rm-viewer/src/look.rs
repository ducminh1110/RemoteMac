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

/// The window buttons, as macOS draws them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Close,
    Minimize,
    Zoom,
}

pub const LIGHTS: [Light; 3] = [Light::Close, Light::Minimize, Light::Zoom];
/// a light's diameter, and from one light's centre to the next (DIPs)
pub const LIGHT_D: f32 = 12.0;
pub const LIGHT_STEP: f32 = 20.0;

/// The three window buttons, the first centred at (x, y): red, yellow, green; grey when the
/// window is not the active one (unless the pointer is over them), their marks shown while the
/// pointer is over the group (`hover`), `down` darker.
#[allow(clippy::too_many_arguments)]
pub fn traffic_lights(c: &mut Canvas, x: f32, y: f32, s: f32, active: bool, hover: bool, down: Option<Light>, maximized: bool, dark: bool) {
    let r = LIGHT_D * s / 2.0;
    for (i, l) in LIGHTS.iter().enumerate() {
        let cx = x + i as f32 * LIGHT_STEP * s;
        let (fill, rim, mark) = match l {
            Light::Close => (Rgba::rgb(0xff, 0x5f, 0x57), Rgba::rgb(0xe2, 0x46, 0x3f), Rgba::rgb(0x7e, 0x0a, 0x04)),
            Light::Minimize => (Rgba::rgb(0xfe, 0xbc, 0x2e), Rgba::rgb(0xe1, 0xa1, 0x16), Rgba::rgb(0x98, 0x57, 0x00)),
            Light::Zoom => (Rgba::rgb(0x28, 0xc8, 0x40), Rgba::rgb(0x14, 0xae, 0x2c), Rgba::rgb(0x00, 0x64, 0x00)),
        };
        let (fill, rim) = if active || hover {
            (fill, rim)
        } else if dark {
            (Rgba::rgb(0x46, 0x46, 0x4b), Rgba::rgb(0x52, 0x52, 0x57))
        } else {
            (Rgba::rgb(0xdc, 0xdc, 0xde), Rgba::rgb(0xcb, 0xcb, 0xcf))
        };
        let pressed = down == Some(*l);
        let (fill, rim) = if pressed { (fill.shade(-0.18), rim.shade(-0.18)) } else { (fill, rim) };
        c.fill_circle(cx, y, r, rim);
        c.fill_circle(cx, y, r - 0.5 * s.max(1.0), fill);
        if hover {
            let m = mark.alpha(0.78);
            let k = r * 0.42;
            let w = (0.85 * s).max(0.75);
            match l {
                Light::Close => {
                    c.fill_capsule(cx - k, y - k, cx + k, y + k, w, m);
                    c.fill_capsule(cx - k, y + k, cx + k, y - k, w, m);
                }
                Light::Minimize => c.fill_capsule(cx - k * 1.15, y, cx + k * 1.15, y, w, m),
                Light::Zoom => {
                    // two arrowheads in opposite corners (pointing in when it would restore)
                    let t = r * 0.46;
                    let leg = t * 1.35;
                    if maximized {
                        tri(c, cx - t * 0.12, y - t * 0.12, leg, -1.0, -1.0, m);
                        tri(c, cx + t * 0.12, y + t * 0.12, leg, 1.0, 1.0, m);
                    } else {
                        tri(c, cx - t, y - t, leg, 1.0, 1.0, m);
                        tri(c, cx + t, y + t, leg, -1.0, -1.0, m);
                    }
                }
            }
        }
    }
}

/// A right triangle: its right angle at (x, y), legs `leg` long towards (dx, dy).
fn tri(c: &mut Canvas, x: f32, y: f32, leg: f32, dx: f32, dy: f32, col: Rgba) {
    let (a, b, d) = ((x, y), (x + dx * leg, y), (x, y + dy * leg));
    let (x0, x1) = (a.0.min(b.0).floor().max(0.0) as usize, a.0.max(b.0).ceil().max(0.0) as usize);
    let (y0, y1) = (a.1.min(d.1).floor().max(0.0) as usize, a.1.max(d.1).ceil().max(0.0) as usize);
    for py in y0..=y1 {
        for px in x0..=x1 {
            // 4x4 samples of the triangle
            let mut n = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let (qx, qy) = (px as f32 + (sx as f32 + 0.5) / 4.0, py as f32 + (sy as f32 + 0.5) / 4.0);
                    if inside(qx, qy, a, b, d) {
                        n += 1;
                    }
                }
            }
            if n > 0 {
                c.blend(px, py, col, n as f32 / 16.0);
            }
        }
    }
}

fn inside(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let s = |p: (f32, f32), q: (f32, f32)| (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0);
    let (d1, d2, d3) = (s(a, b), s(b, c), s(c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

/// Which light is at (x, y) for lights starting at (x0, y0) (centre of the first), if any.
pub fn light_at(x: f32, y: f32, x0: f32, y0: f32, s: f32) -> Option<Light> {
    if (y - y0).abs() > LIGHT_STEP * s / 2.0 {
        return None;
    }
    let i = ((x - (x0 - LIGHT_STEP * s / 2.0)) / (LIGHT_STEP * s)).floor();
    (0.0..3.0).contains(&i).then(|| LIGHTS[i as usize])
}

/// Symbols in SF Symbols' manner (regular weight), `d` px tall, centred at (x, y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    /// "display": a screen on a stand
    Display,
    /// "gearshape"
    Gear,
    /// "magnifyingglass"
    Search,
    /// "xmark.circle.fill"
    Clear,
    /// "plus"
    Plus,
    /// "lock"
    Lock,
    /// "globe"
    Globe,
    /// "number"
    Number,
    /// "exclamationmark.circle.fill"
    Warning,
}

pub fn symbol(c: &mut Canvas, sym: Symbol, x: f32, y: f32, d: f32, col: Rgba) {
    let k = d / 16.0;
    let w = 1.45 * k;
    match sym {
        Symbol::Display => {
            c.stroke_round_rect_with(x - 8.0 * k, y - 6.5 * k, 16.0 * k, 10.5 * k, 2.2 * k, w, |_, _| col);
            c.fill_capsule(x, y + 4.0 * k, x, y + 6.6 * k, w * 0.55, col);
            c.fill_capsule(x - 3.4 * k, y + 7.0 * k, x + 3.4 * k, y + 7.0 * k, w * 0.55, col);
        }
        Symbol::Gear => {
            // "gearshape": eight rounded teeth on a ring, an open hub
            let teeth = 8;
            for i in 0..teeth {
                let a = (i as f32 + 0.5) / teeth as f32 * std::f32::consts::TAU;
                let (sn, cs) = a.sin_cos();
                let (t0, t1) = (5.6 * k, 7.3 * k);
                let (px, py) = (-sn, cs);
                let hw = 1.05 * k;
                c.fill_capsule(x + cs * t0 + px * hw * 0.35, y + sn * t0 + py * hw * 0.35, x + cs * t1, y + sn * t1, hw, col);
                c.fill_capsule(x + cs * t0 - px * hw * 0.35, y + sn * t0 - py * hw * 0.35, x + cs * t1, y + sn * t1, hw, col);
            }
            c.arc(x, y, 4.9 * k, 1.6 * k, 0.0, std::f32::consts::TAU, |_| col);
            c.arc(x, y, 1.9 * k, w * 0.9, 0.0, std::f32::consts::TAU, |_| col);
        }
        Symbol::Search => {
            let (cx, cy, r) = (x - 1.2 * k, y - 1.2 * k, 5.0 * k);
            c.arc(cx, cy, r, w, 0.0, std::f32::consts::TAU, |_| col);
            c.fill_capsule(cx + r * 0.74, cy + r * 0.74, x + 6.6 * k, y + 6.6 * k, w * 0.62, col);
        }
        Symbol::Clear => {
            c.fill_circle(x, y, 7.0 * k, col);
            let m = 2.6 * k;
            c.fill_capsule(x - m, y - m, x + m, y + m, 0.7 * k, Rgba::WHITE);
            c.fill_capsule(x - m, y + m, x + m, y - m, 0.7 * k, Rgba::WHITE);
        }
        Symbol::Plus => {
            c.fill_capsule(x - 6.5 * k, y, x + 6.5 * k, y, w * 0.55, col);
            c.fill_capsule(x, y - 6.5 * k, x, y + 6.5 * k, w * 0.55, col);
        }
        Symbol::Lock => {
            // the shackle: the top half of a ring (arc angles run clockwise from 12 o'clock)
            c.arc(x, y - 2.2 * k, 3.6 * k, w, 1.5 * std::f32::consts::PI, 2.5 * std::f32::consts::PI, |_| col);
            c.fill_capsule(x - 3.6 * k, y - 2.2 * k, x - 3.6 * k, y + 0.5 * k, w * 0.5, col);
            c.fill_capsule(x + 3.6 * k, y - 2.2 * k, x + 3.6 * k, y + 0.5 * k, w * 0.5, col);
            c.fill_round_rect(x - 6.0 * k, y + 0.2 * k, 12.0 * k, 8.4 * k, 2.0 * k, col);
        }
        Symbol::Globe => {
            let r = 7.0 * k;
            c.arc(x, y, r, w, 0.0, std::f32::consts::TAU, |_| col);
            c.fill_capsule(x - r, y, x + r, y, w * 0.45, col);
            c.fill_capsule(x, y - r, x, y + r, w * 0.45, col);
            // a meridian: an ellipse half as wide
            let n = 28;
            for i in 0..n {
                let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
                c.fill_capsule(x + a0.cos() * r * 0.45, y + a0.sin() * r, x + a1.cos() * r * 0.45, y + a1.sin() * r, w * 0.45, col);
            }
        }
        Symbol::Number => {
            let (h, v) = (5.2 * k, 6.4 * k);
            c.fill_capsule(x - 1.6 * k - 1.0 * k, y - v, x - 2.6 * k - 1.0 * k, y + v, w * 0.5, col);
            c.fill_capsule(x + 3.0 * k - 1.0 * k, y - v, x + 2.0 * k - 1.0 * k, y + v, w * 0.5, col);
            c.fill_capsule(x - h, y - 2.4 * k, x + h, y - 2.4 * k, w * 0.5, col);
            c.fill_capsule(x - h - 0.6 * k, y + 2.4 * k, x + h - 0.6 * k, y + 2.4 * k, w * 0.5, col);
        }
        Symbol::Warning => {
            c.fill_circle(x, y, 7.0 * k, col);
            c.fill_capsule(x, y - 3.8 * k, x, y + 0.8 * k, 0.95 * k, Rgba::WHITE);
            c.fill_circle(x, y + 3.6 * k, 1.05 * k, Rgba::WHITE);
        }
    }
}

/// A Mac laptop, `w` px wide, its screen centred at (x, y): an aluminium lid with a black
/// bezel and a bright wallpaper, the base under it.
pub fn mac_icon(c: &mut Canvas, x: f32, y: f32, w: f32, dark: bool) {
    let k = w / 100.0;
    let (sw, sh) = (78.0 * k, 52.0 * k);
    let (sx, sy) = (x - sw / 2.0, y - sh / 2.0);
    c.shadow(sx, sy, sw, sh + 6.0 * k, 5.0 * k, 10.0 * k, 4.0 * k, Rgba::BLACK.alpha(if dark { 0.5 } else { 0.18 }));
    // the lid and its bezel
    c.fill_round_rect(sx, sy, sw, sh, 5.0 * k, Rgba::rgb(0x2a, 0x2b, 0x30));
    let (bx, by, bw, bh) = (sx + 3.0 * k, sy + 3.0 * k, sw - 6.0 * k, sh - 6.5 * k);
    // the wallpaper: blue into violet, with two soft waves of light
    let (a, b, cc) = (Rgba::rgb(0x2b, 0x7b, 0xff), Rgba::rgb(0x7a, 0x4c, 0xff), Rgba::rgb(0xff, 0x8a, 0xc8));
    c.fill_round_rect_with(bx, by, bw, bh, 2.0 * k, |px, py| {
        let u = ((px - bx) / bw).clamp(0.0, 1.0);
        let v = ((py - by) / bh).clamp(0.0, 1.0);
        let wave = (-(((v - 0.62 + 0.18 * (u * 3.1).sin()) * 5.0).powi(2))).exp();
        let wave2 = (-(((v - 0.35 + 0.12 * (u * 4.0 + 1.0).cos()) * 7.0).powi(2))).exp();
        a.lerp(b, u * 0.8 + v * 0.2).lerp(cc, wave * 0.55).lerp(Rgba::WHITE, wave2 * 0.25)
    });
    // the base: a thin slab, wider than the lid, with the notch to open it
    let (fw, fh) = (96.0 * k, 5.0 * k);
    let fy = sy + sh;
    c.fill_round_rect_with(x - fw / 2.0, fy, fw, fh, fh / 2.0, |_, py| Rgba::rgb(0xe3, 0xe4, 0xe8).lerp(Rgba::rgb(0x9a, 0x9c, 0xa3), ((py - fy) / fh).clamp(0.0, 1.0)));
    c.fill_round_rect(x - 9.0 * k, fy, 18.0 * k, 1.8 * k, 0.9 * k, Rgba::rgb(0xb2, 0xb4, 0xba));
}

/// Apple's spinning progress indicator: twelve spokes, the lead one darkest, `t` seconds in.
pub fn spinner(c: &mut Canvas, cx: f32, cy: f32, d: f32, col: Rgba, t: f32) {
    let lead = (t * 12.0).floor() % 12.0;
    let (r0, r1, w) = (d * 0.25, d * 0.48, d * 0.045);
    for i in 0..12 {
        let a = i as f32 / 12.0 * std::f32::consts::TAU;
        let (sn, cs) = a.sin_cos();
        let age = (lead - i as f32).rem_euclid(12.0) / 12.0;
        c.fill_capsule(cx + sn * r0, cy - cs * r0, cx + sn * r1, cy - cs * r1, w, col.fade(1.0 - 0.78 * age));
    }
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
