//! What the relay's two web front ends share (the setup on 127.0.0.1, the status page over
//! HTTPS): strict security headers, the core pages, and the provider's branding.
//!
//! The pages are shared core, served from the installed `/opt/taro/core/web/relay/` (so
//! `enterprise-update.zip` updates them) with copies built into the binary as a fallback. The
//! provider's look comes from the provider folder only: `/brand/tokens.css` is generated from
//! provider.json's primary colour, `/brand/logo.png` and `/brand/icon.png` are its files, and
//! `/brand/theme.css` is the provider's own optional stylesheet, loaded last. Core pages never
//! contain a provider's colour or name; an update of them cannot reset the branding.

use axum::body::Body;
use axum::http::{header, HeaderValue, Response, StatusCode};
use taro_update::policy::Layout;

pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The core web files built in (fallback when the installed copy is missing).
pub fn embedded(rel: &str) -> Option<&'static str> {
    Some(match rel {
        "setup/index.html" => include_str!("../web/setup/index.html"),
        "setup/app.css" => include_str!("../web/setup/app.css"),
        "setup/app.js" => include_str!("../web/setup/app.js"),
        "status/index.html" => include_str!("../web/status/index.html"),
        "status/app.css" => include_str!("../web/status/app.css"),
        "status/app.js" => include_str!("../web/status/app.js"),
        "common/console.css" => include_str!("../../../packages/console/console.css"),
        "common/console.js" => include_str!("../../../packages/console/console.js"),
        _ => return None,
    })
}

fn content_type(rel: &str) -> &'static str {
    match rel.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub fn secure(mut r: Response<Body>) -> Response<Body> {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

pub fn bytes(status: StatusCode, ctype: &str, body: impl Into<Body>) -> Response<Body> {
    secure(
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, ctype)
            .body(body.into())
            .unwrap(),
    )
}

pub fn not_found() -> Response<Body> {
    bytes(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        "not found",
    )
}

/// A core page or asset: `setup/app.js`, `status/index.html`.
pub fn core_asset(layout: &Layout, rel: &str) -> Response<Body> {
    if taro_update::policy::check_relative(rel).is_err() {
        return not_found();
    }
    let installed = layout.core().join("web/relay").join(rel);
    if let Ok(b) = std::fs::read(&installed) {
        return bytes(StatusCode::OK, content_type(rel), b);
    }
    match embedded(rel) {
        Some(s) => bytes(StatusCode::OK, content_type(rel), s),
        None => not_found(),
    }
}

/// The provider, if its folder holds a valid provider.json.
pub fn provider(layout: &Layout) -> Result<Option<rm_brand::Provider>, String> {
    rm_brand::Provider::load(&layout.provider)
}

/// `/brand/<file>`: tokens.css from provider.json, or a file of the provider folder.
pub fn brand(layout: &Layout, file: &str) -> Response<Body> {
    let p = provider(layout).ok().flatten();
    match file {
        "tokens.css" => {
            let tokens = p
                .as_ref()
                .map(|p| p.tokens())
                .unwrap_or_else(|| rm_brand::Tokens::derive(rm_brand::TARO_PRIMARY));
            bytes(StatusCode::OK, "text/css; charset=utf-8", tokens.css())
        }
        "theme.css" => match std::fs::read(layout.provider.join("theme.css")) {
            Ok(b) => bytes(StatusCode::OK, "text/css; charset=utf-8", b),
            // no override: an empty stylesheet, not an error
            Err(_) => bytes(StatusCode::OK, "text/css; charset=utf-8", ""),
        },
        "logo.png" | "icon.png" => {
            let named = p.as_ref().and_then(|p| {
                if file == "logo.png" {
                    p.asset(&layout.provider, &p.branding.logo)
                } else {
                    p.asset(&layout.provider, &p.branding.icon)
                }
            });
            match named.and_then(|f| std::fs::read(f).ok()) {
                Some(b) if b.starts_with(b"\x89PNG") => bytes(StatusCode::OK, "image/png", b),
                _ => bytes(StatusCode::OK, "image/svg+xml", DEFAULT_MARK),
            }
        }
        _ => not_found(),
    }
}

/// Without a provider logo: a neutral rounded square in the primary colour.
const DEFAULT_MARK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#7C4AE8"/><rect x="8" y="10" width="16" height="11" rx="2" fill="#fff"/></svg>"##;

/// The provider's public identity (never the key).
pub fn public_identity(p: &rm_brand::Provider) -> serde_json::Value {
    serde_json::json!({
        "provider_id": p.provider_id,
        "product": p.product.name,
        "short_name": p.product.short_name,
        "vendor": p.product.vendor,
        "support_url": p.product.support_url,
        "support_email": p.product.support_email,
        "relay_host": p.relay.host,
        "relay_port": p.relay.port,
        "primary_color": p.branding.primary_color,
        "has_logo": p.branding.logo.is_some(),
    })
}

/// A random hex token.
pub fn token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

pub fn same(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
