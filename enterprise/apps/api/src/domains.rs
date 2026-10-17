//! Custom domains and TLS.
//!
//! An organization adds a hostname; Taro hands out a verification token to publish as the TXT
//! record `_taro-verify.<hostname>`; verification reads DNS for real. Only then may the TLS
//! front end (Caddy with on-demand TLS) get a certificate: it asks `GET /internal/tls/ask` on
//! the ops listener (loopback only), which says yes for verified hostnames and the platform's
//! own. A worker checks each hostname over HTTPS (a valid certificate and `/healthz`), records
//! when its certificate expires, and opens an incident when renewal looks stuck (under 14 days
//! left) or the check fails. Private keys stay with Caddy.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::authz::Permission;
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::identity::Auth;
use crate::state::AppState;

/// A hostname a customer may claim: lowercase labels of letters, digits and hyphens, at least
/// two labels, no IP addresses, not the platform's own.
pub fn valid_hostname(h: &str, reserved: &[&str]) -> Result<String, &'static str> {
    let h = h.trim().trim_end_matches('.').to_lowercase();
    if h.len() > 253 || !h.contains('.') {
        return Err("Type a full hostname, like portal.example.com.");
    }
    if h.parse::<std::net::IpAddr>().is_ok() {
        return Err("An IP address cannot be a custom domain.");
    }
    for label in h.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err("A hostname is letters, digits, hyphens and dots.");
        }
    }
    if h.split('.')
        .next_back()
        .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
    {
        return Err("That is not a public hostname.");
    }
    if reserved
        .iter()
        .any(|r| h == *r || h.ends_with(&format!(".{r}")))
    {
        return Err("That hostname belongs to the platform.");
    }
    Ok(h)
}

fn reserved(state: &AppState) -> Vec<String> {
    [
        state.config.public_url.host_str(),
        state.config.ops_url.host_str(),
    ]
    .into_iter()
    .flatten()
    .map(String::from)
    .collect()
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Domain {
    pub id: Uuid,
    pub organization_id: Option<Uuid>,
    pub hostname: String,
    pub kind: String,
    pub state: String,
    pub verification_token: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub tls_checked_at: Option<DateTime<Utc>>,
    pub tls_expires_at: Option<DateTime<Utc>>,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, organization_id, hostname, kind, state, verification_token, verified_at, tls_checked_at, tls_expires_at, last_check_at, last_error, created_at";

fn instructions(state: &AppState, d: &Domain) -> serde_json::Value {
    let target = state.config.public_url.host_str().unwrap_or("portal");
    json!({
        "records": [
            { "type": "TXT", "name": format!("_taro-verify.{}", d.hostname), "value": format!("taro-verify={}", d.verification_token), "purpose": "proves you control the domain" },
            { "type": "CNAME", "name": d.hostname, "value": target, "purpose": "sends the domain's traffic to the portal (or A/AAAA records to the same addresses)" },
        ]
    })
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require(&state, org, Permission::DomainsManage).await?;
    let rows: Vec<Domain> = sqlx::query_as(&format!("SELECT {COLUMNS} FROM domain_assignments WHERE organization_id = $1 AND state <> 'disabled' ORDER BY created_at")).bind(org).fetch_all(&state.db).await?;
    let out: Vec<_> = rows
        .iter()
        .map(|d| json!({ "domain": d, "dns": instructions(&state, d) }))
        .collect();
    Ok(Json(json!({ "domains": out })))
}

#[derive(Deserialize)]
struct NewDomain {
    hostname: String,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Json(b): Json<NewDomain>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require(&state, org, Permission::DomainsManage).await?;
    let r = reserved(&state);
    let host = valid_hostname(
        &b.hostname,
        &r.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .map_err(ApiError::invalid)?;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM domain_assignments WHERE organization_id = $1 AND state <> 'disabled'").bind(org).fetch_one(&state.db).await?;
    if n >= 5 {
        return Err(ApiError::conflict("An organization has at most 5 domains."));
    }
    let id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    // a hostname taken by another tenant stays theirs; a disabled one can be claimed again
    sqlx::query("DELETE FROM domain_assignments WHERE hostname = $1 AND state = 'disabled'")
        .bind(&host)
        .execute(&mut *tx)
        .await?;
    let d: Domain = sqlx::query_as(&format!("INSERT INTO domain_assignments (id, organization_id, hostname, kind, verification_token, state, created_by) VALUES ($1, $2, $3, 'custom', $4, 'pending_verification', $5) RETURNING {COLUMNS}"))
        .bind(id)
        .bind(org)
        .bind(&host)
        .bind(crypto::token(24))
        .bind(auth.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match ApiError::from(e) {
            ApiError::Conflict(_) => ApiError::conflict("That hostname is already in use."),
            other => other,
        })?;
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "domain.added", "domain")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref())
            .details(json!({ "hostname": host })),
    )
    .await?;
    tx.commit().await?;
    let dns = instructions(&state, &d);
    Ok((
        StatusCode::CREATED,
        Json(json!({ "domain": d, "dns": dns })),
    ))
}

/// The TXT values published at `_taro-verify.<host>`.
async fn txt_values(host: &str) -> Result<Vec<String>, String> {
    let resolver = hickory_resolver::Resolver::builder_with_config(
        hickory_resolver::config::ResolverConfig::udp_and_tcp(
            &hickory_resolver::config::CLOUDFLARE,
        ),
        hickory_resolver::net::runtime::TokioRuntimeProvider::default(),
    )
    .build()
    .map_err(|e| format!("DNS resolver: {e}"))?;
    match resolver.txt_lookup(format!("_taro-verify.{host}.")).await {
        Ok(r) => Ok(r
            .answers()
            .iter()
            .filter_map(|rec| match &rec.data {
                hickory_resolver::proto::rr::RData::TXT(t) => Some(
                    t.txt_data
                        .iter()
                        .map(|b| String::from_utf8_lossy(b).to_string())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect()),
        Err(e) => Err(format!(
            "no TXT record found at _taro-verify.{host} ({})",
            e
        )),
    }
}

async fn verify(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require(&state, org, Permission::DomainsManage).await?;
    let d: Domain = sqlx::query_as(&format!("SELECT {COLUMNS} FROM domain_assignments WHERE id = $1 AND organization_id = $2 AND state <> 'disabled'")).bind(id).bind(org).fetch_one(&state.db).await?;
    if !state.limiter.allow(
        "domain_verify",
        &id.to_string(),
        20,
        std::time::Duration::from_secs(3600),
    ) {
        return Err(ApiError::RateLimited);
    }
    let want = format!("taro-verify={}", d.verification_token);
    let found = txt_values(&d.hostname).await;
    let ok = found
        .as_ref()
        .is_ok_and(|v| v.iter().any(|t| t.trim() == want));
    if ok {
        let mut tx = state.db.begin().await?;
        sqlx::query("UPDATE domain_assignments SET state = 'provisioning_tls', verified_at = now(), last_check_at = now(), last_error = NULL WHERE id = $1 AND state IN ('pending_verification', 'error')").bind(id).execute(&mut *tx).await?;
        audit::record(
            &mut *tx,
            Event::new(auth.actor(), "domain.verified", "domain")
                .org(org)
                .resource(id)
                .ip(auth.ip.as_deref())
                .details(json!({ "hostname": d.hostname })),
        )
        .await?;
        tx.commit().await?;
    } else {
        let why = match found {
            Ok(v) if v.is_empty() => format!("no TXT record at _taro-verify.{}", d.hostname),
            Ok(_) => "the TXT record does not have the expected value (DNS changes can take a while to appear)".to_string(),
            Err(e) => e,
        };
        sqlx::query(
            "UPDATE domain_assignments SET last_check_at = now(), last_error = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(&why)
        .execute(&state.db)
        .await?;
    }
    let d: Domain = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM domain_assignments WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "verified": ok, "domain": d })))
}

async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    auth.require(&state, org, Permission::DomainsManage).await?;
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE domain_assignments SET state = 'disabled' WHERE id = $1 AND organization_id = $2 AND state <> 'disabled'").bind(id).bind(org).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &mut *tx,
        Event::new(auth.actor(), "domain.removed", "domain")
            .org(org)
            .resource(id)
            .ip(auth.ip.as_deref()),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct Ask {
    domain: String,
}

/// Caddy's on-demand TLS question: may a certificate be obtained for this name? Loopback only.
pub async fn tls_ask(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Query(q): Query<Ask>,
) -> StatusCode {
    if !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN;
    }
    let host = q.domain.trim().trim_end_matches('.').to_lowercase();
    if reserved(&state).contains(&host) {
        return StatusCode::OK;
    }
    let ok: Result<(bool,), _> = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM domain_assignments WHERE hostname = $1 AND state IN ('provisioning_tls', 'active'))").bind(&host).fetch_one(&state.db).await;
    match ok {
        Ok((true,)) => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}

/// The hostnames the customer listener answers for (the portal's and the active custom ones).
pub async fn active_hosts(state: &AppState) -> Vec<String> {
    let mut v = reserved(state);
    if let Ok(rows) = sqlx::query_as::<_, (String,)>(
        "SELECT hostname FROM domain_assignments WHERE state IN ('provisioning_tls', 'active')",
    )
    .fetch_all(&state.db)
    .await
    {
        v.extend(rows.into_iter().map(|(h,)| h));
    }
    v
}

/// The certificate a host presents (valid for its name under the web PKI): when it expires.
pub async fn certificate_expiry(host: &str, port: u16) -> Result<DateTime<Utc>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| e.to_string())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name =
        rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| "no answer in 10 s".to_string())?
    .map_err(|e| e.to_string())?;
    let tls = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        connector.connect(name, tcp),
    )
    .await
    .map_err(|_| "TLS handshake timed out".to_string())?
    .map_err(|e| format!("TLS: {e}"))?;
    let (_, conn) = tls.get_ref();
    let cert = conn
        .peer_certificates()
        .and_then(|c| c.first())
        .ok_or("no certificate")?;
    let (_, x) = x509_parser::parse_x509_certificate(cert.as_ref()).map_err(|e| e.to_string())?;
    DateTime::from_timestamp(x.validity().not_after.timestamp(), 0)
        .ok_or_else(|| "bad expiry".into())
}

/// Every verified hostname: a valid certificate, `/healthz` answering over HTTPS, expiry noted.
pub async fn check_all(state: &AppState) -> anyhow::Result<()> {
    let rows: Vec<(Uuid, String, String, Option<Uuid>)> = sqlx::query_as("SELECT id, hostname, state, organization_id FROM domain_assignments WHERE state IN ('provisioning_tls', 'active', 'error') AND verified_at IS NOT NULL").fetch_all(&state.db).await?;
    for (id, host, st, org) in rows {
        let expiry = certificate_expiry(&host, 443).await;
        let health = match &expiry {
            Ok(_) => match state
                .http
                .get(format!("https://{host}/healthz"))
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => Ok(()),
                Ok(r) => Err(format!("/healthz answered {}", r.status())),
                Err(e) => Err(e.to_string()),
            },
            Err(e) => Err(e.clone()),
        };
        match (health, expiry) {
            (Ok(()), Ok(exp)) => {
                sqlx::query("UPDATE domain_assignments SET state = 'active', tls_checked_at = now(), tls_expires_at = $2, last_check_at = now(), last_error = NULL WHERE id = $1").bind(id).bind(exp).execute(&state.db).await?;
                if exp - Utc::now() < chrono::Duration::days(14) {
                    crate::incidents::open(
                        state,
                        crate::incidents::New {
                            severity: "sev3",
                            component: "tls",
                            title: format!(
                                "The certificate for {host} expires {}",
                                exp.format("%Y-%m-%d")
                            ),
                            impact:
                                "Renewal may be failing; the domain stops working when it expires."
                                    .into(),
                            dedupe_key: Some(format!("tls-expiry:{id}")),
                            linked: json!({ "domain_id": id, "organization_id": org }),
                        },
                    )
                    .await?;
                }
            }
            (Err(e), _) | (Ok(()), Err(e)) => {
                // a domain still waiting for its first certificate is not an outage yet
                let next = if st == "provisioning_tls" {
                    "provisioning_tls"
                } else {
                    "error"
                };
                sqlx::query("UPDATE domain_assignments SET state = $2, last_check_at = now(), last_error = $3 WHERE id = $1").bind(id).bind(next).bind(&e).execute(&state.db).await?;
                if next == "error" {
                    crate::incidents::open(
                        state,
                        crate::incidents::New {
                            severity: "sev3",
                            component: "tls",
                            title: format!("{host} fails its HTTPS check"),
                            impact: e.clone(),
                            dedupe_key: Some(format!("tls:{id}")),
                            linked: json!({ "domain_id": id, "organization_id": org }),
                        },
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

pub async fn run(state: AppState) {
    loop {
        if let Err(e) = check_all(&state).await {
            tracing::error!(error = %e, "domain checks");
        }
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/organizations/{org}/domains",
            get(list).post(create),
        )
        .route(
            "/api/v1/organizations/{org}/domains/{id}/verify",
            post(verify),
        )
        .route(
            "/api/v1/organizations/{org}/domains/{id}",
            axum::routing::delete(remove),
        )
}

#[cfg(test)]
mod tests {
    use super::valid_hostname;

    #[test]
    fn hostnames() {
        let r = ["portal.taro.example", "ops.taro.example"];
        assert_eq!(
            valid_hostname("Portal.Customer.EXAMPLE.", &r).unwrap(),
            "portal.customer.example"
        );
        for bad in [
            "localhost",
            "10.0.0.1",
            "a..b.com",
            "-a.b.com",
            "a_b.example.com",
            "x.portal.taro.example",
            "portal.taro.example",
            "1.2.3.4",
            "a.b.123",
        ] {
            assert!(valid_hostname(bad, &r).is_err(), "{bad}");
        }
    }
}
