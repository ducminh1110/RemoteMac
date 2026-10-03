//! AES-128-GCM with the deterministic IVs of Sunshine's control stream (stream.cpp): the
//! sequence number in the low bytes, two ASCII bytes naming the direction and stream at 10..12.

use aes::Aes128;
use aes_gcm::aead::consts::U12;
use aes_gcm::aead::AeadInPlace;
use aes_gcm::{AesGcm, KeyInit};

pub fn iv(seq: u32, a: u8, b: u8) -> [u8; 12] {
    let mut iv = [0u8; 12];
    iv[..4].copy_from_slice(&seq.to_le_bytes());
    iv[10] = a;
    iv[11] = b;
    iv
}

/// Ciphertext and tag.
pub fn seal(key: &[u8; 16], iv: &[u8; 12], plain: &[u8]) -> (Vec<u8>, [u8; 16]) {
    let c = AesGcm::<Aes128, U12>::new_from_slice(key).expect("16-byte key");
    let mut buf = plain.to_vec();
    let tag = c.encrypt_in_place_detached(aes_gcm::Nonce::<U12>::from_slice(iv), &[], &mut buf).expect("gcm");
    (buf, tag.into())
}

pub fn open(key: &[u8; 16], iv: &[u8; 12], tag: &[u8], cipher: &[u8]) -> Option<Vec<u8>> {
    let c = AesGcm::<Aes128, U12>::new_from_slice(key).ok()?;
    let mut buf = cipher.to_vec();
    c.decrypt_in_place_detached(aes_gcm::Nonce::<U12>::from_slice(iv), &[], &mut buf, aes_gcm::Tag::from_slice(tag.get(..16)?)).ok()?;
    Some(buf)
}
