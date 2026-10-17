//! Wizard state. Everything the UI sees comes from [`Wizard::view`]; secret-bearing fields are
//! `Secret`, which has no `Serialize` implementation, so they cannot end up in a response by
//! accident (each one is `#[serde(skip)]`, and the compiler rejects a derive that forgets it).

use crate::branding::ImageInfo;
use crate::check::Report;
use crate::config::{Endpoints, Timings};
use crate::package::Built;
use crate::preflight::PreflightReport;
use crate::redact::DiagLog;
use crate::releases::{ReleaseInfo, Selected};
use crate::secret::Secret;
use rm_brand::{
    Branding, ClientUpdates, FieldError, Generated, Product, Provider, Relay, Theme, Tokens,
    Updates, SCHEMA,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use taro_update::manifest::{Component, COMPONENT_IDS};
use taro_update::sig::TrustedKeys;
use tokio::sync::Notify;

/// The relay port Taro uses unless the provider picks another.
pub const DEFAULT_PORT: u16 = 7470;
/// Length of a generated relay admission key.
pub const KEY_LEN: usize = 32;

#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    pub provider_id: String,
    pub name: String,
    pub short_name: String,
    pub vendor: String,
    pub support_url: Option<String>,
    pub support_email: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Upload {
    pub info: ImageInfo,
    pub sha256: String,
    #[serde(skip)]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrandingState {
    pub saved: bool,
    /// `#RRGGBB`, normalized by rm-brand
    pub primary_color: String,
    pub theme: Theme,
    pub logo: Option<Upload>,
    pub icon: Option<Upload>,
}

impl Default for BrandingState {
    fn default() -> Self {
        BrandingState {
            saved: false,
            primary_color: rm_brand::TARO_PRIMARY.hex(),
            theme: Theme::System,
            logo: None,
            icon: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RelayState {
    pub saved: bool,
    pub host: String,
    pub port: u16,
    #[serde(skip)]
    pub key: Option<Secret>,
    /// `••••…abcd`
    pub key_masked: Option<String>,
    /// generated | entered | imported
    pub key_source: Option<String>,
    pub client_updates: ClientUpdates,
    /// the provider's portal origin: sign-in and machine management in the client
    pub portal: Option<String>,
}

impl Default for RelayState {
    fn default() -> Self {
        RelayState {
            saved: false,
            host: String::new(),
            port: DEFAULT_PORT,
            key: None,
            key_masked: None,
            key_source: None,
            client_updates: ClientUpdates::Notify,
            portal: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct ComponentsState {
    pub loading: bool,
    pub releases: Option<Vec<ReleaseInfo>>,
    pub error: Option<String>,
    pub verifying: Option<String>,
    pub selected: Option<Selected>,
    /// component ids to build
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BuildItem {
    pub id: String,
    pub label: String,
    /// pending | running | ok | fail | skipped
    pub status: String,
    pub detail: Option<String>,
    pub done: u64,
    pub total: u64,
}

/// One produced package, with what it must contain (for validation).
#[derive(Clone, Debug)]
pub struct PackageRecord {
    pub component: Component,
    /// relative to the output folder (`windows/Acme-1.4.1-windows-x64.zip`)
    pub artifact: String,
    pub built: Built,
    /// every file of the verified official package: path -> sha256
    pub source_files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Artifact {
    pub component: Option<String>,
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BuildResult {
    /// the output folder (absolute)
    pub dir: String,
    pub name: String,
    pub built_at: String,
    pub taro_version: String,
    pub enterprise_version: String,
    pub provider_json_sha256: String,
    pub artifacts: Vec<Artifact>,
    /// [`Wizard::rev`] the build was made from
    pub rev: u64,
    /// provider.json as written (it holds the relay key: never serialized)
    #[serde(skip)]
    pub provider_json: Vec<u8>,
    #[serde(skip)]
    pub logo: Option<Vec<u8>>,
    #[serde(skip)]
    pub icon: Option<Vec<u8>>,
    #[serde(skip)]
    pub packages: Vec<PackageRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BuildState {
    pub running: bool,
    pub phase: String,
    pub items: Vec<BuildItem>,
    pub error: Option<String>,
    pub cancelled: bool,
    pub output_dir: String,
    pub result: Option<BuildResult>,
    #[serde(skip)]
    pub cancel: Option<Arc<AtomicBool>>,
    #[serde(skip)]
    pub generation: u64,
}

pub struct Wizard {
    pub preflight: Option<PreflightReport>,
    pub preflight_running: bool,
    pub identity: Option<Identity>,
    pub branding: BrandingState,
    pub relay: RelayState,
    pub components: ComponentsState,
    pub build: BuildState,
    pub validation: Option<Report>,
    pub validation_running: bool,
    /// Step 5: release signing on GitHub (optional)
    pub signing: crate::signing::SigningState,
    /// bumped on every change of an input, so a build of older inputs shows as stale
    pub rev: u64,
}

pub type FolderOpener = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

pub struct AppState {
    pub access_token: String,
    pub addr: SocketAddr,
    pub endpoints: Endpoints,
    pub timings: Timings,
    pub http: reqwest::Client,
    pub log: DiagLog,
    /// the release keys component manifests must be signed with
    pub keys: TrustedKeys,
    pub open_folder: FolderOpener,
    /// where release signing keys are backed up (Step 5)
    pub key_dir: PathBuf,
    pub shutdown: Arc<Notify>,
    wizard: Mutex<Wizard>,
}

pub struct Options {
    pub endpoints: Endpoints,
    pub timings: Timings,
    /// `taro_update::official_keys()` in the shipped wizard (main.rs); tests pass a test key
    pub keys: TrustedKeys,
    pub output_dir: PathBuf,
    pub open_folder: FolderOpener,
    /// `platform::default_key_dir()` in the shipped wizard
    pub key_dir: PathBuf,
}

impl AppState {
    pub fn new(addr: SocketAddr, access_token: String, o: Options) -> Arc<Self> {
        let log = DiagLog::new();
        log.register_secret(&Secret::new(access_token.clone()));
        let w = Wizard {
            preflight: None,
            preflight_running: false,
            identity: None,
            branding: BrandingState::default(),
            relay: RelayState::default(),
            components: ComponentsState {
                platforms: COMPONENT_IDS.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            build: BuildState {
                running: false,
                phase: "idle".into(),
                items: vec![],
                error: None,
                cancelled: false,
                output_dir: o.output_dir.display().to_string(),
                result: None,
                cancel: None,
                generation: 0,
            },
            validation: None,
            validation_running: false,
            signing: Default::default(),
            rev: 0,
        };
        Arc::new(AppState {
            access_token,
            addr,
            http: crate::config::http_client(),
            endpoints: o.endpoints,
            timings: o.timings,
            log,
            keys: o.keys,
            open_folder: o.open_folder,
            key_dir: o.key_dir,
            shutdown: Arc::new(Notify::new()),
            wizard: Mutex::new(w),
        })
    }

    /// Lock the wizard state. Never hold the guard across an `.await`.
    pub fn w(&self) -> MutexGuard<'_, Wizard> {
        self.wizard.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set the relay key (and keep it out of the log and of every other response).
    pub fn set_relay_key(&self, key: Secret, source: &str) {
        self.log.register_secret(&key);
        let mut w = self.w();
        w.relay.key_masked = Some(crate::secret::mask(&key));
        w.relay.key = Some(key);
        w.relay.key_source = Some(source.into());
    }
}

/// A valid placeholder provider, so one step's fields can be checked with
/// [`Provider::problems`] on their own (the other steps' fields are filled with valid values).
pub fn placeholder() -> Provider {
    Provider {
        schema: SCHEMA.into(),
        provider_id: "placeholder".into(),
        product: Product {
            name: "Placeholder".into(),
            short_name: "Placeholder".into(),
            vendor: "Placeholder".into(),
            support_url: None,
            support_email: None,
        },
        branding: Branding {
            primary_color: rm_brand::TARO_PRIMARY.hex(),
            tokens: None,
            theme: Theme::System,
            logo: None,
            icon: None,
        },
        relay: Relay {
            host: "relay.example.com".into(),
            port: DEFAULT_PORT,
            key: "placeholder-key-0123456789".into(),
        },
        account: None,
        updates: Updates::default(),
        generated: None,
    }
}

/// The problems of the fields starting with one of `prefixes`, as `{field: message}`.
pub fn field_errors(p: &Provider, prefixes: &[&str]) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    for FieldError { field, message } in p.problems() {
        if prefixes.iter().any(|x| field.starts_with(x)) && !m.contains_key(&field) {
            m.insert(field, json!(message));
        }
    }
    m
}

fn step(id: &str, title: &str, status: &str, summary: impl Into<String>) -> Value {
    json!({ "id": id, "title": title, "status": status, "summary": summary.into() })
}

/// The contrast numbers the UI shows for a colour (from rm-brand's tokens).
pub fn contrast(t: &Tokens) -> Value {
    let c = |a: &str, b: &str| {
        let (Ok(a), Ok(b)) = (rm_brand::Color::parse(a), rm_brand::Color::parse(b)) else {
            return 0.0;
        };
        (a.contrast(b) * 100.0).round() / 100.0
    };
    let light = c(&t.on_primary, &t.primary);
    let dark = c(&t.on_primary_dark, &t.primary_dark);
    json!({
        "on_primary": light,
        "on_primary_dark": dark,
        "aa": rm_brand::AA,
        "light_ok": light >= rm_brand::AA,
        "dark_ok": dark >= rm_brand::AA,
    })
}

impl Wizard {
    /// The provider as it would be written now (`generated` as given).
    pub fn provider(&self, generated: Option<Generated>) -> Result<Provider, String> {
        let i = self
            .identity
            .as_ref()
            .ok_or("Save the product identity first.")?;
        if !self.branding.saved {
            return Err("Save the branding first.".into());
        }
        if !self.relay.saved {
            return Err("Save the relay endpoint first.".into());
        }
        let key = self
            .relay
            .key
            .as_ref()
            .ok_or("Generate or enter the relay admission key first.")?;
        let p = Provider {
            schema: SCHEMA.into(),
            provider_id: i.provider_id.clone(),
            product: Product {
                name: i.name.clone(),
                short_name: i.short_name.clone(),
                vendor: i.vendor.clone(),
                support_url: i.support_url.clone(),
                support_email: i.support_email.clone(),
            },
            branding: Branding {
                primary_color: self.branding.primary_color.clone(),
                tokens: None,
                theme: self.branding.theme,
                logo: self.branding.logo.as_ref().map(|_| "logo.png".to_string()),
                icon: self.branding.icon.as_ref().map(|_| "icon.png".to_string()),
            },
            relay: Relay {
                host: self.relay.host.clone(),
                port: self.relay.port,
                key: key.expose().to_string(),
            },
            account: self
                .relay
                .portal
                .clone()
                .map(|portal| rm_brand::Account { portal }),
            updates: Updates {
                clients: self.relay.client_updates,
            },
            generated,
        }
        .normalized();
        if let Some(f) = p.problems().first() {
            return Err(format!("provider.json: {}: {}", f.field, f.message));
        }
        Ok(p)
    }

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
        v.push(match &self.identity {
            None => step("identity", "Product identity", "todo", "Not saved"),
            Some(i) => step(
                "identity",
                "Product identity",
                "ok",
                format!("{} ({})", i.name, i.provider_id),
            ),
        });
        v.push(if self.branding.saved {
            let t = Tokens::derive(
                rm_brand::Color::parse(&self.branding.primary_color)
                    .unwrap_or(rm_brand::TARO_PRIMARY),
            );
            let low = !contrast(&t)["light_ok"].as_bool().unwrap_or(true);
            step(
                "branding",
                "Branding",
                if low { "warn" } else { "ok" },
                format!(
                    "{}{}",
                    self.branding.primary_color,
                    if low { " (low contrast)" } else { "" }
                ),
            )
        } else {
            step("branding", "Branding", "todo", "Not saved")
        });
        v.push(if self.relay.saved {
            step(
                "relay",
                "Relay endpoint",
                "ok",
                format!("{}:{}", self.relay.host, self.relay.port),
            )
        } else {
            step("relay", "Relay endpoint", "todo", "Not saved")
        });
        let g = &self.signing;
        v.push(if g.running {
            step(
                "signing",
                "Signing & release",
                "running",
                "Running on GitHub",
            )
        } else if let Some(e) = &g.error {
            step("signing", "Signing & release", "fail", e.clone())
        } else if g.done {
            step(
                "signing",
                "Signing & release",
                "ok",
                "Released, packages built",
            )
        } else if let Some(t) = &g.fork_to {
            step(
                "signing",
                "Signing & release",
                "todo",
                format!("Start makes {t}"),
            )
        } else if let Some(r) = &g.repo {
            step(
                "signing",
                "Signing & release",
                if g.has_secret { "ok" } else { "todo" },
                if g.has_secret {
                    format!("{}: signing set up", r.full_name)
                } else {
                    format!("{}: no signing key yet", r.full_name)
                },
            )
        } else {
            step(
                "signing",
                "Signing & release",
                "optional",
                "One token does it all",
            )
        });
        let c = &self.components;
        v.push(if c.verifying.is_some() || c.loading {
            step("components", "Components & versions", "running", "Checking")
        } else if let Some(s) = &c.selected {
            step(
                "components",
                "Components & versions",
                if s.components.iter().all(|x| x.url_ok) {
                    "ok"
                } else {
                    "warn"
                },
                format!(
                    "Enterprise {} (Taro {})",
                    s.enterprise_version, s.taro_version
                ),
            )
        } else if let Some(e) = &c.error {
            step("components", "Components & versions", "fail", e.clone())
        } else {
            step(
                "components",
                "Components & versions",
                "todo",
                "No release chosen",
            )
        });
        let b = &self.build;
        v.push(if b.running {
            step("build", "Build", "running", b.phase.clone())
        } else if let Some(r) = &b.result {
            step("build", "Build", "ok", r.name.clone())
        } else if b.cancelled {
            step("build", "Build", "todo", "Cancelled")
        } else if let Some(e) = &b.error {
            step("build", "Build", "fail", e.clone())
        } else {
            step("build", "Build", "todo", "Not built yet")
        });
        v.push(match (&self.validation, self.validation_running) {
            (_, true) => step("validate", "Validate & export", "running", "Checking"),
            (None, _) => step("validate", "Validate & export", "todo", "Not validated"),
            (Some(r), _) if r.ok => step(
                "validate",
                "Validate & export",
                "ok",
                format!("{} checks passed", r.checks.len()),
            ),
            (Some(r), _) => step(
                "validate",
                "Validate & export",
                "fail",
                format!(
                    "{} check(s) failed",
                    r.checks.iter().filter(|c| c.status == "fail").count()
                ),
            ),
        });
        v
    }

    /// Everything the UI may see. Contains no secrets.
    pub fn view(&self) -> Value {
        let tokens = rm_brand::Color::parse(&self.branding.primary_color)
            .ok()
            .map(Tokens::derive);
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "steps": self.steps(),
            "preflight": self.preflight,
            "identity": self.identity,
            "branding": self.branding,
            "tokens": tokens,
            "contrast": tokens.as_ref().map(contrast),
            "relay": self.relay,
            "components": self.components,
            "build": self.build,
            "stale": self.build.result.as_ref().is_some_and(|r| r.rev != self.rev),
            "validation": self.validation,
            "signing": self.signing,
            "config": {
                "default_port": DEFAULT_PORT,
                "key_len": KEY_LEN,
                "taro_primary": rm_brand::TARO_PRIMARY.hex(),
                "download_hosts": crate::config::DOWNLOAD_HOSTS,
                "component_ids": COMPONENT_IDS,
                "release_repo": crate::config::RELEASE_REPO,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> Wizard {
        let mut w = Wizard {
            preflight: None,
            preflight_running: false,
            identity: Some(Identity {
                provider_id: "acme-cloud".into(),
                name: "Acme Connect".into(),
                short_name: "Acme".into(),
                vendor: "Acme Cloud Ltd".into(),
                support_url: Some("https://acme.example/help".into()),
                support_email: None,
            }),
            branding: BrandingState::default(),
            relay: RelayState::default(),
            components: ComponentsState::default(),
            build: BuildState {
                running: false,
                phase: "idle".into(),
                items: vec![],
                error: None,
                cancelled: false,
                output_dir: String::new(),
                result: None,
                cancel: None,
                generation: 0,
            },
            validation: None,
            validation_running: false,
            signing: Default::default(),
            rev: 0,
        };
        w.branding.primary_color = "#3D7A5B".into();
        w.branding.saved = true;
        w.relay.host = "relay.acme.example".into();
        w.relay.key = Some(Secret::new("k3y-0123456789abcdef_XYZ"));
        w.relay.saved = true;
        w
    }

    #[test]
    fn the_generated_provider_json_is_valid_and_keeps_the_colour() {
        let w = filled();
        let p = w.provider(None).unwrap();
        assert!(p.problems().is_empty(), "{:?}", p.problems());
        // a colour that is no preset is written exactly, tokens derived by rm-brand
        assert_eq!(p.branding.primary_color, "#3D7A5B");
        assert_eq!(
            p.branding.tokens,
            Some(Tokens::derive(rm_brand::Color(0x3d, 0x7a, 0x5b)))
        );
        let json = p.to_json();
        let back = Provider::from_json(&json).unwrap();
        assert_eq!(back, p);
        assert_eq!(back.relay.key, "k3y-0123456789abcdef_XYZ");
        assert_eq!(back.updates.clients, ClientUpdates::Notify);
        assert!(back.branding.logo.is_none());
        // the view never carries the key
        assert!(!w.view().to_string().contains("k3y-0123456789abcdef_XYZ"));
    }

    #[test]
    fn missing_steps_are_named() {
        let mut w = filled();
        w.relay.key = None;
        assert!(w.provider(None).unwrap_err().contains("admission key"));
        let mut w = filled();
        w.branding.saved = false;
        assert!(w.provider(None).unwrap_err().contains("branding"));
        let mut w = filled();
        w.identity = None;
        assert!(w.provider(None).unwrap_err().contains("identity"));
    }

    #[test]
    fn field_errors_are_filtered_by_step() {
        let mut p = placeholder();
        p.provider_id = "Bad Id".into();
        p.relay.port = 0;
        let f = field_errors(&p, &["provider_id", "product."]);
        assert!(f.contains_key("provider_id"));
        assert!(!f.contains_key("relay.port"));
        assert!(placeholder().problems().is_empty());
    }
}
