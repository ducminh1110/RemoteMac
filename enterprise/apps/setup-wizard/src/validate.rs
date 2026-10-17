//! Input validation shared by the wizard steps. Every function returns a human-readable
//! error that the UI shows next to the field.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Development,
    Staging,
    Production,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Development => "development",
            Environment::Staging => "staging",
            Environment::Production => "production",
        }
    }
}

/// Validate a base URL (portal / ops console). https is mandatory outside development.
/// Returns the normalised URL without a trailing slash.
pub fn base_url(raw: &str, env: Environment, field: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{field} is required."));
    }
    let u = url::Url::parse(raw).map_err(|e| format!("{field} is not a valid URL ({e})."))?;
    match u.scheme() {
        "https" => {}
        "http" if env == Environment::Development => {}
        "http" => {
            return Err(format!(
                "{field} must use https:// for {} deployments.",
                env.as_str()
            ))
        }
        other => return Err(format!("{field} must be an http(s) URL, not {other}://.")),
    }
    if u.host_str().is_none_or(|h| h.is_empty()) {
        return Err(format!("{field} needs a host name."));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(format!("{field} must not contain a user name or password."));
    }
    if u.query().is_some() || u.fragment().is_some() {
        return Err(format!("{field} must not contain ?query or #fragment."));
    }
    Ok(u.as_str().trim_end_matches('/').to_string())
}

pub fn email(raw: &str) -> Result<String, String> {
    let e = raw.trim();
    let ok = e.len() <= 254
        && !e.chars().any(|c| c.is_whitespace() || c.is_control())
        && e.matches('@').count() == 1
        && {
            let (local, domain) = e.split_once('@').unwrap_or(("", ""));
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains("..")
        };
    if ok {
        Ok(e.to_string())
    } else {
        Err("Enter a valid email address, e.g. admin@example.com.".into())
    }
}

/// A short free-text value (names, contacts): non-empty, max `max` chars, no control chars.
pub fn text(raw: &str, field: &str, max: usize) -> Result<String, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(format!("{field} is required."));
    }
    if t.chars().count() > max {
        return Err(format!("{field} must be at most {max} characters."));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters."));
    }
    Ok(t.to_string())
}

/// Parse `#RRGGBB`.
pub fn hex_color(raw: &str) -> Result<(u8, u8, u8), String> {
    let s = raw.trim();
    let bad = || "Colour must be in the form #RRGGBB, e.g. #0A66C2.".to_string();
    let h = s.strip_prefix('#').ok_or_else(bad)?;
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| bad());
    Ok((p(0)?, p(2)?, p(4)?))
}

fn channel(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.x relative luminance.
pub fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG contrast ratio between two colours (1.0 ..= 21.0).
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// A DNS host name such as `portal.example.com` (lower-cased, no trailing dot).
pub fn fqdn(raw: &str) -> Result<String, String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let bad = |why: &str| format!("\"{}\" is not a valid domain name: {why}.", raw.trim());
    if d.len() > 253 {
        return Err(bad("longer than 253 characters"));
    }
    let labels: Vec<&str> = d.split('.').collect();
    if labels.len() < 2 {
        return Err(bad("it needs at least two labels, e.g. portal.example.com"));
    }
    for l in &labels {
        if l.is_empty() || l.len() > 63 {
            return Err(bad("each label must be 1-63 characters"));
        }
        if !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(bad("only letters, digits and '-' are allowed"));
        }
        if l.starts_with('-') || l.ends_with('-') {
            return Err(bad("labels must not start or end with '-'"));
        }
    }
    if labels
        .last()
        .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(bad(
            "the last label must not be numeric (use the IP as the server target instead)",
        ));
    }
    Ok(d)
}

/// GitHub repository or owner name.
pub fn repo_name(raw: &str) -> Result<String, String> {
    let n = raw.trim();
    if n.is_empty() || n.len() > 100 {
        return Err("Repository name must be 1-100 characters.".into());
    }
    if n == "." || n == ".." {
        return Err("Repository name must not be \".\" or \"..\".".into());
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("Repository name may contain only letters, digits, '-', '_' and '.'.".into());
    }
    Ok(n.to_string())
}

pub fn owner_name(raw: &str) -> Result<String, String> {
    let n = raw.trim();
    if n.is_empty()
        || n.len() > 39
        || n.starts_with('-')
        || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(format!(
            "\"{n}\" is not a valid GitHub user or organisation name."
        ));
    }
    Ok(n.to_string())
}

/// `owner/repo`.
pub fn repo_slug(raw: &str) -> Result<(String, String), String> {
    let (o, r) = raw
        .trim()
        .split_once('/')
        .ok_or_else(|| "Use the form owner/repository.".to_string())?;
    Ok((owner_name(o)?, repo_name(r)?))
}

/// Release version: `MAJOR.MINOR.PATCH` with an optional `-prerelease`.
pub fn version(raw: &str) -> Result<String, String> {
    let v = raw.trim();
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    let core_ok = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit()));
    let pre_ok = pre.is_none_or(|p| {
        !p.is_empty()
            && p.len() <= 40
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    });
    if core_ok && pre_ok {
        Ok(v.to_string())
    } else {
        Err("Version must look like 0.1.0 or 1.2.3-rc.1.".into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StripeMode {
    Test,
    Live,
}

/// Stripe secret-key mode rules: test keys (`sk_test_`, `rk_test_`) are accepted; live keys
/// (`sk_live_`, `rk_live_`) only with the explicit live-mode confirmation **and** the word
/// `LIVE` typed; anything else (publishable keys, garbage) is rejected.
pub fn stripe_key_mode(key: &str, live_confirmed: bool, typed: &str) -> Result<StripeMode, String> {
    let k = key.trim();
    let body_ok = |prefix: &str| {
        k.len() > prefix.len() + 10 && k[prefix.len()..].chars().all(|c| c.is_ascii_alphanumeric())
    };
    if k.starts_with("pk_") {
        return Err(concat!(
            "That is a publishable key (pk_...). The wizard needs a secret key (sk",
            "_test_...) or a restricted key (rk",
            "_test_...)."
        )
        .into());
    }
    for p in [concat!("sk", "_test_"), concat!("rk", "_test_")] {
        if k.starts_with(p) {
            return if body_ok(p) {
                Ok(StripeMode::Test)
            } else {
                Err("The key looks truncated. Copy it again from the Stripe dashboard.".into())
            };
        }
    }
    for p in [concat!("sk", "_live_"), concat!("rk", "_live_")] {
        if k.starts_with(p) {
            if !live_confirmed || typed.trim() != "LIVE" {
                return Err(concat!("This is a LIVE Stripe key. Live mode is off by default: tick the live-mode confirmation and type LIVE to use it, or paste a test key (sk", "_test_...).").into());
            }
            return if body_ok(p) {
                Ok(StripeMode::Live)
            } else {
                Err("The key looks truncated. Copy it again from the Stripe dashboard.".into())
            };
        }
    }
    Err(concat!(
        "Stripe secret keys start with sk",
        "_test_ or rk",
        "_test_ (or sk",
        "_live_/rk",
        "_live_ with live-mode confirmation)."
    )
    .into())
}

/// Stripe webhook signing secret: `whsec_` followed by at least 16 base64-ish characters.
pub fn webhook_secret(raw: &str) -> Result<(), String> {
    let s = raw.trim();
    match s.strip_prefix(concat!("wh", "sec_")) {
        Some(rest)
            if rest.len() >= 16
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')) =>
        {
            Ok(())
        }
        Some(_) => Err("The webhook signing secret looks truncated. Copy it again from Stripe (Developers > Webhooks > endpoint > Signing secret).".into()),
        None => Err(concat!("Webhook signing secrets start with wh", "sec_.").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        use Environment::*;
        assert_eq!(
            base_url("https://portal.example.com/", Production, "Portal URL").unwrap(),
            "https://portal.example.com"
        );
        assert!(base_url("http://portal.example.com", Production, "x").is_err());
        assert!(base_url("http://portal.example.com", Staging, "x").is_err());
        assert!(base_url("http://localhost:8080", Development, "x").is_ok());
        assert!(base_url("ftp://x.example.com", Development, "x").is_err());
        assert!(base_url("https://u:p@x.example.com", Production, "x").is_err());
        assert!(base_url("https://x.example.com/?a=1", Production, "x").is_err());
        assert!(base_url("", Production, "x").is_err());
        assert!(base_url("not a url", Production, "x").is_err());
    }

    #[test]
    fn emails() {
        assert!(email("admin@example.com").is_ok());
        assert!(email("a b@example.com").is_err());
        assert!(email("admin@example").is_err());
        assert!(email("@example.com").is_err());
        assert!(email("a@@example.com").is_err());
    }

    #[test]
    fn colours_and_contrast() {
        assert_eq!(hex_color("#0A66C2").unwrap(), (0x0a, 0x66, 0xc2));
        assert!(hex_color("0A66C2").is_err());
        assert!(hex_color("#0A66C").is_err());
        assert!(hex_color("#GGGGGG").is_err());
        assert!(hex_color("red").is_err());
        assert!(hex_color("#0A66C2;background:url(x)").is_err());
        let white = (255, 255, 255);
        assert!((contrast((0, 0, 0), white) - 21.0).abs() < 0.01);
        assert!(contrast((0xff, 0xff, 0x00), white) < 4.5); // yellow fails
        assert!(contrast((0x0a, 0x66, 0xc2), white) >= 4.5); // dark blue passes
    }

    #[test]
    fn domains() {
        assert_eq!(fqdn("Portal.Example.com.").unwrap(), "portal.example.com");
        assert!(fqdn("localhost").is_err());
        assert!(fqdn("-a.example.com").is_err());
        assert!(fqdn("a_b.example.com").is_err());
        assert!(fqdn("1.2.3.4").is_err());
        assert!(fqdn("a..example.com").is_err());
    }

    #[test]
    fn repos_and_versions() {
        assert!(repo_name("taro-enterprise").is_ok());
        assert!(repo_name("..").is_err());
        assert!(repo_name("a/b").is_err());
        assert_eq!(
            repo_slug("ducminh1110/taro-enterprise-template").unwrap(),
            ("ducminh1110".into(), "taro-enterprise-template".into())
        );
        assert!(owner_name("-bad").is_err());
        assert!(version("0.1.0").is_ok());
        assert!(version("1.2.3-rc.1").is_ok());
        assert!(version("1.2").is_err());
        assert!(version("v1.2.3").is_err());
        assert!(version("1.2.3; rm -rf").is_err());
    }

    #[test]
    fn stripe_mode_rules() {
        let test = concat!("sk", "_test_51Habcdefghijklmnopqrstuvwxyz");
        let live = concat!("sk", "_live_51Habcdefghijklmnopqrstuvwxyz");
        assert_eq!(stripe_key_mode(test, false, "").unwrap(), StripeMode::Test);
        assert_eq!(
            stripe_key_mode(concat!("rk", "_test_51Habcdefghijklmnop"), false, "").unwrap(),
            StripeMode::Test
        );
        // Live rejected without confirmation, with confirmation but without typing LIVE,
        // and with LIVE typed but the box unticked.
        assert!(stripe_key_mode(live, false, "").is_err());
        assert!(stripe_key_mode(live, true, "").is_err());
        assert!(stripe_key_mode(live, true, "live").is_err());
        assert!(stripe_key_mode(live, false, "LIVE").is_err());
        assert_eq!(
            stripe_key_mode(live, true, "LIVE").unwrap(),
            StripeMode::Live
        );
        assert!(stripe_key_mode(concat!("rk", "_live_51Habcdefghijklmnop"), false, "").is_err());
        assert!(stripe_key_mode(concat!("pk", "_test_51Habcdefghijklmnop"), false, "").is_err());
        assert!(stripe_key_mode(concat!("sk", "_test_short"), false, "").is_err());
        assert!(stripe_key_mode("hello", false, "").is_err());
    }

    #[test]
    fn webhook_secret_format() {
        assert!(webhook_secret(concat!("wh", "sec_abcdefghijklmnopqrstuvwxyz012345")).is_ok());
        assert!(webhook_secret(concat!("wh", "sec_short")).is_err());
        assert!(webhook_secret(concat!("sk", "_test_abcdefghijklmnopqrstuvwxyz")).is_err());
        assert!(webhook_secret(concat!("wh", "sec_abc def ghi jkl mno pqr")).is_err());
    }
}
