//! The first visit to the Provider Console makes the first administrator, and administrators
//! sign in to the console with a password (a provider's own server may have no mail server).
//!
//! Whoever reaches the console first must not become its administrator: the first-run form asks
//! for the setup code that only the server's operator can read (`sudo taro-relay portal
//! setup-code`, or `TARO_SETUP_CODE`). Once a platform administrator exists the form is closed.
//! Passwords are kept as Argon2id hashes; a wrong address and a wrong password get one answer.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::audit::{self, Actor, Event};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::{valid_email, Auth, SignInFrom};
use crate::state::{AppState, Audience};

pub const MIN_PASSWORD: usize = 12;
const MAX_PASSWORD: usize = 256;

/// No platform administrator yet: the first-run form is open.
pub async fn needs_admin(db: &sqlx::PgPool) -> ApiResult<bool> {
    let (none,): (bool,) = sqlx::query_as(
        "SELECT NOT EXISTS (SELECT 1 FROM operator_grants WHERE role = 'platform_admin')",
    )
    .fetch_one(db)
    .await?;
    Ok(none)
}

/// The setup code this server expects, if any (the file is read each time: the relay writes it).
fn setup_code(state: &AppState) -> Option<String> {
    if let Some(c) = &state.config.setup_code {
        return Some(c.expose().trim().to_string());
    }
    let p = state.config.setup_code_file.as_ref()?;
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Codes are compared without dashes, spaces or case: `abcd-efgh` is `ABCDEFGH`.
fn normal(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub fn hash_password(p: &str) -> ApiResult<String> {
    Argon2::default()
        .hash_password(p.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("password hash: {e}")))
}

fn verify_password(p: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(p.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

fn check_new_password(p: &str) -> ApiResult<()> {
    if p.chars().count() < MIN_PASSWORD {
        return Err(ApiError::invalid(format!(
            "Choose a password of at least {MIN_PASSWORD} characters."
        )));
    }
    if p.len() > MAX_PASSWORD {
        return Err(ApiError::invalid("That password is too long."));
    }
    Ok(())
}

async fn status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({
        "needs_admin": needs_admin(&state.db).await?,
        "mail": state.config.smtp_url.is_some(),
    })))
}

#[derive(Deserialize)]
struct NewAdmin {
    setup_code: String,
    email: String,
    name: String,
    password: String,
}

async fn create_admin(
    State(state): State<AppState>,
    from: SignInFrom,
    Json(b): Json<NewAdmin>,
) -> ApiResult<Response> {
    if from.audience != Audience::Ops {
        return Err(ApiError::NotFound);
    }
    let ip = from.ip.clone().unwrap_or_default();
    if !state
        .limiter
        .allow("first_run", &ip, 10, std::time::Duration::from_secs(600))
    {
        return Err(ApiError::RateLimited);
    }
    if !needs_admin(&state.db).await? {
        return Err(ApiError::conflict(
            "This console already has an administrator. Sign in instead.",
        ));
    }
    let want = setup_code(&state).ok_or_else(|| {
        ApiError::Unavailable(
            "This server has no setup code. Run `sudo taro-relay portal setup-code` on it.".into(),
        )
    })?;
    if !crypto::eq(normal(&b.setup_code).as_bytes(), normal(&want).as_bytes()) {
        audit::record(
            &state.db,
            Event::new(Actor::System, "setup.admin_refused", "user")
                .result(audit::Outcome::Denied)
                .ip(from.ip.as_deref()),
        )
        .await?;
        return Err(ApiError::invalid(
            "The setup code is wrong. Read it on the server: sudo taro-relay portal setup-code",
        ));
    }
    let email = b.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(ApiError::invalid("Type your email address."));
    }
    let name: String = b.name.trim().chars().take(80).collect();
    if name.is_empty() {
        return Err(ApiError::invalid("Type your name."));
    }
    check_new_password(&b.password)?;
    let hash = {
        let p = b.password.clone();
        tokio::task::spawn_blocking(move || hash_password(&p))
            .await
            .map_err(|e| ApiError::Internal(e.into()))??
    };
    let mut tx = state.db.begin().await?;
    // two first visits at once: one administrator
    sqlx::query("SELECT pg_advisory_xact_lock(4207)")
        .execute(&mut *tx)
        .await?;
    let (taken,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM operator_grants WHERE role = 'platform_admin')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if taken {
        return Err(ApiError::conflict(
            "This console already has an administrator. Sign in instead.",
        ));
    }
    let (user, _) = crate::identity::find_or_create_user(&mut tx, &email, &name).await?;
    sqlx::query(
        "UPDATE users SET password_hash = $2, display_name = $3, updated_at = now() WHERE id = $1",
    )
    .bind(user)
    .bind(&hash)
    .bind(&name)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO operator_grants (user_id, role) VALUES ($1, 'platform_admin') ON CONFLICT DO NOTHING")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(Actor::Operator(user), "setup.admin_created", "user")
            .resource(user)
            .ip(from.ip.as_deref()),
    )
    .await?;
    let cookie = crate::identity::new_session(
        &state,
        &mut tx,
        user,
        Audience::Ops,
        from.ip.as_deref(),
        from.user_agent.as_deref(),
    )
    .await?;
    tx.commit().await?;
    // the code has done its work
    if let Some(p) = &state.config.setup_code_file {
        let _ = std::fs::remove_file(p);
    }
    let mut res = (StatusCode::CREATED, Json(json!({ "user_id": user }))).into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(res)
}

#[derive(Deserialize)]
struct PasswordSignIn {
    email: String,
    password: String,
}

/// A hash to check against when the address is unknown, so both answers take as long.
fn dummy_hash() -> &'static str {
    static H: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    H.get_or_init(|| hash_password("not a password of anyone").unwrap_or_default())
}

async fn password_sign_in(
    State(state): State<AppState>,
    from: SignInFrom,
    Json(b): Json<PasswordSignIn>,
) -> ApiResult<Response> {
    if from.audience != Audience::Ops {
        return Err(ApiError::NotFound);
    }
    let email = b.email.trim().to_lowercase();
    let ip = from.ip.clone().unwrap_or_default();
    if !state
        .limiter
        .allow("password_ip", &ip, 20, std::time::Duration::from_secs(600))
        || !state.limiter.allow(
            "password_email",
            &email,
            10,
            std::time::Duration::from_secs(600),
        )
    {
        return Err(ApiError::RateLimited);
    }
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT password_hash FROM users WHERE email = $1 AND disabled_at IS NULL")
            .bind(&email)
            .fetch_optional(&state.db)
            .await?;
    let hash = row.and_then(|r| r.0);
    let ok = {
        let (p, h, known) = (
            b.password.clone(),
            hash.clone().unwrap_or_else(|| dummy_hash().to_string()),
            hash.is_some(),
        );
        tokio::task::spawn_blocking(move || verify_password(&p, &h) && known)
            .await
            .map_err(|e| ApiError::Internal(e.into()))?
    };
    if !ok {
        audit::record(
            &state.db,
            Event::new(Actor::System, "auth.password_failed", "session")
                .result(audit::Outcome::Denied)
                .ip(from.ip.as_deref()),
        )
        .await?;
        return Err(ApiError::invalid(
            "The email address or the password is wrong.",
        ));
    }
    // the operator check, a fresh session and the audit, as every console sign-in
    let (cookie, user) =
        crate::identity::sign_in(&state, &from, &email, "", "password", None).await?;
    let mut res = Json(json!({ "user_id": user })).into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(res)
}

#[derive(Deserialize)]
struct PasswordChange {
    #[serde(default)]
    current: String,
    new: String,
}

async fn change_password(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<PasswordChange>,
) -> ApiResult<Json<serde_json::Value>> {
    if auth.audience != Audience::Ops {
        return Err(ApiError::NotFound);
    }
    check_new_password(&b.new)?;
    let (old,): (Option<String>,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
        .bind(auth.user_id)
        .fetch_one(&state.db)
        .await?;
    if let Some(h) = old {
        let p = b.current.clone();
        let ok = tokio::task::spawn_blocking(move || verify_password(&p, &h))
            .await
            .map_err(|e| ApiError::Internal(e.into()))?;
        if !ok {
            return Err(ApiError::invalid("Your current password is wrong."));
        }
    }
    let p = b.new.clone();
    let hash = tokio::task::spawn_blocking(move || hash_password(&p))
        .await
        .map_err(|e| ApiError::Internal(e.into()))??;
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(auth.user_id)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    // other sessions end: whoever knew the old password is signed out
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND id <> $2 AND revoked_at IS NULL")
        .bind(auth.user_id)
        .bind(auth.session_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "auth.password_changed", "user")
            .resource(auth.user_id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "changed": true })))
}

/// The console's first-run and password routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/setup/status", get(status))
        .route("/api/v1/setup/admin", post(create_admin))
        .route("/api/v1/auth/password", post(password_sign_in))
        .route("/api/v1/auth/password/change", post(change_password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_passwords() {
        assert_eq!(normal(" abcd-efgh jkLM "), "ABCDEFGHJKLM");
        let h = hash_password("correct horse battery").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &h));
        assert!(!verify_password("correct horse batterY", &h));
        assert!(!verify_password("x", "not a hash"));
        assert!(check_new_password("short").is_err());
        assert!(check_new_password("twelve chars").is_ok());
    }
}
