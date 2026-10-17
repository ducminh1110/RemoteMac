//! Taro of a provider (Taro Enterprise): the official, unmodified Taro.exe (renamed, say
//! `Acme.exe`) with a `provider/` folder next to it: `provider.json` (rm-brand: the product's
//! name, its colour, its relay and key, how updates come) and its icon. Read once at start; every
//! part of Taro asks here for the product's name, its colour and its icon. Without the folder this
//! is the standard Taro, exactly as always.
//!
//! A provider folder that is there but wrong (no `provider.json`, or one that does not validate)
//! is an error, never a silent fall back to the standard Taro: that would take the provider's
//! customers to Taro's relay instead of their provider's.

use crate::paint::{Canvas, Rgba};
use rm_brand::{ClientUpdates, Color, Provider};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The standard product's name (and what a provider's Taro is based on).
pub const TARO: &str = "Taro";
/// Taro's own accent: its violet, brighter in dark mode.
pub const TARO_ACCENT: Rgba = Rgba::rgb(0x7c, 0x4a, 0xe8);
pub const TARO_ACCENT_DARK: Rgba = Rgba::rgb(0xa0, 0x72, 0xff);
/// The largest icon.png read (bytes, and pixels on a side).
const ICON_MAX_BYTES: u64 = 4 << 20;
const ICON_MAX_SIDE: u32 = 2048;

/// This copy of Taro: the standard one, or a provider's.
#[derive(Debug, Clone, PartialEq)]
pub struct Brand {
    provider: Option<Provider>,
    /// the provider folder
    dir: Option<PathBuf>,
    /// this program's file name (`Acme.exe`), said where Taro would say "Taro.exe"
    exe: Option<String>,
}

/// How this copy takes new Taro versions (update.rs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Updates {
    /// the standard Taro: the release's installer, over the installed Taro
    Installer,
    /// a provider's Taro: the release's plain Taro.exe in place of this one (the provider
    /// folder stays as it is)
    InPlace,
    /// a provider's Taro that never looks: the provider ships new packages
    Off,
}

static BRAND: OnceLock<Brand> = OnceLock::new();
static STANDARD: Brand = Brand::standard();

fn rgba(hex: &str, or: Rgba) -> Rgba {
    Color::parse(hex).map(|c| Rgba::rgb(c.0, c.1, c.2)).unwrap_or(or)
}

impl Brand {
    /// The standard Taro (no provider folder).
    pub const fn standard() -> Brand {
        Brand { provider: None, dir: None, exe: None }
    }

    /// A provider's Taro, from its provider (tests; [`Brand::next_to`] reads the folder).
    pub fn of(p: Provider, dir: Option<PathBuf>, exe: Option<String>) -> Brand {
        Brand { provider: Some(p), dir, exe }
    }

    /// The copy of Taro whose program is `exe`: a provider's when `provider/` is next to it.
    /// The folder without a valid `provider.json` is an error (see the module's note).
    pub fn next_to(exe: &Path) -> Result<Brand, String> {
        let Some(dir) = rm_brand::dir_next_to(exe) else { return Ok(Brand::standard()) };
        if !dir.exists() {
            return Ok(Brand::standard());
        }
        match Provider::load(&dir)? {
            Some(p) => Ok(Brand::of(p, Some(dir), exe.file_name().map(|n| n.to_string_lossy().into_owned()))),
            None => Err(format!("{} has no {}", dir.display(), rm_brand::FILE)),
        }
    }

    pub fn provider(&self) -> Option<&Provider> {
        self.provider.as_ref()
    }

    pub fn is_provider(&self) -> bool {
        self.provider.is_some()
    }

    /// The product's name: "Taro", or the provider's ("Acme Connect").
    pub fn name(&self) -> &str {
        self.provider.as_ref().map_or(TARO, |p| p.product.name.as_str())
    }

    /// The short name ("Acme"): file names, short labels.
    pub fn short_name(&self) -> &str {
        self.provider.as_ref().map_or(TARO, |p| p.product.short_name.as_str())
    }

    /// The provider's company (None: the standard Taro).
    pub fn vendor(&self) -> Option<&str> {
        self.provider.as_ref().map(|p| p.product.vendor.as_str())
    }

    /// Where the provider's customers get help: its support page, else its email.
    pub fn support(&self) -> Option<&str> {
        let p = self.provider.as_ref()?;
        p.product.support_url.as_deref().or(p.product.support_email.as_deref())
    }

    /// The accent every component draws with: Taro's violet, or the provider's primary colour
    /// (its dark-mode variant in dark mode), as rm-brand derives them.
    pub fn accent(&self, dark: bool) -> Rgba {
        match &self.provider {
            None if dark => TARO_ACCENT_DARK,
            None => TARO_ACCENT,
            Some(p) => {
                let t = p.tokens();
                rgba(if dark { &t.primary_dark } else { &t.primary }, if dark { TARO_ACCENT_DARK } else { TARO_ACCENT })
            }
        }
    }

    /// Text and symbols on the accent: white on Taro's violet; on a provider's colour white or
    /// near black, whichever reads better (a pale colour gets dark text).
    pub fn on_accent(&self, dark: bool) -> Rgba {
        match &self.provider {
            None => Rgba::WHITE,
            Some(p) => {
                let t = p.tokens();
                rgba(if dark { &t.on_primary_dark } else { &t.on_primary }, Rgba::WHITE)
            }
        }
    }

    /// How updates come to this copy.
    pub fn updates(&self) -> Updates {
        match &self.provider {
            None => Updates::Installer,
            Some(p) if p.updates.clients == ClientUpdates::Off => Updates::Off,
            Some(_) => Updates::InPlace,
        }
    }

    /// The provider's relay (`host:port`) and its key.
    pub fn relay(&self) -> Option<(String, &str)> {
        self.provider.as_ref().map(|p| (p.relay.address(), p.relay.key.as_str()))
    }

    /// The provider's icon.png, when it has one.
    pub fn icon_file(&self) -> Option<PathBuf> {
        let (p, dir) = (self.provider.as_ref()?, self.dir.as_ref()?);
        p.asset(dir, &p.branding.icon)
    }

    /// `text` with the product's name where it says "Taro" (the word alone: not "TaroDock",
    /// not "taro://"), and this program's name for "Taro.exe". The standard Taro: as it is.
    pub fn named<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let Some(p) = &self.provider else { return Cow::Borrowed(text) };
        if !text.contains(TARO) {
            return Cow::Borrowed(text);
        }
        // (a letter, a digit, or a path's slash: "Logs/Taro/" is a folder, not the product)
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '/' | '\\'));
        let mut out = String::with_capacity(text.len() + 16);
        let mut at = 0;
        while let Some(i) = text[at..].find(TARO).map(|i| i + at) {
            out.push_str(&text[at..i]);
            let after = &text[i + TARO.len()..];
            let alone = !word(text[..i].chars().next_back()) && !word(after.chars().next());
            if alone && after.starts_with(".exe") && self.exe.is_some() {
                out.push_str(self.exe.as_deref().unwrap_or_default());
                at = i + TARO.len() + 4;
                continue;
            }
            out.push_str(if alone && !after.starts_with("://") { &p.product.name } else { TARO });
            at = i + TARO.len();
        }
        out.push_str(&text[at..]);
        Cow::Owned(out)
    }

    /// The icon, `n` pixels square: Taro's own; a provider's icon.png; or, a provider without
    /// one, Taro's mark in the provider's colour.
    pub fn icon(&self, n: usize) -> Canvas {
        if self.provider.is_none() {
            return crate::logo::icon(n);
        }
        if let Some(img) = self.icon_file().and_then(|f| match read_png(&f) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("{}: {e} (the mark in the provider's colour instead)", f.display());
                None
            }
        }) {
            return fit(&img, n);
        }
        let mut c = Canvas::new(n, n);
        let pad = (n as f32 * 0.06).round();
        crate::look::mark(&mut c, pad, pad, n as f32 - 2.0 * pad, self.accent(false), self.on_accent(false));
        c
    }
}

/// This copy, read once at start from the folder next to the running program, and its relay
/// and key made the built-in ones (rm-protocol, rm-relay) when it is a provider's.
pub fn init() -> Result<&'static Brand, String> {
    let exe = std::env::current_exe().map_err(|e| format!("where this program is: {e}"))?;
    install(Brand::next_to(&exe)?)
}

/// Make `b` this process's copy (once; later calls keep the first).
pub fn install(b: Brand) -> Result<&'static Brand, String> {
    if let Some((relay, key)) = b.relay() {
        rm_protocol::session::set_builtin_relay(&relay)?;
        rm_relay::set_builtin_key(key)?;
    }
    let _ = BRAND.set(b);
    Ok(get())
}

/// This copy (the standard Taro until [`init`]).
pub fn get() -> &'static Brand {
    BRAND.get().unwrap_or(&STANDARD)
}

pub fn name() -> &'static str {
    get().name()
}

pub fn short_name() -> &'static str {
    get().short_name()
}

pub fn provider() -> Option<&'static Provider> {
    get().provider()
}

pub fn accent(dark: bool) -> Rgba {
    get().accent(dark)
}

pub fn on_accent(dark: bool) -> Rgba {
    get().on_accent(dark)
}

/// [`Brand::named`] for this copy.
pub fn named(text: &str) -> Cow<'_, str> {
    get().named(text)
}

/// [`Brand::icon`] for this copy.
pub fn icon(n: usize) -> Canvas {
    get().icon(n)
}

/// The icon as straight-alpha RGBA bytes (window and tray icons, the .ico file).
pub fn icon_rgba(n: usize) -> Vec<u8> {
    crate::logo::straight_rgba(&icon(n))
}

/// A PNG file as a canvas (premultiplied), within sane limits.
pub fn read_png(path: &Path) -> Result<Canvas, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > ICON_MAX_BYTES {
        return Err(format!("larger than {} MB", ICON_MAX_BYTES >> 20));
    }
    decode_png(&std::fs::read(path).map_err(|e| e.to_string())?)
}

/// A PNG (any colour type and depth) as a canvas.
pub fn decode_png(bytes: &[u8]) -> Result<Canvas, String> {
    let mut d = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), png::Limits { bytes: 64 << 20 });
    d.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = d.read_info().map_err(|e| format!("not a PNG ({e})"))?;
    let (w, h) = (r.info().width, r.info().height);
    if w == 0 || h == 0 || w > ICON_MAX_SIDE || h > ICON_MAX_SIDE {
        return Err(format!("{w}×{h} pixels: at most {ICON_MAX_SIDE} on a side"));
    }
    let mut buf = vec![0u8; r.output_buffer_size().ok_or("too large")?];
    let info = r.next_frame(&mut buf).map_err(|e| format!("not a readable PNG ({e})"))?;
    let px = &buf[..info.buffer_size()];
    let n = (info.width * info.height) as usize;
    let mut out = Vec::with_capacity(n * 4);
    match info.color_type {
        png::ColorType::Rgba => out.extend_from_slice(px),
        png::ColorType::Rgb => px.chunks_exact(3).for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::GrayscaleAlpha => px.chunks_exact(2).for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]])),
        png::ColorType::Grayscale => px.iter().for_each(|&v| out.extend_from_slice(&[v, v, v, 255])),
        png::ColorType::Indexed => return Err("an indexed PNG that was not expanded".into()),
    }
    if out.len() != n * 4 {
        return Err("a PNG of an unexpected layout".into());
    }
    Ok(Canvas::from_rgba(info.width as usize, info.height as usize, &out))
}

/// Half the size, each pixel the mean of four (a clean step down for a large icon).
fn halve(c: &Canvas) -> Canvas {
    let (w, h) = ((c.w / 2).max(1), (c.h / 2).max(1));
    let mut o = Canvas::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut s = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = c.px[(2 * y + dy).min(c.h - 1) * c.w + (2 * x + dx).min(c.w - 1)];
                for i in 0..4 {
                    s[i] += p[i] as u32;
                }
            }
            o.px[y * w + x] = s.map(|v| ((v + 2) / 4) as u8);
        }
    }
    o
}

/// `img` in an `n`-pixel square, its proportions kept, centred.
pub fn fit(img: &Canvas, n: usize) -> Canvas {
    let mut s = img.clone();
    while s.w >= 2 * n && s.h >= 2 * n {
        s = halve(&s);
    }
    let k = n as f32 / s.w.max(s.h).max(1) as f32;
    let (w, h) = (s.w as f32 * k, s.h as f32 * k);
    let mut c = Canvas::new(n, n);
    c.draw(&s, (n as f32 - w) / 2.0, (n as f32 - h) / 2.0, w, h, 0.0, 1.0);
    c
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Acme Connect's provider.json, as Wizard 1 writes it (`relay` and `colour` vary).
    pub(crate) fn acme_json(colour: &str, updates: &str) -> String {
        format!(
            r##"{{"schema":"taro.provider/v1","provider_id":"acme-cloud",
              "product":{{"name":"Acme Connect","short_name":"Acme","vendor":"Acme Cloud Ltd","support_url":"https://acme.example/help"}},
              "branding":{{"primary_color":"{colour}","icon":"icon.png"}},
              "relay":{{"host":"relay.acme.example","port":7471,"key":"test-key-bbbbbbbbbbbbbbbb"}},
              "updates":{{"clients":"{updates}"}}}}"##
        )
    }

    pub(crate) fn acme(colour: &str, updates: &str) -> Brand {
        Brand::of(Provider::from_json(&acme_json(colour, updates)).unwrap(), None, Some("Acme.exe".into()))
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rm-viewer-brand-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An opaque pixel (BGRA) close to `c`.
    fn near(p: [u8; 4], c: Rgba) -> bool {
        let d = |v: u8, f: f32| (v as f32 - f * 255.0).abs() < 12.0;
        p[3] == 255 && d(p[2], c.r) && d(p[1], c.g) && d(p[0], c.b)
    }

    fn png_bytes(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut wr = e.write_header().unwrap();
            wr.write_image_data(&rgba.repeat((w * h) as usize)).unwrap();
        }
        out
    }

    #[test]
    fn no_provider_folder_is_the_standard_taro() {
        let d = temp("none");
        let b = Brand::next_to(&d.join("Taro.exe")).unwrap();
        assert_eq!(b, Brand::standard());
        assert_eq!((b.name(), b.short_name(), b.vendor(), b.support()), ("Taro", "Taro", None, None));
        assert_eq!(b.accent(false), Rgba::rgb(0x7c, 0x4a, 0xe8));
        assert_eq!(b.accent(true), Rgba::rgb(0xa0, 0x72, 0xff));
        assert_eq!(b.on_accent(false), Rgba::WHITE);
        assert_eq!(b.updates(), Updates::Installer);
        assert_eq!(b.relay(), None);
        assert_eq!(b.named("Taro is up to date. Start Taro.exe"), "Taro is up to date. Start Taro.exe");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_valid_provider_gives_its_name_colour_relay_and_key() {
        let d = temp("valid");
        std::fs::create_dir_all(d.join("provider")).unwrap();
        std::fs::write(d.join("provider").join("provider.json"), acme_json("#0F766E", "notify")).unwrap();
        let b = Brand::next_to(&d.join("Acme.exe")).unwrap();
        assert!(b.is_provider());
        assert_eq!((b.name(), b.short_name(), b.vendor(), b.support()), ("Acme Connect", "Acme", Some("Acme Cloud Ltd"), Some("https://acme.example/help")));
        assert_eq!(b.relay(), Some(("relay.acme.example:7471".to_string(), "test-key-bbbbbbbbbbbbbbbb")));
        assert_eq!(b.updates(), Updates::InPlace);
        // the colour as rm-brand derives it: the primary in light mode, its dark variant in dark
        let t = b.provider().unwrap().tokens();
        assert_eq!(b.accent(false), rgba(&t.primary, Rgba::BLACK));
        assert_eq!(b.accent(false), Rgba::rgb(0x0f, 0x76, 0x6e));
        assert_eq!(b.accent(true), rgba(&t.primary_dark, Rgba::BLACK));
        assert_ne!(b.accent(true), b.accent(false), "lighter in dark mode");
        assert_eq!(b.on_accent(false), Rgba::WHITE);
        // no icon.png in the folder: the mark in the provider's colour
        let icon = b.icon(64);
        assert!(near(icon.get(32, 6), b.accent(false)), "the mark's colour at its top: {:?}", icon.get(32, 6));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_invalid_or_missing_provider_json_is_an_error() {
        let d = temp("invalid");
        std::fs::create_dir_all(d.join("provider")).unwrap();
        // the folder without the file: not the standard Taro (it would use Taro's relay)
        let e = Brand::next_to(&d.join("Acme.exe")).unwrap_err();
        assert!(e.contains("provider.json"), "{e}");
        std::fs::write(d.join("provider").join("provider.json"), "{ not json").unwrap();
        assert!(Brand::next_to(&d.join("Acme.exe")).is_err());
        // valid JSON, a wrong field: named
        std::fs::write(d.join("provider").join("provider.json"), acme_json("#0F766E", "notify").replace("test-key-bbbbbbbbbbbbbbbb", "short")).unwrap();
        let e = Brand::next_to(&d.join("Acme.exe")).unwrap_err();
        assert!(e.contains("relay.key"), "{e}");
        std::fs::write(d.join("provider").join("provider.json"), acme_json("purple", "notify")).unwrap();
        assert!(Brand::next_to(&d.join("Acme.exe")).unwrap_err().contains("branding.primary_color"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_pale_colour_gets_dark_text_and_updates_can_be_off() {
        let b = acme("#FDE047", "off");
        assert_eq!(b.on_accent(false), Rgba::rgb(0x11, 0x11, 0x11));
        assert_eq!(b.updates(), Updates::Off);
        // a mid colour: white
        assert_eq!(acme("#2563EB", "notify").on_accent(false), Rgba::WHITE);
    }

    #[test]
    fn the_product_name_takes_taros_place_in_words_only() {
        let b = acme("#0F766E", "notify");
        assert_eq!(b.named("Taro is up to date."), "Acme Connect is up to date.");
        assert_eq!(b.named("About this Taro"), "About this Acme Connect");
        assert_eq!(b.named("Updates come from Taro's releases"), "Updates come from Acme Connect's releases");
        assert_eq!(b.named("start Taro.exe with --logs-enabled"), "start Acme.exe with --logs-enabled");
        // identifiers, links and lower-case file names stay
        assert_eq!(b.named("TaroDock"), "TaroDock");
        assert_eq!(b.named("a taro:// link, ./taro-update.sh"), "a taro:// link, ./taro-update.sh");
        assert_eq!(b.named("Taro://x"), "Taro://x");
        assert_eq!(b.named("the log: ~/Library/Logs/Taro/taro.log, %LOCALAPPDATA%\\Taro\\crash.log"), "the log: ~/Library/Logs/Taro/taro.log, %LOCALAPPDATA%\\Taro\\crash.log");
        assert_eq!(b.named("no name here"), "no name here");
        assert!(matches!(b.named("no name here"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_providers_icon_png_is_read_and_scaled() {
        let d = temp("icon");
        std::fs::create_dir_all(d.join("provider")).unwrap();
        std::fs::write(d.join("provider").join("provider.json"), acme_json("#0F766E", "notify")).unwrap();
        std::fs::write(d.join("provider").join("icon.png"), png_bytes(256, 256, [255, 0, 0, 255])).unwrap();
        let b = Brand::next_to(&d.join("Acme.exe")).unwrap();
        assert!(b.icon_file().is_some());
        let icon = b.icon(32);
        assert_eq!((icon.w, icon.h), (32, 32));
        // red, opaque (BGRA, premultiplied)
        assert_eq!(icon.get(16, 16), [0, 0, 255, 255]);
        // a broken icon.png: the mark instead, no failure
        std::fs::write(d.join("provider").join("icon.png"), b"not a png").unwrap();
        let icon = b.icon(32);
        assert!(near(icon.get(16, 3), b.accent(false)), "{:?}", icon.get(16, 3));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pngs_of_each_kind_decode() {
        let c = decode_png(&png_bytes(3, 2, [10, 20, 30, 255])).unwrap();
        assert_eq!((c.w, c.h), (3, 2));
        assert_eq!(c.get(1, 1), [30, 20, 10, 255]);
        assert!(decode_png(b"\x89PNG\r\n\x1a\nbroken").is_err());
        // a tall image fits its square, centred
        let tall = Canvas::filled(10, 40, Rgba::rgb(0, 0, 255));
        let f = fit(&tall, 16);
        assert_eq!(f.get(8, 8)[3], 255);
        assert_eq!(f.get(0, 8)[3], 0, "transparent beside it");
    }
}
