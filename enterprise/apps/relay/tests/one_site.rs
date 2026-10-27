//! Updating a relay set up before 0.2 installs the portal and the console: turned on, folders
//! and secrets made, units written again; then the relay's own PostgreSQL starts, the API runs
//! and the console's first visit makes the administrator with the setup code.
//!
//! The PostgreSQL programs: TARO_PG_TEST_BIN (default /usr/lib/postgresql/16/bin, the
//! distribution's; the release bundles its own in core/pg). Running as root, PostgreSQL runs as
//! the `postgres` user. Skipped when neither is available.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use taro_relay::config::{Runtime, TlsMode};
use taro_relay::system::Fake;
use taro_update::policy::Layout;
use tower::ServiceExt;

fn provider() -> rm_brand::Provider {
    rm_brand::Provider {
        schema: rm_brand::SCHEMA.into(),
        provider_id: "acme-cloud".into(),
        product: rm_brand::Product {
            name: "Acme Connect".into(),
            short_name: "Acme".into(),
            vendor: "Acme Cloud Ltd".into(),
            support_url: None,
            support_email: None,
        },
        branding: rm_brand::Branding {
            primary_color: "#0F766E".into(),
            tokens: None,
            theme: rm_brand::Theme::System,
            logo: Some("logo.png".into()),
            icon: Some("icon.png".into()),
        },
        relay: rm_brand::Relay {
            host: "relay.acme.example".into(),
            port: 7470,
            key: "acme-admission-key-0123456789".into(),
        },
        account: None,
        updates: Default::default(),
        generated: None,
    }
    .normalized()
}

/// A PNG of that size, as far as the upload checks read it (signature and IHDR).
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

fn pg_bin() -> Option<PathBuf> {
    let p = PathBuf::from(
        std::env::var("TARO_PG_TEST_BIN").unwrap_or_else(|_| "/usr/lib/postgresql/16/bin".into()),
    );
    p.join("postgres").exists().then_some(p)
}

/// The uid PostgreSQL runs as when this test runs as root.
fn postgres_uid() -> Option<u32> {
    let out = std::process::Command::new("id")
        .args(["-u", "postgres"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn chown_r(p: &Path, uid: u32) {
    let _ = std::process::Command::new("chown")
        .args(["-R", &uid.to_string()])
        .arg(p)
        .status();
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "relay.acme.example");
    if method != "GET" {
        req = req.header(header::ORIGIN, "https://relay.acme.example");
    }
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let mut req = req.body(body).unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo::<std::net::SocketAddr>(
            "127.0.0.1:40000".parse().unwrap(),
        ));
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updating_an_old_relay_brings_the_portal_and_the_console() {
    let Some(bin) = pg_bin() else {
        eprintln!("skipped: no PostgreSQL programs (TARO_PG_TEST_BIN)");
        return;
    };
    let root = unsafe { libc::geteuid() } == 0;
    let uid = if root {
        match postgres_uid() {
            Some(u) => Some(u),
            None => {
                eprintln!("skipped: running as root without a postgres user");
                return;
            }
        }
    } else {
        None
    };
    let base = std::env::temp_dir().join(format!("taro-one-site-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let l = Layout::under(&base);
    std::fs::create_dir_all(&l.provider).unwrap();
    std::fs::write(l.provider.join("provider.json"), provider().to_json()).unwrap();
    for (f, w, h) in [("logo.png", 64, 32), ("icon.png", 32, 32)] {
        std::fs::write(l.provider.join(f), png(w, h)).unwrap();
    }
    // a relay set up with 0.1: no [portal] in its settings, its units without the portal's folders
    let mut old = Runtime::defaults(&l, "relay.acme.example", 7470);
    old.portal = None;
    old.tls.mode = TlsMode::Files;
    old.save(&l, "0").unwrap();
    let units = base.join("units");
    std::fs::create_dir_all(&units).unwrap();
    std::fs::write(
        units.join("taro-relay.service"),
        "ReadWritePaths=/var/lib/taro/relay /var/lib/taro/tls\n",
    )
    .unwrap();
    let sys = Fake::new(units.clone());

    // the update's last step before the new core starts
    taro_relay::update::before_restart(&l, &sys).unwrap();
    let r = Runtime::load(&l).unwrap().unwrap();
    let p = r.portal.clone().expect("turned on by the update");
    assert!(p.enabled && p.database == "embedded");
    let code = std::fs::read_to_string(taro_relay::portal::code_file(&l)).unwrap();
    assert_eq!(code.trim().len(), 14, "a setup code");
    assert_eq!(
        std::fs::read_to_string(taro_relay::portal::secret_file(&l))
            .unwrap()
            .trim()
            .len(),
        64
    );
    let unit = std::fs::read_to_string(units.join("taro-relay.service")).unwrap();
    assert!(
        unit.contains("/portal") && unit.contains("/pg") && unit.contains("Acme Connect"),
        "{unit}"
    );
    // run again: nothing changes, the code stays
    taro_relay::update::before_restart(&l, &sys).unwrap();
    assert_eq!(
        std::fs::read_to_string(taro_relay::portal::code_file(&l)).unwrap(),
        code
    );

    // the relay's own database and the site
    std::env::set_var("TARO_PG_BIN", &bin);
    if let Some(u) = uid {
        std::env::set_var("TARO_PG_UID", u.to_string());
        chown_r(&l.state, u);
        // the setup code file is removed by the API (as the service's user would)
    }
    let mut pg = None;
    let site = taro_relay::portal::start_site(&l, &r, &provider(), &p, &mut pg)
        .await
        .unwrap_or_else(|e| panic!("the site did not start: {e}"));
    assert!(pg.is_some(), "the embedded PostgreSQL runs");

    let (s, v, t) = call(&site, "GET", "/console/api/v1/setup/status", None).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(v["needs_admin"], true);
    // the provider's look, from its folder, until an administrator publishes another
    let (_, v, t) = call(&site, "GET", "/api/v1/branding", None).await;
    assert_eq!(v["branding"]["displayName"], "Acme Connect", "{t}");
    assert_eq!(v["branding"]["primaryColor"], "#0F766E", "{t}");
    for k in ["logoAssetId", "faviconAssetId"] {
        let id = v["branding"][k]
            .as_str()
            .unwrap_or_else(|| panic!("{k}: {t}"));
        let (s, _, _) = call(&site, "GET", &format!("/api/v1/assets/{id}"), None).await;
        assert_eq!(s, StatusCode::OK, "{k}");
    }
    let (s, _, html) = call(&site, "GET", "/console/", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("<base href=\"/console/\">"), "{html}");
    let (s, _, html) = call(&site, "GET", "/", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("<html"), "the portal");
    let (s, _, t) = call(&site, "POST", "/console/api/v1/setup/admin", Some(json!({
        "setup_code": code.trim(), "email": "owner@acme.example", "name": "Owner", "password": "a long enough password" }))).await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert!(
        !taro_relay::portal::code_file(&l).exists(),
        "the code has done its work"
    );
    // the Macs' endpoint is there (a wrong code is refused)
    let (s, _, _) = call(
        &site,
        "POST",
        "/api/v1/agent/enroll",
        Some(json!({ "code": "XXXX-XXXX-XXXX", "name": "Mac" })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // the database stops with the relay, and starts again with its data
    drop(site);
    drop(pg.take());
    let mut pg = None;
    let site = taro_relay::portal::start_site(&l, &r, &provider(), &p, &mut pg)
        .await
        .unwrap();
    let (_, v, _) = call(&site, "GET", "/console/api/v1/setup/status", None).await;
    assert_eq!(v["needs_admin"], false, "the administrator is still there");
    drop(pg);
    let _ = std::fs::remove_dir_all(&base);
}
