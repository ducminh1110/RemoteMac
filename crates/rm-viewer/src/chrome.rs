//! Mac-style window chrome drawn by the viewer instead of the Windows caption: a title bar with
//! the three "traffic lights" (close, minimise, zoom) and the centred title, and under it a strip
//! with the Mac app's menu bar. Geometry, hit testing and the anti-aliased light sprites are pure
//! functions (tested on any OS); `ui` paints them with GDI.

/// Heights in DIPs (1 DIP = 1 Mac point).
pub const TITLE_H: f64 = 28.0;
pub const MENU_H: f64 = 24.0;
/// Light diameter, centre spacing and the first centre, as on macOS.
const LIGHT_D: f64 = 12.0;
const LIGHT_STEP: f64 = 20.0;
const LIGHT_X0: f64 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Close,
    Minimize,
    Zoom,
}

pub const LIGHTS: [Light; 3] = [Light::Close, Light::Minimize, Light::Zoom];

/// Pixel height of the chrome above the remote picture.
pub fn bar_height(scale: f64, has_menu: bool) -> i32 {
    ((TITLE_H + if has_menu { MENU_H } else { 0.0 }) * scale).round() as i32
}

pub fn title_height(scale: f64) -> i32 {
    (TITLE_H * scale).round() as i32
}

/// Diameter in pixels of one light.
pub fn light_size(scale: f64) -> i32 {
    (LIGHT_D * scale).round().max(6.0) as i32
}

/// Top-left pixel of a light's sprite.
pub fn light_origin(l: Light, scale: f64) -> (i32, i32) {
    let i = LIGHTS.iter().position(|x| *x == l).unwrap_or(0) as f64;
    let d = light_size(scale);
    let cx = ((LIGHT_X0 + i * LIGHT_STEP) * scale).round() as i32;
    let cy = (TITLE_H / 2.0 * scale).round() as i32;
    (cx - d / 2, cy - d / 2)
}

/// Which light is under (x, y) (client pixels); slightly generous like the Mac's.
pub fn hit_light(x: i32, y: i32, scale: f64) -> Option<Light> {
    let d = light_size(scale);
    LIGHTS.into_iter().find(|l| {
        let (ox, oy) = light_origin(*l, scale);
        let pad = (2.0 * scale) as i32;
        x >= ox - pad && x < ox + d + pad && y >= oy - pad && y < oy + d + pad
    })
}

/// Hovering anywhere over the group shows the glyphs on all three, as on the Mac.
pub fn over_lights(x: i32, y: i32, scale: f64) -> bool {
    let d = light_size(scale);
    let (x0, y0) = light_origin(Light::Close, scale);
    let (x1, _) = light_origin(Light::Zoom, scale);
    let pad = (4.0 * scale) as i32;
    x >= x0 - pad && x < x1 + d + pad && y >= y0 - pad && y < y0 + d + pad
}

/// Pixels left of the title / menus reserved by the lights.
pub fn lights_width(scale: f64) -> i32 {
    let (x1, _) = light_origin(Light::Zoom, scale);
    x1 + light_size(scale) + (12.0 * scale) as i32
}

pub type Rgb = (u8, u8, u8);

pub fn light_color(l: Light, active: bool) -> Rgb {
    if !active {
        return (206, 206, 206);
    }
    match l {
        Light::Close => (255, 95, 87),
        Light::Minimize => (254, 188, 46),
        Light::Zoom => (40, 200, 64),
    }
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round().clamp(0.0, 255.0) as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn seg_dist(px: f64, py: f64, (ax, ay): (f64, f64), (bx, by): (f64, f64)) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}

/// One light as a `d`x`d` BGRA sprite over background `bg`: anti-aliased disc with a slightly
/// darker rim, plus its glyph (x, -, +) when `glyph` is set.
pub fn light_sprite(d: i32, l: Light, active: bool, glyph: bool, bg: Rgb) -> Vec<u8> {
    let d = d.max(1) as usize;
    let base = light_color(l, active);
    let rim = mix(base, (0, 0, 0), 0.12);
    let ink = mix(base, (0, 0, 0), 0.55);
    let r = d as f64 / 2.0;
    let segs: Vec<((f64, f64), (f64, f64))> = match l {
        Light::Close => vec![((-0.3, -0.3), (0.3, 0.3)), ((-0.3, 0.3), (0.3, -0.3))],
        Light::Minimize => vec![((-0.36, 0.0), (0.36, 0.0))],
        Light::Zoom => vec![((-0.36, 0.0), (0.36, 0.0)), ((0.0, -0.36), (0.0, 0.36))],
    };
    let stroke = (d as f64 / 12.0).max(0.9) * 0.6;
    const S: usize = 4; // 4x4 supersampling
    let mut out = Vec::with_capacity(d * d * 4);
    for y in 0..d {
        for x in 0..d {
            let (mut cov_disc, mut cov_core, mut cov_ink) = (0.0, 0.0, 0.0);
            for sy in 0..S {
                for sx in 0..S {
                    let px = x as f64 + (sx as f64 + 0.5) / S as f64 - r;
                    let py = y as f64 + (sy as f64 + 0.5) / S as f64 - r;
                    let dist = (px * px + py * py).sqrt();
                    if dist <= r {
                        cov_disc += 1.0;
                        if dist <= r - (d as f64 / 16.0).max(0.6) {
                            cov_core += 1.0;
                        }
                        if glyph && active && segs.iter().any(|(a, b)| seg_dist(px / r, py / r, *a, *b) * r <= stroke) {
                            cov_ink += 1.0;
                        }
                    }
                }
            }
            let n = (S * S) as f64;
            // rim colour where the disc is not core, base inside, ink over it
            let disc = if cov_disc > 0.0 { mix(rim, base, cov_core / cov_disc) } else { base };
            let lit = mix(disc, ink, cov_ink / n);
            let px = mix(bg, lit, cov_disc / n);
            out.extend_from_slice(&[px.2, px.1, px.0, 255]);
        }
    }
    out
}

/// Title bar / menu strip colours (light appearance).
pub fn title_bg(active: bool) -> Rgb {
    if active { (232, 232, 232) } else { (246, 246, 246) }
}
pub const MENU_BG: Rgb = (246, 246, 246);
pub const HAIRLINE: Rgb = (208, 208, 208);
pub const MENU_HIGHLIGHT: Rgb = (10, 100, 220);
pub fn title_fg(active: bool) -> Rgb {
    if active { (77, 77, 77) } else { (172, 172, 172) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lights_are_where_the_mac_puts_them() {
        assert_eq!(light_origin(Light::Close, 1.0), (14, 8));
        assert_eq!(light_origin(Light::Zoom, 1.0), (54, 8));
        assert_eq!(light_origin(Light::Close, 2.0), (28, 16));
        assert_eq!(bar_height(1.0, true), 52);
        assert_eq!(bar_height(1.5, false), 42);
    }

    #[test]
    fn hit_testing() {
        assert_eq!(hit_light(20, 14, 1.0), Some(Light::Close));
        assert_eq!(hit_light(40, 14, 1.0), Some(Light::Minimize));
        assert_eq!(hit_light(60, 14, 1.0), Some(Light::Zoom));
        assert_eq!(hit_light(30, 14, 1.0), None);
        assert_eq!(hit_light(20, 40, 1.0), None);
        assert!(over_lights(30, 14, 1.0));
        assert!(!over_lights(120, 14, 1.0));
        assert_eq!(hit_light(40, 28, 2.0), Some(Light::Close));
    }

    #[test]
    fn sprites_are_antialiased_discs_with_glyphs() {
        let bg = title_bg(true);
        let s = light_sprite(12, Light::Close, true, false, bg);
        assert_eq!(s.len(), 12 * 12 * 4);
        let px = |s: &[u8], x: usize, y: usize| (s[(y * 12 + x) * 4 + 2], s[(y * 12 + x) * 4 + 1], s[(y * 12 + x) * 4]);
        assert_eq!(px(&s, 0, 0), bg); // corner is background
        assert_eq!(px(&s, 6, 6), light_color(Light::Close, true)); // centre is the light colour
        let edge = px(&s, 0, 6); // rim pixel is a blend, not a hard edge
        assert!(edge != bg && edge != light_color(Light::Close, true), "{edge:?}");
        let g = light_sprite(12, Light::Close, true, true, bg);
        assert!(px(&g, 6, 6).0 < 200, "glyph darkens the centre: {:?}", px(&g, 6, 6));
        let inactive = light_sprite(12, Light::Zoom, false, true, bg);
        assert_eq!(px(&inactive, 6, 6), (206, 206, 206)); // grey, no glyph, when the window is inactive
    }
}
