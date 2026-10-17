//! The provider folder (and so the primary colour) is outside what an Enterprise update can
//! change: the update policy refuses every provider path, and an update packed and applied with
//! taro-update over a relay installed from this builder's package leaves the provider.json it
//! produced byte-identical.

mod common;
use common::*;
use std::io::Read;
use std::path::{Path, PathBuf};
use taro_update::apply::{self, Hooks};
use taro_update::pack::{self, Input};
use taro_update::policy::{self, Layout};
use taro_update::sig::TrustedKeys;

struct OkHooks(Vec<String>);

impl Hooks for OkHooks {
    fn restart(&mut self, s: &[String]) -> Result<(), String> {
        self.0.extend(s.iter().cloned());
        Ok(())
    }
    fn health(&mut self, _v: &str) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn the_update_policy_never_reaches_the_provider_folder() {
    for p in [
        "provider/provider.json",
        "provider/logo.png",
        "provider/icon.png",
        "etc/taro/provider/provider.json",
        "core/../provider/provider.json",
        "../provider/provider.json",
        "/etc/taro/provider/provider.json",
        "core/provider.json",
        "core/web/../../provider/provider.json",
    ] {
        assert!(
            policy::check_target("relay", p).is_err(),
            "{p} must be refused"
        );
    }
    assert!(policy::check_target("relay", "core/bin/taro-relay").is_ok());
}

/// Unpack the regular files of a tar.gz into `to` (modes kept, links skipped).
fn unpack(tgz: &Path, to: &Path) {
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(
        std::fs::File::open(tgz).unwrap(),
    ));
    for e in a.entries().unwrap() {
        let mut e = e.unwrap();
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let p = to.join(e.path().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut b = Vec::new();
        e.read_to_end(&mut b).unwrap();
        std::fs::write(&p, b).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = e.header().mode().unwrap() & 0o777;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(m)).unwrap();
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::create_dir_all(to).unwrap();
            std::fs::copy(e.path(), &t).unwrap();
        }
    }
}

fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_enterprise_update_keeps_the_built_provider_folder() {
    // build the provider's packages with a custom colour
    let w = wizard("preserve").await;
    w.fill().await;
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    assert_eq!(w.build().await.0, 200);
    let st = w.wait_build().await;
    assert_eq!(st["validation"]["ok"], true, "{}", st["validation"]);
    let dir = PathBuf::from(st["build"]["result"]["dir"].as_str().unwrap());

    // the relay package as the server receives it
    let base = tmp("preserve-server");
    unpack(
        &dir.join("linux/acme-cloud-relay-0.2.0-linux-x86_64.tar.gz"),
        &base.join("pkg"),
    );
    let pkg = base.join("pkg").join(relay_top("x86_64"));
    let built_pj = std::fs::read(dir.join("provider/provider.json")).unwrap();
    assert_eq!(
        std::fs::read(pkg.join("provider/provider.json")).unwrap(),
        built_pj
    );

    // installed as install.sh does: core under /opt/taro, provider folder in /etc/taro/provider
    let layout = Layout::under(&base.join("server"));
    copy_tree(&pkg.join("core"), &layout.core());
    copy_tree(&pkg.join("provider"), &layout.provider);
    apply::record_fresh_install(&layout, "relay", RELAY, "x86_64", "2026-10-17T00:00:00Z").unwrap();
    let before = snapshot(&layout.provider);
    assert!(before.iter().any(|(p, _)| p.ends_with("provider.json")));
    assert!(before.iter().any(|(p, _)| p.ends_with("logo.png")));

    // the next Enterprise release: a new relay binary and new pages
    let key = taro_update::sig::generate();
    let keys = TrustedKeys::from_keys(vec![key.verifying_key()]);
    let v1 = pack::pack(
        &Input {
            component: "relay".into(),
            version: RELAY.into(),
            released_at: "t1".into(),
            cores: vec![("x86_64".into(), pkg.join("core"))],
            base: None,
            migrations: None,
            restart: vec!["taro-relay".into()],
            source_commit: None,
        },
        &key,
        &base.join("v1.zip"),
    )
    .unwrap();
    let next = base.join("next-core");
    copy_tree(&pkg.join("core"), &next);
    std::fs::write(next.join("bin/taro-relay"), b"\x7fELF taro-relay 0.3.0").unwrap();
    std::fs::write(
        next.join("web/relay/status/index.html"),
        "<main>status v2</main>\n",
    )
    .unwrap();
    std::fs::write(
        next.join("web/relay/status/app.css"),
        ":root{--primary:var(--brand)}\n",
    )
    .unwrap();
    let zip = base.join("enterprise-update.zip");
    pack::pack(
        &Input {
            component: "relay".into(),
            version: "0.3.0".into(),
            released_at: "t2".into(),
            cores: vec![("x86_64".into(), next.clone())],
            base: Some(v1.core_files),
            migrations: None,
            restart: vec!["taro-relay".into()],
            source_commit: None,
        },
        &key,
        &zip,
    )
    .unwrap();
    // nothing in the package points at the provider folder
    let opened = apply::open(&zip, &keys).unwrap();
    assert!(opened
        .manifest
        .files
        .iter()
        .all(|f| f.path.starts_with("core/")));

    let mut hooks = OkHooks(vec![]);
    let r = apply::apply(
        &layout,
        &zip,
        &keys,
        &mut hooks,
        &apply::Options {
            now: "2026-10-17T02:00:00Z".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((r.from.as_str(), r.to.as_str()), (RELAY, "0.3.0"));
    assert_eq!(
        std::fs::read(layout.core().join("bin/taro-relay")).unwrap(),
        b"\x7fELF taro-relay 0.3.0"
    );
    assert_eq!(hooks.0, vec!["taro-relay".to_string()]);

    // the provider folder: byte for byte what the builder wrote, colour included
    assert_eq!(snapshot(&layout.provider), before);
    let after = std::fs::read(layout.provider.join("provider.json")).unwrap();
    assert_eq!(after, built_pj);
    let p = rm_brand::Provider::load(&layout.provider).unwrap().unwrap();
    assert_eq!(p.branding.primary_color, "#3D7A5B");
    assert_eq!(
        p.tokens(),
        rm_brand::Tokens::derive(rm_brand::Color(0x3d, 0x7a, 0x5b))
    );
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&w.out);
}
