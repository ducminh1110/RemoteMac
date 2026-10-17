//! Signing in to the Taro app, and its "My Macs" list.
//!
//! A device authorization grant (as RFC 8628): the app asks for a code
//! (`POST /api/v1/devices/authorize`), shows it and opens `<portal>/device?code=…`; the user,
//! signed in to the portal, sees which app asks and approves (or denies) it; the app, polling
//! `POST /api/v1/devices/token`, then gets a token for this user. Only hashes of the device code
//! and of the token are stored; codes live ten minutes and work once.
//!
//! With the token (`Authorization: Bearer tdv_…`) the app lists the Macs assigned to the user
//! (`GET /api/v1/device/macs`) and connects to one (`POST /api/v1/device/macs/{id}/connect`):
//! a one-time connection grant is issued and redeemed on the spot, with every check of a grant
//! from the portal (assigned to this user, ready, connecting not paused). Tokens last 90 days,
//! are listed and revocable in the portal (`/api/v1/auth/devices`), and end at sign-out.

use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::state::{AppState, Audience};

const CODE_MINUTES: i64 = 10;
const INTERVAL_SECS: i64 = 5;
const TOKEN_DAYS: i64 = 90;
/// No vowels (no words), no look-alikes.
const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

fn user_code() -> String {
    let mut r = rand::rngs::OsRng;
    let mut c = |n| {
        (0..n)
            .map(|_| ALPHABET[r.gen_range(0..ALPHABET.len())] as char)
            .collect::<String>()
    };
    format!("{}-{}", c(4), c(4))
}

/// What the user typed: case and dashes do not matter.
fn normalize(code: &str) -> Option<String> {
    let c: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (c.len() == 8 && c.bytes().all(|b| ALPHABET.contains(&b)))
        .then(|| format!("{}-{}", &c[..4], &c[4..]))
}

fn client_name(c: Option<String>) -> String {
    let c: String = c
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if c.trim().is_empty() {
        "Taro".into()
    } else {
        c.trim().to_string()
    }
}

#[derive(Deserialize)]
struct AuthorizeReq {
    client: Option<String>,
}

async fn authorize(
    State(state): State<AppState>,
    parts: Parts,
    Json(b): Json<AuthorizeReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let ip = crate::identity::client_ip(&parts, &state);
    if !state.limiter.allow(
        "device_authorize",
        ip.as_deref().unwrap_or(""),
        20,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    if !crate::ops::flag(&state, "devices.sign_in").await {
        return Err(ApiError::Unavailable(
            "Signing in to the app is paused for a moment. Try again later.".into(),
        ));
    }
    let device_code = crypto::token(32);
    let code = user_code();
    let expires = Utc::now() + Duration::minutes(CODE_MINUTES);
    sqlx::query("INSERT INTO device_authorizations (id, device_code_hash, user_code, client, ip, expires_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(Uuid::new_v4())
        .bind(crypto::hash(&device_code))
        .bind(&code)
        .bind(client_name(b.client))
        .bind(ip.as_deref())
        .bind(expires)
        .execute(&state.db)
        .await?;
    let portal = state.config.public_url.origin().ascii_serialization();
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "device_code": device_code,
            "user_code": code,
            "verification_uri": format!("{portal}/device"),
            "verification_uri_complete": format!("{portal}/device?code={code}"),
            "expires_in": CODE_MINUTES * 60,
            "interval": INTERVAL_SECS,
        })),
    ))
}

#[derive(Deserialize)]
struct TokenReq {
    device_code: String,
}

type Pending = (
    Uuid,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<Uuid>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

/// The app's poll: pending (202), slow down (202), approved (200 with the token, once), or an
/// error when the code was denied, expired or used.
async fn token(
    State(state): State<AppState>,
    Json(b): Json<TokenReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let mut tx = state.db.begin().await?;
    let row: Option<Pending> = sqlx::query_as("SELECT id, client, expires_at, last_poll_at, approved_by, denied_at, consumed_at FROM device_authorizations WHERE device_code_hash = $1 FOR UPDATE")
        .bind(crypto::hash(&b.device_code))
        .fetch_optional(&mut *tx)
        .await?;
    let Some((id, client, expires, last_poll, approved_by, denied, consumed)) = row else {
        return Err(ApiError::invalid(
            "This sign-in code is not known. Sign in again.",
        ));
    };
    if consumed.is_some() {
        return Err(ApiError::invalid(
            "This sign-in code was used already. Sign in again.",
        ));
    }
    if denied.is_some() {
        return Err(ApiError::invalid("The sign-in was denied in the portal."));
    }
    if expires <= Utc::now() {
        return Err(ApiError::invalid(
            "The sign-in code expired. Sign in again.",
        ));
    }
    sqlx::query("UPDATE device_authorizations SET last_poll_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let Some(user) = approved_by else {
        let fast = last_poll.is_some_and(|t| Utc::now() - t < Duration::seconds(INTERVAL_SECS - 1));
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(
                json!({ "status": if fast { "slow_down" } else { "pending" }, "interval": INTERVAL_SECS }),
            ),
        ));
    };
    let tid = Uuid::new_v4();
    let secret = crypto::token(32);
    let until = Utc::now() + Duration::days(TOKEN_DAYS);
    sqlx::query("INSERT INTO device_tokens (id, user_id, secret_hash, client, expires_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(tid)
        .bind(user)
        .bind(crypto::hash(&secret))
        .bind(&client)
        .bind(until)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE device_authorizations SET consumed_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(Actor::User(user), "device.signed_in", "device_token")
            .resource(tid)
            .details(json!({ "client": client })),
    )
    .await?;
    let (email,): (String,) = sqlx::query_as("SELECT email FROM users WHERE id = $1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::OK,
        Json(
            json!({ "status": "approved", "access_token": format!("tdv_{}.{secret}", tid.simple()), "email": email, "expires_at": until }),
        ),
    ))
}

#[derive(Serialize, sqlx::FromRow)]
struct PendingView {
    user_code: String,
    client: String,
    ip: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct CodeReq {
    user_code: String,
}

/// The portal shows which app asks before the user approves.
async fn pending(
    State(state): State<AppState>,
    _auth: Auth,
    axum::extract::Query(q): axum::extract::Query<CodeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    let code = normalize(&q.user_code)
        .ok_or_else(|| ApiError::invalid("The code has 8 letters, like BCDF-GHJK."))?;
    let row: Option<PendingView> = sqlx::query_as("SELECT user_code, client, ip, created_at, expires_at FROM device_authorizations WHERE user_code = $1 AND approved_at IS NULL AND denied_at IS NULL AND consumed_at IS NULL AND expires_at > now()")
        .bind(&code)
        .fetch_optional(&state.db)
        .await?;
    let r = row.ok_or_else(|| ApiError::invalid("This code is not waiting for approval (it may have expired). Choose Sign in in the app again."))?;
    Ok(Json(json!({ "request": r })))
}

async fn decide(state: &AppState, auth: &Auth, code: &str, approve: bool) -> ApiResult<()> {
    if auth.audience != Audience::Customer {
        return Err(ApiError::Forbidden);
    }
    let code = normalize(code)
        .ok_or_else(|| ApiError::invalid("The code has 8 letters, like BCDF-GHJK."))?;
    let sql = if approve {
        "UPDATE device_authorizations SET approved_by = $2, approved_at = now() WHERE user_code = $1 AND approved_at IS NULL AND denied_at IS NULL AND consumed_at IS NULL AND expires_at > now() RETURNING id, client"
    } else {
        "UPDATE device_authorizations SET denied_at = now() WHERE user_code = $1 AND approved_at IS NULL AND denied_at IS NULL AND consumed_at IS NULL AND expires_at > now() AND $2::uuid IS NOT NULL RETURNING id, client"
    };
    let row: Option<(Uuid, String)> = sqlx::query_as(sql)
        .bind(&code)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?;
    let (id, client) = row.ok_or_else(|| ApiError::invalid("This code is not waiting for approval (it may have expired). Choose Sign in in the app again."))?;
    audit::record(
        &state.db,
        Event::new(
            auth.actor(),
            if approve {
                "device.approved"
            } else {
                "device.denied"
            },
            "device_authorization",
        )
        .resource(id)
        .ip(auth.ip.as_deref())
        .details(json!({ "client": client })),
    )
    .await?;
    Ok(())
}

async fn approve(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<CodeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    decide(&state, &auth, &b.user_code, true).await?;
    Ok(Json(json!({ "approved": true })))
}

async fn deny(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<CodeReq>,
) -> ApiResult<Json<serde_json::Value>> {
    decide(&state, &auth, &b.user_code, false).await?;
    Ok(Json(json!({ "denied": true })))
}

#[derive(Serialize, sqlx::FromRow)]
struct DeviceView {
    id: Uuid,
    client: String,
    created_at: DateTime<Utc>,
    last_used_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

/// The apps signed in to this account (portal: Account).
async fn list_devices(
    State(state): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<DeviceView> = sqlx::query_as("SELECT id, client, created_at, last_used_at, expires_at FROM device_tokens WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now() ORDER BY last_used_at DESC")
        .bind(auth.user_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({ "devices": rows })))
}

async fn revoke_device(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let r = sqlx::query("UPDATE device_tokens SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL")
        .bind(id)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.db,
        Event::new(auth.actor(), "device.revoked", "device_token")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    Ok(Json(json!({ "revoked": true })))
}

/// The Taro app, signed in: `Authorization: Bearer tdv_<id>.<secret>`. It acts as its user in
/// the portal (an [`Auth`] of the customer audience), for the device routes only.
pub struct Device(pub Auth);

impl FromRequestParts<AppState> for Device {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let raw = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|v| v.strip_prefix("tdv_"))
            .ok_or(ApiError::Unauthenticated)?;
        let (id, secret) = raw.split_once('.').ok_or(ApiError::Unauthenticated)?;
        let id = Uuid::parse_str(id).map_err(|_| ApiError::Unauthenticated)?;
        let row: Option<(Uuid, String, Vec<u8>, DateTime<Utc>)> = sqlx::query_as("SELECT t.user_id, u.email, t.secret_hash, t.last_used_at FROM device_tokens t JOIN users u ON u.id = t.user_id WHERE t.id = $1 AND t.revoked_at IS NULL AND t.expires_at > now() AND u.disabled_at IS NULL")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
        let Some((user_id, email, hash, used)) = row else {
            return Err(ApiError::Unauthenticated);
        };
        if !crypto::eq(&hash, &crypto::hash(secret)) {
            return Err(ApiError::Unauthenticated);
        }
        if Utc::now() - used > Duration::minutes(5) {
            sqlx::query("UPDATE device_tokens SET last_used_at = now() WHERE id = $1")
                .bind(id)
                .execute(&state.db)
                .await?;
        }
        Ok(Device(Auth {
            user_id,
            email,
            session_id: id,
            audience: Audience::Customer,
            csrf: String::new(),
            authenticated_at: used,
            operator_roles: vec![],
            ip: crate::identity::client_ip(parts, state),
        }))
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct MacView {
    rental_id: Uuid,
    organization_id: Uuid,
    organization: String,
    plan: String,
    status: String,
    mac_name: Option<String>,
    model: Option<String>,
    region: Option<String>,
    connectable: bool,
    ends_at: Option<DateTime<Utc>>,
}

async fn me(Device(auth): Device) -> Json<serde_json::Value> {
    Json(json!({ "email": auth.email }))
}

/// The Macs assigned to the signed-in user, in every organization, that are not over.
async fn macs(
    State(state): State<AppState>,
    Device(auth): Device,
) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<MacView> = sqlx::query_as(
        "SELECT r.id AS rental_id, r.organization_id, o.name AS organization, p.name AS plan, r.status,
                m.display_name AS mac_name, m.model, m.region,
                (r.status = 'ready' AND m.state = 'assigned' AND m.provider_state = 'in_use') AS connectable,
                r.ends_at
           FROM rentals r
           JOIN organizations o ON o.id = r.organization_id
           JOIN memberships ms ON ms.organization_id = r.organization_id AND ms.user_id = $1
           JOIN mac_plans p ON p.id = r.plan_id
           JOIN rental_assignments a ON a.rental_id = r.id AND a.user_id = $1 AND a.revoked_at IS NULL
           LEFT JOIN mac_resources m ON m.id = r.resource_id
          WHERE r.status NOT IN ('cancelled', 'failed', 'expired')
          ORDER BY (r.status = 'ready') DESC, r.created_at DESC LIMIT 100",
    )
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "macs": rows })))
}

/// Connect: a grant issued and redeemed at once, with all of a grant's checks.
async fn connect(
    State(state): State<AppState>,
    Device(auth): Device,
    Path(rental): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let (grant, _) = crate::connectivity::issue(&state, &auth, rental).await?;
    let (client,): (String,) = sqlx::query_as("SELECT client FROM device_tokens WHERE id = $1")
        .bind(auth.session_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(
        crate::connectivity::redeem_grant(&state, &grant, Some(&client), auth.ip.as_deref())
            .await?,
    ))
}

async fn sign_out(State(state): State<AppState>, Device(auth): Device) -> ApiResult<StatusCode> {
    sqlx::query("UPDATE device_tokens SET revoked_at = now() WHERE id = $1")
        .bind(auth.session_id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state.db,
        Event::new(auth.actor(), "device.signed_out", "device_token").resource(auth.session_id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The portal's side: approve or deny a code, list and revoke signed-in apps (session + CSRF).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/pending", get(pending))
        .route("/api/v1/devices/approve", post(approve))
        .route("/api/v1/devices/deny", post(deny))
        .route("/api/v1/auth/devices", get(list_devices))
        .route("/api/v1/auth/devices/{id}/revoke", post(revoke_device))
}

/// The Taro app's side: no cookie, no browser Origin (a code or a bearer token instead).
pub fn app_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/authorize", post(authorize))
        .route("/api/v1/devices/token", post(token))
        .route("/api/v1/device/me", get(me))
        .route("/api/v1/device/macs", get(macs))
        .route("/api/v1/device/macs/{id}/connect", post(connect))
        .route("/api/v1/device/sign-out", post(sign_out))
}

/// Paths of [`app_routes`]: exempt from the browser Origin check.
pub fn is_app_path(path: &str) -> bool {
    path == "/api/v1/devices/authorize"
        || path == "/api/v1/devices/token"
        || path.starts_with("/api/v1/device/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        for _ in 0..200 {
            let c = user_code();
            assert_eq!(c.len(), 9);
            assert_eq!(normalize(&c).as_deref(), Some(c.as_str()));
            assert_eq!(
                normalize(&c.to_lowercase().replace('-', " ")).as_deref(),
                Some(c.as_str())
            );
        }
        assert_eq!(normalize("bcdf ghjk").as_deref(), Some("BCDF-GHJK"));
        for bad in ["", "BCDF-GHJ", "BCDF-GHJKL", "ABCD-EFGH", "BCDF-GHJ1"] {
            assert!(normalize(bad).is_none(), "{bad}");
        }
        assert!(
            is_app_path("/api/v1/device/macs/x/connect") && !is_app_path("/api/v1/devices/approve")
        );
    }
}
