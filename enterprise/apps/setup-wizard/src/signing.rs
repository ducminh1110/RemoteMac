//! Step 5: release signing and the release itself, on the operator's GitHub repository.
//!
//! With a GitHub token and the repository (the one holding the Enterprise release workflow),
//! **Start** does everything a signed release needs, with no secret handled by hand:
//!
//! 1. a release signing key (ed25519) is made on this PC;
//! 2. its private half is stored as the Actions secret `TARO_ENTERPRISE_SIGNING_KEY`, sealed
//!    for the repository's public key (only GitHub Actions can read it back);
//! 3. its public half is committed to `enterprise/keys/release-signing.pub`, so the relays and
//!    the wizard built there trust it (taro-update refuses to sign with any other key);
//! 4. a backup is written on this PC (DPAPI on Windows);
//! 5. the release workflow is started for the version in the repository's Cargo.toml, and
//!    followed until GitHub Actions has built and signed the release (free on GitHub).
//!
//! A key is never replaced silently: when the secret exists already it is kept (or the backup
//! of this PC is reused), unless the operator asks for a new key; then the new public key is
//! added next to the old ones when releases signed by them exist (relays installed from those
//! trust only the keys they were built with). The private key is never shown, logged, put in a
//! package, or sent anywhere but sealed to GitHub.

use crate::config::{KEYS_FILE, RELEASE_WORKFLOW, SIGNING_SECRET, TAG_PREFIX, VERSION_FILE};
use crate::github::{GitHub, Repo, Run};
use crate::secret::Secret;
use crate::state::{AppState, BuildItem};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use taro_update::sig;

const KEYS_HEADER: &str = "\
# Taro Enterprise release signing keys (ed25519). Every enterprise-update.zip and
# taro-enterprise-components.json is signed with the private half, held only in the
# TARO_ENTERPRISE_SIGNING_KEY secret of the release workflow (TaroEnterpriseSetup.exe makes both
# halves when you press Start). To rotate: add the new key
# here first (one release signed by the old key carries it), then switch the secret.
";

#[derive(Clone, Debug, Serialize, Default)]
pub struct SigningState {
    pub login: Option<String>,
    pub repo: Option<Repo>,
    /// TARO_ENTERPRISE_SIGNING_KEY exists in the repository
    pub has_secret: bool,
    /// the public keys in enterprise/keys/release-signing.pub
    pub repo_keys: Vec<String>,
    /// enterprise-v… tags that exist
    pub releases: Vec<String>,
    /// the workspace version of enterprise/Cargo.toml (what the workflow releases)
    pub version: Option<String>,
    /// the public half of the key this session holds (generated or from the backup)
    pub key_public: Option<String>,
    pub key_source: Option<String>,
    pub backup_path: Option<String>,
    pub items: Vec<BuildItem>,
    pub running: bool,
    pub run: Option<Run>,
    pub error: Option<String>,
    pub done: bool,
    #[serde(skip)]
    pub token: Option<Secret>,
    #[serde(skip)]
    pub key_secret: Option<Secret>,
}

fn item(id: &str, label: impl Into<String>) -> BuildItem {
    BuildItem {
        id: id.into(),
        label: label.into(),
        status: "pending".into(),
        detail: None,
        done: 0,
        total: 0,
    }
}

/// The public keys of a release-signing.pub file (comments and blank lines skipped).
pub fn keys_in(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `version = "x.y.z"` of `[workspace.package]` in a Cargo.toml.
pub fn workspace_version(toml: &str) -> Option<String> {
    let mut inside = false;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            inside = t == "[workspace.package]";
            continue;
        }
        if inside {
            if let Some(v) = t.strip_prefix("version") {
                let v = v.trim_start().strip_prefix('=')?.trim().trim_matches('"');
                return Some(v.to_string());
            }
        }
    }
    None
}

fn backup_file(dir: &std::path::Path, repo: &str) -> PathBuf {
    dir.join(format!("{}.key", repo.replace('/', "__")))
}

/// The key backed up on this PC for `repo`, if any.
pub fn load_backup(dir: &std::path::Path, repo: &str) -> Option<Secret> {
    let raw = std::fs::read(backup_file(dir, repo)).ok()?;
    let plain = crate::platform::unprotect(&raw).ok()?;
    let text = String::from_utf8(plain).ok()?;
    sig::parse_secret(text.trim()).ok()?;
    Some(Secret::new(text.trim().to_string()))
}

fn public_of(secret: &Secret) -> Option<String> {
    sig::parse_secret(secret.expose())
        .ok()
        .map(|k| sig::public_text(&k.verifying_key()))
}

fn gh(app: &AppState, token: &Secret) -> GitHub {
    GitHub::new(&app.endpoints.github_api, app.http.clone(), token.clone())
}

/// Sign in with a token and look at the repository: its secret, keys file, releases, version.
pub async fn connect(app: &Arc<AppState>, token: Secret, repo: &str) -> Result<String, String> {
    app.log.register_secret(&token);
    let g = gh(app, &token);
    let login = g
        .login()
        .await
        .map_err(|e| format!("The token was not accepted: {e}"))?;
    let r = g
        .repo(repo)
        .await
        .map_err(|e| format!("{repo}: {e}. The token needs access to this repository."))?;
    if !r.admin {
        return Err(format!(
            "The token cannot change {}'s secrets. Give it the Secrets (read and write), Contents (read and write) and Actions (read and write) permissions of this repository.",
            r.full_name
        ));
    }
    let has_secret = g
        .has_secret(&r.full_name, SIGNING_SECRET)
        .await
        .map_err(|e| format!("Could not look at the repository's secrets: {e}"))?;
    let keys = g
        .file(&r.full_name, KEYS_FILE, &r.default_branch)
        .await
        .map_err(|e| e.to_string())?
        .map(|f| keys_in(&f.text))
        .unwrap_or_default();
    let version = g
        .file(&r.full_name, VERSION_FILE, &r.default_branch)
        .await
        .map_err(|e| e.to_string())?
        .and_then(|f| workspace_version(&f.text));
    let releases: Vec<String> = g
        .release_tags(&r.full_name)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|t| t.starts_with(TAG_PREFIX))
        .collect();
    let backup = load_backup(&app.key_dir, &r.full_name);
    let full = r.full_name.clone();
    let mut w = app.w();
    let s = &mut w.signing;
    s.login = Some(login.clone());
    s.has_secret = has_secret;
    s.repo_keys = keys;
    s.releases = releases;
    s.version = version;
    s.repo = Some(r);
    s.token = Some(token);
    s.error = None;
    if s.key_secret.is_none() {
        if let Some(b) = backup {
            app.log.register_secret(&b);
            s.key_public = public_of(&b);
            s.key_source = Some("the backup on this PC".into());
            s.backup_path = Some(backup_file(&app.key_dir, &full).display().to_string());
            s.key_secret = Some(b);
        }
    }
    w.rev += 1;
    app.log
        .info(format!("GitHub: signed in as {login}, repository {full}"));
    Ok(format!("Signed in as {login}; {full} is ready."))
}

pub struct StartReq {
    pub version: String,
    pub taro_version: Option<String>,
    /// make a new key even though the repository already has the secret
    pub new_key: bool,
}

/// Plan and run Start in the background; the items show its progress.
pub fn start(app: &Arc<AppState>, req: StartReq) -> Result<(), String> {
    let (token, repo) = {
        let w = app.w();
        let s = &w.signing;
        if s.running {
            return Err("Start is already running.".into());
        }
        match (&s.token, &s.repo) {
            (Some(t), Some(r)) => (t.clone(), r.clone()),
            _ => return Err("Sign in to GitHub first.".into()),
        }
    };
    let tag = format!("{TAG_PREFIX}{}", req.version);
    {
        let w = app.w();
        let s = &w.signing;
        if s.releases.contains(&tag) {
            return Err(format!(
                "{tag} exists already. Raise the version in {VERSION_FILE} for a new release."
            ));
        }
        match &s.version {
            Some(v) if *v == req.version => {}
            Some(v) => {
                return Err(format!(
                    "The release workflow releases the version of {VERSION_FILE}, which is {v} (not {}).",
                    req.version
                ))
            }
            None => return Err(format!("{VERSION_FILE} has no [workspace.package] version.")),
        }
    }
    let reuse = {
        let w = app.w();
        let s = &w.signing;
        let held = s
            .key_public
            .as_ref()
            .is_some_and(|p| s.repo_keys.contains(p));
        if req.new_key {
            Reuse::New
        } else if s.has_secret && held {
            Reuse::Held
        } else if s.has_secret {
            Reuse::Secret
        } else {
            Reuse::New
        }
    };
    let mut items = vec![];
    match reuse {
        Reuse::New => {
            items.push(item("key", "Make a release signing key on this PC (ed25519)"));
            items.push(item("secret", format!("Store it as the Actions secret {SIGNING_SECRET} (sealed for the repository)")));
            items.push(item("pub", format!("Commit its public half to {KEYS_FILE}")));
            items.push(item("backup", "Keep an encrypted backup of the key on this PC"));
        }
        Reuse::Held => items.push(skipped("key", "Signing key", "the repository's secret matches the key this PC holds; nothing to change")),
        Reuse::Secret => items.push(skipped("key", "Signing key", &format!("{SIGNING_SECRET} is set already and is kept (the workflow refuses it unless it matches {KEYS_FILE})"))),
    }
    items.push(item(
        "dispatch",
        format!("Start the Enterprise release workflow for {tag}"),
    ));
    items.push(item("run", "GitHub Actions builds and signs the release"));
    {
        let mut w = app.w();
        let s = &mut w.signing;
        s.items = items;
        s.running = true;
        s.done = false;
        s.error = None;
        s.run = None;
        w.rev += 1;
    }
    let app2 = app.clone();
    tokio::spawn(async move {
        let r = run(&app2, &token, &repo, &req, reuse, &tag).await;
        let mut w = app2.w();
        let s = &mut w.signing;
        s.running = false;
        match r {
            Ok(()) => s.done = true,
            Err(e) => {
                for it in s.items.iter_mut().filter(|i| i.status == "running") {
                    it.status = "fail".into();
                    it.detail = Some(e.clone());
                }
                s.error = Some(e.clone());
                app2.log.warn(format!("release signing: {e}"));
            }
        }
        w.rev += 1;
    });
    Ok(())
}

#[derive(Clone, Copy)]
enum Reuse {
    New,
    Held,
    Secret,
}

fn skipped(id: &str, label: &str, why: &str) -> BuildItem {
    BuildItem {
        status: "skipped".into(),
        detail: Some(why.into()),
        ..item(id, label)
    }
}

fn set(app: &AppState, id: &str, status: &str, detail: Option<String>) {
    let mut w = app.w();
    if let Some(it) = w.signing.items.iter_mut().find(|i| i.id == id) {
        it.status = status.into();
        it.detail = detail;
    }
    w.rev += 1;
}

async fn run(
    app: &Arc<AppState>,
    token: &Secret,
    repo: &Repo,
    req: &StartReq,
    reuse: Reuse,
    tag: &str,
) -> Result<(), String> {
    let g = gh(app, token);
    let full = &repo.full_name;
    if let Reuse::New = reuse {
        // 1. the key
        set(app, "key", "running", None);
        let k = sig::generate();
        let secret = Secret::new(sig::secret_text(&k));
        app.log.register_secret(&secret);
        let public = sig::public_text(&k.verifying_key());
        drop(k);
        set(app, "key", "ok", Some(public.clone()));

        // 2. the secret, sealed for the repository
        set(app, "secret", "running", None);
        let (key_id, repo_key) = g.secrets_key(full).await.map_err(|e| e.to_string())?;
        let sealed = crate::sealed::seal_for_github(&repo_key, &secret)?;
        g.put_secret(full, SIGNING_SECRET, &sealed, &key_id)
            .await
            .map_err(|e| format!("could not store the secret: {e}"))?;
        set(
            app,
            "secret",
            "ok",
            Some("stored; only GitHub Actions can read it".into()),
        );
        {
            let mut w = app.w();
            w.signing.has_secret = true;
            w.signing.key_public = Some(public.clone());
            w.signing.key_source = Some("made on this PC".into());
            w.signing.key_secret = Some(secret.clone());
        }

        // 3. the public half in the repository (next to the old keys when releases exist)
        set(app, "pub", "running", None);
        let current = g
            .file(full, KEYS_FILE, &repo.default_branch)
            .await
            .map_err(|e| e.to_string())?;
        let has_releases = !app.w().signing.releases.is_empty();
        let text = match (&current, has_releases) {
            (Some(f), true) => format!("{}\n{public}\n", f.text.trim_end()),
            _ => format!("{KEYS_HEADER}{public}\n"),
        };
        let commit = g
            .put_file(
                full,
                KEYS_FILE,
                &repo.default_branch,
                &text,
                current.as_ref().map(|f| f.sha.as_str()),
                "Release signing key made by TaroEnterpriseSetup",
            )
            .await
            .map_err(|e| format!("could not commit {KEYS_FILE}: {e}"))?;
        let keys = keys_in(&text);
        app.w().signing.repo_keys = keys;
        set(
            app,
            "pub",
            "ok",
            Some(format!("commit {}", commit.get(..7).unwrap_or(&commit))),
        );

        // 4. the backup
        set(app, "backup", "running", None);
        let path = backup_file(&app.key_dir, full);
        let protected = crate::platform::protect(secret.expose().as_bytes())?;
        crate::platform::write_private(&path, &protected)?;
        app.w().signing.backup_path = Some(path.display().to_string());
        set(app, "backup", "ok", Some(path.display().to_string()));
        app.log
            .info(format!("release signing key made for {full}: {public}"));
    }

    // 5. the workflow
    set(app, "dispatch", "running", None);
    let started = crate::util::rfc3339(crate::util::now_secs().saturating_sub(5));
    let mut inputs = json!({ "version": req.version });
    if let Some(t) = req.taro_version.as_deref().filter(|t| !t.is_empty()) {
        inputs["taro_version"] = json!(t);
    }
    g.dispatch(full, RELEASE_WORKFLOW, &repo.default_branch, inputs)
        .await
        .map_err(|e| format!("could not start {RELEASE_WORKFLOW}: {e}"))?;
    set(
        app,
        "dispatch",
        "ok",
        Some(format!("{RELEASE_WORKFLOW} on {}", repo.default_branch)),
    );

    // 6. follow the run it started (the newest manual run created after the dispatch)
    set(
        app,
        "run",
        "running",
        Some("waiting for GitHub to start the run".into()),
    );
    let poll = app.timings.actions_poll;
    let mut found: Option<Run> = None;
    loop {
        tokio::time::sleep(poll).await;
        let runs = match g.runs(full, RELEASE_WORKFLOW).await {
            Ok(r) => r,
            Err(e) => {
                app.log.warn(format!("looking at the run: {e}"));
                continue;
            }
        };
        let run = match &found {
            Some(f) => runs.into_iter().find(|r| r.id == f.id),
            None => runs.into_iter().find(|r| r.created_at >= started),
        };
        let Some(run) = run else { continue };
        found = Some(run.clone());
        app.w().signing.run = Some(run.clone());
        if run.status != "completed" {
            set(
                app,
                "run",
                "running",
                Some(format!("{}: {}", run.status, run.html_url)),
            );
            continue;
        }
        return match run.conclusion.as_deref() {
            Some("success") => {
                set(
                    app,
                    "run",
                    "ok",
                    Some(format!("{tag} is published and signed: {}", run.html_url)),
                );
                app.w().signing.releases.push(tag.to_string());
                Ok(())
            }
            other => Err(format!(
                "the release workflow ended with {}: see {}",
                other.unwrap_or("no result"),
                run.html_url
            )),
        };
    }
}

/// The release keys of the connected repository, which Step 6 trusts in addition to the
/// wizard's own (it read them itself, over HTTPS, with the operator's token).
pub fn repo_trust(app: &AppState) -> sig::TrustedKeys {
    let w = app.w();
    let text = w.signing.repo_keys.join("\n");
    match sig::TrustedKeys::parse(&text) {
        Ok(k) if !w.signing.repo_keys.is_empty() => app.keys.merged(&k),
        _ => app.keys.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_keys_are_read() {
        let toml = "[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.3.1\"\nedition = \"2021\"\n[package]\nversion = \"9.9.9\"\n";
        assert_eq!(workspace_version(toml).as_deref(), Some("0.3.1"));
        assert_eq!(workspace_version("[package]\nversion = \"1.0.0\"\n"), None);
        let k = keys_in("# a\n\ned25519:AAA=\n  # b\ned25519:BBB=\n");
        assert_eq!(k, vec!["ed25519:AAA=", "ed25519:BBB="]);
    }
}
