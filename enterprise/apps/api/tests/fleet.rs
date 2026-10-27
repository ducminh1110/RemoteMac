//! The provider's own Macs: a Mac enrolls with a code from the console and keeps a heartbeat; an
//! operator assigns it to a customer, who connects; an administrator takes it over (the customer
//! is cut off and cannot connect) and hands it back.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};

/// The Mac's side: no browser, a Bearer credential.
fn mac() -> Client {
    Client {
        cookie: None,
        csrf: None,
        origin: "",
        ops: false,
    }
}

async fn beat(w: &World, token: &str, applied: i64) -> Res {
    let auth = format!("Bearer {token}");
    let body = json!({ "taro_id": "rm-123456789", "applied_epoch": applied, "version": "1.6.0" });
    w.raw(
        &mut mac(),
        Method::POST,
        "/api/v1/agent/heartbeat",
        Some(("application/json", body.to_string().into_bytes())),
        &[("authorization", auth.as_str())],
    )
    .await
}

/// The Mac takes the epoch the server gives (a restart of its session), as Taro for Mac does.
async fn follow(w: &World, token: &str) -> Value {
    let r = beat(w, token, 0).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let e = r.json["epoch"].as_i64().unwrap();
    let r = beat(w, token, e).await;
    r.json
}

/// The provisioning queue, a few rounds (a fleet Mac is ready once its heartbeat runs the new
/// password, so `settle` would wait for ever).
async fn work(w: &World) {
    for _ in 0..6 {
        sqlx::query("UPDATE provisioning_jobs SET run_after = now(), locked_until = NULL WHERE state IN ('queued', 'waiting_provider', 'retryable_failed')")
            .execute(&w.state.db)
            .await
            .unwrap();
        taro_enterprise::provisioning::work_once(&w.state)
            .await
            .unwrap();
    }
}

async fn grant(w: &World, c: &mut Client, rental: &str) -> Res {
    w.call(
        c,
        Method::POST,
        &format!("/api/v1/rentals/{rental}/connect-grant"),
        None,
    )
    .await
}

#[tokio::test]
async fn a_mac_enrolls_is_assigned_and_an_administrator_takes_it_over() {
    let Some(w) = world_fleet(|c| c.relay_address = Some("relay.acme.test:7470".into())).await
    else {
        return;
    };
    let mut ops = w.console();
    assert_eq!(
        w.sign_in(&mut ops, "admin@taro.test").await.status,
        StatusCode::OK
    );
    let r = w
        .call(&mut ops, Method::GET, "/api/v1/ops/fleet", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["macs"].as_array().unwrap().len(), 0);
    assert_eq!(r.json["provider"], "fleet");

    // a code from the console; a customer cannot make one
    let mut nobody = w.user("nobody@acme.test").await;
    let r = w
        .call(
            &mut nobody,
            Method::POST,
            "/api/v1/ops/fleet/enrollment-codes",
            Some(json!({})),
        )
        .await;
    assert_ne!(r.status, StatusCode::CREATED);
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/fleet/enrollment-codes",
            Some(json!({ "label": "rack 1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let code = r.json["code"].as_str().unwrap().to_string();

    // the Mac enrolls (the code as typed: lower case); once
    let facts = |code: &str| {
        json!({ "code": code, "name": "Studio Mac", "model": "Mac14,3", "chip": "Apple M2 Pro", "cpu_cores": 10,
                "memory_gb": 16, "storage_gb": 512, "macos": "Version 15.1", "version": "1.6.0" })
    };
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/agent/enroll",
            Some(facts("WRONG-CODE-XXXX")),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/agent/enroll",
            Some(facts(&code.to_lowercase())),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let token = r.json["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("tag_"));
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/agent/enroll",
            Some(facts(&code)),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "one use");

    // its heartbeat: the password of its epoch, the relay
    assert_eq!(
        beat(&w, "tag_nope.nope", 0).await.status,
        StatusCode::UNAUTHORIZED
    );
    let first = follow(&w, &token).await;
    assert_eq!(first["relay"], "relay.acme.test:7470");
    let p1 = first["password"].as_str().unwrap().to_string();
    assert_eq!(p1.len(), 24);
    taro_enterprise::reconcile::sync_inventory(&w.state)
        .await
        .unwrap();
    let r = w
        .call(&mut ops, Method::GET, "/api/v1/ops/fleet", None)
        .await;
    let m = &r.json["macs"][0];
    assert_eq!(m["online"], true, "{m}");
    assert_eq!(m["state"], "available");
    assert_eq!(m["chip"], "M2 Pro");
    assert_eq!(m["taro_id"], "rm-123456789");
    let mac_id = m["id"].as_str().unwrap().to_string();

    // assigned, this Mac in particular; ready once the Mac runs the rental's password
    let plan = w.plan("m4-day").await;
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": "ana@acme.test", "plan_id": plan, "mac_id": mac_id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rental = r.json["rental"]["id"].as_str().unwrap().to_string();
    work(&w).await;
    assert_eq!(
        w.rental_status(&rental).await,
        "provisioning",
        "waiting for the Mac"
    );
    let second = follow(&w, &token).await;
    let p2 = second["password"].as_str().unwrap().to_string();
    assert_ne!(p1, p2, "the rental has a password of its own");
    work(&w).await;
    assert_eq!(w.rental_status(&rental).await, "ready");

    // the customer connects: the Mac's ID, the relay and the current password
    let mut ana = w.user("ana@acme.test").await;
    let g = grant(&w, &mut ana, &rental).await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/connect/redeem",
            Some(json!({ "grant": g.json["grant"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["taro_id"], "rm-123456789");
    assert_eq!(r.json["relay"], "relay.acme.test:7470");
    assert_eq!(r.json["password"], p2.as_str());

    // an administrator takes it over: a new password (the customer's session ends) and no grant
    // for the customer meanwhile
    let r = w
        .call(
            &mut ops,
            Method::POST,
            &format!("/api/v1/ops/fleet/{mac_id}/control"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["took_over"], true);
    let link = r.json["taro_link"].as_str().unwrap().to_string();
    let admin_grant = url::Url::parse(&link)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "grant")
        .unwrap()
        .1
        .to_string();
    let g = grant(&w, &mut ana, &rental).await;
    assert_eq!(g.status, StatusCode::CONFLICT, "{}", g.text);
    assert!(g.text.contains("administrator"), "{}", g.text);
    let third = follow(&w, &token).await;
    let p3 = third["password"].as_str().unwrap().to_string();
    assert_ne!(p3, p2);
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/connect/redeem",
            Some(json!({ "grant": admin_grant })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["password"], p3.as_str());
    let r = w
        .call(&mut ops, Method::GET, "/api/v1/ops/fleet", None)
        .await;
    assert_eq!(r.json["macs"][0]["control"]["by_email"], "admin@taro.test");
    assert_eq!(r.json["macs"][0]["assigned_email"], "ana@acme.test");

    // a second administrator cannot take it from the first
    sqlx::query("INSERT INTO users (id, email, display_name, email_verified) VALUES ($1, 'ops2@taro.test', 'Ops Two', true)")
        .bind(uuid::Uuid::new_v4())
        .execute(&w.state.db)
        .await
        .unwrap();
    sqlx::query("INSERT INTO operator_grants (user_id, role) SELECT id, 'platform_admin' FROM users WHERE email = 'ops2@taro.test'")
        .execute(&w.state.db)
        .await
        .unwrap();
    let mut ops2 = w.console();
    assert_eq!(
        w.sign_in(&mut ops2, "ops2@taro.test").await.status,
        StatusCode::OK
    );
    let r = w
        .call(
            &mut ops2,
            Method::POST,
            &format!("/api/v1/ops/fleet/{mac_id}/control"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // handed back: a new password again (the administrator's session ends); the customer can
    // connect, with it
    let r = w
        .call(
            &mut ops,
            Method::POST,
            &format!("/api/v1/ops/fleet/{mac_id}/release"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let fourth = follow(&w, &token).await;
    let p4 = fourth["password"].as_str().unwrap().to_string();
    assert_ne!(p4, p3);
    let g = grant(&w, &mut ana, &rental).await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/connect/redeem",
            Some(json!({ "grant": g.json["grant"] })),
        )
        .await;
    assert_eq!(r.json["password"], p4.as_str());

    // a Mac that is assigned cannot be removed; removed, its credential stops working
    let r = w
        .call(
            &mut ops,
            Method::DELETE,
            &format!("/api/v1/ops/fleet/{mac_id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    w.drop_db().await;
}

#[tokio::test]
async fn an_unassigned_mac_is_controlled_freely_and_kept_out_of_new_rentals() {
    let Some(w) = world_fleet(|c| c.relay_address = Some("relay.acme.test:7470".into())).await
    else {
        return;
    };
    let mut ops = w.console();
    assert_eq!(
        w.sign_in(&mut ops, "admin@taro.test").await.status,
        StatusCode::OK
    );
    let code = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/fleet/enrollment-codes",
            Some(json!({})),
        )
        .await
        .json["code"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut mac(),
            Method::POST,
            "/api/v1/agent/enroll",
            Some(json!({ "code": code, "name": "Spare Mac" })),
        )
        .await;
    let token = r.json["token"].as_str().unwrap().to_string();
    let before = follow(&w, &token).await;
    taro_enterprise::reconcile::sync_inventory(&w.state)
        .await
        .unwrap();
    let mac_id = w
        .call(&mut ops, Method::GET, "/api/v1/ops/fleet", None)
        .await
        .json["macs"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // no customer: no take-over, the password stays
    let r = w
        .call(
            &mut ops,
            Method::POST,
            &format!("/api/v1/ops/fleet/{mac_id}/control"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json["took_over"], false);
    assert_eq!(follow(&w, &token).await["password"], before["password"]);

    // while it is controlled, assigning it (or any free one) does not take it
    let plan = w.plan("m4-day").await;
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": "bo@acme.test", "plan_id": plan, "mac_id": mac_id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    w.drop_db().await;
}
