//! `taro-relay update`: Enterprise updates of this server's core from `enterprise-update.zip`.
//!
//! Discovery reads the GitHub releases of the update source (`updates.source` in relay.toml):
//! tags `enterprise-v*` newer than what is installed, each with an `enterprise-update.zip`.
//! Each package says which installed versions it applies to; when the newest does not apply
//! directly, the chain of updates in between is used, and a gap means a full installation.
//! Applying is `taro_update::apply` (signature, policy, backup, rollback) with systemd hooks.
//!
//! Policy (`updates.mode`): manual: only by hand; notify: the hourly timer looks and records
//! what is available; auto: the timer also installs, inside the maintenance window only.

use crate::config::{Runtime, UpdateMode, Window};
use crate::system::System;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use taro_update::apply::{self, Hooks, Options};
use taro_update::policy::Layout;
use taro_update::sig::TrustedKeys;
use taro_update::version::Version;

pub const ASSET: &str = "enterprise-update.zip";

/// Trusted keys: the official ones. Debug builds (tests) may add keys from TARO_UPDATE_TEST_KEYS.
pub fn keys() -> TrustedKeys {
    #[cfg(debug_assertions)]
    if let Ok(t) = std::env::var("TARO_UPDATE_TEST_KEYS") {
        return TrustedKeys::parse(&t).expect("TARO_UPDATE_TEST_KEYS");
    }
    taro_update::official_keys()
}

pub struct SystemdHooks<'a> {
    pub layout: &'a Layout,
    pub sys: &'a dyn System,
    pub relay_port: u16,
    pub probe: &'a dyn crate::setup::Probe,
}

/// Before the new core starts: a relay set up before 0.2 gets the portal and the console (turned
/// on, folders and secrets made), and the systemd units this core expects (their writable
/// folders) are written again.
pub fn before_restart(layout: &Layout, sys: &dyn System) -> Result<(), String> {
    for line in crate::portal::ensure(layout, sys)? {
        println!("  {line}");
    }
    if let Ok(Some(r)) = Runtime::load(layout) {
        if r.portal.as_ref().is_some_and(|p| p.enabled) {
            crate::portal::prepare(layout, sys)?;
            if let Ok(c) = std::fs::read_to_string(crate::portal::code_file(layout)) {
                println!(
                    "  the console's first visit makes its administrator: {}/console/ with the setup code {}",
                    crate::portal::site(&r),
                    c.trim()
                );
            }
        }
    }
    if !sys.systemd() {
        return Ok(());
    }
    let product = crate::web::provider(layout)
        .ok()
        .flatten()
        .map(|p| p.product.name)
        .unwrap_or_else(|| "Taro".into());
    let root = std::env::var_os("TARO_ROOT")
        .filter(|r| !r.is_empty())
        .map(std::path::PathBuf::from);
    let dir = sys.units_dir();
    for (name, text) in crate::setup::units(layout, root.as_deref()) {
        let path = dir.join(name);
        // only units that exist (a relay never set up has none)
        if path.exists() {
            std::fs::write(&path, text.replace("{product}", &product))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    sys.run("systemctl", &["daemon-reload"]).map(|_| ())
}

impl Hooks for SystemdHooks<'_> {
    fn restart(&mut self, services: &[String]) -> Result<(), String> {
        before_restart(self.layout, self.sys)?;
        for s in services {
            self.sys.run("systemctl", &["restart", s])?;
        }
        Ok(())
    }
    fn health(&mut self, version: &str) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..30 {
            match self.probe.health(self.relay_port, version) {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        Err(last)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Available {
    pub version: String,
    pub tag: String,
    pub url: String,
    pub size: u64,
    /// "sha256:…" as GitHub reports it
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    draft: bool,
    assets: Vec<GhAsset>,
}

fn client() -> http::Client {
    http::Client::new()
}

/// A small blocking client (the CLI is synchronous).
mod http {
    pub struct Client {
        rt: tokio::runtime::Runtime,
        c: reqwest::Client,
    }

    impl Client {
        pub fn new() -> Client {
            Client {
                rt: tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime"),
                c: reqwest::Client::builder()
                    .user_agent(concat!("taro-relay/", env!("CARGO_PKG_VERSION")))
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .expect("http"),
            }
        }

        pub fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, String> {
            self.rt.block_on(async {
                let r = self
                    .c
                    .get(url)
                    .header("Accept", "application/vnd.github+json")
                    .send()
                    .await
                    .map_err(|e| format!("{url}: {e}"))?;
                if !r.status().is_success() {
                    return Err(format!("{url}: {}", r.status()));
                }
                r.json().await.map_err(|e| format!("{url}: {e}"))
            })
        }

        pub fn download(&self, url: &str, to: &std::path::Path, max: u64) -> Result<(), String> {
            self.rt.block_on(async {
                use std::io::Write;
                let mut r = self
                    .c
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| format!("{url}: {e}"))?;
                if !r.status().is_success() {
                    return Err(format!("{url}: {}", r.status()));
                }
                let tmp = to.with_extension("part");
                let mut f =
                    std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
                let mut n = 0u64;
                while let Some(chunk) = r.chunk().await.map_err(|e| e.to_string())? {
                    n += chunk.len() as u64;
                    if n > max {
                        return Err("the download is larger than any update".into());
                    }
                    f.write_all(&chunk).map_err(|e| e.to_string())?;
                }
                f.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, to).map_err(|e| e.to_string())
            })
        }
    }
}

/// Newer Enterprise releases with an update package, oldest first.
pub fn discover(source: &str, installed: &str) -> Result<Vec<Available>, String> {
    let have = Version::parse(installed).ok_or_else(|| format!("{installed}: not a version"))?;
    let releases: Vec<GhRelease> = client().get_json(&format!("{source}?per_page=50"))?;
    let mut v: Vec<(Version, Available)> = releases
        .into_iter()
        .filter(|r| !r.draft && r.tag_name.starts_with("enterprise-v"))
        .filter_map(|r| {
            let ver = Version::parse(&r.tag_name)?;
            let a = r.assets.into_iter().find(|a| a.name == ASSET)?;
            (ver > have).then(|| {
                (
                    ver.clone(),
                    Available {
                        version: ver.to_string(),
                        tag: r.tag_name,
                        url: a.browser_download_url,
                        size: a.size,
                        digest: a.digest,
                    },
                )
            })
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(v.into_iter().map(|(_, a)| a).collect())
}

/// Download (once) into the update folder; checked against GitHub's sha256 when it gives one.
pub fn fetch(layout: &Layout, a: &Available) -> Result<PathBuf, String> {
    if !a.url.starts_with("https://github.com/")
        && !a.url.starts_with("https://objects.githubusercontent.com/")
    {
        return Err(format!("{}: not a GitHub release download", a.url));
    }
    let dir = layout.update_state().join("downloads").join(&a.version);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join(ASSET);
    if !p.exists() {
        client().download(&a.url, &p, apply::MAX_PACKAGE)?;
    }
    if let Some(d) = a.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        let have = taro_update::sha256_file(&p).map_err(|e| e.to_string())?;
        if have != d {
            let _ = std::fs::remove_file(&p);
            return Err(format!(
                "{}: the download does not match GitHub's checksum",
                a.version
            ));
        }
    }
    Ok(p)
}

/// The updates to apply in order, from what is installed to the newest reachable version.
pub fn chain(
    layout: &Layout,
    avail: &[Available],
    installed: &str,
) -> Result<Vec<(Available, PathBuf)>, String> {
    let keys = keys();
    let mut at = installed.to_string();
    let mut out = Vec::new();
    // walk up: at each step the newest package that applies to the version we are at
    loop {
        let mut next = None;
        for a in avail.iter().rev() {
            if Version::parse(&a.version) <= Version::parse(&at) {
                continue;
            }
            let p = fetch(layout, a)?;
            let pkg = apply::open(&p, &keys).map_err(|e| format!("{}: {e}", a.version))?;
            let (lo, hi, cur) = (
                Version::parse(&pkg.manifest.min_from_version),
                Version::parse(&pkg.manifest.max_from_version),
                Version::parse(&at),
            );
            if lo <= cur && cur <= hi {
                next = Some((a.clone(), p));
                break;
            }
        }
        match next {
            Some((a, p)) => {
                at = a.version.clone();
                out.push((a, p));
            }
            None => break,
        }
    }
    if out.is_empty() && !avail.is_empty() {
        return Err(format!(
            "{} is available but no update applies to {installed}: install the full Enterprise bundle (keep /etc/taro and /var/lib/taro), see the recovery guide",
            avail.last().unwrap().version
        ));
    }
    Ok(out)
}

fn record_available(layout: &Layout, v: &serde_json::Value) {
    let _ = std::fs::create_dir_all(layout.update_state());
    let _ = std::fs::write(layout.update_state().join("available.json"), v.to_string());
}

pub fn now_unix() -> u64 {
    crate::tls::now_unix() as u64
}

/// What the hourly timer does, by policy. Returns a line for the journal.
pub fn auto(
    layout: &Layout,
    sys: &dyn System,
    probe: &dyn crate::setup::Probe,
) -> Result<String, String> {
    let r = Runtime::load(layout)?.ok_or("not set up")?;
    if r.updates.mode == UpdateMode::Manual {
        return Ok("updates are manual: nothing checked".into());
    }
    let inst = apply::installed(layout)?;
    let avail = discover(&r.updates.source, &inst.version)?;
    if avail.is_empty() {
        record_available(
            layout,
            &serde_json::json!({ "checked": now_unix(), "installed": inst.version, "available": [] }),
        );
        return Ok(format!("{} is current", inst.version));
    }
    let ch = chain(layout, &avail, &inst.version);
    record_available(
        layout,
        &serde_json::json!({ "checked": now_unix(), "installed": inst.version, "available": avail, "chain": ch.as_ref().map(|c| c.iter().map(|(a, _)| a.version.clone()).collect::<Vec<_>>()).map_err(|e| e.clone()) }),
    );
    let ch = ch?;
    if r.updates.mode == UpdateMode::Notify {
        return Ok(format!(
            "available: {} (install with `sudo taro-relay update apply --latest`)",
            ch.iter()
                .map(|(a, _)| a.version.as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    }
    let w = Window::parse(r.updates.window.as_deref().unwrap_or(""))
        .map_err(|e| format!("updates.window: {e}"))?;
    if !w.contains(now_unix()) {
        return Ok("an update is available; waiting for the maintenance window".into());
    }
    apply_chain(layout, sys, probe, &ch, false)
}

pub fn apply_chain(
    layout: &Layout,
    sys: &dyn System,
    probe: &dyn crate::setup::Probe,
    ch: &[(Available, PathBuf)],
    overwrite_modified: bool,
) -> Result<String, String> {
    let port = Runtime::load(layout)?
        .map(|r| r.relay_port())
        .unwrap_or(7470);
    let mut done = Vec::new();
    for (a, p) in ch {
        apply_file(layout, sys, probe, p, overwrite_modified, port)
            .map_err(|e| format!("{}: {e}", a.version))?;
        done.push(a.version.clone());
    }
    Ok(format!("updated to {}", done.join(" -> ")))
}

pub fn apply_file(
    layout: &Layout,
    sys: &dyn System,
    probe: &dyn crate::setup::Probe,
    zip: &Path,
    overwrite_modified: bool,
    relay_port: u16,
) -> Result<apply::Report, String> {
    let mut hooks = SystemdHooks {
        layout,
        sys,
        relay_port,
        probe,
    };
    let opts = Options {
        overwrite_modified,
        now: crate::setup::stamp(),
        keep_backups: 3,
    };
    apply::apply(layout, zip, &keys(), &mut hooks, &opts).map_err(|e| e.to_string())
}
