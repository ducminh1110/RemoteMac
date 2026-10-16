//! In-app notifications (and the same by mail when it matters).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::state::AppState;

/// Tell a user something (never fails the caller: a lost notice is logged, not raised).
pub async fn notify(
    state: &AppState,
    user: Uuid,
    org: Option<Uuid>,
    kind: &str,
    title: &str,
    body: &str,
) {
    let r = sqlx::query("INSERT INTO notifications (id, user_id, organization_id, kind, title, body) VALUES ($1, $2, $3, $4, $5, $6)").bind(Uuid::new_v4()).bind(user).bind(org).bind(kind).bind(title).bind(body).execute(&state.db).await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "notification not stored");
        return;
    }
    if matches!(
        kind,
        "rental.ready" | "rental.failed" | "billing.payment_failed"
    ) {
        if let Ok((email,)) =
            sqlx::query_as::<_, (String,)>("SELECT email FROM users WHERE id = $1")
                .bind(user)
                .fetch_one(&state.db)
                .await
        {
            let body = format!(
                "{body}\n\n{}",
                state.config.public_url.origin().ascii_serialization()
            );
            if let Err(e) = state.mailer.send(&email, title, &body).await {
                tracing::warn!(error = %crate::redact::redact(&e.to_string()), "notification mail not sent");
            }
        }
    }
}

/// Everyone in an organization holding one of `roles`.
pub async fn notify_roles(
    state: &AppState,
    org: Uuid,
    roles: &[&str],
    kind: &str,
    title: &str,
    body: &str,
) {
    let users: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT user_id FROM memberships WHERE organization_id = $1 AND role = ANY($2)",
    )
    .bind(org)
    .bind(roles.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    for (u,) in users {
        notify(state, u, Some(org), kind, title, body).await;
    }
}

#[derive(Serialize, sqlx::FromRow)]
struct Row {
    id: Uuid,
    organization_id: Option<Uuid>,
    kind: String,
    title: String,
    body: String,
    created_at: DateTime<Utc>,
    read_at: Option<DateTime<Utc>>,
}

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<Row> = sqlx::query_as("SELECT id, organization_id, kind, title, body, created_at, read_at FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT 50").bind(auth.user_id).fetch_all(&state.db).await?;
    let unread = rows.iter().filter(|r| r.read_at.is_none()).count();
    Ok(Json(json!({ "notifications": rows, "unread": unread })))
}

async fn read(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query("UPDATE notifications SET read_at = coalesce(read_at, now()) WHERE id = $1 AND user_id = $2").bind(id).bind(auth.user_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/notifications", get(list))
        .route("/api/v1/notifications/{id}/read", post(read))
}
