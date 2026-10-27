//! Connecting to a rented Mac. The portal asks for a connection grant (only the member the Mac
//! is assigned to, only while the rental is ready); the grant lives two minutes and works once.
//! The Taro app redeems it (`POST /api/v1/connect/redeem`) and gets the Mac's Taro ID, relay
//! and password for this rental, fetched live from the provider (Taro Enterprise stores none of
//! them). Relay nodes are checked for reachability; nothing about them is made up.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Actor, Event, Outcome};
use crate::authz::Permission;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::provider;
use crate::state::AppState;

const GRANT_SECONDS: i64 = 120;

/// Issue a one-time grant to connect to `rental` (the member it is assigned to, while it is
/// ready). `auth` is a portal session or the Taro app's sign-in (see devices.rs).
pub async fn issue(
    state: &AppState,
    auth: &Auth,
    rental: Uuid,
) -> ApiResult<(String, chrono::DateTime<Utc>)> {
    let (r, role) = crate::rentals::visible(state, auth, rental).await?;
    if !role.can(Permission::MacsConnect) {
        return Err(ApiError::Forbidden);
    }
    if r.assigned_user_id != Some(auth.user_id) {
        audit::record(
            &state.db,
            Event::new(auth.actor(), "connect.grant", "rental")
                .org(r.organization_id)
                .resource(rental)
                .result(Outcome::Denied)
                .ip(auth.ip.as_deref())
                .details(json!({ "why": "not assigned" })),
        )
        .await?;
        return Err(ApiError::conflict(
            "This Mac is assigned to someone else. Ask an admin to assign it to you.",
        ));
    }
    if r.status != "ready" {
        return Err(ApiError::conflict(format!(
            "The Mac cannot be connected to while the rental is {}.",
            r.status
        )));
    }
    if !crate::ops::flag(state, "connect.grants").await {
        return Err(ApiError::Unavailable(
            "Connecting is paused for a moment. Try again later.".into(),
        ));
    }
    if !state.limiter.allow(
        "grant",
        &auth.user_id.to_string(),
        30,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let (resource, held): (Uuid, bool) =
        sqlx::query_as("SELECT r.resource_id, m.control_by IS NOT NULL FROM rentals r JOIN mac_resources m ON m.id = r.resource_id WHERE r.id = $1")
            .bind(rental)
            .fetch_one(&state.db)
            .await?;
    if held {
        return Err(ApiError::conflict(
            "An administrator is working on this Mac. You can connect again when they hand it back.",
        ));
    }
    let id = Uuid::new_v4();
    let secret = crypto::token(32);
    let expires = Utc::now() + Duration::seconds(GRANT_SECONDS);
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO connection_grants (id, rental_id, resource_id, user_id, secret_hash, expires_at) VALUES ($1, $2, $3, $4, $5, $6)").bind(id).bind(rental).bind(resource).bind(auth.user_id).bind(crypto::hash(&secret)).bind(expires).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "connect.grant", "rental")
            .org(r.organization_id)
            .resource(rental)
            .ip(auth.ip.as_deref())
            .details(json!({ "grant": id, "expires_at": expires })),
    )
    .await?;
    tx.commit().await?;
    Ok((format!("{id}.{secret}"), expires))
}

async fn grant(
    State(state): State<AppState>,
    auth: Auth,
    Path(rental): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let (grant, expires) = issue(&state, &auth, rental).await?;
    let server = state.config.public_url.origin().ascii_serialization();
    let mut link = url::Url::parse("taro://connect").unwrap();
    link.query_pairs_mut()
        .append_pair("server", &server)
        .append_pair("grant", &grant);
    Ok((
        StatusCode::CREATED,
        Json(
            json!({ "grant": grant, "expires_at": expires, "redeem_url": format!("{server}/api/v1/connect/redeem"), "taro_link": link.as_str() }),
        ),
    ))
}

#[derive(Deserialize)]
struct Redeem {
    grant: String,
    client: Option<String>,
}

/// The Taro app trades a grant for the Mac's connection details (once, before it expires, while
/// the rental is still ready and still assigned to the grant's user).
async fn redeem(
    State(state): State<AppState>,
    parts: axum::http::request::Parts,
    Json(b): Json<Redeem>,
) -> ApiResult<Json<serde_json::Value>> {
    let ip = crate::identity::client_ip(&parts, &state);
    if !state.limiter.allow(
        "redeem_ip",
        ip.as_deref().unwrap_or(""),
        30,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    Ok(Json(
        redeem_grant(&state, &b.grant, b.client.as_deref(), ip.as_deref()).await?,
    ))
}

/// Trade a grant for the Mac's connection details (once).
pub async fn redeem_grant(
    state: &AppState,
    grant: &str,
    client: Option<&str>,
    ip: Option<&str>,
) -> ApiResult<serde_json::Value> {
    let bad = || {
        ApiError::invalid(
            "This connection link is not valid any more. Choose Connect again in the portal.",
        )
    };
    let (id, secret) = grant.split_once('.').ok_or_else(bad)?;
    let id: Uuid = id.parse().map_err(|_| bad())?;
    let mut tx = state.db.begin().await?;
    // an administrator's grant: no rental, and only while they still have the Mac
    let admin: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE connection_grants g SET redeemed_at = now() FROM mac_resources m
         WHERE g.id = $1 AND g.secret_hash = $2 AND g.rental_id IS NULL AND g.redeemed_at IS NULL AND g.revoked_at IS NULL AND g.expires_at > now()
           AND m.id = g.resource_id AND m.control_by = g.user_id
         RETURNING g.user_id, g.resource_id",
    )
    .bind(id)
    .bind(crypto::hash(secret))
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((user, resource)) = admin {
        let (ext, name): (String, String) =
            sqlx::query_as("SELECT external_id, display_name FROM mac_resources WHERE id = $1")
                .bind(resource)
                .fetch_one(&mut *tx)
                .await?;
        let conn = provider::bounded("connection", state.provider.connection(&ext))
            .await
            .map_err(|e| {
                ApiError::Unavailable(format!(
                    "The Mac cannot be reached right now ({}). Try again in a minute.",
                    e.message
                ))
            })?;
        sqlx::query("INSERT INTO remote_sessions (id, grant_id, rental_id, user_id, client) VALUES ($1, $2, NULL, $3, $4)").bind(Uuid::new_v4()).bind(id).bind(user).bind(client.map(|c| c.chars().take(80).collect::<String>())).execute(&mut *tx).await?;
        audit::record(
            &mut *tx,
            Event::new(Actor::Operator(user), "connect.redeemed", "mac_resource")
                .resource(resource)
                .ip(ip)
                .details(json!({ "grant": id, "administrator": true })),
        )
        .await?;
        tx.commit().await?;
        state.metrics.inc("taro_connections_total", &[]);
        return Ok(
            json!({ "taro_id": conn.taro_id, "relay": conn.relay, "password": conn.password, "mac_name": name, "rental_ends_at": null }),
        );
    }
    let row: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "UPDATE connection_grants g SET redeemed_at = now() FROM rentals r, rental_assignments a, mac_resources m
         WHERE g.id = $1 AND g.secret_hash = $2 AND g.redeemed_at IS NULL AND g.revoked_at IS NULL AND g.expires_at > now()
           AND r.id = g.rental_id AND r.status = 'ready' AND r.resource_id = g.resource_id
           AND a.rental_id = r.id AND a.user_id = g.user_id AND a.revoked_at IS NULL
           AND m.id = g.resource_id AND m.control_by IS NULL
         RETURNING g.rental_id, g.user_id, g.resource_id",
    )
    .bind(id)
    .bind(crypto::hash(secret))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((rental, user, resource)) = row else {
        audit::record(
            &mut *tx,
            Event::new(Actor::System, "connect.redeem", "grant")
                .resource(id)
                .result(Outcome::Denied)
                .ip(ip),
        )
        .await?;
        tx.commit().await?;
        return Err(bad());
    };
    let (ext, name, org, ends): (String, String, Uuid, Option<chrono::DateTime<Utc>>) = sqlx::query_as("SELECT m.external_id, m.display_name, r.organization_id, r.ends_at FROM mac_resources m JOIN rentals r ON r.resource_id = m.id WHERE m.id = $1 AND r.id = $2").bind(resource).bind(rental).fetch_one(&mut *tx).await?;
    let conn = provider::bounded("connection", state.provider.connection(&ext))
        .await
        .map_err(|e| {
            ApiError::Unavailable(format!(
                "The Mac cannot be reached right now ({}). Try again in a minute.",
                e.message
            ))
        })?;
    sqlx::query("INSERT INTO remote_sessions (id, grant_id, rental_id, user_id, client) VALUES ($1, $2, $3, $4, $5)").bind(Uuid::new_v4()).bind(id).bind(rental).bind(user).bind(client.map(|c| c.chars().take(80).collect::<String>())).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(Actor::User(user), "connect.redeemed", "rental")
            .org(org)
            .resource(rental)
            .ip(ip)
            .details(json!({ "grant": id })),
    )
    .await?;
    tx.commit().await?;
    state.metrics.inc("taro_connections_total", &[]);
    Ok(
        json!({ "taro_id": conn.taro_id, "relay": conn.relay, "password": conn.password, "mac_name": name, "rental_ends_at": ends }),
    )
}

/// Check every relay node: can it be reached over TCP, and how long did that take.
pub async fn check_relays(state: &AppState) -> anyhow::Result<()> {
    let nodes: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, address FROM relay_nodes")
        .fetch_all(&state.db)
        .await?;
    for (id, addr) in nodes {
        let t = std::time::Instant::now();
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::TcpStream::connect(addr.as_str()),
        )
        .await;
        let (ok, err) = match r {
            Ok(Ok(_)) => (true, None),
            Ok(Err(e)) => (false, Some(e.to_string())),
            Err(_) => (false, Some("no answer in 5 s".into())),
        };
        let ms = ok.then(|| t.elapsed().as_millis() as i32);
        sqlx::query("UPDATE relay_nodes SET reachable = $2, latency_ms = $3, last_error = $4, last_check_at = now() WHERE id = $1").bind(id).bind(ok).bind(ms).bind(&err).execute(&state.db).await?;
        if !ok {
            crate::incidents::open(
                state,
                crate::incidents::New {
                    severity: "sev2",
                    component: "relay",
                    title: format!("Relay {addr} cannot be reached"),
                    impact: "Connections through this relay fail.".into(),
                    dedupe_key: Some(format!("relay:{id}")),
                    linked: json!({ "relay_id": id }),
                },
            )
            .await?;
        }
    }
    Ok(())
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/rentals/{id}/connect-grant", post(grant))
}

/// The Taro app's endpoint: no session (a grant is the credential), no CSRF.
pub fn redeem_routes() -> Router<AppState> {
    Router::new().route("/api/v1/connect/redeem", post(redeem))
}
