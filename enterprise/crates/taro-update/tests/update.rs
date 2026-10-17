//! enterprise-update.zip from end to end: two releases are packed, the first is installed with
//! a provider's branding, TLS files and relay data, and the second is applied over it; then
//! everything that must be refused is, and failed updates roll back.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use taro_update::apply::{self, Error, Hooks, Options};
use taro_update::manifest::*;
use taro_update::pack::{self, Input};
use taro_update::policy::Layout;
use taro_update::sig::{self, TrustedKeys};

struct Fake {
    restarts: Vec<Vec<String>>,
    fail_health: Option<String>,
    checked: Vec<String>,
}

impl Fake {
    fn ok() -> Fake {
        Fake {
            restarts: vec![],
            fail_health: None,
            checked: vec![],
        }
    }
}

impl Hooks for Fake {
    fn restart(&mut self, s: &[String]) -> Result<(), String> {
        self.restarts.push(s.to_vec());
        Ok(())
    }
    fn health(&mut self, v: &str) -> Result<(), String> {
        self.checked.push(v.into());
        match &self.fail_health {
            Some(bad) if bad == v => Err("the relay did not answer".into()),
            _ => Ok(()),
        }
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("taro-update-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn put(base: &Path, rel: &str, text: &str) {
    let p = base.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
}

fn read(base: &Path, rel: &str) -> String {
    std::fs::read_to_string(base.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// A release's core for one arch.
fn core(dir: &Path, arch: &str, version: &str, layout_v2: bool) {
    put(
        dir,
        "bin/taro-relay",
        &format!("relay binary {version} {arch}"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("bin/taro-relay"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    put(
        dir,
        "web/relay/app.js",
        "console.log('shared, unchanged')\n",
    );
    if layout_v2 {
        put(
            dir,
            "web/relay/index.html",
            "<main class=dashboard-v2><img src=/brand/logo.png></main>\n",
        );
        put(
            dir,
            "web/relay/app.css",
            ".dashboard-v2{display:grid;color:var(--primary)}\n",
        );
        put(
            dir,
            "web/relay/nav.js",
            "export const nav = 'new shared navigation'\n",
        );
    } else {
        put(
            dir,
            "web/relay/index.html",
            "<main class=dashboard-v1><img src=/brand/logo.png></main>\n",
        );
        put(
            dir,
            "web/relay/app.css",
            ".dashboard-v1{color:var(--primary)}\n",
        );
        put(dir, "share/old-notes.txt", "removed in 0.2.0\n");
    }
}

struct World {
    base: PathBuf,
    layout: Layout,
    key: ed25519_dalek::SigningKey,
    keys: TrustedKeys,
    v1_files: CoreFiles,
    v2_zip: PathBuf,
}

const PROVIDER_JSON: &str =
    "{\"provider_id\":\"acme-cloud\",\"branding\":{\"primary_color\":\"#3D7A5B\"}}\n";

fn world(name: &str) -> World {
    let base = tmp(name);
    let key = sig::generate();
    let keys = TrustedKeys::from_keys(vec![key.verifying_key()]);
    // release 0.1.0 and 0.2.0, two arches each
    for (v, l2) in [("0.1.0", false), ("0.2.0", true)] {
        for a in ["x86_64", "aarch64"] {
            core(&base.join(format!("rel-{v}/{a}")), a, v, l2);
        }
    }
    let cores = |v: &str| {
        vec![
            ("x86_64".to_string(), base.join(format!("rel-{v}/x86_64"))),
            ("aarch64".to_string(), base.join(format!("rel-{v}/aarch64"))),
        ]
    };
    let v1 = pack::pack(
        &Input {
            component: "relay".into(),
            version: "0.1.0".into(),
            released_at: "t1".into(),
            cores: cores("0.1.0"),
            base: None,
            migrations: None,
            restart: vec!["taro-relay".into()],
            source_commit: None,
        },
        &key,
        &base.join("u1.zip"),
    )
    .unwrap();
    let mig = base.join("migrations");
    put(
        &mig,
        "0002-update-check.json",
        r#"{"description":"daily update checks","ops":[{"op":"set_default","file":"relay.toml","table":"updates","key":"check","value":"daily"},{"op":"set_default","file":"relay.toml","table":"updates","key":"mode","value":"auto"}]}"#,
    );
    let v2_zip = base.join("enterprise-update.zip");
    pack::pack(
        &Input {
            component: "relay".into(),
            version: "0.2.0".into(),
            released_at: "t2".into(),
            cores: cores("0.2.0"),
            base: Some(v1.core_files.clone()),
            migrations: Some(mig),
            restart: vec!["taro-relay".into()],
            source_commit: Some("abc123".into()),
        },
        &key,
        &v2_zip,
    )
    .unwrap();

    // a server with 0.1.0 installed (the full bundle), branded and configured
    let layout = Layout::under(&base.join("server"));
    copy_tree(&base.join("rel-0.1.0/x86_64"), &layout.core());
    apply::record_fresh_install(&layout, "relay", "0.1.0", "x86_64", "t0").unwrap();
    put(&layout.provider, "provider.json", PROVIDER_JSON);
    put(&layout.provider, "logo.png", "PNG provider logo");
    put(
        &layout.provider,
        "theme.css",
        ".dashboard-v1{border-radius:0}\n",
    );
    put(
        &layout.runtime,
        "relay.toml",
        "# set by Wizard 2\nport = 7470\n\n[updates]\nmode = \"manual\"\n",
    );
    put(
        &layout.state,
        "tls/privkey.pem",
        "-----BEGIN PRIVATE KEY----- test",
    );
    put(
        &layout.state,
        "tls/fullchain.pem",
        "-----BEGIN CERTIFICATE----- test",
    );
    put(&layout.state, "relay/ids.json", "{\"123456789\":\"owner\"}");
    World {
        base,
        layout,
        key,
        keys,
        v1_files: v1.core_files,
        v2_zip,
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

/// Everything that must survive any update.
fn preserved(w: &World) -> Vec<(PathBuf, Vec<u8>)> {
    let l = &w.layout;
    ["provider.json", "logo.png", "theme.css"]
        .iter()
        .map(|f| l.provider.join(f))
        .chain(
            ["tls/privkey.pem", "tls/fullchain.pem", "relay/ids.json"]
                .iter()
                .map(|f| l.state.join(f)),
        )
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect()
}

fn assert_preserved(before: &[(PathBuf, Vec<u8>)]) {
    for (p, b) in before {
        assert_eq!(&std::fs::read(p).unwrap(), b, "{} changed", p.display());
    }
}

fn zip_names(p: &Path) -> BTreeSet<String> {
    let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    z.file_names().map(|n| n.unwrap().to_string()).collect()
}

#[test]
fn the_zip_holds_only_what_changed() {
    let w = world("contents");
    let names = zip_names(&w.v2_zip);
    let payload: BTreeSet<&str> = names
        .iter()
        .filter(|n| n.starts_with("payload/"))
        .map(String::as_str)
        .collect();
    // binaries per arch; the shared web files once; app.js (unchanged) not at all
    let want: BTreeSet<&str> = [
        "payload/x86_64/core/bin/taro-relay",
        "payload/aarch64/core/bin/taro-relay",
        "payload/any/core/web/relay/app.css",
        "payload/any/core/web/relay/index.html",
        "payload/any/core/web/relay/nav.js",
    ]
    .into();
    assert_eq!(payload, want);
    for n in &names {
        assert!(
            ["manifest.json", "manifest.json.sig", "checksums.sha256"].contains(&n.as_str())
                || n.starts_with("payload/")
                || n.starts_with("migrations/"),
            "{n}"
        );
    }
    let p = apply::open(&w.v2_zip, &w.keys).unwrap();
    let m = &p.manifest;
    assert_eq!(
        (
            m.min_from_version.as_str(),
            m.max_from_version.as_str(),
            m.version.as_str()
        ),
        ("0.1.0", "0.1.0", "0.2.0")
    );
    assert_eq!(
        m.remove.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["core/share/old-notes.txt"]
    );
    assert!(m.files.iter().all(|f| f.path.starts_with("core/")));
    assert!(m
        .files
        .iter()
        .any(|f| f.path == "core/web/relay/nav.js" && f.op == Op::Add));
    assert!(m
        .files
        .iter()
        .any(|f| f.path == "core/web/relay/app.css" && f.op == Op::Replace));
    // the same input packs to the same files
    let again = w.base.join("again.zip");
    let cores = vec![
        ("x86_64".to_string(), w.base.join("rel-0.2.0/x86_64")),
        ("aarch64".to_string(), w.base.join("rel-0.2.0/aarch64")),
    ];
    pack::pack(
        &Input {
            component: "relay".into(),
            version: "0.2.0".into(),
            released_at: "t2".into(),
            cores,
            base: Some(w.v1_files.clone()),
            migrations: Some(w.base.join("migrations")),
            restart: vec!["taro-relay".into()],
            source_commit: Some("abc123".into()),
        },
        &w.key,
        &again,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&again).unwrap(),
        std::fs::read(&w.v2_zip).unwrap()
    );
}

#[test]
fn an_update_replaces_the_core_and_keeps_the_provider() {
    let w = world("apply");
    let before = preserved(&w);
    let mut hooks = Fake::ok();
    let r = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut hooks,
        &Options {
            now: "2026-10-17T01:00:00Z".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((r.from.as_str(), r.to.as_str()), ("0.1.0", "0.2.0"));
    let core = w.layout.core();
    assert_eq!(read(&core, "bin/taro-relay"), "relay binary 0.2.0 x86_64");
    assert!(read(&core, "web/relay/index.html").contains("dashboard-v2"));
    assert!(read(&core, "web/relay/app.css").contains("dashboard-v2"));
    assert!(core.join("web/relay/nav.js").exists());
    assert!(!core.join("share/old-notes.txt").exists());
    // app.js did not change between the releases: not shipped, not touched
    assert!(!r.written.iter().any(|p| p.ends_with("app.js")));
    assert_eq!(
        read(&core, "web/relay/app.js"),
        "console.log('shared, unchanged')\n"
    );
    assert_preserved(&before);
    // the migration added the new setting and kept the operator's
    let t = read(&w.layout.runtime, "relay.toml");
    assert!(
        t.contains("# set by Wizard 2")
            && t.contains("mode = \"manual\"")
            && t.contains("check = \"daily\""),
        "{t}"
    );
    let inst = apply::installed(&w.layout).unwrap();
    assert_eq!(inst.version, "0.2.0");
    assert_eq!(inst.migrations, vec!["0002-update-check".to_string()]);
    assert!(
        inst.files.contains_key("core/web/relay/nav.js")
            && !inst.files.contains_key("core/share/old-notes.txt")
    );
    assert_eq!(hooks.restarts, vec![vec!["taro-relay".to_string()]]);
    assert_eq!(hooks.checked, vec!["0.2.0".to_string()]);
    // the same update again: already installed, nothing happens
    assert!(matches!(
        apply::apply(
            &w.layout,
            &w.v2_zip,
            &w.keys,
            &mut Fake::ok(),
            &Options::default()
        ),
        Err(Error::AlreadyCurrent(_))
    ));
    // and the operator can go back
    let msg = apply::rollback_last(&w.layout, &mut Fake::ok(), "t9").unwrap();
    assert!(msg.contains("0.1.0"), "{msg}");
    assert!(read(&core, "web/relay/index.html").contains("dashboard-v1"));
    assert!(core.join("share/old-notes.txt").exists() && !core.join("web/relay/nav.js").exists());
    assert_eq!(apply::installed(&w.layout).unwrap().version, "0.1.0");
    assert!(read(&w.layout.runtime, "relay.toml")
        .find("check")
        .is_none());
    assert_preserved(&before);
}

#[test]
fn a_failed_health_check_restores_the_previous_core() {
    let w = world("health");
    let before = preserved(&w);
    let old_css = read(&w.layout.core(), "web/relay/app.css");
    let mut hooks = Fake {
        fail_health: Some("0.2.0".into()),
        ..Fake::ok()
    };
    let e = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut hooks,
        &Options::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            Error::Failed {
                rolled_back: true,
                ..
            }
        ),
        "{e}"
    );
    assert_eq!(read(&w.layout.core(), "web/relay/app.css"), old_css);
    assert_eq!(
        read(&w.layout.core(), "bin/taro-relay"),
        "relay binary 0.1.0 x86_64"
    );
    assert!(!w.layout.core().join("web/relay/nav.js").exists());
    assert!(w.layout.core().join("share/old-notes.txt").exists());
    assert_eq!(apply::installed(&w.layout).unwrap().version, "0.1.0");
    assert!(!read(&w.layout.runtime, "relay.toml").contains("check"));
    assert_preserved(&before);
    // restarted twice: the new version, then the old one again (healthy)
    assert_eq!(hooks.restarts.len(), 2);
    assert_eq!(
        hooks.checked,
        vec!["0.2.0".to_string(), "0.1.0".to_string()]
    );
    let h = apply::read_history(&w.layout);
    assert_eq!(h.last().unwrap().result, "rolled_back");
    assert!(!w.layout.update_state().join("journal.json").exists());
}

#[test]
fn an_interrupted_update_is_rolled_back_on_the_next_run() {
    let w = world("interrupted");
    let before = preserved(&w);
    // the update ran up to the swap and the machine lost power: simulate by applying with a
    // health failure but deleting the backup restore… simpler: apply, then put a journal back
    let mut hooks = Fake::ok();
    let r = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut hooks,
        &Options::default(),
    )
    .unwrap();
    std::fs::write(
        w.layout.update_state().join("journal.json"),
        serde_json::json!({"from":"0.1.0","to":"0.2.0","backup":r.backup}).to_string(),
    )
    .unwrap();
    let what = apply::recover(&w.layout, &mut Fake::ok(), "t5")
        .unwrap()
        .unwrap();
    assert!(what.contains("interrupted"), "{what}");
    assert_eq!(apply::installed(&w.layout).unwrap().version, "0.1.0");
    assert!(read(&w.layout.core(), "web/relay/index.html").contains("dashboard-v1"));
    assert_preserved(&before);
}

#[test]
fn hand_changed_core_files_are_not_overwritten_silently() {
    let w = world("conflict");
    put(
        &w.layout.core(),
        "web/relay/app.css",
        ".dashboard-v1{color:hotpink} /* edited on the server */\n",
    );
    let e = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut Fake::ok(),
        &Options::default(),
    )
    .unwrap_err();
    let Error::Conflicts(c) = &e else {
        panic!("{e}")
    };
    assert_eq!(
        c.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        vec!["core/web/relay/app.css"]
    );
    assert!(e.to_string().contains("/etc/taro/provider"));
    // nothing changed
    assert_eq!(apply::installed(&w.layout).unwrap().version, "0.1.0");
    assert!(read(&w.layout.core(), "web/relay/index.html").contains("dashboard-v1"));
    // with the operator's consent: kept aside, then updated
    let r = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut Fake::ok(),
        &Options {
            overwrite_modified: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.kept_aside, vec!["core/web/relay/app.css".to_string()]);
    assert!(read(
        &w.layout.update_state(),
        "conflicts/0.2.0/core/web/relay/app.css"
    )
    .contains("hotpink"));
    assert!(read(&w.layout.core(), "web/relay/app.css").contains("dashboard-v2"));
}

#[test]
fn incompatible_versions_are_refused() {
    let w = world("versions");
    // a server still on 0.0.9: 0.2.0 only applies to 0.1.0
    let mut inst = apply::installed(&w.layout).unwrap();
    inst.version = "0.0.9".into();
    std::fs::write(
        w.layout.installed_record(),
        serde_json::to_vec(&inst).unwrap(),
    )
    .unwrap();
    let e = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut Fake::ok(),
        &Options::default(),
    )
    .unwrap_err();
    assert!(
        matches!(e, Error::Incompatible(_)) && e.to_string().contains("in between"),
        "{e}"
    );
    // a newer server: never a downgrade
    inst.version = "0.3.0".into();
    std::fs::write(
        w.layout.installed_record(),
        serde_json::to_vec(&inst).unwrap(),
    )
    .unwrap();
    let e = apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut Fake::ok(),
        &Options::default(),
    )
    .unwrap_err();
    assert!(matches!(e, Error::Incompatible(_)), "{e}");
    assert_eq!(
        read(&w.layout.core(), "bin/taro-relay"),
        "relay binary 0.1.0 x86_64"
    );
}

/// A package written by hand: a manifest (signed with `key`) and entries as given.
fn forge(
    path: &Path,
    m: &UpdateManifest,
    key: &ed25519_dalek::SigningKey,
    entries: &[(&str, &[u8])],
    checksums: bool,
) {
    let mb = serde_json::to_vec_pretty(m).unwrap();
    let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let o = zip::write::SimpleFileOptions::default();
    let mut sums = String::new();
    let mut all: Vec<(String, Vec<u8>)> = vec![
        ("manifest.json".into(), mb.clone()),
        ("manifest.json.sig".into(), sig::sign(key, &mb).into_bytes()),
    ];
    all.extend(entries.iter().map(|(n, b)| (n.to_string(), b.to_vec())));
    for (n, b) in &all {
        z.start_file(n.as_str(), o).unwrap();
        z.write_all(b).unwrap();
        sums.push_str(&format!("{}  {n}\n", taro_update::sha256_hex(b)));
    }
    if checksums {
        z.start_file("checksums.sha256", o).unwrap();
        z.write_all(sums.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

fn manifest_for(path: &str, source: &str, bytes: &[u8]) -> UpdateManifest {
    UpdateManifest {
        schema: UPDATE_SCHEMA.into(),
        product: "taro-enterprise".into(),
        component: "relay".into(),
        version: "0.2.0".into(),
        min_from_version: "0.1.0".into(),
        max_from_version: "0.1.0".into(),
        released_at: "t".into(),
        permitted_paths: vec!["core/bin/".into(), "core/web/".into(), "core/share/".into()],
        files: vec![FileEntry {
            op: Op::Replace,
            path: path.into(),
            source: source.into(),
            sha256: taro_update::sha256_hex(bytes),
            size: bytes.len() as u64,
            mode: 0o644,
            arch: None,
        }],
        remove: vec![],
        migrations: vec![],
        restart: vec![],
        source_commit: None,
    }
}

#[test]
fn forged_tampered_and_escaping_packages_are_refused() {
    let w = world("refused");
    let before = preserved(&w);
    let refuse = |zip: &Path, what: &str| {
        let e = apply::apply(
            &w.layout,
            zip,
            &w.keys,
            &mut Fake::ok(),
            &Options::default(),
        )
        .unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{what}: {e}");
        e.to_string()
    };
    let bytes = b"evil";
    let z = w.base.join("x.zip");

    // signed by someone else
    forge(
        &z,
        &manifest_for(
            "core/web/relay/app.css",
            "payload/any/core/web/relay/app.css",
            bytes,
        ),
        &sig::generate(),
        &[("payload/any/core/web/relay/app.css", bytes)],
        true,
    );
    assert!(refuse(&z, "foreign key").contains("trusted"));

    // the payload changed after signing (its sha256 in the signed manifest no longer matches)
    let m = manifest_for(
        "core/web/relay/app.css",
        "payload/any/core/web/relay/app.css",
        bytes,
    );
    forge(
        &z,
        &m,
        &w.key,
        &[("payload/any/core/web/relay/app.css", b"EVIL")],
        true,
    );
    assert!(refuse(&z, "tampered payload").contains("match"));

    // a checksum line that does not match
    forge(
        &z,
        &m,
        &w.key,
        &[("payload/any/core/web/relay/app.css", bytes)],
        false,
    );
    refuse(&z, "no checksums");

    // paths out of the core, even when properly signed
    for target in [
        "../../etc/passwd",
        "/etc/passwd",
        "core/../../../etc/shadow",
        "provider/provider.json",
        "core/INSTALLED.json",
        "INSTALLED.json",
        "core/other/x",
    ] {
        let m = manifest_for(target, "payload/any/x", bytes);
        forge(&z, &m, &w.key, &[("payload/any/x", bytes)], true);
        refuse(&z, target);
    }
    // a zip entry escaping the folder
    let m = manifest_for("core/web/relay/app.css", "payload/../../../evil", bytes);
    forge(&z, &m, &w.key, &[("payload/../../../evil", bytes)], true);
    refuse(&z, "entry with ..");
    // something that is not part of an update
    let m = manifest_for(
        "core/web/relay/app.css",
        "payload/any/core/web/relay/app.css",
        bytes,
    );
    forge(
        &z,
        &m,
        &w.key,
        &[
            ("payload/any/core/web/relay/app.css", bytes),
            ("install.sh", b"rm -rf /"),
        ],
        true,
    );
    refuse(&z, "extra entry");
    // permitted paths widened by the manifest
    let mut m2 = m.clone();
    m2.permitted_paths.push("".into());
    forge(
        &z,
        &m2,
        &w.key,
        &[("payload/any/core/web/relay/app.css", bytes)],
        true,
    );
    refuse(&z, "widened permitted paths");
    // another product
    let mut m3 = m.clone();
    m3.product = "taro".into();
    forge(
        &z,
        &m3,
        &w.key,
        &[("payload/any/core/web/relay/app.css", bytes)],
        true,
    );
    refuse(&z, "another product");

    // not one of these touched the server
    assert_eq!(apply::installed(&w.layout).unwrap().version, "0.1.0");
    assert!(read(&w.layout.core(), "web/relay/app.css").contains("dashboard-v1"));
    assert!(!w.base.join("server/etc/passwd").exists());
    assert_preserved(&before);
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_refused() {
    let w = world("symlink");
    // in the package
    let z = w.base.join("link.zip");
    {
        let mb = serde_json::to_vec(&manifest_for(
            "core/web/relay/app.css",
            "payload/any/core/web/relay/app.css",
            b"x",
        ))
        .unwrap();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
        zw.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zw.write_all(&mb).unwrap();
        zw.add_symlink(
            "payload/any/core/web/relay/app.css",
            "/etc/passwd",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zw.finish().unwrap();
    }
    assert!(matches!(
        apply::apply(&w.layout, &z, &w.keys, &mut Fake::ok(), &Options::default()),
        Err(Error::Invalid(_))
    ));
    // on the server: a link planted inside the core
    let outside = w.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(w.layout.core().join("web")).unwrap();
    std::os::unix::fs::symlink(&outside, w.layout.core().join("web")).unwrap();
    assert!(apply::apply(
        &w.layout,
        &w.v2_zip,
        &w.keys,
        &mut Fake::ok(),
        &Options::default()
    )
    .is_err());
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing was written through the link"
    );
}
