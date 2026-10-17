//! Wizard 2: the relay's first-run setup, a local web page on 127.0.0.1 behind a one-time token
//! (reached from the operator's PC through `ssh -L`). It never listens on a public address.
//!
//! It checks the machine, DNS and the network, gets the certificate, opens the firewall, writes
//! `/etc/taro/runtime/relay.toml`, creates the service account and the systemd units, starts
//! the relay and checks it answers. Run again later, it starts from the current settings and
//! keeps the relay's data, the certificate and the provider folder as they are.

use crate::checks::{self, Check, Net, Status};
use crate::config::{Runtime, TlsMode, UpdateMode, Window};
use crate::system::{System, USER};
use crate::tls;
use crate::web;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, Method, Response, StatusCode};
use axum::middleware::Next;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use taro_update::policy::Layout;

/// Checks that tell whether the relay answers (the real one probes it; tests fake it).
pub trait Probe: Send + Sync {
    fn health(&self, relay_port: u16, version: &str) -> Result<String, String>;
}

pub struct RealProbe;

impl Probe for RealProbe {
    fn health(&self, relay_port: u16, version: &str) -> Result<String, String> {
        crate::serve::probe(relay_port, Some(version))
    }
}

pub struct Setup {
    pub layout: Layout,
    pub sys: Arc<dyn System>,
    pub net: Arc<dyn Net>,
    pub probe: Arc<dyn Probe>,
    pub token: String,
    pub port: u16,
    pub draft: Mutex<Draft>,
    pub challenges: tls::Challenges,
    pub log: Mutex<Vec<String>>,
    pub done: tokio::sync::Notify,
    /// `TARO_ROOT`: the installation lives under this folder (tests, portable installs)
    pub root_override: Option<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub runtime: Option<Runtime>,
    pub system: Vec<Check>,
    pub network: Vec<Check>,
    pub tls: Option<Value>,
    pub firewall: Option<checks::Firewall>,
    pub applied: Option<Value>,
}

fn now_rfc3339() -> String {
    let s = tls::now_unix();
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn stamp() -> String {
    now_rfc3339()
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

impl Setup {
    fn note(&self, s: impl Into<String>) {
        let s = s.into();
        eprintln!("setup: {s}");
        self.log
            .lock()
            .unwrap()
            .push(format!("{} {s}", now_rfc3339()));
    }

    fn provider(&self) -> Result<rm_brand::Provider, String> {
        match web::provider(&self.layout) {
            Ok(Some(p)) => Ok(p),
            Ok(None) => Err(format!("{} is missing: install the relay package built for you by the Taro Enterprise package builder (it carries your provider folder)", self.layout.provider.join(rm_brand::FILE).display())),
            Err(e) => Err(e),
        }
    }

    /// The settings being edited: the draft, else the saved ones, else defaults.
    fn current(&self) -> Result<Runtime, String> {
        if let Some(r) = self.draft.lock().unwrap().runtime.clone() {
            return Ok(r);
        }
        if let Some(r) = Runtime::load(&self.layout)? {
            return Ok(r);
        }
        let p = self.provider()?;
        Ok(Runtime::defaults(&self.layout, &p.relay.host, p.relay.port))
    }
}

type S = Arc<Setup>;

pub fn router(s: S) -> Router {
    let api = Router::new()
        .route("/api/state", get(state))
        .route("/api/checks/system", post(check_system))
        .route("/api/checks/network", post(check_network))
        .route("/api/settings", post(settings))
        .route("/api/tls", post(tls_step))
        .route("/api/firewall/check", post(firewall_check))
        .route("/api/firewall/open", post(firewall_open))
        .route("/api/apply", post(apply))
        .route("/api/diagnostics.txt", get(diagnostics))
        .route("/api/finish", post(finish))
        .layer(axum::middleware::from_fn_with_state(s.clone(), api_guard));
    Router::new()
        .merge(api)
        .route("/", get(index))
        .route("/assets/{file}", get(asset))
        .route("/brand/{file}", get(brand))
        .route(
            "/favicon.ico",
            get(|State(s): State<S>| async move { web::brand(&s.layout, "icon.png") }),
        )
        .layer(axum::middleware::from_fn_with_state(s.clone(), host_guard))
        .with_state(s)
}

fn deny(status: StatusCode, msg: &str) -> Response<Body> {
    web::bytes(
        status,
        "application/json",
        json!({ "error": msg }).to_string(),
    )
}

/// Only `127.0.0.1:<port>` / `localhost:<port>` (no DNS rebinding), same-origin requests only.
async fn host_guard(State(s): State<S>, req: Request, next: Next) -> Response<Body> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let ok = [
        format!("127.0.0.1:{}", s.port),
        format!("localhost:{}", s.port),
    ];
    if !ok.iter().any(|o| o == host) {
        return deny(StatusCode::MISDIRECTED_REQUEST, "wrong host");
    }
    if let Some(site) = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
    {
        if site != "same-origin" && site != "none" {
            return deny(StatusCode::FORBIDDEN, "cross-site request");
        }
    }
    web::secure(next.run(req).await)
}

/// The API: the token in X-Setup-Token, and on changes an Origin of this page.
async fn api_guard(State(s): State<S>, req: Request, next: Next) -> Response<Body> {
    let tok = req
        .headers()
        .get("x-setup-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !web::same(tok, &s.token) {
        return deny(StatusCode::UNAUTHORIZED, "missing or wrong setup token");
    }
    if req.method() != Method::GET {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let ok = [
            format!("http://127.0.0.1:{}", s.port),
            format!("http://localhost:{}", s.port),
        ];
        if !ok.iter().any(|o| o == origin) {
            return deny(StatusCode::FORBIDDEN, "wrong origin");
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct Tok {
    t: Option<String>,
}

async fn index(State(s): State<S>, Query(q): Query<Tok>) -> Response<Body> {
    if !q.t.as_deref().is_some_and(|t| web::same(t, &s.token)) {
        return web::bytes(
            StatusCode::UNAUTHORIZED,
            "text/plain; charset=utf-8",
            "Open the address printed by `taro-relay setup` (it includes the one-time token).",
        );
    }
    web::core_asset(&s.layout, "setup/index.html")
}

async fn asset(State(s): State<S>, UrlPath(file): UrlPath<String>) -> Response<Body> {
    web::core_asset(&s.layout, &format!("setup/{file}"))
}

async fn brand(State(s): State<S>, UrlPath(file): UrlPath<String>) -> Response<Body> {
    web::brand(&s.layout, &file)
}

async fn acme_challenge(State(s): State<S>, UrlPath(token): UrlPath<String>) -> Response<Body> {
    match s.challenges.get(&token) {
        Some(v) => web::bytes(StatusCode::OK, "text/plain", v),
        None => web::not_found(),
    }
}

fn ok(v: Value) -> Response<Body> {
    web::bytes(StatusCode::OK, "application/json", v.to_string())
}

fn bad(msg: impl Into<String>) -> Response<Body> {
    web::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        json!({ "error": msg.into() }).to_string(),
    )
}

fn field_errors(errs: Vec<(String, String)>) -> Response<Body> {
    web::bytes(StatusCode::UNPROCESSABLE_ENTITY, "application/json", json!({ "error": "Some settings need attention.", "fields": errs.into_iter().map(|(f, m)| json!({"field": f, "message": m})).collect::<Vec<_>>() }).to_string())
}

fn service_running(sys: &dyn System) -> bool {
    sys.run("systemctl", &["is-active", "--quiet", "taro-relay"])
        .is_ok()
}

async fn state(State(s): State<S>) -> Response<Body> {
    let provider = s.provider();
    let saved = Runtime::load(&s.layout);
    let installed = taro_update::apply::installed(&s.layout).ok();
    let d = s.draft.lock().unwrap().clone();
    let current = s.current().ok();
    let cert = current
        .as_ref()
        .filter(|r| r.tls.mode != TlsMode::Off)
        .and_then(|r| tls::check_pair(&r.tls.cert, &r.tls.key, &r.relay.public_host).ok());
    ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "configured": matches!(saved, Ok(Some(_))),
        "config_error": saved.err(),
        "provider": provider.as_ref().ok().map(web::public_identity),
        "provider_error": provider.err(),
        "installed": installed.map(|i| json!({"version": i.version, "arch": i.arch})),
        "settings": current.map(|r| settings_json(&r)),
        "certificate": cert,
        "system": d.system,
        "network": d.network,
        "tls": d.tls,
        "firewall": d.firewall,
        "applied": d.applied,
        "service_running": service_running(s.sys.as_ref()),
        "paths": {
            "provider": s.layout.provider, "runtime": Runtime::path(&s.layout), "state": s.layout.state, "core": s.layout.core(),
        },
    }))
}

fn settings_json(r: &Runtime) -> Value {
    json!({
        "public_host": r.relay.public_host,
        "port": r.relay_port(),
        "tls_mode": r.tls.mode,
        "email": r.tls.email,
        "staging": r.tls.directory == crate::config::LETS_ENCRYPT_STAGING,
        "directory": r.tls.directory,
        "cert": r.tls.cert, "key": r.tls.key,
        "update_mode": r.updates.mode,
        "window": r.updates.window,
    })
}

async fn check_system(State(s): State<S>) -> Response<Body> {
    let r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    let v = checks::system(
        s.sys.as_ref(),
        &s.layout,
        r.relay_port(),
        r.tls.mode == TlsMode::Acme || r.tls.mode == TlsMode::Files,
        service_running(s.sys.as_ref()),
    );
    s.note(format!("system checks: {:?}", checks::worst(&v)));
    s.draft.lock().unwrap().system = v.clone();
    ok(json!({ "checks": v, "status": checks::worst(&v) }))
}

async fn check_network(State(s): State<S>) -> Response<Body> {
    let r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    let acme = (r.tls.mode == TlsMode::Acme).then_some(r.tls.directory.as_str());
    let v = checks::network(
        s.net.as_ref(),
        &r.relay.public_host,
        acme,
        &r.updates.source,
    )
    .await;
    s.note(format!("network checks: {:?}", checks::worst(&v)));
    s.draft.lock().unwrap().network = v.clone();
    ok(json!({ "checks": v, "status": checks::worst(&v) }))
}

#[derive(Deserialize)]
struct SettingsIn {
    tls_mode: TlsMode,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    agree_tos: bool,
    #[serde(default)]
    staging: bool,
    #[serde(default)]
    cert: Option<String>,
    #[serde(default)]
    key: Option<String>,
    update_mode: UpdateMode,
    #[serde(default)]
    window: Option<String>,
}

/// Validate and keep the settings (nothing is written until Apply).
async fn settings(State(s): State<S>, Json(i): Json<SettingsIn>) -> Response<Body> {
    let p = match s.provider() {
        Ok(p) => p,
        Err(e) => return bad(e),
    };
    let mut r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    let mut errs = Vec::new();
    // the host and port are the provider's: the clients built for it connect there
    r.relay.public_host = p.relay.host.clone();
    r.relay.listen = format!("0.0.0.0:{}", p.relay.port);
    r.tls.mode = i.tls_mode;
    r.tls.email = i
        .email
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty());
    match i.tls_mode {
        TlsMode::Acme => {
            if r.tls.email.as_deref().is_none_or(|e| {
                !e.contains('@') || e.contains(char::is_whitespace) || e.len() > 120
            }) {
                errs.push((
                    "email".into(),
                    "An email address for the certificate authority (expiry notices).".into(),
                ));
            }
            if !i.agree_tos {
                errs.push((
                    "agree_tos".into(),
                    "Accept the certificate authority's terms to get a certificate.".into(),
                ));
            }
            r.tls.directory = if i.staging {
                crate::config::LETS_ENCRYPT_STAGING
            } else {
                crate::config::LETS_ENCRYPT
            }
            .into();
            r.tls.cert = s.layout.state.join("tls/fullchain.pem");
            r.tls.key = s.layout.state.join("tls/privkey.pem");
        }
        TlsMode::Files => {
            let (c, k) = (i.cert.unwrap_or_default(), i.key.unwrap_or_default());
            if !c.starts_with('/') {
                errs.push((
                    "cert".into(),
                    "The full path of the certificate chain (PEM).".into(),
                ));
            }
            if !k.starts_with('/') {
                errs.push((
                    "key".into(),
                    "The full path of the private key (PEM).".into(),
                ));
            }
            r.tls.cert = c.into();
            r.tls.key = k.into();
        }
        TlsMode::Off => {}
    }
    r.updates.mode = i.update_mode;
    r.updates.window = i
        .window
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty());
    if let Some(w) = &r.updates.window {
        if let Err(e) = Window::parse(w) {
            errs.push(("window".into(), e));
        }
    }
    if r.updates.mode == UpdateMode::Auto && r.updates.window.is_none() {
        errs.push((
            "window".into(),
            "Automatic updates restart the relay: give them a maintenance window.".into(),
        ));
    }
    if !errs.is_empty() {
        return field_errors(errs);
    }
    if let Err(e) = r.check() {
        return bad(e);
    }
    s.note(format!(
        "settings: tls {:?}, updates {:?}",
        r.tls.mode, r.updates.mode
    ));
    let out = settings_json(&r);
    let mut d = s.draft.lock().unwrap();
    d.runtime = Some(r);
    d.tls = None;
    d.applied = None;
    ok(json!({ "settings": out }))
}

/// The certificate: kept if the current one is fine, else obtained (ACME) or checked (files).
async fn tls_step(State(s): State<S>) -> Response<Body> {
    let r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    let host = r.relay.public_host.clone();
    let res = match r.tls.mode {
        TlsMode::Off => {
            json!({ "status": "skip", "detail": "No HTTPS status page. The relay itself works without it (sessions are end-to-end encrypted)." })
        }
        TlsMode::Files => match tls::check_pair(&r.tls.cert, &r.tls.key, &host) {
            Ok(i) => {
                json!({ "status": "ok", "detail": format!("{} until {} days from now, by {}", i.names.join(", "), i.days_left, i.issuer), "certificate": i })
            }
            Err(e) => {
                json!({ "status": "fail", "detail": e, "fix": "Point to the PEM chain and key for this host name (readable by root)." })
            }
        },
        TlsMode::Acme => match tls::check_pair(&r.tls.cert, &r.tls.key, &host) {
            Ok(i) if i.days_left > tls::RENEW_DAYS => {
                json!({ "status": "ok", "detail": format!("The current certificate is kept: {} for {} more days. The relay renews it by itself.", i.names.join(", "), i.days_left), "certificate": i })
            }
            _ => {
                let email = r.tls.email.clone().unwrap_or_default();
                let store = tls::Store {
                    dir: r
                        .tls
                        .key
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| s.layout.state.join("tls")),
                };
                // the CA connects to port 80: answer it here, unless the running relay holds it
                let listener = tokio::net::TcpListener::bind(&r.tls.http_listen).await;
                let server = match listener {
                    Ok(l) => {
                        let app = Router::new()
                            .route("/.well-known/acme-challenge/{token}", get(acme_challenge))
                            .with_state(s.clone());
                        Some(tokio::spawn(async move {
                            let _ = axum::serve(l, app).await;
                        }))
                    }
                    Err(e) => {
                        return ok(
                            json!({ "status": "fail", "detail": format!("port 80 is in use ({e})"), "fix": "The certificate authority checks this server on port 80: stop what listens there (`sudo ss -ltnp | grep :80`), then try again." }),
                        );
                    }
                };
                s.note(format!(
                    "requesting a certificate for {host} from {}",
                    r.tls.directory
                ));
                let res = tls::issue(
                    &r.tls.directory,
                    &email,
                    &host,
                    &store,
                    &s.challenges,
                    &r.tls.cert,
                    &r.tls.key,
                )
                .await;
                if let Some(h) = server {
                    h.abort();
                }
                match res {
                    Ok(i) => {
                        json!({ "status": "ok", "detail": format!("Certificate for {} obtained, valid for {} days; the relay renews it automatically.", i.names.join(", "), i.days_left), "certificate": i })
                    }
                    Err(e) => {
                        json!({ "status": "fail", "detail": e, "fix": format!("Check that {host} points at this server, that port 80/tcp is open to the internet (also in your cloud provider's firewall), and try again. Use the staging CA while testing to avoid rate limits.") })
                    }
                }
            }
        },
    };
    s.note(format!("tls: {}", res["status"]));
    s.draft.lock().unwrap().tls = Some(res.clone());
    ok(res)
}

async fn firewall_check(State(s): State<S>) -> Response<Body> {
    let r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    let fw = checks::firewall(s.sys.as_ref(), r.relay_port(), r.tls.mode != TlsMode::Off);
    s.draft.lock().unwrap().firewall = Some(fw.clone());
    ok(
        json!({ "firewall": fw, "cloud": "Also open these ports in your hosting provider's firewall or security group: they cannot be seen from this server." }),
    )
}

#[derive(Deserialize)]
struct Confirm {
    confirm: bool,
}

async fn firewall_open(State(s): State<S>, Json(c): Json<Confirm>) -> Response<Body> {
    if !c.confirm {
        return bad("confirm first");
    }
    let Some(fw) = s.draft.lock().unwrap().firewall.clone() else {
        return bad("check the firewall first");
    };
    match checks::open_firewall(s.sys.as_ref(), &fw) {
        Ok(done) => {
            s.note(format!(
                "firewall: {}",
                if done.is_empty() {
                    "nothing to do".into()
                } else {
                    done.join("; ")
                }
            ));
            ok(json!({ "done": done }))
        }
        Err(e) => bad(format!("the firewall could not be changed: {e}")),
    }
}

/// The systemd units, for this installation's paths.
pub fn units(layout: &Layout, root_override: Option<&Path>) -> Vec<(&'static str, String)> {
    let bin = layout.core().join("bin/taro-relay");
    let env = root_override
        .map(|r| format!("Environment=TARO_ROOT={}\n", r.display()))
        .unwrap_or_default();
    let relay = format!(
        "[Unit]\nDescription=Taro relay ({{product}})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\n{env}ExecStart={bin} run\nUser={USER}\nGroup={USER}\nRestart=always\nRestartSec=2\n# not configured: no restart loop\nRestartPreventExitStatus=78\nAmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\nNoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=yes\nPrivateTmp=yes\nReadWritePaths={state}/relay {state}/tls\nLimitNOFILE=65536\n\n[Install]\nWantedBy=multi-user.target\n",
        bin = bin.display(),
        state = layout.state.display()
    );
    let update_svc = format!("[Unit]\nDescription=Taro relay updates (check, and install in the maintenance window if allowed)\nAfter=network-online.target\n\n[Service]\nType=oneshot\n{env}ExecStart={bin} update auto\n", bin = bin.display());
    let timer = "[Unit]\nDescription=Taro relay updates, hourly\n\n[Timer]\nOnBootSec=10min\nOnUnitActiveSec=1h\nRandomizedDelaySec=10min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n".to_string();
    vec![
        ("taro-relay.service", relay),
        ("taro-update.service", update_svc),
        ("taro-update.timer", timer),
    ]
}

fn mkdir(sys: &dyn System, p: &Path, mode: u32, owner: (&str, &str)) -> Result<(), String> {
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    crate::config::set_mode(p, mode)?;
    sys.chown(p, owner.0, owner.1)
}

/// Apply the settings: everything Wizard 2 does to the machine, in order; stops at the first
/// failure and says where.
pub fn apply_settings(s: &Setup, r: &Runtime) -> Result<Vec<String>, (Vec<String>, String)> {
    let mut steps = Vec::new();
    macro_rules! step {
        ($what:expr, $e:expr) => {
            match $e {
                Ok(v) => {
                    steps.push($what.to_string());
                    v
                }
                Err(e) => return Err((steps, format!("{}: {}", $what, e))),
            }
        };
    }
    let p = step!("provider folder", s.provider());
    if p.relay.host != r.relay.public_host || p.relay.port != r.relay_port() {
        return Err((
            steps,
            format!(
                "the provider's clients connect to {}:{}, the settings say {}:{}",
                p.relay.host,
                p.relay.port,
                r.relay.public_host,
                r.relay_port()
            ),
        ));
    }
    let sys = s.sys.as_ref();
    if !sys.is_root() {
        return Err((steps, "run setup with sudo".into()));
    }
    if sys.run("id", &["-u", USER]).is_err() {
        step!(
            format!("service account {USER}"),
            sys.run(
                "useradd",
                &[
                    "--system",
                    "--user-group",
                    "--no-create-home",
                    "--home-dir",
                    &s.layout.state.display().to_string(),
                    "--shell",
                    "/usr/sbin/nologin",
                    USER
                ]
            )
        );
    } else {
        steps.push(format!("service account {USER} (exists)"));
    }
    let l = &s.layout;
    step!(
        "folders",
        (|| {
            mkdir(sys, &l.runtime, 0o750, ("root", USER))?;
            mkdir(sys, &l.state, 0o755, ("root", "root"))?;
            mkdir(sys, &l.state.join("relay"), 0o700, (USER, USER))?;
            mkdir(sys, &l.state.join("tls"), 0o700, (USER, USER))?;
            mkdir(sys, &l.update_state(), 0o700, ("root", "root"))?;
            // the admission key is in provider.json: readable by the relay, not by everyone
            let pj = l.provider.join(rm_brand::FILE);
            crate::config::set_mode(&pj, 0o640)?;
            sys.chown(&pj, "root", USER)
        })()
    );
    if r.tls.mode != TlsMode::Off {
        for f in [&r.tls.cert, &r.tls.key] {
            if f.starts_with(&l.state) && f.exists() {
                step!(
                    format!("ownership of {}", f.display()),
                    sys.chown(f, USER, USER)
                );
            }
        }
    }
    let mut r = r.clone();
    r.configured_at = now_rfc3339();
    let backup = step!("settings", r.save(l, &stamp()));
    if let Some(b) = backup {
        steps.push(format!("previous settings kept in {}", b.display()));
    }
    step!(
        "settings permissions",
        crate::config::set_mode(&Runtime::path(l), 0o640).and_then(|_| sys.chown(
            &Runtime::path(l),
            "root",
            USER
        ))
    );
    let units_dir = sys.units_dir();
    for (name, text) in units(l, s.root_override.as_deref()) {
        let text = text.replace("{product}", &p.product.name);
        step!(
            format!("unit {name}"),
            std::fs::create_dir_all(&units_dir)
                .and_then(|_| std::fs::write(units_dir.join(name), text))
                .map_err(|e| e.to_string())
        );
    }
    step!(
        "systemctl daemon-reload",
        sys.run("systemctl", &["daemon-reload"])
    );
    step!(
        "enable the relay at boot",
        sys.run("systemctl", &["enable", "taro-relay"])
    );
    step!(
        "start the relay",
        sys.run("systemctl", &["restart", "taro-relay"])
    );
    if r.updates.mode == UpdateMode::Manual {
        let _ = sys.run("systemctl", &["disable", "--now", "taro-update.timer"]);
        steps.push("update checks: manual (timer off)".into());
    } else {
        step!(
            "update checks (timer)",
            sys.run("systemctl", &["enable", "--now", "taro-update.timer"])
        );
    }
    // it answers: a few tries while it starts
    let mut last = String::new();
    for _ in 0..20 {
        match s.probe.health(r.relay_port(), env!("CARGO_PKG_VERSION")) {
            Ok(h) => {
                steps.push(format!("health: {h}"));
                return Ok(steps);
            }
            Err(e) => last = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err((steps, format!("the relay did not answer its health check: {last}. See `journalctl -u taro-relay -n 50`.")))
}

async fn apply(State(s): State<S>, Json(c): Json<Confirm>) -> Response<Body> {
    if !c.confirm {
        return bad("confirm first");
    }
    let r = match s.current() {
        Ok(r) => r,
        Err(e) => return bad(e),
    };
    if let Err(e) = r.check() {
        return bad(e);
    }
    let blocking: Vec<String> = s
        .draft
        .lock()
        .unwrap()
        .system
        .iter()
        .filter(|c| c.status == Status::Fail)
        .map(|c| c.label.clone())
        .collect();
    if !blocking.is_empty() {
        return bad(format!("fix these first: {}", blocking.join(", ")));
    }
    let s2 = s.clone();
    let res = tokio::task::spawn_blocking(move || apply_settings(&s2, &r))
        .await
        .unwrap_or_else(|e| Err((vec![], e.to_string())));
    let v = match res {
        Ok(steps) => {
            s.note("applied; the relay is running");
            json!({ "status": "ok", "steps": steps })
        }
        Err((steps, e)) => {
            s.note(format!("apply failed: {e}"));
            json!({ "status": "fail", "steps": steps, "error": e, "recovery": "Nothing else was changed after this step. Fix the cause and press Apply again: setup can be run any number of times, and keeps the relay's data, the certificate and the provider folder." })
        }
    };
    s.draft.lock().unwrap().applied = Some(v.clone());
    ok(v)
}

async fn diagnostics(State(s): State<S>, _h: HeaderMap) -> Response<Body> {
    let mut t = String::new();
    t.push_str(&format!(
        "taro-relay {} setup diagnostics, {}\n\n",
        env!("CARGO_PKG_VERSION"),
        now_rfc3339()
    ));
    let d = s.draft.lock().unwrap().clone();
    for c in d.system.iter().chain(d.network.iter()) {
        t.push_str(&format!(
            "[{:?}] {}: {}{}\n",
            c.status,
            c.label,
            c.detail,
            if c.fix.is_empty() {
                String::new()
            } else {
                format!(" -> {}", c.fix)
            }
        ));
    }
    if let Some(v) = &d.tls {
        t.push_str(&format!("\nTLS: {v}\n"));
    }
    if let Some(v) = &d.applied {
        t.push_str(&format!("\nApply: {v}\n"));
    }
    t.push_str("\nLog:\n");
    for l in s.log.lock().unwrap().iter() {
        t.push_str(l);
        t.push('\n');
    }
    if let Ok(j) = s.sys.run(
        "journalctl",
        &["-u", "taro-relay", "-n", "60", "--no-pager"],
    ) {
        t.push_str("\njournalctl -u taro-relay:\n");
        t.push_str(&j);
    }
    // the admission key never leaves the server in a diagnostic
    if let Ok(p) = s.provider() {
        t = t.replace(&p.relay.key, "[relay key]");
    }
    web::bytes(StatusCode::OK, "text/plain; charset=utf-8", t)
}

async fn finish(State(s): State<S>) -> Response<Body> {
    s.done.notify_one();
    ok(json!({ "ok": true }))
}
