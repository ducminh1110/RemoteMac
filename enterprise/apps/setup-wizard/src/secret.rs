//! In-memory secret handling.
//!
//! [`Secret`] holds a value that must not leak into logs, responses or summaries: the wizard's
//! access token and the provider's relay admission key (which belongs in provider.json and
//! nowhere else). It deliberately implements neither `Display` nor `Serialize`, its `Debug` output is a fixed
//! placeholder, and its buffer is zeroed when dropped. The only way to read the value is
//! [`Secret::expose`], which keeps every use site greppable.

use std::fmt;
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(Zeroizing::new(value.into()))
    }

    /// The raw value. Only for writing provider.json, comparing, or the explicit "reveal"
    /// request of the UI.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

/// `n` random bytes from the operating system CSPRNG, hex-encoded (2n characters).
pub fn random_hex(n: usize) -> String {
    let mut buf = Zeroizing::new(vec![0u8; n]);
    getrandom::fill(&mut buf).expect("operating system random number generator unavailable");
    hex(&buf)
}

/// The alphabet of generated relay keys (64 symbols: one per 6 bits, no modulo bias).
const KEY_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// A new relay admission key: `len` characters of `[A-Za-z0-9_-]` from the OS CSPRNG.
pub fn relay_key(len: usize) -> Secret {
    let mut buf = Zeroizing::new(vec![0u8; len]);
    getrandom::fill(&mut buf).expect("operating system random number generator unavailable");
    let s: String = buf
        .iter()
        .map(|b| KEY_ALPHABET[(b & 63) as usize] as char)
        .collect();
    Secret::new(s)
}

/// `••••…` plus the last four characters, for showing that a key is set without showing it.
pub fn mask(s: &Secret) -> String {
    let v = s.expose();
    let n = v.chars().count();
    if n <= 8 {
        return "•".repeat(n.max(8));
    }
    let tail: String = v.chars().skip(n - 4).collect();
    format!("{}{tail}", "•".repeat(n - 4))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

/// Constant-time string comparison (for the wizard's access token).
pub fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_value() {
        let s = Secret::new(concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert_eq!(format!("{s:?}"), "Secret([REDACTED])");
        assert!(!format!("{:?}", Some(s.clone())).contains(concat!("gh", "p_")));
    }

    #[test]
    fn random_hex_shape() {
        let a = random_hex(32);
        let b = random_hex(32);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn relay_keys_are_valid_and_distinct() {
        let a = relay_key(32);
        let b = relay_key(32);
        assert_eq!(a.len(), 32);
        assert!(rm_brand::valid_relay_key(a.expose()));
        assert!(a
            .expose()
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_ne!(a.expose(), b.expose());
        let m = mask(&a);
        assert!(!m.contains(&a.expose()[..28]));
        assert!(m.ends_with(&a.expose()[28..]));
    }

    #[test]
    fn ct_eq_works() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
