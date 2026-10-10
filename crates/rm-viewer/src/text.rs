//! Text as macOS draws it, on every surface MacBridge paints itself. Windows' GDI snaps each
//! letter to the pixel grid (hinting) and to whole pixels of advance, which made the text look
//! jagged and the letters crowd; macOS does neither. Here:
//!
//! - Inter (bundled, variable: weight and optical size) is shaped with HarfBuzz's rules
//!   (rustybuzz): kerning, ligatures, marks;
//! - the optical size follows the size in DIPs, as SF's Text and Display cuts do, and the
//!   tracking follows Apple's table for SF (tighter as text grows, until the display sizes);
//! - outlines are drawn unhinted at fractional positions (exact area coverage), the baseline on
//!   a whole pixel, as Core Text draws them;
//! - letters Inter does not have (Chinese, Japanese, Korean, symbols, emoji in outline) come from
//!   Windows' own fonts.
//!
//! Lines are coverage masks (one byte a pixel), painted with `Canvas::fill_mask`.

use ab_glyph_rasterizer::{point, Point, Rasterizer};
use rustybuzz::ttf_parser::{self, OutlineBuilder, Tag};
use rustybuzz::{Face, UnicodeBuffer, Variation};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

static INTER: &[u8] = include_bytes!("../fonts/InterVariable.ttf");

/// The display scale most text is drawn at (for the optical size of sizes given in pixels).
static SCALE: AtomicU32 = AtomicU32::new(0x3f80_0000); // 1.0

/// Sizes passed in device pixels are this many pixels per DIP (the system's scale).
pub fn set_scale_hint(scale: f32) {
    SCALE.store(scale.clamp(0.5, 8.0).to_bits(), Ordering::Relaxed);
}

fn scale_hint() -> f32 {
    f32::from_bits(SCALE.load(Ordering::Relaxed))
}

/// Apple's tracking for SF at a size in points (DIPs), as a fraction of the size: looser below
/// 12, tighter up to 17, about none from the display sizes on (Inter's display cut is already
/// set tighter). Inter is a little wider than SF, so text sizes are set a touch tighter still.
pub fn tracking(dip: f32) -> f32 {
    const T: [(f32, f32); 9] = [(9.0, 0.012), (11.0, 0.005), (12.0, 0.0), (13.0, -0.006), (14.0, -0.010), (15.0, -0.014), (17.0, -0.020), (20.0, -0.016), (28.0, -0.010)];
    if dip <= T[0].0 {
        return T[0].1;
    }
    for w in T.windows(2) {
        if dip <= w[1].0 {
            let k = (dip - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + (w[1].1 - w[0].1) * k;
        }
    }
    T[T.len() - 1].1
}

/// A face we may draw with: Inter, or one of the system's fonts for what Inter lacks.
struct Source {
    data: &'static [u8],
    index: u32,
    variable: bool,
}

fn inter() -> &'static ttf_parser::Face<'static> {
    static F: OnceLock<ttf_parser::Face<'static>> = OnceLock::new();
    F.get_or_init(|| ttf_parser::Face::parse(INTER, 0).expect("bundled Inter"))
}

/// The system's fonts for what Inter does not cover, in order (loaded the first time needed).
fn fallback_files() -> &'static [(String, u32)] {
    static F: OnceLock<Vec<(String, u32)>> = OnceLock::new();
    F.get_or_init(|| {
        if cfg!(windows) {
            let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
            ["segoeui.ttf", "msyh.ttc", "YuGothM.ttc", "malgun.ttf", "Nirmala.ttf", "seguisym.ttf", "seguiemj.ttf", "arialuni.ttf"].iter().map(|f| (dir.clone() + f, 0)).collect()
        } else {
            ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"].iter().map(|f| (f.to_string(), 0)).collect()
        }
    })
}

type Loaded = OnceLock<Option<(&'static [u8], ttf_parser::Face<'static>)>>;

fn fallback(i: usize) -> Option<&'static ttf_parser::Face<'static>> {
    static F: OnceLock<Vec<Loaded>> = OnceLock::new();
    let all = F.get_or_init(|| fallback_files().iter().map(|_| OnceLock::new()).collect());
    let (path, index) = fallback_files().get(i)?;
    all[i]
        .get_or_init(|| {
            let data: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
            let face = ttf_parser::Face::parse(data, *index).ok()?;
            Some((data, face))
        })
        .as_ref()
        .map(|(_, f)| f)
}

fn source(font: usize) -> Source {
    if font == 0 {
        return Source { data: INTER, index: 0, variable: true };
    }
    let f = fallback(font - 1).expect("a loaded fallback");
    Source { data: f.raw_face().data, index: fallback_files()[font - 1].1, variable: false }
}

/// Marks, joiners and selectors stay with the letter before them.
fn joins(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F | 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x1F3FB..=0x1F3FF)
}

/// Which face draws `c` (0 Inter, n the n-th fallback).
fn font_for(c: char) -> usize {
    if c.is_control() || inter().glyph_index(c).is_some() {
        return 0;
    }
    for i in 0..fallback_files().len() {
        if fallback(i).is_some_and(|f| f.glyph_index(c).is_some()) {
            return i + 1;
        }
    }
    0
}

/// The face for a run, with Inter's weight and optical size set.
fn face(font: usize, weight: f32, opsz: f32) -> Option<Face<'static>> {
    let s = source(font);
    let mut f = Face::from_slice(s.data, s.index)?;
    if s.variable {
        f.set_variations(&[Variation { tag: Tag::from_bytes(b"wght"), value: weight.clamp(100.0, 900.0) }, Variation { tag: Tag::from_bytes(b"opsz"), value: opsz.clamp(14.0, 32.0) }]);
    }
    Some(f)
}

#[derive(Clone, Copy, Debug)]
struct Glyph {
    run: usize,
    id: u16,
    /// where the glyph is drawn (device px), its offset from the pen, and its vertical offset
    x: f32,
    dx: f32,
    dy: f32,
    /// the byte in the text this glyph starts
    cluster: usize,
}

/// A shaped line: glyphs at fractional positions and its advance.
pub struct Layout {
    faces: Vec<(Face<'static>, f32)>,
    glyphs: Vec<Glyph>,
    /// the pen after the last glyph (device px)
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// (byte offset, x) at each place a caret can stand, first to last
    pub stops: Vec<(usize, f32)>,
}

/// How a line is set: the size in device pixels, the weight (400 regular … 700 bold), and the
/// scale (device pixels per DIP) for the optical size and the tracking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub px: f32,
    pub weight: f32,
    pub scale: f32,
}

impl Style {
    pub fn new(px: f32, weight: i32) -> Style {
        Style { px, weight: weight as f32, scale: scale_hint() }
    }
    pub fn dip(size: f32, weight: i32, scale: f32) -> Style {
        Style { px: size * scale, weight: weight as f32, scale }
    }
}

/// Shape `s` in one line.
pub fn layout(s: &str, st: Style) -> Layout {
    let dip = st.px / st.scale.max(0.1);
    // small text a hair heavier, as Apple's text cut is
    let weight = st.weight + if dip < 14.0 { 10.0 } else { 0.0 };
    let track = tracking(dip) * st.px;
    let k_inter = st.px / inter().units_per_em() as f32;
    let mut out = Layout { faces: vec![], glyphs: vec![], width: 0.0, ascent: inter().ascender() as f32 * k_inter, descent: -(inter().descender() as f32) * k_inter, stops: vec![] };
    // runs of one face each
    let mut runs: Vec<(usize, usize, usize)> = vec![]; // (font, start, end)
    for (i, c) in s.char_indices() {
        let f = match runs.last() {
            Some(r) if joins(c) => r.0,
            _ => font_for(c),
        };
        match runs.last_mut() {
            Some(r) if r.0 == f => r.2 = i + c.len_utf8(),
            _ => runs.push((f, i, i + c.len_utf8())),
        }
    }
    let mut pen = 0.0f32;
    let mut last_track = 0.0f32;
    for (font, start, end) in runs {
        let Some(face) = face(font, weight, dip) else { continue };
        let k = st.px / face.units_per_em() as f32;
        let mut buf = UnicodeBuffer::new();
        buf.push_str(&s[start..end]);
        buf.guess_segment_properties();
        let shaped = rustybuzz::shape(&face, &[], buf);
        let run = out.faces.len();
        let tr = if font == 0 { track } else { 0.0 };
        let (infos, pos) = (shaped.glyph_infos(), shaped.glyph_positions());
        for (n, (gi, gp)) in infos.iter().zip(pos).enumerate() {
            let cluster = start + gi.cluster as usize;
            let dx = gp.x_offset as f32 * k;
            out.glyphs.push(Glyph { run, id: gi.glyph_id as u16, x: pen + dx, dx, dy: gp.y_offset as f32 * k, cluster });
            pen += gp.x_advance as f32 * k;
            // tracking between clusters, not inside one (marks stay on their letter)
            let next_cluster = infos.get(n + 1).map(|g| g.cluster);
            if next_cluster != Some(gi.cluster) {
                pen += tr;
                last_track = tr;
            }
        }
        out.faces.push((face, k));
    }
    // no tracking after the last letter
    out.width = (pen - last_track).max(0.0);
    // caret stops: each cluster's start, and the end
    let mut stops: Vec<(usize, f32)> = vec![];
    for g in &out.glyphs {
        if stops.last().is_none_or(|l| l.0 != g.cluster) {
            stops.push((g.cluster, g.x - g.dx));
        }
    }
    stops.sort_by_key(|s| s.0);
    stops.dedup_by_key(|s| s.0);
    stops.push((s.len(), out.width));
    out.stops = stops;
    out
}

impl Layout {
    /// Cut to at most `max_w` px with an ellipsis (shaped with the line's first face).
    fn ellipsize(&mut self, max_w: f32, st: Style) {
        if self.width <= max_w + 0.01 || self.glyphs.is_empty() {
            return;
        }
        let dots = layout("…", st);
        let room = (max_w - dots.width).max(0.0);
        // keep the glyphs whose cluster ends before `room`
        let mut keep = 0;
        for i in 0..self.glyphs.len() {
            let next = self.glyphs.get(i + 1).map_or(self.width, |n| n.x - n.dx);
            if next > room + 0.01 {
                break;
            }
            keep = i + 1;
        }
        // not after a space
        while keep > 0 && self.faces[self.glyphs[keep - 1].run].0.glyph_index(' ').map(|g| g.0) == Some(self.glyphs[keep - 1].id) {
            keep -= 1;
        }
        let pen = self.glyphs.get(keep).map_or(self.width, |g| g.x - g.dx);
        self.glyphs.truncate(keep);
        let base = self.faces.len();
        for g in &dots.glyphs {
            self.glyphs.push(Glyph { run: base + g.run, x: pen + g.x, ..*g });
        }
        self.faces.extend(dots.faces);
        self.width = pen + dots.width;
    }

    /// The line as coverage: (mask, width, height), the baseline `ascent` (rounded) from the top.
    pub fn rasterize(&self) -> (Vec<u8>, usize, usize) {
        let asc = self.ascent.round();
        let h = (asc + self.descent).ceil().max(1.0) as usize;
        // ink may reach left of the pen's start (a "j", an italic): keep it, shifted right
        let mut left = 0.0f32;
        let mut right = self.width;
        for g in &self.glyphs {
            let (face, k) = &self.faces[g.run];
            if let Some(b) = face.glyph_bounding_box(ttf_parser::GlyphId(g.id)) {
                left = left.min(g.x + b.x_min as f32 * k);
                right = right.max(g.x + b.x_max as f32 * k);
            }
        }
        let shift = (-left).ceil().max(0.0);
        let w = (right + shift).ceil().max(1.0) as usize + 1;
        let mut r = Rasterizer::new(w, h);
        for g in &self.glyphs {
            let (face, k) = &self.faces[g.run];
            let mut pen = Pen { r: &mut r, ox: g.x + shift, oy: asc - g.dy, k: *k, start: point(0.0, 0.0), last: point(0.0, 0.0) };
            face.outline_glyph(ttf_parser::GlyphId(g.id), &mut pen);
        }
        let mut mask = vec![0u8; w * h];
        r.for_each_pixel(|i, a| {
            if let Some(m) = mask.get_mut(i) {
                *m = (a.abs().min(1.0) * 255.0 + 0.5) as u8;
            }
        });
        // trim the unused column on the right
        let used = (self.width.max(right) + shift).ceil().max(1.0) as usize;
        if used < w {
            let mut t = Vec::with_capacity(used * h);
            for row in mask.chunks_exact(w) {
                t.extend_from_slice(&row[..used]);
            }
            return (t, used, h);
        }
        (mask, w, h)
    }
}

struct Pen<'a> {
    r: &'a mut Rasterizer,
    ox: f32,
    oy: f32,
    k: f32,
    start: Point,
    last: Point,
}

impl Pen<'_> {
    fn p(&self, x: f32, y: f32) -> Point {
        point(self.ox + x * self.k, self.oy - y * self.k)
    }
}

impl OutlineBuilder for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.start = p;
        self.last = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.r.draw_line(self.last, p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.p(x1, y1), self.p(x, y));
        self.r.draw_quad(self.last, c, p);
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        self.r.draw_cubic(self.last, c1, c2, p);
        self.last = p;
    }
    fn close(&mut self) {
        if self.last != self.start {
            self.r.draw_line(self.last, self.start);
        }
        self.last = self.start;
    }
}

/// One line of `s` as coverage, at most `max_w` px wide (cut with an ellipsis).
pub fn line(s: &str, st: Style, max_w: f32) -> (Vec<u8>, usize, usize) {
    let mut l = layout(s, st);
    l.ellipsize(max_w.max(1.0), st);
    l.rasterize()
}

/// `line` with the size in device pixels, at the system's scale: (mask, width, height).
pub fn mask(s: &str, px: i32, weight: i32, max_w: usize) -> (Vec<u8>, usize, usize) {
    line(s, Style::new(px as f32, weight), max_w as f32)
}

/// How wide `s` is set (device px).
pub fn width(s: &str, st: Style) -> f32 {
    layout(s, st).width
}

/// Where a caret stands before each character of `s` and after the last (device px): one
/// position per character boundary, `chars + 1` in all.
pub fn carets(s: &str, st: Style) -> Vec<f32> {
    let l = layout(s, st);
    let mut out = Vec::with_capacity(s.chars().count() + 1);
    for (b, _) in s.char_indices() {
        // the cluster this character is in, and where the next one starts
        let k = l.stops.iter().rposition(|st| st.0 <= b).unwrap_or(0);
        let (b0, x0) = l.stops[k];
        let (b1, x1) = l.stops.get(k + 1).copied().unwrap_or((s.len(), l.width));
        let t = if b1 > b0 { (b - b0) as f32 / (b1 - b0) as f32 } else { 0.0 };
        out.push(x0 + (x1 - x0) * t);
    }
    out.push(l.width);
    out
}

/// `s` in at most `lines` lines of at most `max_w` px, broken between words (a word too long
/// for a line is cut there); the last line ends with an ellipsis when the text goes on.
pub fn wrap(s: &str, st: Style, max_w: f32, lines: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let next = if cur.is_empty() { w.to_string() } else { format!("{cur} {w}") };
        if width(&next, st) <= max_w || cur.is_empty() {
            cur = next;
            i += 1;
            if cur == w && width(&cur, st) > max_w && out.len() + 1 < lines {
                // one word wider than the line: cut it where it no longer fits
                let mut cut = String::new();
                let mut rest = String::new();
                for c in cur.chars() {
                    if rest.is_empty() && width(&format!("{cut}{c}"), st) <= max_w {
                        cut.push(c);
                    } else {
                        rest.push(c);
                    }
                }
                out.push(cut);
                cur = rest;
            }
            continue;
        }
        out.push(std::mem::take(&mut cur));
        if out.len() == lines {
            break;
        }
    }
    if out.len() < lines && !cur.is_empty() {
        out.push(std::mem::take(&mut cur));
    }
    // what did not fit goes on the last line (ellipsized when drawn)
    if i < words.len() || !cur.is_empty() {
        if let Some(last) = out.last_mut() {
            let rest: Vec<&str> = std::iter::once(cur.as_str()).filter(|c| !c.is_empty()).chain(words[i..].iter().copied()).collect();
            if !rest.is_empty() {
                *last = format!("{last} {}", rest.join(" "));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(m: &(Vec<u8>, usize, usize)) -> u32 {
        m.0.iter().map(|&v| v as u32).sum()
    }

    #[test]
    fn letters_are_kerned_and_tracked() {
        let st = Style { px: 26.0, weight: 400.0, scale: 2.0 };
        let (av, a, v) = (width("AV", st), width("A", st), width("V", st));
        assert!(av < a + v - 0.5, "kerned: {av} vs {a} + {v}");
        // fractional advances: not whole pixels
        let w = width("Illuminate", st);
        assert!((w - w.round()).abs() > 0.001 || w > 0.0);
        assert!(tracking(17.0) < tracking(13.0) && tracking(11.0) > 0.0);
    }

    #[test]
    fn a_line_has_its_height_and_ink() {
        let m = line("Applications", Style { px: 13.0, weight: 400.0, scale: 1.0 }, 1000.0);
        assert!(m.2 >= 15 && m.2 <= 17, "height {}", m.2);
        assert!(m.1 > 60 && m.1 < 90, "width {}", m.1);
        assert!(ink(&m) > 20_000);
        // edges are soft: some partial coverage
        assert!(m.0.iter().any(|&v| v > 20 && v < 230));
    }

    #[test]
    fn heavier_weights_have_more_ink() {
        let st = |w| Style { px: 20.0, weight: w, scale: 1.0 };
        assert!(ink(&line("Connect", st(700.0), 999.0)) > ink(&line("Connect", st(400.0), 999.0)) * 11 / 10);
    }

    #[test]
    fn long_lines_end_with_an_ellipsis() {
        let st = Style { px: 13.0, weight: 400.0, scale: 1.0 };
        let full = line("A rather long name for an application", st, 1000.0);
        let cut = line("A rather long name for an application", st, 120.0);
        assert!(cut.1 <= 122 && cut.1 > 90, "cut {}", cut.1);
        assert!(full.1 > 200);
    }

    #[test]
    fn vietnamese_and_missing_letters_never_fail() {
        let st = Style { px: 14.0, weight: 500.0, scale: 1.0 };
        let vi = line("Kết nối đến máy Mac", st, 999.0);
        assert!(ink(&vi) > 10_000);
        // decomposed marks too (as macOS names files)
        let nfd = line("Ke\u{0302}\u{0301}t no\u{0302}\u{0301}i", st, 999.0);
        assert!(ink(&nfd) > 5_000);
        let _ = line("日本語 한국어 中文 ✓ 😀", st, 999.0);
        let _ = line("", st, 999.0);
    }

    #[test]
    fn names_wrap_between_words_in_two_lines() {
        let st = Style { px: 12.0, weight: 500.0, scale: 1.0 };
        assert_eq!(wrap("Safari", st, 100.0, 2), vec!["Safari"]);
        let two = wrap("Audio MIDI Setup", st, 80.0, 2);
        assert_eq!(two.len(), 2, "{two:?}");
        assert!(width(&two[0], st) <= 80.0);
        let many = wrap("A very long application name indeed", st, 70.0, 2);
        assert_eq!(many.len(), 2);
        assert!(many[1].contains("indeed"), "the rest stays on the last line: {many:?}");
        assert_eq!(wrap("", st, 80.0, 2), Vec::<String>::new());
    }

    #[test]
    fn a_caret_for_every_character() {
        let st = Style { px: 13.0, weight: 400.0, scale: 1.0 };
        let c = carets("máy Mac", st);
        assert_eq!(c.len(), 8);
        assert!(c.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(carets("", st), vec![0.0]);
    }

    #[test]
    fn carets_stand_between_letters() {
        let l = layout("abc", Style { px: 13.0, weight: 400.0, scale: 1.0 });
        assert_eq!(l.stops.iter().map(|s| s.0).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
        assert!(l.stops.windows(2).all(|w| w[1].1 > w[0].1));
    }
}

/// Writes a specimen of the text at the sizes the UI uses (RM_PREVIEW=dir cargo test specimen).
#[cfg(test)]
mod preview {
    use super::*;
    use crate::paint::{Canvas, Rgba};

    #[test]
    fn specimen() {
        let Some(dir) = std::env::var_os("RM_PREVIEW") else { return };
        let s: f32 = std::env::var("RM_PREVIEW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
        let mut c = Canvas::filled((520.0 * s) as usize, (300.0 * s) as usize, Rgba::WHITE);
        c.fill_round_rect(260.0 * s, 0.0, 260.0 * s, 300.0 * s, 0.0, Rgba::rgb(0x1e, 0x1e, 0x22));
        let rows: [(&str, f32, i32); 8] = [
            ("Applications", 26.0, 700),
            ("Screen Sharing", 15.0, 600),
            ("Connected through the relay", 13.0, 400),
            ("Kết nối đến máy Mac của bạn", 13.0, 500),
            ("Mac Desktop  TextEdit  Xcode", 12.0, 500),
            ("Search apps", 13.0, 400),
            ("Volume, Keyboard, Glass · 60 fps", 11.0, 400),
            ("Connect", 13.0, 600),
        ];
        for half in 0..2 {
            let (x0, fg) = if half == 0 { (16.0, Rgba::rgb(0x1d, 0x1d, 0x1f)) } else { (276.0, Rgba::rgb(0xec, 0xec, 0xee)) };
            let mut y = 14.0;
            for (t, size, w) in rows {
                let m = line(t, Style::dip(size, w, s), 1000.0);
                c.fill_mask(&m.0, m.1, (x0 * s) as isize, (y * s) as isize, fg);
                y += size * 1.3 + 8.0;
            }
        }
        let mut ppm = format!("P6 {} {} 255\n", c.w, c.h).into_bytes();
        for p in &c.px {
            ppm.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        std::fs::write(std::path::Path::new(&dir).join("text.ppm"), ppm).unwrap();
    }
}
