//! The whole wizard through its API against the fake GitHub: every expected file, the provider
//! folder byte-identical everywhere, the official programs unchanged, modes kept, SHA256SUMS
//! right, no secret anywhere it should not be; and the refusals (tampered download, bad
//! signature, a host outside the policy, a redirect outside it, cancel) leave no output.

mod common;
use common::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

fn zip_entries(p: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut m = BTreeMap::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        let mut b = Vec::new();
        e.read_to_end(&mut b).unwrap();
        m.insert(e.name().unwrap().to_string(), b);
    }
    m
}

/// path -> (mode, bytes, link)
fn tar_entries(p: &Path) -> BTreeMap<String, (u32, Vec<u8>, Option<String>)> {
    let f = std::fs::File::open(p).unwrap();
    let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut m = BTreeMap::new();
    for e in a.entries().unwrap() {
        let mut e = e.unwrap();
        let path = e.path().unwrap().to_str().unwrap().to_string();
        let mode = e.header().mode().unwrap() & 0o7777;
        let link = e
            .link_name()
            .unwrap()
            .map(|l| l.to_str().unwrap().to_string());
        let mut b = Vec::new();
        e.read_to_end(&mut b).unwrap();
        m.insert(path, (mode, b, link));
    }
    m
}

fn contains(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    hay.windows(n.len()).any(|w| w == n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_build_produces_a_verified_distribution() {
    let w = wizard("flow").await;
    let (s, _) = w.post("/api/preflight/run", json!({})).await;
    assert_eq!(s, 200);
    let st = w.state().await;
    let checks: Vec<(&str, &str)> = st["preflight"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["id"].as_str().unwrap(), c["status"].as_str().unwrap()))
        .collect();
    for id in ["os", "github", "objects", "disk", "loopback"] {
        let c = checks
            .iter()
            .find(|c| c.0 == id)
            .unwrap_or_else(|| panic!("{id}: {checks:?}"));
        assert_ne!(c.1, "fail", "{id}: {}", st["preflight"]);
    }

    w.fill().await;
    let st = w.state().await;
    // newest first, drafts and plain Taro releases left out
    let tags: Vec<&str> = st["components"]["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["tag"].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["enterprise-v0.2.0", "enterprise-v0.1.0"]);
    // the key is never in the state, only masked
    let key = w.relay_key().await;
    assert_eq!(key.len(), 32);
    assert!(rm_brand::valid_relay_key(&key));
    assert!(!st.to_string().contains(&key));
    assert!(st["relay"]["key_masked"]
        .as_str()
        .unwrap()
        .ends_with(&key[28..]));

    let (s, v) = w.select("enterprise-v0.2.0").await;
    assert_eq!(s, 200, "{v}");
    let sel = &v["state"]["components"]["selected"];
    assert_eq!(sel["taro_version"], TARO);
    assert_eq!(sel["relay_version"], RELAY);
    assert_eq!(sel["components"].as_array().unwrap().len(), 4);
    assert!(sel["components"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["url_ok"] == true));

    let (s, v) = w.build().await;
    assert_eq!(s, 200, "{v}");
    let st = w.wait_build().await;
    assert!(st["build"]["error"].is_null(), "{}", st["build"]);
    let r = &st["build"]["result"];
    let dir = Path::new(r["dir"].as_str().unwrap()).to_path_buf();
    assert_eq!(dir.file_name().unwrap(), "acme-cloud-1.4.1");
    assert_eq!(st["validation"]["ok"], true, "{}", st["validation"]);
    assert!(st["validation"]["checks"].as_array().unwrap().len() >= 20);
    assert_eq!(st["stale"], false);

    // every expected file, nothing else
    let names = w.out_names();
    let want: BTreeSet<&str> = [
        "acme-cloud-1.4.1/",
        "acme-cloud-1.4.1/INSTALL.md",
        "acme-cloud-1.4.1/SHA256SUMS",
        "acme-cloud-1.4.1/provider-manifest.json",
        "acme-cloud-1.4.1/provider/",
        "acme-cloud-1.4.1/provider/provider.json",
        "acme-cloud-1.4.1/provider/logo.png",
        "acme-cloud-1.4.1/provider/icon.png",
        "acme-cloud-1.4.1/windows/",
        "acme-cloud-1.4.1/windows/Acme-1.4.1-windows-x64.zip",
        "acme-cloud-1.4.1/macos/",
        "acme-cloud-1.4.1/macos/Acme-1.4.1-macos.tar.gz",
        "acme-cloud-1.4.1/linux/",
        "acme-cloud-1.4.1/linux/acme-cloud-relay-0.2.0-linux-x86_64.tar.gz",
        "acme-cloud-1.4.1/linux/acme-cloud-relay-0.2.0-linux-aarch64.tar.gz",
    ]
    .into();
    let have: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(have, want);

    // provider.json: valid, the custom colour unchanged (no snapping), tokens derived
    let pj = std::fs::read(dir.join("provider/provider.json")).unwrap();
    let p = rm_brand::Provider::from_json(std::str::from_utf8(&pj).unwrap()).unwrap();
    assert_eq!(p.branding.primary_color, "#3D7A5B");
    assert_eq!(
        p.branding.tokens,
        Some(rm_brand::Tokens::derive(rm_brand::Color(0x3d, 0x7a, 0x5b)))
    );
    assert_eq!(p.relay.key, key);
    assert_eq!(p.relay.address(), "relay.acme.example:7470");
    assert_eq!(p.branding.logo.as_deref(), Some("logo.png"));
    assert_eq!(p.branding.icon.as_deref(), Some("icon.png"));
    let g = p.generated.as_ref().unwrap();
    assert!(g.by.starts_with("TaroEnterpriseSetup "));
    assert_eq!(g.taro_version, TARO);
    assert_eq!(g.enterprise_version.as_deref(), Some(ENT));
    assert_eq!(pj, p.clone().normalized().to_json().into_bytes());

    // Windows: renamed top folder and exe, exe unchanged, provider folder identical
    let z = zip_entries(&dir.join("windows/Acme-1.4.1-windows-x64.zip"));
    let zn: BTreeSet<&str> = z.keys().map(String::as_str).collect();
    let zwant: BTreeSet<&str> = [
        "Acme/",
        "Acme/Acme.exe",
        "Acme/README.txt",
        "Acme/licenses/THIRD-PARTY.txt",
        "Acme/provider/",
        "Acme/provider/provider.json",
        "Acme/provider/logo.png",
        "Acme/provider/icon.png",
    ]
    .into();
    assert_eq!(zn, zwant);
    assert_eq!(z["Acme/Acme.exe"], TARO_EXE);
    assert_eq!(z["Acme/provider/provider.json"], pj);
    assert_eq!(
        z["Acme/provider/logo.png"],
        std::fs::read(dir.join("provider/logo.png")).unwrap()
    );
    let readme = String::from_utf8(z["Acme/README.txt"].clone()).unwrap();
    assert!(
        readme.contains("Acme Connect for Windows") && readme.contains("relay.acme.example:7470")
    );
    assert!(readme.contains("\r\n"));

    // macOS: programs unchanged and executable, junk dropped
    let m = tar_entries(&dir.join("macos/Acme-1.4.1-macos.tar.gz"));
    for (f, b) in [
        ("taro", MAC_BIN),
        ("taro.sh", TARO_SH),
        ("taro-update.sh", TARO_UPDATE_SH),
    ] {
        let e = &m[&format!("Acme/{f}")];
        assert_eq!(e.1, b, "{f}");
        assert_eq!(e.0, 0o755, "{f} mode");
    }
    assert!(!m.keys().any(|k| k.contains("._")), "{:?}", m.keys());
    assert_eq!(m["Acme/provider/provider.json"].1, pj);
    assert_eq!(m["Acme/provider/provider.json"].0, 0o644);
    assert!(String::from_utf8_lossy(&m["Acme/README.txt"].1).contains("Acme Connect for macOS"));

    // relays: the same tree, provider filled, binaries and modes and links kept
    for arch in ["x86_64", "aarch64"] {
        let t = tar_entries(&dir.join(format!("linux/acme-cloud-relay-0.2.0-linux-{arch}.tar.gz")));
        let top = relay_top(arch);
        let src = tar_entries_bytes(&w.sources[&format!("{top}.tar.gz")]);
        for (path, (mode, bytes, link)) in &src {
            let o = t.get(path).unwrap_or_else(|| panic!("{path} missing"));
            assert_eq!(&o.1, bytes, "{path}");
            assert_eq!(o.0, *mode, "{path} mode");
            assert_eq!(&o.2, link, "{path} link");
        }
        assert_eq!(t[&format!("{top}/core/bin/taro-relay")].0, 0o755);
        assert_eq!(t[&format!("{top}/install.sh")].0, 0o755);
        assert_eq!(t[&format!("{top}/provider/provider.json")].1, pj);
        assert_eq!(t[&format!("{top}/provider/provider.json")].0, 0o640);
        assert_eq!(t[&format!("{top}/provider/icon.png")].0, 0o644);
        assert_eq!(t[&format!("{top}/README.md")].1, b"# taro-relay\n");
        // the official tree plus exactly the three provider files
        let extra: BTreeSet<&String> = t.keys().filter(|k| !src.contains_key(*k)).collect();
        let want: BTreeSet<String> = ["provider.json", "logo.png", "icon.png"]
            .iter()
            .map(|f| format!("{top}/provider/{f}"))
            .collect();
        assert_eq!(extra, want.iter().collect());
    }

    // SHA256SUMS lists every other file, and each matches
    let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).unwrap();
    let mut listed = BTreeSet::new();
    for l in sums.lines() {
        let (h, f) = l.split_once("  ").unwrap();
        assert_eq!(taro_update::sha256_file(&dir.join(f)).unwrap(), h, "{f}");
        listed.insert(f.to_string());
    }
    let files: BTreeSet<String> = want
        .iter()
        .filter(|n| !n.ends_with('/') && !n.ends_with("SHA256SUMS"))
        .map(|n| n.trim_start_matches("acme-cloud-1.4.1/").to_string())
        .collect();
    assert_eq!(listed, files);

    // provider-manifest.json
    let pm: Value =
        serde_json::from_slice(&std::fs::read(dir.join("provider-manifest.json")).unwrap())
            .unwrap();
    assert_eq!(pm["schema"], "taro.provider-distribution/v1");
    assert_eq!(pm["provider_id"], "acme-cloud");
    assert_eq!(pm["provider_json_sha256"], taro_update::sha256_hex(&pj));
    assert_eq!(pm["taro_version"], TARO);
    assert_eq!(pm["enterprise_version"], ENT);
    assert_eq!(pm["compatibility"]["client_version"], TARO);
    assert_eq!(pm["compatibility"]["relay_version"], RELAY);
    assert_eq!(pm["options"]["client_updates"], "notify");
    let comps = pm["components"].as_array().unwrap();
    assert_eq!(comps.len(), 4);
    for c in comps {
        let a = c["artifact"].as_str().unwrap();
        assert_eq!(
            c["artifact_sha256"].as_str().unwrap(),
            taro_update::sha256_file(&dir.join(a)).unwrap()
        );
        let src = &w.sources[c["source"]["file"].as_str().unwrap()];
        assert_eq!(c["source"]["sha256"], taro_update::sha256_hex(src));
        assert!(c["source"]["url"]
            .as_str()
            .unwrap()
            .starts_with("https://github.com/"));
    }
    let win = comps.iter().find(|c| c["id"] == "windows-client").unwrap();
    assert_eq!(win["binaries"][0]["path"], "Acme/Acme.exe");
    assert_eq!(
        win["binaries"][0]["sha256"],
        taro_update::sha256_hex(TARO_EXE)
    );

    // INSTALL.md says what to do and what was not done
    let inst = std::fs::read_to_string(dir.join("INSTALL.md")).unwrap();
    for s in [
        "sudo ./install.sh",
        "./taro.sh --password",
        "taro-relay update",
        "notarized",
        "Acme\\Acme.exe",
        "taro-relay-0.2.0-linux-x86_64",
    ] {
        assert!(inst.contains(s), "INSTALL.md lacks {s}");
    }

    // nothing temporary is left
    assert!(!w.out.join("acme-cloud-1.4.1.partial").exists());
    assert!(!dir.join(".downloads").exists());

    // only the fake GitHub was asked, nothing else
    for r in w.fake.reqs() {
        assert!(
            r.starts_with("/gh-api/") || r.starts_with("/gh-web/") || r.starts_with("/objects/"),
            "{r}"
        );
    }
    // each component came through the github.com -> objects redirect
    assert!(w
        .fake
        .reqs()
        .iter()
        .any(|r| r.starts_with("/objects/assets/Taro-windows.zip")));

    // open folder
    let (s, _) = w.post("/api/output/open", json!({})).await;
    assert_eq!(s, 200);
    assert_eq!(w.opened.lock().unwrap().clone(), vec![dir.clone()]);

    // summaries: the key only masked
    let (_, sj) = w.call("GET", "/api/summary.json", None).await;
    assert_eq!(sj["relay"]["admission_key"], st["relay"]["key_masked"]);
    assert_eq!(sj["branding"]["primary_color"], "#3D7A5B");
    let (_, md) = w.call("GET", "/api/summary.md", None).await;
    assert!(md.as_str().unwrap().contains("#3D7A5B"));
    let (_, diag) = w.call("GET", "/api/diagnostics.txt", None).await;
    assert!(diag.as_str().unwrap().contains("validation: ok"));

    // the relay key appears in no response but the reveal, in no file but provider.json
    let reveal_count = w
        .responses
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.contains(&key))
        .count();
    assert_eq!(
        reveal_count, 1,
        "the key must only be in the one reveal response"
    );
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(ACCESS), "access token in a response");
        assert!(
            !body.contains("[REDACTED]"),
            "a response needed scrubbing: {body}"
        );
    }
    for f in ["INSTALL.md", "SHA256SUMS", "provider-manifest.json"] {
        assert!(!contains(&std::fs::read(dir.join(f)).unwrap(), &key), "{f}");
    }
    for (n, b) in z.iter() {
        if !n.ends_with("provider.json") {
            assert!(!contains(b, &key), "{n}");
        }
    }
    for (n, (_, b, _)) in m.iter() {
        if !n.ends_with("provider.json") {
            assert!(!contains(b, &key), "{n}");
        }
    }

    // a later change marks the build stale; building again needs "replace"
    let (s, _) = w
        .post(
            "/api/branding",
            json!({"primary_color": "#2563EB", "theme": "dark"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(w.state().await["stale"], true);
    let (s, v) = w.build().await;
    assert_eq!(s, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("already exists"));
    let (s, v) = w
        .post(
            "/api/build/start",
            json!({"output_dir": w.out.display().to_string(), "replace": true}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let st = w.wait_build().await;
    assert_eq!(st["validation"]["ok"], true, "{}", st["validation"]);
    let p2 = rm_brand::Provider::load(&dir.join("provider"))
        .unwrap()
        .unwrap();
    assert_eq!(p2.branding.primary_color, "#2563EB");
    assert_eq!(p2.relay.key, key, "the key stays the same across rebuilds");
    // the replaced build is gone, nothing but the new one is left
    let tops: BTreeSet<String> = w
        .out_names()
        .into_iter()
        .filter_map(|n| n.split('/').next().map(str::to_string))
        .collect();
    assert_eq!(tops, ["acme-cloud-1.4.1".to_string()].into());
    let _ = std::fs::remove_dir_all(&w.out);
}

fn tar_entries_bytes(b: &[u8]) -> BTreeMap<String, (u32, Vec<u8>, Option<String>)> {
    let d = tmp("src");
    let p = d.join("src.tar.gz");
    std::fs::write(&p, b).unwrap();
    let m = tar_entries(&p);
    let _ = std::fs::remove_dir_all(&d);
    m
}

/// No folder of the provider's name, final or partial, is left.
fn assert_no_output(w: &Wiz) {
    let names = w.out_names();
    assert!(names.is_empty(), "left behind: {names:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_download_is_refused_and_leaves_nothing() {
    let w = wizard("tamper").await;
    w.fill().await;
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    w.fake
        .tamper
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(w.build().await.0, 200);
    let st = w.wait_build().await;
    let e = st["build"]["error"].as_str().unwrap();
    assert!(e.contains("sha256") && e.contains("does not match"), "{e}");
    assert!(st["build"]["result"].is_null());
    assert_eq!(st["steps"][5]["status"], "fail");
    assert_no_output(&w);
    let _ = std::fs::remove_dir_all(&w.out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_signature_is_refused() {
    let w = wizard("badsig").await;
    w.fill().await;
    forge_signature(&w.fake, "enterprise-v0.2.0");
    let (s, v) = w.select("enterprise-v0.2.0").await;
    assert_eq!(s, 400, "{v}");
    assert!(
        v["error"]
            .as_str()
            .unwrap()
            .contains("not by a trusted Taro release key"),
        "{v}"
    );
    let st = w.state().await;
    assert!(st["components"]["selected"].is_null());
    let (s, v) = w.build().await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(
        w.fake.downloads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "only the manifest and its signature were fetched"
    );
    assert_no_output(&w);
    let _ = std::fs::remove_dir_all(&w.out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_component_on_another_host_is_refused() {
    let w = wizard("evilhost").await;
    w.fill().await;
    let (s, v) = w.select("enterprise-v0.1.0").await;
    assert_eq!(s, 200, "{v}");
    let win = v["state"]["components"]["selected"]["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "windows-client")
        .unwrap()
        .clone();
    assert_eq!(win["url_ok"], false);
    assert!(win["url_error"]
        .as_str()
        .unwrap()
        .contains("evil.example.com is not an allowed download host"));
    assert_eq!(v["state"]["steps"][4]["status"], "warn");
    assert_eq!(w.build().await.0, 200);
    let st = w.wait_build().await;
    let e = st["build"]["error"].as_str().unwrap();
    assert!(
        e.contains("not an allowed download host") && e.contains("nothing was downloaded"),
        "{e}"
    );
    let before = w.fake.downloads.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(before, 2, "no component was fetched");
    assert_no_output(&w);

    // the same through a redirect: github.com sends the client somewhere else
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    w.fake
        .evil_redirect
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(w.build().await.0, 200);
    let st = w.wait_build().await;
    let e = st["build"]["error"].as_str().unwrap();
    assert!(
        e.contains("redirect refused") && e.contains("evil.example.com"),
        "{e}"
    );
    assert_no_output(&w);
    let _ = std::fs::remove_dir_all(&w.out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_the_build_and_cleans_up() {
    let w = wizard("cancel").await;
    w.fill().await;
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    w.fake.slow.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(w.build().await.0, 200);
    w.wait_for("a download to run", |s| {
        s["build"]["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|x| x["done"].as_u64().unwrap_or(0) > 0))
    })
    .await;
    // inputs are locked meanwhile
    let (s, _) = w
        .post(
            "/api/branding",
            json!({"primary_color": "#000000", "theme": "light"}),
        )
        .await;
    assert_eq!(s, 409);
    assert_eq!(w.build().await.0, 409);
    let (s, _) = w.post("/api/build/cancel", json!({})).await;
    assert_eq!(s, 200);
    let st = w.wait_build().await;
    assert_eq!(st["build"]["cancelled"], true);
    assert!(st["build"]["error"].is_null());
    assert_no_output(&w);
    let _ = std::fs::remove_dir_all(&w.out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subset_of_platforms_and_a_flat_windows_zip() {
    let w = wizard("subset").await;
    // the standard release's zip has its files at the root (Compress-Archive dist\Taro\*)
    let flat = windows_zip(true);
    {
        let mut f = w.fake.files.lock().unwrap();
        f.insert("/objects/assets/Taro-windows.zip".into(), flat.clone());
    }
    w.fill().await;
    // the manifest pins the folder layout's hash: re-sign one for the flat zip
    let m: Value = serde_json::from_slice(
        &w.fake.files.lock().unwrap()[&format!(
            "/gh-web/{REPO}/releases/download/enterprise-v0.2.0/taro-enterprise-components.json"
        )],
    )
    .unwrap();
    let mut m = m;
    for c in m["components"].as_array_mut().unwrap() {
        if c["id"] == "windows-client" {
            c["sha256"] = json!(taro_update::sha256_hex(&flat));
            c["size"] = json!(flat.len());
        }
    }
    let bytes = serde_json::to_vec(&m).unwrap();
    {
        let mut f = w.fake.files.lock().unwrap();
        f.insert(format!("/gh-web/{REPO}/releases/download/enterprise-v0.2.0/taro-enterprise-components.json.sig"), taro_update::sig::sign(&w.key, &bytes).into_bytes());
        f.insert(format!("/gh-web/{REPO}/releases/download/enterprise-v0.2.0/taro-enterprise-components.json"), bytes);
    }
    assert_eq!(w.select("enterprise-v0.2.0").await.0, 200);
    let (s, _) = w
        .post(
            "/api/components/platforms",
            json!({"platforms": ["windows-client", "linux-relay-x86_64"]}),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = w
        .post(
            "/api/components/platforms",
            json!({"platforms": ["windows-client", "solaris"]}),
        )
        .await;
    assert_eq!(s, 400);
    assert_eq!(w.build().await.0, 200);
    let st = w.wait_build().await;
    assert_eq!(st["validation"]["ok"], true, "{}", st["validation"]);
    let dir = Path::new(st["build"]["result"]["dir"].as_str().unwrap()).to_path_buf();
    assert!(!dir.join("macos").exists());
    assert!(!dir
        .join("linux/acme-cloud-relay-0.2.0-linux-aarch64.tar.gz")
        .exists());
    let z = zip_entries(&dir.join("windows/Acme-1.4.1-windows-x64.zip"));
    assert_eq!(z["Acme/Acme.exe"], TARO_EXE);
    assert!(z.contains_key("Acme/licenses/THIRD-PARTY.txt"));
    let _ = std::fs::remove_dir_all(&w.out);
}
