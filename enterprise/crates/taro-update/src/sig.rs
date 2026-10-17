//! ed25519 signatures over exact bytes (a manifest as published). Keys are written
//! `ed25519:<base64 of 32 bytes>` (public) and `ed25519-secret:<base64 of the 32-byte seed>`.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Public keys, one per line (`#` comments and blank lines ignored). More than one during a
/// key rotation.
#[derive(Clone, Debug)]
pub struct TrustedKeys(Vec<VerifyingKey>);

impl TrustedKeys {
    pub fn parse(text: &str) -> Result<TrustedKeys, String> {
        let mut keys = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            keys.push(parse_public(line).map_err(|e| format!("line {}: {e}", n + 1))?);
        }
        if keys.is_empty() {
            return Err("no public key".into());
        }
        Ok(TrustedKeys(keys))
    }

    /// These keys and `other`'s.
    pub fn merged(&self, other: &TrustedKeys) -> TrustedKeys {
        let mut v = self.0.clone();
        for k in &other.0 {
            if !v.contains(k) {
                v.push(*k);
            }
        }
        TrustedKeys(v)
    }

    /// Whether `k` is one of these keys.
    pub fn contains(&self, k: &VerifyingKey) -> bool {
        self.0.contains(k)
    }

    pub fn from_keys(keys: Vec<VerifyingKey>) -> TrustedKeys {
        TrustedKeys(keys)
    }

    /// The signature (base64, from a `.sig` file) is by one of these keys over `bytes`; which
    /// one, as `ed25519:…`.
    pub fn verify(&self, bytes: &[u8], sig_b64: &str) -> Result<String, String> {
        let raw = B64
            .decode(sig_b64.trim())
            .map_err(|_| "the signature is not base64".to_string())?;
        let sig = Signature::from_slice(&raw)
            .map_err(|_| "the signature is not an ed25519 signature".to_string())?;
        for k in &self.0 {
            if k.verify(bytes, &sig).is_ok() {
                return Ok(public_text(k));
            }
        }
        Err("the signature is not by a trusted Taro release key".into())
    }
}

pub fn parse_public(s: &str) -> Result<VerifyingKey, String> {
    let b = s
        .trim()
        .strip_prefix("ed25519:")
        .ok_or("expected ed25519:<base64>")?;
    let raw: [u8; 32] = B64
        .decode(b)
        .map_err(|_| "not base64")?
        .try_into()
        .map_err(|_| "not 32 bytes")?;
    VerifyingKey::from_bytes(&raw).map_err(|_| "not a valid ed25519 public key".to_string())
}

pub fn parse_secret(s: &str) -> Result<SigningKey, String> {
    let b = s
        .trim()
        .strip_prefix("ed25519-secret:")
        .ok_or("expected ed25519-secret:<base64>")?;
    let raw: [u8; 32] = B64
        .decode(b)
        .map_err(|_| "not base64")?
        .try_into()
        .map_err(|_| "not 32 bytes")?;
    Ok(SigningKey::from_bytes(&raw))
}

pub fn public_text(k: &VerifyingKey) -> String {
    format!("ed25519:{}", B64.encode(k.as_bytes()))
}

pub fn secret_text(k: &SigningKey) -> String {
    format!("ed25519-secret:{}", B64.encode(k.to_bytes()))
}

/// A new key pair from the system's random source.
pub fn generate() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("system random source");
    SigningKey::from_bytes(&seed)
}

/// base64 signature of `bytes`.
pub fn sign(key: &SigningKey, bytes: &[u8]) -> String {
    B64.encode(key.sign(bytes).to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let k = generate();
        let keys =
            TrustedKeys::parse(&format!("# test\n{}\n", public_text(&k.verifying_key()))).unwrap();
        let s = sign(&k, b"manifest");
        assert_eq!(
            keys.verify(b"manifest", &s).unwrap(),
            public_text(&k.verifying_key())
        );
        assert!(keys.verify(b"manifesT", &s).is_err());
        let other = generate();
        assert!(keys
            .verify(b"manifest", &sign(&other, b"manifest"))
            .is_err());
        assert!(keys.verify(b"manifest", "not base64!").is_err());
        assert_eq!(
            parse_secret(&secret_text(&k)).unwrap().to_bytes(),
            k.to_bytes()
        );
    }

    #[test]
    fn the_official_keys_parse() {
        let _ = crate::official_keys();
    }
}
