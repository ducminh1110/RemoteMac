//! Shared test harness: a local fake of the GitHub, Stripe and portal endpoints, a fake DNS
//! resolver, and a client that drives the wizard's router in-process.
#![allow(dead_code)]

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use crypto_box::aead::OsRng;
use crypto_box::SecretKey;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use taro_enterprise_setup::config::{Endpoints, Timings};
use taro_enterprise_setup::domain::{BoxFut, DnsResolver};
use taro_enterprise_setup::state::{AppState, Options};
use tower::ServiceExt;

pub const GH_TOKEN: &str = concat!(
    "gi",
    "thub_pat_11TESTTOKEN0_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ0123456789"
);
pub const DEVICE_TOKEN: &str = concat!("gh", "o_devicetoken0123456789abcdefghijklmn");
pub const DEVICE_CODE: &str = "3584d83530557fdd1f46af8289938c8ef79f9dc5";
pub const STRIPE_KEY: &str = concat!("sk", "_test_51HfakeKey0123456789abcdefghijklmnopqrstuvwxyz");
pub const STRIPE_LIVE_KEY: &str =
    concat!("sk", "_live_51HfakeLive0123456789abcdefghijklmnopqrstuvw");
pub const CREATED_WHSEC: &str = concat!("wh", "sec_createdBySTRIPEfake0123456789abcdef");
pub const LOG_LOOKALIKE: &str = concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789");

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub body: String,
}

pub struct Fake {
    pub base: Mutex<String>,
    pub requests: Mutex<Vec<Recorded>>,
    pub secrets: Mutex<HashMap<String, (String, String)>>,
    pub contents: Mutex<HashMap<String, Vec<u8>>>,
    pub sk: SecretKey,
    pub template_is_template: AtomicBool,
    pub dispatched: AtomicBool,
    pub run_polls: AtomicU32,
    pub release_polls: AtomicU32,
    pub release_visible_polls: AtomicU32,
    pub device_script: Mutex<Vec<&'static str>>,
    pub device_polls: AtomicU32,
    pub repo_created: AtomicBool,
    pub webhook_created: AtomicBool,
}

impl Fake {
    pub fn base(&self) -> String {
        self.base.lock().unwrap().clone()
    }
    pub fn reqs(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

type F = Arc<Fake>;

async fn record(State(f): State<F>, req: Request, next: axum::middleware::Next) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 8 * 1024 * 1024).await.unwrap();
    f.requests.lock().unwrap().push(Recorded {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        auth: parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        body: String::from_utf8_lossy(&bytes).into_owned(),
    });
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

fn gh_auth(h: &HeaderMap) -> bool {
    let a = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    a == format!("Bearer {GH_TOKEN}") || a == format!("Bearer {DEVICE_TOKEN}")
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"message": "Bad credentials"})),
    )
        .into_response()
}

macro_rules! need_auth {
    ($h:expr) => {
        if !gh_auth(&$h) {
            return unauthorized();
        }
    };
}

async fn gh_user(h: HeaderMap) -> Response {
    need_auth!(h);
    let mut r = Json(json!({"login": "octo", "id": 42, "type": "User"})).into_response();
    r.headers_mut()
        .insert("x-oauth-scopes", "repo, workflow".parse().unwrap());
    r
}

async fn gh_orgs(h: HeaderMap) -> Response {
    need_auth!(h);
    Json(json!([{"login": "acme"}, {"login": "viewers"}])).into_response()
}

async fn gh_membership(h: HeaderMap, Path(org): Path<String>) -> Response {
    need_auth!(h);
    let role = if org == "acme" { "admin" } else { "member" };
    Json(json!({"role": role, "state": "active"})).into_response()
}

async fn gh_org(h: HeaderMap, Path(_org): Path<String>) -> Response {
    need_auth!(h);
    Json(json!({"members_can_create_private_repositories": false})).into_response()
}

async fn gh_repo(
    State(f): State<F>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
) -> Response {
    need_auth!(h);
    if o == "tmpl" && r == "template" {
        return Json(json!({
            "full_name": "tmpl/template", "name": "template", "private": false,
            "is_template": f.template_is_template.load(Ordering::SeqCst),
            "html_url": "https://github.com/tmpl/template", "default_branch": "main"
        }))
        .into_response();
    }
    (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response()
}

async fn gh_generate(
    State(f): State<F>,
    h: HeaderMap,
    Path((o, r)): Path<(String, String)>,
    Json(b): Json<Value>,
) -> Response {
    need_auth!(h);
    assert_eq!((o.as_str(), r.as_str()), ("tmpl", "template"));
    assert_eq!(
        b["private"],
        json!(true),
        "repository must be generated private"
    );
    f.repo_created.store(true, Ordering::SeqCst);
    let owner = b["owner"].as_str().unwrap();
    let name = b["name"].as_str().unwrap();
    (
        StatusCode::CREATED,
        Json(json!({
            "full_name": format!("{owner}/{name}"), "name": name, "private": true,
            "html_url": format!("https://github.com/{owner}/{name}"), "default_branch": "main"
        })),
    )
        .into_response()
}

async fn gh_wf_perms(h: HeaderMap, Json(b): Json<Value>) -> Response {
    need_auth!(h);
    assert_eq!(b["default_workflow_permissions"], json!("read"));
    StatusCode::NO_CONTENT.into_response()
}

async fn gh_public_key(State(f): State<F>, h: HeaderMap) -> Response {
    need_auth!(h);
    let pk = f.sk.public_key();
    Json(json!({"key_id": "kid-1", "key": taro_enterprise_setup::util::b64(pk.as_bytes())}))
        .into_response()
}

async fn gh_put_secret(
    State(f): State<F>,
    h: HeaderMap,
    Path((_o, _r, name)): Path<(String, String, String)>,
    Json(b): Json<Value>,
) -> Response {
    need_auth!(h);
    f.secrets.lock().unwrap().insert(
        name,
        (
            b["encrypted_value"].as_str().unwrap().to_string(),
            b["key_id"].as_str().unwrap().to_string(),
        ),
    );
    StatusCode::CREATED.into_response()
}

async fn gh_get_content(
    State(f): State<F>,
    h: HeaderMap,
    Path((_o, _r, path)): Path<(String, String, String)>,
) -> Response {
    need_auth!(h);
    if f.contents.lock().unwrap().contains_key(&path) {
        return Json(json!({"sha": "abc123"})).into_response();
    }
    (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response()
}

async fn gh_put_content(
    State(f): State<F>,
    h: HeaderMap,
    Path((_o, _r, path)): Path<(String, String, String)>,
    Json(b): Json<Value>,
) -> Response {
    need_auth!(h);
    let bytes = taro_enterprise_setup::util::b64_decode(b["content"].as_str().unwrap()).unwrap();
    f.contents.lock().unwrap().insert(path, bytes);
    (
        StatusCode::CREATED,
        Json(json!({"commit": {"sha": "deadbeefcafe"}})),
    )
        .into_response()
}

async fn gh_dispatch(State(f): State<F>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    need_auth!(h);
    assert_eq!(b["inputs"]["version"], json!("0.1.0"));
    f.dispatched.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT.into_response()
}

fn now() -> String {
    taro_enterprise_setup::util::rfc3339(taro_enterprise_setup::util::now_secs())
}

async fn gh_runs(State(f): State<F>, h: HeaderMap) -> Response {
    need_auth!(h);
    let old = json!({"id": 1, "status": "completed", "conclusion": "success", "html_url": "https://github.com/x/runs/1", "created_at": "2020-01-01T00:00:00Z", "event": "workflow_dispatch"});
    let runs = if f.dispatched.load(Ordering::SeqCst) {
        vec![
            json!({"id": 2, "status": "queued", "html_url": "https://github.com/x/runs/2", "created_at": now(), "event": "workflow_dispatch"}),
            old,
        ]
    } else {
        vec![old]
    };
    Json(json!({"total_count": runs.len(), "workflow_runs": runs})).into_response()
}

async fn gh_run(
    State(f): State<F>,
    h: HeaderMap,
    Path((_o, _r, id)): Path<(String, String, u64)>,
) -> Response {
    need_auth!(h);
    assert_eq!(id, 2, "the wizard must follow the newly dispatched run");
    let n = f.run_polls.fetch_add(1, Ordering::SeqCst);
    let (status, conclusion) = match n {
        0 => ("queued", Value::Null),
        1 => ("in_progress", Value::Null),
        _ => ("completed", json!("success")),
    };
    Json(json!({"id": 2, "status": status, "conclusion": conclusion, "html_url": "https://github.com/x/runs/2", "created_at": now()})).into_response()
}

async fn gh_jobs(State(f): State<F>, h: HeaderMap) -> Response {
    need_auth!(h);
    let done = f.run_polls.load(Ordering::SeqCst) >= 3;
    Json(json!({"jobs": [{
        "id": 77, "name": "build", "status": if done { "completed" } else { "in_progress" },
        "conclusion": if done { json!("success") } else { Value::Null },
        "steps": [{"name": "Checkout", "status": "completed", "conclusion": "success", "number": 1},
                  {"name": "Build", "status": if done { "completed" } else { "in_progress" }, "conclusion": if done { json!("success") } else { Value::Null }, "number": 2}]
    }]}))
    .into_response()
}

async fn gh_job_logs(State(f): State<F>, h: HeaderMap) -> Response {
    need_auth!(h);
    let mut r = StatusCode::FOUND.into_response();
    r.headers_mut().insert(
        "location",
        format!("{}/blob/log?sig=xyz", f.base()).parse().unwrap(),
    );
    r
}

async fn blob_log() -> Response {
    format!(
        "2026-10-16T00:00:00Z step 1\n2026-10-16T00:00:01Z using token {LOG_LOOKALIKE}\n2026-10-16T00:00:02Z header Authorization: Bearer abcdefghijklmnopqrstuvwxyz\n2026-10-16T00:00:03Z stripe {STRIPE_KEY}\n2026-10-16T00:00:04Z done\n"
    )
    .into_response()
}

async fn gh_release(
    State(f): State<F>,
    h: HeaderMap,
    Path((_o, _r, tag)): Path<(String, String, String)>,
) -> Response {
    need_auth!(h);
    if !f.dispatched.load(Ordering::SeqCst) || f.run_polls.load(Ordering::SeqCst) < 3 {
        return (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response();
    }
    let n = f.release_polls.fetch_add(1, Ordering::SeqCst);
    match n {
        0 => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"}))).into_response(),
        1 | 2 => Json(json!({"tag_name": tag, "html_url": "https://github.com/x/releases/tag/0.1.0", "draft": false, "assets": []})).into_response(),
        _ => {
            f.release_visible_polls.fetch_add(1, Ordering::SeqCst);
            Json(json!({"tag_name": tag, "html_url": "https://github.com/x/releases/tag/0.1.0", "draft": false,
                "assets": [{"name": "taro-enterprise-0.1.0.tar.gz", "size": 2048, "browser_download_url": "https://github.com/x/d"}]}))
            .into_response()
        }
    }
}

async fn gh_delete(h: HeaderMap) -> Response {
    need_auth!(h);
    (
        StatusCode::FORBIDDEN,
        Json(json!({"message": "Must have admin rights to Repository."})),
    )
        .into_response()
}

async fn device_code(body: String) -> Response {
    assert!(body.contains("client_id=Iv1.testclient"), "{body}");
    Json(json!({"device_code": DEVICE_CODE, "user_code": "WDJB-MJHT", "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 1})).into_response()
}

async fn device_token(State(f): State<F>, body: String) -> Response {
    assert!(
        body.contains(&format!("device_code={DEVICE_CODE}")),
        "{body}"
    );
    assert!(
        body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
        "{body}"
    );
    f.device_polls.fetch_add(1, Ordering::SeqCst);
    let next = {
        let mut s = f.device_script.lock().unwrap();
        if s.is_empty() {
            "token"
        } else {
            s.remove(0)
        }
    };
    Json(match next {
        "pending" => json!({"error": "authorization_pending"}),
        "slow_down" => json!({"error": "slow_down", "interval": 6}),
        "expired" => json!({"error": "expired_token"}),
        "denied" => json!({"error": "access_denied"}),
        _ => {
            json!({"access_token": DEVICE_TOKEN, "token_type": "bearer", "scope": "repo,workflow"})
        }
    })
    .into_response()
}

fn stripe_ok(h: &HeaderMap) -> bool {
    let a = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    a == format!("Bearer {STRIPE_KEY}") || a == format!("Bearer {STRIPE_LIVE_KEY}")
}

macro_rules! need_stripe {
    ($h:expr) => {
        if !stripe_ok(&$h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"message": "Invalid API Key provided", "type": "invalid_request_error"}}))).into_response();
        }
    };
}

async fn st_account(h: HeaderMap) -> Response {
    need_stripe!(h);
    Json(json!({"id": "acct_1Fake", "country": "US", "charges_enabled": true, "details_submitted": true, "settings": {"dashboard": {"display_name": "Acme Test"}}})).into_response()
}

async fn st_webhooks(State(f): State<F>, h: HeaderMap) -> Response {
    need_stripe!(h);
    let data = if f.webhook_created.load(Ordering::SeqCst) {
        vec![
            json!({"id": "we_1", "url": format!("{}/portal/webhooks/stripe", f.base()), "status": "enabled", "enabled_events": ["checkout.session.completed"], "livemode": false}),
        ]
    } else {
        vec![]
    };
    Json(json!({"data": data})).into_response()
}

async fn st_create_webhook(State(f): State<F>, h: HeaderMap, body: String) -> Response {
    need_stripe!(h);
    assert!(
        body.contains("enabled_events%5B6%5D=charge.refunded"),
        "{body}"
    );
    f.webhook_created.store(true, Ordering::SeqCst);
    Json(json!({"id": "we_1", "url": format!("{}/portal/webhooks/stripe", f.base()), "status": "enabled",
        "enabled_events": ["checkout.session.completed","checkout.session.expired","invoice.paid","invoice.payment_failed","customer.subscription.updated","customer.subscription.deleted","charge.refunded"],
        "livemode": false, "secret": CREATED_WHSEC}))
    .into_response()
}

async fn st_checkout(h: HeaderMap, body: String) -> Response {
    need_stripe!(h);
    assert!(body.contains("unit_amount%5D=100"), "{body}");
    assert!(body.contains("quantity%5D=1"), "{body}");
    Json(json!({"id": "cs_test_a1B2c3", "url": "https://checkout.stripe.com/c/pay/cs_test_a1B2c3", "livemode": false, "status": "open"})).into_response()
}

async fn st_expire(h: HeaderMap, Path(id): Path<String>) -> Response {
    need_stripe!(h);
    Json(json!({"id": id, "status": "expired", "livemode": false})).into_response()
}

async fn portal_ok(_q: Query<HashMap<String, String>>) -> Response {
    Json(json!({"status": "ok"})).into_response()
}

async fn portal_providers() -> Response {
    Json(json!({"providers": [{"id": "password"}, {"id": "github"}]})).into_response()
}

pub async fn start_fake() -> Arc<Fake> {
    let f = Arc::new(Fake {
        base: Mutex::new(String::new()),
        requests: Mutex::new(vec![]),
        secrets: Mutex::new(HashMap::new()),
        contents: Mutex::new(HashMap::new()),
        sk: SecretKey::generate(&mut OsRng),
        template_is_template: AtomicBool::new(true),
        dispatched: AtomicBool::new(false),
        run_polls: AtomicU32::new(0),
        release_polls: AtomicU32::new(0),
        release_visible_polls: AtomicU32::new(0),
        device_script: Mutex::new(vec![]),
        device_polls: AtomicU32::new(0),
        repo_created: AtomicBool::new(false),
        webhook_created: AtomicBool::new(false),
    });
    let app = Router::new()
        .route(
            "/gh/",
            get(|| async { Json(json!({"current_user_url": "x"})) }),
        )
        .route("/gh/user", get(gh_user))
        .route("/gh/user/orgs", get(gh_orgs))
        .route("/gh/user/memberships/orgs/{org}", get(gh_membership))
        .route("/gh/orgs/{org}", get(gh_org))
        .route("/gh/repos/{o}/{r}", get(gh_repo).delete(gh_delete))
        .route("/gh/repos/{o}/{r}/generate", post(gh_generate))
        .route(
            "/gh/repos/{o}/{r}/actions/permissions/workflow",
            put(gh_wf_perms),
        )
        .route(
            "/gh/repos/{o}/{r}/actions/secrets/public-key",
            get(gh_public_key),
        )
        .route(
            "/gh/repos/{o}/{r}/actions/secrets/{name}",
            put(gh_put_secret),
        )
        .route(
            "/gh/repos/{o}/{r}/contents/{*path}",
            get(gh_get_content).put(gh_put_content),
        )
        .route(
            "/gh/repos/{o}/{r}/actions/workflows/{wf}/dispatches",
            post(gh_dispatch),
        )
        .route(
            "/gh/repos/{o}/{r}/actions/workflows/{wf}/runs",
            get(gh_runs),
        )
        .route("/gh/repos/{o}/{r}/actions/runs/{id}", get(gh_run))
        .route("/gh/repos/{o}/{r}/actions/runs/{id}/jobs", get(gh_jobs))
        .route("/gh/repos/{o}/{r}/actions/jobs/{id}/logs", get(gh_job_logs))
        .route("/gh/repos/{o}/{r}/releases/tags/{tag}", get(gh_release))
        .route("/blob/log", get(blob_log))
        .route("/web/login/device/code", post(device_code))
        .route("/web/login/oauth/access_token", post(device_token))
        .route(
            "/stripe/v1/",
            get(|| async { (StatusCode::UNAUTHORIZED, "no key") }),
        )
        .route("/stripe/v1/account", get(st_account))
        .route(
            "/stripe/v1/webhook_endpoints",
            get(st_webhooks).post(st_create_webhook),
        )
        .route("/stripe/v1/checkout/sessions", post(st_checkout))
        .route("/stripe/v1/checkout/sessions/{id}/expire", post(st_expire))
        .route("/portal/healthz", get(portal_ok))
        .route("/portal/readyz", get(portal_ok))
        .route("/portal/api/v1/auth/providers", get(portal_providers))
        .layer(axum::middleware::from_fn_with_state(f.clone(), record))
        .with_state(f.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    *f.base.lock().unwrap() = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    f
}

#[derive(Default)]
pub struct FakeDns {
    pub txt: Mutex<HashMap<String, Vec<String>>>,
    pub ips: Mutex<HashMap<String, Vec<IpAddr>>>,
}

impl DnsResolver for FakeDns {
    fn txt<'a>(&'a self, n: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
        Box::pin(async move {
            self.txt
                .lock()
                .unwrap()
                .get(n)
                .cloned()
                .ok_or("no records found".into())
        })
    }
    fn ips<'a>(&'a self, n: &'a str) -> BoxFut<'a, Result<Vec<IpAddr>, String>> {
        Box::pin(async move {
            self.ips
                .lock()
                .unwrap()
                .get(n)
                .cloned()
                .ok_or("no records found".into())
        })
    }
    fn cname<'a>(&'a self, _n: &'a str) -> BoxFut<'a, Result<Vec<String>, String>> {
        Box::pin(async move { Err("no records found".into()) })
    }
}

pub const WIZ_PORT: u16 = 45999;
pub const ACCESS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub struct Wiz {
    pub app: Arc<AppState>,
    pub router: Router,
    pub responses: Mutex<Vec<String>>,
    pub dns: Arc<FakeDns>,
    pub fake: Arc<Fake>,
}

pub fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        github_api: format!("{base}/gh"),
        github_web: format!("{base}/web"),
        stripe_api: format!("{base}/stripe"),
        domain_health: format!("{base}/portal/healthz?d={{domain}}"),
    }
}

pub async fn wizard() -> Wiz {
    let fake = start_fake().await;
    let dns = Arc::new(FakeDns::default());
    let app = AppState::new(
        SocketAddr::from(([127, 0, 0, 1], WIZ_PORT)),
        ACCESS.to_string(),
        Options {
            endpoints: endpoints(&fake.base()),
            timings: Timings::fast(),
            dns: dns.clone(),
            remember_dir: None,
            github_client_id: Some("Iv1.testclient".into()),
            managed_zone: None,
            template: "tmpl/template".into(),
        },
    );
    let router = taro_enterprise_setup::server::router(app.clone());
    Wiz {
        app,
        router,
        responses: Mutex::new(vec![]),
        dns,
        fake,
    }
}

impl Wiz {
    /// Send a well-formed request (correct Host, Origin and token).
    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("host", format!("127.0.0.1:{WIZ_PORT}"))
            .header("origin", format!("http://127.0.0.1:{WIZ_PORT}"))
            .header("x-setup-token", ACCESS);
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let (s, text) = self.raw(b.body(body).unwrap()).await;
        (
            s,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn upload(&self, kind: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/branding/{kind}"))
            .header("host", format!("127.0.0.1:{WIZ_PORT}"))
            .header("origin", format!("http://127.0.0.1:{WIZ_PORT}"))
            .header("x-setup-token", ACCESS)
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes))
            .unwrap();
        let (s, text) = self.raw(req).await;
        (
            s,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    pub async fn raw(&self, req: Request) -> (StatusCode, String) {
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let s = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.responses.lock().unwrap().push(text.clone());
        (s, text)
    }

    pub async fn state(&self) -> Value {
        self.call("GET", "/api/state", None).await.1["state"].clone()
    }

    /// Poll the state until `pred` holds (or panic after ~10 s).
    pub async fn wait_for(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..1000 {
            let s = self.state().await;
            if pred(&s) {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}: {}", self.state().await);
    }
}
