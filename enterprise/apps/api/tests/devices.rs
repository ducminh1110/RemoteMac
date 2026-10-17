//! Signing in to the Taro app (device authorization grant) and its "My Macs" list, end to end
//! through the real routers and a real Postgres.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};

macro_rules! world {
    () => {
        match world().await {
            Some(w) => w,
            None => return,
        }
    };
}

fn app() -> Client {
    Client {
        cookie: None,
        csrf: None,
        origin: "",
        ops: false,
    }
}

async fn post_app(w: &World, path: &str, body: Value, bearer: Option<&str>) -> Res {
    let auth = bearer.map(|t| format!("Bearer {t}"));
    let extra: Vec<(&str, &str)> = auth.iter().map(|a| ("authorization", a.as_str())).collect();
    w.raw(
        &mut app(),
        Method::POST,
        path,
        Some(("application/json", body.to_string().into_bytes())),
        &extra,
    )
    .await
}

async fn get_app(w: &World, path: &str, bearer: &str) -> Res {
    let auth = format!("Bearer {bearer}");
    w.raw(
        &mut app(),
        Method::GET,
        path,
        None,
        &[("authorization", auth.as_str())],
    )
    .await
}

/// A ready rental of `org`, assigned to its creator.
async fn ready_rental(w: &World, c: &mut Client, org: &str) -> String {
    let r = w
        .call(
            c,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "devices-0001" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rental = r.json["id"].as_str().unwrap().to_string();
    let r = w
        .call(
            c,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/checkout"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = w
        .stripe_event("checkout.session.completed", w.completed_for(&rental), true)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "ready");
    rental
}

/// The app asks for a code and the user approves it; the app gets its token.
async fn sign_in_app(w: &World, user: &mut Client) -> (String, String) {
    let r = post_app(
        w,
        "/api/v1/devices/authorize",
        json!({ "client": "Taro 1.4.1 on Windows" }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let device = r.json["device_code"].as_str().unwrap().to_string();
    let code = r.json["user_code"].as_str().unwrap().to_string();
    let r = w
        .call(
            user,
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = post_app(
        w,
        "/api/v1/devices/token",
        json!({ "device_code": device }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    (r.json["access_token"].as_str().unwrap().to_string(), code)
}

#[tokio::test]
async fn the_app_signs_in_lists_its_macs_and_connects() {
    let w = world!();
    let mut owner = w.user("owner@acme.test").await;
    let org = w.org(&mut owner, "Acme Studio").await;
    let rental = ready_rental(&w, &mut owner, &org).await;

    // the app asks; the code is shown in the app and the portal page opens
    let r = post_app(
        &w,
        "/api/v1/devices/authorize",
        json!({ "client": "Taro 1.4.1 on Windows\u{7}" }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let device = r.json["device_code"].as_str().unwrap().to_string();
    let code = r.json["user_code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 9);
    assert_eq!(r.json["verification_uri"], "http://portal.test/device");
    assert_eq!(
        r.json["verification_uri_complete"],
        format!("http://portal.test/device?code={code}")
    );
    assert_eq!(r.json["interval"], 5);

    // not approved yet; polling too fast is told to slow down
    let poll = || {
        post_app(
            &w,
            "/api/v1/devices/token",
            json!({ "device_code": device.clone() }),
            None,
        )
    };
    let r = poll().await;
    assert_eq!(
        (r.status, r.json["status"].as_str()),
        (StatusCode::ACCEPTED, Some("pending"))
    );
    let r = poll().await;
    assert_eq!(r.json["status"], "slow_down");

    // approving needs a portal session (and its Origin and CSRF token)
    let r = w
        .call(
            &mut w.browser(),
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let mut no_origin = Client {
        origin: "",
        ..w.user("owner@acme.test").await
    };
    let r = w
        .call(
            &mut no_origin,
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a cross-site approval is refused"
    );

    // the portal shows which app asks (the code typed loosely is found), then approves
    let loose = code.to_lowercase().replace('-', " ");
    let r = w
        .call(
            &mut owner,
            Method::GET,
            &format!(
                "/api/v1/devices/pending?user_code={}",
                loose.replace(' ', "%20")
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["request"]["client"], "Taro 1.4.1 on Windows");
    let r = w
        .call(
            &mut owner,
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": loose })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    // the app gets its token once
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let r = poll().await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["email"], "owner@acme.test");
    let token = r.json["access_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("tdv_"));
    let r = poll().await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a code works once"
    );
    let (stored,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM device_tokens WHERE secret_hash = $1")
            .bind(token.split('.').nth(1).unwrap().as_bytes())
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!(stored, 0, "only the hash is stored");

    // My Macs, and connecting
    let r = get_app(&w, "/api/v1/device/me", &token).await;
    assert_eq!(r.json["email"], "owner@acme.test");
    let r = get_app(&w, "/api/v1/device/macs", &token).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let macs = r.json["macs"].as_array().unwrap();
    assert_eq!(macs.len(), 1);
    assert_eq!(macs[0]["rental_id"], rental.as_str());
    assert_eq!(macs[0]["organization"], "Acme Studio");
    assert_eq!(macs[0]["connectable"], true);
    let r = post_app(
        &w,
        &format!("/api/v1/device/macs/{rental}/connect"),
        json!({}),
        Some(&token),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["taro_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(r.json["password"].as_str().is_some_and(|s| !s.is_empty()));
    let (client,): (Option<String>,) =
        sqlx::query_as("SELECT client FROM remote_sessions WHERE rental_id = $1::uuid")
            .bind(&rental)
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!(client.as_deref(), Some("Taro 1.4.1 on Windows"));

    // someone else's token sees nothing of this organization, and cannot connect
    let mut other = w.user("other@else.test").await;
    let _ = w.org(&mut other, "Else").await;
    let (other_token, _) = sign_in_app(&w, &mut other).await;
    let r = get_app(&w, "/api/v1/device/macs", &other_token).await;
    assert_eq!(r.json["macs"].as_array().unwrap().len(), 0);
    let r = post_app(
        &w,
        &format!("/api/v1/device/macs/{rental}/connect"),
        json!({}),
        Some(&other_token),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // bad tokens
    for bad in [
        "",
        "tdv_x.y",
        &format!("{}x", token),
        &token.replace("tdv_", ""),
    ] {
        assert_eq!(
            get_app(&w, "/api/v1/device/macs", bad).await.status,
            StatusCode::UNAUTHORIZED,
            "{bad}"
        );
    }

    // the portal lists the signed-in app and revokes it
    let r = w
        .call(&mut owner, Method::GET, "/api/v1/auth/devices", None)
        .await;
    let devices = r.json["devices"].as_array().unwrap().clone();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["client"], "Taro 1.4.1 on Windows");
    let id = devices[0]["id"].as_str().unwrap();
    let r = w
        .call(
            &mut other,
            Method::POST,
            &format!("/api/v1/auth/devices/{id}/revoke"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "only its user revokes it");
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/auth/devices/{id}/revoke"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        get_app(&w, "/api/v1/device/macs", &token).await.status,
        StatusCode::UNAUTHORIZED
    );

    // signing out of the app ends its token
    let (t2, _) = sign_in_app(&w, &mut owner).await;
    assert_eq!(
        get_app(&w, "/api/v1/device/macs", &t2).await.status,
        StatusCode::OK
    );
    assert_eq!(
        post_app(&w, "/api/v1/device/sign-out", json!({}), Some(&t2))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        get_app(&w, "/api/v1/device/macs", &t2).await.status,
        StatusCode::UNAUTHORIZED
    );

    // everything is in the audit log
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM audit_events WHERE action IN ('device.approved', 'device.signed_in', 'device.revoked', 'device.signed_out')")
        .fetch_one(&w.state.db)
        .await
        .unwrap();
    assert!(n >= 6, "{n}");
}

#[tokio::test]
async fn denied_expired_and_paused() {
    let w = world!();
    let mut owner = w.user("owner@acme.test").await;
    let _ = w.org(&mut owner, "Acme Studio").await;

    // denied in the portal: the app is told so
    let r = post_app(&w, "/api/v1/devices/authorize", json!({}), None).await;
    let device = r.json["device_code"].as_str().unwrap().to_string();
    let code = r.json["user_code"].as_str().unwrap().to_string();
    let r = w
        .call(
            &mut owner,
            Method::POST,
            "/api/v1/devices/deny",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = post_app(
        &w,
        "/api/v1/devices/token",
        json!({ "device_code": device }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.text.contains("denied"), "{}", r.text);
    let r = w
        .call(
            &mut owner,
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a denied code cannot be approved"
    );

    // expired
    let r = post_app(&w, "/api/v1/devices/authorize", json!({}), None).await;
    let device = r.json["device_code"].as_str().unwrap().to_string();
    let code = r.json["user_code"].as_str().unwrap().to_string();
    sqlx::query("UPDATE device_authorizations SET expires_at = now() - interval '1 second' WHERE user_code = $1")
        .bind(&code)
        .execute(&w.state.db)
        .await
        .unwrap();
    let r = w
        .call(
            &mut owner,
            Method::POST,
            "/api/v1/devices/approve",
            Some(json!({ "user_code": code })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = post_app(
        &w,
        "/api/v1/devices/token",
        json!({ "device_code": device }),
        None,
    )
    .await;
    assert!(r.text.contains("expired"), "{}", r.text);
    let r = post_app(
        &w,
        "/api/v1/devices/token",
        json!({ "device_code": "nonsense" }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // the kill switch
    sqlx::query("UPDATE feature_flags SET enabled = false WHERE key = 'devices.sign_in'")
        .execute(&w.state.db)
        .await
        .unwrap();
    let r = post_app(&w, "/api/v1/devices/authorize", json!({}), None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
}
