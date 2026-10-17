//! The files that describe a release: `manifest.json` inside `enterprise-update.zip`, the core
//! file lists (`enterprise-core-files.json`, what a release's core is, so the next release can
//! ship only what changed) and the component manifest (`taro-enterprise-components.json`, the
//! exact client and relay files a provider's package is built from).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const UPDATE_SCHEMA: &str = "taro.enterprise-update/v1";
pub const CORE_FILES_SCHEMA: &str = "taro.core-files/v1";
pub const INSTALLED_SCHEMA: &str = "taro.installed/v1";
pub const COMPONENTS_SCHEMA: &str = "taro.components/v1";

/// The names inside `enterprise-update.zip` (anything else in it is refused).
pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_SIG: &str = "manifest.json.sig";
pub const CHECKSUMS: &str = "checksums.sha256";
pub const PAYLOAD: &str = "payload/";
pub const MIGRATIONS: &str = "migrations/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// a file the installed version does not have
    Add,
    /// a file it has, with other contents
    Replace,
}

/// One file to put in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub op: Op,
    /// where, under the installation root (`core/…`)
    pub path: String,
    /// where in the zip (`payload/any/core/…`, `payload/x86_64/core/…`)
    pub source: String,
    pub sha256: String,
    pub size: u64,
    /// unix permission bits (0o755 for programs, 0o644 otherwise)
    pub mode: u32,
    /// only for this architecture (binaries); none: for all
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveEntry {
    pub path: String,
    /// the contents being removed (it must still be that, or it is a conflict)
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationEntry {
    /// `0002-update-window`: applied once, in order
    pub id: String,
    /// `migrations/0002-update-window.json`
    pub file: String,
    pub sha256: String,
}

/// `manifest.json`: everything the updater does with the package, signed as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub schema: String,
    pub product: String,
    pub component: String,
    /// the version this update brings
    pub version: String,
    /// installed versions it applies to (inclusive); older ones need the updates in between,
    /// or a full installation
    pub min_from_version: String,
    pub max_from_version: String,
    pub released_at: String,
    /// the core prefixes the files below stay within (the updater checks them against its
    /// own policy as well)
    pub permitted_paths: Vec<String>,
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub remove: Vec<RemoveEntry>,
    #[serde(default)]
    pub migrations: Vec<MigrationEntry>,
    /// systemd services to restart afterwards
    #[serde(default)]
    pub restart: Vec<String>,
    /// the source commit, for the record
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
}

impl UpdateManifest {
    /// The files for one architecture.
    pub fn files_for<'a>(&'a self, arch: &'a str) -> impl Iterator<Item = &'a FileEntry> + 'a {
        self.files
            .iter()
            .filter(move |f| f.arch.as_deref().is_none_or(|a| a == arch))
    }

    pub fn removals_for<'a>(&'a self, arch: &'a str) -> impl Iterator<Item = &'a RemoveEntry> + 'a {
        self.remove
            .iter()
            .filter(move |f| f.arch.as_deref().is_none_or(|a| a == arch))
    }
}

/// One core file's identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreFile {
    pub sha256: String,
    pub size: u64,
    pub mode: u32,
}

/// `enterprise-core-files.json`: a release's core, per architecture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreFiles {
    pub schema: String,
    pub product: String,
    pub component: String,
    pub version: String,
    /// arch -> path -> file
    pub files: BTreeMap<String, BTreeMap<String, CoreFile>>,
}

/// `<root>/INSTALLED.json`: what the installed core is (so changes made by hand show up as
/// conflicts instead of being overwritten).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub schema: String,
    pub product: String,
    pub component: String,
    pub version: String,
    pub arch: String,
    /// path -> sha256
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub migrations: Vec<String>,
    pub updated_at: String,
}

/// One downloadable file of a provider's distribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    /// `windows-client`, `macos-client`, `linux-relay-x86_64`, `linux-relay-aarch64`
    pub id: String,
    pub version: String,
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// `taro-enterprise-components.json`: the exact files an Enterprise release is made of
/// (clients from the Taro release it was tested with, the relay built with it). Signed
/// (`.sig` next to it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Components {
    pub schema: String,
    pub enterprise_version: String,
    pub taro_version: String,
    pub released_at: String,
    pub components: Vec<Component>,
}

pub const COMPONENT_IDS: &[&str] = &[
    "windows-client",
    "macos-client",
    "linux-relay-x86_64",
    "linux-relay-aarch64",
];

impl Components {
    /// Parse after checking the signature.
    pub fn verify(
        bytes: &[u8],
        sig: &str,
        keys: &crate::sig::TrustedKeys,
    ) -> Result<Components, String> {
        keys.verify(bytes, sig)
            .map_err(|e| format!("taro-enterprise-components.json: {e}"))?;
        let c: Components = serde_json::from_slice(bytes)
            .map_err(|e| format!("taro-enterprise-components.json: {e}"))?;
        if c.schema != COMPONENTS_SCHEMA {
            return Err(format!(
                "taro-enterprise-components.json: schema {} is not {COMPONENTS_SCHEMA}",
                c.schema
            ));
        }
        for comp in &c.components {
            if comp.sha256.len() != 64 || !comp.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("{}: bad sha256", comp.id));
            }
            if !comp.url.starts_with("https://") {
                return Err(format!("{}: not an https download", comp.id));
            }
        }
        Ok(c)
    }

    pub fn get(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }
}
