//! libsodium sealed boxes (`crypto_box_seal`) for GitHub Actions secrets.
//!
//! GitHub hands out an X25519 public key per repository (base64). Each secret value is
//! encrypted to it with an anonymous sealed box and uploaded base64-encoded.

use crate::secret::Secret;
use crate::util::{b64, b64_decode};
use crypto_box::{aead::OsRng, PublicKey, KEY_SIZE};

pub fn seal_for_github(public_key_b64: &str, value: &Secret) -> Result<String, String> {
    let raw = b64_decode(public_key_b64)
        .ok_or_else(|| "GitHub returned a repository public key that is not base64.".to_string())?;
    let bytes: [u8; KEY_SIZE] = raw
        .try_into()
        .map_err(|_| "GitHub returned a repository public key of the wrong length.".to_string())?;
    let pk = PublicKey::from(bytes);
    let sealed = pk
        .seal(&mut OsRng, value.expose().as_bytes())
        .map_err(|_| "Encryption failed.".to_string())?;
    Ok(b64(&sealed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto_box::SecretKey;

    #[test]
    fn round_trip() {
        let sk = SecretKey::generate(&mut OsRng);
        let pk_b64 = b64(sk.public_key().as_bytes());
        let value = Secret::new(concat!("sk", "_test_51Hsecretvalue0123456789"));
        let ct = seal_for_github(&pk_b64, &value).unwrap();
        let raw = b64_decode(&ct).unwrap();
        // 32-byte ephemeral key + 16-byte tag + plaintext.
        assert_eq!(raw.len(), 32 + 16 + value.len());
        let pt = sk.unseal(&raw).unwrap();
        assert_eq!(pt, value.expose().as_bytes());
        // Fresh ephemeral key each time.
        assert_ne!(ct, seal_for_github(&pk_b64, &value).unwrap());
    }

    #[test]
    fn wrong_key_cannot_open() {
        let sk = SecretKey::generate(&mut OsRng);
        let other = SecretKey::generate(&mut OsRng);
        let ct = seal_for_github(&b64(sk.public_key().as_bytes()), &Secret::new("x")).unwrap();
        assert!(other.unseal(&b64_decode(&ct).unwrap()).is_err());
    }

    #[test]
    fn bad_public_key() {
        assert!(seal_for_github("not base64!!", &Secret::new("x")).is_err());
        assert!(seal_for_github(&b64(&[1, 2, 3]), &Secret::new("x")).is_err());
    }
}
