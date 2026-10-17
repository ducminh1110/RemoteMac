//! Taro's icon, drawn as an icon of macOS 26 is made: a squircle (Apple's continuous corners)
//! in a violet gradient lit from above, and on it a taro (khoai môn): its lilac corm with soft
//! rings and a shine, two green leaves above it. The same drawing at every size (the README's
//! picture, the app's icon).

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

/// Coverage of pixel (x, y) by the shape `inside` (a test on a point), 4x4 samples where
/// `near_edge` says the edge may pass.
fn coverage(x: usize, y: usize, inside: impl Fn(f32, f32) -> bool) -> f32 {
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let corners = [(px - 0.5, py - 0.5), (px + 0.5, py - 0.5), (px - 0.5, py + 0.5), (px + 0.5, py + 0.5)];
    let n_in = corners.iter().filter(|(a, b)| inside(*a, *b)).count();
    if n_in == 4 {
        return 1.0;
    }
    if n_in == 0 && !inside(px, py) {
        return 0.0;
    }
    let mut n = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            if inside(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0) {
                n += 1;
            }
        }
    }
    n as f32 / 16.0
}

/// The taro's corm: an egg fuller at the bottom, half-width `a` and half-height `b` around (cx, cy).
fn in_corm(x: f32, y: f32, cx: f32, cy: f32, a: f32, b: f32) -> bool {
    let v = (y - cy) / b;
    let half = a * (1.0 + 0.17 * v);
    let u = (x - cx) / half;
    u * u + v * v <= 1.0
}

/// A leaf from its base (bx, by) to its tip (tx, ty), `wide` across at its widest.
fn in_leaf(x: f32, y: f32, (bx, by): (f32, f32), (tx, ty): (f32, f32), wide: f32) -> bool {
    let (dx, dy) = (tx - bx, ty - by);
    let len = (dx * dx + dy * dy).sqrt();
    let (ux, uy) = (dx / len, dy / len);
    let along = (x - bx) * ux + (y - by) * uy;
    let across = -(x - bx) * uy + (y - by) * ux;
    if !(0.0..=len).contains(&along) {
        return false;
    }
    let t = along / len;
    // round at the base, drawn to a point at the tip (a taro leaf, simply)
    let half = wide / 2.0 * (std::f32::consts::PI * t).sin().powf(0.75) * (1.0 - 0.25 * t);
    across.abs() <= half
}

/// The icon, `n` pixels square (transparent around the squircle, as macOS icons are): a taro
/// (khoai môn, Taro's namesake) on a squircle of violet, lit from above.
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
    // the body: lavender at the top left into a deep violet, a pink glow low on the right
    let mut body = Canvas::new(n, n);
    let (top, mid, deep, glow) = (Rgba::rgb(0xc4, 0xa6, 0xff), Rgba::rgb(0x84, 0x4a, 0xf0), Rgba::rgb(0x4a, 0x1d, 0x96), Rgba::rgb(0xf0, 0x7a, 0xc8));
    for y in 0..n {
        for x in 0..n {
            let k = squircle(x, y, cx, cy, a);
            if k <= 0.0 {
                continue;
            }
            let (u, v) = ((x as f32 - (cx - a)) / (2.0 * a), (y as f32 - (cy - a)) / (2.0 * a));
            let t = (u * 0.35 + v * 0.65).clamp(0.0, 1.0);
            let base = if t < 0.5 { top.lerp(mid, t / 0.5) } else { mid.lerp(deep, (t - 0.5) / 0.5) };
            let g = (-(((u - 0.88).powi(2) + (v - 0.95).powi(2)) / 0.10)).exp();
            body.blend(x, y, base.lerp(glow, g * 0.45), k);
        }
    }
    // light from above: a soft sheen at the top
    let sheen = Rgba::WHITE.alpha(0.20);
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
    // the taro: its corm, lilac with rings, under two leaves
    let (tx, ty, ta, tb) = (512.0 * s, 618.0 * s, 236.0 * s, 236.0 * s);
    let deep_violet = Rgba::rgb(0x2e, 0x10, 0x65);
    // its shadow on the body
    let mut ts = Canvas::new(n, n);
    for y in 0..n {
        for x in 0..n {
            let k = coverage(x, y, |px, py| in_corm(px, py - 26.0 * s, tx, ty, ta, tb));
            if k > 0.0 {
                ts.blend(x, y, deep_violet, k);
            }
        }
    }
    ts.blur((28.0 * s).round().max(1.0) as usize, 3);
    let tm: Vec<u8> = ts.px.iter().map(|p| (p[3] as f32 * 0.42) as u8).collect();
    body.fill_mask(&tm, n, 0, 0, deep_violet);
    // the leaves behind the corm's top: one up and to the right, a smaller one to the left
    let leaf = |body: &mut Canvas, base: (f32, f32), tip: (f32, f32), wide: f32| {
        let (x0, x1) = (base.0.min(tip.0) - wide, base.0.max(tip.0) + wide);
        let (y0, y1) = (base.1.min(tip.1) - wide, base.1.max(tip.1) + wide);
        let (light, dark) = (Rgba::rgb(0x9b, 0xf0, 0x8c), Rgba::rgb(0x1f, 0x9d, 0x4f));
        for y in (y0.max(0.0) as usize)..(y1.min(n as f32) as usize) {
            for x in (x0.max(0.0) as usize)..(x1.min(n as f32) as usize) {
                let k = coverage(x, y, |px, py| in_leaf(px, py, base, tip, wide));
                if k > 0.0 {
                    let (dx, dy) = (tip.0 - base.0, tip.1 - base.1);
                    let t = (((x as f32 - base.0) * dx + (y as f32 - base.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                    body.blend(x, y, dark.lerp(light, t * 0.8), k);
                }
            }
        }
        // its vein
        let (vx, vy) = (base.0 + (tip.0 - base.0) * 0.82, base.1 + (tip.1 - base.1) * 0.82);
        body.fill_capsule(base.0, base.1, vx, vy, 5.0 * s, Rgba::WHITE.alpha(0.38));
    };
    body.fill_capsule(tx + 2.0 * s, ty - tb + 40.0 * s, tx + 24.0 * s, ty - tb - 60.0 * s, 13.0 * s, Rgba::rgb(0x2a, 0xa8, 0x5a));
    leaf(&mut body, (tx + 24.0 * s, ty - tb - 52.0 * s), (tx + 222.0 * s, ty - tb - 232.0 * s), 158.0 * s);
    leaf(&mut body, (tx + 4.0 * s, ty - tb - 36.0 * s), (tx - 158.0 * s, ty - tb - 172.0 * s), 112.0 * s);
    // the corm: lilac lit from the top left
    let (hi, lo) = (Rgba::rgb(0xf6, 0xee, 0xff), Rgba::rgb(0xc9, 0xa9, 0xf7));
    let mut corm = Canvas::new(n, n);
    let mut cov = vec![0f32; n * n];
    for y in ((ty - tb - 2.0) as usize).min(n)..((ty + tb + 2.0) as usize).min(n) {
        for x in ((tx - ta * 1.3) as usize).min(n)..((tx + ta * 1.3) as usize).min(n) {
            let k = coverage(x, y, |px, py| in_corm(px, py, tx, ty, ta, tb));
            if k > 0.0 {
                cov[y * n + x] = k;
                let (u, v) = ((x as f32 - tx) / ta, (y as f32 - ty) / tb);
                let t = ((u * 0.45 + v * 0.75) * 0.5 + 0.5).clamp(0.0, 1.0);
                corm.blend(x, y, hi.lerp(lo, t), k);
            }
        }
    }
    // its rings: soft violet bands around it, curving with its roundness (seen from a little
    // above)
    let ring = Rgba::rgb(0x8f, 0x5c, 0xe0);
    let mut rings = Canvas::new(n, n);
    for (i, f) in [-0.5f32, -0.12, 0.26, 0.62].iter().enumerate() {
        let r = 560.0 * s;
        let yc = ty + f * tb - r;
        rings.arc(tx, yc, r, (15.0 - i as f32 * 1.5) * s, std::f32::consts::PI - 0.5, std::f32::consts::PI + 0.5, |_| ring.alpha(0.5));
    }
    // a few specks, as on its skin
    for (u, v, r) in [(-0.45f32, 0.15f32, 9.0f32), (0.35, -0.2, 7.0), (0.15, 0.55, 8.0), (-0.2, 0.75, 6.0), (0.55, 0.45, 6.0), (-0.6, -0.35, 5.0)] {
        rings.fill_circle(tx + u * ta, ty + v * tb, r * s, ring.alpha(0.45));
    }
    let rm: Vec<u8> = rings.px.iter().zip(&cov).map(|(p, k)| (p[3] as f32 * k) as u8).collect();
    corm.fill_mask(&rm, n, 0, 0, ring);
    // the shine on its top left
    let mut shine = Canvas::new(n, n);
    for y in 0..n {
        for x in 0..n {
            let k = cov[y * n + x];
            if k <= 0.0 {
                continue;
            }
            let (du, dv) = ((x as f32 - (tx - ta * 0.42)) / (ta * 0.34), (y as f32 - (ty - tb * 0.52)) / (tb * 0.24));
            let g = (-(du * du + dv * dv) * 1.6).exp();
            if g > 0.01 {
                shine.blend(x, y, Rgba::WHITE, g * 0.75 * k);
            }
        }
    }
    corm.composite(&shine, 0, 0, 1.0);
    body.composite(&corm, 0, 0, 1.0);
    c.composite(&body, 0, 0, 1.0);
    c
}

/// The icon as straight-alpha RGBA (for a window's or a shortcut's icon).
pub fn rgba(n: usize) -> Vec<u8> {
    straight_rgba(&icon(n))
}

/// A canvas (premultiplied BGRA) as straight-alpha RGBA bytes (icons for Windows).
pub fn straight_rgba(c: &Canvas) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.px.len() * 4);
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
    fn the_icon_is_a_squircle_with_its_taro() {
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
