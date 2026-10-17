//! Applying `enterprise-update.zip` to an installation, as a controlled partial update:
//!
//! 1. the package: only the names an update may contain, no links, sizes bounded; the manifest's
//!    signature by a trusted release key; every path against the built-in [`policy`]
//! 2. compatibility: same product and component, the installed version inside the range the
//!    update applies to (else: the updates in between, or a full installation)
//! 3. conflicts: a core file changed by hand since it was installed is not overwritten silently
//!    (refused, or with `overwrite_modified` kept aside first)
//! 4. staging: the files are extracted next to the core and checked against their sha256
//! 5. a backup of every file that changes (and the record, and the runtime files migrations
//!    touch), and a journal, so an interrupted update is rolled back on the next run
//! 6. the swap (one rename per file), the migrations, the new record
//! 7. restart of the services the manifest names, a health check, and a rollback if it fails
//!
//! Nothing outside `<root>/core/` changes, apart from the record, settings a migration adds,
//! and the update's own state under `<state>/update/`.

use crate::manifest::*;
use crate::policy::{self, Layout};
use crate::sig::TrustedKeys;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest package and file accepted (no zip bombs).
pub const MAX_PACKAGE: u64 = 1 << 30;
pub const MAX_FILE: u64 = 256 << 20;
const MAX_MANIFEST: u64 = 8 << 20;

/// Restarting and checking the services (systemd on a server; a fake in tests).
pub trait Hooks {
    fn restart(&mut self, services: &[String]) -> Result<(), String>;
    /// The new version answers as it should.
    fn health(&mut self, version: &str) -> Result<(), String>;
}

#[derive(Clone, Debug)]
pub struct Options {
    /// keep hand-changed core files aside (in `<state>/update/conflicts/`) and update them
    pub overwrite_modified: bool,
    /// `2026-10-17T01:00:00Z`: for the record and backup names
    pub now: String,
    /// backups kept after a successful update
    pub keep_backups: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            overwrite_modified: false,
            now: "1970-01-01T00:00:00Z".into(),
            keep_backups: 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub path: String,
    pub why: String,
}

#[derive(Debug)]
pub enum Error {
    /// the package is not a valid, authentic Taro update
    Invalid(String),
    /// it does not apply to this installation
    Incompatible(String),
    AlreadyCurrent(String),
    /// core files changed by hand: nothing was changed
    Conflicts(Vec<Conflict>),
    /// it went wrong after changes began; `rolled_back` says whether the previous core is back
    Failed {
        why: String,
        rolled_back: bool,
        rollback_error: Option<String>,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Invalid(w) => write!(f, "the update package was refused: {w}"),
            Error::Incompatible(w) => write!(f, "{w}"),
            Error::AlreadyCurrent(v) => write!(f, "version {v} is already installed"),
            Error::Conflicts(c) => {
                writeln!(f, "core files were changed on this server since they were installed; nothing was updated:")?;
                for x in c {
                    writeln!(f, "  {}: {}", x.path, x.why)?;
                }
                write!(f, "Move such changes to the provider folder (/etc/taro/provider: theme.css, logo, provider.json), or run the update again with --overwrite-modified (the changed files are kept in /var/lib/taro/update/conflicts/).")
            }
            Error::Failed {
                why,
                rolled_back: true,
                ..
            } => write!(
                f,
                "the update failed and the previous version was restored: {why}"
            ),
            Error::Failed {
                why,
                rolled_back: false,
                rollback_error,
            } => {
                write!(
                    f,
                    "the update failed and could NOT be rolled back automatically: {why}"
                )?;
                if let Some(e) = rollback_error {
                    write!(f, " (rollback: {e})")?;
                }
                write!(
                    f,
                    ". Run `taro-relay update rollback` to restore the backup."
                )
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub written: Vec<String>,
    pub removed: Vec<String>,
    pub unchanged: Vec<String>,
    pub migrations: Vec<String>,
    pub kept_aside: Vec<String>,
    pub backup: PathBuf,
}

/// One line of `<state>/update/history.jsonl`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: String,
    pub from: String,
    pub to: String,
    /// ok | failed | rolled_back | refused
    pub result: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub backup: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupInfo {
    from: String,
    to: String,
    /// every core path the update touches
    paths: Vec<String>,
    /// the ones that did not exist before (removed again on rollback)
    absent: Vec<String>,
    /// runtime files saved (migrations)
    runtime: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Journal {
    from: String,
    to: String,
    backup: PathBuf,
}

/// An opened, checked package.
pub struct Package {
    pub manifest: UpdateManifest,
    pub signed_by: String,
    zip: zip::ZipArchive<std::fs::File>,
}

fn read_entry(
    zip: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut e = zip
        .by_name(name)
        .map_err(|_| format!("{name} is missing"))?;
    if e.size() > limit {
        return Err(format!("{name} is too large"));
    }
    let mut v = Vec::with_capacity(e.size() as usize);
    e.by_ref()
        .take(limit + 1)
        .read_to_end(&mut v)
        .map_err(|e| format!("{name}: {e}"))?;
    if v.len() as u64 > limit {
        return Err(format!("{name} is too large"));
    }
    Ok(v)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Open and check a package: names, signature, manifest, policy, checksums.
pub fn open(path: &Path, keys: &TrustedKeys) -> Result<Package, Error> {
    let inv = Error::Invalid;
    let meta = std::fs::metadata(path).map_err(|e| inv(format!("{}: {e}", path.display())))?;
    if meta.len() > MAX_PACKAGE {
        return Err(inv("larger than any Taro update".into()));
    }
    let f = std::fs::File::open(path).map_err(|e| inv(format!("{}: {e}", path.display())))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| inv(format!("not a zip file ({e})")))?;

    // names: only what an update holds, each a plain path, no links, no duplicates
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let e = zip.by_index_raw(i).map_err(|e| inv(e.to_string()))?;
        let name = e
            .name()
            .map_err(|_| inv("an entry name is not valid UTF-8".into()))?
            .to_string();
        if e.is_dir() || name.ends_with('/') {
            return Err(inv(format!("{name}: folders are not entries of an update")));
        }
        if e.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(inv(format!("{name}: symbolic links are refused")));
        }
        policy::check_relative(&name).map_err(inv)?;
        let allowed = name == MANIFEST
            || name == MANIFEST_SIG
            || name == CHECKSUMS
            || name.starts_with(PAYLOAD)
            || name.starts_with(MIGRATIONS);
        if !allowed {
            return Err(inv(format!("{name}: not something an update contains")));
        }
        if e.size() > MAX_FILE {
            return Err(inv(format!("{name}: too large")));
        }
        total += e.size();
        if !names.insert(name.clone()) {
            return Err(inv(format!("{name} appears twice")));
        }
    }
    if total > MAX_PACKAGE {
        return Err(inv("its contents are larger than any Taro update".into()));
    }

    // the signature over the exact manifest bytes, before anything in it is believed
    let manifest_bytes = read_entry(&mut zip, MANIFEST, MAX_MANIFEST).map_err(inv)?;
    let sig = String::from_utf8(read_entry(&mut zip, MANIFEST_SIG, 4096).map_err(inv)?)
        .map_err(|_| inv("the signature is not text".into()))?;
    let signed_by = keys.verify(&manifest_bytes, &sig).map_err(inv)?;
    let m: UpdateManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| inv(format!("manifest.json: {e}")))?;

    if m.schema != UPDATE_SCHEMA {
        return Err(inv(format!(
            "manifest schema {} is not {UPDATE_SCHEMA}",
            m.schema
        )));
    }
    if m.product != policy::PRODUCT {
        return Err(inv(format!(
            "it is for {}, not {}",
            m.product,
            policy::PRODUCT
        )));
    }
    let allowed = policy::prefixes(&m.component)
        .ok_or_else(|| inv(format!("unknown component {}", m.component)))?;
    for v in [&m.version, &m.min_from_version, &m.max_from_version] {
        crate::version::Version::parse(v).ok_or_else(|| inv(format!("{v}: not a version")))?;
    }
    for p in &m.permitted_paths {
        if !allowed.contains(&p.as_str()) {
            return Err(inv(format!(
                "it asks to change {p}, which a {} update may not",
                m.component
            )));
        }
    }
    let mut seen = BTreeSet::new();
    for f in &m.files {
        policy::check_target(&m.component, &f.path).map_err(inv)?;
        if !m
            .permitted_paths
            .iter()
            .any(|p| f.path.starts_with(p.as_str()))
        {
            return Err(inv(format!(
                "{}: outside the paths the manifest itself permits",
                f.path
            )));
        }
        if !f.source.starts_with(PAYLOAD) || !names.contains(&f.source) {
            return Err(inv(format!(
                "{}: its contents ({}) are not in the package",
                f.path, f.source
            )));
        }
        if !is_hex64(&f.sha256) || !(f.mode == 0o644 || f.mode == 0o755) || f.size > MAX_FILE {
            return Err(inv(format!("{}: bad checksum, mode or size", f.path)));
        }
        if let Some(a) = &f.arch {
            if !policy::ARCHES.contains(&a.as_str()) {
                return Err(inv(format!("{}: unknown architecture {a}", f.path)));
            }
        }
        for a in policy::ARCHES {
            if f.arch.as_deref().is_none_or(|x| x == *a) && !seen.insert((f.path.clone(), *a)) {
                return Err(inv(format!("{} is listed twice", f.path)));
            }
        }
    }
    for r in &m.remove {
        policy::check_target(&m.component, &r.path).map_err(inv)?;
        if !is_hex64(&r.sha256) {
            return Err(inv(format!("{}: bad checksum", r.path)));
        }
        for a in policy::ARCHES {
            if r.arch.as_deref().is_none_or(|x| x == *a) && seen.contains(&(r.path.clone(), *a)) {
                return Err(inv(format!("{} is both written and removed", r.path)));
            }
        }
    }
    for mg in &m.migrations {
        if !mg.file.starts_with(MIGRATIONS) || !names.contains(&mg.file) || !is_hex64(&mg.sha256) {
            return Err(inv(format!("migration {}: not in the package", mg.id)));
        }
    }
    for s in &m.restart {
        if !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
            || s.is_empty()
        {
            return Err(inv(format!("{s}: not a service name")));
        }
    }

    // checksums.sha256 lists every other entry with its sha256
    let sums = String::from_utf8(read_entry(&mut zip, CHECKSUMS, MAX_MANIFEST).map_err(inv)?)
        .map_err(|_| inv("checksums.sha256 is not text".into()))?;
    let mut listed = BTreeMap::new();
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (sha, name) = line
            .split_once("  ")
            .ok_or_else(|| inv(format!("checksums.sha256: bad line {line:?}")))?;
        listed.insert(name.to_string(), sha.to_string());
    }
    for name in names.iter().filter(|n| *n != CHECKSUMS) {
        let Some(sha) = listed.get(name) else {
            return Err(inv(format!("{name} is not in checksums.sha256")));
        };
        let bytes = read_entry(&mut zip, name, MAX_FILE).map_err(inv)?;
        if crate::sha256_hex(&bytes) != *sha {
            return Err(inv(format!("{name}: its checksum does not match")));
        }
    }
    if listed.len() != names.len() - 1 {
        return Err(inv(
            "checksums.sha256 lists files that are not in the package".into(),
        ));
    }
    Ok(Package {
        manifest: m,
        signed_by,
        zip,
    })
}

fn read_installed(layout: &Layout) -> Result<Installed, String> {
    let p = layout.installed_record();
    let s = std::fs::read(&p)
        .map_err(|e| format!("{}: {e} (is Taro Enterprise installed here?)", p.display()))?;
    let i: Installed = serde_json::from_slice(&s).map_err(|e| format!("{}: {e}", p.display()))?;
    if i.schema != INSTALLED_SCHEMA {
        return Err(format!("{}: unknown schema {}", p.display(), i.schema));
    }
    Ok(i)
}

/// What is installed, if anything.
pub fn installed(layout: &Layout) -> Result<Installed, String> {
    read_installed(layout)
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    crate::migrate::write_atomic(
        path,
        &serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?,
    )
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    }
    let _ = (p, mode);
    Ok(())
}

fn copy_preserving(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::copy(from, to).map_err(|e| format!("{} -> {}: {e}", from.display(), to.display()))?;
    Ok(())
}

/// Put `from` at `to` by a copy next to it and a rename (a reader sees the old or the new file).
fn replace_with_copy(from: &Path, to: &Path) -> Result<(), String> {
    let tmp = to.with_file_name(format!(
        ".{}.taro-restore",
        to.file_name().unwrap_or_default().to_string_lossy()
    ));
    copy_preserving(from, &tmp)?;
    std::fs::rename(&tmp, to).map_err(|e| format!("{}: {e}", to.display()))
}

fn history(layout: &Layout, e: &HistoryEntry) {
    use std::io::Write;
    let p = layout.update_state().join("history.jsonl");
    let _ = std::fs::create_dir_all(layout.update_state());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default());
    }
}

/// The update history, oldest first.
pub fn read_history(layout: &Layout) -> Vec<HistoryEntry> {
    std::fs::read_to_string(layout.update_state().join("history.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn journal_path(layout: &Layout) -> PathBuf {
    layout.update_state().join("journal.json")
}

/// Restore a backup: the core files as they were, the record, the runtime files.
fn restore(layout: &Layout, backup: &Path) -> Result<BackupInfo, String> {
    let info: BackupInfo = serde_json::from_slice(
        &std::fs::read(backup.join("info.json"))
            .map_err(|e| format!("{}: {e}", backup.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for p in &info.paths {
        let target = match policy::resolve(&layout.root, p) {
            Ok(t) => t,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        if info.absent.contains(p) {
            if target.exists() {
                if let Err(e) = std::fs::remove_file(&target) {
                    errors.push(format!("{}: {e}", target.display()));
                }
            }
        } else if let Err(e) = replace_with_copy(&backup.join("files").join(p), &target) {
            errors.push(e);
        }
    }
    let rec = backup.join(policy::INSTALLED);
    if rec.exists() {
        if let Err(e) = replace_with_copy(&rec, &layout.installed_record()) {
            errors.push(e);
        }
    }
    for f in &info.runtime {
        if let Err(e) = replace_with_copy(&backup.join("runtime").join(f), &layout.runtime.join(f))
        {
            errors.push(e);
        }
    }
    if errors.is_empty() {
        Ok(info)
    } else {
        Err(errors.join("; "))
    }
}

/// An update interrupted midway (power cut, killed): roll it back. Some(what) when there was one.
pub fn recover(
    layout: &Layout,
    hooks: &mut dyn Hooks,
    now: &str,
) -> Result<Option<String>, String> {
    let jp = journal_path(layout);
    let Ok(bytes) = std::fs::read(&jp) else {
        return Ok(None);
    };
    let j: Journal =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", jp.display()))?;
    restore(layout, &j.backup)?;
    let _ = std::fs::remove_file(&jp);
    let _ = hooks.restart(&["taro-relay".to_string()]);
    let what = format!(
        "an interrupted update {} -> {} was rolled back",
        j.from, j.to
    );
    history(
        layout,
        &HistoryEntry {
            at: now.into(),
            from: j.from,
            to: j.to,
            result: "rolled_back".into(),
            detail: "interrupted".into(),
            backup: Some(j.backup),
        },
    );
    Ok(Some(what))
}

/// Undo the last successful update (from its backup).
pub fn rollback_last(layout: &Layout, hooks: &mut dyn Hooks, now: &str) -> Result<String, String> {
    if let Some(w) = recover(layout, hooks, now)? {
        return Ok(w);
    }
    let last = read_history(layout)
        .into_iter()
        .rev()
        .find(|h| h.result == "ok")
        .ok_or("no update to roll back")?;
    let backup = last
        .backup
        .clone()
        .filter(|b| b.join("info.json").exists())
        .ok_or("the backup of the last update is gone")?;
    let info = restore(layout, &backup)?;
    let services = vec!["taro-relay".to_string()];
    hooks.restart(&services)?;
    hooks.health(&info.from)?;
    history(
        layout,
        &HistoryEntry {
            at: now.into(),
            from: info.to.clone(),
            to: info.from.clone(),
            result: "rolled_back".into(),
            detail: "by the operator".into(),
            backup: Some(backup),
        },
    );
    // that backup is spent: a second rollback goes one further back only by hand
    Ok(format!("rolled back from {} to {}", info.to, info.from))
}

/// Check that `pkg` applies here, without changing anything: the plan, or why not.
pub fn check(layout: &Layout, pkg: &Package, overwrite_modified: bool) -> Result<Plan, Error> {
    let inst = read_installed(layout).map_err(Error::Incompatible)?;
    plan(layout, &inst, &pkg.manifest, overwrite_modified)
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub from: String,
    pub to: String,
    pub write: Vec<FileEntry>,
    pub unchanged: Vec<String>,
    pub remove: Vec<String>,
    pub conflicts: Vec<Conflict>,
    pub migrations: Vec<MigrationEntry>,
}

fn plan(
    layout: &Layout,
    inst: &Installed,
    m: &UpdateManifest,
    overwrite_modified: bool,
) -> Result<Plan, Error> {
    use crate::version::Version;
    if inst.product != m.product || inst.component != m.component {
        return Err(Error::Incompatible(format!(
            "this server has {} {}, the update is for {} {}",
            inst.product, inst.component, m.product, m.component
        )));
    }
    let (Some(have), Some(to), Some(lo), Some(hi)) = (
        Version::parse(&inst.version),
        Version::parse(&m.version),
        Version::parse(&m.min_from_version),
        Version::parse(&m.max_from_version),
    ) else {
        return Err(Error::Incompatible("a version could not be read".into()));
    };
    if have == to {
        return Err(Error::AlreadyCurrent(inst.version.clone()));
    }
    if have > to {
        return Err(Error::Incompatible(format!("this server has {} which is newer than {}: an update never goes back (use `taro-relay update rollback`)", inst.version, m.version)));
    }
    if have < lo || have > hi {
        let range = if lo == hi {
            m.min_from_version.clone()
        } else {
            format!("{} to {}", m.min_from_version, m.max_from_version)
        };
        return Err(Error::Incompatible(format!(
            "update {} applies to version {range}; this server has {}. Apply the updates in between first (`taro-relay update check` finds them), or for a jump this large install the full bundle and keep /etc/taro and /var/lib/taro.",
            m.version, inst.version
        )));
    }
    let arch = inst.arch.as_str();
    let mut p = Plan {
        from: inst.version.clone(),
        to: m.version.clone(),
        write: vec![],
        unchanged: vec![],
        remove: vec![],
        conflicts: vec![],
        migrations: vec![],
    };
    let conflict = |p: &mut Plan, path: &str, why: &str| {
        p.conflicts.push(Conflict {
            path: path.into(),
            why: why.into(),
        })
    };
    for f in m.files_for(arch) {
        let target = policy::resolve(&layout.root, &f.path).map_err(Error::Invalid)?;
        let current = if target.is_file() {
            Some(
                crate::sha256_file(&target)
                    .map_err(|e| Error::Invalid(format!("{}: {e}", target.display())))?,
            )
        } else {
            None
        };
        let recorded = inst.files.get(&f.path);
        if current.as_deref() == Some(f.sha256.as_str()) {
            p.unchanged.push(f.path.clone());
            continue;
        }
        match (&current, recorded) {
            (Some(c), Some(r)) if c != r => conflict(
                &mut p,
                &f.path,
                "changed on this server since it was installed",
            ),
            (Some(_), None) => conflict(
                &mut p,
                &f.path,
                "a file not installed by Taro is in the way",
            ),
            _ => {}
        }
        if target.exists() && !target.is_file() {
            return Err(Error::Incompatible(format!(
                "{} is not a file",
                target.display()
            )));
        }
        p.write.push(f.clone());
    }
    for r in m.removals_for(arch) {
        let target = policy::resolve(&layout.root, &r.path).map_err(Error::Invalid)?;
        if !target.is_file() {
            continue;
        }
        let current = crate::sha256_file(&target).map_err(|e| Error::Invalid(e.to_string()))?;
        if Some(&current) != inst.files.get(&r.path) {
            conflict(
                &mut p,
                &r.path,
                "to be removed, but changed on this server since it was installed",
            );
        }
        p.remove.push(r.path.clone());
    }
    p.migrations = m
        .migrations
        .iter()
        .filter(|mg| !inst.migrations.contains(&mg.id))
        .cloned()
        .collect();
    if !p.conflicts.is_empty() && !overwrite_modified {
        return Err(Error::Conflicts(p.conflicts));
    }
    Ok(p)
}

/// Apply `zip` to the installation at `layout`.
pub fn apply(
    layout: &Layout,
    zip: &Path,
    keys: &TrustedKeys,
    hooks: &mut dyn Hooks,
    opts: &Options,
) -> Result<Report, Error> {
    if let Err(e) = recover(layout, hooks, &opts.now) {
        return Err(Error::Failed {
            why: format!("an earlier interrupted update could not be rolled back: {e}"),
            rolled_back: false,
            rollback_error: None,
        });
    }
    let mut pkg = open(zip, keys)?;
    let inst = read_installed(layout).map_err(Error::Incompatible)?;
    let plan = match plan(layout, &inst, &pkg.manifest, opts.overwrite_modified) {
        Ok(p) => p,
        Err(e) => {
            if !matches!(e, Error::AlreadyCurrent(_)) {
                history(
                    layout,
                    &HistoryEntry {
                        at: opts.now.clone(),
                        from: inst.version.clone(),
                        to: pkg.manifest.version.clone(),
                        result: "refused".into(),
                        detail: e.to_string(),
                        backup: None,
                    },
                );
            }
            return Err(e);
        }
    };
    let m = pkg.manifest.clone();

    // staging, next to the core (the same file system: the swap is a rename)
    let staging = layout.root.join(format!(".staging-{}", m.version));
    let _ = std::fs::remove_dir_all(&staging);
    let stage = |pkg: &mut Package| -> Result<Vec<(FileEntry, PathBuf)>, String> {
        let mut out = Vec::new();
        for f in &plan.write {
            let bytes = read_entry(&mut pkg.zip, &f.source, MAX_FILE)?;
            if bytes.len() as u64 != f.size || crate::sha256_hex(&bytes) != f.sha256 {
                return Err(format!(
                    "{}: the contents do not match the manifest",
                    f.path
                ));
            }
            let at = staging.join(&f.path);
            std::fs::create_dir_all(at.parent().unwrap())
                .map_err(|e| format!("{}: {e}", staging.display()))?;
            std::fs::write(&at, &bytes).map_err(|e| format!("{}: {e}", at.display()))?;
            set_mode(&at, f.mode).map_err(|e| format!("{}: {e}", at.display()))?;
            out.push((f.clone(), at));
        }
        Ok(out)
    };
    let staged = match stage(&mut pkg) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Invalid(e));
        }
    };
    let mut migrations = Vec::new();
    for mg in &plan.migrations {
        let bytes = read_entry(&mut pkg.zip, &mg.file, MAX_MANIFEST).map_err(Error::Invalid)?;
        if crate::sha256_hex(&bytes) != mg.sha256 {
            return Err(Error::Invalid(format!("migration {}: checksum", mg.id)));
        }
        migrations.push((
            mg.id.clone(),
            crate::migrate::Migration::parse(&bytes)
                .map_err(|e| Error::Invalid(format!("migration {}: {e}", mg.id)))?,
        ));
    }

    // the backup
    let stamp: String = opts
        .now
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let backup = layout
        .update_state()
        .join("backups")
        .join(format!("{}-to-{}-{stamp}", plan.from, plan.to));
    let make_backup = || -> Result<BackupInfo, String> {
        let _ = std::fs::remove_dir_all(&backup);
        let mut info = BackupInfo {
            from: plan.from.clone(),
            to: plan.to.clone(),
            paths: vec![],
            absent: vec![],
            runtime: vec![],
        };
        for path in plan.write.iter().map(|f| &f.path).chain(plan.remove.iter()) {
            let target = policy::resolve(&layout.root, path)?;
            info.paths.push(path.clone());
            if target.is_file() {
                copy_preserving(&target, &backup.join("files").join(path))?;
            } else {
                info.absent.push(path.clone());
            }
        }
        copy_preserving(&layout.installed_record(), &backup.join(policy::INSTALLED))?;
        let files: BTreeSet<String> = migrations
            .iter()
            .flat_map(|(_, m)| {
                m.ops
                    .iter()
                    .map(|crate::migrate::Step::SetDefault { file, .. }| file.clone())
            })
            .collect();
        for f in files {
            let src = layout.runtime.join(&f);
            if src.is_file() {
                copy_preserving(&src, &backup.join("runtime").join(&f))?;
                info.runtime.push(f);
            }
        }
        for c in &plan.conflicts {
            let src = layout.root.join(&c.path);
            if src.is_file() {
                copy_preserving(
                    &src,
                    &layout
                        .update_state()
                        .join("conflicts")
                        .join(&plan.to)
                        .join(&c.path),
                )?;
            }
        }
        write_json(&backup.join("info.json"), &info)?;
        Ok(info)
    };
    if let Err(e) = make_backup() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(Error::Failed {
            why: format!("the backup could not be made: {e}"),
            rolled_back: true,
            rollback_error: None,
        });
    }
    if let Err(e) = write_json(
        &journal_path(layout),
        &Journal {
            from: plan.from.clone(),
            to: plan.to.clone(),
            backup: backup.clone(),
        },
    ) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(Error::Failed {
            why: format!("the journal could not be written: {e}"),
            rolled_back: true,
            rollback_error: None,
        });
    }

    let fail =
        |why: String, hooks: &mut dyn Hooks| -> Error {
            let r = restore(layout, &backup);
            let _ = std::fs::remove_dir_all(&staging);
            let restarted = if r.is_ok() {
                hooks
                    .restart(&m.restart)
                    .and_then(|_| hooks.health(&plan.from))
                    .err()
            } else {
                None
            };
            let ok = r.is_ok();
            if ok {
                let _ = std::fs::remove_file(journal_path(layout));
            }
            history(
                layout,
                &HistoryEntry {
                    at: opts.now.clone(),
                    from: plan.from.clone(),
                    to: plan.to.clone(),
                    result: if ok {
                        "rolled_back".into()
                    } else {
                        "failed".into()
                    },
                    detail: why.clone(),
                    backup: Some(backup.clone()),
                },
            );
            Error::Failed {
                why,
                rolled_back: ok,
                rollback_error: r.err().or(restarted
                    .map(|e| format!("the previous version did not come back healthy: {e}"))),
            }
        };

    // the swap
    for (f, at) in &staged {
        let target = match policy::resolve(&layout.root, &f.path) {
            Ok(t) => t,
            Err(e) => return Err(fail(e, hooks)),
        };
        if let Err(e) = std::fs::create_dir_all(target.parent().unwrap())
            .and_then(|_| std::fs::rename(at, &target))
        {
            return Err(fail(format!("{}: {e}", target.display()), hooks));
        }
    }
    for path in &plan.remove {
        if let Err(e) = std::fs::remove_file(layout.root.join(path)) {
            return Err(fail(format!("{path}: {e}"), hooks));
        }
    }
    let mut applied = Vec::new();
    for (id, mg) in &migrations {
        if let Err(e) = mg.apply(&layout.runtime) {
            return Err(fail(format!("migration {id}: {e}"), hooks));
        }
        applied.push(id.clone());
    }
    let mut rec = inst.clone();
    for f in &plan.write {
        rec.files.insert(f.path.clone(), f.sha256.clone());
    }
    for f in pkg.manifest.files_for(&inst.arch) {
        rec.files
            .entry(f.path.clone())
            .or_insert_with(|| f.sha256.clone());
    }
    for p in &plan.remove {
        rec.files.remove(p);
    }
    rec.version = m.version.clone();
    rec.migrations.extend(applied.iter().cloned());
    rec.updated_at = opts.now.clone();
    if let Err(e) = write_json(&layout.installed_record(), &rec) {
        return Err(fail(format!("the record: {e}"), hooks));
    }
    if let Err(e) = hooks.restart(&m.restart) {
        return Err(fail(
            format!("restarting {}: {e}", m.restart.join(", ")),
            hooks,
        ));
    }
    if let Err(e) = hooks.health(&m.version) {
        return Err(fail(
            format!("the health check of {} failed: {e}", m.version),
            hooks,
        ));
    }

    let _ = std::fs::remove_file(journal_path(layout));
    let _ = std::fs::remove_dir_all(&staging);
    history(
        layout,
        &HistoryEntry {
            at: opts.now.clone(),
            from: plan.from.clone(),
            to: plan.to.clone(),
            result: "ok".into(),
            detail: format!(
                "{} written, {} removed, signed by {}",
                plan.write.len(),
                plan.remove.len(),
                pkg.signed_by
            ),
            backup: Some(backup.clone()),
        },
    );
    prune_backups(layout, opts.keep_backups);
    Ok(Report {
        from: plan.from,
        to: plan.to,
        written: plan.write.iter().map(|f| f.path.clone()).collect(),
        removed: plan.remove,
        unchanged: plan.unchanged,
        migrations: applied,
        kept_aside: plan.conflicts.into_iter().map(|c| c.path).collect(),
        backup,
    })
}

fn prune_backups(layout: &Layout, keep: usize) {
    let dir = layout.update_state().join("backups");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut all: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    all.sort();
    let n = all.len().saturating_sub(keep.max(1));
    for (_, p) in all.into_iter().take(n) {
        let _ = std::fs::remove_dir_all(p);
    }
}

/// After a fresh installation (the full bundle): record the core as installed.
pub fn record_fresh_install(
    layout: &Layout,
    component: &str,
    version: &str,
    arch: &str,
    now: &str,
) -> Result<Installed, String> {
    crate::version::Version::parse(version).ok_or_else(|| format!("{version}: not a version"))?;
    if !policy::ARCHES.contains(&arch) {
        return Err(format!("unknown architecture {arch}"));
    }
    let files = crate::pack::scan_core(&layout.core())?;
    for p in files.keys() {
        policy::check_target(component, p)?;
    }
    let rec = Installed {
        schema: INSTALLED_SCHEMA.into(),
        product: policy::PRODUCT.into(),
        component: component.into(),
        version: version.into(),
        arch: arch.into(),
        files: files.into_iter().map(|(p, (f, _))| (p, f.sha256)).collect(),
        migrations: vec![],
        updated_at: now.into(),
    };
    write_json(&layout.installed_record(), &rec)?;
    Ok(rec)
}
