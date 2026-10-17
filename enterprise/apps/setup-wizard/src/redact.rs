//! Redaction and the in-memory diagnostic log.
//!
//! Two layers protect against leaking credentials:
//! 1. [`redact`] replaces anything shaped like a token (GitHub, Stripe, webhook secrets,
//!    `Bearer ...`, `access_token=...`).
//! 2. [`DiagLog`] additionally knows every secret the wizard currently holds and replaces exact
//!    occurrences, so even an unusual-looking credential never reaches the log or a response.

use crate::secret::Secret;
use regex::Regex;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const REDACTED: &str = "[REDACTED]";

fn patterns() -> &'static [Regex] {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // GitHub tokens: classic PAT, OAuth, user-to-server, server-to-server, refresh.
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{8,}",
            r"\bgithub_pat_[A-Za-z0-9_]{8,}",
            // Stripe secret / restricted keys and webhook signing secrets.
            r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{6,}",
            r"\bwhsec_[A-Za-z0-9+/=]{6,}",
            // Authorization header values.
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{12,}",
            r#"(?i)\bauthorization:\s*\S+"#,
            // OAuth / device-flow fields in query strings or JSON.
            r#"(?i)(access_token|refresh_token|device_code|client_secret)(["']?\s*[:=]\s*["']?)[^"'&\s,}]+"#,
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid redaction pattern"))
        .collect()
    })
}

/// Replace anything that looks like a credential with `[REDACTED]`.
pub fn redact(input: &str) -> String {
    let mut out = input.to_string();
    for (i, re) in patterns().iter().enumerate() {
        out = if i == patterns().len() - 1 {
            re.replace_all(&out, format!("${{1}}${{2}}{REDACTED}"))
                .into_owned()
        } else {
            re.replace_all(&out, REDACTED).into_owned()
        };
    }
    out
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LogEntry {
    pub ts: u64,
    pub level: &'static str,
    pub msg: String,
}

#[derive(Default)]
struct Inner {
    entries: Vec<LogEntry>,
    known: Vec<Secret>,
}

/// Bounded, redacted, in-memory log. Never written to disk by the wizard; the operator can
/// download it from the UI.
#[derive(Clone, Default)]
pub struct DiagLog(Arc<Mutex<Inner>>);

const MAX_ENTRIES: usize = 2000;

impl DiagLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a secret so that exact occurrences are scrubbed from the log and from every
    /// API response. Very short values are ignored (they would scrub ordinary words).
    pub fn register_secret(&self, s: &Secret) {
        if s.len() < 8 {
            return;
        }
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !g.known.iter().any(|k| k.expose() == s.expose()) {
            g.known.push(s.clone());
        }
    }

    /// Scrub known secrets and token-shaped strings.
    pub fn scrub(&self, text: &str) -> String {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = text.to_string();
        for k in &g.known {
            if out.contains(k.expose()) {
                out = out.replace(k.expose(), REDACTED);
            }
        }
        drop(g);
        redact(&out)
    }

    /// Scrub only exact known secrets (used on API responses, where pattern redaction would
    /// also rewrite legitimate help text such as "Authorization: Bearer").
    pub fn scrub_known(&self, text: &str) -> (String, bool) {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = text.to_string();
        let mut hit = false;
        for k in &g.known {
            if out.contains(k.expose()) {
                hit = true;
                out = out.replace(k.expose(), REDACTED);
            }
        }
        (out, hit)
    }

    fn push(&self, level: &'static str, msg: &str) {
        let msg = self.scrub(msg);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.entries.len() >= MAX_ENTRIES {
            g.entries.remove(0);
        }
        g.entries.push(LogEntry { ts, level, msg });
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.push("info", msg.as_ref());
    }
    pub fn warn(&self, msg: impl AsRef<str>) {
        self.push("warn", msg.as_ref());
    }
    pub fn error(&self, msg: impl AsRef<str>) {
        self.push("error", msg.as_ref());
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .clone()
    }

    /// Plain-text export for the "Download diagnostic log" button.
    pub fn export_text(&self) -> String {
        let mut s = String::from("Taro Enterprise Setup - diagnostic log (redacted)\n");
        for e in self.entries() {
            s.push_str(&format!(
                "{} [{}] {}\n",
                crate::util::rfc3339(e.ts),
                e.level,
                e.msg
            ));
        }
        // Final pass, in case anything slipped through.
        self.scrub(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_github_tokens() {
        let s = redact(concat!(
            "token gh",
            "p_1234567890abcdefghijABCDEFGHIJ012345 and gi",
            "thub_pat_11ABCDEFG0_abcdefghijklmnop"
        ));
        assert!(!s.contains(concat!("gh", "p_1234")), "{s}");
        assert!(!s.contains(concat!("gi", "thub_pat_11")), "{s}");
        assert_eq!(s.matches(REDACTED).count(), 2);
    }

    #[test]
    fn redacts_stripe_and_webhook() {
        for t in [
            concat!("sk", "_live_51Habcdefghijk"),
            concat!("sk", "_test_51Habcdefghijk"),
            concat!("rk", "_live_51Habcdefghijk"),
            concat!("rk", "_test_51Habcdefghijk"),
            concat!("wh", "sec_abcdefghijklmnop"),
        ] {
            let s = redact(&format!("key={t};"));
            assert!(!s.contains(t), "{t} -> {s}");
        }
    }

    #[test]
    fn redacts_bearer_and_fields() {
        let s = redact("Authorization: Bearer abc.def-ghi_jkl");
        assert!(!s.contains("abc.def"), "{s}");
        let s = redact("curl -H 'bearer abcdefghijklmnop'");
        assert!(!s.contains("abcdefghij"), "{s}");
        let s = redact(concat!(
            r#"{"access_token":"gh"#,
            r#"o_zzzzzzzzzzzz","token_type":"bearer"}"#
        ));
        assert!(!s.contains(concat!("gh", "o_zzz")), "{s}");
        let s = redact("access_token=xyz123secret&scope=repo");
        assert!(!s.contains("xyz123secret"), "{s}");
        assert!(s.contains("scope=repo"), "{s}");
    }

    #[test]
    fn leaves_normal_text() {
        let t = "Created repository acme/taro-prod (private) at https://github.com/acme/taro-prod";
        assert_eq!(redact(t), t);
    }

    #[test]
    fn log_scrubs_known_secret_even_without_pattern() {
        let log = DiagLog::new();
        let odd = Secret::new("weird-format-credential-0000");
        log.register_secret(&odd);
        log.info("value is weird-format-credential-0000 here");
        let text = log.export_text();
        assert!(!text.contains("weird-format-credential-0000"));
        assert!(text.contains(REDACTED));
    }
}
