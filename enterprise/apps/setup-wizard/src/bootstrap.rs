//! Step 7: GitHub bootstrap. Builds the exact action plan shown to the operator, then (after
//! explicit confirmation) executes it in the background, recording an honest result per action.

use crate::github::{GhError, GitHub, RepoInfo};
use crate::secret::Secret;
use crate::state::{ActionResult, AppState, Wizard};
use std::sync::Arc;

pub struct SecretItem {
    pub name: &'static str,
    pub value: Option<Secret>,
    pub skip_reason: Option<String>,
    pub label: String,
}

pub struct FileItem {
    pub path: String,
    pub bytes: Vec<u8>,
}

pub struct Input {
    pub template_owner: String,
    pub template_repo: String,
    pub owner: String,
    pub name: String,
    /// Repository created by this wizard session earlier (re-running only re-applies the rest).
    pub reuse: Option<RepoInfo>,
    pub secrets: Vec<SecretItem>,
    pub files: Vec<FileItem>,
}

fn action(id: impl Into<String>, label: impl Into<String>) -> ActionResult {
    ActionResult {
        id: id.into(),
        label: label.into(),
        status: "pending".into(),
        detail: None,
    }
}

/// Gather inputs from the wizard state (validated already). Errors name what is missing.
pub fn gather(w: &mut Wizard, owner: &str, name: &str, template: &str) -> Result<Input, String> {
    let (to, tr) = crate::validate::repo_slug(template)?;
    let owner = crate::validate::owner_name(owner)?;
    let name = crate::validate::repo_name(name)?;
    if w.github.user.is_none() {
        return Err("Sign in to GitHub first.".into());
    }
    if w.deployment.is_none() || w.identity.is_none() {
        return Err("Complete the Deployment mode and Product identity steps first; their values go into deploy/deployment.json.".into());
    }
    let reuse = match &w.github.repo {
        Some(r) if w.github.created_by_wizard && !w.github.deleted => {
            if r.full_name.eq_ignore_ascii_case(&format!("{owner}/{name}")) {
                Some(r.clone())
            } else {
                return Err(format!(
                    "This session already created {}. Finish with that repository (or delete it below) before creating another.",
                    r.full_name
                ));
            }
        }
        _ => None,
    };

    if w.github.taro_secret_key.is_none() {
        w.github.taro_secret_key = Some(Secret::new(crate::secret::random_hex(32)));
    }
    let mut secrets = vec![SecretItem {
        name: "TARO_SECRET_KEY",
        value: w.github.taro_secret_key.clone(),
        skip_reason: None,
        label: "Set Actions secret TARO_SECRET_KEY (new random 32-byte key, generated on this PC)"
            .into(),
    }];
    let billing = w.billing_enabled();
    let mode = match w.billing.mode {
        Some(crate::validate::StripeMode::Live) => "LIVE mode",
        _ => "test mode",
    };
    let (key, key_skip) = if !billing {
        (None, Some("billing module not enabled".to_string()))
    } else if w.billing.account.is_some() {
        (w.billing.key.clone(), None)
    } else {
        (
            None,
            Some("Stripe key not validated in the Billing step".to_string()),
        )
    };
    secrets.push(SecretItem {
        name: "STRIPE_SECRET_KEY",
        value: key,
        skip_reason: key_skip,
        label: format!("Set Actions secret STRIPE_SECRET_KEY (Stripe {mode} key)"),
    });
    let (wh, wh_skip) = if !billing {
        (None, Some("billing module not enabled".to_string()))
    } else if w.billing.webhook_secret.is_some() {
        (w.billing.webhook_secret.clone(), None)
    } else {
        (
            None,
            Some("no webhook signing secret entered in the Billing step".to_string()),
        )
    };
    secrets.push(SecretItem {
        name: "STRIPE_WEBHOOK_SECRET",
        value: wh,
        skip_reason: wh_skip,
        label: "Set Actions secret STRIPE_WEBHOOK_SECRET (webhook signing secret)".into(),
    });

    let mut files = vec![FileItem {
        path: "deploy/deployment.json".into(),
        bytes: serde_json::to_vec_pretty(&crate::summary::deployment_json(w)).unwrap_or_default(),
    }];
    if let (Some(p), Some(u)) = (crate::summary::logo_path(w), &w.branding.logo) {
        files.push(FileItem {
            path: p,
            bytes: u.bytes.clone(),
        });
    }
    if let (Some(p), Some(u)) = (crate::summary::favicon_path(w), &w.branding.favicon) {
        files.push(FileItem {
            path: p,
            bytes: u.bytes.clone(),
        });
    }

    Ok(Input {
        template_owner: to,
        template_repo: tr,
        owner,
        name,
        reuse,
        secrets,
        files,
    })
}

pub fn plan(i: &Input) -> Vec<ActionResult> {
    let t = format!("{}/{}", i.template_owner, i.template_repo);
    let full = format!("{}/{}", i.owner, i.name);
    let mut v = vec![action(
        "template",
        format!("Check that {t} is a template repository this token can read"),
    )];
    v.push(if i.reuse.is_some() {
        action(
            "create",
            format!("Re-use {full}, created earlier in this session"),
        )
    } else {
        action(
            "create",
            format!("Create the PRIVATE repository {full} from the template {t}"),
        )
    });
    v.push(action(
        "workflow_perms",
        "Set the repository's default GITHUB_TOKEN workflow permissions to read-only",
    ));
    for s in &i.secrets {
        let mut a = action(format!("secret:{}", s.name), s.label.clone());
        if let Some(r) = &s.skip_reason {
            a.status = "skipped".into();
            a.detail = Some(format!("Will be skipped: {r}"));
        }
        v.push(a);
    }
    for f in &i.files {
        v.push(action(
            format!("file:{}", f.path),
            format!("Commit {} ({} bytes)", f.path, f.bytes.len()),
        ));
    }
    v
}

fn set(app: &AppState, id: &str, status: &str, detail: Option<String>) {
    let detail = detail.map(|d| app.log.scrub(&d));
    let mut w = app.w();
    if let Some(p) = w.github.plan.as_mut() {
        if let Some(a) = p.iter_mut().find(|a| a.id == id) {
            a.status = status.into();
            a.detail = detail.clone();
        }
    }
    drop(w);
    let msg = format!(
        "bootstrap {id}: {status}{}",
        detail.map(|d| format!(" - {d}")).unwrap_or_default()
    );
    if status == "fail" {
        app.log.error(msg);
    } else {
        app.log.info(msg);
    }
}

fn fail(app: &AppState, id: &str, e: String) {
    set(app, id, "fail", Some(e.clone()));
    let mut w = app.w();
    if let Some(p) = w.github.plan.as_mut() {
        for a in p.iter_mut().filter(|a| a.status == "pending") {
            a.status = "skipped".into();
            a.detail = Some("Not run because an earlier action failed".into());
        }
    }
    w.github.bootstrap_running = false;
    w.github.bootstrap_error = Some(app.log.scrub(&e));
}

fn is_not_ready(e: &GhError) -> bool {
    matches!(e.status, Some(404) | Some(409))
}

/// Retry `f` while GitHub reports the freshly generated repository is not ready yet.
async fn retry<T, F, Fut>(app: &AppState, mut f: F) -> Result<T, GhError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, GhError>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Err(e) if is_not_ready(&e) && attempt + 1 < app.timings.repo_ready_attempts => {
                attempt += 1;
                tokio::time::sleep(app.timings.repo_ready_retry).await;
            }
            other => return other,
        }
    }
}

fn gh_hint(e: &GhError, what: &str) -> String {
    let base = e.to_string();
    match e.status {
        Some(401) => format!("{base}. The token is invalid or expired; sign in again."),
        Some(403) => format!("{base}. The token lacks permission to {what}. For a fine-grained token check the repository permissions listed above."),
        Some(404) => format!("{base}. Not found, or the token cannot see it."),
        Some(422) => format!("{base}."),
        _ => base,
    }
}

pub async fn run(app: Arc<AppState>, gh: GitHub, input: Input) {
    let t_full = format!("{}/{}", input.template_owner, input.template_repo);

    // 1. Template
    set(&app, "template", "running", None);
    match gh.get_repo(&input.template_owner, &input.template_repo).await {
        Ok(Some(r)) if r.is_template => set(&app, "template", "ok", Some(format!("{} is a template", r.full_name))),
        Ok(Some(r)) => {
            return fail(&app, "template", format!(
                "{} exists but is not marked as a template repository (Settings > General > Template repository). Choose another template or ask its owner to enable it.",
                r.full_name
            ))
        }
        Ok(None) => {
            return fail(&app, "template", format!(
                "Template {t_full} was not found or this token cannot read it. For a fine-grained token, the template must be public or within the token's resource owner."
            ))
        }
        Err(e) => return fail(&app, "template", gh_hint(&e, "read the template")),
    }

    // 2. Create (or re-use)
    let repo = if let Some(r) = input.reuse.clone() {
        set(
            &app,
            "create",
            "skipped",
            Some(format!("{} already created in this session", r.full_name)),
        );
        r
    } else {
        set(&app, "create", "running", None);
        match gh
            .generate_from_template(
                &input.template_owner,
                &input.template_repo,
                &input.owner,
                &input.name,
                "Taro Enterprise deployment (created by TaroEnterpriseSetup)",
            )
            .await
        {
            Ok(r) => {
                {
                    let mut w = app.w();
                    w.github.repo = Some(r.clone());
                    w.github.created_by_wizard = true;
                    w.github.deleted = false;
                }
                if !r.private {
                    return fail(&app, "create", format!(
                        "GitHub created {} but reports it as PUBLIC. Stopping before any configuration is written. Make it private in Settings > General, or delete it below.",
                        r.full_name
                    ));
                }
                set(
                    &app,
                    "create",
                    "ok",
                    Some(format!("Created private repository {}", r.full_name)),
                );
                r
            }
            Err(e) if e.status == Some(422) => {
                return fail(
                    &app,
                    "create",
                    format!(
                    "{e}. A repository named {}/{} probably exists already; choose another name.",
                    input.owner, input.name
                ),
                )
            }
            Err(e) => {
                return fail(
                    &app,
                    "create",
                    gh_hint(
                        &e,
                        "create repositories for this owner (Administration: read and write)",
                    ),
                )
            }
        }
    };
    let (o, n) = match repo.full_name.split_once('/') {
        Some((o, n)) => (o.to_string(), n.to_string()),
        None => (input.owner.clone(), input.name.clone()),
    };

    // 3. Workflow permissions (warn only)
    set(&app, "workflow_perms", "running", None);
    match retry(&app, || gh.set_workflow_permissions_read(&o, &n)).await {
        Ok(()) => set(&app, "workflow_perms", "ok", Some("default_workflow_permissions = read".into())),
        Err(e) => set(
            &app,
            "workflow_perms",
            "warn",
            Some(format!(
                "Not changed ({e}). An organisation policy may control this, or the token lacks Administration write. Set it manually under Settings > Actions > General."
            )),
        ),
    }

    // 4. Secrets
    let needs_key = input.secrets.iter().any(|s| s.value.is_some());
    let key = if needs_key {
        match retry(&app, || gh.actions_public_key(&o, &n)).await {
            Ok(k) => Some(k),
            Err(e) => {
                let first = input
                    .secrets
                    .iter()
                    .find(|s| s.value.is_some())
                    .map(|s| format!("secret:{}", s.name))
                    .unwrap_or_default();
                return fail(
                    &app,
                    &first,
                    gh_hint(
                        &e,
                        "read the repository's Actions public key (Secrets: read and write)",
                    ),
                );
            }
        }
    } else {
        None
    };
    for s in &input.secrets {
        let id = format!("secret:{}", s.name);
        let Some(value) = &s.value else {
            set(&app, &id, "skipped", s.skip_reason.clone());
            continue;
        };
        let Some((key_id, pk)) = &key else { continue };
        set(&app, &id, "running", None);
        let sealed = match crate::sealed::seal_for_github(pk, value) {
            Ok(c) => c,
            Err(e) => return fail(&app, &id, e),
        };
        match gh.put_actions_secret(&o, &n, s.name, &sealed, key_id).await {
            Ok(()) => set(
                &app,
                &id,
                "ok",
                Some("encrypted with the repository key (sealed box) and stored".into()),
            ),
            Err(e) => {
                return fail(
                    &app,
                    &id,
                    gh_hint(&e, "write Actions secrets (Secrets: read and write)"),
                )
            }
        }
    }

    // 5. Files (non-secret configuration and branding)
    for f in &input.files {
        let id = format!("file:{}", f.path);
        set(&app, &id, "running", None);
        let sha = match retry(&app, || gh.content_sha(&o, &n, &f.path)).await {
            Ok(s) => s,
            Err(e) => return fail(&app, &id, gh_hint(&e, "read repository contents")),
        };
        let msg = if sha.is_some() {
            format!("Update {} (TaroEnterpriseSetup)", f.path)
        } else {
            format!("Add {} (TaroEnterpriseSetup)", f.path)
        };
        match retry(&app, || gh.put_content(&o, &n, &f.path, &f.bytes, &msg, sha.as_deref())).await {
            Ok(commit) => set(
                &app,
                &id,
                "ok",
                Some(if commit.is_empty() {
                    "committed".into()
                } else {
                    format!("commit {}", &commit[..commit.len().min(7)])
                }),
            ),
            Err(e) => return fail(&app, &id, gh_hint(&e, "write repository contents (Contents: read and write; Workflows: read and write if the path is under .github/workflows)")),
        }
    }

    let mut w = app.w();
    w.github.bootstrap_running = false;
    w.github.bootstrap_done = true;
    w.github.bootstrap_error = None;
    drop(w);
    app.log
        .info(format!("bootstrap finished for {}", repo.full_name));
}
