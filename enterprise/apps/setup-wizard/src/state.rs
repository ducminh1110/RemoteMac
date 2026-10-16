//! Wizard state. Everything the UI sees comes from [`Wizard::view`]; secret-bearing fields are
//! `Secret`, which has no `Serialize` implementation, so they cannot end up in a response by
//! accident (each one is `#[serde(skip)]`, and the compiler rejects a derive that forgets it).

use crate::branding::ImageInfo;
use crate::config::{Endpoints, Http, Timings};
use crate::domain::{DnsRecord, DnsResolver, DomainVerification, Strategy, Target};
use crate::github::{Job, OrgInfo, Release, RepoInfo, UserInfo, WorkflowRun};
use crate::preflight::PreflightReport;
use crate::redact::DiagLog;
use crate::secret::Secret;
use crate::stripe::{AccountInfo, WebhookEndpoint};
use crate::validate::{Environment, StripeMode};
use crate::verify::VerificationReport;
use serde::Serialize;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::Notify;

pub const MODULES: [&str; 4] = ["my_macs", "rentals", "billing", "team"];
pub const PROFILE_SINGLE: &str = "single_server_compose";
pub const PROVIDER_MOCK: &str = "mock";

#[derive(Clone, Debug, Serialize)]
pub struct Deployment {
    pub environment: Environment,
    pub profile: String,
    pub portal_url: String,
    pub ops_url: String,
    pub provider_adapter: String,
    pub admin_email: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    pub deployment_name: String,
    pub operator_name: String,
    pub support_contact: String,
    pub modules: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Upload {
    pub info: ImageInfo,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Branding {
    pub saved: bool,
    pub primary_color: String,
    pub contrast_vs_white: Option<f64>,
    pub contrast_warning: Option<String>,
    pub theme: String,
    pub density: String,
    pub logo: Option<Upload>,
    pub favicon: Option<Upload>,
}

impl Default for Branding {
    fn default() -> Self {
        Branding {
            saved: false,
            primary_color: "#0A66C2".into(),
            contrast_vs_white: None,
            contrast_warning: None,
            theme: "light".into(),
            density: "comfortable".into(),
            logo: None,
            favicon: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct DomainState {
    pub strategy: Option<Strategy>,
    pub domain: Option<String>,
    pub target: Option<String>,
    #[serde(skip)]
    pub target_parsed: Option<Target>,
    /// Random value for the `_taro-verify` TXT record (public by design: it goes into DNS).
    pub verify_token: Option<String>,
    pub records: Vec<DnsRecord>,
    pub verification: Option<DomainVerification>,
    pub verifying: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SmokeResult {
    pub ok: bool,
    pub session_id: Option<String>,
    pub session_url: Option<String>,
    pub expired: bool,
    pub detail: String,
    pub at: u64,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Billing {
    #[serde(skip)]
    pub key: Option<Secret>,
    pub key_present: bool,
    pub mode: Option<StripeMode>,
    pub account: Option<AccountInfo>,
    pub key_error: Option<String>,
    #[serde(skip)]
    pub webhook_secret: Option<Secret>,
    pub webhook_secret_present: bool,
    pub webhook_secret_source: Option<String>,
    pub webhook: Option<WebhookEndpoint>,
    pub webhook_missing_events: Vec<String>,
    pub webhook_note: Option<String>,
    pub smoke: Option<SmokeResult>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct DeviceView {
    pub phase: String,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub interval: Option<u64>,
    pub expires_at: Option<u64>,
    pub error: Option<String>,
    #[serde(skip)]
    pub generation: u64,
    #[serde(skip)]
    pub cancel: Option<Arc<Notify>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionResult {
    pub id: String,
    pub label: String,
    /// pending | running | ok | warn | fail | skipped
    pub status: String,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct GithubState {
    #[serde(skip)]
    pub token: Option<Secret>,
    pub user: Option<UserInfo>,
    pub auth_method: Option<String>,
    pub auth_error: Option<String>,
    pub device: DeviceView,
    pub remembered_available: bool,
    pub remember_supported: bool,
    pub orgs: Option<Vec<OrgInfo>>,
    pub owner: Option<String>,
    pub repo_name: Option<String>,
    pub template: String,
    pub plan: Option<Vec<ActionResult>>,
    pub bootstrap_running: bool,
    pub bootstrap_done: bool,
    pub bootstrap_error: Option<String>,
    pub repo: Option<RepoInfo>,
    pub created_by_wizard: bool,
    pub deleted: bool,
    #[serde(skip)]
    pub taro_secret_key: Option<Secret>,
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct ReleaseState {
    pub running: bool,
    pub version: Option<String>,
    pub phase: String,
    pub dispatched_at: Option<String>,
    pub run: Option<WorkflowRun>,
    pub jobs: Vec<Job>,
    pub log_tail: Option<String>,
    /// Only set once `GET /releases/tags/{version}` returned the release with >= 1 asset.
    pub release: Option<Release>,
    pub error: Option<String>,
    pub attempts: u32,
}

#[derive(Default)]
pub struct Wizard {
    pub preflight: Option<PreflightReport>,
    pub preflight_running: bool,
    pub deployment: Option<Deployment>,
    pub identity: Option<Identity>,
    pub branding: Branding,
    pub domain: DomainState,
    pub billing: Billing,
    pub github: GithubState,
    pub release: ReleaseState,
    pub verification: Option<VerificationReport>,
    pub verification_running: bool,
}

pub struct AppState {
    pub access_token: String,
    pub addr: SocketAddr,
    pub endpoints: Endpoints,
    pub timings: Timings,
    pub http: Http,
    pub log: DiagLog,
    pub dns: Arc<dyn DnsResolver>,
    pub remember_dir: Option<PathBuf>,
    pub github_client_id: Option<String>,
    pub managed_zone: Option<String>,
    pub shutdown: Arc<Notify>,
    wizard: Mutex<Wizard>,
}

pub struct Options {
    pub endpoints: Endpoints,
    pub timings: Timings,
    pub dns: Arc<dyn DnsResolver>,
    pub remember_dir: Option<PathBuf>,
    pub github_client_id: Option<String>,
    pub managed_zone: Option<String>,
    pub template: String,
}

impl AppState {
    pub fn new(addr: SocketAddr, access_token: String, o: Options) -> Arc<Self> {
        let log = DiagLog::new();
        log.register_secret(&Secret::new(access_token.clone()));
        let mut w = Wizard::default();
        w.github.template = o.template;
        w.github.remember_supported = crate::remember::supported() && o.remember_dir.is_some();
        w.github.remembered_available = o
            .remember_dir
            .as_deref()
            .is_some_and(crate::remember::stored_exists);
        w.github.device.phase = "idle".into();
        w.release.phase = "idle".into();
        Arc::new(AppState {
            access_token,
            addr,
            http: Http::new(o.timings.http_timeout),
            endpoints: o.endpoints,
            timings: o.timings,
            log,
            dns: o.dns,
            remember_dir: o.remember_dir,
            github_client_id: o.github_client_id.filter(|c| !c.trim().is_empty()),
            managed_zone: o.managed_zone.filter(|z| !z.trim().is_empty()),
            shutdown: Arc::new(Notify::new()),
            wizard: Mutex::new(w),
        })
    }

    /// Lock the wizard state. Never hold the guard across an `.await` (the guard is `!Send`,
    /// so spawned tasks fail to compile if one tries).
    pub fn w(&self) -> MutexGuard<'_, Wizard> {
        self.wizard.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn github_client(&self) -> Option<crate::github::GitHub> {
        let token = self.w().github.token.clone()?;
        Some(crate::github::GitHub::new(
            &self.endpoints.github_api,
            self.http.api.clone(),
            self.http.plain.clone(),
            token,
        ))
    }

    pub fn stripe_client(&self) -> Option<crate::stripe::Stripe> {
        let key = self.w().billing.key.clone()?;
        Some(crate::stripe::Stripe::new(
            &self.endpoints.stripe_api,
            self.http.api.clone(),
            key,
        ))
    }
}

fn step(id: &str, title: &str, status: &str, summary: impl Into<String>) -> Value {
    json!({ "id": id, "title": title, "status": status, "summary": summary.into() })
}

impl Wizard {
    pub fn billing_enabled(&self) -> bool {
        self.identity
            .as_ref()
            .is_none_or(|i| i.modules.iter().any(|m| m == "billing"))
    }

    /// Step list with honest statuses: ok | warn | fail | todo | running | skipped | needs_config.
    pub fn steps(&self) -> Vec<Value> {
        let mut v = Vec::new();
        v.push(match (&self.preflight, self.preflight_running) {
            (_, true) => step("preflight", "Preflight", "running", "Running checks"),
            (None, _) => step("preflight", "Preflight", "todo", "Not run yet"),
            (Some(p), _) => {
                let fails = p.checks.iter().filter(|c| c.status == "fail").count();
                let warns = p.checks.iter().filter(|c| c.status == "warn").count();
                if fails > 0 {
                    step(
                        "preflight",
                        "Preflight",
                        "fail",
                        format!("{fails} check(s) failed"),
                    )
                } else if warns > 0 {
                    step(
                        "preflight",
                        "Preflight",
                        "warn",
                        format!("{warns} warning(s)"),
                    )
                } else {
                    step("preflight", "Preflight", "ok", "All checks passed")
                }
            }
        });
        v.push(match &self.deployment {
            None => step("deployment", "Deployment mode", "todo", "Not saved"),
            Some(d) => step(
                "deployment",
                "Deployment mode",
                "ok",
                format!("{} - single server", d.environment.as_str()),
            ),
        });
        v.push(match &self.identity {
            None => step("identity", "Product identity", "todo", "Not saved"),
            Some(i) => step(
                "identity",
                "Product identity",
                "ok",
                i.deployment_name.clone(),
            ),
        });
        v.push(if self.branding.saved {
            let st = if self.branding.contrast_warning.is_some() {
                "warn"
            } else {
                "ok"
            };
            step(
                "branding",
                "Branding",
                st,
                self.branding.primary_color.clone(),
            )
        } else {
            step("branding", "Branding", "todo", "Not saved")
        });
        v.push(match (&self.domain.domain, &self.domain.verification) {
            (None, _) => step("domain", "Domain & TLS", "todo", "No domain chosen"),
            (Some(_), _) if self.domain.verifying => {
                step("domain", "Domain & TLS", "running", "Verifying")
            }
            (Some(d), None) => step(
                "domain",
                "Domain & TLS",
                "todo",
                format!("{d}: not verified"),
            ),
            (Some(d), Some(r)) if r.dns_ok && r.https_ok => {
                step("domain", "Domain & TLS", "ok", format!("{d} verified"))
            }
            (Some(d), Some(r)) => step(
                "domain",
                "Domain & TLS",
                "fail",
                format!(
                    "{d}: {}",
                    if !r.dns_ok {
                        "DNS not verified"
                    } else {
                        "HTTPS check failed"
                    }
                ),
            ),
        });
        v.push(if !self.billing_enabled() {
            step(
                "billing",
                "Billing",
                "skipped",
                "Billing module not enabled",
            )
        } else if let Some(e) = &self.billing.key_error {
            step("billing", "Billing", "fail", e.clone())
        } else if self.billing.account.is_none() {
            step("billing", "Billing", "todo", "Stripe key not validated")
        } else if !self.billing.webhook_secret_present {
            step(
                "billing",
                "Billing",
                "warn",
                "Key OK; webhook secret missing",
            )
        } else {
            let mode = match self.billing.mode {
                Some(StripeMode::Live) => "LIVE mode",
                _ => "test mode",
            };
            step("billing", "Billing", "ok", format!("Stripe {mode}"))
        });
        let g = &self.github;
        v.push(if g.bootstrap_running {
            step(
                "github",
                "GitHub bootstrap",
                "running",
                "Creating repository",
            )
        } else if g.bootstrap_done {
            step(
                "github",
                "GitHub bootstrap",
                "ok",
                g.repo
                    .as_ref()
                    .map(|r| r.full_name.clone())
                    .unwrap_or_default(),
            )
        } else if let Some(e) = &g.bootstrap_error {
            step("github", "GitHub bootstrap", "fail", e.clone())
        } else if let Some(u) = &g.user {
            step(
                "github",
                "GitHub bootstrap",
                "todo",
                format!("Signed in as {}", u.login),
            )
        } else {
            step("github", "GitHub bootstrap", "todo", "Not signed in")
        });
        let r = &self.release;
        v.push(if r.running {
            step("release", "Build & release", "running", r.phase.clone())
        } else if r.release.is_some() {
            step(
                "release",
                "Build & release",
                "ok",
                format!("Release {}", r.version.clone().unwrap_or_default()),
            )
        } else if let Some(e) = &r.error {
            step("release", "Build & release", "fail", e.clone())
        } else {
            step("release", "Build & release", "todo", "Not started")
        });
        v.push(match (&self.verification, self.verification_running) {
            (_, true) => step("verify", "Verification", "running", "Checking"),
            (None, _) => step("verify", "Verification", "todo", "Not run yet"),
            (Some(rep), _) => {
                let fails = rep.checks.iter().filter(|c| c.status == "fail").count();
                let skipped = rep.checks.iter().filter(|c| c.status == "skipped").count();
                if fails > 0 {
                    step(
                        "verify",
                        "Verification",
                        "fail",
                        format!("{fails} check(s) failed"),
                    )
                } else if skipped > 0 {
                    step(
                        "verify",
                        "Verification",
                        "warn",
                        format!("{skipped} check(s) skipped"),
                    )
                } else {
                    step("verify", "Verification", "ok", "All checks passed")
                }
            }
        });
        v
    }

    /// Everything the UI may see. Contains no secrets.
    pub fn view(&self, app: &AppState) -> Value {
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "steps": self.steps(),
            "preflight": self.preflight,
            "deployment": self.deployment,
            "identity": self.identity,
            "branding": self.branding,
            "domain": self.domain,
            "billing": self.billing,
            "billing_enabled": self.billing_enabled(),
            "github": self.github,
            "release": self.release,
            "verification": self.verification,
            "config": {
                "device_flow_available": app.github_client_id.is_some(),
                "managed_zone": app.managed_zone,
                "modules": MODULES,
                "webhook_events": crate::stripe::WEBHOOK_EVENTS,
            }
        })
    }
}
