//! The GitHub REST calls of the release signing step (Step 5), with the operator's token.
//!
//! The token is a [`Secret`] and is attached only to requests to the configured API base
//! (`https://api.github.com` in the shipped wizard). The HTTP client never follows redirects
//! (see [`crate::config::http_client`]), so it cannot be forwarded anywhere else.

use crate::secret::Secret;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct GhError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "GitHub {s}: {}", self.message),
            None => write!(f, "GitHub: {}", self.message),
        }
    }
}

pub type GhResult<T> = Result<T, GhError>;

fn err(status: StatusCode, body: &Value) -> GhError {
    GhError {
        status: Some(status.as_u16()),
        message: body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| status.canonical_reason().unwrap_or("error"))
            .to_string(),
    }
}

fn repo_of(v: &Value, full: &str) -> Repo {
    Repo {
        full_name: v["full_name"].as_str().unwrap_or(full).to_string(),
        html_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
        private: v["private"].as_bool().unwrap_or(false),
        admin: v["permissions"]["admin"].as_bool().unwrap_or(false),
        parent: v["parent"]["full_name"].as_str().map(str::to_string),
    }
}

#[derive(Clone)]
pub struct GitHub {
    base: String,
    http: reqwest::Client,
    token: Secret,
}

#[derive(Debug, Clone, Serialize)]
pub struct Repo {
    pub full_name: String,
    pub html_url: String,
    pub default_branch: String,
    pub private: bool,
    /// the token may change the repository's settings and secrets
    pub admin: bool,
    /// the repository it was forked from (`owner/name`)
    pub parent: Option<String>,
}

/// What `GET /user` says about the token's account.
pub struct User {
    pub login: String,
    /// `X-OAuth-Scopes` of a classic token (`ghp_…`); None for fine-grained tokens
    pub scopes: Option<Vec<String>>,
}

/// A text file of the repository: its contents and the blob sha (needed to update it).
pub struct File {
    pub text: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub id: u64,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: String,
    pub created_at: String,
}

impl GitHub {
    pub fn new(base: &str, http: reqwest::Client, token: Secret) -> Self {
        GitHub {
            base: base.trim_end_matches('/').to_string(),
            http,
            token,
        }
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> GhResult<(StatusCode, Value)> {
        debug_assert!(path.starts_with('/'));
        let mut rb = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.map_err(|e| GhError {
            status: None,
            message: format!(
                "{} ({})",
                if e.is_timeout() {
                    "timed out"
                } else {
                    "could not reach GitHub"
                },
                e.without_url()
            ),
        })?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((status, body))
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> GhResult<Value> {
        let (s, b) = self.call(method, path, body).await?;
        if s.is_success() {
            Ok(b)
        } else {
            Err(err(s, &b))
        }
    }

    /// The signed-in account (proves the token works) and a classic token's scopes.
    pub async fn user(&self) -> GhResult<User> {
        let r = self
            .http
            .get(format!("{}/user", self.base))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| GhError {
                status: None,
                message: format!("could not reach GitHub ({})", e.without_url()),
            })?;
        let status = r.status();
        let scopes = r
            .headers()
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            });
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(err(status, &body));
        }
        Ok(User {
            login: body["login"].as_str().unwrap_or_default().to_string(),
            scopes,
        })
    }

    /// The repository, or None when it does not exist (or the token cannot see it).
    pub async fn find_repo(&self, full: &str) -> GhResult<Option<Repo>> {
        match self.repo(full).await {
            Ok(r) => Ok(Some(r)),
            Err(GhError {
                status: Some(404), ..
            }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fork `full` into the token's account (GitHub returns the existing fork if there is
    /// one). The copy is made in the background: wait for its files with [`Self::file`].
    pub async fn fork(&self, full: &str) -> GhResult<Repo> {
        let v = self
            .ok(
                Method::POST,
                &format!("/repos/{full}/forks"),
                Some(json!({ "default_branch_only": true })),
            )
            .await?;
        Ok(repo_of(&v, full))
    }

    /// Allow GitHub Actions to run, and the workflow (Actions start disabled in a fork).
    pub async fn enable_actions(&self, full: &str, workflow: &str) -> GhResult<()> {
        self.ok(
            Method::PUT,
            &format!("/repos/{full}/actions/permissions"),
            Some(json!({ "enabled": true, "allowed_actions": "all" })),
        )
        .await?;
        self.ok(
            Method::PUT,
            &format!("/repos/{full}/actions/workflows/{workflow}/enable"),
            None,
        )
        .await
        .map(|_| ())
    }

    /// Bring a fork's branch up to date with its parent. Returns GitHub's message.
    pub async fn merge_upstream(&self, full: &str, branch: &str) -> GhResult<String> {
        let v = self
            .ok(
                Method::POST,
                &format!("/repos/{full}/merge-upstream"),
                Some(json!({ "branch": branch })),
            )
            .await?;
        Ok(v["message"].as_str().unwrap_or("up to date").to_string())
    }

    pub async fn repo(&self, full: &str) -> GhResult<Repo> {
        let v = self
            .ok(Method::GET, &format!("/repos/{full}"), None)
            .await?;
        Ok(repo_of(&v, full))
    }

    /// Whether an Actions secret of this name exists (its value can never be read).
    pub async fn has_secret(&self, full: &str, name: &str) -> GhResult<bool> {
        Ok(self.secret_updated(full, name).await?.is_some())
    }

    /// When the secret was last set (`Some("")` when GitHub does not say); `None`: no secret.
    pub async fn secret_updated(&self, full: &str, name: &str) -> GhResult<Option<String>> {
        let (s, b) = self
            .call(
                Method::GET,
                &format!("/repos/{full}/actions/secrets/{name}"),
                None,
            )
            .await?;
        match s {
            s if s.is_success() => Ok(Some(
                b["updated_at"].as_str().unwrap_or_default().to_string(),
            )),
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(err(s, &b)),
        }
    }

    /// Let workflows' GITHUB_TOKEN write (publish releases) by default in the repository.
    pub async fn set_workflow_write(&self, full: &str) -> GhResult<()> {
        self.ok(
            Method::PUT,
            &format!("/repos/{full}/actions/permissions/workflow"),
            Some(json!({ "default_workflow_permissions": "write" })),
        )
        .await
        .map(|_| ())
    }

    /// `(key_id, base64 X25519 public key)` that Actions secrets are sealed for.
    pub async fn secrets_key(&self, full: &str) -> GhResult<(String, String)> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{full}/actions/secrets/public-key"),
                None,
            )
            .await?;
        match (v["key_id"].as_str(), v["key"].as_str()) {
            (Some(id), Some(k)) => Ok((id.to_string(), k.to_string())),
            _ => Err(GhError {
                status: None,
                message: "no secrets public key in the answer".into(),
            }),
        }
    }

    /// Create or replace an Actions secret with a value already sealed for the repository.
    pub async fn put_secret(
        &self,
        full: &str,
        name: &str,
        sealed: &str,
        key_id: &str,
    ) -> GhResult<()> {
        self.ok(
            Method::PUT,
            &format!("/repos/{full}/actions/secrets/{name}"),
            Some(json!({ "encrypted_value": sealed, "key_id": key_id })),
        )
        .await
        .map(|_| ())
    }

    /// A text file on a branch (None when it does not exist).
    pub async fn file(&self, full: &str, path: &str, branch: &str) -> GhResult<Option<File>> {
        let (s, b) = self
            .call(
                Method::GET,
                &format!("/repos/{full}/contents/{path}?ref={branch}"),
                None,
            )
            .await?;
        if s == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !s.is_success() {
            return Err(err(s, &b));
        }
        let raw = b["content"]
            .as_str()
            .unwrap_or_default()
            .replace(['\n', '\r'], "");
        let bytes = crate::util::b64_decode(&raw).ok_or_else(|| GhError {
            status: None,
            message: format!("{path}: content is not base64"),
        })?;
        Ok(Some(File {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            sha: b["sha"].as_str().unwrap_or_default().to_string(),
        }))
    }

    /// Commit a text file on a branch (created, or replaced when `sha` is the current blob).
    pub async fn put_file(
        &self,
        full: &str,
        path: &str,
        branch: &str,
        text: &str,
        sha: Option<&str>,
        message: &str,
    ) -> GhResult<String> {
        let mut body = json!({
            "message": message,
            "content": crate::util::b64(text.as_bytes()),
            "branch": branch,
        });
        if let Some(s) = sha {
            body["sha"] = json!(s);
        }
        let v = self
            .ok(
                Method::PUT,
                &format!("/repos/{full}/contents/{path}"),
                Some(body),
            )
            .await?;
        Ok(v["commit"]["sha"].as_str().unwrap_or_default().to_string())
    }

    /// Tags of the repository's releases (drafts included: a tag is taken either way).
    pub async fn release_tags(&self, full: &str) -> GhResult<Vec<String>> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{full}/releases?per_page=100"),
                None,
            )
            .await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| r["tag_name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn dispatch(
        &self,
        full: &str,
        workflow: &str,
        branch: &str,
        inputs: Value,
    ) -> GhResult<()> {
        self.ok(
            Method::POST,
            &format!("/repos/{full}/actions/workflows/{workflow}/dispatches"),
            Some(json!({ "ref": branch, "inputs": inputs })),
        )
        .await
        .map(|_| ())
    }

    /// The newest manually started runs of a workflow.
    pub async fn runs(&self, full: &str, workflow: &str) -> GhResult<Vec<Run>> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{full}/actions/workflows/{workflow}/runs?event=workflow_dispatch&per_page=10"),
                None,
            )
            .await?;
        Ok(v["workflow_runs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| Run {
                        id: r["id"].as_u64().unwrap_or_default(),
                        status: r["status"].as_str().unwrap_or_default().to_string(),
                        conclusion: r["conclusion"].as_str().map(str::to_string),
                        html_url: r["html_url"].as_str().unwrap_or_default().to_string(),
                        created_at: r["created_at"].as_str().unwrap_or_default().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
}
