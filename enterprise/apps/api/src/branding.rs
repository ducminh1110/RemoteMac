//! Branding: versioned, validated configurations (name, colour, theme, density, modules,
//! support contact, logo and favicon). A draft is published to take effect; publishing an
//! older version is the rollback. Nothing in it can carry markup, CSS or script: colours are
//! `#RRGGBB`, images are checked PNG, JPEG or WebP, text is plain.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
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

pub const MODULES: &[&str] = &["my_macs", "rentals", "billing", "team"];
const MAX_IMAGE: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    pub display_name: String,
    pub primary_color: String,
    pub theme_mode: String,
    pub density: String,
    pub enabled_modules: Vec<String>,
    pub support_contact: String,
    pub logo_asset_id: Option<Uuid>,
    pub favicon_asset_id: Option<Uuid>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            display_name: "Taro Enterprise".into(),
            primary_color: "#6D5AE6".into(),
            theme_mode: "light".into(),
            density: "comfortable".into(),
            enabled_modules: MODULES.iter().map(|m| m.to_string()).collect(),
            support_contact: String::new(),
            logo_asset_id: None,
            favicon_asset_id: None,
        }
    }
}

fn hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// WCAG relative luminance contrast of a colour against white.
pub fn contrast_with_white(rgb: (u8, u8, u8)) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(rgb.0) + 0.7152 * lin(rgb.1) + 0.0722 * lin(rgb.2);
    1.05 / (l + 0.05)
}

pub fn validate(c: &Config) -> Result<Vec<String>, String> {
    let mut warnings = vec![];
    let name = c.display_name.trim();
    if name.is_empty()
        || name.chars().count() > 60
        || name.chars().any(|ch| ch.is_control() || "<>".contains(ch))
    {
        return Err("The display name is 1 to 60 characters, without < or >.".into());
    }
    let rgb = hex_color(&c.primary_color).ok_or("The primary colour is #RRGGBB.")?;
    if contrast_with_white(rgb) < 4.5 {
        warnings.push(format!("{} has a contrast of {:.1}:1 against white; text in it may be hard to read (4.5:1 is the minimum for text).", c.primary_color, contrast_with_white(rgb)));
    }
    if !["light", "dark", "system"].contains(&c.theme_mode.as_str()) {
        return Err("The theme is light, dark or system.".into());
    }
    if !["comfortable", "compact"].contains(&c.density.as_str()) {
        return Err("The density is comfortable or compact.".into());
    }
    if c.enabled_modules
        .iter()
        .any(|m| !MODULES.contains(&m.as_str()))
    {
        return Err(format!("Modules are some of {}.", MODULES.join(", ")));
    }
    let s = c.support_contact.trim();
    if !s.is_empty()
        && !(crate::identity::valid_email(s)
            || (s.starts_with("https://") && url::Url::parse(s).is_ok()))
        || s.len() > 200
    {
        return Err("The support contact is an email address or an https:// link.".into());
    }
    Ok(warnings)
}

/// What an image really is (by its bytes), and its size.
pub fn sniff(b: &[u8]) -> Option<(&'static str, u32, u32)> {
    if b.len() > 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") && &b[12..16] == b"IHDR" {
        let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
        return Some(("image/png", w, h));
    }
    if b.len() > 4 && b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // walk the segments to a start-of-frame
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                return None;
            }
            let marker = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some(("image/jpeg", w, h));
            }
            i += 2 + len;
        }
        return None;
    }
    if b.len() > 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8X" => Some((
                "image/webp",
                1 + u32::from_le_bytes([b[24], b[25], b[26], 0]),
                1 + u32::from_le_bytes([b[27], b[28], b[29], 0]),
            )),
            b"VP8 " => Some((
                "image/webp",
                (u16::from_le_bytes([b[26], b[27]]) & 0x3FFF) as u32,
                (u16::from_le_bytes([b[28], b[29]]) & 0x3FFF) as u32,
            )),
            b"VP8L" => {
                let v = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
                Some(("image/webp", (v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
            }
            _ => None,
        };
    }
    None
}

#[derive(Serialize, sqlx::FromRow)]
struct Version {
    id: Uuid,
    version: i32,
    config: serde_json::Value,
    published_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

/// The published configuration in force for an organization (or the platform's, when it has
/// none or `org` is None).
pub async fn current(db: &sqlx::PgPool, org: Option<Uuid>) -> anyhow::Result<Config> {
    // the organization's own, else the platform's, else the built-in default
    let scopes = if org.is_some() {
        vec![org, None]
    } else {
        vec![None]
    };
    for scope in scopes {
        let row: Option<(serde_json::Value,)> = sqlx::query_as("SELECT config FROM branding_configs WHERE organization_id IS NOT DISTINCT FROM $1 AND published_at IS NOT NULL ORDER BY published_at DESC LIMIT 1").bind(scope).fetch_optional(db).await?;
        if let Some(c) = row.and_then(|(c,)| serde_json::from_value(c).ok()) {
            return Ok(c);
        }
    }
    Ok(Config::default())
}

/// `GET /api/v1/branding`: the look for this hostname (a custom domain's organization, or the
/// platform's), for the portal to theme itself before sign-in.
async fn public(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let c = for_host(&state, &headers).await?;
    Ok(Json(json!({ "branding": c })))
}

/// The look for the request's hostname: a custom domain's organization, or the provider's.
async fn for_host(state: &AppState, headers: &HeaderMap) -> ApiResult<Config> {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_lowercase())
        .unwrap_or_default();
    let org: Option<(Uuid,)> = sqlx::query_as("SELECT organization_id FROM domain_assignments WHERE hostname = $1 AND state = 'active' AND organization_id IS NOT NULL").bind(&host).fetch_optional(&state.db).await?;
    Ok(current(&state.db, org.map(|o| o.0)).await?)
}

/// `GET /brand/tokens.css`: the console's colour tokens derived (by rm-brand, as for the relay
/// and the clients) from the published primary colour. Loaded after console.css.
pub async fn tokens_css(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let primary = for_host(&state, &headers)
        .await
        .ok()
        .and_then(|c| rm_brand::Color::parse(&c.primary_color).ok())
        .unwrap_or(rm_brand::TARO_PRIMARY);
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        rm_brand::Tokens::derive(primary).css(),
    )
        .into_response()
}

async fn versions(state: &AppState, org: Option<Uuid>) -> ApiResult<Json<serde_json::Value>> {
    let rows: Vec<Version> = sqlx::query_as("SELECT id, version, config, published_at, created_at FROM branding_configs WHERE organization_id IS NOT DISTINCT FROM $1 ORDER BY version DESC LIMIT 50").bind(org).fetch_all(&state.db).await?;
    let live = rows
        .iter()
        .filter(|v| v.published_at.is_some())
        .max_by_key(|v| v.published_at)
        .map(|v| v.version);
    Ok(Json(
        json!({ "versions": rows, "published_version": live, "current": current(&state.db, org).await?, "modules": MODULES }),
    ))
}

async fn create_version(
    state: &AppState,
    auth: &Auth,
    org: Option<Uuid>,
    c: Config,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let warnings = validate(&c).map_err(ApiError::invalid)?;
    for asset in [c.logo_asset_id, c.favicon_asset_id].into_iter().flatten() {
        let (ok,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1 AND organization_id IS NOT DISTINCT FROM $2)").bind(asset).bind(org).fetch_one(&state.db).await?;
        if !ok {
            return Err(ApiError::invalid(
                "An image in the configuration was not uploaded here.",
            ));
        }
    }
    if let Some(org) = org {
        // only modules the organization is entitled to
        let ent: Vec<(String,)> =
            sqlx::query_as("SELECT feature FROM entitlements WHERE organization_id = $1")
                .bind(org)
                .fetch_all(&state.db)
                .await?;
        if let Some(m) = c
            .enabled_modules
            .iter()
            .find(|m| !ent.iter().any(|(f,)| f == *m))
        {
            return Err(ApiError::invalid(format!(
                "This organization is not entitled to the {m} module."
            )));
        }
    }
    let mut tx = state.db.begin().await?;
    let (next,): (i32,) = sqlx::query_as("SELECT coalesce(max(version), 0) + 1 FROM branding_configs WHERE organization_id IS NOT DISTINCT FROM $1").bind(org).fetch_one(&mut *tx).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO branding_configs (id, organization_id, version, config, created_by) VALUES ($1, $2, $3, $4, $5)").bind(id).bind(org).bind(next).bind(serde_json::to_value(&c).map_err(|e| ApiError::Internal(e.into()))?).bind(auth.user_id).execute(&mut *tx).await?;
    let mut e = Event::new(auth.actor(), "branding.version_created", "branding")
        .resource(next)
        .ip(auth.ip.as_deref());
    if let Some(o) = org {
        e = e.org(o);
    }
    audit::record(&mut *tx, e).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "version": next, "warnings": warnings })),
    ))
}

async fn publish(
    state: &AppState,
    auth: &Auth,
    org: Option<Uuid>,
    version: i32,
) -> ApiResult<StatusCode> {
    let mut tx = state.db.begin().await?;
    let r = sqlx::query("UPDATE branding_configs SET published_at = now() WHERE organization_id IS NOT DISTINCT FROM $1 AND version = $2").bind(org).bind(version).execute(&mut *tx).await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    let mut e = Event::new(auth.actor(), "branding.published", "branding")
        .resource(version)
        .ip(auth.ip.as_deref());
    if let Some(o) = org {
        e = e.org(o);
    }
    audit::record(&mut *tx, e).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AssetKind {
    kind: String,
}

async fn upload(
    state: &AppState,
    auth: &Auth,
    org: Option<Uuid>,
    kind: &str,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if !["logo", "favicon"].contains(&kind) {
        return Err(ApiError::invalid("kind is logo or favicon"));
    }
    if body.len() > MAX_IMAGE {
        return Err(ApiError::invalid("Images are at most 1 MB."));
    }
    let (ct, w, h) = sniff(&body).ok_or_else(|| {
        ApiError::invalid(
            "Upload a PNG, JPEG or WebP image (SVG and other formats are not accepted).",
        )
    })?;
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        return Err(ApiError::invalid("Images are at most 1024×1024 pixels."));
    }
    if kind == "favicon" && (w != h || w < 16) {
        return Err(ApiError::invalid(
            "A favicon is square, 16×16 pixels or more.",
        ));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO assets (id, organization_id, kind, content_type, bytes, sha256, width, height, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)")
        .bind(id)
        .bind(org)
        .bind(kind)
        .bind(ct)
        .bind(body.as_ref())
        .bind(crate::crypto::sha256_hex(&body))
        .bind(w as i32)
        .bind(h as i32)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": id, "content_type": ct, "width": w, "height": h })),
    ))
}

/// An uploaded image, as an image and nothing else.
async fn asset(State(state): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Response> {
    let (ct, bytes, sha): (String, Vec<u8>, String) =
        sqlx::query_as("SELECT content_type, bytes, sha256 FROM assets WHERE id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    Ok((
        [
            (header::CONTENT_TYPE, ct),
            (
                header::CACHE_CONTROL,
                "public, max-age=86400, immutable".into(),
            ),
            (header::ETAG, format!("\"{sha}\"")),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; sandbox".into(),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        bytes,
    )
        .into_response())
}

async fn org_versions(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require(&state, org, Permission::BrandingManage)
        .await?;
    versions(&state, Some(org)).await
}
async fn org_create(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Json(c): Json<Config>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require(&state, org, Permission::BrandingManage)
        .await?;
    create_version(&state, &auth, Some(org), c).await
}
async fn org_publish(
    State(state): State<AppState>,
    auth: Auth,
    Path((org, v)): Path<(Uuid, i32)>,
) -> ApiResult<StatusCode> {
    auth.require(&state, org, Permission::BrandingManage)
        .await?;
    publish(&state, &auth, Some(org), v).await
}
async fn org_upload(
    State(state): State<AppState>,
    auth: Auth,
    Path(org): Path<Uuid>,
    Query(k): Query<AssetKind>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require(&state, org, Permission::BrandingManage)
        .await?;
    upload(&state, &auth, Some(org), &k.kind, body).await
}

async fn ops_versions(
    State(state): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<serde_json::Value>> {
    auth.require_op(&state, Permission::OpsConfigManage).await?;
    versions(&state, None).await
}
async fn ops_create(
    State(state): State<AppState>,
    auth: Auth,
    Json(c): Json<Config>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require_op(&state, Permission::OpsConfigManage).await?;
    create_version(&state, &auth, None, c).await
}
async fn ops_publish(
    State(state): State<AppState>,
    auth: Auth,
    Path(v): Path<i32>,
) -> ApiResult<StatusCode> {
    auth.require_op(&state, Permission::OpsConfigManage).await?;
    publish(&state, &auth, None, v).await
}
async fn ops_upload(
    State(state): State<AppState>,
    auth: Auth,
    Query(k): Query<AssetKind>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    auth.require_op(&state, Permission::OpsConfigManage).await?;
    upload(&state, &auth, None, &k.kind, body).await
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/branding", get(public))
        .route("/api/v1/assets/{id}", get(asset))
        .route(
            "/api/v1/organizations/{org}/branding",
            get(org_versions).post(org_create),
        )
        .route(
            "/api/v1/organizations/{org}/branding/{version}/publish",
            post(org_publish),
        )
        .route("/api/v1/organizations/{org}/assets", post(org_upload))
}

pub fn ops_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/ops/branding", get(ops_versions).post(ops_create))
        .route("/api/v1/ops/branding/{version}/publish", post(ops_publish))
        .route("/api/v1/ops/assets", post(ops_upload))
        .route("/api/v1/branding", get(public))
        .route("/api/v1/assets/{id}", get(asset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configurations_are_checked() {
        assert!(validate(&Config::default()).unwrap().is_empty());
        let warn = validate(&Config {
            primary_color: "#FFD60A".into(),
            ..Config::default()
        })
        .unwrap();
        assert_eq!(warn.len(), 1, "a light yellow is low contrast");
        for bad in [
            Config {
                primary_color: "red".into(),
                ..Config::default()
            },
            Config {
                primary_color: "#12345".into(),
                ..Config::default()
            },
            Config {
                display_name: "<script>".into(),
                ..Config::default()
            },
            Config {
                theme_mode: "neon".into(),
                ..Config::default()
            },
            Config {
                enabled_modules: vec!["shell".into()],
                ..Config::default()
            },
            Config {
                support_contact: "javascript:alert(1)".into(),
                ..Config::default()
            },
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
        // unknown keys (a "css" field, say) are refused when parsed
        assert!(serde_json::from_value::<Config>(json!({ "displayName": "x", "primaryColor": "#000000", "themeMode": "light", "density": "compact", "enabledModules": [], "supportContact": "", "logoAssetId": null, "faviconAssetId": null, "customCss": "body{}" })).is_err());
        assert!((contrast_with_white((0, 0, 0)) - 21.0).abs() < 0.01);
    }

    #[test]
    fn images_by_their_bytes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&64u32.to_be_bytes());
        png.extend_from_slice(&32u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(sniff(&png), Some(("image/png", 64, 32)));
        assert_eq!(
            sniff(b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>"),
            None
        );
        assert_eq!(sniff(b"GIF89a................................"), None);
        // a JPEG: SOI, an APP0 segment, then SOF0 with 48x40
        let jpg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00,
            0x28, 0x00, 0x30, 0x03, 0x00, 0x00,
        ];
        assert_eq!(sniff(&jpg), Some(("image/jpeg", 48, 40)));
    }
}
