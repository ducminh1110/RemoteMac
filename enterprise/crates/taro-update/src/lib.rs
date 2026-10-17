//! Taro Enterprise releases and updates.
//!
//! - [`sig`]: ed25519 signatures. Release files are signed by the private key held only in the
//!   release workflow's `TARO_ENTERPRISE_SIGNING_KEY` secret; [`official_keys`] are the public
//!   halves this code trusts (`enterprise/keys/release-signing.pub`).
//! - [`manifest`]: `enterprise-update.zip`'s `manifest.json`, the core file lists and the
//!   component manifest the package builder (Wizard 1) pins its downloads to.
//! - [`policy`]: what an update may touch, built in (never taken from the package): files under
//!   `core/` of the installation, by component. Provider customization (`/etc/taro/provider`)
//!   and runtime state (`/etc/taro/runtime`, `/var/lib/taro`) are outside it by construction.
//! - [`pack`]: builds `enterprise-update.zip` from a release's core trees (CI).
//! - [`apply`]: applies one to an installation: verify, check, stage, back up, swap, migrate,
//!   restart, health-check, and roll back on failure.

pub mod apply;
pub mod manifest;
pub mod migrate;
pub mod pack;
pub mod policy;
pub mod sig;
pub mod version;

/// The public release keys this build trusts.
pub fn official_keys() -> sig::TrustedKeys {
    sig::TrustedKeys::parse(include_str!("../../../keys/release-signing.pub"))
        .expect("enterprise/keys/release-signing.pub is valid")
}

/// sha256 of bytes, lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// sha256 of a file, streamed.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
