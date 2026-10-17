//! Local HTTP server: request guard, embedded UI, JSON API.
//!
//! Every request must carry `Host: 127.0.0.1:<port>` (DNS-rebinding guard); an `Origin`
//! header, when present, must be `http://127.0.0.1:<port>` and is mandatory for non-GET API
//! calls; `Sec-Fetch-Site`, when present, must be `same-origin` or `none`. API calls need the
//! one-time access token in `X-Setup-Token`; the page itself needs it as `?t=`.

use crate::domain::{self, Strategy};
use crate::secret::{ct_eq, Secret};
use crate::state::{
    AppState, Deployment, Identity, Upload, MODULES, PROFILE_SINGLE, PROVIDER_MOCK,
};
use crate::validate::{self, Environment, StripeMode};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob: data:; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

type App = Arc<AppState>;

// ---------------------------------------------------------------------------------------------
// Errors / responses
// ---------------------------------------------------------------------------------------------

pub struct ApiErr {
    status: StatusCode,
    error: String,
    fields: Option<Value>,
}

impl ApiErr {
    fn bad(msg: impl Into<String>) -> Self {
        ApiErr {
            status: StatusCode::BAD_REQUEST,
            error: msg.into(),
            fields: None,
        }
    }
    fn conflict(msg: impl Into<String>) -> Self {
        ApiErr {
            status: StatusCode::CONFLICT,
            error: msg.into(),
            fields: None,
        }
    }
    fn upstream(msg: impl Into<String>) -> Self {
        ApiErr {
            status: StatusCode::BAD_GATEWAY,
            error: msg.into(),
            fields: None,
        }
    }
    fn fields(fields: Value) -> Self {
        ApiErr {
            status: StatusCode::BAD_REQUEST,
            error: "Some fields need attention.".into(),
            fields: Some(fields),
        }
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "ok": false, "error": self.error, "fields": self.fields })),
        )
            .into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiErr>;

fn ok(app: &AppState, message: Option<String>) -> ApiResult {
    let w = app.w();
    Ok(Json(
        json!({ "ok": true, "message": message, "state": w.view(app) }),
    ))
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, ApiErr> {
    serde_json::from_slice(body).map_err(|e| ApiErr::bad(format!("Invalid request: {e}")))
}

// ---------------------------------------------------------------------------------------------
// Router and guard
// ---------------------------------------------------------------------------------------------

pub fn router(app: App) -> Router {
    let uploads = Router::new()
        .route("/api/branding/{kind}", post(branding_upload))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024));
    let api = Router::new()
        .route("/api/state", get(get_state))
        .route("/api/preflight/run", post(preflight_run))
        .route("/api/deployment", post(deployment_save))
        .route("/api/identity", post(identity_save))
        .route("/api/branding", post(branding_save))
        .route("/api/branding/{kind}", delete(branding_delete))
        .route("/api/domain", post(domain_save))
        .route("/api/domain/verify", post(domain_verify))
        .route("/api/billing/key", post(billing_key))
        .route("/api/billing/webhook-secret", post(billing_webhook_secret))
        .route("/api/billing/webhook/check", post(billing_webhook_check))
        .route("/api/billing/webhook/create", post(billing_webhook_create))
        .route("/api/billing/smoke", post(billing_smoke))
        .route("/api/github/pat", post(github_pat))
        .route("/api/github/device/start", post(github_device_start))
        .route("/api/github/device/cancel", post(github_device_cancel))
        .route("/api/github/remembered/use", post(github_remembered_use))
        .route(
            "/api/github/remembered/forget",
            post(github_remembered_forget),
        )
        .route("/api/github/logout", post(github_logout))
        .route("/api/github/orgs", post(github_orgs))
        .route("/api/github/plan", post(github_plan))
        .route("/api/github/bootstrap", post(github_bootstrap))
        .route("/api/github/delete-repo", post(github_delete_repo))
        .route("/api/release/start", post(release_start))
        .route("/api/verify/run", post(verify_run))
        .route("/api/summary.json", get(summary_json))
        .route("/api/summary.md", get(summary_md))
        .route("/api/diagnostics.txt", get(diagnostics))
        .route("/api/shutdown", post(shutdown))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .merge(uploads)
        .layer(middleware::from_fn_with_state(app.clone(), scrub_responses));
    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_js))
        .route("/assets/app.css", get(app_css))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .merge(api)
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

fn deny(status: StatusCode, why: &str) -> Response {
    let mut r = (status, why.to_string()).into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

fn query_token(q: Option<&str>) -> Option<&str> {
    q?.split('&').find_map(|kv| kv.strip_prefix("t="))
}

pub async fn guard(State(app): State<App>, req: Request, next: Next) -> Response {
    let port = app.addr.port();
    let expected_host = format!("127.0.0.1:{port}");
    let expected_origin = format!("http://127.0.0.1:{port}");
    let h = req.headers();
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    if host != Some(expected_host.as_str()) {
        app.log.warn(format!(
            "rejected request with Host {:?} (DNS-rebinding guard)",
            host.unwrap_or("<none>")
        ));
        return deny(StatusCode::MISDIRECTED_REQUEST, "Wrong Host header.");
    }
    let path = req.uri().path().to_string();
    let is_api = path.starts_with("/api/");
    match h.get(header::ORIGIN).map(|v| v.to_str().unwrap_or("")) {
        Some(o) if o != expected_origin => {
            app.log.warn(format!("rejected request from Origin {o:?}"));
            return deny(StatusCode::FORBIDDEN, "Cross-origin request rejected.");
        }
        None if is_api && req.method() != Method::GET && req.method() != Method::HEAD => {
            return deny(StatusCode::FORBIDDEN, "Origin header required.");
        }
        _ => {}
    }
    if let Some(site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" && site != "none" {
            return deny(StatusCode::FORBIDDEN, "Cross-site request rejected.");
        }
    }
    if is_api {
        let tok = h
            .get("x-setup-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !ct_eq(tok, &app.access_token) {
            return deny(StatusCode::UNAUTHORIZED, "Missing or wrong setup token.");
        }
    } else if path == "/" {
        let ok = query_token(req.uri().query()).is_some_and(|t| ct_eq(t, &app.access_token));
        if !ok {
            return deny(
                StatusCode::FORBIDDEN,
                "Open the wizard with the link printed in the TaroEnterpriseSetup console window (it contains a one-time access token).",
            );
        }
    }
    let mut resp = next.run(req).await;
    let hm = resp.headers_mut();
    hm.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    hm.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    hm.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    hm.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    hm.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    hm.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    hm.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    resp
}

/// Defence in depth: scrub any secret the wizard holds from every API response body.
async fn scrub_responses(State(app): State<App>, req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return deny(StatusCode::INTERNAL_SERVER_ERROR, "response error"),
    };
    let text = String::from_utf8_lossy(&bytes);
    let (clean, hit) = app.log.scrub_known(&text);
    if hit {
        app.log
            .error("a response contained a secret and was scrubbed (this is a bug)");
    }
    let mut parts = parts;
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(clean))
}

async fn index() -> Response {
    let mut r = INDEX_HTML.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    r
}

async fn app_js() -> Response {
    let mut r = APP_JS.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript; charset=utf-8"),
    );
    r
}

async fn app_css() -> Response {
    let mut r = APP_CSS.into_response();
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    r
}

async fn not_found() -> Response {
    deny(StatusCode::NOT_FOUND, "Not found.")
}

// ---------------------------------------------------------------------------------------------
// Steps 1-4
// ---------------------------------------------------------------------------------------------

async fn get_state(State(app): State<App>) -> ApiResult {
    ok(&app, None)
}

async fn preflight_run(State(app): State<App>) -> ApiResult {
    app.w().preflight_running = true;
    app.log.info("preflight: running");
    let rep = crate::preflight::run(
        &app.http.plain,
        &app.endpoints.github_api,
        &app.endpoints.stripe_api,
        app.addr,
    )
    .await;
    for c in &rep.checks {
        app.log
            .info(format!("preflight {}: {} - {}", c.id, c.status, c.detail));
    }
    {
        let mut w = app.w();
        w.preflight = Some(rep);
        w.preflight_running = false;
    }
    ok(&app, None)
}

#[derive(Deserialize)]
struct DeploymentReq {
    environment: Environment,
    profile: String,
    portal_url: String,
    ops_url: String,
    provider_adapter: String,
    admin_email: String,
}

async fn deployment_save(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: DeploymentReq = parse(&body)?;
    let mut f = serde_json::Map::new();
    if r.profile != PROFILE_SINGLE {
        f.insert(
            "profile".into(),
            json!("Only \"Single server with Docker Compose\" is available today."),
        );
    }
    if r.provider_adapter != PROVIDER_MOCK {
        f.insert(
            "provider_adapter".into(),
            json!("Only the mock provider adapter exists today."),
        );
    }
    let portal = validate::base_url(&r.portal_url, r.environment, "Portal URL");
    let ops = validate::base_url(&r.ops_url, r.environment, "Ops console URL");
    let email = validate::email(&r.admin_email);
    if let Err(e) = &portal {
        f.insert("portal_url".into(), json!(e));
    }
    if let Err(e) = &ops {
        f.insert("ops_url".into(), json!(e));
    }
    if let Err(e) = &email {
        f.insert("admin_email".into(), json!(e));
    }
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    let d = Deployment {
        environment: r.environment,
        profile: r.profile,
        portal_url: portal.unwrap_or_default(),
        ops_url: ops.unwrap_or_default(),
        provider_adapter: r.provider_adapter,
        admin_email: email.unwrap_or_default(),
    };
    app.log.info(format!(
        "deployment saved: {} portal={} ops={}",
        d.environment.as_str(),
        d.portal_url,
        d.ops_url
    ));
    app.w().deployment = Some(d);
    ok(&app, Some("Deployment mode saved.".into()))
}

#[derive(Deserialize)]
struct IdentityReq {
    deployment_name: String,
    operator_name: String,
    support_contact: String,
    modules: Vec<String>,
}

async fn identity_save(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: IdentityReq = parse(&body)?;
    let mut f = serde_json::Map::new();
    let name = validate::text(&r.deployment_name, "Deployment name", 80);
    let op = validate::text(&r.operator_name, "Owner/operator name", 120);
    let sc = r.support_contact.trim();
    let support = if sc.contains('@') && !sc.contains("://") {
        validate::email(sc)
    } else {
        validate::base_url(sc, Environment::Production, "Support contact")
            .map_err(|_| "Support contact must be an email address or an https:// URL.".to_string())
    };
    for (k, v) in [
        ("deployment_name", &name),
        ("operator_name", &op),
        ("support_contact", &support),
    ] {
        if let Err(e) = v {
            f.insert(k.into(), json!(e));
        }
    }
    let mut modules: Vec<String> = Vec::new();
    for m in &r.modules {
        if !MODULES.contains(&m.as_str()) {
            f.insert("modules".into(), json!(format!("Unknown module {m:?}.")));
        } else if !modules.contains(m) {
            modules.push(m.clone());
        }
    }
    if modules.is_empty() && !f.contains_key("modules") {
        f.insert("modules".into(), json!("Enable at least one module."));
    }
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    modules.sort_by_key(|m| MODULES.iter().position(|x| x == m));
    app.log
        .info(format!("identity saved, modules: {}", modules.join(",")));
    app.w().identity = Some(Identity {
        deployment_name: name.unwrap_or_default(),
        operator_name: op.unwrap_or_default(),
        support_contact: support.unwrap_or_default(),
        modules,
    });
    ok(&app, Some("Product identity saved.".into()))
}

#[derive(Deserialize)]
struct BrandingReq {
    primary_color: String,
    theme: String,
    density: String,
}

async fn branding_save(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: BrandingReq = parse(&body)?;
    let mut f = serde_json::Map::new();
    let rgb = validate::hex_color(&r.primary_color);
    if let Err(e) = &rgb {
        f.insert("primary_color".into(), json!(e));
    }
    if r.theme != "light" && r.theme != "dark" {
        f.insert("theme".into(), json!("Theme must be light or dark."));
    }
    if r.density != "comfortable" && r.density != "compact" {
        f.insert(
            "density".into(),
            json!("Density must be comfortable or compact."),
        );
    }
    let Ok(rgb) = rgb else {
        return Err(ApiErr::fields(Value::Object(f)));
    };
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    let c = validate::contrast(rgb, (255, 255, 255));
    let mut w = app.w();
    let b = &mut w.branding;
    b.primary_color = format!("#{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2);
    b.contrast_vs_white = Some((c * 100.0).round() / 100.0);
    b.contrast_warning = (c < 4.5).then(|| {
        format!("Contrast against white is {c:.2}:1, below the 4.5:1 WCAG AA minimum for text. Fine for large shapes, but don't use it for text or button labels on white.")
    });
    b.theme = r.theme;
    b.density = r.density;
    b.saved = true;
    drop(w);
    ok(&app, Some("Branding saved.".into()))
}

async fn branding_upload(
    State(app): State<App>,
    Path(kind): Path<String>,
    body: Bytes,
) -> ApiResult {
    if kind != "logo" && kind != "favicon" {
        return Err(ApiErr::bad("Unknown image slot."));
    }
    let info = crate::branding::validate_image(&body).map_err(|e| {
        app.log.warn(format!("{kind} upload rejected: {e}"));
        ApiErr::bad(e)
    })?;
    app.log.info(format!(
        "{kind} accepted: {} {}x{} {} bytes",
        info.kind.mime(),
        info.width,
        info.height,
        info.bytes
    ));
    let up = Upload {
        info,
        bytes: body.to_vec(),
    };
    let mut w = app.w();
    if kind == "logo" {
        w.branding.logo = Some(up);
    } else {
        w.branding.favicon = Some(up);
    }
    drop(w);
    ok(&app, Some(format!("{kind} accepted.")))
}

async fn branding_delete(State(app): State<App>, Path(kind): Path<String>) -> ApiResult {
    let mut w = app.w();
    match kind.as_str() {
        "logo" => w.branding.logo = None,
        "favicon" => w.branding.favicon = None,
        _ => return Err(ApiErr::bad("Unknown image slot.")),
    }
    drop(w);
    ok(&app, None)
}

// ---------------------------------------------------------------------------------------------
// Step 5: domain
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct DomainReq {
    strategy: Strategy,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    label: String,
    target: String,
}

async fn domain_save(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: DomainReq = parse(&body)?;
    let mut f = serde_json::Map::new();
    let domain = match r.strategy {
        Strategy::Managed => match &app.managed_zone {
            None => {
                return Err(ApiErr {
                    status: StatusCode::BAD_REQUEST,
                    error: "Needs config: no Taro-managed DNS zone is configured for this wizard (TARO_SETUP_MANAGED_ZONE). Use your own domain instead.".into(),
                    fields: None,
                })
            }
            Some(zone) => validate::fqdn(&format!("{}.{zone}", r.label.trim())),
        },
        Strategy::Custom => validate::fqdn(&r.domain),
    };
    if let Err(e) = &domain {
        f.insert(
            if r.strategy == Strategy::Managed {
                "label"
            } else {
                "domain"
            }
            .into(),
            json!(e),
        );
    }
    let target = domain::parse_target(&r.target);
    if let Err(e) = &target {
        f.insert("target".into(), json!(e));
    }
    let (Ok(domain), Ok(target)) = (domain, target) else {
        return Err(ApiErr::fields(Value::Object(f)));
    };
    let mut w = app.w();
    let d = &mut w.domain;
    if d.domain.as_deref() != Some(domain.as_str()) || d.verify_token.is_none() {
        d.verify_token = Some(crate::secret::random_hex(16));
    }
    d.strategy = Some(r.strategy);
    d.domain = Some(domain.clone());
    d.target = Some(r.target.trim().to_ascii_lowercase());
    d.records = domain::records(
        &domain,
        &target,
        d.verify_token.as_deref().unwrap_or_default(),
    );
    d.target_parsed = Some(target);
    d.verification = None;
    drop(w);
    app.log.info(format!("domain set: {domain}"));
    ok(
        &app,
        Some("DNS records generated. Create them at your DNS provider, then verify.".into()),
    )
}

async fn domain_verify(State(app): State<App>) -> ApiResult {
    let (domain, target, token) = {
        let mut w = app.w();
        let d = &mut w.domain;
        match (&d.domain, &d.target_parsed, &d.verify_token) {
            (Some(a), Some(b), Some(c)) => {
                d.verifying = true;
                (a.clone(), b.clone(), c.clone())
            }
            _ => return Err(ApiErr::bad("Save a domain first.")),
        }
    };
    let mut checks = domain::verify_dns(&app.dns, &domain, &target, &token).await;
    let dns_ok = checks.iter().all(|c| c.ok);
    let https_ok = if dns_ok {
        let url = app.endpoints.domain_health_url(&domain);
        let c = domain::check_health(&app.http.plain, &url, "HTTPS health (/healthz)").await;
        let okk = c.ok;
        checks.push(c);
        okk
    } else {
        checks.push(domain::Check {
            name: "HTTPS health (/healthz)".into(),
            ok: false,
            detail: "not checked until DNS verifies".into(),
            hint: None,
        });
        false
    };
    for c in &checks {
        app.log.info(format!(
            "domain check {}: {} {}",
            c.name,
            if c.ok { "ok" } else { "FAIL" },
            c.detail
        ));
    }
    {
        let mut w = app.w();
        w.domain.verifying = false;
        w.domain.verification = Some(domain::DomainVerification {
            dns_ok,
            https_ok,
            checks,
            at: crate::util::now_secs(),
        });
    }
    ok(&app, None)
}

// ---------------------------------------------------------------------------------------------
// Step 6: billing
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct KeyReq {
    key: String,
    #[serde(default)]
    live_confirmed: bool,
    #[serde(default)]
    live_typed: String,
}

async fn billing_key(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: KeyReq = parse(&body)?;
    let key = Secret::new(r.key.trim());
    zeroize::Zeroize::zeroize(&mut { r.key });
    app.log.register_secret(&key);
    let mode = validate::stripe_key_mode(key.expose(), r.live_confirmed, &r.live_typed)
        .map_err(|e| ApiErr::fields(json!({ "key": e })))?;
    let client =
        crate::stripe::Stripe::new(&app.endpoints.stripe_api, app.http.api.clone(), key.clone());
    app.log.info(format!(
        "stripe: validating {mode:?} key with GET /v1/account"
    ));
    match client.account().await {
        Ok(acct) => {
            app.log
                .info(format!("stripe: key valid for account {}", acct.id));
            let mut w = app.w();
            let b = &mut w.billing;
            b.key = Some(key);
            b.key_present = true;
            b.mode = Some(mode);
            b.account = Some(acct);
            b.key_error = None;
            b.smoke = None;
            drop(w);
            ok(&app, Some("Stripe key validated.".into()))
        }
        Err(e) => {
            app.log.warn(format!("stripe: key validation failed: {e}"));
            let mut w = app.w();
            w.billing.key = None;
            w.billing.key_present = false;
            w.billing.account = None;
            w.billing.key_error = Some(e.to_string());
            drop(w);
            Err(ApiErr::upstream(e.to_string()))
        }
    }
}

#[derive(Deserialize)]
struct SecretReq {
    secret: String,
}

async fn billing_webhook_secret(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: SecretReq = parse(&body)?;
    let s = Secret::new(r.secret.trim());
    zeroize::Zeroize::zeroize(&mut { r.secret });
    app.log.register_secret(&s);
    validate::webhook_secret(s.expose())
        .map_err(|e| ApiErr::fields(json!({ "webhook_secret": e })))?;
    let mut w = app.w();
    w.billing.webhook_secret = Some(s);
    w.billing.webhook_secret_present = true;
    w.billing.webhook_secret_source = Some("entered".into());
    drop(w);
    app.log
        .info("stripe: webhook signing secret stored in memory");
    ok(
        &app,
        Some("Webhook signing secret stored (in memory only).".into()),
    )
}

fn webhook_url(app: &AppState) -> Result<String, ApiErr> {
    app.w()
        .deployment
        .as_ref()
        .map(|d| format!("{}/webhooks/stripe", d.portal_url))
        .ok_or_else(|| ApiErr::bad("Save the Deployment mode step first (the webhook URL is <portal>/webhooks/stripe)."))
}

async fn billing_webhook_check(State(app): State<App>) -> ApiResult {
    let url = webhook_url(&app)?;
    let client = app
        .stripe_client()
        .ok_or_else(|| ApiErr::bad("Validate the Stripe key first."))?;
    let list = client
        .list_webhooks()
        .await
        .map_err(|e| ApiErr::upstream(e.to_string()))?;
    let found = list.into_iter().find(|h| h.url == url);
    let mut w = app.w();
    match found {
        Some(h) => {
            w.billing.webhook_missing_events =
                h.missing_events().iter().map(|s| s.to_string()).collect();
            w.billing.webhook_note = Some(if w.billing.webhook_missing_events.is_empty() {
                format!(
                    "Endpoint {} exists with all required events ({}).",
                    h.id, h.status
                )
            } else {
                format!(
                    "Endpoint {} exists but is missing events; add them in the Stripe dashboard.",
                    h.id
                )
            });
            w.billing.webhook = Some(h);
        }
        None => {
            w.billing.webhook = None;
            w.billing.webhook_missing_events = vec![];
            w.billing.webhook_note = Some(format!("No webhook endpoint for {url} exists yet."));
        }
    }
    drop(w);
    ok(&app, None)
}

#[derive(Deserialize)]
struct ConfirmReq {
    #[serde(default)]
    confirm: bool,
}

async fn billing_webhook_create(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: ConfirmReq = parse(&body)?;
    if !r.confirm {
        return Err(ApiErr::bad(
            "Creating the webhook endpoint needs explicit confirmation.",
        ));
    }
    let url = webhook_url(&app)?;
    let client = app
        .stripe_client()
        .ok_or_else(|| ApiErr::bad("Validate the Stripe key first."))?;
    let list = client
        .list_webhooks()
        .await
        .map_err(|e| ApiErr::upstream(e.to_string()))?;
    if let Some(h) = list.into_iter().find(|h| h.url == url) {
        let mut w = app.w();
        w.billing.webhook_missing_events =
            h.missing_events().iter().map(|s| s.to_string()).collect();
        w.billing.webhook_note = Some(format!(
            "Endpoint {} already exists; not creating a duplicate. Stripe only reveals its signing secret in the dashboard: paste it above.",
            h.id
        ));
        w.billing.webhook = Some(h);
        drop(w);
        return ok(&app, None);
    }
    app.log
        .info(format!("stripe: creating webhook endpoint for {url}"));
    let (h, secret) = client
        .create_webhook(&url)
        .await
        .map_err(|e| ApiErr::upstream(e.to_string()))?;
    let mut w = app.w();
    w.billing.webhook_missing_events = h.missing_events().iter().map(|s| s.to_string()).collect();
    if let Some(s) = secret {
        app.log.register_secret(&s);
        w.billing.webhook_secret = Some(s);
        w.billing.webhook_secret_present = true;
        w.billing.webhook_secret_source = Some("created".into());
    }
    w.billing.webhook_note = Some(format!(
        "Created endpoint {}. Its signing secret was captured in memory and will be stored as a GitHub Actions secret.",
        h.id
    ));
    w.billing.webhook = Some(h);
    drop(w);
    ok(&app, Some("Webhook endpoint created.".into()))
}

async fn billing_smoke(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: ConfirmReq = parse(&body)?;
    if !r.confirm {
        return Err(ApiErr::bad("The smoke test needs explicit confirmation."));
    }
    let (mode, portal) = {
        let w = app.w();
        (
            w.billing.mode,
            w.deployment.as_ref().map(|d| d.portal_url.clone()),
        )
    };
    if mode != Some(StripeMode::Test) {
        return Err(ApiErr::bad(
            "The billing smoke test runs only with a test-mode key.",
        ));
    }
    let portal = portal.ok_or_else(|| ApiErr::bad("Save the Deployment mode step first."))?;
    let client = app
        .stripe_client()
        .ok_or_else(|| ApiErr::bad("Validate the Stripe key first."))?;
    let now = crate::util::now_secs();
    let result = match client.create_smoke_session(&portal).await {
        Err(e) => crate::state::SmokeResult {
            ok: false,
            session_id: None,
            session_url: None,
            expired: false,
            detail: format!("Creating the Checkout Session failed: {e}"),
            at: now,
        },
        Ok(s) => {
            let exp = client.expire_session(&s.id).await;
            let expired = matches!(&exp, Ok(x) if x.status.as_deref() == Some("expired"));
            let valid = s.id.starts_with("cs_test_") && s.url.is_some() && !s.livemode;
            let detail = match (&exp, valid) {
                (_, false) if s.livemode => "Stripe returned a LIVE session; it was expired immediately.".to_string(),
                (_, false) => "Stripe's response was missing the session id or URL.".to_string(),
                (Ok(_), true) if expired => "Stripe created a test Checkout Session (1 x 100 minor units) and it was expired again.".to_string(),
                (Ok(x), true) => format!("Session created; expire returned status {:?}.", x.status),
                (Err(e), true) => format!("Session created, but expiring it failed: {e}. It will expire by itself after 24 h."),
            };
            crate::state::SmokeResult {
                ok: valid && expired,
                session_id: Some(s.id),
                session_url: s.url,
                expired,
                detail,
                at: now,
            }
        }
    };
    app.log.info(format!(
        "stripe smoke test: ok={} {}",
        result.ok, result.detail
    ));
    app.w().billing.smoke = Some(result);
    ok(&app, None)
}

// ---------------------------------------------------------------------------------------------
// Step 7: GitHub
// ---------------------------------------------------------------------------------------------

async fn adopt_token(app: &AppState, token: Secret, method: &str) -> Result<String, ApiErr> {
    app.log.register_secret(&token);
    let gh = crate::github::GitHub::new(
        &app.endpoints.github_api,
        app.http.api.clone(),
        app.http.plain.clone(),
        token.clone(),
    );
    match gh.user().await {
        Ok(u) => {
            let login = u.login.clone();
            app.log
                .info(format!("github: authenticated as {login} via {method}"));
            let mut w = app.w();
            w.github.token = Some(token);
            w.github.user = Some(u);
            w.github.auth_method = Some(method.into());
            w.github.auth_error = None;
            w.github.orgs = None;
            Ok(login)
        }
        Err(e) => {
            app.log.warn(format!("github: token rejected: {e}"));
            app.w().github.auth_error = Some(e.to_string());
            Err(ApiErr::upstream(match e.status {
                Some(401) => {
                    "GitHub rejected the token (401). It may be mistyped, expired or revoked."
                        .into()
                }
                _ => e.to_string(),
            }))
        }
    }
}

#[derive(Deserialize)]
struct PatReq {
    token: String,
    #[serde(default)]
    remember: bool,
}

async fn github_pat(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: PatReq = parse(&body)?;
    let token = Secret::new(r.token.trim());
    zeroize::Zeroize::zeroize(&mut { r.token });
    app.log.register_secret(&token);
    if token.is_empty()
        || token
            .expose()
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ApiErr::fields(
            json!({ "token": "Paste the token exactly as GitHub showed it." }),
        ));
    }
    let login = adopt_token(&app, token.clone(), "pat").await?;
    let mut msg = format!("Authenticated as {login}.");
    if r.remember {
        msg.push(' ');
        msg.push_str(&remember(&app, &token));
    }
    ok(&app, Some(msg))
}

fn remember(app: &AppState, token: &Secret) -> String {
    match &app.remember_dir {
        Some(dir) if crate::remember::supported() => match crate::remember::save(dir, token) {
            Ok(()) => {
                app.w().github.remembered_available = true;
                app.log.info("github: token stored with DPAPI");
                "The token is remembered on this PC (DPAPI, current Windows user).".into()
            }
            Err(e) => {
                app.log.warn(format!("github: remembering failed: {e}"));
                format!("It could not be remembered: {e}")
            }
        },
        _ => "Remembering is not available on this OS; the token stays in memory only.".into(),
    }
}

#[derive(Deserialize)]
struct DeviceReq {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    remember: bool,
}

async fn github_device_start(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: DeviceReq = parse(&body)?;
    let client_id = r
        .client_id
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| app.github_client_id.clone())
        .ok_or_else(|| ApiErr::bad("Needs config: no OAuth client id. Set TARO_SETUP_GITHUB_CLIENT_ID or enter one, or use a fine-grained token instead."))?;
    if client_id.len() > 64
        || !client_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err(ApiErr::fields(
            json!({ "client_id": "That does not look like a GitHub OAuth client id." }),
        ));
    }
    let flow =
        crate::github::DeviceFlow::new(&app.endpoints.github_web, app.http.api.clone(), &client_id);
    let code = flow.start().await.map_err(ApiErr::upstream)?;
    app.log.register_secret(&code.device_code);
    let cancel = Arc::new(tokio::sync::Notify::new());
    let generation = {
        let mut w = app.w();
        let d = &mut w.github.device;
        if let Some(c) = d.cancel.take() {
            c.notify_one();
        }
        d.generation += 1;
        d.phase = "waiting".into();
        d.user_code = Some(code.user_code.clone());
        d.verification_uri = Some(code.verification_uri.clone());
        d.interval = Some(code.interval);
        d.expires_at = Some(crate::util::now_secs() + code.expires_in);
        d.error = None;
        d.cancel = Some(cancel.clone());
        d.generation
    };
    app.log.info(format!(
        "github: device flow started, code {}",
        code.user_code
    ));
    let app2 = app.clone();
    let remember_it = r.remember;
    tokio::spawn(async move {
        let unit = app2.timings.device_unit;
        let a3 = app2.clone();
        let res = tokio::select! {
            r = flow.poll_until(&code, unit, move |i| {
                let mut w = a3.w();
                if w.github.device.generation == generation { w.github.device.interval = Some(i); }
            }) => r,
            _ = cancel.notified() => return,
        };
        let current = app2.w().github.device.generation == generation;
        if !current {
            return;
        }
        match res {
            Ok(out) => match adopt_token(&app2, out.token.clone(), "device").await {
                Ok(_) => {
                    let mut w = app2.w();
                    w.github.device.phase = "done".into();
                    w.github.device.user_code = None;
                    drop(w);
                    if remember_it {
                        let m = remember(&app2, &out.token);
                        app2.log.info(m);
                    }
                }
                Err(e) => {
                    let mut w = app2.w();
                    w.github.device.phase = "error".into();
                    w.github.device.error = Some(e.error);
                }
            },
            Err(e) => {
                app2.log.warn(format!("github: device flow ended: {e}"));
                let mut w = app2.w();
                w.github.device.phase = "error".into();
                w.github.device.error = Some(e);
            }
        }
    });
    ok(&app, None)
}

async fn github_device_cancel(State(app): State<App>) -> ApiResult {
    {
        let mut w = app.w();
        let d = &mut w.github.device;
        if let Some(c) = d.cancel.take() {
            c.notify_one();
        }
        d.generation += 1;
        d.phase = "idle".into();
        d.user_code = None;
        d.verification_uri = None;
        d.error = None;
    }
    ok(&app, None)
}

async fn github_remembered_use(State(app): State<App>) -> ApiResult {
    let dir = app
        .remember_dir
        .clone()
        .ok_or_else(|| ApiErr::bad("Not available on this OS."))?;
    let token = crate::remember::load(&dir).map_err(ApiErr::bad)?;
    let login = adopt_token(&app, token, "remembered").await?;
    ok(
        &app,
        Some(format!(
            "Authenticated as {login} with the remembered token."
        )),
    )
}

async fn github_remembered_forget(State(app): State<App>) -> ApiResult {
    if let Some(dir) = &app.remember_dir {
        crate::remember::forget(dir).map_err(ApiErr::bad)?;
    }
    app.w().github.remembered_available = false;
    app.log.info("github: remembered token deleted");
    ok(
        &app,
        Some("The remembered token was deleted from this PC.".into()),
    )
}

async fn github_logout(State(app): State<App>) -> ApiResult {
    {
        let mut w = app.w();
        w.github.token = None; // zeroized on drop
        w.github.user = None;
        w.github.auth_method = None;
        w.github.orgs = None;
    }
    app.log
        .info("github: signed out (token dropped from memory)");
    ok(
        &app,
        Some("Signed out. The token was removed from memory.".into()),
    )
}

async fn github_orgs(State(app): State<App>) -> ApiResult {
    let gh = app
        .github_client()
        .ok_or_else(|| ApiErr::bad("Sign in to GitHub first."))?;
    let orgs = gh
        .orgs()
        .await
        .map_err(|e| ApiErr::upstream(e.to_string()))?;
    app.w().github.orgs = Some(orgs);
    ok(&app, None)
}

#[derive(Deserialize)]
struct PlanReq {
    owner: String,
    repo: String,
    template: String,
    #[serde(default)]
    confirm: bool,
}

async fn github_plan(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: PlanReq = parse(&body)?;
    let mut w = app.w();
    if w.github.bootstrap_running {
        return Err(ApiErr::conflict("The bootstrap is already running."));
    }
    let input =
        crate::bootstrap::gather(&mut w, &r.owner, &r.repo, &r.template).map_err(ApiErr::bad)?;
    w.github.plan = Some(crate::bootstrap::plan(&input));
    w.github.owner = Some(input.owner.clone());
    w.github.repo_name = Some(input.name.clone());
    w.github.template = format!("{}/{}", input.template_owner, input.template_repo);
    w.github.bootstrap_error = None;
    drop(w);
    ok(&app, None)
}

async fn github_bootstrap(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: PlanReq = parse(&body)?;
    if !r.confirm {
        return Err(ApiErr::bad(
            "Creating the repository needs explicit confirmation.",
        ));
    }
    let gh = app
        .github_client()
        .ok_or_else(|| ApiErr::bad("Sign in to GitHub first."))?;
    let input = {
        let mut w = app.w();
        if w.github.bootstrap_running {
            return Err(ApiErr::conflict("The bootstrap is already running."));
        }
        let input = crate::bootstrap::gather(&mut w, &r.owner, &r.repo, &r.template)
            .map_err(ApiErr::bad)?;
        let plan = crate::bootstrap::plan(&input);
        let shown: Option<Vec<String>> = w
            .github
            .plan
            .as_ref()
            .map(|p| p.iter().map(|a| a.label.clone()).collect());
        let now: Vec<String> = plan.iter().map(|a| a.label.clone()).collect();
        if shown.as_ref() != Some(&now) {
            w.github.plan = Some(plan);
            return Err(ApiErr::conflict("The plan changed since it was shown (inputs were edited). Review the updated plan and confirm again."));
        }
        w.github.plan = Some(plan);
        w.github.bootstrap_running = true;
        w.github.bootstrap_done = false;
        w.github.bootstrap_error = None;
        input
    };
    app.log.info(format!(
        "github: bootstrap confirmed for {}/{}",
        input.owner, input.name
    ));
    tokio::spawn(crate::bootstrap::run(app.clone(), gh, input));
    ok(&app, Some("Bootstrap started.".into()))
}

#[derive(Deserialize)]
struct DeleteReq {
    confirm_name: String,
}

async fn github_delete_repo(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: DeleteReq = parse(&body)?;
    let gh = app
        .github_client()
        .ok_or_else(|| ApiErr::bad("Sign in to GitHub first."))?;
    let (o, n, full) = {
        let w = app.w();
        let g = &w.github;
        if g.bootstrap_running || w.release.running {
            return Err(ApiErr::conflict(
                "Wait until the running task has finished.",
            ));
        }
        let repo = match (&g.repo, g.created_by_wizard, g.deleted) {
            (Some(r), true, false) => r.clone(),
            _ => {
                return Err(ApiErr::bad(
                    "Only a repository created by this wizard session can be deleted here.",
                ))
            }
        };
        if r.confirm_name.trim() != repo.full_name {
            return Err(ApiErr::fields(
                json!({ "confirm_name": format!("Type {} exactly to confirm.", repo.full_name) }),
            ));
        }
        let (o, n) = repo
            .full_name
            .split_once('/')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_default();
        (o, n, repo.full_name)
    };
    app.log.warn(format!(
        "github: deleting repository {full} on operator request"
    ));
    match gh.delete_repo(&o, &n).await {
        Ok(()) => {
            let mut w = app.w();
            w.github.deleted = true;
            w.github.repo = None;
            w.github.created_by_wizard = false;
            w.github.bootstrap_done = false;
            w.github.plan = None;
            w.release = crate::state::ReleaseState { phase: "idle".into(), ..Default::default() };
            drop(w);
            ok(&app, Some(format!("Deleted {full}.")))
        }
        Err(e) if matches!(e.status, Some(403) | Some(404)) => Err(ApiErr {
            status: StatusCode::FORBIDDEN,
            error: format!(
                "GitHub refused to delete {full} ({e}). Deleting needs the delete_repo scope (classic token / OAuth) or Administration: read and write (fine-grained). Delete it yourself under Settings > General > Danger Zone on github.com."
            ),
            fields: None,
        }),
        Err(e) => Err(ApiErr::upstream(e.to_string())),
    }
}

// ---------------------------------------------------------------------------------------------
// Step 8-9
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ReleaseReq {
    version: String,
}

async fn release_start(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: ReleaseReq = parse(&body)?;
    let version =
        validate::version(&r.version).map_err(|e| ApiErr::fields(json!({ "version": e })))?;
    let gh = app
        .github_client()
        .ok_or_else(|| ApiErr::bad("Sign in to GitHub first."))?;
    let input = {
        let mut w = app.w();
        if w.release.running {
            return Err(ApiErr::conflict(
                "A release run is already being followed; wait for it to finish.",
            ));
        }
        if !w.github.bootstrap_done {
            return Err(ApiErr::bad("Finish the GitHub bootstrap first."));
        }
        let repo = w
            .github
            .repo
            .clone()
            .ok_or_else(|| ApiErr::bad("No repository."))?;
        let (o, n) = repo
            .full_name
            .split_once('/')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or_default();
        let rel = &mut w.release;
        rel.running = true;
        rel.version = Some(version.clone());
        rel.phase = "Starting".into();
        rel.run = None;
        rel.jobs.clear();
        rel.log_tail = None;
        rel.release = None;
        rel.error = None;
        rel.dispatched_at = None;
        rel.attempts += 1;
        crate::release::Input {
            owner: o,
            repo: n,
            git_ref: repo.default_branch.clone().unwrap_or_else(|| "main".into()),
            version,
        }
    };
    tokio::spawn(crate::release::run(app.clone(), gh, input));
    ok(&app, Some("Release started.".into()))
}

async fn verify_run(State(app): State<App>) -> ApiResult {
    use crate::verify::vcheck;
    let (portal, domain) = {
        let mut w = app.w();
        w.verification_running = true;
        (
            w.deployment.as_ref().map(|d| d.portal_url.clone()),
            w.domain.domain.clone(),
        )
    };
    let mut checks = match &portal {
        Some(p) => crate::verify::portal_checks(&app.http.plain, p).await,
        None => vec![vcheck(
            "healthz",
            "Portal checks",
            "skipped",
            "No portal URL saved (Deployment mode step).",
            None,
        )],
    };
    checks.push(match app.stripe_client() {
        Some(s) => match s.account().await {
            Ok(a) => vcheck(
                "stripe",
                "Stripe key re-check",
                "ok",
                format!("Key valid for {}", a.id),
                None,
            ),
            Err(e) => vcheck(
                "stripe",
                "Stripe key re-check",
                "fail",
                e.to_string(),
                Some("Re-enter the key in the Billing step."),
            ),
        },
        None => vcheck(
            "stripe",
            "Stripe key re-check",
            "skipped",
            "No Stripe key in this session.",
            None,
        ),
    });
    checks.push(match &domain {
        Some(d) => {
            let c = domain::check_health(
                &app.http.plain,
                &app.endpoints.domain_health_url(d),
                "Domain TLS",
            )
            .await;
            vcheck(
                "tls",
                "Domain TLS",
                if c.ok { "ok" } else { "fail" },
                if c.ok {
                    format!("{d}: certificate trusted, /healthz 200")
                } else {
                    c.detail
                },
                c.hint.as_deref(),
            )
        }
        None => vcheck("tls", "Domain TLS", "skipped", "No domain saved.", None),
    });
    for c in &checks {
        app.log
            .info(format!("verify {}: {} {}", c.id, c.status, c.detail));
    }
    {
        let mut w = app.w();
        w.verification_running = false;
        w.verification = Some(crate::verify::VerificationReport {
            portal_url: portal,
            checks,
            at: crate::util::now_secs(),
        });
    }
    ok(&app, None)
}

fn download(body: String, ctype: &'static str, name: &'static str) -> Response {
    let mut r = body.into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .unwrap_or(HeaderValue::from_static("attachment")),
    );
    r
}

async fn summary_json(State(app): State<App>) -> Response {
    let v = crate::summary::summary_json(&app.w());
    download(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
        "application/json",
        "taro-deployment-summary.json",
    )
}

async fn summary_md(State(app): State<App>) -> Response {
    let md = crate::summary::summary_markdown(&app.w());
    download(
        md,
        "text/markdown; charset=utf-8",
        "taro-deployment-summary.md",
    )
}

async fn diagnostics(State(app): State<App>) -> Response {
    download(
        app.log.export_text(),
        "text/plain; charset=utf-8",
        "taro-setup-diagnostics.txt",
    )
}

async fn shutdown(State(app): State<App>) -> ApiResult {
    app.log.info("shutdown requested from the UI");
    app.shutdown.notify_one();
    Ok(Json(
        json!({ "ok": true, "message": "The wizard is shutting down. You can close this tab." }),
    ))
}
