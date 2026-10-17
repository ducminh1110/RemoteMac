//! Sign-in and sessions.
//!
//! * Email: passwordless. A request sends one mail with a link and a 6-digit code (one use, 15
//!   minutes, five tries); the answer never says whether the address has an account.
//! * Google: OpenID Connect, authorization code flow with PKCE; state (bound to the browser by a
//!   cookie), nonce, issuer, audience, signature and expiry are all checked here, and only a
//!   verified Google email signs in.
//! * Sessions: a random token in an HttpOnly, SameSite=Lax cookie (`__Host-` and Secure over
//!   https), stored as its SHA-256; a CSRF token goes with it and every state-changing request
//!   carries it back in `X-CSRF-Token`. A new sign-in always makes a new session; sign-out and
//!   revocation end it on the server.
//!
//! The Customer Portal and the Provider Console have separate cookies and session audiences: an
//! operator signs in to the console on its own, and only users with an operator role can.

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Actor, Event, Outcome};
use crate::authz::{OperatorRole, Permission, Role};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Audience};

const EMAIL_TOKEN_MINUTES: i64 = 15;
const EMAIL_CODE_TRIES: i32 = 5;

fn cookie_name(state: &AppState, a: Audience) -> &'static str {
    match (state.config.secure_cookies(), a) {
        (true, Audience::Customer) => "__Host-taro_session",
        (true, Audience::Ops) => "__Host-taro_ops_session",
        (false, Audience::Customer) => "taro_session",
        (false, Audience::Ops) => "taro_ops_session",
    }
}

fn session_lifetime(a: Audience) -> Duration {
    match a {
        Audience::Customer => Duration::days(30),
        Audience::Ops => Duration::hours(12),
    }
}

/// How long a session may sit unused before it ends.
fn idle_limit(a: Audience) -> Duration {
    match a {
        Audience::Customer => Duration::days(7),
        Audience::Ops => Duration::hours(2),
    }
}

pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn set_cookie(state: &AppState, name: &str, value: &str, max_age: i64) -> HeaderValue {
    let secure = if state.config.secure_cookies() {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie is ASCII")
}

/// The caller's IP: the socket's, or the last X-Forwarded-For hop behind a trusted proxy.
pub fn client_ip(parts: &Parts, state: &AppState) -> Option<String> {
    if state.config.trusted_proxies {
        if let Some(f) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        {
            // the hop our own proxy added (the last): earlier ones are the client's to make up
            return f.rsplit(',').next().map(|s| s.trim().to_string());
        }
    }
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
}

/// A signed-in caller. Taking it as an argument requires a session of this listener's audience
/// and, for any request that changes something, the session's CSRF token.
#[derive(Debug, Clone)]
pub struct Auth {
    pub user_id: Uuid,
    pub email: String,
    pub session_id: Uuid,
    pub audience: Audience,
    pub csrf: String,
    pub authenticated_at: DateTime<Utc>,
    pub operator_roles: Vec<OperatorRole>,
    pub ip: Option<String>,
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let audience = *parts
            .extensions
            .get::<Audience>()
            .ok_or(ApiError::Unauthenticated)?;
        let token = read_cookie(&parts.headers, cookie_name(state, audience))
            .ok_or(ApiError::Unauthenticated)?;
        let row: Option<(Uuid, Uuid, String, DateTime<Utc>, DateTime<Utc>, String)> = sqlx::query_as(
            "SELECT s.id, s.user_id, s.csrf_token, s.authenticated_at, s.last_seen_at, u.email FROM sessions s JOIN users u ON u.id = s.user_id
             WHERE s.token_hash = $1 AND s.audience = $2 AND s.revoked_at IS NULL AND s.expires_at > now() AND u.disabled_at IS NULL",
        )
        .bind(crypto::hash(&token))
        .bind(audience.key())
        .fetch_optional(&state.db)
        .await?;
        let Some((session_id, user_id, csrf, authenticated_at, last_seen, email)) = row else {
            return Err(ApiError::Unauthenticated);
        };
        if Utc::now() - last_seen > idle_limit(audience) {
            sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
                .bind(session_id)
                .execute(&state.db)
                .await?;
            return Err(ApiError::Unauthenticated);
        }
        if Utc::now() - last_seen > Duration::minutes(5) {
            sqlx::query("UPDATE sessions SET last_seen_at = now() WHERE id = $1")
                .bind(session_id)
                .execute(&state.db)
                .await?;
        }
        if !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS) {
            let sent = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !crypto::eq(sent.as_bytes(), csrf.as_bytes()) {
                return Err(ApiError::Csrf);
            }
        }
        let operator_roles = if audience == Audience::Ops {
            operator_roles(&state.db, user_id).await?
        } else {
            vec![]
        };
        Ok(Auth {
            user_id,
            email,
            session_id,
            audience,
            csrf,
            authenticated_at,
            operator_roles,
            ip: client_ip(parts, state),
        })
    }
}

pub async fn operator_roles(db: &sqlx::PgPool, user: Uuid) -> ApiResult<Vec<OperatorRole>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT role FROM operator_grants WHERE user_id = $1")
            .bind(user)
            .fetch_all(db)
            .await?;
    Ok(rows
        .iter()
        .filter_map(|(r,)| OperatorRole::parse(r))
        .collect())
}

impl Auth {
    /// The caller's role in `org`. Not a member: NotFound (whether the organization exists is
    /// none of their business).
    pub async fn org_role(&self, state: &AppState, org: Uuid) -> ApiResult<Role> {
        if self.audience != Audience::Customer {
            return Err(ApiError::Forbidden);
        }
        let r: Option<(String,)> = sqlx::query_as("SELECT m.role FROM memberships m JOIN organizations o ON o.id = m.organization_id WHERE m.organization_id = $1 AND m.user_id = $2").bind(org).bind(self.user_id).fetch_optional(&state.db).await?;
        r.and_then(|(r,)| Role::parse(&r)).ok_or(ApiError::NotFound)
    }

    /// The caller's role in `org`, which must grant `p`; a refusal is audited.
    pub async fn require(&self, state: &AppState, org: Uuid, p: Permission) -> ApiResult<Role> {
        let role = self.org_role(state, org).await?;
        if role.can(p) {
            return Ok(role);
        }
        audit::record(
            &state.db,
            Event::new(Actor::User(self.user_id), "authz.denied", "permission")
                .org(org)
                .resource(p.key())
                .result(Outcome::Denied)
                .ip(self.ip.as_deref()),
        )
        .await?;
        Err(ApiError::Forbidden)
    }

    /// An operator permission (Provider Console only).
    pub async fn require_op(&self, state: &AppState, p: Permission) -> ApiResult<()> {
        if self.audience == Audience::Ops
            && self
                .operator_roles
                .iter()
                .any(|r| r.permissions().contains(&p))
        {
            return Ok(());
        }
        audit::record(
            &state.db,
            Event::new(
                if self.audience == Audience::Ops {
                    Actor::Operator(self.user_id)
                } else {
                    Actor::User(self.user_id)
                },
                "authz.denied",
                "permission",
            )
            .resource(p.key())
            .result(Outcome::Denied)
            .ip(self.ip.as_deref()),
        )
        .await?;
        Err(ApiError::Forbidden)
    }

    /// Sensitive changes need a recent sign-in, not only a live session.
    pub fn require_recent(&self, minutes: i64) -> ApiResult<()> {
        if Utc::now() - self.authenticated_at <= Duration::minutes(minutes) {
            Ok(())
        } else {
            Err(ApiError::Conflict(format!(
                "sign in again to do this (a sign-in in the last {minutes} minutes is needed)"
            )))
        }
    }

    pub fn actor(&self) -> Actor {
        match self.audience {
            Audience::Customer => Actor::User(self.user_id),
            Audience::Ops => Actor::Operator(self.user_id),
        }
    }
}

// ------------------------------------------------------------------ users and sessions

async fn find_or_create_user(
    tx: &mut sqlx::PgConnection,
    email: &str,
    name: &str,
) -> ApiResult<(Uuid, bool)> {
    if let Some((id,)) = sqlx::query_as::<_, (Uuid,)>("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(&mut *tx)
        .await?
    {
        sqlx::query("UPDATE users SET email_verified = true, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        return Ok((id, false));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, display_name, email_verified) VALUES ($1, $2, $3, true)",
    )
    .bind(id)
    .bind(email)
    .bind(name)
    .execute(&mut *tx)
    .await?;
    Ok((id, true))
}

/// The customer account of `email`, made if it does not exist yet (an operator assigning a Mac).
pub async fn customer_user(
    state: &AppState,
    email: &str,
    ip: Option<&str>,
    method: &str,
) -> ApiResult<Uuid> {
    let mut tx = state.db.begin().await?;
    let (user, created) = find_or_create_user(&mut tx, email, "").await?;
    if created {
        audit::record(
            &mut *tx,
            Event::new(Actor::User(user), "user.created", "user")
                .resource(user)
                .ip(ip)
                .details(json!({ "method": method })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(user)
}

/// The configured first administrator becomes platform_admin while the platform has none.
async fn bootstrap_admin(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    user: Uuid,
    email: &str,
) -> ApiResult<()> {
    if state.config.bootstrap_admin_email.as_deref() != Some(email) {
        return Ok(());
    }
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM operator_grants WHERE role = 'platform_admin'")
            .fetch_one(&mut *tx)
            .await?;
    if n == 0 {
        sqlx::query("INSERT INTO operator_grants (user_id, role) VALUES ($1, 'platform_admin') ON CONFLICT DO NOTHING").bind(user).execute(&mut *tx).await?;
        audit::record(
            &mut *tx,
            Event::new(Actor::System, "operator.bootstrap_admin", "user").resource(user),
        )
        .await?;
    }
    Ok(())
}

/// Start a session; the Set-Cookie header for it.
async fn new_session(
    state: &AppState,
    tx: &mut sqlx::PgConnection,
    user: Uuid,
    audience: Audience,
    ip: Option<&str>,
    ua: Option<&str>,
) -> ApiResult<HeaderValue> {
    let token = crypto::token(32);
    let life = session_lifetime(audience);
    sqlx::query("INSERT INTO sessions (id, user_id, audience, token_hash, csrf_token, expires_at, ip, user_agent) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(Uuid::new_v4())
        .bind(user)
        .bind(audience.key())
        .bind(crypto::hash(&token))
        .bind(crypto::token(24))
        .bind(Utc::now() + life)
        .bind(ip)
        .bind(ua.map(|u| u.chars().take(200).collect::<String>()))
        .execute(&mut *tx)
        .await?;
    Ok(set_cookie(
        state,
        cookie_name(state, audience),
        &token,
        life.num_seconds(),
    ))
}

/// Finish a sign-in for a known address: the user (created on the portal, never on the
/// console), the operator check, and a fresh session (the old one, if any, ends).
async fn sign_in(
    state: &AppState,
    parts: &SignInFrom,
    email: &str,
    name: &str,
    method: &str,
    google_sub: Option<&str>,
) -> ApiResult<(HeaderValue, Uuid)> {
    let mut tx = state.db.begin().await?;
    let user = match parts.audience {
        Audience::Customer => {
            let (user, created) = find_or_create_user(&mut tx, email, name).await?;
            if created {
                audit::record(
                    &mut *tx,
                    Event::new(Actor::User(user), "user.created", "user")
                        .resource(user)
                        .ip(parts.ip.as_deref())
                        .details(json!({ "method": method })),
                )
                .await?;
            }
            user
        }
        Audience::Ops => {
            let existing: Option<(Uuid,)> =
                sqlx::query_as("SELECT id FROM users WHERE email = $1 AND disabled_at IS NULL")
                    .bind(email)
                    .fetch_optional(&mut *tx)
                    .await?;
            let user = match existing {
                Some((u,)) => u,
                None if state.config.bootstrap_admin_email.as_deref() == Some(email) => {
                    find_or_create_user(&mut tx, email, name).await?.0
                }
                None => return Err(deny_ops(state, None, parts, method).await),
            };
            bootstrap_admin(state, &mut tx, user, email).await?;
            let (n,): (i64,) =
                sqlx::query_as("SELECT count(*) FROM operator_grants WHERE user_id = $1")
                    .bind(user)
                    .fetch_one(&mut *tx)
                    .await?;
            if n == 0 {
                tx.rollback().await?;
                return Err(deny_ops(state, Some(user), parts, method).await);
            }
            user
        }
    };
    bootstrap_admin(state, &mut tx, user, email).await?;
    if let Some(sub) = google_sub {
        let linked = sqlx::query("INSERT INTO auth_identities (id, user_id, provider, subject, email, last_used_at) VALUES ($1, $2, 'google', $3, $4, now()) ON CONFLICT (provider, subject) DO UPDATE SET last_used_at = now()")
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(sub)
            .bind(email)
            .execute(&mut *tx)
            .await?;
        if linked.rows_affected() > 0 {
            audit::record(
                &mut *tx,
                Event::new(Actor::User(user), "identity.google_used", "user").resource(user),
            )
            .await?;
        }
    } else {
        sqlx::query("INSERT INTO auth_identities (id, user_id, provider, subject, email, last_used_at) VALUES ($1, $2, 'email', $3, $3, now()) ON CONFLICT (provider, subject) DO UPDATE SET last_used_at = now()").bind(Uuid::new_v4()).bind(user).bind(email).execute(&mut *tx).await?;
    }
    if let Some(old) = &parts.old_cookie {
        sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(crypto::hash(old))
        .execute(&mut *tx)
        .await?;
    }
    let cookie = new_session(
        state,
        &mut tx,
        user,
        parts.audience,
        parts.ip.as_deref(),
        parts.user_agent.as_deref(),
    )
    .await?;
    let action = if parts.audience == Audience::Ops {
        "auth.ops_sign_in"
    } else {
        "auth.sign_in"
    };
    audit::record(
        &mut *tx,
        Event::new(
            if parts.audience == Audience::Ops {
                Actor::Operator(user)
            } else {
                Actor::User(user)
            },
            action,
            "session",
        )
        .resource(user)
        .ip(parts.ip.as_deref())
        .details(json!({ "method": method })),
    )
    .await?;
    tx.commit().await?;
    state.metrics.inc(
        "taro_sign_ins_total",
        &[
            ("method", method),
            ("audience", parts.audience.key()),
            ("result", "success"),
        ],
    );
    Ok((cookie, user))
}

async fn deny_ops(
    state: &AppState,
    user: Option<Uuid>,
    parts: &SignInFrom,
    method: &str,
) -> ApiError {
    let _ = audit::record(
        &state.db,
        Event::new(
            user.map_or(Actor::System, Actor::User),
            "auth.ops_sign_in",
            "session",
        )
        .result(Outcome::Denied)
        .ip(parts.ip.as_deref())
        .details(json!({ "method": method })),
    )
    .await;
    state.metrics.inc(
        "taro_sign_ins_total",
        &[
            ("method", method),
            ("audience", "ops"),
            ("result", "denied"),
        ],
    );
    ApiError::Forbidden
}

/// Where a sign-in comes from.
pub struct SignInFrom {
    pub audience: Audience,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub old_cookie: Option<String>,
}

impl FromRequestParts<AppState> for SignInFrom {
    type Rejection = ApiError;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let audience = *parts
            .extensions
            .get::<Audience>()
            .ok_or(ApiError::Forbidden)?;
        Ok(SignInFrom {
            audience,
            ip: client_ip(parts, state),
            user_agent: parts
                .headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
            old_cookie: read_cookie(&parts.headers, cookie_name(state, audience)),
        })
    }
}

// ------------------------------------------------------------------ email

pub fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && domain.len() <= 253
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && e.len() <= 254
        && !e.contains(char::is_whitespace)
        && !domain.contains('@')
}

#[derive(Deserialize)]
struct EmailStart {
    email: String,
}

async fn email_start(
    State(state): State<AppState>,
    from: SignInFrom,
    Json(b): Json<EmailStart>,
) -> ApiResult<impl IntoResponse> {
    send_code(&state, from.audience, from.ip.clone(), &b.email).await?;
    // the same answer whether or not the address has an account
    Ok((StatusCode::ACCEPTED, Json(json!({ "sent": true }))))
}

/// Mail a sign-in code (and link) to `email` for `audience` (the Taro app signs in with it too:
/// devices.rs).
pub async fn send_code(
    state: &AppState,
    audience: Audience,
    ip: Option<String>,
    email: &str,
) -> ApiResult<()> {
    let from = SignInFrom {
        audience,
        ip,
        user_agent: None,
        old_cookie: None,
    };
    let email = email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(ApiError::invalid("Type a valid email address."));
    }
    let ip = from.ip.clone().unwrap_or_default();
    if !state.limiter.allow(
        "email_start_ip",
        &ip,
        20,
        std::time::Duration::from_secs(600),
    ) || !state.limiter.allow(
        "email_start_addr",
        &email,
        5,
        std::time::Duration::from_secs(900),
    ) {
        state
            .metrics
            .inc("taro_rate_limited_total", &[("bucket", "email_start")]);
        return Err(ApiError::RateLimited);
    }
    let token = crypto::token(32);
    let code = crypto::code();
    sqlx::query("INSERT INTO email_login_tokens (id, email, audience, token_hash, code_hash, expires_at, ip) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(Uuid::new_v4())
        .bind(&email)
        .bind(from.audience.key())
        .bind(crypto::hash(&token))
        .bind(crypto::hash(&format!("{email}:{code}")))
        .bind(Utc::now() + Duration::minutes(EMAIL_TOKEN_MINUTES))
        .bind(&from.ip)
        .execute(&state.db)
        .await?;
    let origin = from.audience.origin(&state.config);
    let product = if from.audience == Audience::Ops {
        "Taro Operations"
    } else {
        "Taro Enterprise"
    };
    let body = format!(
        "Sign in to {product}\n\nOpen this link to sign in:\n{origin}/auth/verify?token={token}\n\nOr type this code: {code}\n\nThe link and the code work once, for {EMAIL_TOKEN_MINUTES} minutes. If you did not ask to sign in, ignore this mail."
    );
    if let Err(e) = state
        .mailer
        .send(
            &email,
            &format!("Your {product} sign-in code: {code}"),
            &body,
        )
        .await
    {
        tracing::error!(error = %crate::redact::redact(&e.to_string()), "sign-in mail not sent");
        return Err(ApiError::Unavailable(
            "The sign-in mail could not be sent. Try again in a minute.".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct EmailVerify {
    token: Option<String>,
    email: Option<String>,
    code: Option<String>,
}

async fn email_verify(
    State(state): State<AppState>,
    from: SignInFrom,
    Json(b): Json<EmailVerify>,
) -> ApiResult<Response> {
    let ip = from.ip.clone().unwrap_or_default();
    if !state.limiter.allow(
        "email_verify_ip",
        &ip,
        30,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let fail =
        || ApiError::invalid("That link or code is wrong or has expired. Ask for a new one.");
    let email = match (b.token, b.email, b.code) {
        (Some(token), _, _) => {
            let row: Option<(String,)> = sqlx::query_as("UPDATE email_login_tokens SET used_at = now() WHERE token_hash = $1 AND audience = $2 AND used_at IS NULL AND expires_at > now() RETURNING email")
                .bind(crypto::hash(&token))
                .bind(from.audience.key())
                .fetch_optional(&state.db)
                .await?;
            row.ok_or_else(fail)?.0
        }
        (None, Some(email), Some(code)) => check_code(&state, from.audience, &email, &code).await?,
        _ => {
            return Err(ApiError::invalid(
                "Send the token from the link, or the email address and the code.",
            ))
        }
    };
    let (cookie, user) = sign_in(&state, &from, &email, "", "email", None).await?;
    let mut res = Json(json!({ "user_id": user })).into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(res)
}

/// The address whose newest open code this is (once; five wrong tries burn the request).
pub async fn check_code(
    state: &AppState,
    audience: Audience,
    email: &str,
    code: &str,
) -> ApiResult<String> {
    let fail =
        || ApiError::invalid("That link or code is wrong or has expired. Ask for a new one.");
    let from = SignInFrom {
        audience,
        ip: None,
        user_agent: None,
        old_cookie: None,
    };
    let email = email.trim().to_lowercase();
    let code = code.trim().to_string();
    // the newest open request for this address takes the try
    let row: Option<(Uuid, Vec<u8>, i32)> = sqlx::query_as("SELECT id, code_hash, attempts FROM email_login_tokens WHERE email = $1 AND audience = $2 AND used_at IS NULL AND expires_at > now() ORDER BY created_at DESC LIMIT 1")
                .bind(&email)
                .bind(from.audience.key())
                .fetch_optional(&state.db)
                .await?;
    let (id, code_hash, attempts) = row.ok_or_else(fail)?;
    if attempts >= EMAIL_CODE_TRIES {
        return Err(fail());
    }
    if !crypto::eq(&crypto::hash(&format!("{email}:{code}")), &code_hash) {
        // the fifth wrong code burns the request
        sqlx::query("UPDATE email_login_tokens SET attempts = attempts + 1, used_at = CASE WHEN attempts + 1 >= $2 THEN now() ELSE NULL END WHERE id = $1").bind(id).bind(EMAIL_CODE_TRIES).execute(&state.db).await?;
        state.metrics.inc(
            "taro_sign_ins_total",
            &[
                ("method", "email_code"),
                ("audience", from.audience.key()),
                ("result", "wrong_code"),
            ],
        );
        return Err(fail());
    }
    let used = sqlx::query(
        "UPDATE email_login_tokens SET used_at = now() WHERE id = $1 AND used_at IS NULL",
    )
    .bind(id)
    .execute(&state.db)
    .await?;
    if used.rows_affected() == 0 {
        return Err(fail());
    }
    Ok(email)
}

// ------------------------------------------------------------------ Google (OpenID Connect)

#[derive(Deserialize)]
struct GoogleStart {
    return_to: Option<String>,
}

/// Only paths on this site: never `//other.host` or a full URL.
fn safe_return(p: Option<&str>) -> String {
    match p {
        Some(p)
            if p.starts_with('/') && !p.starts_with("//") && !p.contains('\\') && p.len() < 512 =>
        {
            p.to_string()
        }
        _ => "/".into(),
    }
}

fn redirect_uri(state: &AppState, a: Audience) -> String {
    format!("{}/api/v1/auth/google/callback", a.origin(&state.config))
}

fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

async fn google_start(
    State(state): State<AppState>,
    from: SignInFrom,
    Query(q): Query<GoogleStart>,
) -> ApiResult<Response> {
    let g = state.config.google.as_ref().ok_or(ApiError::NotFound)?;
    if !state.limiter.allow(
        "google_start_ip",
        from.ip.as_deref().unwrap_or(""),
        30,
        std::time::Duration::from_secs(600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let (st, nonce, verifier) = (crypto::token(24), crypto::token(24), crypto::token(48));
    sqlx::query("INSERT INTO oidc_states (state, nonce, pkce_verifier, audience, return_to, expires_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(&st)
        .bind(&nonce)
        .bind(&verifier)
        .bind(from.audience.key())
        .bind(safe_return(q.return_to.as_deref()))
        .bind(Utc::now() + Duration::minutes(10))
        .execute(&state.db)
        .await?;
    let mut u =
        url::Url::parse(&state.config.google_auth_url).map_err(|e| ApiError::Internal(e.into()))?;
    u.query_pairs_mut()
        .append_pair("client_id", &g.client_id)
        .append_pair("redirect_uri", &redirect_uri(&state, from.audience))
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", &st)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &pkce_challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account");
    let mut res = Redirect::to(u.as_str()).into_response();
    // binds the sign-in to this browser: the callback must come back to the browser that started it
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, "taro_oidc", &st, 600),
    );
    Ok(res)
}

#[derive(Deserialize)]
struct GoogleCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GoogleClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub nonce: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub name: Option<String>,
}

async fn google_keys(state: &AppState) -> anyhow::Result<jsonwebtoken::jwk::JwkSet> {
    if let Some((at, set)) = state.jwks.read().await.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(3600) {
            return Ok(set.clone());
        }
    }
    let set: jsonwebtoken::jwk::JwkSet = state
        .http
        .get(&state.config.google_jwks_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    *state.jwks.write().await = Some((std::time::Instant::now(), set.clone()));
    Ok(set)
}

/// Verify a Google ID token: signature (a key from Google's set), issuer, audience, expiry and
/// nonce. Returns its claims.
pub async fn verify_id_token(
    state: &AppState,
    id_token: &str,
    nonce: &str,
) -> ApiResult<GoogleClaims> {
    let g = state.config.google.as_ref().ok_or(ApiError::NotFound)?;
    let header = jsonwebtoken::decode_header(id_token).map_err(|_| ApiError::Unauthenticated)?;
    if header.alg != jsonwebtoken::Algorithm::RS256 {
        return Err(ApiError::Unauthenticated);
    }
    let kid = header.kid.ok_or(ApiError::Unauthenticated)?;
    let keys = google_keys(state)
        .await
        .map_err(|e| ApiError::Unavailable(format!("Google's keys could not be fetched: {e}")))?;
    let jwk = keys.find(&kid).ok_or(ApiError::Unauthenticated)?;
    let key = jsonwebtoken::DecodingKey::from_jwk(jwk).map_err(|_| ApiError::Unauthenticated)?;
    let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    v.set_audience(&[&g.client_id]);
    let issuers = [
        state.config.google_issuer.clone(),
        state
            .config
            .google_issuer
            .trim_start_matches("https://")
            .to_string(),
    ];
    v.set_issuer(&issuers);
    v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let data = jsonwebtoken::decode::<GoogleClaims>(id_token, &key, &v)
        .map_err(|_| ApiError::Unauthenticated)?;
    if data.claims.nonce.as_deref() != Some(nonce) {
        return Err(ApiError::Unauthenticated);
    }
    Ok(data.claims)
}

async fn google_callback(
    State(state): State<AppState>,
    from: SignInFrom,
    headers: HeaderMap,
    Query(q): Query<GoogleCallback>,
) -> ApiResult<Response> {
    let g = state
        .config
        .google
        .as_ref()
        .ok_or(ApiError::NotFound)?
        .clone();
    let origin = from.audience.origin(&state.config);
    let back = |why: &str| Redirect::to(&format!("{origin}/sign-in?error={why}")).into_response();
    if q.error.is_some() {
        return Ok(back("cancelled"));
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return Ok(back("invalid"));
    };
    if read_cookie(&headers, "taro_oidc").as_deref() != Some(st.as_str()) {
        return Ok(back("invalid"));
    }
    let row: Option<(String, String, String, String)> = sqlx::query_as("DELETE FROM oidc_states WHERE state = $1 AND expires_at > now() RETURNING nonce, pkce_verifier, audience, return_to").bind(&st).fetch_optional(&state.db).await?;
    let Some((nonce, verifier, audience, return_to)) = row else {
        return Ok(back("expired"));
    };
    if audience != from.audience.key() {
        return Ok(back("invalid"));
    }
    let form = [
        ("code", code.as_str()),
        ("client_id", g.client_id.as_str()),
        ("client_secret", g.client_secret.expose()),
        ("redirect_uri", &redirect_uri(&state, from.audience)),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier.as_str()),
    ];
    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: String,
    }
    let tok: TokenResponse = match state
        .http
        .post(&state.config.google_token_url)
        .form(&form)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            r.json().await.map_err(|e| ApiError::Internal(e.into()))?
        }
        Ok(r) => {
            tracing::warn!(status = %r.status(), "Google refused the code exchange");
            return Ok(back("invalid"));
        }
        Err(e) => {
            return Err(ApiError::Unavailable(format!(
                "Google could not be reached: {e}"
            )))
        }
    };
    let claims = match verify_id_token(&state, &tok.id_token, &nonce).await {
        Ok(c) => c,
        Err(ApiError::Unavailable(m)) => return Err(ApiError::Unavailable(m)),
        Err(_) => {
            state.metrics.inc(
                "taro_sign_ins_total",
                &[
                    ("method", "google"),
                    ("audience", from.audience.key()),
                    ("result", "bad_token"),
                ],
            );
            return Ok(back("invalid"));
        }
    };
    let Some(email) = claims
        .email
        .filter(|_| claims.email_verified == Some(true))
        .map(|e| e.to_lowercase())
    else {
        return Ok(back("unverified_email"));
    };
    // an identity seen before signs in its own user, whatever its email says today
    let linked: Option<(String,)> = sqlx::query_as("SELECT u.email FROM auth_identities i JOIN users u ON u.id = i.user_id WHERE i.provider = 'google' AND i.subject = $1").bind(&claims.sub).fetch_optional(&state.db).await?;
    let email = linked.map(|(e,)| e).unwrap_or(email);
    match sign_in(
        &state,
        &from,
        &email,
        claims.name.as_deref().unwrap_or(""),
        "google",
        Some(&claims.sub),
    )
    .await
    {
        Ok((cookie, _)) => {
            let mut res = Redirect::to(&format!("{origin}{return_to}")).into_response();
            res.headers_mut().append(header::SET_COOKIE, cookie);
            res.headers_mut()
                .append(header::SET_COOKIE, set_cookie(&state, "taro_oidc", "", 0));
            Ok(res)
        }
        Err(ApiError::Forbidden) => Ok(back("not_an_operator")),
        Err(e) => Err(e),
    }
}

// ------------------------------------------------------------------ me, sign-out, sessions

#[derive(Serialize, sqlx::FromRow)]
struct OrgOfMine {
    id: Uuid,
    name: String,
    slug: String,
    role: String,
}

async fn me(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<serde_json::Value>> {
    let (name,): (String,) = sqlx::query_as("SELECT display_name FROM users WHERE id = $1")
        .bind(auth.user_id)
        .fetch_one(&state.db)
        .await?;
    let orgs: Vec<OrgOfMine> = if auth.audience == Audience::Customer {
        sqlx::query_as("SELECT o.id, o.name, o.slug, m.role FROM memberships m JOIN organizations o ON o.id = m.organization_id WHERE m.user_id = $1 ORDER BY o.name").bind(auth.user_id).fetch_all(&state.db).await?
    } else {
        vec![]
    };
    let ops_permissions: Vec<String> = {
        let mut p: Vec<String> = auth
            .operator_roles
            .iter()
            .flat_map(|r| r.permissions())
            .map(|p| p.key())
            .collect();
        p.sort();
        p.dedup();
        p
    };
    let roles: Vec<serde_json::Value> = orgs.iter().filter_map(|o| Role::parse(&o.role).map(|r| json!({ "organization_id": o.id, "permissions": r.permissions().iter().map(|p| p.key()).collect::<Vec<_>>() }))).collect();
    Ok(Json(json!({
        "id": auth.user_id,
        "email": auth.email,
        "display_name": name,
        "csrf_token": auth.csrf,
        "audience": auth.audience.key(),
        "organizations": orgs,
        "organization_permissions": roles,
        "operator_roles": auth.operator_roles.iter().map(|r| r.key()).collect::<Vec<_>>(),
        "operator_permissions": ops_permissions,
    })))
}

async fn sign_out(State(state): State<AppState>, auth: Auth) -> ApiResult<Response> {
    sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1")
        .bind(auth.session_id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state.db,
        Event::new(auth.actor(), "auth.sign_out", "session")
            .resource(auth.session_id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, cookie_name(&state, auth.audience), "", 0),
    );
    Ok(res)
}

#[derive(Serialize, sqlx::FromRow)]
struct SessionRow {
    id: Uuid,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    ip: Option<String>,
    user_agent: Option<String>,
}

async fn sessions(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<SessionRow> = sqlx::query_as("SELECT id, created_at, last_seen_at, expires_at, ip, user_agent FROM sessions WHERE user_id = $1 AND audience = $2 AND revoked_at IS NULL AND expires_at > now() ORDER BY last_seen_at DESC")
        .bind(auth.user_id)
        .bind(auth.audience.key())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(
        json!({ "current": auth.session_id, "sessions": rows }),
    ))
}

async fn revoke_session(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let r = sqlx::query("UPDATE sessions SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL").bind(id).bind(auth.user_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &state.db,
        Event::new(auth.actor(), "auth.session_revoked", "session")
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_others(
    State(state): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<serde_json::Value>> {
    let r = sqlx::query("UPDATE sessions SET revoked_at = now() WHERE user_id = $1 AND id <> $2 AND revoked_at IS NULL").bind(auth.user_id).bind(auth.session_id).execute(&state.db).await?;
    audit::record(
        &state.db,
        Event::new(auth.actor(), "auth.other_sessions_revoked", "session")
            .details(json!({ "count": r.rows_affected() }))
            .ip(auth.ip.as_deref()),
    )
    .await?;
    Ok(Json(json!({ "revoked": r.rows_affected() })))
}

#[derive(Deserialize)]
struct Profile {
    display_name: String,
}

async fn update_me(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<Profile>,
) -> ApiResult<StatusCode> {
    let name = b.display_name.trim();
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError::invalid("A name is at most 80 characters."));
    }
    sqlx::query("UPDATE users SET display_name = $1, updated_at = now() WHERE id = $2")
        .bind(name)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn providers(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "google": state.config.google.is_some(),
        "email": true,
        "environment": state.config.env.as_str(),
        "simulated_provider": state.provider.is_simulated(),
        "app_download_url": state.config.app_download_url,
        "billing": state.billing.is_some(),
        "billing_live": state.config.stripe.as_ref().is_some_and(|s| s.live),
    }))
}

/// Development only: the mail that would have been sent (sign-in codes), newest first.
async fn dev_outbox(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    if state.config.env != crate::config::Env::Development || state.mailer.kind() != "dev_outbox" {
        return Err(ApiError::NotFound);
    }
    let rows: Vec<(String, String, String, DateTime<Utc>)> = sqlx::query_as("SELECT to_email, subject, body, created_at FROM dev_outbox ORDER BY created_at DESC LIMIT 20").fetch_all(&state.db).await?;
    Ok(Json(json!(rows.into_iter().map(|(to, subject, body, at)| json!({ "to": to, "subject": subject, "body": body, "at": at })).collect::<Vec<_>>())))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/providers", get(providers))
        .route("/api/v1/auth/email/start", post(email_start))
        .route("/api/v1/auth/email/verify", post(email_verify))
        .route("/api/v1/auth/google/start", get(google_start))
        .route("/api/v1/auth/google/callback", get(google_callback))
        .route("/api/v1/auth/sign-out", post(sign_out))
        .route("/api/v1/auth/sessions", get(sessions))
        .route("/api/v1/auth/sessions/{id}/revoke", post(revoke_session))
        .route("/api/v1/auth/sessions/revoke-others", post(revoke_others))
        .route("/api/v1/me", get(me).patch(update_me))
        .route("/api/v1/dev/outbox", get(dev_outbox))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails_and_return_paths() {
        assert!(valid_email("a@b.co") && valid_email("first.last+tag@example.com"));
        assert!(
            !valid_email("a@b")
                && !valid_email("@b.co")
                && !valid_email("a b@c.co")
                && !valid_email("a@.co")
                && !valid_email("a@b@c.co")
        );
        assert_eq!(safe_return(Some("/my-macs?x=1")), "/my-macs?x=1");
        for bad in [
            "//evil.example",
            "https://evil.example",
            "\\\\evil",
            "javascript:alert(1)",
        ] {
            assert_eq!(safe_return(Some(bad)), "/", "{bad}");
        }
        // RFC 7636 appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn cookies_are_read_by_name() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; taro_session=abc; b=2"),
        );
        assert_eq!(read_cookie(&h, "taro_session").as_deref(), Some("abc"));
        assert_eq!(read_cookie(&h, "taro"), None);
    }
}
