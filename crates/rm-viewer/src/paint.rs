//! A small software painter for MacBridge's own surfaces (the app loading window, glass menus,
//! the search palette, banners, the Dock): premultiplied BGRA pixels, anti-aliased rounded
//! rectangles and circles from signed distances, gradients, scaled images, blur and shadows.
//! The pixels are what `UpdateLayeredWindow` takes, so a surface is drawn here and shown as is.
//! Text is drawn by the platform (surface.rs) into a coverage mask that is composited here.

/// A colour with straight alpha, 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const CLEAR: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const WHITE: Rgba = Rgba { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
    pub const BLACK: Rgba = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }

    #[allow(clippy::self_named_constructors)]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: a as f32 / 255.0 }
    }

    pub fn alpha(self, a: f32) -> Rgba {
        Rgba { a: a.clamp(0.0, 1.0), ..self }
    }

    pub fn fade(self, k: f32) -> Rgba {
        Rgba { a: (self.a * k).clamp(0.0, 1.0), ..self }
    }

    pub fn lerp(self, o: Rgba, t: f32) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        Rgba { r: self.r + (o.r - self.r) * t, g: self.g + (o.g - self.g) * t, b: self.b + (o.b - self.b) * t, a: self.a + (o.a - self.a) * t }
    }

    /// Lighter (k > 0, towards white) or darker (k < 0, towards black), alpha kept.
    pub fn shade(self, k: f32) -> Rgba {
        let target = if k >= 0.0 { Rgba::WHITE } else { Rgba::BLACK };
        Rgba { a: self.a, ..self.lerp(target, k.abs()) }
    }

    /// Relative luminance (sRGB weights), 0..1.
    pub fn luma(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    fn premul(self) -> [f32; 4] {
        [self.b * self.a, self.g * self.a, self.r * self.a, self.a]
    }
}

/// Premultiplied BGRA, row after row (what a 32-bit top-down DIB holds).
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[u8; 4]>,
}

/// Signed distance to a rounded rectangle at (x, y) of size w x h, radius r: negative inside.
#[allow(clippy::too_many_arguments)]
pub fn sdf_round_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.clamp(0.0, w.min(h) * 0.5);
    let (cx, cy) = (px - (x + w * 0.5), py - (y + h * 0.5));
    let (qx, qy) = (cx.abs() - (w * 0.5 - r), cy.abs() - (h * 0.5 - r));
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// Anti-aliased coverage of a pixel whose centre is `d` from the edge (one pixel wide ramp).
pub fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![[0; 4]; w * h] }
    }

    pub fn filled(w: usize, h: usize, c: Rgba) -> Canvas {
        let mut k = Canvas::new(w, h);
        k.clear(c);
        k
    }

    pub fn clear(&mut self, c: Rgba) {
        let p = c.premul();
        let v = [q(p[0]), q(p[1]), q(p[2]), q(p[3])];
        self.px.iter_mut().for_each(|x| *x = v);
    }

    pub fn get(&self, x: usize, y: usize) -> [u8; 4] {
        self.px[y * self.w + x]
    }

    /// Source-over of a straight-alpha colour with this much coverage.
    #[inline]
    pub fn blend(&mut self, x: usize, y: usize, c: Rgba, cov: f32) {
        if x >= self.w || y >= self.h {
            return;
        }
        let a = (c.a * cov).clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let d = &mut self.px[y * self.w + x];
        let inv = 1.0 - a;
        let s = [c.b * a, c.g * a, c.r * a, a];
        for i in 0..4 {
            d[i] = q(s[i] + d[i] as f32 / 255.0 * inv);
        }
    }

    /// Fill a rounded rectangle, coloured per pixel by `paint(x, y)` (gradients).
    #[allow(clippy::too_many_arguments)]
    pub fn fill_round_rect_with(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, paint: impl Fn(f32, f32) -> Rgba) {
        let (x0, y0) = ((x.floor() as isize).max(0) as usize, (y.floor() as isize).max(0) as usize);
        let (x1, y1) = (((x + w).ceil() as usize).min(self.w), ((y + h).ceil() as usize).min(self.h));
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let cov = coverage(sdf_round_rect(fx, fy, x, y, w, h, r));
                if cov > 0.0 {
                    self.blend(px, py, paint(fx, fy), cov);
                }
            }
        }
    }

    pub fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgba) {
        self.fill_round_rect_with(x, y, w, h, r, |_, _| c);
    }

    /// A line `width` wide along the inside of a rounded rectangle's edge.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_round_rect_with(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, paint: impl Fn(f32, f32) -> Rgba) {
        let (x0, y0) = ((x.floor() as isize).max(0) as usize, (y.floor() as isize).max(0) as usize);
        let (x1, y1) = (((x + w).ceil() as usize).min(self.w), ((y + h).ceil() as usize).min(self.h));
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let d = sdf_round_rect(fx, fy, x, y, w, h, r);
                // inside the shape and within `width` of its edge
                let cov = coverage(d) * coverage(-d - width);
                if cov > 0.0 {
                    self.blend(px, py, paint(fx, fy), cov);
                }
            }
        }
    }

    /// A line from (x0, y0) to (x1, y1), `rad` thick on each side, with round ends.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_capsule(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, rad: f32, c: Rgba) {
        let (lx, ly) = (x0.min(x1) - rad - 1.0, y0.min(y1) - rad - 1.0);
        let (hx, hy) = (x0.max(x1) + rad + 1.0, y0.max(y1) + rad + 1.0);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for py in (ly.max(0.0) as usize)..(hy.max(0.0) as usize).min(self.h) {
            for px in (lx.max(0.0) as usize)..(hx.max(0.0) as usize).min(self.w) {
                let (fx, fy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
                let t = ((fx * dx + fy * dy) / len2).clamp(0.0, 1.0);
                let d = ((fx - dx * t).powi(2) + (fy - dy * t).powi(2)).sqrt() - rad;
                let cov = coverage(d);
                if cov > 0.0 {
                    self.blend(px, py, c, cov);
                }
            }
        }
    }

    /// Draw `src` stretched to (x, y, w, h) keeping its `inset`-px border unscaled (a shadow
    /// or a frame made once at a small size, for any size).
    #[allow(clippy::too_many_arguments)]
    pub fn nine_slice(&mut self, src: &Canvas, inset: usize, x: isize, y: isize, w: usize, h: usize, opacity: f32) {
        if src.w < 2 * inset + 1 || src.h < 2 * inset + 1 || w < 2 * inset || h < 2 * inset {
            return;
        }
        let map = |d: usize, len: usize, slen: usize| -> usize {
            if d < inset {
                d
            } else if d >= len - inset {
                slen - (len - d)
            } else {
                inset + (d - inset) * (slen - 2 * inset) / (len - 2 * inset).max(1)
            }
        };
        for dy in 0..h {
            let ty = y + dy as isize;
            if ty < 0 || ty as usize >= self.h {
                continue;
            }
            let sy = map(dy, h, src.h);
            for dx in 0..w {
                let tx = x + dx as isize;
                if tx < 0 || tx as usize >= self.w {
                    continue;
                }
                let s = src.px[sy * src.w + map(dx, w, src.w)];
                if s[3] == 0 {
                    continue;
                }
                let d = &mut self.px[ty as usize * self.w + tx as usize];
                let inv = 1.0 - s[3] as f32 / 255.0 * opacity;
                for i in 0..4 {
                    d[i] = q(s[i] as f32 / 255.0 * opacity + d[i] as f32 / 255.0 * inv);
                }
            }
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, rad: f32, c: Rgba) {
        self.fill_round_rect(cx - rad, cy - rad, rad * 2.0, rad * 2.0, rad, c);
    }

    /// A ring arc from angle `a0` to `a1` (radians, clockwise from 12 o'clock), round caps.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(&mut self, cx: f32, cy: f32, rad: f32, width: f32, a0: f32, a1: f32, c: impl Fn(f32) -> Rgba) {
        let reach = rad + width;
        let (x0, y0) = (((cx - reach).floor().max(0.0)) as usize, ((cy - reach).floor().max(0.0)) as usize);
        let (x1, y1) = (((cx + reach).ceil() as usize).min(self.w), ((cy + reach).ceil() as usize).min(self.h));
        let span = (a1 - a0).max(0.0);
        let tau = std::f32::consts::TAU;
        for py in y0..y1 {
            for px in x0..x1 {
                let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let dist = (dx * dx + dy * dy).sqrt();
                let ang = (dx.atan2(-dy) + tau) % tau;
                let rel = (ang - a0 + tau * 2.0) % tau;
                let d = if rel <= span {
                    (dist - rad).abs() - width * 0.5
                } else {
                    // the round caps at both ends
                    let cap = |a: f32| {
                        let (ex, ey) = (cx + rad * a.sin(), cy - rad * a.cos());
                        ((px as f32 + 0.5 - ex).powi(2) + (py as f32 + 0.5 - ey).powi(2)).sqrt() - width * 0.5
                    };
                    cap(a0).min(cap(a0 + span))
                };
                let cov = coverage(d);
                if cov > 0.0 {
                    self.blend(px, py, c((rel / span.max(1e-3)).min(1.0)), cov);
                }
            }
        }
    }

    /// Draw `src` (premultiplied) scaled into (x, y, w, h), bilinear, at `opacity`, clipped to a
    /// rounded rectangle of radius `r` (0: none).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, src: &Canvas, x: f32, y: f32, w: f32, h: f32, r: f32, opacity: f32) {
        if src.w == 0 || src.h == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let (x0, y0) = ((x.floor() as isize).max(0) as usize, (y.floor() as isize).max(0) as usize);
        let (x1, y1) = (((x + w).ceil() as usize).min(self.w), ((y + h).ceil() as usize).min(self.h));
        let (sx, sy) = (src.w as f32 / w, src.h as f32 / h);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let cov = if r > 0.0 { coverage(sdf_round_rect(fx, fy, x, y, w, h, r)) } else { coverage(sdf_round_rect(fx, fy, x, y, w, h, 0.0)) };
                if cov <= 0.0 {
                    continue;
                }
                let p = src.sample((fx - x) * sx - 0.5, (fy - y) * sy - 0.5);
                let k = opacity * cov;
                let d = &mut self.px[py * self.w + px];
                let inv = 1.0 - p[3] * k;
                for i in 0..4 {
                    d[i] = q(p[i] * k + d[i] as f32 / 255.0 * inv);
                }
            }
        }
    }

    /// Bilinear sample (premultiplied, 0..1), edges clamped.
    pub fn sample(&self, fx: f32, fy: f32) -> [f32; 4] {
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |x: f32, y: f32| {
            let xi = (x as isize).clamp(0, self.w as isize - 1) as usize;
            let yi = (y as isize).clamp(0, self.h as isize - 1) as usize;
            self.px[yi * self.w + xi]
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        let mut o = [0f32; 4];
        for i in 0..4 {
            let top = a[i] as f32 * (1.0 - tx) + b[i] as f32 * tx;
            let bot = c[i] as f32 * (1.0 - tx) + d[i] as f32 * tx;
            o[i] = (top * (1.0 - ty) + bot * ty) / 255.0;
        }
        o
    }

    /// The part (x, y, w, h) of this canvas; outside it, its nearest edge pixel.
    pub fn crop(&self, x: isize, y: isize, w: usize, h: usize) -> Canvas {
        let mut out = Canvas::new(w, h);
        if self.w == 0 || self.h == 0 {
            return out;
        }
        for oy in 0..h {
            let sy = (y + oy as isize).clamp(0, self.h as isize - 1) as usize;
            for ox in 0..w {
                let sx = (x + ox as isize).clamp(0, self.w as isize - 1) as usize;
                out.px[oy * w + ox] = self.px[sy * self.w + sx];
            }
        }
        out
    }

    /// Place `src` 1:1 at (x, y) (source-over).
    pub fn composite(&mut self, src: &Canvas, x: isize, y: isize, opacity: f32) {
        for sy in 0..src.h {
            let dy = y + sy as isize;
            if dy < 0 || dy as usize >= self.h {
                continue;
            }
            for sx in 0..src.w {
                let dx = x + sx as isize;
                if dx < 0 || dx as usize >= self.w {
                    continue;
                }
                let s = src.px[sy * src.w + sx];
                if s[3] == 0 {
                    continue;
                }
                let k = opacity;
                let d = &mut self.px[dy as usize * self.w + dx as usize];
                let inv = 1.0 - s[3] as f32 / 255.0 * k;
                for i in 0..4 {
                    d[i] = q(s[i] as f32 / 255.0 * k + d[i] as f32 / 255.0 * inv);
                }
            }
        }
    }

    /// Paint `color` through a coverage mask (one byte per pixel, `mw` wide) placed at (x, y):
    /// how text and shadows are put on.
    pub fn fill_mask(&mut self, mask: &[u8], mw: usize, x: isize, y: isize, color: Rgba) {
        let mh = mask.len() / mw.max(1);
        // light on dark reads thinner than dark on light when blended as sRGB: its partial
        // coverage is lifted, as text rendering's contrast enhancement does
        let lift = color.luma() > 0.5;
        let lut: [f32; 256] = std::array::from_fn(|i| {
            let v = i as f32 / 255.0;
            if lift { v.powf(0.78) } else { v }
        });
        for my in 0..mh {
            let dy = y + my as isize;
            if dy < 0 || dy as usize >= self.h {
                continue;
            }
            for mx in 0..mw {
                let dx = x + mx as isize;
                let m = mask[my * mw + mx];
                if m == 0 || dx < 0 || dx as usize >= self.w {
                    continue;
                }
                self.blend(dx as usize, dy as usize, color, lut[m as usize]);
            }
        }
    }

    /// Box blur, `iterations` passes each way (three approach a Gaussian).
    pub fn blur(&mut self, radius: usize, iterations: usize) {
        if radius == 0 || self.w == 0 || self.h == 0 {
            return;
        }
        let mut tmp = self.px.clone();
        for _ in 0..iterations.max(1) {
            box_pass(&self.px, &mut tmp, self.w, self.h, radius, true);
            box_pass(&tmp, &mut self.px, self.w, self.h, radius, false);
        }
    }

    /// More (> 1) or less colourful, around each pixel's luma.
    pub fn saturate(&mut self, amount: f32) {
        if (amount - 1.0).abs() < 1e-3 {
            return;
        }
        for p in &mut self.px {
            let a = p[3] as f32;
            if a == 0.0 {
                continue;
            }
            let (b, g, r) = (p[0] as f32, p[1] as f32, p[2] as f32);
            let l = r * 0.2126 + g * 0.7152 + b * 0.0722;
            let f = |v: f32| (l + (v - l) * amount).clamp(0.0, a) as u8;
            *p = [f(b), f(g), f(r), p[3]];
        }
    }

    /// Multiply every pixel's alpha (and colour) by `mask` (same size, 0..255).
    pub fn mask(&mut self, mask: &[u8]) {
        for (p, &m) in self.px.iter_mut().zip(mask) {
            if m != 255 {
                for c in p.iter_mut() {
                    *c = ((*c as u32 * m as u32 + 127) / 255) as u8;
                }
            }
        }
    }

    /// A soft drop shadow of a rounded rectangle, drawn under what is already there would be the
    /// usual order: call this first. `spread` blurs, `dy` moves it down.
    #[allow(clippy::too_many_arguments)]
    pub fn shadow(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, spread: f32, dy: f32, color: Rgba) {
        let pad = (spread * 2.0).ceil() as usize + 2;
        let (sw, sh) = (w.ceil() as usize + pad * 2, h.ceil() as usize + pad * 2);
        let mut s = Canvas::new(sw, sh);
        s.fill_round_rect(pad as f32, pad as f32, w, h, r, Rgba::BLACK);
        s.blur((spread / 2.0).round().max(1.0) as usize, 3);
        let mask: Vec<u8> = s.px.iter().map(|p| p[3]).collect();
        self.fill_mask(&mask, sw, (x - pad as f32).round() as isize, (y + dy - pad as f32).round() as isize, color);
    }

    /// The average colour (straight alpha) of the pixels at least half opaque.
    pub fn average(&self) -> Rgba {
        let (mut r, mut g, mut b, mut n) = (0f64, 0f64, 0f64, 0f64);
        for p in &self.px {
            if p[3] >= 128 {
                let a = p[3] as f64;
                r += p[2] as f64 / a;
                g += p[1] as f64 / a;
                b += p[0] as f64 / a;
                n += 1.0;
            }
        }
        if n == 0.0 {
            return Rgba::rgb(128, 128, 128);
        }
        Rgba { r: (r / n) as f32, g: (g / n) as f32, b: (b / n) as f32, a: 1.0 }
    }

    /// From straight-alpha RGBA bytes (an app icon from the Mac).
    pub fn from_rgba(w: usize, h: usize, rgba: &[u8]) -> Canvas {
        let px = rgba
            .chunks_exact(4)
            .map(|c| {
                let a = c[3] as u32;
                let m = |v: u8| ((v as u32 * a + 127) / 255) as u8;
                [m(c[2]), m(c[1]), m(c[0]), c[3]]
            })
            .collect();
        Canvas { w, h, px }
    }
}

#[inline]
fn q(v: f32) -> u8 {
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

fn box_pass(src: &[[u8; 4]], dst: &mut [[u8; 4]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let win = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut acc = [0u32; 4];
        let at = |i: isize| src[idx(line, i.clamp(0, len as isize - 1) as usize)];
        for i in -(r as isize)..=(r as isize) {
            let p = at(i);
            for c in 0..4 {
                acc[c] += p[c] as u32;
            }
        }
        for i in 0..len {
            let o = &mut dst[idx(line, i)];
            for c in 0..4 {
                o[c] = (acc[c] / win) as u8;
            }
            let (out, inn) = (at(i as isize - r as isize), at(i as isize + r as isize + 1));
            for c in 0..4 {
                acc[c] = acc[c] + inn[c] as u32 - out[c] as u32;
            }
        }
    }
}

/// The colour an app's icon is mostly made of (its "dominant" colour): the most common
/// saturated hue among its opaque pixels, averaged; for a grey icon, its average colour.
pub fn dominant(icon: &Canvas) -> Rgba {
    let mut buckets = [(0f64, 0f64, 0f64, 0f64); 24];
    for p in &icon.px {
        if p[3] < 160 {
            continue;
        }
        let a = p[3] as f64;
        let (r, g, b) = (p[2] as f64 / a, p[1] as f64 / a, p[0] as f64 / a);
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        if sat < 0.25 || mx < 0.2 {
            continue;
        }
        let d = mx - mn;
        let hue = if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 } / 6.0;
        let k = ((hue * 24.0) as usize).min(23);
        let wgt = sat * mx;
        buckets[k].0 += r * wgt;
        buckets[k].1 += g * wgt;
        buckets[k].2 += b * wgt;
        buckets[k].3 += wgt;
    }
    // a hue and its two neighbours together (one colour across a bucket edge)
    let score = |i: usize| buckets[i].3 + 0.5 * (buckets[(i + 23) % 24].3 + buckets[(i + 1) % 24].3);
    let best = (0..24).max_by(|&a, &b| score(a).total_cmp(&score(b))).unwrap_or(0);
    let total: f64 = buckets.iter().map(|b| b.3).sum();
    if buckets[best].3 <= 0.0 || buckets[best].3 < total * 0.08 {
        return icon.average();
    }
    let (r, g, b, w) = buckets[best];
    Rgba { r: (r / w) as f32, g: (g / w) as f32, b: (b / w) as f32, a: 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rect_is_filled_with_soft_edges_and_round_corners() {
        let mut c = Canvas::new(40, 30);
        c.fill_round_rect(5.5, 5.0, 30.0, 20.0, 8.0, Rgba::rgb(255, 0, 0));
        assert_eq!(c.get(20, 15), [0, 0, 255, 255], "inside: opaque red (BGRA)");
        assert_eq!(c.get(1, 1)[3], 0, "outside: clear");
        assert_eq!(c.get(5, 5)[3], 0, "the corner is rounded off");
        let edge = c.get(5, 15)[3];
        assert!(edge > 60 && edge < 200, "the edge is anti-aliased: {edge}");
    }

    #[test]
    fn premultiplied_blending() {
        let mut c = Canvas::filled(2, 1, Rgba::WHITE);
        c.blend(0, 0, Rgba::rgb(0, 0, 0).alpha(0.5), 1.0);
        let p = c.get(0, 0);
        assert!((p[0] as i32 - 128).abs() <= 1 && p[3] == 255, "{p:?}");
        let mut t = Canvas::new(1, 1);
        t.blend(0, 0, Rgba::rgb(255, 255, 255).alpha(0.5), 1.0);
        assert_eq!(t.get(0, 0), [128, 128, 128, 128], "premultiplied");
    }

    #[test]
    fn blur_spreads_and_keeps_energy() {
        let mut c = Canvas::new(21, 21);
        c.px[10 * 21 + 10] = [255, 255, 255, 255];
        let before: u32 = c.px.iter().map(|p| p[3] as u32).sum();
        c.blur(2, 1);
        assert!(c.get(10, 10)[3] < 255 && c.get(12, 12)[3] > 0);
        let after: u32 = c.px.iter().map(|p| p[3] as u32).sum();
        assert!((before as i32 - after as i32).abs() < 40, "{before} {after}");
    }

    #[test]
    fn images_scale_bilinearly_and_masks_paint_text() {
        let src = Canvas::filled(2, 2, Rgba::rgb(0, 255, 0));
        let mut c = Canvas::new(10, 10);
        c.draw(&src, 0.0, 0.0, 10.0, 10.0, 0.0, 1.0);
        assert_eq!(c.get(5, 5), [0, 255, 0, 255]);
        let mut t = Canvas::new(4, 1);
        t.fill_mask(&[0, 128, 255, 0], 4, 0, 0, Rgba::BLACK);
        assert_eq!(t.get(0, 0)[3], 0);
        assert!((t.get(1, 0)[3] as i32 - 128).abs() <= 1 && t.get(2, 0)[3] == 255);
        // light text: its partial coverage lifted (it reads as heavy as dark text)
        let mut l = Canvas::new(4, 1);
        l.fill_mask(&[0, 128, 255, 0], 4, 0, 0, Rgba::WHITE);
        assert!(l.get(1, 0)[3] > 140 && l.get(2, 0)[3] == 255 && l.get(0, 0)[3] == 0);
    }

    #[test]
    fn arcs_and_shadows() {
        let mut c = Canvas::new(40, 40);
        c.arc(20.0, 20.0, 12.0, 3.0, 0.0, std::f32::consts::PI, |_| Rgba::WHITE);
        assert!(c.get(32, 20)[3] > 200, "3 o'clock is on the half arc");
        assert_eq!(c.get(8, 20)[3], 0, "9 o'clock is not");
        let mut s = Canvas::new(60, 60);
        s.shadow(15.0, 15.0, 30.0, 30.0, 6.0, 8.0, 4.0, Rgba::BLACK.alpha(0.5));
        assert!(s.get(30, 30)[3] > 100 && s.get(30, 48)[3] > 0 && s.get(2, 2)[3] == 0);
    }

    #[test]
    fn the_dominant_colour_of_an_icon() {
        // a teal icon on a white plate with a small orange mark: teal wins
        let mut icon = Canvas::filled(32, 32, Rgba::WHITE);
        icon.fill_round_rect(2.0, 2.0, 28.0, 28.0, 6.0, Rgba::rgb(20, 160, 170));
        icon.fill_circle(26.0, 6.0, 3.0, Rgba::rgb(250, 140, 20));
        let d = dominant(&icon);
        assert!(d.g > 0.5 && d.b > 0.5 && d.r < 0.2, "{d:?}");
        // a grey icon: its average
        let g = dominant(&Canvas::filled(8, 8, Rgba::rgb(100, 100, 100)));
        assert!((g.r - 100.0 / 255.0).abs() < 0.01);
    }

    #[test]
    fn capsules_and_nine_slices() {
        let mut c = Canvas::new(30, 30);
        c.fill_capsule(5.0, 15.0, 25.0, 15.0, 2.0, Rgba::WHITE);
        assert_eq!(c.get(15, 15)[3], 255);
        assert_eq!(c.get(15, 20)[3], 0);
        let mut src = Canvas::new(9, 9);
        src.fill_round_rect(0.0, 0.0, 9.0, 9.0, 3.0, Rgba::WHITE);
        let mut d = Canvas::new(40, 20);
        d.nine_slice(&src, 4, 0, 0, 40, 20, 1.0);
        assert_eq!(d.get(20, 10)[3], 255, "the middle stretches");
        assert_eq!(d.get(0, 0)[3], src.get(0, 0)[3], "corners stay as they are");
    }

    #[test]
    fn rgba_icons_are_premultiplied() {
        let c = Canvas::from_rgba(1, 1, &[255, 0, 0, 128]);
        assert_eq!(c.get(0, 0), [0, 0, 128, 128]);
    }
}
