//! The five functions of moonlight-common-c's PlatformCrypto.h, in Rust (RustCrypto AES), so the
//! library needs neither OpenSSL nor mbedTLS. Semantics follow PlatformCrypto.c's OpenSSL path:
//!  - AES-128-GCM: one message per call, IV of any length (12 or 16 bytes are used), tag out/in.
//!  - AES-128-CBC encrypt: the chain continues across calls on a context (input stream) until
//!    CIPHER_FLAG_RESET_IV; CIPHER_FLAG_PAD_TO_BLOCK_SIZE pads each message (PKCS#7, in place).
//!  - AES-128-CBC decrypt with CIPHER_FLAG_FINISH: one message, PKCS#7 padding removed (audio).

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::aead::AeadInPlace;
use aes_gcm::AesGcm;
use std::os::raw::{c_int, c_uchar};

const ALGORITHM_AES_CBC: c_int = 1;
const ALGORITHM_AES_GCM: c_int = 2;
const CIPHER_FLAG_RESET_IV: c_int = 0x01;
const CIPHER_FLAG_FINISH: c_int = 0x02;
const CIPHER_FLAG_PAD_TO_BLOCK_SIZE: c_int = 0x04;

/// Opaque to C (PPLT_CRYPTO_CONTEXT is only ever passed back to these functions).
pub struct CryptoContext {
    /// CBC chain: the last ciphertext block (or the IV before the first)
    chain: Option<[u8; 16]>,
}

#[no_mangle]
pub extern "C" fn PltCreateCryptoContext() -> *mut CryptoContext {
    Box::into_raw(Box::new(CryptoContext { chain: None }))
}

/// # Safety
/// `ctx` must come from [`PltCreateCryptoContext`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn PltDestroyCryptoContext(ctx: *mut CryptoContext) {
    if !ctx.is_null() {
        drop(Box::from_raw(ctx));
    }
}

/// # Safety
/// `data` must be valid for `length` bytes.
#[no_mangle]
pub unsafe extern "C" fn PltGenerateRandomData(data: *mut c_uchar, length: c_int) {
    if length > 0 {
        let _ = getrandom::getrandom(std::slice::from_raw_parts_mut(data, length as usize));
    }
}

unsafe fn slice<'a>(p: *const c_uchar, n: c_int) -> &'a [u8] {
    if p.is_null() || n <= 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p, n as usize)
    }
}

fn gcm(key: &[u8], iv: &[u8], data: &mut [u8], tag: &mut [u8], encrypt: bool) -> bool {
    macro_rules! run {
        ($n:ty) => {{
            let Ok(c) = AesGcm::<Aes128, $n>::new_from_slice(key) else { return false };
            let nonce = aes_gcm::Nonce::<$n>::from_slice(iv);
            if encrypt {
                match c.encrypt_in_place_detached(nonce, &[], data) {
                    Ok(t) => {
                        let n = tag.len().min(16);
                        tag[..n].copy_from_slice(&t[..n]);
                        true
                    }
                    Err(_) => false,
                }
            } else {
                tag.len() == 16 && c.decrypt_in_place_detached(nonce, &[], data, aes_gcm::Tag::from_slice(tag)).is_ok()
            }
        }};
    }
    match iv.len() {
        12 => run!(U12),
        16 => run!(U16),
        _ => false,
    }
}

/// # Safety
/// The pointers follow PlatformCrypto.h's contract (buffers valid for the given lengths; with
/// CBC padding `input` and `output` have room for the padded length).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn PltEncryptMessage(
    ctx: *mut CryptoContext,
    algorithm: c_int,
    flags: c_int,
    key: *mut c_uchar,
    key_len: c_int,
    iv: *mut c_uchar,
    iv_len: c_int,
    tag: *mut c_uchar,
    tag_len: c_int,
    input: *mut c_uchar,
    input_len: c_int,
    output: *mut c_uchar,
    output_len: *mut c_int,
) -> bool {
    let (key, iv) = (slice(key, key_len), slice(iv, iv_len));
    let Some(ctx) = ctx.as_mut() else { return false };
    match algorithm {
        ALGORITHM_AES_GCM => {
            let mut buf = slice(input, input_len).to_vec();
            let tag = if tag.is_null() { return false } else { std::slice::from_raw_parts_mut(tag, tag_len as usize) };
            if !gcm(key, iv, &mut buf, tag, true) {
                return false;
            }
            std::ptr::copy_nonoverlapping(buf.as_ptr(), output, buf.len());
            *output_len = buf.len() as c_int;
            true
        }
        ALGORITHM_AES_CBC => {
            let Ok(c) = Aes128::new_from_slice(key) else { return false };
            if ctx.chain.is_none() || flags & CIPHER_FLAG_RESET_IV != 0 {
                let Ok(v) = iv.try_into() else { return false };
                ctx.chain = Some(v);
            }
            let mut msg = slice(input, input_len).to_vec();
            if flags & (CIPHER_FLAG_PAD_TO_BLOCK_SIZE | CIPHER_FLAG_FINISH) != 0 {
                let pad = 16 - msg.len() % 16;
                msg.extend(std::iter::repeat_n(pad as u8, pad));
            }
            if msg.len() % 16 != 0 {
                return false;
            }
            let mut prev = ctx.chain.unwrap();
            for b in msg.chunks_mut(16) {
                for (x, p) in b.iter_mut().zip(prev) {
                    *x ^= p;
                }
                c.encrypt_block(aes::Block::from_mut_slice(b));
                prev.copy_from_slice(b);
            }
            ctx.chain = Some(prev);
            std::ptr::copy_nonoverlapping(msg.as_ptr(), output, msg.len());
            *output_len = msg.len() as c_int;
            true
        }
        _ => false,
    }
}

/// # Safety
/// As [`PltEncryptMessage`].
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn PltDecryptMessage(
    ctx: *mut CryptoContext,
    algorithm: c_int,
    flags: c_int,
    key: *mut c_uchar,
    key_len: c_int,
    iv: *mut c_uchar,
    iv_len: c_int,
    tag: *mut c_uchar,
    tag_len: c_int,
    input: *mut c_uchar,
    input_len: c_int,
    output: *mut c_uchar,
    output_len: *mut c_int,
) -> bool {
    let (key, iv) = (slice(key, key_len), slice(iv, iv_len));
    let Some(ctx) = ctx.as_mut() else { return false };
    match algorithm {
        ALGORITHM_AES_GCM => {
            let mut buf = slice(input, input_len).to_vec();
            let tag = if tag.is_null() { return false } else { std::slice::from_raw_parts_mut(tag, tag_len as usize) };
            if !gcm(key, iv, &mut buf, tag, false) {
                return false;
            }
            std::ptr::copy_nonoverlapping(buf.as_ptr(), output, buf.len());
            *output_len = buf.len() as c_int;
            true
        }
        ALGORITHM_AES_CBC => {
            let Ok(c) = Aes128::new_from_slice(key) else { return false };
            if ctx.chain.is_none() || flags & CIPHER_FLAG_RESET_IV != 0 {
                let Ok(v) = iv.try_into() else { return false };
                ctx.chain = Some(v);
            }
            let mut msg = slice(input, input_len).to_vec();
            if msg.len() % 16 != 0 {
                return false;
            }
            let mut prev = ctx.chain.unwrap();
            for b in msg.chunks_mut(16) {
                let ct: [u8; 16] = (&*b).try_into().unwrap();
                c.decrypt_block(aes::Block::from_mut_slice(b));
                for (x, p) in b.iter_mut().zip(prev) {
                    *x ^= p;
                }
                prev = ct;
            }
            ctx.chain = Some(prev);
            let mut n = msg.len();
            if flags & CIPHER_FLAG_FINISH != 0 {
                let pad = *msg.last().unwrap_or(&0) as usize;
                if pad == 0 || pad > 16 || pad > n || !msg[n - pad..].iter().all(|&x| x as usize == pad) {
                    return false;
                }
                n -= pad;
            }
            std::ptr::copy_nonoverlapping(msg.as_ptr(), output, n);
            *output_len = n as c_int;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_round_trip_and_tamper() {
        let key = [7u8; 16];
        for ivn in [12usize, 16] {
            let iv = vec![3u8; ivn];
            let mut msg = b"moonlight control".to_vec();
            let mut tag = [0u8; 16];
            assert!(gcm(&key, &iv, &mut msg, &mut tag, true));
            let mut back = msg.clone();
            assert!(gcm(&key, &iv, &mut back, &mut tag.clone(), false));
            assert_eq!(back, b"moonlight control");
            msg[0] ^= 1;
            assert!(!gcm(&key, &iv, &mut msg, &mut tag, false));
        }
    }

    #[test]
    fn cbc_matches_nist_and_round_trips() {
        // NIST SP 800-38A F.2.1 CBC-AES128 first block
        let key: [u8; 16] = [0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c];
        let mut iv: [u8; 16] = std::array::from_fn(|i| i as u8);
        let mut pt: [u8; 16] = [0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93, 0x17, 0x2a];
        let want = [0x76, 0x49, 0xab, 0xac, 0x81, 0x19, 0xb2, 0x46, 0xce, 0xe9, 0x8e, 0x9b, 0x12, 0xe9, 0x19, 0x7d];
        let mut key2 = key;
        unsafe {
            let ctx = PltCreateCryptoContext();
            let (mut out, mut n) = ([0u8; 32], 0);
            assert!(PltEncryptMessage(ctx, ALGORITHM_AES_CBC, 0, key2.as_mut_ptr(), 16, iv.as_mut_ptr(), 16, std::ptr::null_mut(), 0, pt.as_mut_ptr(), 16, out.as_mut_ptr(), &mut n));
            assert_eq!(&out[..16], &want);
            PltDestroyCryptoContext(ctx);
            // padded message, decrypted with FINISH (as the audio stream)
            let (e, d) = (PltCreateCryptoContext(), PltCreateCryptoContext());
            let mut m = *b"hello input\0\0\0\0\0";
            let (mut ct, mut cn) = ([0u8; 32], 0);
            assert!(PltEncryptMessage(e, ALGORITHM_AES_CBC, CIPHER_FLAG_PAD_TO_BLOCK_SIZE, key2.as_mut_ptr(), 16, iv.as_mut_ptr(), 16, std::ptr::null_mut(), 0, m.as_mut_ptr(), 11, ct.as_mut_ptr(), &mut cn));
            assert_eq!(cn, 16);
            let (mut pt2, mut pn) = ([0u8; 32], 0);
            assert!(PltDecryptMessage(d, ALGORITHM_AES_CBC, CIPHER_FLAG_RESET_IV | CIPHER_FLAG_FINISH, key2.as_mut_ptr(), 16, iv.as_mut_ptr(), 16, std::ptr::null_mut(), 0, ct.as_mut_ptr(), cn, pt2.as_mut_ptr(), &mut pn));
            assert_eq!(&pt2[..pn as usize], b"hello input");
            PltDestroyCryptoContext(e);
            PltDestroyCryptoContext(d);
        }
    }
}
