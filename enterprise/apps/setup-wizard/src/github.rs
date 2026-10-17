//! GitHub REST client and OAuth device flow.
//!
//! The token lives in a [`Secret`] and is only attached to requests whose URL starts with
//! the configured API base (`https://api.github.com` in production). The client never follows
//! redirects with the token attached; the one place GitHub redirects (job log downloads) is
//! followed manually with an unauthenticated client.

use crate::secret::Secret;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct GhError {
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for GhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(s) => write!(f, "GitHub API {s}: {}", self.message),
            None => write!(f, "GitHub API: {}", self.message),
        }
    }
}

impl GhError {
    fn net(e: reqwest::Error) -> Self {
        let kind = if e.is_timeout() {
            "timed out"
        } else if e.is_connect() {
            "could not connect"
        } else {
            "request failed"
        };
        GhError {
            status: None,
            message: format!("{kind} ({})", e.without_url()),
        }
    }
}

pub type GhResult<T> = Result<T, GhError>;

#[derive(Clone)]
pub struct GitHub {
    base: String,
    http: reqwest::Client,
    plain: reqwest::Client,
    token: Secret,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserInfo {
    pub login: String,
    pub id: u64,
    pub kind: String,
    /// `X-OAuth-Scopes` (classic PATs / OAuth tokens only; fine-grained PATs don't send it).
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub full_name: String,
    pub name: String,
    pub private: bool,
    #[serde(default)]
    pub is_template: bool,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub event: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStep {
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub steps: Vec<JobStep>,
    #[serde(default)]
    pub html_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgInfo {
    pub login: String,
    pub role: String,
    pub can_create_repos: Option<bool>,
}

struct Resp {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

fn err_from(status: StatusCode, body: &Value) -> GhError {
    let mut message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error"))
        .to_string();
    if let Some(errs) = body.get("errors").and_then(Value::as_array) {
        let extra: Vec<String> = errs
            .iter()
            .filter_map(|e| {
                e.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| e.as_str().map(str::to_string))
            })
            .collect();
        if !extra.is_empty() {
            message = format!("{message} ({})", extra.join("; "));
        }
    }
    GhError {
        status: Some(status.as_u16()),
        message,
    }
}

impl GitHub {
    pub fn new(base: &str, http: reqwest::Client, plain: reqwest::Client, token: Secret) -> Self {
        GitHub {
            base: base.trim_end_matches('/').to_string(),
            http,
            plain,
            token,
        }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> GhResult<Resp> {
        debug_assert!(path.starts_with('/'));
        let url = format!("{}{}", self.base, path);
        let mut rb = self
            .http
            .request(method, &url)
            .bearer_auth(self.token.expose())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.map_err(GhError::net)?;
        let status = r.status();
        let headers = r.headers().clone();
        let text = r.text().await.map_err(GhError::net)?;
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok(Resp {
            status,
            headers,
            body,
        })
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> GhResult<Value> {
        let r = self.call(method, path, body).await?;
        if r.status.is_success() {
            Ok(r.body)
        } else {
            Err(err_from(r.status, &r.body))
        }
    }

    fn parse<T: for<'de> Deserialize<'de>>(v: Value, what: &str) -> GhResult<T> {
        serde_json::from_value(v).map_err(|e| GhError {
            status: None,
            message: format!("unexpected {what} response: {e}"),
        })
    }

    /// `GET /user`: proves the token works (the prefix alone proves nothing).
    pub async fn user(&self) -> GhResult<UserInfo> {
        let r = self.call(Method::GET, "/user", None).await?;
        if !r.status.is_success() {
            return Err(err_from(r.status, &r.body));
        }
        let scopes = r
            .headers
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            });
        Ok(UserInfo {
            login: r.body["login"].as_str().unwrap_or_default().to_string(),
            id: r.body["id"].as_u64().unwrap_or_default(),
            kind: r.body["type"].as_str().unwrap_or("User").to_string(),
            scopes,
        })
    }

    /// Organisations the user belongs to, with whether they can create private repos there.
    pub async fn orgs(&self) -> GhResult<Vec<OrgInfo>> {
        let list = self
            .ok(Method::GET, "/user/orgs?per_page=100", None)
            .await?;
        let mut out = Vec::new();
        for o in list.as_array().cloned().unwrap_or_default() {
            let Some(login) = o["login"].as_str().map(str::to_string) else {
                continue;
            };
            let m = self
                .call(
                    Method::GET,
                    &format!("/user/memberships/orgs/{login}"),
                    None,
                )
                .await?;
            let (role, active) = if m.status.is_success() {
                (
                    m.body["role"].as_str().unwrap_or("unknown").to_string(),
                    m.body["state"].as_str() == Some("active"),
                )
            } else {
                ("unknown".to_string(), false)
            };
            let can = if !active {
                Some(false)
            } else if role == "admin" {
                Some(true)
            } else {
                // Members may create repos only if the org allows it.
                let org = self
                    .call(Method::GET, &format!("/orgs/{login}"), None)
                    .await?;
                if org.status.is_success() {
                    org.body["members_can_create_private_repositories"].as_bool()
                } else {
                    None
                }
            };
            out.push(OrgInfo {
                login,
                role,
                can_create_repos: can,
            });
        }
        Ok(out)
    }

    pub async fn get_repo(&self, owner: &str, repo: &str) -> GhResult<Option<RepoInfo>> {
        let r = self
            .call(Method::GET, &format!("/repos/{owner}/{repo}"), None)
            .await?;
        if r.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status.is_success() {
            return Err(err_from(r.status, &r.body));
        }
        Self::parse(r.body, "repository").map(Some)
    }

    /// `POST /repos/{template_owner}/{template_repo}/generate` with `private: true`.
    pub async fn generate_from_template(
        &self,
        template_owner: &str,
        template_repo: &str,
        owner: &str,
        name: &str,
        description: &str,
    ) -> GhResult<RepoInfo> {
        let v = self
            .ok(
                Method::POST,
                &format!("/repos/{template_owner}/{template_repo}/generate"),
                Some(json!({
                    "owner": owner,
                    "name": name,
                    "description": description,
                    "include_all_branches": false,
                    "private": true,
                })),
            )
            .await?;
        Self::parse(v, "repository")
    }

    /// `(key_id, base64 key)` for Actions secrets.
    pub async fn actions_public_key(&self, owner: &str, repo: &str) -> GhResult<(String, String)> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{owner}/{repo}/actions/secrets/public-key"),
                None,
            )
            .await?;
        match (v["key_id"].as_str(), v["key"].as_str()) {
            (Some(id), Some(k)) => Ok((id.to_string(), k.to_string())),
            _ => Err(GhError {
                status: None,
                message: "public-key response is missing key_id/key".into(),
            }),
        }
    }

    pub async fn put_actions_secret(
        &self,
        owner: &str,
        repo: &str,
        name: &str,
        encrypted_b64: &str,
        key_id: &str,
    ) -> GhResult<()> {
        self.ok(
            Method::PUT,
            &format!("/repos/{owner}/{repo}/actions/secrets/{name}"),
            Some(json!({ "encrypted_value": encrypted_b64, "key_id": key_id })),
        )
        .await
        .map(|_| ())
    }

    /// SHA of an existing file, `None` if it does not exist.
    pub async fn content_sha(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
    ) -> GhResult<Option<String>> {
        let r = self
            .call(
                Method::GET,
                &format!("/repos/{owner}/{repo}/contents/{path}"),
                None,
            )
            .await?;
        if r.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status.is_success() {
            return Err(err_from(r.status, &r.body));
        }
        Ok(r.body["sha"].as_str().map(str::to_string))
    }

    /// Create or update a file via `PUT /repos/{o}/{r}/contents/{path}`. Returns the commit SHA.
    pub async fn put_content(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        bytes: &[u8],
        message: &str,
        sha: Option<&str>,
    ) -> GhResult<String> {
        let mut body = json!({ "message": message, "content": crate::util::b64(bytes) });
        if let Some(s) = sha {
            body["sha"] = json!(s);
        }
        let v = self
            .ok(
                Method::PUT,
                &format!("/repos/{owner}/{repo}/contents/{path}"),
                Some(body),
            )
            .await?;
        Ok(v["commit"]["sha"].as_str().unwrap_or_default().to_string())
    }

    pub async fn set_workflow_permissions_read(&self, owner: &str, repo: &str) -> GhResult<()> {
        self.ok(
            Method::PUT,
            &format!("/repos/{owner}/{repo}/actions/permissions/workflow"),
            Some(json!({
                "default_workflow_permissions": "read",
                "can_approve_pull_request_reviews": false
            })),
        )
        .await
        .map(|_| ())
    }

    pub async fn dispatch_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow: &str,
        git_ref: &str,
        version: &str,
    ) -> GhResult<()> {
        self.ok(
            Method::POST,
            &format!("/repos/{owner}/{repo}/actions/workflows/{workflow}/dispatches"),
            Some(json!({ "ref": git_ref, "inputs": { "version": version } })),
        )
        .await
        .map(|_| ())
    }

    pub async fn list_workflow_runs(
        &self,
        owner: &str,
        repo: &str,
        workflow: &str,
    ) -> GhResult<Vec<WorkflowRun>> {
        let v = self
            .ok(
                Method::GET,
                &format!(
                    "/repos/{owner}/{repo}/actions/workflows/{workflow}/runs?event=workflow_dispatch&per_page=20"
                ),
                None,
            )
            .await?;
        Self::parse(v["workflow_runs"].clone(), "workflow runs")
    }

    pub async fn get_run(&self, owner: &str, repo: &str, id: u64) -> GhResult<WorkflowRun> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{owner}/{repo}/actions/runs/{id}"),
                None,
            )
            .await?;
        Self::parse(v, "workflow run")
    }

    pub async fn run_jobs(&self, owner: &str, repo: &str, id: u64) -> GhResult<Vec<Job>> {
        let v = self
            .ok(
                Method::GET,
                &format!("/repos/{owner}/{repo}/actions/runs/{id}/jobs?per_page=50"),
                None,
            )
            .await?;
        Self::parse(v["jobs"].clone(), "jobs")
    }

    /// Download a job's log. GitHub answers with a redirect to a short-lived pre-signed URL;
    /// that URL is fetched **without** the token.
    pub async fn job_log(&self, owner: &str, repo: &str, job_id: u64) -> GhResult<Option<String>> {
        let r = self
            .call(
                Method::GET,
                &format!("/repos/{owner}/{repo}/actions/jobs/{job_id}/logs"),
                None,
            )
            .await?;
        if r.status.is_redirection() {
            let Some(loc) = r.headers.get("location").and_then(|l| l.to_str().ok()) else {
                return Ok(None);
            };
            let resp = self.plain.get(loc).send().await.map_err(GhError::net)?;
            if !resp.status().is_success() {
                return Ok(None);
            }
            return resp.text().await.map(Some).map_err(GhError::net);
        }
        if r.status.is_success() {
            return Ok(r.body.as_str().map(str::to_string));
        }
        if r.status == StatusCode::NOT_FOUND || r.status == StatusCode::GONE {
            return Ok(None);
        }
        Err(err_from(r.status, &r.body))
    }

    pub async fn release_by_tag(
        &self,
        owner: &str,
        repo: &str,
        tag: &str,
    ) -> GhResult<Option<Release>> {
        let r = self
            .call(
                Method::GET,
                &format!("/repos/{owner}/{repo}/releases/tags/{tag}"),
                None,
            )
            .await?;
        if r.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status.is_success() {
            return Err(err_from(r.status, &r.body));
        }
        Self::parse(r.body, "release").map(Some)
    }

    pub async fn delete_repo(&self, owner: &str, repo: &str) -> GhResult<()> {
        self.ok(Method::DELETE, &format!("/repos/{owner}/{repo}"), None)
            .await
            .map(|_| ())
    }
}

// ---------------------------------------------------------------------------------------------
// OAuth device flow (RFC 8628 as implemented by GitHub)
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub struct DeviceCode {
    pub device_code: Secret,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug)]
pub enum PollResult {
    Pending,
    SlowDown(Option<u64>),
    Token(Secret),
    Expired,
    Denied,
    Failed(String),
}

#[derive(Debug)]
pub struct DeviceOutcome {
    pub token: Secret,
    pub polls: u32,
    pub final_interval: u64,
}

#[derive(Clone)]
pub struct DeviceFlow {
    web_base: String,
    http: reqwest::Client,
    client_id: String,
}

/// Scopes requested for OAuth apps (ignored by GitHub Apps, whose permissions are fixed).
pub const DEVICE_SCOPES: &str = "repo workflow";

impl DeviceFlow {
    pub fn new(web_base: &str, http: reqwest::Client, client_id: &str) -> Self {
        DeviceFlow {
            web_base: web_base.trim_end_matches('/').to_string(),
            http,
            client_id: client_id.to_string(),
        }
    }

    pub async fn start(&self) -> Result<DeviceCode, String> {
        let r = self
            .http
            .post(format!("{}/login/device/code", self.web_base))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", DEVICE_SCOPES),
            ])
            .send()
            .await
            .map_err(|e| format!("Could not reach github.com: {}", e.without_url()))?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if let Some(err) = v["error"].as_str() {
            let desc = v["error_description"].as_str().unwrap_or("");
            return Err(match err {
                "device_flow_disabled" => "Device flow is not enabled for this OAuth/GitHub App (enable it in the app settings).".into(),
                "incorrect_client_credentials" => "GitHub does not recognise this client id.".into(),
                _ => format!("GitHub refused the device flow: {err} {desc}"),
            });
        }
        if !status.is_success() {
            return Err(format!(
                "GitHub returned HTTP {status} for the device code request."
            ));
        }
        match (
            v["device_code"].as_str(),
            v["user_code"].as_str(),
            v["verification_uri"].as_str(),
        ) {
            (Some(dc), Some(uc), Some(uri)) => Ok(DeviceCode {
                device_code: Secret::new(dc),
                user_code: uc.to_string(),
                verification_uri: uri.to_string(),
                expires_in: v["expires_in"].as_u64().unwrap_or(900),
                interval: v["interval"].as_u64().unwrap_or(5).max(1),
            }),
            _ => Err("GitHub's device code response was incomplete.".into()),
        }
    }

    pub async fn poll_once(&self, device_code: &Secret) -> PollResult {
        let r = match self
            .http
            .post(format!("{}/login/oauth/access_token", self.web_base))
            .header("Accept", "application/json")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("device_code", device_code.expose()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return PollResult::Failed(format!("network error: {}", e.without_url())),
        };
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if let Some(t) = v["access_token"].as_str() {
            return PollResult::Token(Secret::new(t));
        }
        match v["error"].as_str() {
            Some("authorization_pending") => PollResult::Pending,
            Some("slow_down") => PollResult::SlowDown(v["interval"].as_u64()),
            Some("expired_token") => PollResult::Expired,
            Some("access_denied") => PollResult::Denied,
            Some(other) => PollResult::Failed(format!(
                "{other}: {}",
                v["error_description"].as_str().unwrap_or("")
            )),
            None => PollResult::Failed("unexpected response from GitHub".into()),
        }
    }

    /// Poll until the user authorises (or the code expires / is denied).
    ///
    /// Waits `interval` units between polls; on `slow_down` the interval grows by at least 5 (or to
    /// the larger value GitHub sends back), as RFC 8628 requires. Transient network errors are retried.
    pub async fn poll_until(
        &self,
        code: &DeviceCode,
        unit: Duration,
        mut on_wait: impl FnMut(u64),
    ) -> Result<DeviceOutcome, String> {
        let mut interval = code.interval.max(1);
        let mut elapsed = 0u64;
        let mut polls = 0u32;
        let mut net_failures = 0u32;
        loop {
            on_wait(interval);
            tokio::time::sleep(unit * interval as u32).await;
            elapsed += interval;
            polls += 1;
            match self.poll_once(&code.device_code).await {
                PollResult::Token(token) => {
                    return Ok(DeviceOutcome {
                        token,
                        polls,
                        final_interval: interval,
                    })
                }
                PollResult::Pending => {}
                PollResult::SlowDown(new) => {
                    interval = new.unwrap_or(0).max(interval + 5);
                }
                PollResult::Expired => {
                    return Err("The code expired before it was entered. Start again.".into())
                }
                PollResult::Denied => return Err("Authorisation was denied on GitHub.".into()),
                PollResult::Failed(m) if m.starts_with("network error") && net_failures < 3 => {
                    net_failures += 1;
                }
                PollResult::Failed(m) => return Err(format!("Device flow failed: {m}")),
            }
            if elapsed >= code.expires_in {
                return Err("The code expired before it was entered. Start again.".into());
            }
        }
    }
}
