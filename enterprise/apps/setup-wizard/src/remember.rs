//! Optional "remember on this PC" storage for the GitHub token.
//!
//! Windows only: the token is encrypted with DPAPI (`CryptProtectData`, current-user scope)
//! and stored in `%APPDATA%\TaroEnterpriseSetup\github-token.dpapi`. Nothing is ever written
//! unless the operator ticks the box. On other operating systems the option is not offered.

use crate::secret::Secret;
use std::path::PathBuf;

pub const FILE_NAME: &str = "github-token.dpapi";

pub fn supported() -> bool {
    cfg!(windows)
}

/// `%APPDATA%\TaroEnterpriseSetup`.
pub fn default_dir() -> Option<PathBuf> {
    if !supported() {
        return None;
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("TaroEnterpriseSetup"))
}

pub fn stored_exists(dir: &std::path::Path) -> bool {
    dir.join(FILE_NAME).is_file()
}

pub fn forget(dir: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(dir.join(FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete the stored token: {e}")),
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    use zeroize::Zeroize;

    fn take_blob(b: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: the blob was allocated by DPAPI with LocalAlloc and is `cbData` bytes long.
        unsafe {
            let v = std::slice::from_raw_parts(b.pbData, b.cbData as usize).to_vec();
            std::ptr::write_bytes(b.pbData, 0, b.cbData as usize);
            let _ = LocalFree(Some(HLOCAL(b.pbData as *mut core::ffi::c_void)));
            v
        }
    }

    pub fn protect(plain: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input points to `plain`, which outlives the call; out is written by DPAPI.
        unsafe {
            CryptProtectData(
                &input,
                windows::core::w!("Taro Enterprise Setup GitHub token"),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| format!("CryptProtectData failed: {e}"))?;
        Ok(take_blob(out))
    }

    pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>, String> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: cipher.len() as u32,
            pbData: cipher.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: as above.
        unsafe {
            CryptUnprotectData(
                &input,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
        .map_err(|e| format!("CryptUnprotectData failed: {e}"))?;
        Ok(take_blob(out))
    }

    pub fn save(dir: &std::path::Path, token: &Secret) -> Result<(), String> {
        let blob = protect(token.expose().as_bytes())?;
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {dir:?}: {e}"))?;
        std::fs::write(dir.join(FILE_NAME), blob)
            .map_err(|e| format!("Could not write the protected token: {e}"))
    }

    pub fn load(dir: &std::path::Path) -> Result<Secret, String> {
        let blob = std::fs::read(dir.join(FILE_NAME))
            .map_err(|e| format!("Could not read the stored token: {e}"))?;
        let mut plain = unprotect(&blob)?;
        let s = String::from_utf8(plain.clone()).map_err(|_| "Stored token is corrupt".to_string());
        plain.zeroize();
        s.map(Secret::new)
    }
}

#[cfg(windows)]
pub use imp::{load, save};

#[cfg(not(windows))]
pub fn save(_dir: &std::path::Path, _token: &Secret) -> Result<(), String> {
    Err("Remembering the token is only available on Windows (DPAPI).".into())
}

#[cfg(not(windows))]
pub fn load(_dir: &std::path::Path) -> Result<Secret, String> {
    Err("Remembering the token is only available on Windows (DPAPI).".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn not_offered_off_windows() {
        if !super::supported() {
            assert!(super::default_dir().is_none());
            assert!(
                super::save(std::path::Path::new("."), &crate::secret::Secret::new("x")).is_err()
            );
        }
    }
}
