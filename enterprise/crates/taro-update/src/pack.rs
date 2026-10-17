//! Builds `enterprise-update.zip` (the release workflow runs this): the core files of the new
//! release that are new or changed since the previous release's core, the removals, the
//! migrations, a manifest signed with the release key, and `checksums.sha256`.
//!
//! Every path is checked against [`crate::policy`] here as well as when applying: a core tree
//! with a file outside what its component may change does not build.

use crate::manifest::*;
use crate::policy;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Input {
    pub component: String,
    pub version: String,
    pub released_at: String,
    /// (arch, the release's `core/` folder for it)
    pub cores: Vec<(String, PathBuf)>,
    /// the previous release's core (none: the first release, every file is new)
    pub base: Option<CoreFiles>,
    /// `NNNN-name.json` migration files to ship (see [`crate::migrate`])
    pub migrations: Option<PathBuf>,
    pub restart: Vec<String>,
    pub source_commit: Option<String>,
}

pub struct Output {
    pub manifest: UpdateManifest,
    pub core_files: CoreFiles,
}

fn mode_of(m: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o111 != 0 {
            return 0o755;
        }
    }
    let _ = m;
    0o644
}

/// The files of a `core/` folder as `core/<relative path>` -> (identity, absolute path).
pub fn scan_core(dir: &Path) -> Result<BTreeMap<String, (CoreFile, PathBuf)>, String> {
    fn walk(
        base: &Path,
        dir: &Path,
        out: &mut BTreeMap<String, (CoreFile, PathBuf)>,
    ) -> Result<(), String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            if m.file_type().is_symlink() {
                return Err(format!("{}: symbolic links are not shipped", p.display()));
            }
            if m.is_dir() {
                walk(base, &p, out)?;
                continue;
            }
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let path = format!("core/{rel}");
            let sha = crate::sha256_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            out.insert(
                path,
                (
                    CoreFile {
                        sha256: sha,
                        size: m.len(),
                        mode: mode_of(&m),
                    },
                    p,
                ),
            );
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// Build the package into `out` and sign its manifest with `key`.
pub fn pack(input: &Input, key: &ed25519_dalek::SigningKey, out: &Path) -> Result<Output, String> {
    let component = &input.component;
    let prefixes =
        policy::prefixes(component).ok_or_else(|| format!("unknown component {component}"))?;
    if crate::version::Version::parse(&input.version).is_none() {
        return Err(format!("{}: not a version", input.version));
    }
    if input.cores.is_empty() {
        return Err("no core folder given".into());
    }
    let mut trees = BTreeMap::new();
    for (arch, dir) in &input.cores {
        if !policy::ARCHES.contains(&arch.as_str()) {
            return Err(format!("unknown architecture {arch}"));
        }
        let t = scan_core(dir)?;
        for p in t.keys() {
            policy::check_target(component, p)?;
        }
        trees.insert(arch.clone(), t);
    }
    let arches: Vec<String> = trees.keys().cloned().collect();
    let empty = BTreeMap::new();
    let base_files = |arch: &str| {
        input
            .base
            .as_ref()
            .and_then(|b| b.files.get(arch))
            .unwrap_or(&empty)
    };
    if let Some(b) = &input.base {
        if b.component != *component || b.product != policy::PRODUCT {
            return Err("the previous core is another product or component".into());
        }
        if !crate::version::lt(&b.version, &input.version).unwrap_or(false) {
            return Err(format!(
                "the previous core ({}) is not older than {}",
                b.version, input.version
            ));
        }
    }

    // what changed, per arch: path -> (op, file, source path)
    let mut changed: BTreeMap<String, BTreeMap<String, (Op, CoreFile, PathBuf)>> = BTreeMap::new();
    let mut removed: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for arch in &arches {
        let new = &trees[arch];
        let old = base_files(arch);
        for (p, (f, src)) in new {
            match old.get(p) {
                Some(o) if o.sha256 == f.sha256 && o.mode == f.mode => {}
                Some(_) => {
                    changed
                        .entry(arch.clone())
                        .or_default()
                        .insert(p.clone(), (Op::Replace, f.clone(), src.clone()));
                }
                None => {
                    changed
                        .entry(arch.clone())
                        .or_default()
                        .insert(p.clone(), (Op::Add, f.clone(), src.clone()));
                }
            }
        }
        for (p, o) in old {
            if !new.contains_key(p) {
                removed
                    .entry(arch.clone())
                    .or_default()
                    .insert(p.clone(), o.sha256.clone());
            }
        }
    }

    // a change identical on every arch is shipped once, for all
    let all_paths: BTreeSet<String> = changed.values().flat_map(|m| m.keys().cloned()).collect();
    let mut files = Vec::new();
    let mut payload: Vec<(String, PathBuf)> = Vec::new();
    for p in &all_paths {
        let per: Vec<Option<&(Op, CoreFile, PathBuf)>> = arches
            .iter()
            .map(|a| changed.get(a).and_then(|m| m.get(p)))
            .collect();
        let same = per.iter().all(|x| x.is_some())
            && per
                .windows(2)
                .all(|w| matches!((w[0], w[1]), (Some(a), Some(b)) if a.0 == b.0 && a.1 == b.1));
        if same {
            let (op, f, src) = per[0].unwrap();
            let source = format!("{PAYLOAD}any/{p}");
            payload.push((source.clone(), src.clone()));
            files.push(FileEntry {
                op: *op,
                path: p.clone(),
                source,
                sha256: f.sha256.clone(),
                size: f.size,
                mode: f.mode,
                arch: None,
            });
        } else {
            for (arch, x) in arches.iter().zip(per) {
                if let Some((op, f, src)) = x {
                    let source = format!("{PAYLOAD}{arch}/{p}");
                    payload.push((source.clone(), src.clone()));
                    files.push(FileEntry {
                        op: *op,
                        path: p.clone(),
                        source,
                        sha256: f.sha256.clone(),
                        size: f.size,
                        mode: f.mode,
                        arch: Some(arch.clone()),
                    });
                }
            }
        }
    }
    let rm_paths: BTreeSet<String> = removed.values().flat_map(|m| m.keys().cloned()).collect();
    let mut remove = Vec::new();
    for p in &rm_paths {
        let per: Vec<Option<&String>> = arches
            .iter()
            .map(|a| removed.get(a).and_then(|m| m.get(p)))
            .collect();
        if per.iter().all(|x| x.is_some()) && per.windows(2).all(|w| w[0] == w[1]) {
            remove.push(RemoveEntry {
                path: p.clone(),
                sha256: per[0].unwrap().clone(),
                arch: None,
            });
        } else {
            for (arch, x) in arches.iter().zip(per) {
                if let Some(sha) = x {
                    remove.push(RemoveEntry {
                        path: p.clone(),
                        sha256: sha.clone(),
                        arch: Some(arch.clone()),
                    });
                }
            }
        }
    }

    // migrations: NNNN-name.json, in name order
    let mut migrations = Vec::new();
    let mut migration_files = Vec::new();
    if let Some(dir) = &input.migrations {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        for n in names {
            let bytes = std::fs::read(dir.join(&n)).map_err(|e| format!("{n}: {e}"))?;
            crate::migrate::Migration::parse(&bytes).map_err(|e| format!("{n}: {e}"))?;
            let id = n.trim_end_matches(".json").to_string();
            policy::check_relative(&n)?;
            let file = format!("{MIGRATIONS}{n}");
            migrations.push(MigrationEntry {
                id,
                file: file.clone(),
                sha256: crate::sha256_hex(&bytes),
            });
            migration_files.push((file, bytes));
        }
    }

    let (min_from, max_from) = match &input.base {
        Some(b) => (b.version.clone(), b.version.clone()),
        // the first release: nothing installed can be older; a full installation it is
        None => ("0.0.0".into(), "0.0.0".into()),
    };
    let manifest = UpdateManifest {
        schema: UPDATE_SCHEMA.into(),
        product: policy::PRODUCT.into(),
        component: component.clone(),
        version: input.version.clone(),
        min_from_version: min_from,
        max_from_version: max_from,
        released_at: input.released_at.clone(),
        permitted_paths: prefixes.iter().map(|s| s.to_string()).collect(),
        files,
        remove,
        migrations,
        restart: input.restart.clone(),
        source_commit: input.source_commit.clone(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let sig = crate::sig::sign(key, &manifest_bytes);

    let core_files = CoreFiles {
        schema: CORE_FILES_SCHEMA.into(),
        product: policy::PRODUCT.into(),
        component: component.clone(),
        version: input.version.clone(),
        files: trees
            .iter()
            .map(|(a, t)| {
                (
                    a.clone(),
                    t.iter().map(|(p, (f, _))| (p.clone(), f.clone())).collect(),
                )
            })
            .collect(),
    };

    // the zip: fixed order and times, so the same input gives the same file
    let f = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let mut sums = Vec::new();
    let mut put =
        |z: &mut zip::ZipWriter<std::fs::File>, name: &str, bytes: &[u8]| -> Result<(), String> {
            z.start_file(name, opts).map_err(|e| e.to_string())?;
            z.write_all(bytes).map_err(|e| e.to_string())?;
            sums.push(format!("{}  {name}", crate::sha256_hex(bytes)));
            Ok(())
        };
    put(&mut z, MANIFEST, &manifest_bytes)?;
    put(&mut z, MANIFEST_SIG, format!("{sig}\n").as_bytes())?;
    payload.sort();
    for (name, src) in &payload {
        let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
        put(&mut z, name, &bytes)?;
    }
    for (name, bytes) in &migration_files {
        put(&mut z, name, bytes)?;
    }
    let sums_text = sums.join("\n") + "\n";
    z.start_file(CHECKSUMS, opts).map_err(|e| e.to_string())?;
    z.write_all(sums_text.as_bytes())
        .map_err(|e| e.to_string())?;
    z.finish().map_err(|e| e.to_string())?;
    Ok(Output {
        manifest,
        core_files,
    })
}
