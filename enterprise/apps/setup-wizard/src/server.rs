//! Local HTTP server: request guard, embedded UI, JSON API.
//!
//! Every request must carry `Host: 127.0.0.1:<port>` (DNS-rebinding guard); an `Origin`
//! header, when present, must be `http://127.0.0.1:<port>` and is mandatory for non-GET API
//! calls; `Sec-Fetch-Site`, when present, must be `same-origin` or `none`. API calls need the
//! one-time access token in `X-Setup-Token`; the page itself needs it as `?t=`.

use crate::branding::Slot;
use crate::secret::{ct_eq, Secret};
use crate::state::{field_errors, placeholder, AppState, Identity, Upload};
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rm_brand::{ClientUpdates, Color, Theme, Tokens};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");

/// The shared console look of the Enterprise pages (the relay serves the same files).
fn common_file(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "console.css" => (
            include_str!("../../../packages/console/console.css"),
            "text/css; charset=utf-8",
        ),
        "console.js" => (
            include_str!("../../../packages/console/console.js"),
            "text/javascript; charset=utf-8",
        ),
        _ => return None,
    })
}

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
        json!({ "ok": true, "message": message, "state": w.view() }),
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
        .route(
            "/api/branding/{slot}",
            post(image_upload).delete(image_delete),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024));
    let api = Router::new()
        .route("/api/state", get(get_state))
        .route("/api/preflight/run", post(preflight_run))
        .route("/api/identity", post(identity_save))
        .route("/api/provider/import", post(provider_import))
        .route("/api/branding", post(branding_save))
        .route("/api/branding/tokens", post(branding_tokens))
        .route("/api/relay", post(relay_save))
        .route("/api/relay/key/generate", post(relay_key_generate))
        .route("/api/signing/connect", post(signing_connect))
        .route("/api/signing/disconnect", post(signing_disconnect))
        .route("/api/signing/start", post(signing_start))
        .route("/api/components/discover", post(components_discover))
        .route("/api/components/select", post(components_select))
        .route("/api/components/platforms", post(components_platforms))
        .route("/api/build/start", post(build_start))
        .route("/api/build/cancel", post(build_cancel))
        .route("/api/validate/run", post(validate_run))
        .route("/api/output/open", post(output_open))
        .route("/api/summary.json", get(summary_json))
        .route("/api/summary.md", get(summary_md))
        .route("/api/diagnostics.txt", get(diagnostics))
        .route("/api/shutdown", post(shutdown))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .merge(uploads)
        .layer(middleware::from_fn_with_state(app.clone(), scrub_responses));
    // Not scrubbed: the explicit "reveal" of the relay key (the only response that may carry
    // it) and the uploaded images (binary, no secrets).
    let unscrubbed = Router::new()
        .route("/api/relay/key/reveal", post(relay_key_reveal))
        .route("/api/branding/{slot}/image", get(image_get))
        .layer(DefaultBodyLimit::max(1024));
    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_js))
        .route("/assets/app.css", get(app_css))
        .route("/common/{file}", get(common))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .merge(api)
        .merge(unscrubbed)
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

async fn common(Path(file): Path<String>) -> Response {
    match common_file(&file) {
        None => deny(StatusCode::NOT_FOUND, "Not found."),
        Some((body, ctype)) => {
            let mut r = body.into_response();
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
            r
        }
    }
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
// Steps
// ---------------------------------------------------------------------------------------------

async fn get_state(State(app): State<App>) -> ApiResult {
    ok(&app, None)
}

/// Inputs cannot change while a build reads them.
fn not_building(app: &AppState) -> Result<(), ApiErr> {
    if app.w().build.running {
        return Err(ApiErr::conflict(
            "A build is running. Wait for it to finish or cancel it first.",
        ));
    }
    Ok(())
}

async fn preflight_run(State(app): State<App>) -> ApiResult {
    let out = {
        let mut w = app.w();
        w.preflight_running = true;
        std::path::PathBuf::from(&w.build.output_dir)
    };
    app.log.info("preflight: running");
    let rep = crate::preflight::run(&app.http, &app.endpoints, app.addr, &out).await;
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

fn opt(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[derive(Deserialize)]
struct IdentityReq {
    provider_id: String,
    name: String,
    short_name: String,
    vendor: String,
    #[serde(default)]
    support_url: Option<String>,
    #[serde(default)]
    support_email: Option<String>,
}

async fn identity_save(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: IdentityReq = parse(&body)?;
    let i = Identity {
        provider_id: r.provider_id.trim().to_string(),
        name: r.name.trim().to_string(),
        short_name: r.short_name.trim().to_string(),
        vendor: r.vendor.trim().to_string(),
        support_url: opt(&r.support_url),
        support_email: opt(&r.support_email),
    };
    let mut p = placeholder();
    p.provider_id = i.provider_id.clone();
    p.product.name = i.name.clone();
    p.product.short_name = i.short_name.clone();
    p.product.vendor = i.vendor.clone();
    p.product.support_url = i.support_url.clone();
    p.product.support_email = i.support_email.clone();
    let mut f = field_errors(&p, &["provider_id", "product."]);
    if !f.contains_key("product.short_name") && crate::build::windows_reserved(&i.short_name) {
        f.insert(
            "product.short_name".into(),
            json!("Windows reserves this name (CON, NUL, COM1 …) or it ends with a dot."),
        );
    }
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    app.log.info(format!(
        "identity saved: {} ({}), short name {}",
        i.name, i.provider_id, i.short_name
    ));
    let mut w = app.w();
    w.identity = Some(i);
    w.rev += 1;
    drop(w);
    ok(&app, Some("Product identity saved.".into()))
}

#[derive(Deserialize)]
struct ImportReq {
    json: String,
}

/// Fill the steps from an existing provider.json (a rebuild for a newer release keeps the
/// same identity, colour and, above all, the same relay key).
async fn provider_import(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: ImportReq = parse(&body)?;
    let p = rm_brand::Provider::from_json(&r.json).map_err(ApiErr::bad)?;
    let key = Secret::new(p.relay.key.clone());
    zeroize::Zeroize::zeroize(&mut { r.json });
    app.set_relay_key(key, "imported");
    let c = Color::parse(&p.branding.primary_color).map_err(ApiErr::bad)?;
    {
        let mut w = app.w();
        w.identity = Some(Identity {
            provider_id: p.provider_id.clone(),
            name: p.product.name.clone(),
            short_name: p.product.short_name.clone(),
            vendor: p.product.vendor.clone(),
            support_url: p.product.support_url.clone(),
            support_email: p.product.support_email.clone(),
        });
        w.branding.primary_color = c.hex();
        w.branding.theme = p.branding.theme;
        w.branding.saved = true;
        w.relay.host = p.relay.host.clone();
        w.relay.port = p.relay.port;
        w.relay.client_updates = p.updates.clients;
        w.relay.portal = p.account.as_ref().map(|a| a.portal.clone());
        w.relay.saved = true;
        w.rev += 1;
    }
    app.log.info(format!(
        "provider.json of {} imported (relay key kept)",
        p.provider_id
    ));
    let imgs = [p.branding.logo.is_some(), p.branding.icon.is_some()];
    ok(
        &app,
        Some(format!(
            "Imported {}: identity, colour, relay endpoint and the same admission key.{}",
            p.provider_id,
            if imgs.iter().any(|x| *x) {
                " Upload logo.png / icon.png again (they are separate files)."
            } else {
                ""
            }
        )),
    )
}

#[derive(Deserialize)]
struct TokensReq {
    color: String,
}

/// rm-brand's derived tokens for a colour (the live preview uses exactly these).
async fn branding_tokens(body: Bytes) -> ApiResult {
    let r: TokensReq = parse(&body)?;
    let c = Color::parse(&r.color).map_err(|e| ApiErr::fields(json!({ "primary_color": e })))?;
    let t = Tokens::derive(c);
    Ok(Json(json!({
        "ok": true,
        "color": c.hex(),
        "contrast": crate::state::contrast(&t),
        "tokens": t,
    })))
}

#[derive(Deserialize)]
struct BrandingReq {
    primary_color: String,
    theme: Theme,
}

async fn branding_save(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: BrandingReq = parse(&body)?;
    let c = Color::parse(&r.primary_color)
        .map_err(|e| ApiErr::fields(json!({ "primary_color": e })))?;
    let mut p = placeholder();
    p.branding.primary_color = c.hex();
    p.branding.theme = r.theme;
    let f = field_errors(&p.normalized(), &["branding."]);
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    {
        let mut w = app.w();
        if w.branding.primary_color != c.hex() || w.branding.theme != r.theme || !w.branding.saved {
            w.rev += 1;
        }
        w.branding.primary_color = c.hex();
        w.branding.theme = r.theme;
        w.branding.saved = true;
    }
    app.log.info(format!("branding saved: {}", c.hex()));
    ok(&app, None)
}

async fn image_upload(State(app): State<App>, Path(slot): Path<String>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let slot = Slot::parse(&slot).ok_or_else(|| ApiErr::bad("Unknown image slot."))?;
    let info = crate::branding::validate_provider_png(&body, slot).map_err(|e| {
        app.log
            .warn(format!("{} upload rejected: {e}", slot.name()));
        ApiErr::bad(e)
    })?;
    app.log.info(format!(
        "{} accepted: {}x{} {} bytes",
        slot.file(),
        info.width,
        info.height,
        info.bytes
    ));
    let up = Upload {
        info,
        sha256: taro_update::sha256_hex(&body),
        bytes: body.to_vec(),
    };
    let mut w = app.w();
    match slot {
        Slot::Logo => w.branding.logo = Some(up),
        Slot::Icon => w.branding.icon = Some(up),
    }
    w.rev += 1;
    drop(w);
    ok(&app, Some(format!("{} accepted.", slot.file())))
}

async fn image_delete(State(app): State<App>, Path(slot): Path<String>) -> ApiResult {
    not_building(&app)?;
    let slot = Slot::parse(&slot).ok_or_else(|| ApiErr::bad("Unknown image slot."))?;
    let mut w = app.w();
    match slot {
        Slot::Logo => w.branding.logo = None,
        Slot::Icon => w.branding.icon = None,
    }
    w.rev += 1;
    drop(w);
    ok(&app, None)
}

async fn image_get(State(app): State<App>, Path(slot): Path<String>) -> Response {
    let Some(slot) = Slot::parse(&slot) else {
        return not_found().await;
    };
    let bytes = {
        let w = app.w();
        match slot {
            Slot::Logo => w.branding.logo.as_ref().map(|u| u.bytes.clone()),
            Slot::Icon => w.branding.icon.as_ref().map(|u| u.bytes.clone()),
        }
    };
    match bytes {
        None => not_found().await,
        Some(b) => {
            let mut r = b.into_response();
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
            r
        }
    }
}

#[derive(Deserialize)]
struct RelayReq {
    host: String,
    port: Value,
    /// a key the provider already uses (a rebuild); none: keep the current one
    #[serde(default)]
    key: Option<String>,
    client_updates: ClientUpdates,
    /// the provider's portal (sign-in and machine management); empty: none
    #[serde(default)]
    portal: Option<String>,
}

async fn relay_save(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: RelayReq = parse(&body)?;
    let host = r.host.trim().trim_end_matches('.').to_ascii_lowercase();
    let port = match &r.port {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    };
    let mut f = serde_json::Map::new();
    let port = match port {
        Some(p) if (1..=65535).contains(&p) => p as u16,
        _ => {
            f.insert("relay.port".into(), json!("1-65535"));
            0
        }
    };
    let entered = r
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(Secret::new);
    zeroize::Zeroize::zeroize(&mut { r.key });
    let portal = r
        .portal
        .as_deref()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());
    let mut p = placeholder();
    p.relay.host = host.clone();
    p.account = portal.clone().map(|portal| rm_brand::Account { portal });
    if port != 0 {
        p.relay.port = port;
    }
    if let Some(k) = &entered {
        app.log.register_secret(k);
        p.relay.key = k.expose().to_string();
    }
    for (k, v) in field_errors(&p, &["relay.", "account."]) {
        f.entry(k).or_insert(v);
    }
    let has_key = entered.is_some() || app.w().relay.key.is_some();
    if !has_key && !f.contains_key("relay.key") {
        f.insert(
            "relay.key".into(),
            json!("Generate a key, or enter the one your relay already uses."),
        );
    }
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    if let Some(k) = entered {
        let same = app
            .w()
            .relay
            .key
            .as_ref()
            .is_some_and(|old| ct_eq(old.expose(), k.expose()));
        if !same {
            app.set_relay_key(k, "entered");
        }
    }
    {
        let mut w = app.w();
        w.relay.host = host.clone();
        w.relay.port = port;
        w.relay.client_updates = r.client_updates;
        w.relay.portal = portal;
        w.relay.saved = true;
        w.rev += 1;
    }
    app.log.info(format!("relay endpoint saved: {host}:{port}"));
    ok(&app, Some("Relay endpoint saved.".into()))
}

async fn relay_key_generate(State(app): State<App>) -> ApiResult {
    not_building(&app)?;
    let k = crate::secret::relay_key(crate::state::KEY_LEN);
    app.set_relay_key(k, "generated");
    app.w().rev += 1;
    app.log.info("relay key: a new one was generated");
    ok(
        &app,
        Some("A new admission key was generated. Clients built with an older key can no longer use this relay once it runs with the new one.".into()),
    )
}

async fn relay_key_reveal(State(app): State<App>) -> Response {
    let k = app.w().relay.key.as_ref().map(|k| k.expose().to_string());
    match k {
        None => ApiErr::bad("No admission key yet.").into_response(),
        Some(k) => Json(json!({ "ok": true, "key": k })).into_response(),
    }
}

/// The release repository: the one signed in to in Step 5, else the default.
fn release_endpoints(app: &App) -> crate::config::Endpoints {
    let mut e = app.endpoints.clone();
    if let Some(r) = &app.w().signing.repo {
        e.repo = r.full_name.clone();
    }
    e
}

#[derive(Deserialize)]
struct SigningConnectReq {
    token: String,
    repo: String,
}

async fn signing_connect(State(app): State<App>, body: Bytes) -> ApiResult {
    let mut r: SigningConnectReq = parse(&body)?;
    let token = Secret::new(r.token.trim().to_string());
    zeroize::Zeroize::zeroize(&mut r.token);
    let repo = r.repo.trim().trim_matches('/').to_string();
    let valid = repo.split('/').count() == 2
        && repo.split('/').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        });
    let mut f = serde_json::Map::new();
    if token.is_empty() {
        f.insert("github.token".into(), json!("Paste a GitHub token."));
    }
    if !valid {
        f.insert(
            "github.repo".into(),
            json!("owner/repository, like ducminh1110/RemoteMac."),
        );
    }
    if !f.is_empty() {
        return Err(ApiErr::fields(Value::Object(f)));
    }
    match crate::signing::connect(&app, token, &repo).await {
        Ok(msg) => ok(&app, Some(msg)),
        Err(e) => {
            app.log.warn(format!("GitHub: {e}"));
            app.w().signing.error = None;
            Err(ApiErr::bad(e))
        }
    }
}

async fn signing_disconnect(State(app): State<App>) -> ApiResult {
    {
        let mut w = app.w();
        if w.signing.running {
            return Err(ApiErr::bad("Wait for Start to finish."));
        }
        // the key this session made stays usable from its backup
        w.signing = Default::default();
        w.rev += 1;
    }
    ok(&app, Some("Signed out of GitHub.".into()))
}

#[derive(Deserialize)]
struct SigningStartReq {
    version: String,
    #[serde(default)]
    taro_version: Option<String>,
    #[serde(default)]
    new_key: bool,
}

async fn signing_start(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: SigningStartReq = parse(&body)?;
    crate::signing::start(
        &app,
        crate::signing::StartReq {
            version: r.version.trim().to_string(),
            taro_version: r.taro_version.map(|t| t.trim().to_string()),
            new_key: r.new_key,
        },
    )
    .map_err(ApiErr::bad)?;
    ok(&app, Some("Started.".into()))
}

async fn components_discover(State(app): State<App>) -> ApiResult {
    app.w().components.loading = true;
    let r = crate::releases::discover(&app.http, &release_endpoints(&app), &app.timings).await;
    let mut w = app.w();
    w.components.loading = false;
    match r {
        Ok(list) => {
            app.log.info(format!(
                "releases: {} Enterprise release(s) found",
                list.len()
            ));
            w.components.error = None;
            let n = list.len();
            w.components.releases = Some(list);
            drop(w);
            ok(
                &app,
                Some(if n == 0 {
                    "No Enterprise release is published yet.".into()
                } else {
                    format!("{n} Enterprise release(s) found.")
                }),
            )
        }
        Err(e) => {
            app.log.warn(format!("releases: {e}"));
            w.components.error = Some(e.clone());
            Err(ApiErr::upstream(e))
        }
    }
}

#[derive(Deserialize)]
struct SelectReq {
    tag: String,
}

async fn components_select(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: SelectReq = parse(&body)?;
    let rel = {
        let mut w = app.w();
        let rel = w
            .components
            .releases
            .as_ref()
            .and_then(|l| l.iter().find(|x| x.tag == r.tag).cloned())
            .ok_or_else(|| ApiErr::bad("Unknown release: look for releases first."))?;
        w.components.verifying = Some(rel.tag.clone());
        rel
    };
    let keys = crate::signing::repo_trust(&app);
    let res = crate::releases::fetch_verified(
        &app.http,
        &release_endpoints(&app),
        &app.timings,
        &keys,
        &rel,
    )
    .await;
    let mut w = app.w();
    w.components.verifying = None;
    match res {
        Ok(sel) => {
            app.log.info(format!(
                "{}: component manifest verified (signed by {}), Taro {}",
                sel.tag, sel.signed_by, sel.taro_version
            ));
            for x in &sel.warnings {
                app.log.warn(format!("{}: {x}", sel.tag));
            }
            let msg = format!(
                "{}: the component manifest's signature is valid (Taro {}).",
                sel.tag, sel.taro_version
            );
            w.components.error = None;
            w.components.selected = Some(sel);
            w.rev += 1;
            drop(w);
            ok(&app, Some(msg))
        }
        Err(e) => {
            app.log.warn(format!("{}: {e}", rel.tag));
            w.components.selected = None;
            w.components.error = Some(e.clone());
            drop(w);
            Err(ApiErr::bad(e))
        }
    }
}

#[derive(Deserialize)]
struct PlatformsReq {
    platforms: Vec<String>,
}

async fn components_platforms(State(app): State<App>, body: Bytes) -> ApiResult {
    not_building(&app)?;
    let r: PlatformsReq = parse(&body)?;
    let mut v = Vec::new();
    for p in &r.platforms {
        if !taro_update::manifest::COMPONENT_IDS.contains(&p.as_str()) {
            return Err(ApiErr::bad(format!("Unknown platform {p:?}.")));
        }
        if !v.contains(p) {
            v.push(p.clone());
        }
    }
    v.sort_by_key(|x| {
        taro_update::manifest::COMPONENT_IDS
            .iter()
            .position(|y| y == x)
    });
    let mut w = app.w();
    w.components.platforms = v;
    w.rev += 1;
    drop(w);
    ok(&app, None)
}

#[derive(Deserialize)]
struct BuildReq {
    output_dir: String,
    #[serde(default)]
    replace: bool,
}

async fn build_start(State(app): State<App>, body: Bytes) -> ApiResult {
    let r: BuildReq = parse(&body)?;
    crate::build::start(&app, &r.output_dir, r.replace).map_err(|e| {
        if e.contains("already") && e.contains("running") {
            ApiErr::conflict(e)
        } else {
            ApiErr::bad(e)
        }
    })?;
    ok(&app, Some("Build started.".into()))
}

async fn build_cancel(State(app): State<App>) -> ApiResult {
    let c = app.w().build.cancel.clone();
    match c {
        Some(c) => {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
            app.log.info("build: cancel requested");
            ok(&app, Some("Cancelling…".into()))
        }
        None => Err(ApiErr::bad("No build is running.")),
    }
}

async fn validate_run(State(app): State<App>) -> ApiResult {
    let result = {
        let mut w = app.w();
        let r = w
            .build
            .result
            .clone()
            .ok_or_else(|| ApiErr::bad("Build the packages first."))?;
        w.validation_running = true;
        r
    };
    let report = tokio::task::spawn_blocking(move || crate::check::run(&result))
        .await
        .map_err(|e| ApiErr::bad(e.to_string()))?;
    app.log.info(format!(
        "validation: {} ({} checks)",
        if report.ok { "ok" } else { "FAILED" },
        report.checks.len()
    ));
    {
        let mut w = app.w();
        w.validation_running = false;
        w.validation = Some(report);
    }
    ok(&app, None)
}

async fn output_open(State(app): State<App>) -> ApiResult {
    let dir = app
        .w()
        .build
        .result
        .as_ref()
        .map(|r| std::path::PathBuf::from(&r.dir))
        .ok_or_else(|| ApiErr::bad("Nothing has been built yet."))?;
    (app.open_folder)(&dir).map_err(|e| ApiErr::bad(format!("Could not open the folder: {e}")))?;
    ok(&app, Some(format!("Opened {}.", dir.display())))
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
        "taro-provider-build-summary.json",
    )
}

async fn summary_md(State(app): State<App>) -> Response {
    let md = crate::summary::summary_markdown(&app.w());
    download(
        md,
        "text/markdown; charset=utf-8",
        "taro-provider-build-summary.md",
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
