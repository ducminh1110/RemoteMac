//! Organizations (tenants), their members and invitations. The organization in a path is only
//! a request: what the caller may do there comes from their membership, read here.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::authz::{Permission, Role};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::{valid_email, Auth};
use crate::state::AppState;

/// The modules an organization has by default.
pub const DEFAULT_ENTITLEMENTS: &[&str] = &["my_macs", "rentals", "billing", "team"];

#[derive(Serialize, sqlx::FromRow)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub created_at: DateTime<Utc>,
    pub suspended_at: Option<DateTime<Utc>>,
}

fn slugify(name: &str) -> String {
    let mut s = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').chars().take(40).collect::<String>();
    if s.len() < 2 {
        format!(
            "org-{}",
            crypto::token(4).to_lowercase().replace(['_', '-'], "x")
        )
    } else {
        s
    }
}

#[derive(Deserialize)]
struct NewOrg {
    name: String,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<NewOrg>,
) -> ApiResult<(StatusCode, Json<Organization>)> {
    if auth.audience != crate::state::Audience::Customer {
        return Err(ApiError::Forbidden);
    }
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "An organization's name is 1 to 120 characters.",
        ));
    }
    if !state.limiter.allow(
        "org_create",
        &auth.user_id.to_string(),
        10,
        std::time::Duration::from_secs(3600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let mut tx = state.db.begin().await?;
    let base = slugify(name);
    let mut slug = base.clone();
    for n in 2.. {
        let (taken,): (bool,) =
            sqlx::query_as("SELECT EXISTS (SELECT 1 FROM organizations WHERE slug = $1)")
                .bind(&slug)
                .fetch_one(&mut *tx)
                .await?;
        if !taken {
            break;
        }
        slug = format!("{base}-{n}");
    }
    let id = Uuid::new_v4();
    let org: Organization = sqlx::query_as("INSERT INTO organizations (id, name, slug, created_by) VALUES ($1, $2, $3, $4) RETURNING id, name, slug, created_at, suspended_at").bind(id).bind(name).bind(&slug).bind(auth.user_id).fetch_one(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(id)
    .bind(auth.user_id)
    .execute(&mut *tx)
    .await?;
    for f in DEFAULT_ENTITLEMENTS {
        sqlx::query("INSERT INTO entitlements (organization_id, feature, source) VALUES ($1, $2, 'default')").bind(id).bind(f).execute(&mut *tx).await?;
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "organization.created", "organization")
            .org(id)
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(org)))
}

async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as("SELECT o.id, o.name, o.slug, m.role FROM memberships m JOIN organizations o ON o.id = m.organization_id WHERE m.user_id = $1 ORDER BY o.name").bind(auth.user_id).fetch_all(&state.db).await?;
    Ok(Json(
        json!({ "organizations": rows.into_iter().map(|(id, name, slug, role)| json!({ "id": id, "name": name, "slug": slug, "role": role })).collect::<Vec<_>>() }),
    ))
}

async fn get_one(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let role = auth.require(&state, org, Permission::OrgRead).await?;
    let o: Organization = sqlx::query_as(
        "SELECT id, name, slug, created_at, suspended_at FROM organizations WHERE id = $1",
    )
    .bind(org)
    .fetch_one(&state.db)
    .await?;
    let ent: Vec<(String,)> = sqlx::query_as(
        "SELECT feature FROM entitlements WHERE organization_id = $1 ORDER BY feature",
    )
    .bind(org)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        json!({ "organization": o, "role": role, "permissions": role.permissions().iter().map(|p| p.key()).collect::<Vec<_>>(), "entitlements": ent.into_iter().map(|(f,)| f).collect::<Vec<_>>() }),
    ))
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Json(b): Json<NewOrg>,
) -> ApiResult<StatusCode> {
    auth.require(&state, org, Permission::OrgUpdate).await?;
    let name = b.name.trim();
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "An organization's name is 1 to 120 characters.",
        ));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE organizations SET name = $1 WHERE id = $2")
        .bind(name)
        .bind(org)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "organization.renamed", "organization")
            .org(org)
            .resource(org)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ members

#[derive(Serialize, sqlx::FromRow)]
struct Member {
    user_id: Uuid,
    email: String,
    display_name: String,
    role: String,
    created_at: DateTime<Utc>,
}

async fn members(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let role = auth.require(&state, org, Permission::MembersRead).await?;
    let rows: Vec<Member> = sqlx::query_as("SELECT u.id AS user_id, u.email, u.display_name, m.role, m.created_at FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.organization_id = $1 ORDER BY m.created_at").bind(org).fetch_all(&state.db).await?;
    #[derive(Serialize, sqlx::FromRow)]
    struct Inv {
        id: Uuid,
        email: String,
        role: String,
        created_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    }
    // pending invitations: for those who manage members
    let invitations: Vec<Inv> = if role.can(Permission::MembersManage) {
        sqlx::query_as("SELECT id, email, role, created_at, expires_at FROM invitations WHERE organization_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL ORDER BY created_at DESC").bind(org).fetch_all(&state.db).await?
    } else {
        vec![]
    };
    Ok(Json(json!({ "members": rows, "invitations": invitations })))
}

#[derive(Deserialize)]
struct ChangeRole {
    role: String,
}

async fn owners(tx: &mut sqlx::PgConnection, org: Uuid) -> ApiResult<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM memberships WHERE organization_id = $1 AND role = 'owner'",
    )
    .bind(org)
    .fetch_one(&mut *tx)
    .await?;
    Ok(n)
}

async fn change_role(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, user)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChangeRole>,
) -> ApiResult<StatusCode> {
    let mine = auth.require(&state, org, Permission::MembersManage).await?;
    let to = Role::parse(&b.role).ok_or_else(|| ApiError::invalid("Unknown role."))?;
    let mut tx = state.db.begin().await?;
    let cur: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(org)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    let cur = cur
        .and_then(|(r,)| Role::parse(&r))
        .ok_or(ApiError::NotFound)?;
    // an admin can neither make an owner nor touch one
    if !mine.may_assign(to) || !mine.may_assign(cur) {
        return Err(ApiError::Forbidden);
    }
    if cur == Role::Owner && to != Role::Owner && owners(&mut tx, org).await? <= 1 {
        return Err(ApiError::conflict(
            "An organization keeps at least one owner.",
        ));
    }
    sqlx::query("UPDATE memberships SET role = $1 WHERE organization_id = $2 AND user_id = $3")
        .bind(to.key())
        .bind(org)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "membership.role_changed", "membership")
            .org(org)
            .resource(user)
            .ip(auth.ip.as_deref())
            .details(json!({ "from": cur.key(), "to": to.key() })),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_member(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, user)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    // leaving on one's own needs no permission; removing someone else does
    let mine = if user == auth.user_id {
        auth.org_role(&state, org).await?
    } else {
        auth.require(&state, org, Permission::MembersManage).await?
    };
    let mut tx = state.db.begin().await?;
    let cur: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM memberships WHERE organization_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(org)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?;
    let cur = cur
        .and_then(|(r,)| Role::parse(&r))
        .ok_or(ApiError::NotFound)?;
    if user != auth.user_id && !mine.may_assign(cur) {
        return Err(ApiError::Forbidden);
    }
    if cur == Role::Owner && owners(&mut tx, org).await? <= 1 {
        return Err(ApiError::conflict(
            "An organization keeps at least one owner.",
        ));
    }
    sqlx::query("DELETE FROM memberships WHERE organization_id = $1 AND user_id = $2")
        .bind(org)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    // their Mac assignments in this organization end with the membership
    sqlx::query("UPDATE rental_assignments a SET revoked_at = now(), revoked_by = $3 FROM rentals r WHERE r.id = a.rental_id AND r.organization_id = $1 AND a.user_id = $2 AND a.revoked_at IS NULL").bind(org).bind(user).bind(auth.user_id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        Event::new(
            auth.actor(),
            if user == auth.user_id {
                "membership.left"
            } else {
                "membership.removed"
            },
            "membership",
        )
        .org(org)
        .resource(user)
        .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ invitations

#[derive(Deserialize)]
struct NewInvitation {
    email: String,
    role: String,
}

async fn invite(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Json(b): Json<NewInvitation>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let mine = auth.require(&state, org, Permission::MembersManage).await?;
    let email = b.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(ApiError::invalid("Type a valid email address."));
    }
    let role = Role::parse(&b.role).ok_or_else(|| ApiError::invalid("Unknown role."))?;
    if !mine.may_assign(role) {
        return Err(ApiError::Forbidden);
    }
    if !state.limiter.allow(
        "invite",
        &org.to_string(),
        50,
        std::time::Duration::from_secs(3600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let (already,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM memberships m JOIN users u ON u.id = m.user_id WHERE m.organization_id = $1 AND u.email = $2)").bind(org).bind(&email).fetch_one(&state.db).await?;
    if already {
        return Err(ApiError::conflict("That person is already a member."));
    }
    let token = crypto::token(32);
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    sqlx::query("INSERT INTO invitations (id, organization_id, email, role, token_hash, invited_by, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(id)
        .bind(org)
        .bind(&email)
        .bind(role.key())
        .bind(crypto::hash(&token))
        .bind(auth.user_id)
        .bind(Utc::now() + Duration::days(7))
        .execute(&mut *tx)
        .await
        .map_err(|e| match ApiError::from(e) {
            ApiError::Conflict(_) => ApiError::conflict("That address already has an open invitation."),
            other => other,
        })?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "invitation.created", "invitation")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "role": role.key() })),
    )
    .await?;
    let (org_name,): (String,) = sqlx::query_as("SELECT name FROM organizations WHERE id = $1")
        .bind(org)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    let link = format!(
        "{}/invitations/accept?token={token}",
        state.config.public_url.origin().ascii_serialization()
    );
    let body = format!("{} invited you to {org_name} on Taro Enterprise as {}.\n\nAccept the invitation (sign in with this address):\n{link}\n\nThe invitation works for 7 days.", auth.email, role.key());
    if let Err(e) = state
        .mailer
        .send(
            &email,
            &format!("Join {org_name} on Taro Enterprise"),
            &body,
        )
        .await
    {
        tracing::warn!(error = %crate::redact::redact(&e.to_string()), "invitation mail not sent");
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "email": email, "role": role.key() })),
    ))
}

async fn revoke_invitation(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    auth.require(&state, org, Permission::MembersManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE invitations SET revoked_at = now() WHERE id = $1 AND organization_id = $2 AND accepted_at IS NULL AND revoked_at IS NULL").bind(id).bind(org).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "invitation.revoked", "invitation")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Accept {
    token: String,
}

/// Accept with the token from the mail, signed in as the invited address.
async fn accept(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<Accept>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.db.begin().await?;
    let row: Option<(Uuid, Uuid, String, String)> = sqlx::query_as("SELECT id, organization_id, email, role FROM invitations WHERE token_hash = $1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now() FOR UPDATE").bind(crypto::hash(&b.token)).fetch_optional(&mut *tx).await?;
    let Some((id, org, email, role)) = row else {
        return Err(ApiError::invalid(
            "This invitation is not valid any more. Ask for a new one.",
        ));
    };
    if email != auth.email {
        return Err(ApiError::invalid(format!("This invitation is for another address. Sign in as the invited address to accept it ({}).", mask_email(&email))));
    }
    sqlx::query("INSERT INTO memberships (organization_id, user_id, role) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING").bind(org).bind(auth.user_id).bind(&role).execute(&mut *tx).await?;
    sqlx::query("UPDATE invitations SET accepted_at = now(), accepted_by = $2 WHERE id = $1")
        .bind(id)
        .bind(auth.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "invitation.accepted", "invitation")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "role": role })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "organization_id": org, "role": role })))
}

/// "j***@example.com"
fn mask_email(e: &str) -> String {
    match e.split_once('@') {
        Some((l, d)) => format!("{}***@{d}", l.chars().next().unwrap_or('*')),
        None => "***".into(),
    }
}

async fn audit_log(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    axum::extract::Query(q): axum::extract::Query<crate::Page>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require(&state, org, Permission::AuditRead).await?;
    let rows = audit::list(&state.db, Some(org), q.before, q.limit.unwrap_or(50)).await?;
    Ok(Json(json!({ "events": rows })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/organizations", get(list).post(create))
        .route("/api/v1/organizations/{org}", get(get_one).patch(update))
        .route("/api/v1/organizations/{org}/members", get(members))
        .route(
            "/api/v1/organizations/{org}/members/{user}",
            axum::routing::patch(change_role).delete(remove_member),
        )
        .route("/api/v1/organizations/{org}/invitations", post(invite))
        .route(
            "/api/v1/organizations/{org}/invitations/{id}/revoke",
            post(revoke_invitation),
        )
        .route("/api/v1/organizations/{org}/audit-events", get(audit_log))
        .route("/api/v1/invitations/accept", post(accept))
}

#[cfg(test)]
mod tests {
    #[test]
    fn slugs_and_masks() {
        assert_eq!(super::slugify("Halfpenny Mac Team!"), "halfpenny-mac-team");
        assert!(super::slugify("!!").starts_with("org-"));
        assert_eq!(super::mask_email("jane@example.com"), "j***@example.com");
    }
}
