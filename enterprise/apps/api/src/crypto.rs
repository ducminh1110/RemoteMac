//! Random tokens, their hashes, keyed MACs and constant-time comparison.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// `n` random bytes from the OS generator, as URL-safe base64 (no padding).
pub fn token(n: usize) -> String {
    use base64::Engine;
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// A 6-digit one-time code.
pub fn code() -> String {
    let n = rand::rngs::OsRng.next_u32() % 1_000_000;
    format!("{n:06}")
}

/// SHA-256 of a token: what is stored, so a database leak gives no usable token.
pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// HMAC-SHA256 of `msg` under `key`.
pub fn mac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// A key for one purpose derived from the server's secret (so a grant key is not a CSRF key).
pub fn derive(secret: &str, purpose: &str) -> Vec<u8> {
    mac(secret.as_bytes(), purpose.as_bytes())
}

pub fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_codes_and_macs() {
        let a = token(32);
        assert_eq!(a.len(), 43);
        assert_ne!(a, token(32));
        assert_eq!(code().len(), 6);
        assert_eq!(hash("x"), hash("x"));
        assert!(eq(&mac(b"k", b"m"), &mac(b"k", b"m")));
        assert!(!eq(&mac(b"k", b"m"), &mac(b"k", b"n")));
        assert_ne!(derive("s", "grant"), derive("s", "csrf"));
        // RFC 4231 test case 2
        assert_eq!(
            hex::encode(mac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
