//! The two HTTP apps and what wraps every request: a request id, structured access logs and
//! metrics, security headers, an Origin check on every state-changing request, a Host check on
//! the portal (its own hostname and verified custom domains only), body limits, JSON 404s under
//! /api, and the static web apps.

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::json;
use tokio::sync::RwLock;
use tower_http::services::{ServeDir, ServeFile};

use crate::error::{ApiError, REQUEST_ID};
use crate::state::{AppState, Audience};

async fn request_id(req: Request, next: Next) -> Response {
    let rid = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(String::from)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut res = REQUEST_ID.scope(rid.clone(), next.run(req)).await;
    if let Ok(v) = HeaderValue::from_str(&rid) {
        res.headers_mut().insert("x-request-id", v);
    }
    res
}

async fn observe(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| {
            if req.uri().path().starts_with("/api/") {
                "unmatched".into()
            } else {
                "static".into()
            }
        });
    let audience = req
        .extensions()
        .get::<Audience>()
        .map(|a| a.key())
        .unwrap_or("?");
    let res = next.run(req).await;
    let secs = started.elapsed().as_secs_f64();
    let status = res.status().as_u16();
    let class = format!("{}xx", status / 100);
    state.metrics.inc(
        "taro_http_requests_total",
        &[
            ("app", audience),
            ("route", &route),
            ("method", method.as_str()),
            ("status", &class),
        ],
    );
    state.metrics.observe(
        "taro_http_request_seconds",
        &[("app", audience), ("route", &route)],
        secs,
    );
    if route != "static" {
        // the path pattern, never the query (tokens travel in some queries)
        tracing::info!(request_id = %crate::error::request_id(), app = audience, method = %method, route = %route, status, ms = (secs * 1000.0) as u64, "request");
    }
    res
}

async fn security_headers(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let set = |h: &mut axum::http::HeaderMap, k: header::HeaderName, v: &'static str| {
        h.entry(k).or_insert(HeaderValue::from_static(v));
    };
    set(h, header::CONTENT_SECURITY_POLICY, "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; font-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'");
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        h,
        header::REFERRER_POLICY,
        "strict-origin-when-cross-origin",
    );
    set(h, header::X_FRAME_OPTIONS, "DENY");
    set(
        h,
        header::HeaderName::from_static("permissions-policy"),
        "camera=(), microphone=(), geolocation=(), payment=()",
    );
    set(
        h,
        header::HeaderName::from_static("cross-origin-opener-policy"),
        "same-origin",
    );
    if state.config.secure_cookies() {
        set(
            h,
            header::STRICT_TRANSPORT_SECURITY,
            "max-age=31536000; includeSubDomains",
        );
    }
    if h.get(header::CACHE_CONTROL).is_none()
        && h.get(header::CONTENT_TYPE)
            .is_some_and(|v| v.to_str().unwrap_or("").starts_with("application/json"))
    {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// State-changing requests must come from this app's own origin (or carry no browser Origin at
/// all and be one of the machine endpoints). With the SameSite cookie and the CSRF token, this
/// is the third line against cross-site requests.
async fn origin_check(
    State(state): State<AppState>,
    Extension(audience): Extension<Audience>,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let path = req.uri().path();
    // machine endpoints authenticate by what they carry (a Stripe signature, a grant), not a cookie
    if path.starts_with("/webhooks/") || path == "/api/v1/connect/redeem" {
        return next.run(req).await;
    }
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let own = audience.origin(&state.config);
    let ok = match origin {
        Some(o) => o == own || (audience == Audience::Customer && custom_origin(&state, o).await),
        None => false,
    };
    if !ok {
        return ApiError::Forbidden.into_response();
    }
    next.run(req).await
}

/// An https origin on a verified custom domain.
async fn custom_origin(state: &AppState, origin: &str) -> bool {
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    hosts(state).await.iter().any(|h| h == host)
}

static HOSTS: std::sync::OnceLock<RwLock<(Instant, Arc<Vec<String>>)>> = std::sync::OnceLock::new();

async fn hosts(state: &AppState) -> Arc<Vec<String>> {
    let cell = HOSTS.get_or_init(|| {
        RwLock::new((
            Instant::now() - std::time::Duration::from_secs(3600),
            Arc::new(vec![]),
        ))
    });
    {
        let g = cell.read().await;
        if g.0.elapsed() < std::time::Duration::from_secs(30) {
            return g.1.clone();
        }
    }
    let v = Arc::new(crate::domains::active_hosts(state).await);
    *cell.write().await = (Instant::now(), v.clone());
    v
}

/// The portal answers for its own hostname and verified custom domains only (no tenant can be
/// reached through a Host header someone made up). In development any host is accepted.
async fn host_check(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.config.env == crate::config::Env::Development {
        return next.run(req).await;
    }
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_lowercase())
        .unwrap_or_default();
    let path = req.uri().path();
    if path == "/healthz" || path == "/readyz" || hosts(&state).await.contains(&host) {
        return next.run(req).await;
    }
    (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response()
}

async fn api_not_found() -> ApiError {
    ApiError::NotFound
}

async fn healthz() -> &'static str {
    "ok"
}

/// Ready: the database answers and every migration this build knows has been applied.
async fn readyz(State(state): State<AppState>) -> Response {
    let applied: Result<(i64,), _> =
        sqlx::query_as("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(&state.db)
            .await;
    let known = crate::db::MIGRATOR.iter().count() as i64;
    match applied {
        Ok((n,)) if n >= known => Json(json!({ "ready": true, "migrations": n })).into_response(),
        Ok((n,)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ready": false, "why": format!("{n} of {known} migrations applied") })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ready": false, "why": "the database does not answer" })),
        )
            .into_response(),
    }
}

/// Prometheus metrics (ops listener, loopback only: scrape it from the host).
async fn metrics(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> Response {
    if !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut gauges = vec![];
    if let Ok(rows) =
        sqlx::query_as::<_, (String, i64)>("SELECT status, count(*) FROM rentals GROUP BY status")
            .fetch_all(&state.db)
            .await
    {
        gauges.extend(
            rows.into_iter()
                .map(|(s, n)| (format!("taro_rentals{{status=\"{s}\"}}"), n as f64)),
        );
    }
    if let Ok(rows) = sqlx::query_as::<_, (String, i64)>(
        "SELECT state, count(*) FROM provisioning_jobs GROUP BY state",
    )
    .fetch_all(&state.db)
    .await
    {
        gauges.extend(
            rows.into_iter()
                .map(|(s, n)| (format!("taro_provisioning_jobs{{state=\"{s}\"}}"), n as f64)),
        );
    }
    if let Ok((n, lag)) = sqlx::query_as::<_, (i64, Option<f64>)>("SELECT count(*), extract(epoch from now() - min(received_at))::float8 FROM stripe_webhook_events WHERE state IN ('pending', 'failed')").fetch_one(&state.db).await {
        gauges.push(("taro_stripe_webhook_backlog".into(), n as f64));
        gauges.push(("taro_stripe_webhook_lag_seconds".into(), lag.unwrap_or(0.0)));
    }
    if let Ok((n,)) =
        sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM domain_assignments WHERE state = 'error'")
            .fetch_one(&state.db)
            .await
    {
        gauges.push(("taro_domains_failing".into(), n as f64));
    }
    gauges.push(("taro_db_pool_connections".into(), state.db.size() as f64));
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render(&gauges),
    )
        .into_response()
}

fn web(state: &AppState, app: &str) -> ServeDir<ServeFile> {
    let dir = state.config.web_dir.join("apps").join(app);
    // an SPA: unknown paths are the app's index (its router shows the page or a not-found)
    ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))
}

fn common(state: &AppState, router: Router<AppState>, audience: Audience) -> Router {
    router
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        // the design tokens both apps share
        .nest_service("/_shared", ServeDir::new(state.config.web_dir.join("packages/design-tokens")))
        .layer(middleware::from_fn_with_state(state.clone(), origin_check))
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .layer(Extension(audience))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_headers,
        ))
        .layer(middleware::from_fn(request_id))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            2 * 1024 * 1024,
        ))
        .with_state(state.clone())
}

/// The Customer Portal's listener.
pub fn customer(state: &AppState) -> Router {
    let api = Router::new()
        .merge(crate::identity::routes())
        .merge(crate::organizations::routes())
        .merge(crate::rentals::routes())
        .merge(crate::connectivity::routes())
        .merge(crate::connectivity::redeem_routes())
        .merge(crate::billing::routes())
        .merge(crate::billing::webhook_routes())
        .merge(crate::notifications::routes())
        .merge(crate::branding::routes())
        .merge(crate::domains::routes())
        .fallback_service(web(state, "customer-portal"));
    common(state, api, Audience::Customer)
        .layer(middleware::from_fn_with_state(state.clone(), host_check))
}

/// The Provider Console's listener (Taro Operations; keep it off the public internet or behind
/// the company's access proxy).
pub fn ops(state: &AppState) -> Router {
    let api = Router::new()
        .merge(crate::identity::routes())
        .merge(crate::ops::routes())
        .merge(crate::branding::ops_routes())
        .route("/metrics", get(metrics))
        .route("/internal/tls/ask", get(crate::domains::tls_ask))
        .fallback_service(web(state, "provider-console"));
    common(state, api, Audience::Ops)
}

/// A request body type that both routers accept (re-exported for tests).
pub type HttpBody = Body;
