//! The provider's operators give Macs to customers directly (no checkout): Provider Console,
//! Assignments. Assigning a Mac of a plan to an email address makes the account if needed, its
//! organization if it has none, and a rental marked as paid by the operator; the provisioning
//! worker then prepares a Mac of that plan as for any paid rental, and the customer sees it in
//! the portal and in the Taro app. Ending an assignment releases the Mac.
//!
//! The provider's own systems do the same with `Authorization: Bearer <TARO_OPS_API_KEY>`
//! (automatic assignment, e.g. after a sale elsewhere): a key request never uses a session.
//! `GET /api/v1/ops/assignments/plans` lists the active plans' ids for it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::authz::Permission as P;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::rentals::{self, Status};
use crate::state::AppState;

#[derive(serde::Serialize, sqlx::FromRow)]
struct Row {
    rental_id: Uuid,
    email: Option<String>,
    organization: String,
    plan: String,
    status: String,
    /// why it failed or ended (provisioning's or the operator's words)
    status_reason: Option<String>,
    mac_name: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Who assigns: an operator signed in to the console, or the provider's system with the key.
enum By {
    Operator(Auth),
    Key,
}

impl By {
    fn actor(&self) -> crate::audit::Actor {
        match self {
            By::Operator(a) => a.actor(),
            By::Key => crate::audit::Actor::System,
        }
    }
    fn actor_text(&self) -> String {
        match self {
            By::Operator(a) => format!("operator:{}", a.user_id),
            By::Key => "api-key".into(),
        }
    }
    fn ip(&self) -> Option<&str> {
        match self {
            By::Operator(a) => a.ip.as_deref(),
            By::Key => None,
        }
    }
}

fn bearer(parts: &axum::http::request::Parts) -> Option<&str> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A request to the assignments with a bearer key (exempt from the browser Origin check).
pub fn is_key_request(req: &axum::extract::Request) -> bool {
    req.uri().path().starts_with("/api/v1/ops/assignments")
        && req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer "))
}

async fn by(state: &AppState, parts: &mut axum::http::request::Parts, p: P) -> ApiResult<By> {
    use axum::extract::FromRequestParts;
    if let Some(k) = bearer(parts) {
        let ok = state
            .config
            .ops_api_key
            .as_ref()
            .is_some_and(|want| crate::crypto::eq(want.expose().as_bytes(), k.as_bytes()));
        return if ok {
            Ok(By::Key)
        } else {
            Err(ApiError::Unauthenticated)
        };
    }
    let auth = Auth::from_request_parts(parts, state).await?;
    auth.require_op(state, p).await?;
    Ok(By::Operator(auth))
}

async fn list(
    State(state): State<AppState>,
    mut parts: axum::http::request::Parts,
) -> ApiResult<Json<serde_json::Value>> {
    by(&state, &mut parts, P::OpsOrgsRead).await?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT r.id AS rental_id, u.email, o.name AS organization, p.name AS plan, r.status, r.status_reason, CASE WHEN r.status IN ('cancelled', 'expired', 'failed') THEN NULL ELSE m.display_name END AS mac_name, r.created_at
           FROM rentals r JOIN organizations o ON o.id = r.organization_id JOIN mac_plans p ON p.id = r.plan_id
           LEFT JOIN LATERAL (SELECT user_id FROM rental_assignments WHERE rental_id = r.id AND revoked_at IS NULL ORDER BY assigned_at DESC LIMIT 1) a ON true
           LEFT JOIN users u ON u.id = a.user_id
           LEFT JOIN mac_resources m ON m.id = r.resource_id
          ORDER BY (r.status IN ('cancelled', 'expired', 'failed')), r.created_at DESC LIMIT 500",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "assignments": rows })))
}

/// The active plans an assignment can use (for the provider's systems: the key cannot read the
/// console's plan pages).
async fn plans(
    State(state): State<AppState>,
    mut parts: axum::http::request::Parts,
) -> ApiResult<Json<serde_json::Value>> {
    by(&state, &mut parts, P::OpsOrgsRead).await?;
    let rows: Vec<(Uuid, String, String, Option<i32>)> = sqlx::query_as(
        "SELECT id, name, billing, duration_hours FROM mac_plans WHERE active ORDER BY name",
    )
    .fetch_all(&state.db)
    .await?;
    let plans: Vec<_> = rows
        .into_iter()
        .map(|(id, name, billing, hours)| json!({ "id": id, "name": name, "billing": billing, "duration_hours": hours }))
        .collect();
    Ok(Json(json!({ "plans": plans })))
}

#[derive(Deserialize)]
struct NewAssignment {
    email: String,
    plan_id: Uuid,
    /// a particular Mac (Macs in the console); otherwise the worker picks a free one of the plan
    #[serde(default)]
    mac_id: Option<Uuid>,
}

async fn create(
    State(state): State<AppState>,
    mut parts: axum::http::request::Parts,
    Json(b): Json<NewAssignment>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let auth = by(&state, &mut parts, P::OpsInventoryManage).await?;
    let email = b.email.trim().to_lowercase();
    if !(email.len() <= 254
        && email
            .split_once('@')
            .is_some_and(|(a, d)| !a.is_empty() && d.contains('.')))
    {
        return Err(ApiError::invalid("Type the customer's email address."));
    }
    let plan: Option<(String, String, i32)> = sqlx::query_as(
        "SELECT name, billing, coalesce(duration_hours, 0) FROM mac_plans WHERE id = $1 AND active",
    )
    .bind(b.plan_id)
    .fetch_optional(&state.db)
    .await?;
    let (plan_name, billing, hours) =
        plan.ok_or_else(|| ApiError::invalid("Choose an active plan."))?;
    let user = crate::identity::customer_user(&state, &email, auth.ip(), "operator").await?;
    let mut tx = state.db.begin().await?;
    // the customer's own organization, or a new one for them
    let org: Option<(Uuid,)> = sqlx::query_as("SELECT organization_id FROM memberships WHERE user_id = $1 AND role = 'owner' ORDER BY created_at LIMIT 1")
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?;
    let org = match org {
        Some((o,)) => o,
        None => {
            let o = Uuid::new_v4();
            let local: String = email
                .split('@')
                .next()
                .unwrap_or("customer")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(24)
                .collect::<String>()
                .to_lowercase();
            let slug = format!(
                "{}-{}",
                if local.is_empty() { "customer" } else { &local },
                &o.simple().to_string()[..6]
            );
            sqlx::query(
                "INSERT INTO organizations (id, name, slug, created_by) VALUES ($1, $2, $3, $4)",
            )
            .bind(o)
            .bind(&email)
            .bind(&slug)
            .bind(user)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')",
            )
            .bind(o)
            .bind(user)
            .execute(&mut *tx)
            .await?;
            o
        }
    };
    let id = Uuid::new_v4();
    let actor = auth.actor_text();
    sqlx::query("INSERT INTO rentals (id, organization_id, plan_id, created_by, status, billing, billing_state, amount_minor, currency, duration_hours, idempotency_key) SELECT $1, $2, p.id, $3, 'draft', p.billing, 'paid', 0, p.currency, p.duration_hours, $4 FROM mac_plans p WHERE p.id = $5")
        .bind(id)
        .bind(org)
        .bind(user)
        .bind(format!("operator-{id}"))
        .bind(b.plan_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO rental_events (rental_id, from_status, to_status, reason, actor) VALUES ($1, NULL, 'draft', 'assigned by an operator', $2)")
        .bind(id)
        .bind(&actor)
        .execute(&mut *tx)
        .await?;
    rentals::transition(
        &mut tx,
        id,
        Status::PendingPayment,
        "assigned by an operator",
        &actor,
    )
    .await?;
    rentals::transition(
        &mut tx,
        id,
        Status::Paid,
        "assigned by an operator (no payment)",
        &actor,
    )
    .await?;
    if let Some(mac) = b.mac_id {
        let m: Option<(String, Option<String>, bool)> = sqlx::query_as(
            "SELECT state, hold_reason, control_by IS NOT NULL FROM mac_resources WHERE id = $1 AND provider_id = $2 FOR UPDATE",
        )
        .bind(mac)
        .bind(state.provider_id)
        .fetch_optional(&mut *tx)
        .await?;
        match m {
            Some((st, None, false)) if st == "available" => {}
            Some((_, _, true)) => {
                return Err(ApiError::conflict(
                    "An administrator is working on this Mac. Release it first.",
                ))
            }
            Some(_) => return Err(ApiError::conflict("This Mac is not free.")),
            None => return Err(ApiError::invalid("Choose a Mac of this server.")),
        }
        sqlx::query(
            "UPDATE mac_resources SET state = 'reserved', updated_at = now() WHERE id = $1",
        )
        .bind(mac)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE rentals SET resource_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(mac)
            .execute(&mut *tx)
            .await?;
        rentals::transition(
            &mut tx,
            id,
            Status::Provisioning,
            "the operator chose the Mac",
            &actor,
        )
        .await?;
    }
    sqlx::query("INSERT INTO rental_assignments (id, rental_id, user_id, assigned_by) VALUES ($1, $2, $3, $4)")
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(user)
        .bind(match &auth {
            By::Operator(a) => a.user_id,
            By::Key => user,
        })
        .execute(&mut *tx)
        .await?;
    crate::provisioning::enqueue(&mut tx, id, "provision", state.provider_id).await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "assignment.created", "rental")
            .org(org)
            .resource(id)
            .ip(auth.ip())
            .details(
                json!({ "email": email, "plan": plan_name, "billing": billing, "hours": hours }),
            ),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "rental": rentals::load(&state.db, id).await? })),
    ))
}

/// End an assignment: the Mac is released (or the rental cancelled while it is not ready).
async fn end(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    mut parts: axum::http::request::Parts,
) -> ApiResult<Json<serde_json::Value>> {
    let auth = by(&state, &mut parts, P::OpsInventoryManage).await?;
    let r = rentals::load(&state.db, id).await?;
    let actor = auth.actor_text();
    let mut tx = state.db.begin().await?;
    match Status::parse(&r.status) {
        Some(Status::Ready | Status::Suspended) => {
            rentals::transition(
                &mut tx,
                id,
                Status::CancelPending,
                "ended by an operator",
                &actor,
            )
            .await?;
            crate::provisioning::enqueue(&mut tx, id, "release", state.provider_id).await?;
        }
        Some(Status::Draft | Status::PendingPayment) => {
            rentals::transition(
                &mut tx,
                id,
                Status::Cancelled,
                "ended by an operator",
                &actor,
            )
            .await?;
        }
        Some(Status::Paid | Status::Provisioning) => {
            return Err(ApiError::conflict(
                "The Mac is being prepared. End it once it is ready.",
            ))
        }
        _ => return Err(ApiError::conflict("This assignment has already ended.")),
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "assignment.ended", "rental")
            .org(r.organization_id)
            .resource(id)
            .ip(auth.ip()),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "rental": rentals::load(&state.db, id).await? }),
    ))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ops/assignments", get(list).post(create))
        .route("/api/v1/ops/assignments/plans", get(plans))
        .route("/api/v1/ops/assignments/{id}/end", post(end))
}
