//! The Provider Console's API (`/api/v1/ops/*`), on the ops listener only, for operator
//! sessions only, each route behind its own operator permission. Customers cannot reach it: the
//! customer listener does not serve these routes, and an ops session needs an operator role.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::authz::{OperatorRole, Permission as P};
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::state::AppState;

fn metric(value: impl Serialize, definition: &str, window: &str, source: &str) -> Value {
    json!({ "value": value, "definition": definition, "window": window, "source": source, "as_of": Utc::now() })
}

async fn overview(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsOverviewRead).await?;
    let db = &state.db;
    let (active,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM rentals WHERE status IN ('ready', 'suspended')")
            .fetch_one(db)
            .await?;
    let (available,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM mac_resources WHERE state = 'available' AND hold_reason IS NULL",
    )
    .fetch_one(db)
    .await?;
    let (running,): (i64,) = sqlx::query_as("SELECT count(*) FROM provisioning_jobs WHERE state IN ('queued', 'running', 'waiting_provider', 'retryable_failed')").fetch_one(db).await?;
    let (done, failed): (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE state IN ('succeeded', 'permanent_failed')), count(*) FILTER (WHERE state = 'permanent_failed') FROM provisioning_jobs WHERE completed_at > now() - interval '24 hours'").fetch_one(db).await?;
    let providers: Vec<(String, String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT name, kind, health, last_health_at FROM provider_integrations ORDER BY name",
    )
    .fetch_all(db)
    .await?;
    let (relays, reachable, relay_checked): (i64, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE reachable), max(last_check_at) FROM relay_nodes",
    )
    .fetch_one(db)
    .await?;
    let (backlog, oldest): (i64, Option<DateTime<Utc>>) = sqlx::query_as("SELECT count(*), min(received_at) FROM stripe_webhook_events WHERE state IN ('pending', 'failed')").fetch_one(db).await?;
    let (dead,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM stripe_webhook_events WHERE state = 'dead'")
            .fetch_one(db)
            .await?;
    let (tls_failures,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM domain_assignments WHERE state = 'error'")
            .fetch_one(db)
            .await?;
    let incidents: Vec<(String, i64)> = sqlx::query_as("SELECT severity, count(*) FROM incidents WHERE status <> 'resolved' GROUP BY severity ORDER BY severity").fetch_all(db).await?;
    Ok(Json(json!({
        "simulated_provider": state.provider.is_simulated(),
        "active_rentals": metric(active, "Rentals that are ready or suspended", "now", "rentals"),
        "available_resources": metric(available, "Macs free for new rentals (available and not held)", "now", "mac_resources (Taro's view, reconciled with the provider every minute)"),
        "provisioning_jobs_running": metric(running, "Provisioning jobs queued, running, waiting on the provider or waiting to retry", "now", "provisioning_jobs"),
        "provisioning_failure_rate": metric(if done > 0 { Some(failed as f64 / done as f64) } else { None }, "Jobs that failed permanently / jobs that finished", "last 24 hours", "provisioning_jobs"),
        "provider_health": metric(providers.iter().map(|(n, k, h, at)| json!({ "name": n, "kind": k, "health": h, "checked_at": at })).collect::<Vec<_>>(), "Each provider adapter's own health check", "last check", "provider_integrations"),
        "relay_health": metric(json!({ "nodes": relays, "reachable": reachable, "checked_at": relay_checked }), "Relay nodes reachable over TCP (no other relay telemetry is collected)", "last check", "relay_nodes"),
        "webhook_backlog": metric(json!({ "pending": backlog, "oldest_received_at": oldest, "dead": dead }), "Stripe events stored but not processed yet; dead: given up after 8 tries", "now", "stripe_webhook_events"),
        "tls_failures": metric(tls_failures, "Custom domains failing their HTTPS check", "last check (every 5 minutes)", "domain_assignments"),
        "open_incidents": metric(incidents.into_iter().map(|(s, n)| json!({ "severity": s, "count": n })).collect::<Vec<_>>(), "Incidents not resolved, by severity", "now", "incidents"),
    })))
}

#[derive(Deserialize, Default)]
pub struct Filter {
    q: Option<String>,
    state: Option<String>,
    region: Option<String>,
    chip: Option<String>,
    organization_id: Option<Uuid>,
    before: Option<i64>,
    limit: Option<i64>,
}

async fn organizations(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsOrgsRead).await?;
    let q = f.q.map(|q| format!("%{}%", q.replace(['%', '_'], "")));
    let rows: Vec<(Uuid, String, String, DateTime<Utc>, Option<DateTime<Utc>>, i64, i64)> = sqlx::query_as(
        "SELECT o.id, o.name, o.slug, o.created_at, o.suspended_at,
            (SELECT count(*) FROM memberships m WHERE m.organization_id = o.id),
            (SELECT count(*) FROM rentals r WHERE r.organization_id = o.id AND r.status IN ('ready', 'suspended', 'provisioning', 'paid'))
         FROM organizations o WHERE ($1::text IS NULL OR o.name ILIKE $1 OR o.slug ILIKE $1) ORDER BY o.created_at DESC LIMIT 200",
    )
    .bind(q)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        json!({ "organizations": rows.into_iter().map(|(id, name, slug, at, sus, members, live)| json!({ "id": id, "name": name, "slug": slug, "created_at": at, "suspended_at": sus, "members": members, "live_rentals": live })).collect::<Vec<_>>() }),
    ))
}

async fn organization(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsOrgsRead).await?;
    let o: crate::organizations::Organization = sqlx::query_as(
        "SELECT id, name, slug, created_at, suspended_at FROM organizations WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let rentals: Vec<crate::rentals::Rental> = sqlx::query_as(&format!(
        "{} WHERE r.organization_id = $1 ORDER BY r.created_at DESC LIMIT 100",
        crate::rentals::rental_select()
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let ent: Vec<(String, String)> = sqlx::query_as(
        "SELECT feature, source FROM entitlements WHERE organization_id = $1 ORDER BY feature",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let members: Vec<(String, String)> = sqlx::query_as("SELECT u.email, m.role FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.organization_id = $1 ORDER BY m.created_at").bind(id).fetch_all(&state.db).await?;
    Ok(Json(
        json!({ "organization": o, "rentals": rentals, "entitlements": ent.into_iter().map(|(f, s)| json!({ "feature": f, "source": s })).collect::<Vec<_>>(), "members": members.into_iter().map(|(e, r)| json!({ "email": e, "role": r })).collect::<Vec<_>>() }),
    ))
}

async fn grant_entitlement(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, feature)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsConfigManage).await?;
    if !crate::branding::MODULES.contains(&feature.as_str()) {
        return Err(ApiError::invalid("Unknown feature."));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO entitlements (organization_id, feature, source, granted_by) VALUES ($1, $2, 'manual', $3) ON CONFLICT DO NOTHING").bind(org).bind(&feature).bind(auth.user_id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "entitlement.granted", "organization")
            .org(org)
            .resource(&feature)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_entitlement(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, feature)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsConfigManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("DELETE FROM entitlements WHERE organization_id = $1 AND feature = $2")
        .bind(org)
        .bind(&feature)
        .execute(&mut *tx)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "entitlement.revoked", "organization")
            .org(org)
            .resource(&feature)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ inventory and jobs

async fn resources(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsInventoryRead).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct Res {
        id: Uuid,
        provider: String,
        external_id: String,
        display_name: String,
        model: String,
        chip: String,
        cpu_cores: i32,
        memory_gb: i32,
        storage_gb: i32,
        region: String,
        state: String,
        provider_state: String,
        hold_reason: Option<String>,
        last_synced_at: Option<DateTime<Utc>>,
        rental_id: Option<Uuid>,
        organization: Option<String>,
    }
    let rows: Vec<Res> = sqlx::query_as(
        "SELECT m.id, p.name AS provider, m.external_id, m.display_name, m.model, m.chip, m.cpu_cores, m.memory_gb, m.storage_gb, m.region, m.state, m.provider_state, m.hold_reason, m.last_synced_at, r.id AS rental_id, o.name AS organization
         FROM mac_resources m JOIN provider_integrations p ON p.id = m.provider_id
         LEFT JOIN rentals r ON r.resource_id = m.id AND r.status IN ('provisioning', 'ready', 'suspended', 'cancel_pending')
         LEFT JOIN organizations o ON o.id = r.organization_id
         WHERE ($1::text IS NULL OR m.state = $1) AND ($2::text IS NULL OR m.region = $2) AND ($3::text IS NULL OR m.chip = $3)
         ORDER BY m.display_name",
    )
    .bind(f.state)
    .bind(f.region)
    .bind(f.chip)
    .fetch_all(&state.db)
    .await?;
    let caps = crate::provider::bounded("capabilities", state.provider.list_capabilities())
        .await
        .ok();
    Ok(Json(
        json!({ "resources": rows, "capabilities": caps, "simulated_provider": state.provider.is_simulated() }),
    ))
}

async fn resource_diagnostics(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsInventoryRead).await?;
    let (ext,): (String,) = sqlx::query_as("SELECT external_id FROM mac_resources WHERE id = $1")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let d = crate::provider::bounded("diagnostics", state.provider.safe_diagnostics(&ext))
        .await
        .map_err(|e| ApiError::Unavailable(e.message))?;
    Ok(Json(json!({ "diagnostics": d })))
}

#[derive(Deserialize)]
struct Hold {
    reason: String,
}

/// Keep a Mac out of new rentals (it changes no hardware state; the rental on it, if any, goes on).
async fn hold(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(b): Json<Hold>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let reason = b.reason.trim();
    if reason.is_empty() || reason.len() > 200 {
        return Err(ApiError::invalid("Say why (up to 200 characters)."));
    }
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE mac_resources SET hold_reason = $2, state = CASE WHEN state = 'available' THEN 'maintenance' ELSE state END, updated_at = now() WHERE id = $1").bind(id).bind(reason).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "inventory.held", "mac_resource")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "reason": reason })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unhold(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsInventoryManage).await?;
    let mut tx = state.db.begin().await?;
    // back to unavailable: the next reconciliation makes it available if the provider has it free
    let r = sqlx::query("UPDATE mac_resources SET hold_reason = NULL, state = CASE WHEN state = 'maintenance' THEN 'unavailable' ELSE state END, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "inventory.released_hold", "mac_resource")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn jobs(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsJobsRead).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct J {
        id: Uuid,
        rental_id: Uuid,
        provider_id: Option<Uuid>,
        operation: String,
        state: String,
        attempts: i32,
        max_attempts: i32,
        run_after: DateTime<Utc>,
        last_error: Option<String>,
        correlation_id: Uuid,
        created_at: DateTime<Utc>,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
        rental_status: String,
        organization: String,
    }
    let rows: Vec<J> = sqlx::query_as(
        "SELECT j.id, j.rental_id, j.provider_id, j.operation, j.state, j.attempts, j.max_attempts, j.run_after, j.last_error, j.correlation_id, j.created_at, j.started_at, j.completed_at, r.status AS rental_status, o.name AS organization
         FROM provisioning_jobs j JOIN rentals r ON r.id = j.rental_id JOIN organizations o ON o.id = r.organization_id
         WHERE ($1::text IS NULL OR j.state = $1) ORDER BY j.created_at DESC LIMIT $2",
    )
    .bind(f.state)
    .bind(f.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&state.db)
    .await?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|j| {
            let final_rental =
                crate::rentals::Status::parse(&j.rental_status).is_some_and(|s| s.is_final());
            let retry = matches!(j.state.as_str(), "permanent_failed" | "retryable_failed")
                && !(j.operation == "provision" && final_rental);
            let mut v = serde_json::to_value(&j).unwrap_or_default();
            v["retry_eligible"] = json!(retry);
            v
        })
        .collect();
    Ok(Json(json!({ "jobs": out })))
}

async fn retry_job(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsJobsManage).await?;
    let mut tx = state.db.begin().await?;
    let (op, st, rental_status): (String, String, String) = sqlx::query_as("SELECT j.operation, j.state, r.status FROM provisioning_jobs j JOIN rentals r ON r.id = j.rental_id WHERE j.id = $1 FOR UPDATE OF j").bind(id).fetch_one(&mut *tx).await?;
    if !matches!(st.as_str(), "permanent_failed" | "retryable_failed") {
        return Err(ApiError::conflict(format!("A {st} job is not retried.")));
    }
    if op == "provision"
        && crate::rentals::Status::parse(&rental_status).is_some_and(|s| s.is_final())
    {
        return Err(ApiError::conflict(
            "Its rental has ended; a new rental is needed.",
        ));
    }
    sqlx::query("UPDATE provisioning_jobs SET state = 'queued', attempts = 0, run_after = now(), locked_until = NULL, completed_at = NULL WHERE id = $1").bind(id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "job.retried", "provisioning_job")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_job(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsJobsManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE provisioning_jobs SET state = 'cancelled', completed_at = now(), locked_until = NULL WHERE id = $1 AND state IN ('queued', 'retryable_failed')").bind(id).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "Only a queued job, or one waiting to retry, can be cancelled.",
        ));
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "job.cancelled", "provisioning_job")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reconcile_now(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsJobsManage).await?;
    crate::reconcile::once(&state)
        .await
        .map_err(ApiError::Internal)?;
    audit::record(
        &state.db,
        Event::new(auth.actor(), "reconcile.forced", "platform").ip(auth.ip.as_deref()),
    )
    .await?;
    Ok(Json(json!({ "reconciled_at": Utc::now() })))
}

// ------------------------------------------------------------------ providers and relays

async fn providers(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsProvidersRead).await?;
    let rows: Vec<(Uuid, String, String, Value, bool, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>, bool)> =
        sqlx::query_as("SELECT id, kind, name, config, enabled, health, last_health_at, last_sync_at, last_error, secret_ref IS NOT NULL FROM provider_integrations ORDER BY name").fetch_all(&state.db).await?;
    Ok(Json(
        json!({ "providers": rows.into_iter().map(|(id, kind, name, config, enabled, health, h_at, s_at, err, has_secret)| json!({ "id": id, "kind": kind, "name": name, "config": config, "enabled": enabled, "health": health, "last_health_at": h_at, "last_sync_at": s_at, "last_error": err, "has_secret": has_secret, "active": id == state.provider_id })).collect::<Vec<_>>(), "adapters": ["mock"] }),
    ))
}

#[derive(Deserialize)]
struct NewProvider {
    kind: String,
    name: String,
    config: Option<Value>,
    secret_ref: Option<String>,
}

async fn create_provider(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<NewProvider>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.require_op(&state, P::OpsProvidersManage).await?;
    if b.kind != "mock" {
        return Err(ApiError::invalid(format!(
            "There is no {} adapter yet (implemented: mock).",
            b.kind
        )));
    }
    let name = b.name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err(ApiError::invalid("A name of 1 to 60 characters."));
    }
    // secret references only, e.g. "vault:kv/taro/provider-a": never the secret
    if let Some(r) = &b.secret_ref {
        if r.len() > 200 || !r.contains(':') || r.contains(char::is_whitespace) {
            return Err(ApiError::invalid("secret_ref names a secret in the secret store (scheme:path); it is never the secret itself."));
        }
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO provider_integrations (id, kind, name, config, secret_ref) VALUES ($1, $2, $3, $4, $5)").bind(id).bind(&b.kind).bind(name).bind(b.config.unwrap_or(json!({}))).bind(&b.secret_ref).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "provider.created", "provider_integration")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "kind": b.kind, "name": name })),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn relays(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsRelaysRead).await?;
    let rows: Vec<(Uuid, String, String, String, Option<bool>, Option<i32>, Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as("SELECT id, name, address, region, reachable, latency_ms, last_check_at, last_error FROM relay_nodes ORDER BY name").fetch_all(&state.db).await?;
    Ok(Json(json!({
        "relays": rows.into_iter().map(|(id, name, address, region, ok, ms, at, err)| json!({ "id": id, "name": name, "address": address, "region": region, "reachable": ok, "connect_ms": ms, "last_check_at": at, "last_error": err })).collect::<Vec<_>>(),
        "telemetry": "reachability and TCP connect time only; the relay does not report sessions, latency or loss",
    })))
}

#[derive(Deserialize)]
struct NewRelay {
    name: String,
    address: String,
    region: String,
}

async fn create_relay(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<NewRelay>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.require_op(&state, P::OpsRelaysManage).await?;
    let (host, port) = b
        .address
        .trim()
        .rsplit_once(':')
        .ok_or_else(|| ApiError::invalid("The address is host:port."))?;
    if host.is_empty()
        || port.parse::<u16>().is_err()
        || b.name.trim().is_empty()
        || b.region.trim().is_empty()
    {
        return Err(ApiError::invalid(
            "A name, a region and an address host:port.",
        ));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO relay_nodes (id, name, address, region) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(b.name.trim())
        .bind(b.address.trim())
        .bind(b.region.trim())
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "relay.created", "relay_node")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

async fn delete_relay(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsRelaysManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("DELETE FROM relay_nodes WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "relay.deleted", "relay_node")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn check_relays(State(state): State<AppState>, auth: Auth) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsRelaysManage).await?;
    crate::connectivity::check_relays(&state)
        .await
        .map_err(ApiError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ billing operations

async fn webhook_events(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsBillingRead).await?;
    // the payload stays in the database: type, state and errors are enough to operate
    let rows: Vec<(String, String, bool, String, i32, Option<String>, DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as("SELECT id, type, livemode, state, attempts, last_error, received_at, processed_at FROM stripe_webhook_events WHERE ($1::text IS NULL OR state = $1) ORDER BY received_at DESC LIMIT 200").bind(f.state).fetch_all(&state.db).await?;
    Ok(Json(
        json!({ "events": rows.into_iter().map(|(id, t, live, st, n, err, at, done)| json!({ "id": id, "type": t, "livemode": live, "state": st, "attempts": n, "last_error": err, "received_at": at, "processed_at": done })).collect::<Vec<_>>() }),
    ))
}

async fn replay_event(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsBillingManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE stripe_webhook_events SET state = 'pending', attempts = 0, next_attempt_at = now() WHERE id = $1 AND state IN ('failed', 'dead')").bind(&id).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::conflict("Only a failed or dead event is replayed (processing is idempotent, but processed events need no replay)."));
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "billing.event_replayed", "stripe_event")
            .resource(&id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn billing_rentals(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsBillingRead).await?;
    let rows: Vec<crate::rentals::Rental> = sqlx::query_as(&format!("{} WHERE r.billing_state IN ('past_due', 'refunded') OR (r.status = 'failed' AND r.billing_state = 'paid') ORDER BY r.updated_at DESC LIMIT 200", crate::rentals::rental_select())).fetch_all(&state.db).await?;
    Ok(Json(json!({ "needs_attention": rows })))
}

// ------------------------------------------------------------------ incidents, audit, access, flags

async fn incidents(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsIncidentsRead).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct I {
        id: Uuid,
        severity: String,
        component: String,
        title: String,
        impact: String,
        status: String,
        first_seen: DateTime<Utc>,
        last_seen: DateTime<Utc>,
        occurrences: i32,
        owner_email: Option<String>,
        mitigation: String,
        resolution: String,
        linked: Value,
        resolved_at: Option<DateTime<Utc>>,
    }
    let rows: Vec<I> = sqlx::query_as("SELECT i.id, i.severity, i.component, i.title, i.impact, i.status, i.first_seen, i.last_seen, i.occurrences, u.email AS owner_email, i.mitigation, i.resolution, i.linked, i.resolved_at FROM incidents i LEFT JOIN users u ON u.id = i.owner_id WHERE ($1::text IS NULL OR i.status = $1) ORDER BY i.status = 'resolved', i.severity, i.last_seen DESC LIMIT 200").bind(f.state).fetch_all(&state.db).await?;
    Ok(Json(json!({ "incidents": rows })))
}

#[derive(Deserialize)]
struct IncidentChange {
    status: Option<String>,
    mitigation: Option<String>,
    resolution: Option<String>,
    take_ownership: Option<bool>,
}

async fn update_incident(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(b): Json<IncidentChange>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsIncidentsManage).await?;
    if let Some(s) = &b.status {
        if !["open", "mitigated", "resolved"].contains(&s.as_str()) {
            return Err(ApiError::invalid("status is open, mitigated or resolved"));
        }
        if s == "resolved" && b.resolution.as_deref().unwrap_or("").trim().is_empty() {
            return Err(ApiError::invalid("Write what resolved it."));
        }
    }
    let mut tx = state.db.begin().await?;
    let r = sqlx::query(
        "UPDATE incidents SET status = coalesce($2, status), mitigation = coalesce($3, mitigation), resolution = coalesce($4, resolution),
            owner_id = CASE WHEN $5 THEN $6 ELSE owner_id END, resolved_at = CASE WHEN $2 = 'resolved' THEN now() WHEN $2 IS NOT NULL THEN NULL ELSE resolved_at END WHERE id = $1",
    )
    .bind(id)
    .bind(&b.status)
    .bind(&b.mitigation)
    .bind(&b.resolution)
    .bind(b.take_ownership.unwrap_or(false))
    .bind(auth.user_id)
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "incident.updated", "incident")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "status": b.status })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn audit_events(
    State(state): State<AppState>,
    auth: Auth,
    Query(f): Query<Filter>,
) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsAuditRead).await?;
    let rows = audit::list(
        &state.db,
        f.organization_id,
        f.before,
        f.limit.unwrap_or(100),
    )
    .await?;
    Ok(Json(json!({ "events": rows })))
}

async fn access(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsAccessRead).await?;
    let rows: Vec<(Uuid, String, String, DateTime<Utc>, Option<String>)> = sqlx::query_as("SELECT g.user_id, u.email, g.role, g.created_at, gb.email FROM operator_grants g JOIN users u ON u.id = g.user_id LEFT JOIN users gb ON gb.id = g.granted_by ORDER BY u.email, g.role").fetch_all(&state.db).await?;
    Ok(Json(json!({
        "grants": rows.into_iter().map(|(id, email, role, at, by)| json!({ "user_id": id, "email": email, "role": role, "granted_at": at, "granted_by": by })).collect::<Vec<_>>(),
        "roles": OperatorRole::ALL.iter().map(|r| json!({ "key": r.key(), "description": r.description(), "permissions": r.permissions().iter().map(|p| p.key()).collect::<Vec<_>>() })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct Grant {
    email: String,
    role: String,
}

async fn grant(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<Grant>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsAccessManage).await?;
    auth.require_recent(15)?;
    let role =
        OperatorRole::parse(&b.role).ok_or_else(|| ApiError::invalid("Unknown operator role."))?;
    let email = b.email.trim().to_lowercase();
    let (user,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| {
            ApiError::invalid(
                "That person must sign in to the portal once before they can be made an operator.",
            )
        })?;
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO operator_grants (user_id, role, granted_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING").bind(user).bind(role.key()).bind(auth.user_id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "operator.granted", "user")
            .resource(user)
            .ip(auth.ip.as_deref())
            .details(json!({ "role": role.key() })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke(
    State(state): State<AppState>,
    auth: Auth,
    Path((user, role)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsAccessManage).await?;
    auth.require_recent(15)?;
    let mut tx = state.db.begin().await?;
    if role == "platform_admin" {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM operator_grants WHERE role = 'platform_admin'")
                .fetch_one(&mut *tx)
                .await?;
        if n <= 1 {
            return Err(ApiError::conflict(
                "The platform keeps at least one platform_admin.",
            ));
        }
    }
    let r = sqlx::query("DELETE FROM operator_grants WHERE user_id = $1 AND role = $2")
        .bind(user)
        .bind(&role)
        .execute(&mut *tx)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    // their console sessions end with the last role
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND audience = 'ops' AND revoked_at IS NULL AND NOT EXISTS (SELECT 1 FROM operator_grants WHERE user_id = $1)").bind(user).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "operator.revoked", "user")
            .resource(user)
            .ip(auth.ip.as_deref())
            .details(json!({ "role": role })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn flags(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsFlagsRead).await?;
    let rows: Vec<(String, String, bool, Option<String>, DateTime<Utc>)> = sqlx::query_as("SELECT f.key, f.description, f.enabled, u.email, f.updated_at FROM feature_flags f LEFT JOIN users u ON u.id = f.updated_by ORDER BY f.key").fetch_all(&state.db).await?;
    Ok(Json(
        json!({ "flags": rows.into_iter().map(|(k, d, e, by, at)| json!({ "key": k, "description": d, "enabled": e, "updated_by": by, "updated_at": at })).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct FlagChange {
    enabled: bool,
}

async fn set_flag(
    State(state): State<AppState>,
    auth: Auth,
    Path(key): Path<String>,
    Json(b): Json<FlagChange>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsFlagsManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query(
        "UPDATE feature_flags SET enabled = $2, updated_by = $3, updated_at = now() WHERE key = $1",
    )
    .bind(&key)
    .bind(b.enabled)
    .bind(auth.user_id)
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "flag.changed", "feature_flag")
            .resource(&key)
            .ip(auth.ip.as_deref())
            .details(json!({ "enabled": b.enabled })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A kill switch or rollout flag: off unless it exists and is on.
pub async fn flag(state: &AppState, key: &str) -> bool {
    sqlx::query_as::<_, (bool,)>("SELECT enabled FROM feature_flags WHERE key = $1")
        .bind(key)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .is_some_and(|(e,)| e)
}

// ------------------------------------------------------------------ catalog management

async fn plans(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsOverviewRead).await?;
    let rows: Vec<crate::rentals::Plan> = sqlx::query_as("SELECT id, slug, name, description, chip, cpu_cores, memory_gb, storage_gb, region, billing, duration_hours, price_minor, currency, active FROM mac_plans ORDER BY active DESC, price_minor").fetch_all(&state.db).await?;
    Ok(Json(json!({ "plans": rows })))
}

#[derive(Deserialize)]
struct NewPlan {
    slug: String,
    name: String,
    description: Option<String>,
    chip: String,
    cpu_cores: i32,
    memory_gb: i32,
    storage_gb: i32,
    region: String,
    billing: String,
    duration_hours: Option<i32>,
    price_minor: i64,
    currency: String,
}

async fn create_plan(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<NewPlan>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    auth.require_op(&state, P::OpsConfigManage).await?;
    if !b
        .slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        || b.slug.len() < 2
        || b.price_minor < 0
        || !["one_time", "monthly"].contains(&b.billing.as_str())
        || (b.billing == "one_time") != b.duration_hours.is_some()
    {
        return Err(ApiError::invalid("A slug (a-z, 0-9, -), a price ≥ 0, billing one_time (with duration_hours) or monthly (without)."));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO mac_plans (id, slug, name, description, chip, cpu_cores, memory_gb, storage_gb, region, billing, duration_hours, price_minor, currency) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)")
        .bind(id)
        .bind(&b.slug)
        .bind(&b.name)
        .bind(b.description.unwrap_or_default())
        .bind(&b.chip)
        .bind(b.cpu_cores)
        .bind(b.memory_gb)
        .bind(b.storage_gb)
        .bind(&b.region)
        .bind(&b.billing)
        .bind(b.duration_hours)
        .bind(b.price_minor)
        .bind(b.currency.to_lowercase())
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "plan.created", "mac_plan")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "slug": b.slug, "price_minor": b.price_minor })),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

#[derive(Deserialize)]
struct PlanChange {
    active: bool,
}

/// Plans are retired, not edited: a rental keeps the price it was made with.
async fn set_plan(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(b): Json<PlanChange>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, P::OpsConfigManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE mac_plans SET active = $2 WHERE id = $1")
        .bind(id)
        .bind(b.active)
        .execute(&mut *tx)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "plan.changed", "mac_plan")
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "active": b.active })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn domains(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_op(&state, P::OpsOverviewRead).await?;
    let rows: Vec<crate::domains::Domain> = sqlx::query_as("SELECT id, organization_id, hostname, kind, state, verification_token, verified_at, tls_checked_at, tls_expires_at, last_check_at, last_error, created_at FROM domain_assignments ORDER BY created_at DESC").fetch_all(&state.db).await?;
    // the verification tokens are the customers': not shown here
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|d| {
            let mut v = serde_json::to_value(d).unwrap_or_default();
            v["verification_token"] = Value::Null;
            v
        })
        .collect();
    Ok(Json(json!({ "domains": rows })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ops/overview", get(overview))
        .route("/api/v1/ops/organizations", get(organizations))
        .route("/api/v1/ops/organizations/{id}", get(organization))
        .route(
            "/api/v1/ops/organizations/{id}/entitlements/{feature}",
            post(grant_entitlement).delete(revoke_entitlement),
        )
        .route("/api/v1/ops/mac-resources", get(resources))
        .route(
            "/api/v1/ops/mac-resources/{id}/diagnostics",
            get(resource_diagnostics),
        )
        .route(
            "/api/v1/ops/mac-resources/{id}/hold",
            post(hold).delete(unhold),
        )
        .route("/api/v1/ops/provisioning-jobs", get(jobs))
        .route("/api/v1/ops/provisioning-jobs/{id}/retry", post(retry_job))
        .route(
            "/api/v1/ops/provisioning-jobs/{id}/cancel",
            post(cancel_job),
        )
        .route("/api/v1/ops/reconcile", post(reconcile_now))
        .route(
            "/api/v1/ops/provider-integrations",
            get(providers).post(create_provider),
        )
        .route("/api/v1/ops/relay-nodes", get(relays).post(create_relay))
        .route("/api/v1/ops/relay-nodes/check", post(check_relays))
        .route("/api/v1/ops/relay-nodes/{id}", delete(delete_relay))
        .route("/api/v1/ops/billing/webhook-events", get(webhook_events))
        .route(
            "/api/v1/ops/billing/webhook-events/{id}/replay",
            post(replay_event),
        )
        .route("/api/v1/ops/billing/attention", get(billing_rentals))
        .route("/api/v1/ops/incidents", get(incidents))
        .route("/api/v1/ops/incidents/{id}", patch(update_incident))
        .route("/api/v1/ops/audit-events", get(audit_events))
        .route("/api/v1/ops/access", get(access).post(grant))
        .route("/api/v1/ops/access/{user}/{role}", delete(revoke))
        .route("/api/v1/ops/feature-flags", get(flags))
        .route("/api/v1/ops/feature-flags/{key}", patch(set_flag))
        .route("/api/v1/ops/plans", get(plans).post(create_plan))
        .route("/api/v1/ops/plans/{id}", patch(set_plan))
        .route("/api/v1/ops/domains", get(domains))
}
