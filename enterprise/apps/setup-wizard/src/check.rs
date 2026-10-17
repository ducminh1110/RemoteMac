//! Step 7: validation. Every produced file is opened again and compared with what the build
//! meant to write: the provider folder byte for byte (and through rm-brand's validation), each
//! archive's complete layout, the carried-over programs against the verified official files,
//! the provider manifest and `SHA256SUMS`. Nothing is taken on trust from the build itself
//! except the expected contents it recorded.

use crate::build::{DOWNLOADS, INSTALL, PROVIDER_MANIFEST, PROVIDER_MANIFEST_SCHEMA, SUMS};
use crate::package::{self, Contents, Format, Kind};
use crate::state::{BuildResult, PackageRecord};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, Serialize)]
pub struct Check {
    pub id: String,
    pub name: String,
    /// ok | fail
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub ok: bool,
    pub dir: String,
    pub checks: Vec<Check>,
    pub at: u64,
}

struct Checks(Vec<Check>);

impl Checks {
    fn push(&mut self, id: &str, name: &str, r: Result<String, String>) {
        let (status, detail) = match r {
            Ok(d) => ("ok", d),
            Err(d) => ("fail", d),
        };
        self.0.push(Check {
            id: id.into(),
            name: name.into(),
            status: status.into(),
            detail,
        });
    }
}

fn sha(b: &[u8]) -> String {
    taro_update::sha256_hex(b)
}

/// provider.json bytes: identical to the canonical file, valid for rm-brand, tokens derived.
fn provider_ok(bytes: &[u8], canonical: &[u8]) -> Result<String, String> {
    if bytes != canonical {
        return Err(format!(
            "differs from the canonical provider.json (sha256 {} instead of {})",
            sha(bytes),
            sha(canonical)
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let p = rm_brand::Provider::from_json(text)?;
    if p.branding.tokens.as_ref() != Some(&p.tokens()) {
        return Err("branding.tokens are missing or not derived from the primary colour".into());
    }
    Ok(format!(
        "byte-identical (sha256 {}), valid {} ({} {})",
        &sha(bytes)[..12],
        rm_brand::SCHEMA,
        p.product.name,
        p.branding.primary_color
    ))
}

fn read(path: &Path, fmt: Format) -> Result<Contents, String> {
    let cap = |n: &str| n.contains("/provider/") || n.ends_with("README.txt");
    match fmt {
        Format::Zip => package::read_zip(path, &cap),
        Format::TarGz => package::read_tgz(path, &cap),
    }
}

fn layout_ok(rec: &PackageRecord, c: &Contents) -> Result<String, String> {
    let want: BTreeSet<&String> = rec.built.entries.keys().collect();
    let have: BTreeSet<&String> = c.entries.keys().collect();
    let missing: Vec<&&String> = want.difference(&have).take(5).collect();
    let extra: Vec<&&String> = have.difference(&want).take(5).collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(format!("missing {missing:?}, unexpected {extra:?}"));
    }
    for (p, e) in &rec.built.entries {
        let a = &c.entries[p];
        if a.kind != e.kind {
            return Err(format!("{p}: {:?} instead of {:?}", a.kind, e.kind));
        }
        if e.sha256.is_some() && a.sha256 != e.sha256 {
            return Err(format!("{p}: contents differ from what was built"));
        }
        if e.mode.is_some() && a.mode != e.mode {
            return Err(format!(
                "{p}: mode {:o} instead of {:o}",
                a.mode.unwrap_or(0),
                e.mode.unwrap_or(0)
            ));
        }
        if e.link != a.link {
            return Err(format!("{p}: link target differs"));
        }
    }
    Ok(format!(
        "{} entries, top folder {}/, as expected",
        c.entries.len(),
        rec.built.top
    ))
}

/// Every file carried over is the official file (sha256 of the verified download's entry).
fn carried_ok(rec: &PackageRecord, c: &Contents) -> Result<String, String> {
    let mut n = 0;
    for (p, e) in &rec.built.entries {
        let Some(src) = e.origin.strip_prefix("download:") else {
            continue;
        };
        if e.kind != Kind::File {
            continue;
        }
        let official = rec
            .source_files
            .get(src)
            .ok_or_else(|| format!("{p}: {src} is not in the verified download"))?;
        let actual = c.entries.get(p).and_then(|a| a.sha256.as_ref());
        if actual != Some(official) {
            return Err(format!(
                "{p} is not byte-identical to {src} of the verified download"
            ));
        }
        n += 1;
    }
    let bins: Vec<&str> = rec
        .built
        .binaries
        .iter()
        .map(|(p, _)| p.rsplit('/').next().unwrap_or(p))
        .collect();
    Ok(format!(
        "{n} files byte-identical to the verified download (programs: {})",
        if bins.is_empty() {
            "-".to_string()
        } else {
            bins.join(", ")
        }
    ))
}

fn exec_ok(rec: &PackageRecord, c: &Contents) -> Result<String, String> {
    let mut n = 0;
    for (p, e) in &rec.built.entries {
        if e.kind == Kind::File && e.mode.is_some_and(|m| m & 0o111 != 0) {
            let m = c.entries.get(p).and_then(|a| a.mode).unwrap_or(0);
            if m & 0o111 == 0 {
                return Err(format!("{p} lost its executable bit"));
            }
            n += 1;
        }
    }
    if rec.component.id == "macos-client" {
        for f in ["taro", "taro.sh", "taro-update.sh"] {
            let p = format!("{}/{f}", rec.built.top);
            if c.entries.get(&p).and_then(|a| a.mode).unwrap_or(0) & 0o111 == 0 {
                return Err(format!("{p} is not executable"));
            }
        }
    }
    Ok(format!("{n} executable file(s) keep their mode"))
}

fn sums_ok(dir: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(dir.join(SUMS)).map_err(|e| format!("{SUMS}: {e}"))?;
    let mut listed = BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (h, f) = line
            .split_once("  ")
            .ok_or_else(|| format!("bad line {line:?}"))?;
        crate::package::check_path(f)?;
        let actual = taro_update::sha256_file(&dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
        if actual != h {
            return Err(format!("{f}: sha256 does not match"));
        }
        listed.insert(f.to_string());
    }
    let mut present = BTreeSet::new();
    walk(dir, dir, &mut present)?;
    present.remove(SUMS);
    if listed != present {
        let a: Vec<_> = present.difference(&listed).collect();
        let b: Vec<_> = listed.difference(&present).collect();
        return Err(format!("not listed: {a:?}; listed but missing: {b:?}"));
    }
    Ok(format!("{} files listed, all match", listed.len()))
}

fn walk(root: &Path, d: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
    for e in std::fs::read_dir(d).map_err(|e| format!("{}: {e}", d.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let ft = e.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            walk(root, &e.path(), out)?;
        } else {
            let rel = e
                .path()
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
    Ok(())
}

fn manifest_ok(dir: &Path, r: &BuildResult) -> Result<String, String> {
    let v: Value = serde_json::from_slice(
        &std::fs::read(dir.join(PROVIDER_MANIFEST)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if v["schema"] != PROVIDER_MANIFEST_SCHEMA {
        return Err(format!("schema is not {PROVIDER_MANIFEST_SCHEMA}"));
    }
    if v["provider_json_sha256"].as_str() != Some(sha(&r.provider_json).as_str()) {
        return Err("provider_json_sha256 does not match provider.json".into());
    }
    let comps = v["components"].as_array().ok_or("no components")?;
    for c in comps {
        let a = c["artifact"]
            .as_str()
            .ok_or("component without an artifact")?;
        package::check_path(a)?;
        let h = taro_update::sha256_file(&dir.join(a)).map_err(|e| format!("{a}: {e}"))?;
        if c["artifact_sha256"].as_str() != Some(h.as_str()) {
            return Err(format!("{a}: sha256 differs from the manifest"));
        }
    }
    Ok(format!("{} components, hashes match", comps.len()))
}

/// The relay key only appears in provider.json files.
fn no_key_elsewhere(
    dir: &Path,
    key: &str,
    texts: &BTreeMap<String, Vec<u8>>,
) -> Result<String, String> {
    let k = key.as_bytes();
    let has = |b: &[u8]| !k.is_empty() && b.windows(k.len()).any(|w| w == k);
    for f in [PROVIDER_MANIFEST, INSTALL, SUMS] {
        let b = std::fs::read(dir.join(f)).map_err(|e| e.to_string())?;
        if has(&b) {
            return Err(format!("{f} contains the relay key"));
        }
    }
    for (n, b) in texts {
        if !n.ends_with(rm_brand::FILE) && has(b) {
            return Err(format!("{n} contains the relay key"));
        }
    }
    Ok("the relay key is only in provider.json".into())
}

pub fn run(r: &BuildResult) -> Report {
    let dir = Path::new(&r.dir);
    let mut c = Checks(Vec::new());
    let pdir = dir.join(rm_brand::DIR);
    c.push(
        "provider",
        "provider/provider.json",
        std::fs::read(pdir.join(rm_brand::FILE))
            .map_err(|e| e.to_string())
            .and_then(|b| provider_ok(&b, &r.provider_json)),
    );
    for (n, want) in [("logo.png", &r.logo), ("icon.png", &r.icon)] {
        if let Some(want) = want {
            c.push(
                &format!("provider-{n}"),
                &format!("provider/{n}"),
                match std::fs::read(pdir.join(n)) {
                    Ok(b) if &b == want => Ok(format!("byte-identical ({} bytes)", b.len())),
                    Ok(_) => Err("differs from the uploaded image".into()),
                    Err(e) => Err(e.to_string()),
                },
            );
        }
    }
    let key = rm_brand::Provider::from_json(&String::from_utf8_lossy(&r.provider_json))
        .map(|p| p.relay.key)
        .unwrap_or_default();
    let mut texts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for rec in &r.packages {
        let path = dir.join(&rec.artifact);
        let id = &rec.component.id;
        let art = r.artifacts.iter().find(|a| a.path == rec.artifact);
        c.push(
            &format!("{id}:file"),
            &format!("{}: file", rec.artifact),
            match (taro_update::sha256_file(&path), art) {
                (Ok(h), Some(a)) if h == a.sha256 => Ok(format!(
                    "{} ({})",
                    &h[..12],
                    crate::util::human_size(a.size)
                )),
                (Ok(_), _) => Err("sha256 changed since the build".into()),
                (Err(e), _) => Err(e.to_string()),
            },
        );
        let contents = match read(&path, rec.built.format) {
            Ok(x) => x,
            Err(e) => {
                c.push(
                    &format!("{id}:open"),
                    &format!("{}: opens", rec.artifact),
                    Err(e),
                );
                continue;
            }
        };
        c.push(
            &format!("{id}:layout"),
            &format!("{}: layout", rec.artifact),
            layout_ok(rec, &contents),
        );
        let pj = format!("{}/provider/{}", rec.built.top, rm_brand::FILE);
        c.push(
            &format!("{id}:provider"),
            &format!("{}: provider.json", rec.artifact),
            contents
                .captured
                .get(&pj)
                .ok_or_else(|| format!("{pj} is missing"))
                .and_then(|b| provider_ok(b, &r.provider_json)),
        );
        c.push(
            &format!("{id}:binaries"),
            &format!("{}: official files unchanged", rec.artifact),
            carried_ok(rec, &contents),
        );
        if rec.built.format == Format::TarGz {
            c.push(
                &format!("{id}:modes"),
                &format!("{}: executable modes", rec.artifact),
                exec_ok(rec, &contents),
            );
        }
        for (n, b) in contents.captured {
            texts.insert(format!("{}:{n}", rec.artifact), b);
        }
    }
    c.push("manifest", PROVIDER_MANIFEST, manifest_ok(dir, r));
    c.push("sums", SUMS, sums_ok(dir));
    c.push(
        "secrets",
        "No secret outside provider.json",
        no_key_elsewhere(dir, &key, &texts),
    );
    let leftovers = {
        let partial = dir.with_file_name(format!("{}.partial", r.name));
        if partial.exists() || dir.join(DOWNLOADS).exists() {
            Err("temporary files are left".to_string())
        } else {
            Ok("no temporary folders left".to_string())
        }
    };
    c.push("clean", "Output is complete", leftovers);
    let ok = c.0.iter().all(|x| x.status == "ok");
    Report {
        ok,
        dir: r.dir.clone(),
        checks: c.0,
        at: crate::util::now_secs(),
    }
}
