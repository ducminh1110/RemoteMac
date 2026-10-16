//! In-memory secret handling.
//!
//! [`Secret`] holds a credential (GitHub token, Stripe key, webhook secret, generated keys).
//! It deliberately implements neither `Display` nor `Serialize`, its `Debug` output is a fixed
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

    /// The raw value. Only for building an outgoing request to the one service the secret
    /// belongs to, or for encrypting it.
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
    fn ct_eq_works() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
