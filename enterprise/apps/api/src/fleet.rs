//! The provider's own Macs (`TARO_PROVIDER=fleet`): Taro for Mac enrolls them with a one-use
//! code made in the console and keeps a heartbeat; administrators see them in the console and
//! can take one over.
//!
//! * Enrollment (`/api/v1/agent/enroll`, no session): a code made in the console (24 hours, one
//!   use, only its hash kept) gives the Mac its credential `tag_<id>.<secret>` (only a hash kept).
//! * Heartbeat (`/api/v1/agent/heartbeat`, the credential as a Bearer token): the Mac reports its
//!   ID at the relay and the epoch it runs; the server answers with the password of the current
//!   epoch (derived, see provider/fleet.rs). A new epoch ends every session of the old one.
//! * Control (console): an administrator takes a Mac. A Mac assigned to a customer is taken over:
//!   a new epoch ends the customer's session, and the customer cannot connect until the
//!   administrator releases it (a new epoch again ends the administrator's session).

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Actor, Event, Outcome};
use crate::authz::Permission as P;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::provider::fleet::{password, Fleet, ONLINE_SECONDS};
use crate::state::AppState;

const HEARTBEAT_SECONDS: i64 = 10;
const CODE_HOURS: i64 = 24;
/// What an administrator's grant lives (as a customer's).
const GRANT_SECONDS: i64 = 120;

fn session_key(state: &AppState) -> Vec<u8> {
    crypto::derive(state.config.secret_key.expose(), "fleet-session-password")
}

fn relay(state: &AppState) -> Option<String> {
    state.config.relay_address.clone()
}

/// `ABCD-EFGH-JKLM`, from letters and digits that cannot be mistaken for one another.
fn new_code() -> String {
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut r = rand::rngs::OsRng;
    let c: String = (0..12)
        .map(|_| A[r.gen_range(0..A.len())] as char)
        .collect();
    format!("{}-{}-{}", &c[..4], &c[4..8], &c[8..])
}

fn normal(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

// ------------------------------------------------------------------ the Mac

#[derive(Deserialize)]
struct Enroll {
    code: String,
    name: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    chip: String,
    #[serde(default)]
    cpu_cores: i32,
    #[serde(default)]
    memory_gb: i32,
    #[serde(default)]
    storage_gb: i32,
    #[serde(default)]
    macos: String,
    #[serde(default)]
    version: String,
}

fn text(s: &str, max: usize) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

async fn enroll(
    State(state): State<AppState>,
    parts: axum::http::request::Parts,
    Json(b): Json<Enroll>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let ip = crate::identity::client_ip(&parts, &state);
    if !state.limiter.allow(
        "enroll_ip",
        ip.as_deref().unwrap_or(""),
        20,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let name = text(&b.name, 80);
    if name.is_empty() {
        return Err(ApiError::invalid("The Mac has no name."));
    }
    let mut tx = state.db.begin().await?;
    let code: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE mac_enrollment_codes SET used_at = now() WHERE code_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING id",
    )
    .bind(crypto::hash(&normal(&b.code)))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((code_id,)) = code else {
        audit::record(
            &mut *tx,
            Event::new(Actor::System, "fleet.enroll_refused", "mac_agent")
                .result(Outcome::Denied)
                .ip(ip.as_deref()),
        )
        .await?;
        tx.commit().await?;
        return Err(ApiError::invalid(
            "This enrollment code is wrong, used or expired. Make a new one in the console (Macs > Add a Mac).",
        ));
    };
    let id = Uuid::new_v4();
    let secret = crypto::token(32);
    sqlx::query("INSERT INTO mac_agents (id, secret_hash, display_name, model, chip, cpu_cores, memory_gb, storage_gb, macos, version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
        .bind(id)
        .bind(crypto::hash(&secret))
        .bind(&name)
        .bind(text(&b.model, 80))
        .bind(text(&b.chip, 80))
        .bind(b.cpu_cores.clamp(0, 1024))
        .bind(b.memory_gb.clamp(0, 65536))
        .bind(b.storage_gb.clamp(0, 1 << 20))
        .bind(text(&b.macos, 80))
        .bind(text(&b.version, 40))
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE mac_enrollment_codes SET agent_id = $2 WHERE id = $1")
        .bind(code_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(Actor::System, "fleet.enrolled", "mac_agent")
            .resource(id)
            .ip(ip.as_deref())
            .details(json!({ "name": name })),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "agent_id": id, "token": format!("tag_{id}.{secret}"), "heartbeat_seconds": HEARTBEAT_SECONDS }),
        ),
    ))
}

/// The Mac's credential from `Authorization: Bearer tag_<id>.<secret>`.
async fn agent_of(state: &AppState, headers: &HeaderMap) -> ApiResult<Uuid> {
    let t = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|v| v.strip_prefix("tag_"))
        .ok_or(ApiError::Unauthenticated)?;
    let (id, secret) = t.split_once('.').ok_or(ApiError::Unauthenticated)?;
    let id: Uuid = id.parse().map_err(|_| ApiError::Unauthenticated)?;
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT secret_hash FROM mac_agents WHERE id = $1 AND removed_at IS NULL")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    match row {
        Some((h,)) if crypto::eq(&h, &crypto::hash(secret)) => Ok(id),
        _ => Err(ApiError::Unauthenticated),
    }
}

#[derive(Deserialize)]
struct Beat {
    #[serde(default)]
    taro_id: Option<String>,
    #[serde(default)]
    applied_epoch: i32,
    #[serde(default)]
    version: String,
}

fn valid_taro_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<Beat>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = agent_of(&state, &headers).await?;
    let taro_id = b.taro_id.filter(|t| valid_taro_id(t));
    // (an epoch from the future is not believed)
    let (epoch, was_online): (i32, bool) = sqlx::query_as(
        "UPDATE mac_agents SET taro_id = coalesce($2, taro_id), applied_epoch = LEAST($3, epoch), version = CASE WHEN $4 = '' THEN version ELSE $4 END,
            last_seen_at = now()
         WHERE id = $1
         RETURNING epoch, (SELECT coalesce(last_seen_at > now() - make_interval(secs => 60), false) FROM mac_agents p WHERE p.id = $1)",
    )
    .bind(id)
    .bind(taro_id)
    .bind(b.applied_epoch)
    .bind(text(&b.version, 40))
    .fetch_one(&state.db)
    .await?;
    if !was_online {
        // back (or here for the first time): available for new rentals at once
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::reconcile::sync_inventory(&s).await {
                tracing::warn!(error = %e, "inventory after a Mac came online");
            }
        });
    }
    Ok(Json(json!({
        "password": password(&session_key(&state), id, epoch),
        "epoch": epoch,
        "relay": relay(&state),
        "heartbeat_seconds": HEARTBEAT_SECONDS,
    })))
}

// ------------------------------------------------------------------ the console

#[derive(Serialize, sqlx::FromRow)]
struct MacRow {
    id: Uuid,
    agent_id: Option<Uuid>,
    display_name: String,
    model: String,
    chip: String,
    macos: Option<String>,
    agent_version: Option<String>,
    online: bool,
    last_seen_at: Option<DateTime<Utc>>,
    taro_id: Option<String>,
    state: String,
    hold_reason: Option<String>,
    rental_id: Option<Uuid>,
    rental_status: Option<String>,
    assigned_email: Option<String>,
    organization: Option<String>,
    control_by: Option<Uuid>,
    control_email: Option<String>,
    control_since: Option<DateTime<Utc>>,
}

const LIVE: &str = "('provisioning', 'ready', 'suspended', 'cancel_pending')";

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<serde_json::Value>> {
    auth.require_op(&state, P::OpsInventoryRead).await?;
    let rows: Vec<MacRow> = sqlx::query_as(&format!(
        "SELECT m.id, a.id AS agent_id, m.display_name, m.model, m.chip, a.macos, a.version AS agent_version,
            coalesce(a.last_seen_at > now() - make_interval(secs => {ONLINE_SECONDS}), false) AS online, a.last_seen_at, a.taro_id,
            m.state, m.hold_reason, r.id AS rental_id, r.status AS rental_status, u.email AS assigned_email, o.name AS organization,
            m.control_by, cu.email AS control_email, m.control_since
         FROM mac_resources m
         JOIN provider_integrations p ON p.id = m.provider_id
         LEFT JOIN mac_agents a ON a.id::text = m.external_id
         LEFT JOIN rentals r ON r.resource_id = m.id AND r.status IN {LIVE}
         LEFT JOIN organizations o ON o.id = r.organization_id
         LEFT JOIN LATERAL (SELECT user_id FROM rental_assignments WHERE rental_id = r.id AND revoked_at IS NULL ORDER BY assigned_at DESC LIMIT 1) ra ON true
         LEFT JOIN users u ON u.id = ra.user_id
         LEFT JOIN users cu ON cu.id = m.control_by
         WHERE p.id = $1 AND (a.id IS NULL OR a.removed_at IS NULL) AND m.state <> 'released'
         ORDER BY m.display_name"
    ))
    .bind(state.provider_id)
    .fetch_all(&state.db)
    .await?;
    let macs: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|m| {
            let control = m.control_by.map(
                |_| json!({ "by_email": m.control_email, "since": m.control_since, "mine": m.control_by == Some(auth.user_id) }),
            );
            json!({
                "id": m.id, "agent_id": m.agent_id, "display_name": m.display_name, "model": m.model, "chip": m.chip,
                "macos": m.macos, "agent_version": m.agent_version, "online": m.online, "last_seen_at": m.last_seen_at,
                "taro_id": m.taro_id, "state": m.state, "hold_reason": m.hold_reason, "rental_id": m.rental_id,
                "rental_status": m.rental_status, "assigned_email": m.assigned_email, "organization": m.organization,
                "control": control,
            })
        })
        .collect();
    Ok(Json(json!({
        "macs": macs,
        "provider": state.provider.kind(),
        "enroll": { "server": state.config.public_url.origin().ascii_serialization(), "relay": relay(&state) },
    })))
}

#[derive(Deserialize)]
struct NewCode {
    #[serde(default)]
    label: Option<String>,
}

async fn make_code(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<NewCode>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let code = new_code();
    let expires = Utc::now() + Duration::hours(CODE_HOURS);
    let label = b.label.map(|l| text(&l, 80)).filter(|l| !l.is_empty());
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO mac_enrollment_codes (id, code_hash, label, created_by, expires_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(crypto::hash(&normal(&code)))
        .bind(&label)
        .bind(auth.user_id)
        .bind(expires)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "fleet.code_made", "mac_enrollment_code")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "code": code, "expires_at": expires, "server": state.config.public_url.origin().ascii_serialization() }),
        ),
    ))
}

struct Target {
    agent: Option<Uuid>,
    control_by: Option<Uuid>,
    rental: Option<(Uuid, String)>,
}

async fn target(state: &AppState, id: Uuid) -> ApiResult<Target> {
    let row: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT a.id, m.control_by FROM mac_resources m LEFT JOIN mac_agents a ON a.id::text = m.external_id AND a.removed_at IS NULL WHERE m.id = $1 AND m.provider_id = $2",
    )
    .bind(id)
    .bind(state.provider_id)
    .fetch_optional(&state.db)
    .await?;
    let (agent, control_by) = row.ok_or(ApiError::NotFound)?;
    let rental: Option<(Uuid, String)> = sqlx::query_as(&format!(
        "SELECT id, status FROM rentals WHERE resource_id = $1 AND status IN {LIVE} LIMIT 1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Target {
        agent,
        control_by,
        rental,
    })
}

/// An administrator takes the Mac (a customer's is taken over) and gets a link that opens it in
/// the Taro app.
async fn control(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let t = target(&state, id).await?;
    let agent = t.agent.ok_or_else(|| {
        ApiError::conflict("Only Macs enrolled with Taro for Mac can be controlled from here.")
    })?;
    if let Some(other) = t.control_by.filter(|u| *u != auth.user_id) {
        let (email,): (String,) = sqlx::query_as("SELECT email FROM users WHERE id = $1")
            .bind(other)
            .fetch_one(&state.db)
            .await?;
        return Err(ApiError::conflict(format!(
            "{email} is working on this Mac. Release it first."
        )));
    }
    let took_over = t.rental.is_some() && t.control_by.is_none();
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE mac_resources SET control_by = $2, control_since = coalesce(control_since, now()), updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(auth.user_id)
        .execute(&mut *tx)
        .await?;
    let grant = Uuid::new_v4();
    let secret = crypto::token(32);
    let expires = Utc::now() + Duration::seconds(GRANT_SECONDS);
    sqlx::query("INSERT INTO connection_grants (id, rental_id, resource_id, user_id, secret_hash, expires_at) VALUES ($1, NULL, $2, $3, $4, $5)")
        .bind(grant)
        .bind(id)
        .bind(auth.user_id)
        .bind(crypto::hash(&secret))
        .bind(expires)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(
            auth.actor(),
            if took_over {
                "fleet.taken_over"
            } else {
                "fleet.controlled"
            },
            "mac_resource",
        )
        .resource(id)
        .ip(auth.ip.as_deref())
        .details(json!({ "rental": t.rental.as_ref().map(|r| r.0), "grant": grant })),
    )
    .await?;
    tx.commit().await?;
    if took_over {
        // the customer's session ends; they cannot connect until the Mac is released
        Fleet::new_epoch(&state.db, agent).await?;
    }
    let server = state.config.public_url.origin().ascii_serialization();
    let mut link = url::Url::parse("taro://connect").unwrap();
    link.query_pairs_mut()
        .append_pair("server", &server)
        .append_pair("grant", &format!("{grant}.{secret}"));
    Ok((
        StatusCode::CREATED,
        Json(json!({ "taro_link": link.as_str(), "expires_at": expires, "took_over": took_over })),
    ))
}

/// The administrator hands the Mac back: their session ends, its customer can connect again.
async fn release(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let t = target(&state, id).await?;
    if t.control_by.is_none() {
        return Ok(Json(json!({ "released": true })));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE mac_resources SET control_by = NULL, control_since = NULL, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // grants not yet used are void
    sqlx::query("UPDATE connection_grants SET revoked_at = now() WHERE resource_id = $1 AND rental_id IS NULL AND redeemed_at IS NULL AND revoked_at IS NULL")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "fleet.released", "mac_resource")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "held_by": t.control_by, "rental": t.rental.as_ref().map(|r| r.0) })),
    )
    .await?;
    tx.commit().await?;
    if let Some(a) = t.agent {
        Fleet::new_epoch(&state.db, a).await?;
    }
    Ok(Json(json!({ "released": true })))
}

/// The Mac leaves the fleet (its credential stops working). Not while it is assigned.
async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let t = target(&state, id).await?;
    if t.rental.is_some() {
        return Err(ApiError::conflict(
            "This Mac is assigned. End the assignment first.",
        ));
    }
    let agent = t.agent.ok_or(ApiError::NotFound)?;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE mac_agents SET removed_at = now() WHERE id = $1")
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE mac_resources SET state = 'released', control_by = NULL, control_since = NULL, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "fleet.removed", "mac_resource")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The Mac's endpoints (on the portal's origin, where Macs reach the server).
pub fn agent_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/agent/enroll", post(enroll))
        .route("/api/v1/agent/heartbeat", post(heartbeat))
}

/// Requests from a Mac: no browser, no Origin check (a credential or a code is the proof).
pub fn is_agent_path(path: &str) -> bool {
    path.starts_with("/api/v1/agent/")
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ops/fleet", get(list))
        .route("/api/v1/ops/fleet/enrollment-codes", post(make_code))
        .route("/api/v1/ops/fleet/{id}/control", post(control))
        .route("/api/v1/ops/fleet/{id}/release", post(release))
        .route("/api/v1/ops/fleet/{id}", delete(remove))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_codes() {
        let c = new_code();
        assert_eq!(c.len(), 14);
        assert_eq!(c.matches('-').count(), 2);
        assert!(!c.contains('0') && !c.contains('O') && !c.contains('1') && !c.contains('I'));
        assert_eq!(normal(&c.to_lowercase()), normal(&c));
        assert!(valid_taro_id("rm-123456789") && !valid_taro_id("a b") && !valid_taro_id(""));
    }
}
