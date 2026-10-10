//! Liquid Glass: the material of MacBridge's floating surfaces (menus, the search palette, the
//! reconnect banner, the Dock, the loading window's bar). A Rust port of MobileLab's glass
//! (github.com/ducminh1110/MobileLab, MIT, Copyright (c) 2026 Minh; see NOTICE.md), without Qt:
//!
//! - a bevel height field on a rounded-rectangle signed distance field;
//! - dual-surface refraction of what is behind, with a little chromatic aberration at the rim;
//! - frosting (blur and saturation), a tint, a top sheen, Fresnel and Blinn-Phong light;
//! - one fading 1 px rim and a two-part soft shadow.
//!
//! Everything that depends only on the shape is computed once per (size, radius, parameters) in
//! a [`ShapeTable`]; drawing a surface then samples the backdrop through it. The backdrop is a
//! still capture of what is behind the surface, taken when it appears (surface.rs): glass never
//! re-renders continuously, and never over a live Mac picture.
//!
//! Levels: Full (everything), Blur (frosted, no refraction), Off (solid: the "Reduce
//! transparency" look, also the fallback when no backdrop can be captured).

use crate::paint::{sdf_round_rect, Canvas, Rgba};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Off,
    Blur,
    Full,
}

impl Level {
    /// Settings > Glass (0 full, 1 frosted, 2 off); RM_REDUCE_TRANSPARENCY=1 forces Off.
    pub fn from_setting(v: u8) -> Level {
        if std::env::var_os("RM_REDUCE_TRANSPARENCY").is_some_and(|v| v != "0") {
            return Level::Off;
        }
        match v {
            0 => Level::Full,
            1 => Level::Blur,
            _ => Level::Off,
        }
    }
}

/// What a surface is, for its look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// buttons and small groups
    Control,
    /// the selected thing (accent colour)
    Accent,
    /// a text field (the search box)
    Field,
    /// menus, the palette, banners, dialogs
    Sheet,
    /// a tab bar or the Dock
    Tabs,
    /// almost clear, rim only
    Quiet,
}

/// The physical model. Lengths are device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub refraction: f32,
    pub chroma: f32,
    pub edge_highlight: f32,
    pub specular: f32,
    pub fresnel: f32,
    pub z_radius: f32,
    pub ior: f32,
    pub max_offset: f32,
    pub scale: f32,
    pub blur: f32,
    pub edge_sharp: f32,
    pub saturate: f32,
}

impl Default for Params {
    fn default() -> Self {
        Params { refraction: 0.45, chroma: 0.05, edge_highlight: 0.30, specular: 0.40, fresnel: 1.0, z_radius: 14.0, ior: 1.5, max_offset: 0.0, scale: 1.0, blur: 5.0, edge_sharp: 0.6, saturate: 1.4 }
    }
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    if e0 == e1 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Outward unit normal of the rounded rectangle (0, 0) at (x, y), (0, 0) on its medial axis.
pub fn sdf_normal(x: f32, y: f32, w: f32, h: f32, r: f32) -> (f32, f32) {
    let r = r.clamp(0.0, w.min(h) * 0.5);
    let (px, py) = (x - w * 0.5, y - h * 0.5);
    let (sx, sy) = (if px < 0.0 { -1.0 } else { 1.0 }, if py < 0.0 { -1.0 } else { 1.0 });
    let (qx, qy) = (px.abs() - (w * 0.5 - r), py.abs() - (h * 0.5 - r));
    if qx > 0.0 && qy > 0.0 {
        let l = (qx * qx + qy * qy).sqrt();
        return (qx / l * sx, qy / l * sy);
    }
    if qx > qy {
        (sx, 0.0)
    } else if qy > qx {
        (0.0, sy)
    } else {
        (0.0, 0.0)
    }
}

/// Bevel height `depth` inside the edge: a quarter circle of radius `z`.
pub fn bevel_height(d: f32, z: f32) -> f32 {
    if d <= 0.0 {
        0.0
    } else if d >= z {
        z
    } else {
        (d * (2.0 * z - d)).sqrt()
    }
}

/// Its slope (the edge itself, infinitely steep, is taken half a pixel in).
pub fn bevel_slope(d: f32, z: f32) -> f32 {
    if d >= z || z <= 0.0 {
        return 0.0;
    }
    let d = d.max(0.5);
    (z - d) / (d * (2.0 * z - d)).sqrt()
}

/// Per pixel, shape only.
#[derive(Debug, Clone, Copy, Default)]
pub struct Texel {
    /// refraction offset, 1/16 px, into the shape
    pub rx: i16,
    pub ry: i16,
    /// chromatic aberration (red +c, blue -c), 1/16 px
    pub cx: i16,
    pub cy: i16,
    /// 0 deep inside .. 255 at the edge
    pub edge: u8,
    /// additive white light
    pub add: u8,
    /// Fresnel mix towards white (x 0.2)
    pub wmix: u8,
    pub depth: u8,
    pub mask: u8,
}

pub struct ShapeTable {
    pub w: usize,
    pub h: usize,
    pub radius: f32,
    pub z: f32,
    pub params: Params,
    /// largest displacement of any channel sample (sizes the backdrop margin)
    pub reach: f32,
    pub t: Vec<Texel>,
}

fn q16(v: f32) -> i16 {
    (v.clamp(-2000.0, 2000.0) * 16.0).round() as i16
}

fn q8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn norm3(x: f32, y: f32, z: f32) -> [f32; 3] {
    let l = (x * x + y * y + z * z).sqrt();
    [x / l, y / l, z / l]
}

impl ShapeTable {
    pub fn new(w: usize, h: usize, radius: f32, p: &Params) -> ShapeTable {
        let (w, h) = (w.max(1), h.max(1));
        let (fw, fh) = (w as f32, h as f32);
        let r = radius.clamp(0.0, fw.min(fh) * 0.5);
        let (half_x, half_y) = (fw * 0.5, fh * 0.5);
        let max_d = half_x.min(half_y);
        let z = p.z_radius.min(max_d).max(1.0);
        let k = 1.0 - 1.0 / p.ior.max(1.01);
        let cap_px = if p.max_offset > 0.0 { p.max_offset } else { z };
        let s = p.scale.max(0.5);
        let half = |l: [f32; 3]| norm3(l[0], l[1], l[2] + 1.0);
        let (l1, l2, l3, l4) = (norm3(0.4, 0.7, 1.0), norm3(-0.3, -0.5, 1.0), norm3(0.1, 0.3, 1.0), norm3(0.0, 0.9, 0.4));
        let (h1, h2, h4) = (half(l1), half(l2), half(l4));
        let mut t = vec![Texel::default(); w * h];
        let mut reach = 0f32;
        for y in 0..h {
            for x in 0..w {
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let sdf = sdf_round_rect(cx, cy, 0.0, 0.0, fw, fh, r);
                let tx = &mut t[y * w + x];
                tx.mask = q8(1.0 - smooth(-1.0, 1.0, sdf));
                if sdf > 1.0 {
                    continue;
                }
                let inside = (-sdf).max(0.0);
                let d = inside.max(0.5);
                let (nx, ny) = sdf_normal(cx, cy, fw, fh, r);
                let slope = bevel_slope(d, z);
                let hc = bevel_height(d, z);
                let (gx, gy) = (-nx * slope, -ny * slope);
                let thick = (hc * 2.0) / (z * 2.0).max(1.0);
                // dual surface refraction, then a slight pull towards the centre on the bevel
                let mut rx = (gx * k * 2.0 + gx * k * thick * 0.5) * p.refraction * 30.0 * s;
                let mut ry = (gy * k * 2.0 + gy * k * thick * 0.5) * p.refraction * 30.0 * s;
                let (px, py) = (cx - half_x, cy - half_y);
                let pull = smooth(0.0, z, inside) * (1.0 - smooth(z, 2.0 * z, inside));
                rx += (-px / half_x.max(1.0)) * p.refraction * 4.0 * s * pull;
                ry += (-py / half_y.max(1.0)) * p.refraction * 4.0 * s * pull;
                let mag = (rx * rx + ry * ry).sqrt();
                if mag > 1e-4 {
                    let cap = cap_px * (mag / cap_px).tanh();
                    rx *= cap / mag;
                    ry *= cap / mag;
                }
                let edge = 1.0 - smooth(0.0, (max_d * 0.35).min(z * 1.3), inside);
                let inv_n = 1.0 / (1.0 + slope * slope).sqrt();
                let (n_x, n_y, n_z) = (-gx * inv_n, -gy * inv_n, inv_n);
                let ca = p.chroma * 18.0 * (edge * 0.7 + 0.3) * 2.0 * s;
                let (cxo, cyo) = (n_x * ca, n_y * ca);
                tx.rx = q16(rx);
                tx.ry = q16(ry);
                tx.cx = q16(cxo);
                tx.cy = q16(cyo);
                tx.edge = q8(edge);
                let qm = ((tx.rx as f32 / 16.0).powi(2) + (tx.ry as f32 / 16.0).powi(2)).sqrt();
                reach = reach.max(qm + (cxo * cxo + cyo * cyo).sqrt());
                tx.depth = q8(smooth(0.0, z, inside));
                // light: specular lobes on the bevel, a top-biased inner glow, rim, environment
                let fres = (1.0 - n_z.abs()).powi(4) * p.fresnel;
                let ny_up = -n_y;
                let dot = |v: [f32; 3]| n_x * v[0] + ny_up * v[1] + n_z * v[2];
                let sp1 = dot(h1).max(0.0).powf(22.0);
                let sp2 = dot(h2).max(0.0).powf(18.0) * 0.3;
                let sp_b = dot(l3).max(0.0).powf(6.0) * 0.1;
                let sp4 = dot(h4).max(0.0).powf(28.0) * 0.5;
                let top_bias = 0.5 + 0.5 * (-py / half_y.max(1.0));
                let on_bevel = smooth(0.03, 0.4, 1.0 - n_z);
                let spec = (sp1 + sp2 + sp_b * 0.3 + sp4) * p.specular * on_bevel;
                let env = (ny_up * 0.5 + 0.5) * fres * 0.08;
                let g = 1.0 - smooth(0.0, 6.0 * s, inside);
                let glow = g * g * p.edge_highlight * 0.20 * (0.35 + 0.65 * top_bias);
                let rim = edge * p.edge_highlight * 0.06;
                tx.add = q8(spec + glow + rim + env);
                tx.wmix = q8(fres);
            }
        }
        ShapeTable { w, h, radius: r, z, params: *p, reach, t }
    }

    pub fn at(&self, x: usize, y: usize) -> &Texel {
        &self.t[y * self.w + x]
    }

    /// Shared tables (a menu's rows all have one size): the last few made are kept.
    pub fn cached(w: usize, h: usize, radius: f32, p: &Params) -> Arc<ShapeTable> {
        type Key = (usize, usize, u32, [u32; 9]);
        static CACHE: Mutex<Vec<(Key, Arc<ShapeTable>)>> = Mutex::new(Vec::new());
        let shape = [p.refraction, p.chroma, p.edge_highlight, p.specular, p.fresnel, p.z_radius, p.ior, p.max_offset, p.scale].map(f32::to_bits);
        let key = (w, h, radius.to_bits(), shape);
        let mut c = CACHE.lock().unwrap();
        if let Some(i) = c.iter().position(|(k, _)| *k == key) {
            let e = c.remove(i);
            c.insert(0, e);
            return c[0].1.clone();
        }
        let t = Arc::new(ShapeTable::new(w, h, radius, p));
        c.insert(0, (key, t.clone()));
        c.truncate(12);
        t
    }
}

/// The look of one kind of surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    pub params: Params,
    pub tint_top: Rgba,
    pub tint_bottom: Rgba,
    pub rim_light: Rgba,
    pub rim_lo: Rgba,
    pub rim_dark: Rgba,
    /// the solid colour of the Off level (and when there is no backdrop)
    pub opaque: Rgba,
    pub sheen: f32,
    pub blur_tint_boost: f32,
    pub light_scale: f32,
    pub shadow_alpha: f32,
    pub shadow_spread: f32,
    pub shadow_offset_y: f32,
}

impl Material {
    pub fn for_kind(k: Kind, dark: bool, accent: Rgba) -> Material {
        let w = |a: u8| Rgba::rgba(255, 255, 255, a);
        let mut m = Material {
            params: Params { refraction: 0.42, chroma: 0.04, edge_highlight: 0.30, specular: 0.20, fresnel: 0.8, z_radius: 13.0, blur: 6.0, edge_sharp: 0.65, saturate: 1.5, ..Params::default() },
            tint_top: if dark { Rgba::rgba(66, 72, 88, 128) } else { w(128) },
            tint_bottom: if dark { Rgba::rgba(24, 27, 36, 122) } else { w(77) },
            rim_light: if dark { w(150) } else { w(235) },
            rim_lo: if dark { w(22) } else { w(60) },
            rim_dark: if dark { Rgba::CLEAR } else { Rgba::rgba(30, 50, 80, 20) },
            opaque: if dark { Rgba::rgb(44, 44, 49) } else { Rgba::rgb(246, 246, 248) },
            sheen: if dark { 0.6 } else { 0.9 },
            blur_tint_boost: 1.28,
            light_scale: if dark { 1.0 } else { 0.85 },
            shadow_alpha: if dark { 0.75 } else { 0.32 },
            shadow_spread: 9.0,
            shadow_offset_y: 3.0,
        };
        let p = &mut m.params;
        match k {
            Kind::Control => {}
            Kind::Accent => {
                m.tint_top = accent.shade(0.18).alpha(0.96);
                m.tint_bottom = accent.shade(-0.06).alpha(0.94);
                m.rim_light = w(if dark { 120 } else { 190 });
                m.rim_lo = w(if dark { 14 } else { 30 });
                m.rim_dark = Rgba::CLEAR;
                (p.refraction, p.chroma, p.specular, p.edge_highlight, p.z_radius, p.blur) = (0.10, 0.0, 0.22, 0.16, 12.0, 3.0);
                m.sheen = 1.0;
                m.opaque = accent;
                m.shadow_alpha = 0.0;
            }
            Kind::Tabs => {
                m.tint_top = if dark { w(30) } else { Rgba::rgba(226, 232, 240, 150) };
                m.tint_bottom = if dark { w(18) } else { Rgba::rgba(214, 222, 232, 130) };
                m.rim_light = if dark { w(90) } else { w(210) };
                m.rim_lo = if dark { w(14) } else { w(45) };
                m.rim_dark = if dark { Rgba::CLEAR } else { Rgba::rgba(30, 50, 80, 14) };
                m.light_scale = if dark { 0.7 } else { 0.55 };
                (p.refraction, p.chroma, p.specular, p.edge_highlight, p.z_radius, p.blur, p.saturate) = (0.22, 0.0, 0.12, 0.20, 10.0, 6.0, 1.3);
                m.sheen = 0.7;
                m.shadow_alpha = 0.0;
            }
            Kind::Field => {
                m.tint_top = if dark { w(26) } else { Rgba::rgba(238, 241, 245, 190) };
                m.tint_bottom = if dark { w(16) } else { Rgba::rgba(230, 234, 240, 170) };
                m.rim_light = if dark { w(70) } else { w(170) };
                m.rim_lo = if dark { w(12) } else { w(40) };
                m.rim_dark = if dark { Rgba::CLEAR } else { Rgba::rgba(30, 50, 80, 22) };
                m.light_scale = if dark { 0.5 } else { 0.4 };
                (p.refraction, p.chroma, p.specular, p.edge_highlight, p.z_radius, p.blur, p.saturate) = (0.16, 0.0, 0.08, 0.14, 9.0, 6.0, 1.25);
                m.sheen = 0.4;
                m.shadow_alpha = 0.0;
            }
            Kind::Quiet => {
                m.tint_top = if dark { w(22) } else { w(112) };
                m.tint_bottom = if dark { w(10) } else { Rgba::rgba(240, 244, 249, 72) };
                m.rim_light = if dark { w(84) } else { w(220) };
                m.rim_lo = if dark { w(14) } else { w(50) };
                m.rim_dark = if dark { Rgba::CLEAR } else { Rgba::rgba(30, 50, 80, 16) };
                m.light_scale = if dark { 0.5 } else { 0.6 };
                (p.refraction, p.chroma, p.specular, p.edge_highlight, p.z_radius, p.blur, p.saturate) = (0.30, 0.02, 0.16, 0.22, 10.0, 5.0, 1.3);
                m.sheen = 0.6;
                m.blur_tint_boost = 1.15;
                m.shadow_alpha = 0.0;
            }
            Kind::Sheet => {
                m.tint_top = if dark { Rgba::rgba(56, 60, 72, 226) } else { Rgba::rgba(252, 252, 254, 232) };
                m.tint_bottom = if dark { Rgba::rgba(38, 41, 50, 220) } else { Rgba::rgba(244, 246, 250, 222) };
                m.rim_light = if dark { w(110) } else { w(220) };
                m.rim_lo = if dark { w(20) } else { w(55) };
                m.rim_dark = if dark { Rgba::CLEAR } else { Rgba::rgba(30, 50, 80, 26) };
                (p.refraction, p.chroma, p.specular, p.edge_highlight, p.z_radius, p.blur, p.edge_sharp) = (0.08, 0.0, 0.05, 0.12, 16.0, 9.0, 0.15);
                m.sheen = 0.5;
                m.blur_tint_boost = 1.0;
            }
        }
        m
    }

    /// The look while pressed: the bevel and the light flatten.
    pub fn pressed(&self) -> Material {
        let mut m = *self;
        m.params.z_radius = (self.params.z_radius * 0.45).max(3.0);
        m.params.specular *= 0.4;
        m.params.refraction *= 0.6;
        m.light_scale *= 0.7;
        m.sheen *= 0.5;
        let darken = |c: Rgba| Rgba { r: c.r * 0.92, g: c.g * 0.92, b: c.b * 0.92, a: (c.a + 0.06).min(1.0) };
        m.tint_top = darken(self.tint_top);
        m.tint_bottom = darken(self.tint_bottom);
        m
    }

    /// The model in device pixels at `scale` (display scale).
    pub fn device_params(&self, scale: f32) -> Params {
        let mut p = self.params;
        p.scale = scale;
        p.z_radius *= scale;
        p.max_offset *= scale;
        p.blur = (p.blur * scale).round().max(1.0);
        p
    }
}

/// How much backdrop (device px) is needed around a w x h surface on each side.
pub fn margin(m: &Material, scale: f32, level: Level, w: usize, h: usize, radius: f32) -> usize {
    let p = m.device_params(scale);
    let t = ShapeTable::cached(w, h, radius, &p);
    let reach = if level == Level::Full { t.reach } else { 0.0 };
    reach.ceil() as usize + 2 * p.blur as usize + 3
}

/// The material of a w x h (device px) surface of corner `radius`, from `backdrop`: what is
/// behind it, larger by `origin` px on each side (None: no backdrop, the solid fallback).
#[allow(clippy::too_many_arguments)]
pub fn render(backdrop: Option<&Canvas>, origin: usize, w: usize, h: usize, radius: f32, m: &Material, level: Level, scale: f32) -> Canvas {
    let p = m.device_params(scale);
    let t = ShapeTable::cached(w, h, radius, &p);
    let mut out;
    match backdrop.filter(|_| level != Level::Off) {
        None => {
            out = Canvas::filled(w, h, m.opaque);
        }
        Some(b) => {
            let mut soft = b.clone();
            soft.blur(p.blur as usize, 2);
            soft.saturate(p.saturate);
            let sharp = (level == Level::Full).then(|| {
                let mut s = b.clone();
                s.saturate(p.saturate);
                s
            });
            out = sample_through(&soft, sharp.as_ref(), origin, &t, level == Level::Full, p.edge_sharp);
            let boost = if level == Level::Blur { m.blur_tint_boost } else { 1.0 };
            let (top, bottom) = (m.tint_top.fade(boost), m.tint_bottom.fade(boost));
            let hh = h as f32;
            out.fill_round_rect_with(0.0, 0.0, w as f32, hh, 0.0, |_, y| top.lerp(bottom, y / hh));
        }
    }
    let rad = t.radius;
    let (fw, fh) = (w as f32, h as f32);
    if level != Level::Off && m.sheen > 0.0 && backdrop.is_some() {
        // a faint highlight hugging the top edge: a soft radial at the top left and a top fade
        let sa = m.sheen.min(1.0);
        let (gx, gy, gr) = (fw * 0.18, -fh * 0.2, (fw * 0.55).max(fh * 0.9));
        out.fill_round_rect_with(0.0, 0.0, fw, fh, rad, |x, y| {
            let d = (((x - gx).powi(2) + (y - gy).powi(2)).sqrt() / gr).min(1.0);
            let top = (1.0 - y / (fh * 0.4)).max(0.0);
            Rgba::WHITE.alpha(0.30 * sa * (1.0 - d) + 0.14 * sa * top)
        });
    }
    // light (from the table) and the shape's mask
    let light = level != Level::Off && backdrop.is_some();
    for (px, tx) in out.px.iter_mut().zip(&t.t) {
        if tx.mask == 0 {
            *px = [0; 4];
            continue;
        }
        if light && (tx.add > 0 || tx.wmix > 0) {
            let add = tx.add as f32 / 255.0 * m.light_scale * 255.0;
            let wm = tx.wmix as f32 / 255.0 * 0.2 * m.light_scale;
            for c in px.iter_mut().take(3) {
                let v = *c as f32;
                *c = (v + add + (255.0 - v) * wm).min(255.0) as u8;
            }
        }
        if tx.mask != 255 {
            for c in px.iter_mut() {
                *c = ((*c as u32 * tx.mask as u32 + 127) / 255) as u8;
            }
        }
    }
    // one 1 px border: the hairline under, the rim light over it (bright top left, dim elsewhere)
    let bw = scale.round().max(1.0);
    if level == Level::Off || backdrop.is_none() {
        let c = m.rim_dark.alpha(m.rim_dark.a.max(0.18));
        out.stroke_round_rect_with(0.0, 0.0, fw, fh, rad, bw, |_, _| c);
    } else {
        if m.rim_dark.a > 0.0 {
            out.stroke_round_rect_with(0.0, 0.0, fw, fh, rad, bw, |_, _| m.rim_dark);
        }
        let (rl, rlo) = (m.rim_light, m.rim_lo);
        let len2 = (fw * 0.55).powi(2) + fh.powi(2);
        out.stroke_round_rect_with(0.0, 0.0, fw, fh, rad, bw, |x, y| {
            let t = ((x * fw * 0.55 + y * fh) / len2).clamp(0.0, 1.0);
            if t < 0.42 {
                rl.lerp(rlo, t / 0.42)
            } else if t < 0.68 {
                rlo
            } else {
                rlo.lerp(rl.fade(0.5), (t - 0.68) / 0.32)
            }
        });
    }
    out
}

/// The backdrop seen through the shape (refracted at the rim when `displace`).
fn sample_through(soft: &Canvas, sharp: Option<&Canvas>, origin: usize, t: &ShapeTable, displace: bool, edge_sharp: f32) -> Canvas {
    let mut out = Canvas::new(t.w, t.h);
    for y in 0..t.h {
        for x in 0..t.w {
            let tx = t.at(x, y);
            let (bx, by) = ((origin + x) as f32, (origin + y) as f32);
            let gain = 1.0 + 0.06 * (tx.depth as f32 / 255.0);
            let o = &mut out.px[y * t.w + x];
            if !displace || (tx.rx == 0 && tx.ry == 0 && tx.cx == 0 && tx.cy == 0) {
                let c = soft.sample(bx, by);
                *o = [cl(c[0] * gain), cl(c[1] * gain), cl(c[2] * gain), 255];
                continue;
            }
            let (fx, fy) = (bx + tx.rx as f32 / 16.0, by + tx.ry as f32 / 16.0);
            let (cx, cy) = (tx.cx as f32 / 16.0, tx.cy as f32 / 16.0);
            let edge_mix = 1.0 - edge_sharp * (tx.edge as f32 / 255.0);
            // BGRA: blue samples -c, green on the ray, red +c
            let mut rgb = [0f32; 3];
            for (i, sgn) in [(0usize, -1.0f32), (1, 0.0), (2, 1.0)] {
                let (sx, sy) = (fx + cx * sgn, fy + cy * sgn);
                let mut v = soft.sample(sx, sy)[i];
                if let Some(s) = sharp.filter(|_| edge_mix < 0.999) {
                    v = s.sample(sx, sy)[i] * (1.0 - edge_mix) + v * edge_mix;
                }
                rgb[i] = v;
            }
            *o = [cl(rgb[0] * gain), cl(rgb[1] * gain), cl(rgb[2] * gain), 255];
        }
    }
    out
}

fn cl(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripes(w: usize, h: usize) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h {
            for x in 0..w {
                c.px[y * w + x] = if (x / 6) % 2 == 0 { [40, 40, 220, 255] } else { [230, 230, 230, 255] };
            }
        }
        c
    }

    #[test]
    fn the_bevel_and_normals() {
        assert_eq!(bevel_height(0.0, 10.0), 0.0);
        assert_eq!(bevel_height(20.0, 10.0), 10.0);
        assert!((bevel_height(10.0, 10.0) - 10.0).abs() < 1e-4);
        assert!(bevel_slope(1.0, 10.0) > bevel_slope(5.0, 10.0) && bevel_slope(10.0, 10.0) == 0.0);
        assert_eq!(sdf_normal(0.5, 50.0, 100.0, 100.0, 10.0), (-1.0, 0.0), "left edge points left");
        let (x, y) = sdf_normal(1.0, 1.0, 100.0, 100.0, 20.0);
        assert!(x < -0.6 && y < -0.6, "a corner points out diagonally");
    }

    #[test]
    fn the_table_refracts_at_the_rim_and_leaves_the_middle_calm() {
        let t = ShapeTable::new(120, 60, 16.0, &Params::default());
        let rim = t.at(1, 30);
        let mid = t.at(60, 30);
        assert!(rim.rx > 0, "at the left rim the backdrop is pulled in from the inside: {rim:?}");
        assert_eq!((mid.rx, mid.ry), (0, 0), "{mid:?}");
        assert!(rim.edge > 200 && mid.edge < 10);
        assert_eq!(t.at(0, 0).mask, 0, "the corner is cut");
        assert_eq!(mid.mask, 255);
        assert!(t.reach > 1.0 && t.reach <= t.z * 1.2 + 4.0, "{} {}", t.reach, t.z);
        assert!(t.at(60, 1).add > t.at(60, 58).add, "lit from the top");
    }

    #[test]
    fn levels_render_what_they_promise() {
        let m = Material::for_kind(Kind::Sheet, false, Rgba::rgb(10, 132, 255));
        let (w, h, r) = (80, 40, 12.0);
        let o = margin(&m, 1.0, Level::Full, w, h, r);
        let back = stripes(w + 2 * o, h + 2 * o);
        let full = render(Some(&back), o, w, h, r, &m, Level::Full, 1.0);
        let blur = render(Some(&back), o, w, h, r, &m, Level::Blur, 1.0);
        let off = render(Some(&back), o, w, h, r, &m, Level::Off, 1.0);
        assert_eq!((full.w, full.h), (w, h));
        assert_eq!(full.get(0, 0)[3], 0, "rounded corners are transparent");
        assert_eq!(full.get(40, 20)[3], 255);
        // Off is the solid colour; frosted glass is not the backdrop's sharp stripes any more
        let p = off.get(40, 20);
        assert!((p[2] as i32 - 246).abs() <= 2, "{p:?}");
        let contrast = |c: &Canvas| (0..w).map(|x| c.get(x, 20)[0] as i32).max().unwrap() - (0..w).map(|x| c.get(x, 20)[0] as i32).min().unwrap();
        assert!(contrast(&blur) < 40, "frosted: {}", contrast(&blur));
        assert_ne!(full.px, blur.px, "refraction and light change the full level");
        // no backdrop: the solid fallback, whatever the level
        let none = render(None, 0, w, h, r, &m, Level::Full, 1.0);
        assert_eq!(none.get(40, 20), off.get(40, 20));
    }

    #[test]
    fn pressed_flattens() {
        let m = Material::for_kind(Kind::Control, true, Rgba::rgb(10, 132, 255));
        let p = m.pressed();
        assert!(p.params.z_radius < m.params.z_radius && p.params.specular < m.params.specular);
        assert!(Material::for_kind(Kind::Accent, false, Rgba::rgb(10, 132, 255)).opaque == Rgba::rgb(10, 132, 255));
    }

    #[test]
    fn tables_are_shared() {
        let p = Params::default();
        let a = ShapeTable::cached(33, 21, 8.0, &p);
        let b = ShapeTable::cached(33, 21, 8.0, &p);
        assert!(Arc::ptr_eq(&a, &b));
    }

    /// A picture of the material on a busy backdrop at the three levels (for eyes, not CI):
    /// `RM_GLASS_PROOF=out.ppm cargo test -p rm-viewer glass_proof -- --ignored`.
    #[test]
    #[ignore]
    fn glass_proof() {
        let (w, h) = (900usize, 300usize);
        let mut back = Canvas::new(w, h);
        back.fill_round_rect_with(0.0, 0.0, w as f32, h as f32, 0.0, |x, y| Rgba { r: x / w as f32, g: 0.4 + 0.4 * (y / h as f32), b: 1.0 - x / w as f32, a: 1.0 });
        for i in 0..30 {
            back.fill_circle(30.0 * i as f32 + 10.0, 150.0 + 90.0 * ((i as f32) * 0.7).sin(), 14.0, Rgba::rgb(250, 250, 250));
            back.fill_round_rect(30.0 * i as f32, 40.0, 4.0, 220.0, 0.0, Rgba::rgb(20, 20, 30));
        }
        let mut out = back.clone();
        for (i, level) in [Level::Full, Level::Blur, Level::Off].into_iter().enumerate() {
            let x0 = 30 + i * 290;
            for (kind, (dy, pw, ph, r)) in [(Kind::Sheet, (30, 250, 120, 18.0)), (Kind::Control, (170, 160, 44, 22.0)), (Kind::Accent, (230, 48, 48, 24.0))] {
                let m = Material::for_kind(kind, false, Rgba::rgb(10, 132, 255));
                let o = margin(&m, 1.0, level, pw, ph, r);
                let (bx, by) = (x0 as isize - o as isize, dy as isize - o as isize);
                let mut b = Canvas::new(pw + 2 * o, ph + 2 * o);
                b.composite(&back, -bx, -by, 1.0);
                let g = render(Some(&b), o, pw, ph, r, &m, level, 1.0);
                out.shadow(x0 as f32, dy as f32, pw as f32, ph as f32, r, m.shadow_spread, m.shadow_offset_y, Rgba::BLACK.alpha(0.25 * m.shadow_alpha));
                out.composite(&g, x0 as isize, dy as isize, 1.0);
            }
        }
        let Some(path) = std::env::var_os("RM_GLASS_PROOF") else { return };
        let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
        for p in &out.px {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(path, ppm).unwrap();
    }

    #[test]
    fn reduce_transparency_forces_off() {
        assert_eq!(Level::from_setting(0), Level::Full);
        assert_eq!(Level::from_setting(1), Level::Blur);
        assert_eq!(Level::from_setting(2), Level::Off);
    }
}
