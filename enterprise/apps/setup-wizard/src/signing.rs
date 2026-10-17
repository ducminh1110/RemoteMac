//! Step 5: everything from one GitHub token, A to Z.
//!
//! The operator pastes a token (a classic `ghp_…` with `repo` and `workflow`, or a
//! fine-grained one) and presses **Start**. The wizard then, with no secret handled by hand:
//!
//! 1. makes the operator's own copy of the Taro repository (a fork in their account; Actions
//!    turned on), or brings an existing copy up to date with it;
//! 2. makes a release signing key (ed25519) on this PC, stores its private half as the Actions
//!    secret `TARO_ENTERPRISE_SIGNING_KEY` (sealed for the repository: only Actions can read it
//!    back), commits its public half to `enterprise/keys/release-signing.pub` (the relays built
//!    there trust it; taro-update refuses to sign with any other key) and backs it up on this PC
//!    (DPAPI on Windows);
//! 3. starts the release workflow for the version of the copy's Cargo.toml and follows it
//!    until GitHub Actions has built and signed the release (free on GitHub);
//! 4. checks the release's signature (Step 6), builds the packages with the operator's
//!    product, branding and relay (Steps 2-4) and validates them (Steps 7-8).
//!
//! Run again, it reuses what exists: the copy is synced, the key of this PC's backup is kept,
//! a version already released is used as it is. A key is never replaced silently: an unknown
//! secret is kept unless the operator asks for a new key; then the new public key goes next to
//! the old ones when releases signed by them exist. The private key is never shown, logged,
//! put in a package, or sent anywhere but sealed to GitHub; the token only goes to GitHub's API.
//!
//! One exception, when GitHub refuses the secret (an organization policy, a token without
//! Secrets access): the run stops, the key is kept in memory and the page shows how to add the
//! secret by hand in the repository's settings, with a button that copies the key (only then,
//! only for that repository). Start again sees the secret has been added and goes on with the
//! same key. Start also lets the repository's workflows publish releases (Workflow permissions:
//! Read and write); when GitHub refuses that, the page says where to change it.

use crate::config::{
    Endpoints, KEYS_FILE, NEW_TOKEN_URL, RELEASE_WORKFLOW, SIGNING_SECRET, TAG_PREFIX,
    VERSION_FILE, WORKFLOW_PATH,
};
use crate::github::{GhError, GitHub, Repo, Run};
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
    /// the Taro repository copies are made from
    pub upstream: String,
    /// the operator's repository, once it exists
    pub repo: Option<Repo>,
    /// Start makes this copy of `upstream` first (it does not exist yet)
    pub fork_to: Option<String>,
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
    pub new_token_url: String,
    pub items: Vec<BuildItem>,
    pub running: bool,
    pub run: Option<Run>,
    pub error: Option<String>,
    pub done: bool,
    #[serde(skip)]
    pub token: Option<Secret>,
    #[serde(skip)]
    pub key_secret: Option<Secret>,
    /// GitHub refused the secret: the page shows how to add it by hand (Settings, Secrets and
    /// variables, Actions) and can copy `pending_key` for it
    pub manual_secret: bool,
    /// the key waiting to be added by hand, its repository, and the secret's `updated_at` when
    /// GitHub refused it (a different one later: the operator has added it)
    #[serde(skip)]
    pub pending_key: Option<(String, Secret, Option<String>)>,
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

fn e(x: GhError) -> String {
    x.to_string()
}

/// The release repository: the operator's copy once Step 5 has one, else the default.
pub fn release_endpoints(app: &AppState) -> Endpoints {
    let mut e = app.endpoints.clone();
    if let Some(r) = &app.w().signing.repo {
        e.repo = r.full_name.clone();
    }
    e
}

/// What a repository holds for Step 5.
struct Inspection {
    has_secret: bool,
    keys: Vec<String>,
    version: Option<String>,
    releases: Vec<String>,
    workflow: bool,
}

async fn inspect(g: &GitHub, r: &Repo) -> Result<Inspection, String> {
    let full = &r.full_name;
    let b = &r.default_branch;
    let has_secret = g
        .has_secret(full, SIGNING_SECRET)
        .await
        .map_err(|x| format!("Could not look at {full}'s secrets: {x}"))?;
    let keys = g
        .file(full, KEYS_FILE, b)
        .await
        .map_err(e)?
        .map(|f| keys_in(&f.text))
        .unwrap_or_default();
    let version = g
        .file(full, VERSION_FILE, b)
        .await
        .map_err(e)?
        .and_then(|f| workspace_version(&f.text));
    let workflow = g.file(full, WORKFLOW_PATH, b).await.map_err(e)?.is_some();
    let releases = g
        .release_tags(full)
        .await
        .map_err(e)?
        .into_iter()
        .filter(|t| t.starts_with(TAG_PREFIX))
        .collect();
    Ok(Inspection {
        has_secret,
        keys,
        version,
        releases,
        workflow,
    })
}

fn apply(app: &AppState, i: Inspection) {
    let mut w = app.w();
    let s = &mut w.signing;
    s.has_secret = i.has_secret;
    s.repo_keys = i.keys;
    s.version = i.version;
    s.releases = i.releases;
    w.rev += 1;
}

/// Load this PC's backup of `full`'s key, unless a key is held already.
fn take_backup(app: &AppState, full: &str) {
    let mut w = app.w();
    let s = &mut w.signing;
    if s.key_secret.is_some() {
        return;
    }
    if let Some(b) = load_backup(&app.key_dir, full) {
        app.log.register_secret(&b);
        s.key_public = public_of(&b);
        s.key_source = Some("the backup on this PC".into());
        s.backup_path = Some(backup_file(&app.key_dir, full).display().to_string());
        s.key_secret = Some(b);
    }
}

/// Sign in with a token and find the operator's repository. `repo` empty: their own copy of
/// the Taro repository (the repository itself for its owner), made by Start when missing.
pub async fn connect(app: &Arc<AppState>, token: Secret, repo: &str) -> Result<String, String> {
    app.log.register_secret(&token);
    let g = gh(app, &token);
    let u = g
        .user()
        .await
        .map_err(|x| format!("The token was not accepted: {x}"))?;
    if let Some(sc) = &u.scopes {
        let missing: Vec<&str> = ["repo", "workflow"]
            .into_iter()
            .filter(|n| !sc.iter().any(|x| x == n))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "This token lacks the {} scope{}. Make a token with repo and workflow: {NEW_TOKEN_URL}",
                missing.join(" and "),
                if missing.len() > 1 { "s" } else { "" }
            ));
        }
    }
    let login = u.login;
    let upstream = app.endpoints.repo.clone();
    let (up_owner, up_name) = upstream.split_once('/').unwrap_or(("", &upstream));
    let auto = repo.is_empty();
    let target = if !auto {
        repo.to_string()
    } else if up_owner.eq_ignore_ascii_case(&login) {
        upstream.clone()
    } else {
        format!("{login}/{up_name}")
    };
    let found = g.find_repo(&target).await.map_err(e)?;
    let msg = match found {
        Some(r) => {
            if !r.admin {
                return Err(format!(
                    "The token cannot change {}'s settings and secrets. Use a token of its owner (classic: repo and workflow; fine-grained: Administration, Contents, Actions and Secrets, read and write).",
                    r.full_name
                ));
            }
            let fork = r.parent.as_deref() == Some(upstream.as_str());
            let i = inspect(&g, &r).await?;
            if !i.workflow && !fork {
                return Err(format!(
                    "{} has no {WORKFLOW_PATH} on {}. Leave Repository empty to use your own copy of {upstream}.",
                    r.full_name, r.default_branch
                ));
            }
            let full = r.full_name.clone();
            apply(app, i);
            {
                let mut w = app.w();
                w.signing.repo = Some(r);
                w.signing.fork_to = None;
            }
            take_backup(app, &full);
            if fork {
                format!("Signed in as {login}. Start brings your copy {full} up to date with {upstream} and releases from it.")
            } else {
                format!("Signed in as {login}; {full} is ready.")
            }
        }
        None if auto => {
            let up = g
                .repo(&upstream)
                .await
                .map_err(|x| format!("{upstream}: {x}"))?;
            let version = g
                .file(&upstream, VERSION_FILE, &up.default_branch)
                .await
                .map_err(e)?
                .and_then(|f| workspace_version(&f.text));
            let mut w = app.w();
            let s = &mut w.signing;
            s.repo = None;
            s.fork_to = Some(target.clone());
            s.has_secret = false;
            s.repo_keys = vec![];
            s.releases = vec![];
            s.version = version;
            format!("Signed in as {login}. Start makes your own copy of {upstream} ({target}) and does the rest.")
        }
        None => {
            return Err(format!(
                "{target} does not exist, or the token cannot see it. Leave Repository empty to make your own copy of {upstream}."
            ))
        }
    };
    let mut w = app.w();
    let s = &mut w.signing;
    s.login = Some(login.clone());
    s.upstream = upstream;
    s.new_token_url = NEW_TOKEN_URL.into();
    s.token = Some(token);
    s.error = None;
    s.items.clear();
    s.done = false;
    // a key waiting to be added by hand is for its own repository only
    let here = s.repo.as_ref().map(|r| r.full_name.clone());
    s.manual_secret = s
        .pending_key
        .as_ref()
        .is_some_and(|(r, _, _)| Some(r) == here.as_ref());
    w.rev += 1;
    app.log.info(format!("GitHub: signed in as {login}"));
    Ok(msg)
}

pub struct StartReq {
    pub taro_version: Option<String>,
    /// make a new key even though the repository already has the secret
    pub new_key: bool,
    /// go on to Steps 6-8: check the release, build and validate the packages
    pub build: bool,
}

/// Plan and run Start in the background; the items show its progress.
pub fn start(app: &Arc<AppState>, req: StartReq) -> Result<(), String> {
    let (token, repo, fork_to, upstream) = {
        let w = app.w();
        let s = &w.signing;
        if s.running {
            return Err("Start is already running.".into());
        }
        if w.build.running {
            return Err("A build is running; wait for it or cancel it.".into());
        }
        let Some(t) = s.token.clone() else {
            return Err("Sign in to GitHub first.".into());
        };
        if s.repo.is_none() && s.fork_to.is_none() {
            return Err("Sign in to GitHub first.".into());
        }
        if req.build {
            w.provider(None).map_err(|x| {
                format!("{x} The packages carry them; or untick “Build the packages”.")
            })?;
        }
        (t, s.repo.clone(), s.fork_to.clone(), s.upstream.clone())
    };
    let mut items = vec![];
    if let Some(t) = &fork_to {
        items.push(item("fork", format!("Make your copy of {upstream} ({t})")));
        items.push(item("actions", "Turn on GitHub Actions in your copy"));
    } else if repo.as_ref().and_then(|r| r.parent.as_deref()) == Some(upstream.as_str()) {
        items.push(item("sync", format!("Bring in the latest of {upstream}")));
    }
    items.push(item(
        "key",
        "Release signing key (ed25519, made on this PC)",
    ));
    items.push(item(
        "secret",
        format!("Actions secret {SIGNING_SECRET} (sealed for the repository)"),
    ));
    items.push(item("pub", format!("Its public half in {KEYS_FILE}")));
    items.push(item("backup", "Encrypted backup of the key on this PC"));
    items.push(item(
        "perms",
        "Let the workflow publish releases (Workflow permissions: read and write)",
    ));
    items.push(item(
        "release",
        "Release, built and signed by GitHub Actions",
    ));
    if req.build {
        items.push(item("verify", "Check the release's signature"));
        items.push(item("build", "Build and validate your packages"));
    }
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
        let r = run(&app2, &token, repo, fork_to, &upstream, &req).await;
        let mut w = app2.w();
        let s = &mut w.signing;
        s.running = false;
        match r {
            Ok(()) => s.done = true,
            Err(x) => {
                for it in s.items.iter_mut().filter(|i| i.status == "running") {
                    it.status = "fail".into();
                    it.detail = Some(x.clone());
                }
                s.error = Some(x.clone());
                app2.log.warn(format!("Step 5: {x}"));
            }
        }
        w.rev += 1;
    });
    Ok(())
}

fn skipped(app: &AppState, id: &str, why: &str) {
    set(app, id, "skipped", Some(why.into()));
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
    repo: Option<Repo>,
    fork_to: Option<String>,
    upstream: &str,
    req: &StartReq,
) -> Result<(), String> {
    let g = gh(app, token);
    let poll = app.timings.actions_poll;

    // 1. the operator's copy
    let repo = match (repo, fork_to) {
        (_, Some(_)) => {
            set(app, "fork", "running", None);
            let f = g
                .fork(upstream)
                .await
                .map_err(|x| format!("could not make your copy: {x}"))?;
            // GitHub copies in the background
            let mut ready = false;
            for _ in 0..120 {
                if g.file(&f.full_name, VERSION_FILE, &f.default_branch)
                    .await
                    .ok()
                    .flatten()
                    .is_some()
                {
                    ready = true;
                    break;
                }
                tokio::time::sleep(poll).await;
            }
            if !ready {
                return Err(format!(
                    "GitHub has not finished copying {} yet; press Start again in a minute.",
                    f.full_name
                ));
            }
            let r = g.repo(&f.full_name).await.map_err(e)?;
            set(app, "fork", "ok", Some(r.html_url.clone()));
            set(app, "actions", "running", None);
            g.enable_actions(&r.full_name, RELEASE_WORKFLOW)
                .await
                .map_err(|x| format!("could not turn on GitHub Actions: {x}"))?;
            set(app, "actions", "ok", Some("on".into()));
            app.log
                .info(format!("Step 5: made {} from {upstream}", r.full_name));
            r
        }
        (Some(r), None) => {
            if r.parent.as_deref() == Some(upstream) {
                set(app, "sync", "running", None);
                match g.merge_upstream(&r.full_name, &r.default_branch).await {
                    Ok(m) => set(app, "sync", "ok", Some(m)),
                    Err(GhError {
                        status: Some(409), ..
                    }) => {
                        return Err(format!(
                            "your copy {} has changes that conflict with {upstream}: open {} and choose Sync fork",
                            r.full_name, r.html_url
                        ))
                    }
                    Err(x) => return Err(format!("could not update your copy: {x}")),
                }
            }
            r
        }
        (None, None) => return Err("Sign in to GitHub first.".into()),
    };
    let full = repo.full_name.clone();
    {
        let mut w = app.w();
        w.signing.repo = Some(repo.clone());
        w.signing.fork_to = None;
        w.rev += 1;
    }
    let i = inspect(&g, &repo).await?;
    if !i.workflow {
        return Err(format!(
            "{full} has no {WORKFLOW_PATH} on {}",
            repo.default_branch
        ));
    }
    apply(app, i);
    take_backup(app, &full);

    // 2. the signing key
    let (has_secret, held, pending) = {
        let w = app.w();
        let s = &w.signing;
        let held = s
            .key_public
            .as_ref()
            .is_some_and(|p| s.repo_keys.contains(p));
        let pending = s.pending_key.as_ref().is_some_and(|(r, _, _)| r == &full);
        (s.has_secret, held, pending)
    };
    if req.new_key || !has_secret || pending {
        new_key(app, &g, &repo).await?;
    } else {
        let why = if held {
            "the repository has the key this PC holds; nothing to change".to_string()
        } else {
            format!("{SIGNING_SECRET} is set already and is kept (the workflow refuses it unless it matches {KEYS_FILE})")
        };
        for id in ["key", "secret", "pub", "backup"] {
            skipped(app, id, &why);
        }
    }

    // 3. the release: GITHUB_TOKEN must be allowed to publish it
    set(app, "perms", "running", None);
    match g.set_workflow_write(&full).await {
        Ok(()) => set(app, "perms", "ok", Some("read and write".into())),
        Err(x) => set(
            app,
            "perms",
            "manual",
            Some(format!(
                "GitHub refused the change ({x}). If the release cannot be published, choose Workflow permissions: Read and write permissions, then Save: {}/settings/actions",
                repo.html_url
            )),
        ),
    }
    let version =
        app.w().signing.version.clone().ok_or_else(|| {
            format!("{VERSION_FILE} of {full} has no [workspace.package] version")
        })?;
    let tag = format!("{TAG_PREFIX}{version}");
    if app.w().signing.releases.contains(&tag) {
        skipped(
            app,
            "release",
            &format!("{tag} exists in {full}; it is used as it is"),
        );
    } else {
        release(app, &g, &repo, &version, &tag, req).await?;
    }
    if !req.build {
        return Ok(());
    }

    // 4. Steps 6-8
    set(app, "verify", "running", None);
    let ep = release_endpoints(app);
    let list = crate::releases::discover(&app.http, &ep, &app.timings).await?;
    let rel = list
        .iter()
        .find(|r| r.tag == tag)
        .cloned()
        .ok_or_else(|| format!("{tag} is not among {full}'s releases yet"))?;
    let keys = repo_trust(app);
    let sel = crate::releases::fetch_verified(&app.http, &ep, &app.timings, &keys, &rel).await?;
    set(
        app,
        "verify",
        "ok",
        Some(format!(
            "signed by {}; Taro {}",
            sel.signed_by, sel.taro_version
        )),
    );
    let out = {
        let mut w = app.w();
        w.components.releases = Some(list);
        w.components.error = None;
        w.components.selected = Some(sel);
        w.rev += 1;
        w.build.output_dir.clone()
    };
    set(app, "build", "running", Some("downloading".into()));
    crate::build::start(app, &out, true)?;
    loop {
        tokio::time::sleep(poll).await;
        let w = app.w();
        if w.build.running {
            let phase = w.build.phase.clone();
            drop(w);
            set(app, "build", "running", Some(phase));
            continue;
        }
        let (result, err, val) = (
            w.build.result.clone(),
            w.build.error.clone(),
            w.validation.clone(),
        );
        drop(w);
        return match (result, val) {
            (Some(r), Some(v)) if v.ok => {
                set(
                    app,
                    "build",
                    "ok",
                    Some(format!("{}: {} checks passed", r.name, v.checks.len())),
                );
                Ok(())
            }
            (Some(_), _) => {
                Err("the packages were built but did not pass validation (Step 8)".into())
            }
            (None, _) => Err(err.unwrap_or_else(|| "the build stopped".into())),
        };
    }
}

async fn new_key(app: &Arc<AppState>, g: &GitHub, repo: &Repo) -> Result<(), String> {
    let full = &repo.full_name;
    // the key of a Start that stopped for the secret to be added by hand
    let pending = app
        .w()
        .signing
        .pending_key
        .clone()
        .filter(|(r, _, _)| r == full);
    let secret = match &pending {
        Some((_, s, _)) => {
            let p = public_of(s).ok_or("the kept key is not valid")?;
            set(
                app,
                "key",
                "ok",
                Some(format!("{p} (kept from the last Start)")),
            );
            s.clone()
        }
        None => {
            set(app, "key", "running", None);
            let k = sig::generate();
            let secret = Secret::new(sig::secret_text(&k));
            app.log.register_secret(&secret);
            set(app, "key", "ok", Some(sig::public_text(&k.verifying_key())));
            secret
        }
    };
    let public = public_of(&secret).ok_or("the key is not valid")?;

    set(app, "secret", "running", None);
    let now = g.secret_updated(full, SIGNING_SECRET).await.map_err(e)?;
    let by_hand = match &pending {
        Some((_, _, before)) => now.is_some() && &now != before,
        None => false,
    };
    if by_hand {
        set(
            app,
            "secret",
            "ok",
            Some("added by you in GitHub's settings".into()),
        );
    } else {
        let stored = async {
            let (key_id, repo_key) = g.secrets_key(full).await.map_err(e)?;
            let sealed = crate::sealed::seal_for_github(&repo_key, &secret)?;
            g.put_secret(full, SIGNING_SECRET, &sealed, &key_id)
                .await
                .map_err(e)
        }
        .await;
        if let Err(x) = stored {
            {
                let mut w = app.w();
                w.signing.pending_key = Some((full.clone(), secret.clone(), now));
                w.signing.manual_secret = true;
            }
            set(
                app,
                "secret",
                "manual",
                Some(format!(
                    "GitHub refused it ({x}); add it yourself as shown below"
                )),
            );
            app.log.warn(format!(
                "Step 5: GitHub refused {SIGNING_SECRET} for {full}; the operator adds it by hand"
            ));
            return Err(format!(
                "GitHub did not let this program store {SIGNING_SECRET} ({x}). Add it yourself as shown in “Add the secret yourself”, then press Start again: {}/settings/secrets/actions/new",
                repo.html_url
            ));
        }
        set(
            app,
            "secret",
            "ok",
            Some("stored; only GitHub Actions can read it".into()),
        );
    }
    {
        let mut w = app.w();
        w.signing.pending_key = None;
        w.signing.manual_secret = false;
        w.signing.has_secret = true;
        w.signing.key_public = Some(public.clone());
        w.signing.key_source = Some("made on this PC".into());
        w.signing.key_secret = Some(secret.clone());
    }

    // next to the old keys when releases signed by them exist
    set(app, "pub", "running", None);
    let current = g
        .file(full, KEYS_FILE, &repo.default_branch)
        .await
        .map_err(e)?;
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
        .map_err(|x| format!("could not commit {KEYS_FILE}: {x}"))?;
    app.w().signing.repo_keys = keys_in(&text);
    set(
        app,
        "pub",
        "ok",
        Some(format!("commit {}", commit.get(..7).unwrap_or(&commit))),
    );

    set(app, "backup", "running", None);
    let path = backup_file(&app.key_dir, full);
    let protected = crate::platform::protect(secret.expose().as_bytes())?;
    crate::platform::write_private(&path, &protected)?;
    app.w().signing.backup_path = Some(path.display().to_string());
    set(app, "backup", "ok", Some(path.display().to_string()));
    app.log
        .info(format!("release signing key made for {full}: {public}"));
    Ok(())
}

async fn release(
    app: &Arc<AppState>,
    g: &GitHub,
    repo: &Repo,
    version: &str,
    tag: &str,
    req: &StartReq,
) -> Result<(), String> {
    let full = &repo.full_name;
    set(
        app,
        "release",
        "running",
        Some(format!("starting {RELEASE_WORKFLOW} for {tag}")),
    );
    let started = crate::util::rfc3339(crate::util::now_secs().saturating_sub(5));
    let mut inputs = json!({ "version": version });
    if let Some(t) = req.taro_version.as_deref().filter(|t| !t.is_empty()) {
        inputs["taro_version"] = json!(t);
    }
    g.dispatch(full, RELEASE_WORKFLOW, &repo.default_branch, inputs)
        .await
        .map_err(|x| format!("could not start {RELEASE_WORKFLOW}: {x}"))?;
    let poll = app.timings.actions_poll;
    let mut found: Option<Run> = None;
    loop {
        tokio::time::sleep(poll).await;
        let runs = match g.runs(full, RELEASE_WORKFLOW).await {
            Ok(r) => r,
            Err(x) => {
                app.log.warn(format!("looking at the run: {x}"));
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
                "release",
                "running",
                Some(format!(
                    "{tag} {}: {}",
                    run.status.replace('_', " "),
                    run.html_url
                )),
            );
            continue;
        }
        return match run.conclusion.as_deref() {
            Some("success") => {
                set(
                    app,
                    "release",
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

/// The release keys of the operator's repository, which Step 6 trusts in addition to the
/// wizard's own (read over HTTPS with the operator's token).
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
