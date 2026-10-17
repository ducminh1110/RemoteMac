//! What an Enterprise update may change: built into the updater, never read from a package.
//!
//! An installation is laid out so the three kinds of files never share a folder:
//!
//! | kind | where (Linux) | changed by |
//! |---|---|---|
//! | shared core | `/opt/taro/core/` | `enterprise-update.zip` only |
//! | provider customization | `/etc/taro/provider/` | the provider (Wizard 1's package) |
//! | runtime state | `/etc/taro/runtime/`, `/var/lib/taro/` | Wizard 2, the running relay |
//!
//! Updates write under `<root>/core/` and only below the prefixes their component owns.
//! Migrations (see [`crate::migrate`]) may add missing settings to the runtime files named in
//! [`MIGRATABLE`], never replace them.

use std::path::{Component, Path, PathBuf};

/// The installation's folders.
#[derive(Clone, Debug)]
pub struct Layout {
    /// `/opt/taro`: holds `core/` and `INSTALLED.json`
    pub root: PathBuf,
    /// `/etc/taro/provider`: provider.json, logo, icon, theme overrides
    pub provider: PathBuf,
    /// `/etc/taro/runtime`: relay.toml and other machine settings (Wizard 2)
    pub runtime: PathBuf,
    /// `/var/lib/taro`: the relay's data, TLS certificates, update backups and history
    pub state: PathBuf,
}

impl Layout {
    pub fn system() -> Layout {
        Layout {
            root: "/opt/taro".into(),
            provider: "/etc/taro/provider".into(),
            runtime: "/etc/taro/runtime".into(),
            state: "/var/lib/taro".into(),
        }
    }

    /// Everything under one folder (tests, or a portable install).
    pub fn under(base: &Path) -> Layout {
        Layout {
            root: base.join("opt/taro"),
            provider: base.join("etc/taro/provider"),
            runtime: base.join("etc/taro/runtime"),
            state: base.join("var/lib/taro"),
        }
    }

    pub fn core(&self) -> PathBuf {
        self.root.join("core")
    }

    pub fn installed_record(&self) -> PathBuf {
        self.root.join(INSTALLED)
    }

    pub fn update_state(&self) -> PathBuf {
        self.state.join("update")
    }
}

/// The record of the installed core (written by the installer and by updates only).
pub const INSTALLED: &str = "INSTALLED.json";

/// The product every Enterprise update names.
pub const PRODUCT: &str = "taro-enterprise";

/// The components an update can target, and the core prefixes each owns.
pub fn prefixes(component: &str) -> Option<&'static [&'static str]> {
    match component {
        // the relay server: its binary, its pages (setup, status), shared files
        "relay" => Some(&["core/bin/", "core/web/", "core/share/"]),
        _ => None,
    }
}

/// Runtime files a migration may add settings to (in `Layout::runtime`).
pub const MIGRATABLE: &[&str] = &["relay.toml"];

/// Architectures a package can carry binaries for.
pub const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// A path as written in a package: relative, `/`-separated, no `.`/`..`, no empty parts, no
/// backslashes, drive letters, control characters or NUL. Returns why not.
pub fn check_relative(p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > 240 {
        return Err(format!("\"{p}\": empty or too long"));
    }
    if p.starts_with('/')
        || p.contains('\\')
        || p.contains(':')
        || p.chars().any(|c| c.is_control())
    {
        return Err(format!("\"{p}\": not a plain relative path"));
    }
    for part in p.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("\"{p}\": empty, . or .. in the path"));
        }
        if !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+' | b'@'))
        {
            return Err(format!("\"{p}\": unusual characters"));
        }
    }
    // the OS agrees there is nothing but normal parts
    if !Path::new(p)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(format!("\"{p}\": not a plain relative path"));
    }
    Ok(())
}

/// A core path an update of `component` may write or remove.
pub fn check_target(component: &str, p: &str) -> Result<(), String> {
    check_relative(p)?;
    let Some(allowed) = prefixes(component) else {
        return Err(format!("unknown component \"{component}\""));
    };
    if p == INSTALLED
        || !allowed
            .iter()
            .any(|a| p.starts_with(a) && p.len() > a.len())
    {
        return Err(format!(
            "\"{p}\" is outside what a {component} update may change ({})",
            allowed.join(", ")
        ));
    }
    Ok(())
}

/// `rel` under `root`, refusing a symbolic link anywhere on the way (one could point outside).
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    check_relative(rel)?;
    let mut cur = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("\"{rel}\": {} is a symbolic link", cur.display()))
            }
            Ok(m) if i + 1 < parts.len() && !m.is_dir() => {
                return Err(format!("\"{rel}\": {} is not a folder", cur.display()))
            }
            _ => {}
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_and_odd_paths_are_refused() {
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "core/../../etc/shadow",
            "core//a",
            "core/./a",
            "C:/x",
            "core\\a",
            "core/a\u{0}b",
            "core/ä",
        ] {
            assert!(check_relative(bad).is_err(), "{bad:?}");
        }
        assert!(check_relative("core/web/app.css").is_ok());
    }

    #[test]
    fn components_own_their_prefixes() {
        assert!(check_target("relay", "core/bin/taro-relay").is_ok());
        assert!(check_target("relay", "core/web/setup/app.js").is_ok());
        for bad in [
            "provider/provider.json",
            "core/INSTALLED.json",
            "INSTALLED.json",
            "core/web/",
            "runtime/relay.toml",
            "core/other/x",
        ] {
            assert!(check_target("relay", bad).is_err(), "{bad}");
        }
        assert!(check_target("billing", "core/bin/x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_on_the_way_are_refused() {
        let d = std::env::temp_dir().join(format!("taro-policy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("core")).unwrap();
        std::os::unix::fs::symlink("/etc", d.join("core/web")).unwrap();
        assert!(resolve(&d, "core/web/passwd").is_err());
        assert!(resolve(&d, "core/bin/taro-relay").is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
