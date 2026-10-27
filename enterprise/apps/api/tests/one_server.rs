//! One server: the console's first visit makes the administrator (with the setup code from the
//! server), administrators sign in with a password, and the portal and the console share one
//! origin (the console under /console).

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::*;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use taro_enterprise::config::Secret;
use tower::ServiceExt;

async fn me(w: &World, c: &mut Client) -> Value {
    let r = w.call(c, Method::GET, "/api/v1/me", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    c.csrf = r.json["csrf_token"].as_str().map(String::from);
    r.json
}

#[tokio::test]
async fn the_first_visit_makes_the_administrator_who_signs_in_with_a_password() {
    let Some(w) = world_with(|c| {
        c.bootstrap_admin_email = None;
        c.setup_code = Some(Secret::new("ABCD-EFGH-JKLM"));
    })
    .await
    else {
        return;
    };
    let mut ops = w.console();
    let r = w
        .call(&mut ops, Method::GET, "/api/v1/setup/status", None)
        .await;
    assert_eq!(r.json["needs_admin"], true, "{}", r.text);

    let admin = |code: &str, password: &str| json!({ "setup_code": code, "email": " Owner@Acme.TEST ", "name": "Ana Owner", "password": password });
    // whoever comes first without the code from the server gets nothing
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/setup/admin",
            Some(admin("WRONG-CODE-0000", "a long enough password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert!(r.text.contains("setup code"), "{}", r.text);
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/setup/admin",
            Some(admin("ABCD-EFGH-JKLM", "short")),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // the code as typed by a person: lower case, no dashes
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/setup/admin",
            Some(admin("abcd efgh jklm", "a long enough password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(ops.cookie.is_some(), "signed in at once");
    let m = me(&w, &mut ops).await;
    assert_eq!(m["email"], "owner@acme.test");
    assert!(
        m["operator_roles"].to_string().contains("platform_admin"),
        "{m}"
    );

    // the form is closed now
    let mut other = w.console();
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/setup/admin",
            Some(admin("ABCD-EFGH-JKLM", "another long password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = w
        .call(&mut other, Method::GET, "/api/v1/setup/status", None)
        .await;
    assert_eq!(r.json["needs_admin"], false);

    // the password: one answer for a wrong address and a wrong password
    let login = |email: &str, password: &str| json!({ "email": email, "password": password });
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("owner@acme.test", "not it at all")),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let wrong = r.json["error"]["message"].clone();
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("nobody@acme.test", "a long enough password")),
        )
        .await;
    assert_eq!(r.json["error"]["message"], wrong);
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("OWNER@acme.test", "a long enough password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    me(&w, &mut other).await;

    // changing it: the current one first; other sessions end
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/auth/password/change",
            Some(json!({ "current": "nope", "new": "the new long password" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .call(
            &mut other,
            Method::POST,
            "/api/v1/auth/password/change",
            Some(json!({ "current": "a long enough password", "new": "the new long password" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = w.call(&mut ops, Method::GET, "/api/v1/me", None).await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "the first session ended"
    );
    let mut third = w.console();
    let r = w
        .call(
            &mut third,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("owner@acme.test", "a long enough password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .call(
            &mut third,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("owner@acme.test", "the new long password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);

    // the portal has no password sign-in (customers use email or Google)
    let mut cust = w.browser();
    let r = w
        .call(
            &mut cust,
            Method::POST,
            "/api/v1/auth/password",
            Some(login("owner@acme.test", "the new long password")),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    w.drop_db().await;
}

async fn get(app: &axum::Router, path: &str) -> (StatusCode, String) {
    let mut req = Request::builder()
        .uri(path)
        .header(header::HOST, "portal.test")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo::<std::net::SocketAddr>(
            "127.0.0.1:40000".parse().unwrap(),
        ));
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn the_portal_and_the_console_share_one_origin() {
    let Some(w) = world_with(|c| {
        c.ops_url = url::Url::parse("http://portal.test/console").unwrap();
    })
    .await
    else {
        return;
    };
    let app = taro_enterprise::http::single(&w.state);
    for page in [
        "/console",
        "/console/",
        "/console/macs",
        "/console/organizations/123",
    ] {
        let (s, html) = get(&app, page).await;
        assert_eq!(s, StatusCode::OK, "{page}");
        assert!(html.contains("<base href=\"/console/\">"), "{page}: {html}");
    }
    let (s, body) = get(&app, "/console/api/v1/setup/status").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(body.contains("needs_admin"), "{body}");
    let (s, body) = get(&app, "/console/app.js").await;
    assert_eq!(s, StatusCode::OK);
    assert!(!body.contains("<base"), "a file is itself");
    // the portal: its own page and API at the root
    let (s, html) = get(&app, "/").await;
    assert_eq!(s, StatusCode::OK);
    assert!(!html.contains("<base"), "{html}");
    let (s, body) = get(&app, "/api/v1/auth/providers").await;
    assert_eq!(s, StatusCode::OK, "{body}");
    // the console's API is not the portal's
    let (s, _) = get(&app, "/api/v1/setup/status").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    w.drop_db().await;
}
