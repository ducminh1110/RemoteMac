//! MacBridge's icon, drawn as an icon of macOS 26 is made: a squircle (Apple's continuous
//! corners) in a deep blue gradient lit from above, and on it one window of Liquid Glass (the
//! material of glass.rs: what is behind it bends at its rim, light runs along its edge) with the
//! three window buttons, a bridge of light across it: Mac windows carried over to another screen.
//! The same drawing at every size (the README's picture, the app's icon).

use crate::glass::{self, Kind, Level, Material};
use crate::paint::{Canvas, Rgba};

/// Inside Apple's squircle (a superellipse of exponent 5) of half-size `a` around (cx, cy):
/// the coverage of pixel (x, y), 4x4 samples on the edge.
fn squircle(x: usize, y: usize, cx: f32, cy: f32, a: f32) -> f32 {
    let f = |px: f32, py: f32| ((px - cx).abs() / a).powf(5.0) + ((py - cy).abs() / a).powf(5.0);
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let v = f(px, py);
    if v < 0.9 {
        return 1.0;
    }
    if v > 1.1 {
        return 0.0;
    }
    let mut n = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            if f(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) <= 1.0 {
                n += 1;
            }
        }
    }
    n as f32 / 16.0
}

/// The icon, `n` pixels square (transparent around the squircle, as macOS icons are).
pub fn icon(n: usize) -> Canvas {
    let s = n as f32 / 1024.0;
    let (cx, cy, a) = (512.0 * s, 506.0 * s, 412.0 * s);
    let mut c = Canvas::new(n, n);
    // the shadow it casts
    let mut sh = Canvas::new(n, n);
    for y in 0..n {
        for x in 0..n {
            let k = squircle(x, y, cx, cy + 14.0 * s, a);
            if k > 0.0 {
                sh.blend(x, y, Rgba::BLACK, k);
            }
        }
    }
    sh.blur((22.0 * s).round().max(1.0) as usize, 3);
    let m: Vec<u8> = sh.px.iter().map(|p| (p[3] as f32 * 0.32) as u8).collect();
    c.fill_mask(&m, n, 0, 0, Rgba::BLACK);
    // the body: sky blue at the top left into a deep blue, a violet glow low on the right
    let mut body = Canvas::new(n, n);
    let (top, mid, deep, glow) = (Rgba::rgb(0x7a, 0xd8, 0xff), Rgba::rgb(0x2a, 0x7c, 0xff), Rgba::rgb(0x12, 0x3c, 0xd8), Rgba::rgb(0x9b, 0x5c, 0xff));
    for y in 0..n {
        for x in 0..n {
            let k = squircle(x, y, cx, cy, a);
            if k <= 0.0 {
                continue;
            }
            let (u, v) = ((x as f32 - (cx - a)) / (2.0 * a), (y as f32 - (cy - a)) / (2.0 * a));
            let t = (u * 0.35 + v * 0.65).clamp(0.0, 1.0);
            let base = if t < 0.5 { top.lerp(mid, t / 0.5) } else { mid.lerp(deep, (t - 0.5) / 0.5) };
            let g = (-(((u - 0.85).powi(2) + (v - 0.95).powi(2)) / 0.12)).exp();
            body.blend(x, y, base.lerp(glow, g * 0.55), k);
        }
    }
    // light from above: a soft sheen at the top, the rim a hair brighter there
    let sheen = Rgba::WHITE.alpha(0.22);
    let mut lit = Canvas::new(n, n);
    for y in 0..n {
        let v = (y as f32 - (cy - a)) / (2.0 * a);
        if v > 0.55 {
            break;
        }
        for x in 0..n {
            let k = squircle(x, y, cx, cy, a);
            if k > 0.0 {
                lit.blend(x, y, sheen, k * (1.0 - v / 0.55).powf(1.6));
            }
        }
    }
    body.composite(&lit, 0, 0, 1.0);
    // the window of glass, refracting the body behind it
    let (gw, gh) = ((560.0 * s).round() as usize, (420.0 * s).round() as usize);
    let (gx, gy) = ((cx - gw as f32 / 2.0).round() as isize, (cy - gh as f32 / 2.0 - 6.0 * s).round() as isize);
    let r = 86.0 * s;
    let mut m = Material::for_kind(Kind::Control, false, Rgba::rgb(0x0a, 0x7a, 0xff));
    m.tint_top = Rgba::WHITE.alpha(0.30);
    m.tint_bottom = Rgba::WHITE.alpha(0.12);
    m.rim_light = Rgba::WHITE.alpha(0.95);
    m.rim_lo = Rgba::WHITE.alpha(0.30);
    m.rim_dark = Rgba::CLEAR;
    m.params.refraction = 0.65;
    m.params.z_radius = 34.0;
    m.params.blur = 10.0;
    m.params.specular = 0.45;
    m.sheen = 1.0;
    let gs = s * 4.0; // the material's lengths are in DIPs: this icon's are larger
    let reach = glass::margin(&m, gs, Level::Full, gw, gh, r);
    let back = body.crop(gx - reach as isize, gy - reach as isize, gw + 2 * reach, gh + 2 * reach);
    let pane = glass::render(Some(&back), reach, gw, gh, r, &m, Level::Full, gs);
    body.shadow(gx as f32, gy as f32, gw as f32, gh as f32, r, 40.0 * s, 16.0 * s, Rgba::rgb(0x08, 0x1c, 0x70).alpha(0.45));
    body.composite(&pane, gx, gy, 1.0);
    // the window's three buttons
    let (bx, by, br) = (gx as f32 + 62.0 * s, gy as f32 + 58.0 * s, 17.0 * s);
    for (i, col) in [Rgba::rgb(0xff, 0x5f, 0x57), Rgba::rgb(0xfe, 0xbc, 0x2e), Rgba::rgb(0x28, 0xc8, 0x40)].iter().enumerate() {
        let x = bx + i as f32 * 52.0 * s;
        body.fill_circle(x, by + 2.0 * s, br, Rgba::BLACK.alpha(0.12));
        body.fill_circle(x, by, br, *col);
        body.fill_circle(x - br * 0.3, by - br * 0.35, br * 0.45, Rgba::WHITE.alpha(0.35));
    }
    // a bridge of light across the window: an arch and its deck
    let (ax, ay) = (cx, gy as f32 + gh as f32 * 0.80);
    let span = 190.0 * s;
    let thick = 30.0 * s;
    let white = Rgba::WHITE;
    // (the arch's round ends rest on the deck)
    let deck = ay - 3.0 * s;
    let (ay, top_y) = (deck - thick * 0.5, deck - thick * 0.5);
    body.arc(ax, ay + 6.0 * s, span, thick, 1.5 * std::f32::consts::PI, 2.5 * std::f32::consts::PI, |_| Rgba::rgb(0x08, 0x1c, 0x70).alpha(0.16));
    body.arc(ax, ay, span, thick, 1.5 * std::f32::consts::PI, 2.5 * std::f32::consts::PI, |_| white);
    for k in [-0.62f32, -0.25, 0.25, 0.62] {
        let x = ax + k * span;
        let top = top_y - (span * span - (k * span).powi(2)).sqrt();
        body.fill_capsule(x, top + thick * 0.5, x, deck, 5.0 * s, white.alpha(0.85));
    }
    body.fill_capsule(ax - span - 46.0 * s, deck + 5.0 * s, ax + span + 46.0 * s, deck + 5.0 * s, thick * 0.42, Rgba::rgb(0x08, 0x1c, 0x70).alpha(0.14));
    body.fill_capsule(ax - span - 46.0 * s, deck, ax + span + 46.0 * s, deck, thick * 0.42, white);
    c.composite(&body, 0, 0, 1.0);
    c
}

/// The icon as straight-alpha RGBA (for a window's or a shortcut's icon).
pub fn rgba(n: usize) -> Vec<u8> {
    let c = icon(n);
    let mut out = Vec::with_capacity(n * n * 4);
    for p in &c.px {
        let a = p[3] as f32 / 255.0;
        let un = |v: u8| if a > 0.0 { (v as f32 / a).min(255.0) as u8 } else { 0 };
        out.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), p[3]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_is_a_squircle_with_its_window() {
        let c = icon(128);
        assert_eq!(c.get(2, 2)[3], 0, "clear outside");
        assert_eq!(c.get(64, 64)[3], 255, "opaque inside");
        // Apple's corner is fuller than a circle's but not square
        let corner = squircle(13, 13, 64.0, 63.25, 51.5);
        assert!(corner < 0.5, "{corner}");
    }

    /// RM_PREVIEW=dir cargo test -p rm-viewer logo: the icon at 1024 and 256 pixels.
    #[test]
    fn pictures() {
        let Some(dir) = std::env::var_os("RM_PREVIEW") else { return };
        for n in [1024usize, 256] {
            let c = icon(n);
            // PAM keeps the alpha (ImageMagick turns it into a PNG)
            let mut pam = format!("P7\nWIDTH {n}\nHEIGHT {n}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n").into_bytes();
            for p in &c.px {
                let a = p[3] as f32 / 255.0;
                let un = |v: u8| if a > 0.0 { ((v as f32 / a).min(255.0)) as u8 } else { 0 };
                pam.extend_from_slice(&[un(p[2]), un(p[1]), un(p[0]), p[3]]);
            }
            std::fs::write(std::path::Path::new(&dir).join(format!("logo-{n}.pam")), pam).unwrap();
        }
    }
}
