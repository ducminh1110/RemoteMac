//! Incidents: what operators must look at (a failed paid rental, a provider down, the provider
//! and Taro disagreeing about a Mac). One open incident per dedupe key; a repeat bumps it.

use serde_json::Value;
use uuid::Uuid;

use crate::state::AppState;

pub struct New {
    pub severity: &'static str,
    pub component: &'static str,
    pub title: String,
    pub impact: String,
    pub dedupe_key: Option<String>,
    pub linked: Value,
}

/// Open an incident, or bump the open one with the same dedupe key.
pub async fn open(state: &AppState, n: New) -> anyhow::Result<Uuid> {
    if let Some(k) = &n.dedupe_key {
        if let Some((id,)) = sqlx::query_as::<_, (Uuid,)>("UPDATE incidents SET last_seen = now(), occurrences = occurrences + 1 WHERE dedupe_key = $1 AND status <> 'resolved' RETURNING id").bind(k).fetch_optional(&state.db).await? {
            return Ok(id);
        }
    }
    let id = Uuid::new_v4();
    let r = sqlx::query("INSERT INTO incidents (id, severity, component, title, impact, status, dedupe_key, linked) VALUES ($1, $2, $3, $4, $5, 'open', $6, $7) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(n.severity)
        .bind(n.component)
        .bind(&n.title)
        .bind(&n.impact)
        .bind(&n.dedupe_key)
        .bind(&n.linked)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        // opened concurrently by someone else: that one is it
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM incidents WHERE dedupe_key = $1 AND status <> 'resolved'",
        )
        .bind(&n.dedupe_key)
        .fetch_one(&state.db)
        .await?;
        return Ok(id);
    }
    tracing::warn!(incident = %id, severity = n.severity, component = n.component, title = %n.title, "incident opened");
    state.metrics.inc(
        "taro_incidents_opened_total",
        &[("component", n.component), ("severity", n.severity)],
    );
    Ok(id)
}
