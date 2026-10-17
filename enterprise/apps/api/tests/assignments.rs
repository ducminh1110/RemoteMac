//! An operator assigns Macs to customers by email (no checkout): the customer sees and connects
//! to them; ending the assignment releases the Mac.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

#[tokio::test]
async fn an_operator_assigns_a_mac_and_ends_it() {
    let Some(w) = world().await else { return };
    let mut ops = w.console();
    let r = w.sign_in(&mut ops, "admin@taro.test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let plan = w.plan("m4-day").await;

    // a customer cannot assign, an operator can
    let mut cust = w.user("buyer@acme.test").await;
    let r = w
        .call(
            &mut cust,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": "x@y.test", "plan_id": plan })),
        )
        .await;
    assert_ne!(r.status, StatusCode::CREATED);
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": "nope", "plan_id": plan })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": " Ana@Acme.TEST ", "plan_id": plan })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rental = r.json["rental"]["id"].as_str().unwrap().to_string();
    assert_eq!(r.json["rental"]["status"], "paid");
    assert_eq!(r.json["rental"]["amount_minor"], 0);

    // the worker prepares it; the customer (a new account) signs in and sees it, connectable
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "ready");
    let mut ana = w.user("ana@acme.test").await;
    let orgs = w
        .call(&mut ana, Method::GET, "/api/v1/organizations", None)
        .await;
    let org = orgs.json["organizations"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut ana,
            Method::GET,
            &format!("/api/v1/organizations/{org}/rentals"),
            None,
        )
        .await;
    assert_eq!(r.json["rentals"][0]["id"], rental.as_str(), "{}", r.text);
    let r = w
        .call(
            &mut ana,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/connect-grant"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(r.json["taro_link"]
        .as_str()
        .unwrap()
        .starts_with("taro://connect?"));

    // a second Mac for the same customer goes into the same organization
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(json!({ "email": "ana@acme.test", "plan_id": plan })),
        )
        .await;
    assert_eq!(r.json["rental"]["organization_id"], org.as_str());

    // listed in the console; ended: the Mac is released
    let r = w
        .call(&mut ops, Method::GET, "/api/v1/ops/assignments", None)
        .await;
    let list = r.json["assignments"].as_array().unwrap();
    assert!(list
        .iter()
        .any(|a| a["rental_id"] == rental.as_str() && a["email"] == "ana@acme.test"));
    let r = w
        .call(
            &mut ops,
            Method::POST,
            &format!("/api/v1/ops/assignments/{rental}/end"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    w.settle().await;
    let st = w.rental_status(&rental).await;
    assert!(
        matches!(st.as_str(), "cancelled" | "expired" | "cancel_pending"),
        "{st}"
    );
    let r = w
        .call(
            &mut ops,
            Method::POST,
            &format!("/api/v1/ops/assignments/{rental}/end"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_providers_system_assigns_with_the_api_key() {
    let key = "assign-key-for-tests-0123456789abcdef";
    let Some(w) =
        world_with(|c| c.ops_api_key = Some(taro_enterprise::config::Secret::new(key))).await
    else {
        return;
    };
    let plan = w.plan("m4-day").await;
    let mut machine = Client {
        cookie: None,
        csrf: None,
        origin: "",
        ops: true,
    };
    let body = json!({ "email": "auto@acme.test", "plan_id": plan });
    let send = |k: String| {
        let auth = format!("Bearer {k}");
        let body = body.to_string().into_bytes();
        let w = &w;
        let mut m = Client {
            cookie: None,
            csrf: None,
            origin: "",
            ops: true,
        };
        async move {
            w.raw(
                &mut m,
                Method::POST,
                "/api/v1/ops/assignments",
                Some(("application/json", body)),
                &[("authorization", auth.as_str())],
            )
            .await
        }
    };
    assert_eq!(
        send("wrong-key-wrong-key-wrong-key-wrong".into())
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let r = send(key.into()).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rental = r.json["rental"]["id"].as_str().unwrap().to_string();
    // without a key or a session: refused
    let r = w
        .raw(
            &mut machine,
            Method::POST,
            "/api/v1/ops/assignments",
            Some(("application/json", body.to_string().into_bytes())),
            &[],
        )
        .await;
    assert_ne!(r.status, StatusCode::CREATED);
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "ready");
    let (by,): (String,) = sqlx::query_as(
        "SELECT actor FROM rental_events WHERE rental_id = $1::uuid ORDER BY id LIMIT 1",
    )
    .bind(&rental)
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!(by, "api-key");
}
