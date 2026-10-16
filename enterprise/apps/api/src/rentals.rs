//! The catalog and rentals.
//!
//! A rental's status only moves along [`allowed`]; every move is written to rental_events with
//! its reason and actor, in the same transaction. Clients never set a status: they create a
//! rental (pending payment), pay through Stripe, cancel; webhooks, the provisioning worker and
//! reconciliation do the rest.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::authz::Permission;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Draft,
    PendingPayment,
    Paid,
    Provisioning,
    Ready,
    Suspended,
    CancelPending,
    Expired,
    Failed,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 10] = [
        Status::Draft,
        Status::PendingPayment,
        Status::Paid,
        Status::Provisioning,
        Status::Ready,
        Status::Suspended,
        Status::CancelPending,
        Status::Expired,
        Status::Failed,
        Status::Cancelled,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            Status::Draft => "draft",
            Status::PendingPayment => "pending_payment",
            Status::Paid => "paid",
            Status::Provisioning => "provisioning",
            Status::Ready => "ready",
            Status::Suspended => "suspended",
            Status::CancelPending => "cancel_pending",
            Status::Expired => "expired",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|x| x.key() == s)
    }

    pub fn is_final(&self) -> bool {
        matches!(self, Status::Expired | Status::Failed | Status::Cancelled)
    }
}

/// The rental state machine: which moves exist at all.
pub fn allowed(from: Status, to: Status) -> bool {
    use Status::*;
    matches!(
        (from, to),
        (Draft, PendingPayment)
            | (Draft, Cancelled)
            | (PendingPayment, Paid)
            | (PendingPayment, Cancelled)
            | (Paid, Provisioning)
            | (Paid, Failed)
            | (Provisioning, Ready)
            | (Provisioning, Failed)
            | (Ready, Suspended)
            | (Ready, CancelPending)
            | (Ready, Expired)
            | (Suspended, Ready)
            | (Suspended, CancelPending)
            | (Suspended, Expired)
            | (CancelPending, Cancelled)
    )
}

/// Move a rental from what it is to `to` (refused when the machine has no such move). Returns
/// the status it had.
pub async fn transition(
    tx: &mut sqlx::PgConnection,
    rental: Uuid,
    to: Status,
    reason: &str,
    actor: &str,
) -> ApiResult<Status> {
    let (cur,): (String,) = sqlx::query_as("SELECT status FROM rentals WHERE id = $1 FOR UPDATE")
        .bind(rental)
        .fetch_one(&mut *tx)
        .await?;
    let from = Status::parse(&cur)
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("unknown status {cur}")))?;
    if from == to {
        return Ok(from);
    }
    if !allowed(from, to) {
        return Err(ApiError::conflict(format!(
            "A rental that is {} cannot become {}.",
            from.key(),
            to.key()
        )));
    }
    sqlx::query(
        "UPDATE rentals SET status = $1, status_reason = $2, updated_at = now() WHERE id = $3",
    )
    .bind(to.key())
    .bind(reason)
    .bind(rental)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO rental_events (rental_id, from_status, to_status, reason, actor) VALUES ($1, $2, $3, $4, $5)").bind(rental).bind(from.key()).bind(to.key()).bind(reason).bind(actor).execute(&mut *tx).await?;
    Ok(from)
}

// ------------------------------------------------------------------ catalog

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub chip: String,
    pub cpu_cores: i32,
    pub memory_gb: i32,
    pub storage_gb: i32,
    pub region: String,
    pub billing: String,
    pub duration_hours: Option<i32>,
    pub price_minor: i64,
    pub currency: String,
    pub active: bool,
}

const PLAN_COLUMNS: &str = "id, slug, name, description, chip, cpu_cores, memory_gb, storage_gb, region, billing, duration_hours, price_minor, currency, active";

pub async fn plan(db: impl sqlx::PgExecutor<'_>, id: Uuid) -> ApiResult<Plan> {
    Ok(sqlx::query_as(&format!(
        "SELECT {PLAN_COLUMNS} FROM mac_plans WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(db)
    .await?)
}

/// Macs free now for a plan (its chip in its region, not held by an operator).
pub async fn available_for(
    db: impl sqlx::PgExecutor<'_>,
    chip: &str,
    region: &str,
) -> ApiResult<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM mac_resources WHERE state = 'available' AND hold_reason IS NULL AND chip = $1 AND region = $2").bind(chip).bind(region).fetch_one(db).await?;
    Ok(n)
}

async fn plans(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require(&state, org, Permission::OrgRead).await?;
    let rows: Vec<Plan> = sqlx::query_as(&format!(
        "SELECT {PLAN_COLUMNS} FROM mac_plans WHERE active ORDER BY price_minor"
    ))
    .fetch_all(&state.db)
    .await?;
    let mut out = vec![];
    for p in rows {
        let available = available_for(&state.db, &p.chip, &p.region).await?;
        out.push(json!({ "plan": p, "available": available }));
    }
    Ok(Json(
        json!({ "plans": out, "simulated_provider": state.provider.is_simulated() }),
    ))
}

// ------------------------------------------------------------------ rentals

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Rental {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub plan_id: Uuid,
    pub plan_name: String,
    pub status: String,
    pub status_reason: Option<String>,
    pub billing: String,
    pub billing_state: String,
    pub amount_minor: i64,
    pub currency: String,
    pub duration_hours: Option<i32>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub resource_name: Option<String>,
    pub resource_model: Option<String>,
    pub resource_region: Option<String>,
    pub connection_state: Option<String>,
    pub resource_synced_at: Option<DateTime<Utc>>,
    pub assigned_user_id: Option<Uuid>,
    pub assigned_email: Option<String>,
    pub job_state: Option<String>,
    pub job_attempts: Option<i32>,
}

const RENTAL_SELECT: &str = "SELECT r.id, r.organization_id, r.plan_id, p.name AS plan_name, r.status, r.status_reason, r.billing, r.billing_state, r.amount_minor, r.currency, r.duration_hours, r.starts_at, r.ends_at, r.created_at, r.updated_at, r.created_by,
       m.display_name AS resource_name, m.model AS resource_model, m.region AS resource_region,
       CASE WHEN m.id IS NULL THEN NULL WHEN r.status = 'ready' AND m.state = 'assigned' AND m.provider_state = 'in_use' THEN 'connectable' WHEN r.status = 'ready' THEN 'checking' ELSE 'not_connectable' END AS connection_state,
       m.last_synced_at AS resource_synced_at,
       a.user_id AS assigned_user_id, au.email AS assigned_email,
       j.state AS job_state, j.attempts AS job_attempts
     FROM rentals r JOIN mac_plans p ON p.id = r.plan_id
     LEFT JOIN mac_resources m ON m.id = r.resource_id
     LEFT JOIN LATERAL (SELECT user_id FROM rental_assignments WHERE rental_id = r.id AND revoked_at IS NULL ORDER BY assigned_at DESC LIMIT 1) a ON true
     LEFT JOIN users au ON au.id = a.user_id
     LEFT JOIN provisioning_jobs j ON j.rental_id = r.id AND j.operation = 'provision'";

/// The SELECT that makes a [`Rental`] (for other modules' lists).
pub fn rental_select() -> &'static str {
    RENTAL_SELECT
}

pub async fn load(db: impl sqlx::PgExecutor<'_>, id: Uuid) -> ApiResult<Rental> {
    Ok(sqlx::query_as(&format!("{RENTAL_SELECT} WHERE r.id = $1"))
        .bind(id)
        .fetch_one(db)
        .await?)
}

/// The rental if the caller may see it: everyone with RentalsRead in its organization, or the
/// member it is assigned to. Anyone else gets NotFound.
pub async fn visible(
    state: &AppState,
    auth: &Auth,
    id: Uuid,
) -> ApiResult<(Rental, crate::authz::Role)> {
    let r = load(&state.db, id).await?;
    let role = auth.org_role(state, r.organization_id).await?;
    if role.can(Permission::RentalsRead) || r.assigned_user_id == Some(auth.user_id) {
        return Ok((r, role));
    }
    Err(ApiError::NotFound)
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let role = auth.org_role(&state, org).await?;
    let rows: Vec<Rental> = if role.can(Permission::RentalsRead) {
        sqlx::query_as(&format!(
            "{RENTAL_SELECT} WHERE r.organization_id = $1 ORDER BY r.created_at DESC LIMIT 200"
        ))
        .bind(org)
        .fetch_all(&state.db)
        .await?
    } else {
        // members: the Macs assigned to them
        sqlx::query_as(&format!("{RENTAL_SELECT} WHERE r.organization_id = $1 AND a.user_id = $2 ORDER BY r.created_at DESC LIMIT 200")).bind(org).bind(auth.user_id).fetch_all(&state.db).await?
    };
    Ok(Json(json!({ "rentals": rows })))
}

#[derive(Deserialize)]
struct NewRental {
    plan_id: Uuid,
    /// who uses the Mac (a member of the organization); the creator when left out
    assigned_user_id: Option<Uuid>,
    idempotency_key: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Path(org): Path<Uuid>,
    Json(b): Json<NewRental>,
) -> ApiResult<(StatusCode, Json<Rental>)> {
    auth.require(&state, org, Permission::RentalsCreate).await?;
    if !crate::ops::flag(&state, "rentals.new").await {
        return Err(ApiError::Unavailable(
            "New rentals are paused for a moment. Try again later.".into(),
        ));
    }
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or(b.idempotency_key)
        .ok_or_else(|| ApiError::invalid("An Idempotency-Key header is required."))?;
    if key.len() < 8
        || key.len() > 100
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ApiError::invalid(
            "The idempotency key is 8 to 100 letters, digits, - or _.",
        ));
    }
    // the same key again: the same rental, nothing new
    if let Some((id,)) = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM rentals WHERE organization_id = $1 AND idempotency_key = $2",
    )
    .bind(org)
    .bind(&key)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok((StatusCode::OK, Json(load(&state.db, id).await?)));
    }
    let p = plan(&state.db, b.plan_id)
        .await
        .map_err(|_| ApiError::invalid("No such plan."))?;
    if !p.active {
        return Err(ApiError::invalid("That plan is not offered any more."));
    }
    let assignee = b.assigned_user_id.unwrap_or(auth.user_id);
    let (member,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM memberships WHERE organization_id = $1 AND user_id = $2)",
    )
    .bind(org)
    .bind(assignee)
    .fetch_one(&state.db)
    .await?;
    if !member {
        return Err(ApiError::invalid(
            "The Mac can only be assigned to a member of the organization.",
        ));
    }
    if available_for(&state.db, &p.chip, &p.region).await? == 0 {
        return Err(ApiError::conflict(
            "No Mac of this plan is free right now. Try another plan or region, or try later.",
        ));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    let inserted = sqlx::query("INSERT INTO rentals (id, organization_id, plan_id, created_by, status, billing, amount_minor, currency, duration_hours, idempotency_key) VALUES ($1, $2, $3, $4, 'draft', $5, $6, $7, $8, $9) ON CONFLICT (organization_id, idempotency_key) DO NOTHING")
        .bind(id)
        .bind(org)
        .bind(p.id)
        .bind(auth.user_id)
        .bind(&p.billing)
        .bind(p.price_minor)
        .bind(&p.currency)
        .bind(p.duration_hours)
        .bind(&key)
        .execute(&mut *tx)
        .await?;
    if inserted.rows_affected() == 0 {
        // a concurrent request with the same key won
        tx.rollback().await?;
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM rentals WHERE organization_id = $1 AND idempotency_key = $2",
        )
        .bind(org)
        .bind(&key)
        .fetch_one(&state.db)
        .await?;
        return Ok((StatusCode::OK, Json(load(&state.db, id).await?)));
    }
    sqlx::query("INSERT INTO rental_events (rental_id, from_status, to_status, reason, actor) VALUES ($1, NULL, 'draft', 'created', $2)").bind(id).bind(format!("user:{}", auth.user_id)).execute(&mut *tx).await?;
    transition(
        &mut tx,
        id,
        Status::PendingPayment,
        "waiting for payment",
        &format!("user:{}", auth.user_id),
    )
    .await?;
    sqlx::query("INSERT INTO rental_assignments (id, rental_id, user_id, assigned_by) VALUES ($1, $2, $3, $4)").bind(Uuid::new_v4()).bind(id).bind(assignee).bind(auth.user_id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "rental.created", "rental")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(
                json!({ "plan": p.slug, "amount_minor": p.price_minor, "currency": p.currency }),
            ),
    )
    .await?;
    tx.commit().await?;
    state
        .metrics
        .inc("taro_rentals_created_total", &[("plan", &p.slug)]);
    Ok((StatusCode::CREATED, Json(load(&state.db, id).await?)))
}

async fn get_one(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let (r, role) = visible(&state, &auth, id).await?;
    let events: Vec<(Option<String>, String, String, DateTime<Utc>)> = sqlx::query_as("SELECT from_status, to_status, reason, at FROM rental_events WHERE rental_id = $1 ORDER BY id").bind(id).fetch_all(&state.db).await?;
    let can_connect = role.can(Permission::MacsConnect)
        && r.assigned_user_id == Some(auth.user_id)
        && r.status == "ready";
    Ok(Json(json!({
        "rental": r,
        "history": events.into_iter().map(|(f, t, why, at)| json!({ "from": f, "to": t, "reason": why, "at": at })).collect::<Vec<_>>(),
        "actions": {
            "checkout": role.can(Permission::RentalsCreate) && r.status == "pending_payment",
            "cancel": role.can(Permission::RentalsCancel) && matches!(r.status.as_str(), "pending_payment" | "ready" | "suspended"),
            "assign": role.can(Permission::RentalsAssign) && !Status::parse(&r.status).is_some_and(|s| s.is_final()),
            "connect": can_connect,
        },
    })))
}

#[derive(Deserialize)]
struct Assign {
    user_id: Uuid,
}

async fn assign(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(b): Json<Assign>,
) -> ApiResult<StatusCode> {
    let r = load(&state.db, id).await?;
    auth.require(&state, r.organization_id, Permission::RentalsAssign)
        .await?;
    if Status::parse(&r.status).is_some_and(|s| s.is_final()) {
        return Err(ApiError::conflict("This rental has ended."));
    }
    let (member,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM memberships WHERE organization_id = $1 AND user_id = $2)",
    )
    .bind(r.organization_id)
    .bind(b.user_id)
    .fetch_one(&state.db)
    .await?;
    if !member {
        return Err(ApiError::invalid(
            "The Mac can only be assigned to a member of the organization.",
        ));
    }
    let mut tx = state.db.begin().await?;
    // one person uses a Mac at a time: the previous assignment ends, and their grants with it
    sqlx::query("UPDATE rental_assignments SET revoked_at = now(), revoked_by = $2 WHERE rental_id = $1 AND revoked_at IS NULL AND user_id <> $3").bind(id).bind(auth.user_id).bind(b.user_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE connection_grants SET revoked_at = now() WHERE rental_id = $1 AND user_id <> $2 AND redeemed_at IS NULL AND revoked_at IS NULL").bind(id).bind(b.user_id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO rental_assignments (id, rental_id, user_id, assigned_by) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING").bind(Uuid::new_v4()).bind(id).bind(b.user_id).bind(auth.user_id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "rental.assigned", "rental")
            .org(r.organization_id)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "user_id": b.user_id })),
    )
    .await?;
    tx.commit().await?;
    crate::notifications::notify(
        &state,
        b.user_id,
        Some(r.organization_id),
        "rental.assigned",
        "A Mac was assigned to you",
        &format!(
            "{} is now yours to use.",
            r.resource_name.as_deref().unwrap_or(&r.plan_name)
        ),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn unassign(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, user)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let r = load(&state.db, id).await?;
    auth.require(&state, r.organization_id, Permission::RentalsAssign)
        .await?;
    let mut tx = state.db.begin().await?;
    let x = sqlx::query("UPDATE rental_assignments SET revoked_at = now(), revoked_by = $3 WHERE rental_id = $1 AND user_id = $2 AND revoked_at IS NULL").bind(id).bind(user).bind(auth.user_id).execute(&mut *tx).await?;
    if x.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    sqlx::query("UPDATE connection_grants SET revoked_at = now() WHERE rental_id = $1 AND user_id = $2 AND redeemed_at IS NULL AND revoked_at IS NULL").bind(id).bind(user).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "rental.unassigned", "rental")
            .org(r.organization_id)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "user_id": user })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Cancel. Not paid yet: cancelled now. A one-time rental in use: it ends now and its Mac is
/// released (no automatic refund: that is a billing decision). A monthly one: the subscription
/// stops at the end of the paid period, and the Mac stays until then.
async fn cancel(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Rental>> {
    let r = load(&state.db, id).await?;
    auth.require(&state, r.organization_id, Permission::RentalsCancel)
        .await?;
    let actor = format!("user:{}", auth.user_id);
    let status = Status::parse(&r.status)
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("bad status")))?;
    match status {
        Status::PendingPayment | Status::Draft => {
            let mut tx = state.db.begin().await?;
            transition(
                &mut tx,
                id,
                Status::Cancelled,
                "cancelled before payment",
                &actor,
            )
            .await?;
            audit::record(
                &mut *tx,
                Event::new(auth.actor(), "rental.cancelled", "rental")
                    .org(r.organization_id)
                    .resource(id)
                    .ip(auth.ip.as_deref()),
            )
            .await?;
            let session: Option<(Option<String>,)> =
                sqlx::query_as("SELECT stripe_checkout_session_id FROM rentals WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
            tx.commit().await?;
            if let (Some((Some(s),)), Some(b)) = (session, state.billing.as_ref()) {
                // an open checkout must not take a payment for a cancelled rental
                if let Err(e) = b.expire_checkout(&s).await {
                    tracing::warn!(error = %e, "checkout session not expired");
                }
            }
        }
        Status::Ready | Status::Suspended if r.billing == "monthly" => {
            let (sub,): (Option<String>,) =
                sqlx::query_as("SELECT stripe_subscription_id FROM rentals WHERE id = $1")
                    .bind(id)
                    .fetch_one(&state.db)
                    .await?;
            let billing = state
                .billing
                .as_ref()
                .ok_or_else(|| ApiError::Unavailable("Billing is not configured.".into()))?;
            let sub = sub
                .ok_or_else(|| ApiError::conflict("This rental has no subscription to cancel."))?;
            billing
                .cancel_subscription_at_period_end(&sub)
                .await
                .map_err(|e| ApiError::Unavailable(format!("Stripe: {e}")))?;
            sqlx::query("UPDATE subscriptions SET cancel_at_period_end = true WHERE id = $1")
                .bind(&sub)
                .execute(&state.db)
                .await?;
            audit::record(
                &state.db,
                Event::new(auth.actor(), "rental.cancel_at_period_end", "rental")
                    .org(r.organization_id)
                    .resource(id)
                    .ip(auth.ip.as_deref()),
            )
            .await?;
        }
        Status::Ready | Status::Suspended => {
            let mut tx = state.db.begin().await?;
            transition(
                &mut tx,
                id,
                Status::CancelPending,
                "cancelled by the customer",
                &actor,
            )
            .await?;
            crate::provisioning::enqueue(&mut tx, id, "release", state.provider_id).await?;
            audit::record(
                &mut *tx,
                Event::new(auth.actor(), "rental.cancelled", "rental")
                    .org(r.organization_id)
                    .resource(id)
                    .ip(auth.ip.as_deref()),
            )
            .await?;
            tx.commit().await?;
        }
        Status::Paid | Status::Provisioning => {
            return Err(ApiError::conflict(
                "The Mac is being prepared. Cancel once it is ready.",
            ))
        }
        _ => return Err(ApiError::conflict("This rental has already ended.")),
    }
    Ok(Json(load(&state.db, id).await?))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/organizations/{org}/mac-plans", get(plans))
        .route(
            "/api/v1/organizations/{org}/rentals",
            get(list).post(create),
        )
        .route("/api/v1/rentals/{id}", get(get_one))
        .route("/api/v1/rentals/{id}/cancel", post(cancel))
        .route("/api/v1/rentals/{id}/assignments", post(assign))
        .route(
            "/api/v1/rentals/{id}/assignments/{user}",
            axum::routing::delete(unassign),
        )
}

#[cfg(test)]
mod tests {
    use super::Status::*;
    use super::*;

    #[test]
    fn the_state_machine() {
        // the happy path
        for (a, b) in [
            (Draft, PendingPayment),
            (PendingPayment, Paid),
            (Paid, Provisioning),
            (Provisioning, Ready),
            (Ready, CancelPending),
            (CancelPending, Cancelled),
        ] {
            assert!(allowed(a, b), "{a:?} -> {b:?}");
        }
        // nothing skips payment or provisioning, nothing comes back from an end
        for (a, b) in [
            (PendingPayment, Ready),
            (Draft, Ready),
            (Paid, Ready),
            (Failed, Ready),
            (Cancelled, PendingPayment),
            (Expired, Ready),
            (Ready, Paid),
            (Provisioning, Cancelled),
        ] {
            assert!(!allowed(a, b), "{a:?} -> {b:?}");
        }
        for s in Status::ALL {
            if s.is_final() {
                assert!(
                    Status::ALL.iter().all(|t| !allowed(s, *t)),
                    "{s:?} is final"
                );
            }
            assert_eq!(Status::parse(s.key()), Some(s));
        }
    }
}
