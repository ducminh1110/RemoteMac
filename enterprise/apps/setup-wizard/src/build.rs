//! Step 6: the build.
//!
//! 1. Every selected component's URL is checked against the download policy before anything
//!    is fetched, and the free space is checked.
//! 2. Each component is streamed into `<output>/<name>.partial/.downloads/`, its size and
//!    sha256 enforced against the signed manifest.
//! 3. The packages are assembled in `<name>.partial/` (see [`crate::package`]) with
//!    `provider/`, `provider-manifest.json`, `SHA256SUMS` and `INSTALL.md`.
//! 4. Only then is `<name>.partial` renamed to `<name>`. A failure or a cancel removes the
//!    partial folder, so a half-built output never carries the final name.

use crate::config::BUILT_BY;
use crate::package::{self, Built, Format, InstallInfo, ProviderFiles};
use crate::releases::Selected;
use crate::state::{AppState, Artifact, BuildItem, BuildResult, PackageRecord};
use rm_brand::{Generated, Provider};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use taro_update::manifest::Component;

pub const PROVIDER_MANIFEST: &str = "provider-manifest.json";
pub const PROVIDER_MANIFEST_SCHEMA: &str = "taro.provider-distribution/v1";
pub const SUMS: &str = "SHA256SUMS";
pub const INSTALL: &str = "INSTALL.md";
pub const DOWNLOADS: &str = ".downloads";

/// Everything a build needs, taken from the state when it starts.
pub struct Plan {
    pub provider: Provider,
    pub json: Vec<u8>,
    pub logo: Option<Vec<u8>>,
    pub icon: Option<Vec<u8>>,
    pub selected: Selected,
    pub components: Vec<Component>,
    pub out_root: PathBuf,
    pub replace: bool,
    pub built_at: u64,
    pub rev: u64,
}

impl Plan {
    pub fn name(&self) -> String {
        format!(
            "{}-{}",
            self.provider.provider_id, self.selected.taro_version
        )
    }
}

/// Windows refuses these as file or folder names.
pub fn windows_reserved(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    let stem = up.split('.').next().unwrap_or("");
    matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
        || name.ends_with('.')
        || name.ends_with(' ')
}

/// The output folder the provider typed: absolute, plain.
pub fn check_output_dir(raw: &str) -> Result<PathBuf, String> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("Choose a folder for the packages.".into());
    }
    if t.chars().any(char::is_control) || t.len() > 400 {
        return Err("That folder name has invalid characters.".into());
    }
    let p = PathBuf::from(t);
    if !p.is_absolute() {
        return Err(
            "Use a full path (for example C:\\Users\\you\\Documents\\Taro Enterprise).".into(),
        );
    }
    if p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("The path must not contain \"..\".".into());
    }
    if p.exists() && !p.is_dir() {
        return Err(format!("{} exists and is not a folder.", p.display()));
    }
    Ok(p)
}

/// Artifact paths (relative to the output folder), by component.
pub fn artifact_path(p: &Provider, c: &Component, taro_version: &str) -> Option<String> {
    let short = &p.product.short_name;
    match c.id.as_str() {
        "windows-client" => Some(format!("windows/{short}-{taro_version}-windows-x64.zip")),
        "macos-client" => Some(format!("macos/{short}-{taro_version}-macos.tar.gz")),
        id => id.strip_prefix("linux-relay-").map(|arch| {
            format!(
                "linux/{}-relay-{}-linux-{arch}.tar.gz",
                p.provider_id, c.version
            )
        }),
    }
}

fn item(id: &str, label: &str, total: u64) -> BuildItem {
    BuildItem {
        id: id.into(),
        label: label.into(),
        status: "pending".into(),
        detail: None,
        done: 0,
        total,
    }
}

/// Validate the inputs and start the build in the background.
pub fn start(app: &Arc<AppState>, out_raw: &str, replace: bool) -> Result<(), String> {
    let out_root = check_output_dir(out_raw)?;
    let built_at = crate::util::now_secs();
    let (plan, gen, cancel) = {
        let mut w = app.w();
        if w.build.running {
            return Err("A build is already running.".into());
        }
        let sel = w
            .components
            .selected
            .clone()
            .ok_or("Choose an Enterprise release in Components & versions first.")?;
        if taro_update::version::Version::parse(&sel.taro_version).is_none() {
            return Err(format!(
                "The release pins an odd Taro version {:?}.",
                sel.taro_version
            ));
        }
        let comps: Vec<Component> = w
            .components
            .platforms
            .iter()
            .filter_map(|id| sel.component(id).cloned())
            .collect();
        if comps.is_empty() {
            return Err("Select at least one platform.".into());
        }
        for c in &comps {
            if taro_update::version::Version::parse(&c.version).is_none() {
                return Err(format!("{}: odd version {:?}; refused", c.id, c.version));
            }
        }
        let provider = w.provider(Some(Generated {
            by: BUILT_BY.into(),
            at: crate::util::rfc3339(built_at),
            taro_version: sel.taro_version.clone(),
            enterprise_version: Some(sel.enterprise_version.clone()),
        }))?;
        if windows_reserved(&provider.product.short_name) {
            return Err("The short name is a name Windows reserves for devices; change it in Product identity.".into());
        }
        let plan = Plan {
            json: provider.to_json().into_bytes(),
            logo: w.branding.logo.as_ref().map(|u| u.bytes.clone()),
            icon: w.branding.icon.as_ref().map(|u| u.bytes.clone()),
            provider,
            components: comps,
            selected: sel,
            out_root,
            replace,
            built_at,
            rev: w.rev,
        };
        let final_dir = plan.out_root.join(plan.name());
        if final_dir.exists() && !replace {
            return Err(format!(
                "{} already exists. Tick \"Replace an existing build\" or choose another folder.",
                final_dir.display()
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let b = &mut w.build;
        b.generation += 1;
        b.running = true;
        b.cancelled = false;
        b.error = None;
        b.result = None;
        b.output_dir = plan.out_root.display().to_string();
        b.phase = "Checking".into();
        b.cancel = Some(cancel.clone());
        b.items = plan
            .components
            .iter()
            .map(|c| item(&c.id, crate::releases::label(&c.id), c.size))
            .chain([
                item("provider", "Provider folder and manifest", 0),
                item("validate", "Validation", 0),
            ])
            .collect();
        let gen = b.generation;
        w.validation = None;
        (plan, gen, cancel)
    };
    app.log.info(format!(
        "build {} started: {} component(s) into {}",
        plan.name(),
        plan.components.len(),
        plan.out_root.display()
    ));
    let app = app.clone();
    tokio::spawn(async move {
        let name = plan.name();
        let r = run(&app, gen, &plan, &cancel).await;
        let cancelled = cancel.load(Ordering::SeqCst);
        let partial = plan.out_root.join(format!("{name}.partial"));
        match r {
            Ok(result) => {
                app.log
                    .info(format!("build {name} finished in {}", result.dir));
                let report = crate::check::run(&result);
                app.log.info(format!(
                    "validation: {} ({} checks)",
                    if report.ok { "ok" } else { "FAILED" },
                    report.checks.len()
                ));
                let mut w = app.w();
                if w.build.generation == gen {
                    w.build.running = false;
                    w.build.phase = "Done".into();
                    set_item(
                        &mut w.build.items,
                        "validate",
                        if report.ok { "ok" } else { "fail" },
                        None,
                    );
                    w.build.result = Some(result);
                    w.build.cancel = None;
                    w.validation = Some(report);
                }
            }
            Err(e) => {
                if partial.exists() {
                    if let Err(x) = std::fs::remove_dir_all(&partial) {
                        app.log
                            .warn(format!("could not remove {}: {x}", partial.display()));
                    }
                }
                let msg = if cancelled {
                    "Cancelled.".to_string()
                } else {
                    e
                };
                app.log.warn(format!("build {name}: {msg}"));
                let mut w = app.w();
                if w.build.generation == gen {
                    w.build.running = false;
                    w.build.cancelled = cancelled;
                    w.build.phase = if cancelled { "Cancelled" } else { "Failed" }.into();
                    w.build.error = (!cancelled).then_some(msg);
                    w.build.cancel = None;
                    for it in w.build.items.iter_mut() {
                        if it.status == "running" || it.status == "pending" {
                            it.status = if cancelled { "skipped" } else { "fail" }.into();
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

fn set_item(items: &mut [BuildItem], id: &str, status: &str, detail: Option<String>) {
    if let Some(i) = items.iter_mut().find(|i| i.id == id) {
        i.status = status.into();
        if detail.is_some() {
            i.detail = detail;
        }
    }
}

fn update(app: &AppState, gen: u64, f: impl FnOnce(&mut crate::state::BuildState)) {
    let mut w = app.w();
    if w.build.generation == gen {
        f(&mut w.build);
    }
}

async fn run(
    app: &Arc<AppState>,
    gen: u64,
    plan: &Plan,
    cancel: &Arc<AtomicBool>,
) -> Result<BuildResult, String> {
    let name = plan.name();
    let final_dir = plan.out_root.join(&name);
    let partial = plan.out_root.join(format!("{name}.partial"));
    // 1. policy and space, before any download
    for c in &plan.components {
        crate::download::check_url(&c.url)
            .map_err(|e| format!("{}: {e}; nothing was downloaded", c.id))?;
    }
    std::fs::create_dir_all(&plan.out_root)
        .map_err(|e| format!("{}: {e}", plan.out_root.display()))?;
    if partial.exists() {
        std::fs::remove_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    }
    let need: u64 = plan.components.iter().map(|c| c.size).sum::<u64>() * 2 + 20_000_000;
    if let Ok(free) = crate::platform::free_disk_bytes(&plan.out_root) {
        if free < need {
            return Err(format!(
                "Not enough free space in {}: {} needed, {} free.",
                plan.out_root.display(),
                crate::util::human_size(need),
                crate::util::human_size(free)
            ));
        }
    }
    let dl = partial.join(DOWNLOADS);
    std::fs::create_dir_all(&dl).map_err(|e| format!("{}: {e}", dl.display()))?;

    // 2. downloads
    let mut sources: Vec<(Component, PathBuf)> = Vec::new();
    for c in &plan.components {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        update(app, gen, |b| {
            b.phase = format!("Downloading {}", c.file);
            set_item(&mut b.items, &c.id, "running", Some("downloading".into()));
        });
        let dest = dl.join(&c.file);
        let id = c.id.clone();
        let app2 = app.clone();
        let progress = move |n: u64| {
            update(&app2, gen, |b| {
                if let Some(i) = b.items.iter_mut().find(|i| i.id == id) {
                    i.done = n;
                }
            })
        };
        app.log
            .info(format!("downloading {} ({} bytes)", c.url, c.size));
        if let Err(e) = crate::download::download_verified(
            &app.http,
            &app.endpoints,
            &app.timings,
            &c.url,
            c.size,
            &c.sha256,
            &dest,
            cancel,
            &progress,
        )
        .await
        {
            update(app, gen, |b| {
                set_item(&mut b.items, &c.id, "fail", Some(e.clone()))
            });
            return Err(format!("{}: {e}", crate::releases::label(&c.id)));
        }
        app.log.info(format!(
            "{}: size and sha256 match the signed manifest",
            c.file
        ));
        update(app, gen, |b| {
            set_item(
                &mut b.items,
                &c.id,
                "running",
                Some("verified, packaging".into()),
            )
        });
        sources.push((c.clone(), dest));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }

    // 3. assemble (blocking file work)
    update(app, gen, |b| b.phase = "Assembling packages".into());
    let app2 = app.clone();
    let partial2 = partial.clone();
    let cancel2 = cancel.clone();
    let plan_ref = PlanData::from(plan);
    let result = tokio::task::spawn_blocking(move || {
        assemble(&plan_ref, &partial2, &sources, &cancel2, &|id, st, d| {
            update(&app2, gen, |b| set_item(&mut b.items, id, st, d))
        })
    })
    .await
    .map_err(|e| format!("the build task failed: {e}"))??;
    std::fs::remove_dir_all(&dl).map_err(|e| format!("{}: {e}", dl.display()))?;
    if cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }

    // 4. publish
    let old = plan
        .out_root
        .join(format!("{name}.replaced-{}", plan.built_at));
    if final_dir.exists() {
        if !plan.replace {
            return Err(format!(
                "{} appeared during the build; not replaced",
                final_dir.display()
            ));
        }
        std::fs::rename(&final_dir, &old).map_err(|e| format!("{}: {e}", final_dir.display()))?;
    }
    if let Err(e) = std::fs::rename(&partial, &final_dir) {
        if old.exists() {
            let _ = std::fs::rename(&old, &final_dir);
        }
        return Err(format!("{}: {e}", final_dir.display()));
    }
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    let dir = std::fs::canonicalize(&final_dir).unwrap_or(final_dir);
    Ok(BuildResult {
        dir: dir.display().to_string(),
        ..result
    })
}

/// The owned parts of a [`Plan`] the blocking task needs.
struct PlanData {
    provider: Provider,
    files: ProviderFiles,
    selected: Selected,
    built_at: u64,
    name: String,
    rev: u64,
}

impl From<&Plan> for PlanData {
    fn from(p: &Plan) -> Self {
        PlanData {
            provider: p.provider.clone(),
            files: ProviderFiles {
                json: p.json.clone(),
                logo: p.logo.clone(),
                icon: p.icon.clone(),
            },
            selected: p.selected.clone(),
            built_at: p.built_at,
            name: p.name(),
            rev: p.rev,
        }
    }
}

fn sha_file(p: &Path) -> Result<(String, u64), String> {
    let sha = taro_update::sha256_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let size = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
    Ok((sha, size))
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::Write::write_all(&mut f, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    f.sync_all().map_err(|e| format!("{}: {e}", path.display()))
}

type Progress<'a> = dyn Fn(&str, &str, Option<String>) + 'a;

fn assemble(
    plan: &PlanData,
    dir: &Path,
    sources: &[(Component, PathBuf)],
    cancel: &AtomicBool,
    progress: &Progress<'_>,
) -> Result<BuildResult, String> {
    let p = &plan.provider;
    let tv = &plan.selected.taro_version;
    let mtime = plan.built_at;
    // provider/
    let mut artifacts: Vec<Artifact> = Vec::new();
    for (n, bytes) in plan.files.files() {
        let rel = format!("{}/{n}", rm_brand::DIR);
        write(&dir.join(&rel), bytes)?;
        artifacts.push(Artifact {
            component: None,
            path: rel,
            sha256: hex::encode(Sha256::digest(bytes)),
            size: bytes.len() as u64,
        });
    }
    let mut packages: Vec<PackageRecord> = Vec::new();
    for (c, src) in sources {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        progress(&c.id, "running", Some("packaging".into()));
        let rel = artifact_path(p, c, tv).ok_or_else(|| format!("unknown component {}", c.id))?;
        let out = dir.join(&rel);
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let fmt = if c.id == "windows-client" {
            Format::Zip
        } else {
            Format::TarGz
        };
        let source_files = package::source_hashes(src, fmt)?;
        let built: Built = match c.id.as_str() {
            "windows-client" => package::windows_zip(
                src,
                &out,
                &p.product.short_name,
                &plan.files,
                &package::readme_windows(p, tv),
                mtime,
            ),
            "macos-client" => package::macos_tgz(
                src,
                &out,
                &p.product.short_name,
                &plan.files,
                &package::readme_macos(p, tv),
                mtime,
            ),
            id => package::relay_tgz(
                src,
                &out,
                &plan.files,
                &c.version,
                id.strip_prefix("linux-relay-").unwrap_or(id),
                mtime,
            ),
        }
        .map_err(|e| format!("{}: {e}", crate::releases::label(&c.id)))?;
        let (sha, size) = sha_file(&out)?;
        progress(
            &c.id,
            "ok",
            Some(format!("{rel} ({})", crate::util::human_size(size))),
        );
        artifacts.push(Artifact {
            component: Some(c.id.clone()),
            path: rel.clone(),
            sha256: sha,
            size,
        });
        packages.push(PackageRecord {
            component: c.clone(),
            artifact: rel,
            built,
            source_files,
        });
    }
    progress("provider", "running", None);
    let pj_sha = hex::encode(Sha256::digest(&plan.files.json));
    let built_at = crate::util::rfc3339(plan.built_at);
    let comps: Vec<_> = packages
        .iter()
        .map(|r| {
            let a = artifacts
                .iter()
                .find(|a| a.path == r.artifact)
                .expect("artifact recorded");
            json!({
                "id": r.component.id,
                "version": r.component.version,
                "source": {
                    "file": r.component.file,
                    "url": r.component.url,
                    "sha256": r.component.sha256.to_ascii_lowercase(),
                    "size": r.component.size,
                },
                "artifact": r.artifact,
                "artifact_sha256": a.sha256,
                "size": a.size,
                "binaries": r.built.binaries.iter().map(|(path, sha)| json!({"path": path, "sha256": sha})).collect::<Vec<_>>(),
            })
        })
        .collect();
    let relay_version = plan
        .selected
        .relay_version
        .clone()
        .unwrap_or_else(|| "-".into());
    let manifest = json!({
        "schema": PROVIDER_MANIFEST_SCHEMA,
        "provider_id": p.provider_id,
        "product_name": p.product.name,
        "provider_json_sha256": pj_sha,
        "built_at": built_at,
        "built_by": BUILT_BY,
        "enterprise_version": plan.selected.enterprise_version,
        "taro_version": tv,
        "components_manifest": {
            "release": plan.selected.tag,
            "signed_by": plan.selected.signed_by,
        },
        "provider_files": artifacts.iter().filter(|a| a.component.is_none()).map(|a| json!({"path": a.path, "sha256": a.sha256, "size": a.size})).collect::<Vec<_>>(),
        "components": comps,
        "compatibility": {
            "client_version": tv,
            "relay_version": relay_version,
            "rule": "Clients and the relay must speak the same Taro protocol generation: clients update only to Taro releases the relay supports (update the relay first, or rebuild these packages from a newer Enterprise release).",
        },
        "options": {
            "client_updates": p.updates.clients,
        },
    });
    let mtext = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())? + "\n";
    write(&dir.join(PROVIDER_MANIFEST), mtext.as_bytes())?;
    let relays: Vec<(String, String, String)> = packages
        .iter()
        .filter_map(|r| {
            r.component
                .id
                .strip_prefix("linux-relay-")
                .map(|a| (a.to_string(), r.artifact.clone(), r.built.top.clone()))
        })
        .collect();
    let install = package::install_md(&InstallInfo {
        provider: p,
        taro_version: tv,
        enterprise_version: &plan.selected.enterprise_version,
        windows: packages
            .iter()
            .find(|r| r.component.id == "windows-client")
            .map(|r| r.artifact.as_str()),
        macos: packages
            .iter()
            .find(|r| r.component.id == "macos-client")
            .map(|r| r.artifact.as_str()),
        relays: relays
            .iter()
            .map(|(a, p, t)| (a.as_str(), p.as_str(), t.as_str()))
            .collect(),
    });
    write(&dir.join(INSTALL), install.as_bytes())?;
    // SHA256SUMS: every file but itself (and the temporary downloads)
    let mut lines: Vec<String> = Vec::new();
    let mut files: Vec<String> = artifacts.iter().map(|a| a.path.clone()).collect();
    files.push(PROVIDER_MANIFEST.into());
    files.push(INSTALL.into());
    files.sort();
    for f in &files {
        let (sha, _) = sha_file(&dir.join(f))?;
        lines.push(format!("{sha}  {f}"));
    }
    write(&dir.join(SUMS), (lines.join("\n") + "\n").as_bytes())?;
    for f in [PROVIDER_MANIFEST, INSTALL] {
        let (sha, size) = sha_file(&dir.join(f))?;
        artifacts.push(Artifact {
            component: None,
            path: f.into(),
            sha256: sha,
            size,
        });
    }
    progress(
        "provider",
        "ok",
        Some(format!("{PROVIDER_MANIFEST}, {SUMS}, {INSTALL}")),
    );
    progress("validate", "running", None);
    Ok(BuildResult {
        dir: dir.display().to_string(),
        name: plan.name.clone(),
        built_at,
        taro_version: tv.clone(),
        enterprise_version: plan.selected.enterprise_version.clone(),
        provider_json_sha256: pj_sha,
        artifacts,
        rev: plan.rev,
        provider_json: plan.files.json.clone(),
        logo: plan.files.logo.clone(),
        icon: plan.files.icon.clone(),
        packages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_windows_names() {
        for bad in ["CON", "con", "Nul", "COM1", "lpt9", "aux.txt", "Acme."] {
            assert!(windows_reserved(bad), "{bad}");
        }
        for ok in ["Acme", "Console", "COM", "COMX", "Acme Connect"] {
            assert!(!windows_reserved(ok), "{ok}");
        }
    }

    #[test]
    fn output_dirs() {
        assert!(check_output_dir("").is_err());
        assert!(check_output_dir("relative/dir").is_err());
        let t = std::env::temp_dir();
        assert!(check_output_dir(&t.join("x/../y").display().to_string()).is_err());
        assert_eq!(check_output_dir(&format!(" {} ", t.display())).unwrap(), t);
    }
}
