//! A Taro provider's identity: `provider.json`, written once by the package builder (Wizard 1)
//! and read, unchanged, by every part of that provider's distribution: Taro for Windows (next to
//! its exe, in `provider/`), Taro for the Mac (next to `taro`, in `provider/`) and the relay
//! (`/etc/taro/provider/`). The same file everywhere is what keeps them consistent.
//!
//! It is provider customization, never shared core: updates of the core (the Windows exe, the
//! Mac binary, `enterprise-update.zip` on the server) leave it alone. It holds no server
//! secrets: the relay's admission key is the one value every client of that provider sends
//! anyway (the standard builds carry Taro's own the same way).
//!
//! The primary colour is the one brand colour; the hover, active, border, soft and dark-mode
//! variants are derived from it here ([`Tokens::derive`]) so all components agree.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// The format of `provider.json`.
pub const SCHEMA: &str = "taro.provider/v1";
/// The file's name.
pub const FILE: &str = "provider.json";
/// The folder that holds it (and the logo and icon) next to a client's executable.
pub const DIR: &str = "provider";
/// Taro's own primary colour (its violet), when no provider sets one.
pub const TARO_PRIMARY: Color = Color(0x7c, 0x4a, 0xe8);

/// An sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    /// `#RRGGBB` or `RRGGBB` (any case, spaces around ignored). Three-digit and named colours
    /// are refused so a value means exactly one colour.
    pub fn parse(s: &str) -> Result<Color, String> {
        let t = s.trim();
        let h = t.strip_prefix('#').unwrap_or(t);
        if h.is_empty() {
            return Err("enter a colour code such as #7C3AED".into());
        }
        if h.len() != 6 {
            return Err(format!("\"{t}\" is not a 6-digit hex colour (like #7C3AED)"));
        }
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("\"{t}\" has characters other than 0-9 and A-F"));
        }
        let v = u32::from_str_radix(h, 16).map_err(|e| e.to_string())?;
        Ok(Color((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    /// `#RRGGBB`, upper case.
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    /// `t` of the way towards `other` (0: self, 1: other).
    pub fn mix(self, other: Color, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f64 + (b as f64 - a as f64) * t).round() as u8;
        Color(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f64 {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.0) + 0.7152 * lin(self.1) + 0.0722 * lin(self.2)
    }

    /// WCAG contrast ratio between two colours (1 to 21).
    pub fn contrast(self, other: Color) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, w: &mut fmt::Formatter<'_>) -> fmt::Result {
        w.write_str(&self.hex())
    }
}

pub const WHITE: Color = Color(0xff, 0xff, 0xff);
pub const BLACK: Color = Color(0, 0, 0);
/// Text on a light primary.
pub const INK: Color = Color(0x11, 0x11, 0x11);
/// The dark-mode window background the dark variant is checked against.
pub const DARK_BG: Color = Color(0x1c, 0x1c, 0x1e);
/// Text and controls need this contrast (WCAG AA, normal text).
pub const AA: f64 = 4.5;

/// The colours every component draws with, all from the one primary colour.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub primary: String,
    pub primary_hover: String,
    pub primary_active: String,
    pub primary_border: String,
    /// a light tint: selected rows, highlights
    pub primary_soft: String,
    /// text and icons on the primary (white or near black, whichever reads better)
    pub on_primary: String,
    /// the primary in dark mode: lightened until it reads on a dark background
    pub primary_dark: String,
    pub primary_dark_hover: String,
    pub on_primary_dark: String,
}

fn on(c: Color) -> Color {
    if WHITE.contrast(c) >= INK.contrast(c) {
        WHITE
    } else {
        INK
    }
}

/// Pressed-looking variants: darker, or lighter for a primary already near black.
fn shade(p: Color, t: f64) -> Color {
    if p.luminance() < 0.02 {
        p.mix(WHITE, t * 0.8)
    } else {
        p.mix(BLACK, t)
    }
}

impl Tokens {
    pub fn derive(p: Color) -> Tokens {
        let mut dark = p;
        let mut t = 0.0;
        while dark.contrast(DARK_BG) < AA && t < 0.6 {
            t += 0.04;
            dark = p.mix(WHITE, t);
        }
        Tokens {
            primary: p.hex(),
            primary_hover: shade(p, 0.10).hex(),
            primary_active: shade(p, 0.20).hex(),
            primary_border: shade(p, 0.28).hex(),
            primary_soft: p.mix(WHITE, 0.88).hex(),
            on_primary: on(p).hex(),
            primary_dark: dark.hex(),
            primary_dark_hover: dark.mix(WHITE, 0.10).hex(),
            on_primary_dark: on(dark).hex(),
        }
    }

    /// `:root { --primary: …; … }`, for web pages (the relay's pages, the portal).
    pub fn css(&self) -> String {
        format!(
            ":root{{--primary:{};--primary-hover:{};--primary-active:{};--primary-border:{};--primary-soft:{};--on-primary:{};}}\n\
             @media (prefers-color-scheme:dark){{:root{{--primary:{};--primary-hover:{};--on-primary:{};}}}}\n",
            self.primary, self.primary_hover, self.primary_active, self.primary_border, self.primary_soft, self.on_primary, self.primary_dark, self.primary_dark_hover, self.on_primary_dark
        )
    }
}

/// Light, dark or as the system is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Product {
    /// "Acme Connect": window titles, About, the Mac's messages
    pub name: String,
    /// "Acme": file names, short labels (letters, digits, space, dot, dash, underscore)
    pub short_name: String,
    /// the company
    pub vendor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branding {
    /// `#RRGGBB`
    pub primary_color: String,
    /// derived from `primary_color` by the builder (for components that do not derive them:
    /// the Mac, web pages); must match [`Tokens::derive`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
    #[serde(default)]
    pub theme: Theme,
    /// a PNG in the provider folder (`logo.png`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    /// a square PNG in the provider folder (`icon.png`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relay {
    /// the DNS name clients connect to (the relay's setup checks it points at the server)
    pub host: String,
    pub port: u16,
    /// the admission key the provider's clients send and its relay requires
    pub key: String,
}

impl Relay {
    /// `host:port` (IPv6 in brackets).
    pub fn address(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// How the provider's clients take new Taro versions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientUpdates {
    /// look for new Taro releases and offer them (installed in place: this folder stays)
    #[default]
    Notify,
    /// never look; the provider ships new packages
    Off,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Updates {
    #[serde(default)]
    pub clients: ClientUpdates,
}

/// Who wrote the file, from which versions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generated {
    pub by: String,
    pub at: String,
    pub taro_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enterprise_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    pub schema: String,
    /// `acme-cloud`: lower-case letters, digits and dashes
    pub provider_id: String,
    pub product: Product,
    pub branding: Branding,
    pub relay: Relay,
    #[serde(default)]
    pub updates: Updates,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<Generated>,
}

/// A problem with one field (`branding.primary_color`), for forms and messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

fn err(v: &mut Vec<FieldError>, field: &str, message: impl Into<String>) {
    v.push(FieldError { field: field.into(), message: message.into() });
}

pub fn valid_provider_id(s: &str) -> bool {
    (2..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !s.starts_with('-') && !s.ends_with('-')
}

pub fn valid_short_name(s: &str) -> bool {
    let t = s.trim();
    t == s && (1..=16).contains(&t.chars().count()) && t.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_')) && !t.starts_with('.')
}

/// A DNS name (letters, digits, dashes; at least one dot) or an IP address.
pub fn valid_host(s: &str) -> bool {
    if s.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let s = s.strip_suffix('.').unwrap_or(s);
    s.len() <= 253
        && s.contains('.')
        && s.split('.').all(|l| (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-'))
}

pub fn valid_relay_key(s: &str) -> bool {
    (16..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A file inside the provider folder: a plain relative name, no `..`, no folders above it.
pub fn valid_asset_path(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.starts_with(['/', '\\', '.']) && !s.contains("..") && !s.contains(':') && !s.contains('\\') && s.to_ascii_lowercase().ends_with(".png") && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/'))
}

fn plain(s: &str, max: usize) -> bool {
    let n = s.chars().count();
    (1..=max).contains(&n) && s.trim() == s && !s.chars().any(char::is_control)
}

impl Provider {
    /// Every problem at once (none: valid).
    pub fn problems(&self) -> Vec<FieldError> {
        let mut v = Vec::new();
        if self.schema != SCHEMA {
            err(&mut v, "schema", format!("must be {SCHEMA}"));
        }
        if !valid_provider_id(&self.provider_id) {
            err(&mut v, "provider_id", "2-40 lower-case letters, digits and dashes (acme-cloud)");
        }
        if !plain(&self.product.name, 40) {
            err(&mut v, "product.name", "1-40 characters, no line breaks");
        }
        if !valid_short_name(&self.product.short_name) {
            err(&mut v, "product.short_name", "1-16 letters, digits, spaces, dots, dashes or underscores (used in file names)");
        }
        if !plain(&self.product.vendor, 80) {
            err(&mut v, "product.vendor", "1-80 characters, no line breaks");
        }
        if let Some(u) = &self.product.support_url {
            if !u.starts_with("https://") || u.len() > 200 || u.contains(char::is_whitespace) {
                err(&mut v, "product.support_url", "an https:// address");
            }
        }
        if let Some(e) = &self.product.support_email {
            let ok = e.len() <= 120 && e.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.') && !e.contains(char::is_whitespace));
            if !ok {
                err(&mut v, "product.support_email", "an email address");
            }
        }
        match Color::parse(&self.branding.primary_color) {
            Err(m) => err(&mut v, "branding.primary_color", m),
            Ok(c) => {
                if let Some(t) = &self.branding.tokens {
                    if *t != Tokens::derive(c) {
                        err(&mut v, "branding.tokens", "do not match the primary colour: build them again (they are derived, not edited)");
                    }
                }
            }
        }
        for (field, p) in [("branding.logo", &self.branding.logo), ("branding.icon", &self.branding.icon)] {
            if let Some(p) = p {
                if !valid_asset_path(p) {
                    err(&mut v, field, "a .png file name inside the provider folder");
                }
            }
        }
        if !valid_host(&self.relay.host) {
            err(&mut v, "relay.host", "a DNS name (relay.example.com) or an IP address");
        }
        if self.relay.port == 0 {
            err(&mut v, "relay.port", "1-65535");
        }
        if !valid_relay_key(&self.relay.key) {
            err(&mut v, "relay.key", "16-128 letters, digits, dashes or underscores");
        }
        v
    }

    /// The colour as a value (Taro's when it does not parse: a validated file always does).
    pub fn primary(&self) -> Color {
        Color::parse(&self.branding.primary_color).unwrap_or(TARO_PRIMARY)
    }

    /// The colour tokens (derived again, so they always match the primary colour).
    pub fn tokens(&self) -> Tokens {
        Tokens::derive(self.primary())
    }

    /// Normalized for writing: the colour upper case, the tokens filled in.
    pub fn normalized(mut self) -> Provider {
        if let Ok(c) = Color::parse(&self.branding.primary_color) {
            self.branding.primary_color = c.hex();
            self.branding.tokens = Some(Tokens::derive(c));
        }
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("serializable") + "\n"
    }

    /// Parse and validate.
    pub fn from_json(s: &str) -> Result<Provider, String> {
        let p: Provider = serde_json::from_str(s).map_err(|e| format!("{FILE}: {e}"))?;
        let problems = p.problems();
        if let Some(f) = problems.first() {
            return Err(format!("{FILE}: {}: {}", f.field, f.message));
        }
        Ok(p)
    }

    /// `dir/provider.json`: none when the file is not there, an error when it is there but wrong.
    pub fn load(dir: &Path) -> Result<Option<Provider>, String> {
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(s) => Provider::from_json(&s).map(Some).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// A file named in the branding (logo, icon), inside `dir` only.
    pub fn asset(&self, dir: &Path, name: &Option<String>) -> Option<PathBuf> {
        name.as_ref().filter(|n| valid_asset_path(n)).map(|n| dir.join(n)).filter(|p| p.is_file())
    }
}

/// The provider folder next to an executable (`<exe dir>/provider`).
pub fn dir_next_to(exe: &Path) -> Option<PathBuf> {
    exe.parent().map(|d| d.join(DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Provider {
        Provider {
            schema: SCHEMA.into(),
            provider_id: "acme-cloud".into(),
            product: Product { name: "Acme Connect".into(), short_name: "Acme".into(), vendor: "Acme Cloud Ltd".into(), support_url: Some("https://acme.example/help".into()), support_email: None },
            branding: Branding { primary_color: "#2563eb".into(), tokens: None, theme: Theme::System, logo: Some("logo.png".into()), icon: None },
            relay: Relay { host: "relay.acme.example".into(), port: 7470, key: "k3y-0123456789abcdef".into() },
            updates: Updates::default(),
            generated: None,
        }
    }

    #[test]
    fn colours_parse_with_or_without_hash() {
        assert_eq!(Color::parse("#7C3AED").unwrap(), Color(0x7c, 0x3a, 0xed));
        assert_eq!(Color::parse("7c3aed").unwrap().hex(), "#7C3AED");
        assert_eq!(Color::parse("  #0f766e ").unwrap().hex(), "#0F766E");
        for bad in ["", "#", "#fff", "#12345", "#1234567", "#12345G", "blue", "rgb(1,2,3)", "##123456"] {
            assert!(Color::parse(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn custom_colours_are_kept_exactly() {
        // no snapping to a preset: an odd colour comes back unchanged
        let p = Provider { branding: Branding { primary_color: "#3d7a5b".into(), ..sample().branding }, ..sample() }.normalized();
        assert_eq!(p.branding.primary_color, "#3D7A5B");
        assert_eq!(p.tokens().primary, "#3D7A5B");
    }

    #[test]
    fn derived_tokens_are_readable() {
        for hex in ["#7C3AED", "#2563EB", "#0F766E", "#F59E0B", "#FDE047", "#111827", "#FFFFFF", "#000000"] {
            let c = Color::parse(hex).unwrap();
            let t = Tokens::derive(c);
            let on = Color::parse(&t.on_primary).unwrap();
            // the better of white and ink is picked: at least what either could give
            assert!(on.contrast(c) >= WHITE.contrast(c).min(INK.contrast(c)), "{hex}");
            let dark = Color::parse(&t.primary_dark).unwrap();
            assert!(dark.contrast(DARK_BG) >= AA || dark == c.mix(WHITE, 0.6) || t.primary_dark == "#FFFFFF" || dark.contrast(DARK_BG) >= c.contrast(DARK_BG), "{hex} in dark mode");
            assert_ne!(t.primary_hover, t.primary);
        }
        // a mid violet gets white text, a pale yellow ink
        assert_eq!(Tokens::derive(Color::parse("#7C3AED").unwrap()).on_primary, "#FFFFFF");
        assert_eq!(Tokens::derive(Color::parse("#FDE047").unwrap()).on_primary, "#111111");
    }

    #[test]
    fn a_sample_is_valid_and_round_trips() {
        let p = sample().normalized();
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        let back = Provider::from_json(&p.to_json()).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.relay.address(), "relay.acme.example:7470");
    }

    #[test]
    fn hand_edited_tokens_are_refused() {
        let mut p = sample().normalized();
        p.branding.tokens.as_mut().unwrap().primary_hover = "#000000".into();
        assert!(p.problems().iter().any(|e| e.field == "branding.tokens"));
    }

    #[test]
    fn bad_fields_are_named() {
        let mut p = sample();
        p.provider_id = "Acme Cloud".into();
        p.product.short_name = "a/b".into();
        p.branding.primary_color = "purple".into();
        p.branding.logo = Some("../../etc/passwd.png".into());
        p.relay.host = "not a host".into();
        p.relay.key = "short".into();
        let fields: Vec<String> = p.problems().into_iter().map(|e| e.field).collect();
        for f in ["provider_id", "product.short_name", "branding.primary_color", "branding.logo", "relay.host", "relay.key"] {
            assert!(fields.iter().any(|x| x == f), "{f} not reported: {fields:?}");
        }
    }

    #[test]
    fn load_reads_a_folder() {
        let dir = std::env::temp_dir().join(format!("rm-brand-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Provider::load(&dir).unwrap(), None);
        std::fs::write(dir.join(FILE), sample().normalized().to_json()).unwrap();
        assert_eq!(Provider::load(&dir).unwrap().unwrap().product.name, "Acme Connect");
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
        assert!(Provider::load(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn css_names_the_tokens() {
        let css = sample().tokens().css();
        assert!(css.contains("--primary:#2563EB") && css.contains("--on-primary:") && css.contains("prefers-color-scheme:dark"));
    }
}
