//! The audit trail: who did what to which resource, when, and how it ended. Records never hold
//! secrets or payment details; the table refuses updates and deletes.

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    User(Uuid),
    Operator(Uuid),
    System,
    Stripe,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Denied,
    Failure,
}

pub struct Event<'a> {
    pub actor: Actor,
    pub organization_id: Option<Uuid>,
    pub action: &'a str,
    pub resource_type: &'a str,
    pub resource_id: Option<String>,
    pub result: Outcome,
    pub ip: Option<&'a str>,
    pub details: Value,
}

impl<'a> Event<'a> {
    pub fn new(actor: Actor, action: &'a str, resource_type: &'a str) -> Self {
        Event {
            actor,
            organization_id: None,
            action,
            resource_type,
            resource_id: None,
            result: Outcome::Success,
            ip: None,
            details: Value::Object(Default::default()),
        }
    }
    pub fn org(mut self, id: Uuid) -> Self {
        self.organization_id = Some(id);
        self
    }
    pub fn resource(mut self, id: impl ToString) -> Self {
        self.resource_id = Some(id.to_string());
        self
    }
    pub fn result(mut self, r: Outcome) -> Self {
        self.result = r;
        self
    }
    pub fn ip(mut self, ip: Option<&'a str>) -> Self {
        self.ip = ip;
        self
    }
    pub fn details(mut self, d: Value) -> Self {
        self.details = d;
        self
    }
}

/// Write one record (inside the caller's transaction when given one, so the record and the
/// change commit together).
pub async fn record<'e, E: sqlx::PgExecutor<'e>>(db: E, e: Event<'_>) -> anyhow::Result<()> {
    let (kind, user) = match e.actor {
        Actor::User(u) => ("user", Some(u)),
        Actor::Operator(u) => ("operator", Some(u)),
        Actor::System => ("system", None),
        Actor::Stripe => ("stripe", None),
        Actor::Provider => ("provider", None),
    };
    let result = match e.result {
        Outcome::Success => "success",
        Outcome::Denied => "denied",
        Outcome::Failure => "failure",
    };
    sqlx::query("INSERT INTO audit_events (actor_kind, actor_user_id, organization_id, action, resource_type, resource_id, result, ip, request_id, details) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
        .bind(kind)
        .bind(user)
        .bind(e.organization_id)
        .bind(e.action)
        .bind(e.resource_type)
        .bind(e.resource_id)
        .bind(result)
        .bind(e.ip)
        .bind(crate::error::request_id())
        .bind(e.details)
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Row {
    pub id: i64,
    pub at: chrono::DateTime<chrono::Utc>,
    pub actor_kind: String,
    pub actor_user_id: Option<Uuid>,
    pub actor_email: Option<String>,
    pub organization_id: Option<Uuid>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub result: String,
    pub request_id: Option<String>,
    pub details: Value,
}

/// Newest first, before `before` (an id) when paging.
pub async fn list(
    db: &sqlx::PgPool,
    org: Option<Uuid>,
    before: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<Row>> {
    let rows = sqlx::query_as::<_, Row>(
        "SELECT a.id, a.at, a.actor_kind, a.actor_user_id, u.email AS actor_email, a.organization_id, a.action, a.resource_type, a.resource_id, a.result, a.request_id, a.details
         FROM audit_events a LEFT JOIN users u ON u.id = a.actor_user_id
         WHERE ($1::uuid IS NULL OR a.organization_id = $1) AND ($2::bigint IS NULL OR a.id < $2)
         ORDER BY a.id DESC LIMIT $3",
    )
    .bind(org)
    .bind(before)
    .bind(limit.clamp(1, 200))
    .fetch_all(db)
    .await?;
    Ok(rows)
}
