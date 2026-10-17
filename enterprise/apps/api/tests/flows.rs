//! End to end through the real routers and a real Postgres: the first vertical slice and the
//! acceptance criteria of PRD §19 that the API can show.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

macro_rules! world {
    () => {
        match world().await {
            Some(w) => w,
            None => return,
        }
    };
}

/// Sign in → organization → plan → test checkout → verified webhook → provisioning job → mock
/// provider provisions → Mac assigned → connection grant → redeemed → audited.
#[tokio::test]
async fn the_first_vertical_slice() {
    let w = world!();
    let mut owner = w.user("owner@acme.test").await;
    let org = w.org(&mut owner, "Acme Studio").await;
    let r = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/api/v1/organizations/{org}/mac-plans"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let day = r.json["plans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["plan"]["slug"] == "m4-day")
        .unwrap()
        .clone();
    assert_eq!(
        day["available"], 2,
        "two M4 Macs in eu-west, from the provider's inventory"
    );
    assert_eq!(r.json["simulated_provider"], true);

    // a rental, idempotently
    let body = json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "slice-0001" });
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rental = r.json["id"].as_str().unwrap().to_string();
    assert_eq!(r.json["status"], "pending_payment");
    let again = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(body),
        )
        .await;
    assert_eq!(
        (again.status, again.json["id"].as_str()),
        (StatusCode::OK, Some(rental.as_str())),
        "the same key: the same rental"
    );

    // checkout: the price is the server's
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/checkout"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["url"]
        .as_str()
        .unwrap()
        .starts_with("https://checkout.stripe.test/"));
    assert_eq!(w.stripe.checkouts.lock().unwrap()[0].amount_minor, 1900);

    // the browser coming back from Stripe proves nothing
    let _ = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/rentals/{rental}?checkout=done"),
            None,
        )
        .await;
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "pending_payment");

    // a forged webhook is refused; the real one is stored once and processed once
    assert_eq!(
        w.stripe_event(
            "checkout.session.completed",
            w.completed_for(&rental),
            false
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "pending_payment");
    let id = "evt_slice_completed";
    let now = chrono::Utc::now().timestamp();
    assert_eq!(
        w.stripe_event_with_id(
            id,
            "checkout.session.completed",
            w.completed_for(&rental),
            true,
            now
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        w.stripe_event_with_id(
            id,
            "checkout.session.completed",
            w.completed_for(&rental),
            true,
            now
        )
        .await
        .status,
        StatusCode::OK,
        "a repeat is accepted and ignored"
    );
    let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM stripe_webhook_events")
        .fetch_one(&w.state.db)
        .await
        .unwrap();
    assert_eq!(events, 1);

    // paid -> provisioning -> ready, through the job and the mock provider
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "ready");
    let (jobs, state): (i64, String) = sqlx::query_as("SELECT count(*), max(state) FROM provisioning_jobs WHERE rental_id = $1::uuid AND operation = 'provision'").bind(&rental).fetch_one(&w.state.db).await.unwrap();
    assert_eq!((jobs, state.as_str()), (1, "succeeded"));
    let r = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/api/v1/rentals/{rental}"),
            None,
        )
        .await;
    assert_eq!(
        r.json["rental"]["connection_state"], "connectable",
        "{}",
        r.text
    );
    assert_eq!(r.json["actions"]["connect"], true);
    let history: Vec<&str> = r.json["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["to"].as_str().unwrap())
        .collect();
    assert_eq!(
        history,
        [
            "draft",
            "pending_payment",
            "pending_payment",
            "paid",
            "provisioning",
            "ready"
        ]
    );

    // the Mac goes to a teammate
    let mut mate = w.user("mate@acme.test").await;
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/invitations"),
            Some(json!({ "email": "mate@acme.test", "role": "member" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let token = w
        .mail
        .0
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(t, s, _)| t == "mate@acme.test" && s.starts_with("Join"))
        .unwrap()
        .2
        .split("token=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut mate,
            Method::POST,
            "/api/v1/invitations/accept",
            Some(json!({ "token": token })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let mate_id = w
        .call(&mut mate, Method::GET, "/api/v1/me", None)
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/assignments"),
            Some(json!({ "user_id": mate_id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    // the owner is not assigned any more: no grant for them
    assert_eq!(
        w.call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/connect-grant"),
            None
        )
        .await
        .status,
        StatusCode::CONFLICT
    );

    // the member sees their Mac and connects
    let r = w
        .call(
            &mut mate,
            Method::GET,
            &format!("/api/v1/organizations/{org}/rentals"),
            None,
        )
        .await;
    assert_eq!(r.json["rentals"].as_array().unwrap().len(), 1);
    let r = w
        .call(
            &mut mate,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/connect-grant"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let grant = r.json["grant"].as_str().unwrap().to_string();
    assert!(r.json["taro_link"]
        .as_str()
        .unwrap()
        .starts_with("taro://connect?server="));
    let mut app = Client {
        cookie: None,
        csrf: None,
        origin: "",
        ops: false,
    };
    let r = w
        .call(
            &mut app,
            Method::POST,
            "/api/v1/connect/redeem",
            Some(json!({ "grant": grant, "client": "Taro 1.4.1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["taro_id"].as_str().unwrap().len(), 9);
    assert!(!r.json["password"].as_str().unwrap().is_empty());
    // once only
    assert_eq!(
        w.call(
            &mut app,
            Method::POST,
            "/api/v1/connect/redeem",
            Some(json!({ "grant": grant }))
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // and every step is in the audit trail
    let actions: Vec<(String,)> = sqlx::query_as("SELECT action FROM audit_events ORDER BY id")
        .fetch_all(&w.state.db)
        .await
        .unwrap();
    let actions: Vec<&str> = actions.iter().map(|a| a.0.as_str()).collect();
    for a in [
        "auth.sign_in",
        "organization.created",
        "rental.created",
        "billing.checkout_started",
        "billing.paid",
        "rental.ready",
        "invitation.accepted",
        "rental.assigned",
        "connect.grant",
        "connect.redeemed",
    ] {
        assert!(actions.contains(&a), "{a} missing from {actions:?}");
    }
    // nothing secret reached the audit trail or a mail
    let (details,): (String,) =
        sqlx::query_as("SELECT string_agg(details::text, ' ') FROM audit_events")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert!(
        !details.contains(grant.split('.').nth(1).unwrap()),
        "the grant secret is not audited"
    );
    w.drop_db().await;
}

#[tokio::test]
async fn tenants_are_isolated() {
    let w = world!();
    let mut a = w.user("a@one.test").await;
    let mut b = w.user("b@two.test").await;
    let org_a = w.org(&mut a, "One").await;
    let org_b = w.org(&mut b, "Two").await;
    let r = w
        .call(
            &mut a,
            Method::POST,
            &format!("/api/v1/organizations/{org_a}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-month").await, "idempotency_key": "iso-00001" })),
        )
        .await;
    let rental = r.json["id"].as_str().unwrap().to_string();
    // B, by guessing ids: nothing is there for them (not even a 403 that says it exists)
    for (m, path) in [
        (Method::GET, format!("/api/v1/organizations/{org_a}")),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/members"),
        ),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/rentals"),
        ),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/billing/summary"),
        ),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/billing/invoices"),
        ),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/domains"),
        ),
        (
            Method::GET,
            format!("/api/v1/organizations/{org_a}/audit-events"),
        ),
        (Method::GET, format!("/api/v1/rentals/{rental}")),
        (Method::POST, format!("/api/v1/rentals/{rental}/cancel")),
        (Method::POST, format!("/api/v1/rentals/{rental}/checkout")),
        (
            Method::POST,
            format!("/api/v1/rentals/{rental}/connect-grant"),
        ),
        (
            Method::POST,
            format!("/api/v1/organizations/{org_a}/invitations"),
        ),
    ] {
        let body = if m == Method::POST {
            Some(json!({ "email": "x@y.test", "role": "admin" }))
        } else {
            None
        };
        let r = w.call(&mut b, m.clone(), &path, body).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m} {path}: {}", r.text);
    }
    // creating in A from B's session: refused too
    let r = w
        .call(
            &mut b,
            Method::POST,
            &format!("/api/v1/organizations/{org_a}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-month").await, "idempotency_key": "iso-00002" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // and A cannot assign B (not a member) to A's Mac
    let b_id = w.call(&mut b, Method::GET, "/api/v1/me", None).await.json["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut a,
            Method::POST,
            &format!("/api/v1/rentals/{rental}/assignments"),
            Some(json!({ "user_id": b_id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let _ = org_b;
    w.drop_db().await;
}

#[tokio::test]
async fn roles_inside_an_organization() {
    let w = world!();
    let mut owner = w.user("o@acme.test").await;
    let org = w.org(&mut owner, "Acme").await;
    let mut member = w.user("m@acme.test").await;
    let mut billing = w.user("b@acme.test").await;
    for (who, role) in [
        ("m@acme.test", "member"),
        ("b@acme.test", "billing_manager"),
    ] {
        w.call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/invitations"),
            Some(json!({ "email": who, "role": role })),
        )
        .await;
        let token = w
            .mail
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(t, s, _)| t == who && s.starts_with("Join"))
            .unwrap()
            .2
            .split("token=")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        let c = if role == "member" {
            &mut member
        } else {
            &mut billing
        };
        assert_eq!(
            w.call(
                c,
                Method::POST,
                "/api/v1/invitations/accept",
                Some(json!({ "token": token }))
            )
            .await
            .status,
            StatusCode::OK
        );
    }
    // a member: no billing, no rentals of others, no inviting
    assert_eq!(
        w.call(
            &mut member,
            Method::GET,
            &format!("/api/v1/organizations/{org}/billing/summary"),
            None
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.call(
            &mut member,
            Method::POST,
            &format!("/api/v1/organizations/{org}/invitations"),
            Some(json!({ "email": "z@acme.test", "role": "member" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.call(
            &mut member,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "role-0001" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // a billing manager: billing yes, rentals no
    assert_eq!(
        w.call(
            &mut billing,
            Method::GET,
            &format!("/api/v1/organizations/{org}/billing/summary"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        w.call(
            &mut billing,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "role-0002" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    // the last owner stays an owner
    let me = w
        .call(&mut owner, Method::GET, "/api/v1/me", None)
        .await
        .json["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        w.call(
            &mut owner,
            Method::PATCH,
            &format!("/api/v1/organizations/{org}/members/{me}"),
            Some(json!({ "role": "member" }))
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    // refusals are audited
    let (denied,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM audit_events WHERE result = 'denied'")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert!(denied >= 4, "{denied}");
    w.drop_db().await;
}

#[tokio::test]
async fn customers_never_reach_the_console() {
    let w = world!();
    let mut c = w.user("someone@acme.test").await;
    // the portal does not even have the ops routes
    for path in [
        "/api/v1/ops/overview",
        "/api/v1/ops/mac-resources",
        "/api/v1/ops/audit-events",
        "/metrics",
        "/internal/tls/ask?domain=x.test",
    ] {
        let r = w.call(&mut c, Method::GET, path, None).await;
        assert_ne!(r.status, StatusCode::OK, "{path}");
    }
    // a customer cookie at the console: not a session there
    let mut stolen = w.console();
    stolen.cookie = c
        .cookie
        .as_ref()
        .map(|k| k.replace("taro_session", "taro_ops_session"));
    assert_eq!(
        w.call(&mut stolen, Method::GET, "/api/v1/ops/overview", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // a customer signing in at the console: refused, audited
    let mut ops = w.console();
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/auth/email/start",
            Some(json!({ "email": "someone@acme.test" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let code = w.mail.code("someone@acme.test");
    let r = w
        .call(
            &mut ops,
            Method::POST,
            "/api/v1/auth/email/verify",
            Some(json!({ "email": "someone@acme.test", "code": code })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert!(ops.cookie.is_none());

    // the bootstrap admin gets in, and only operator permissions apply there
    let mut admin = w.console();
    let r = w.sign_in(&mut admin, "admin@taro.test").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = w
        .call(&mut admin, Method::GET, "/api/v1/ops/overview", None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["available_resources"]["value"], 8,
        "the six mock Macs and the two that test failures, all free: {}",
        r.text
    );
    assert!(
        r.json["available_resources"]["definition"]
            .as_str()
            .is_some()
            && r.json["available_resources"]["as_of"].as_str().is_some()
    );
    // a support operator reads but does not change
    let mut support = w.user("support@taro.test").await;
    let _ = &mut support;
    let r = w
        .call(
            &mut admin,
            Method::POST,
            "/api/v1/ops/access",
            Some(json!({ "email": "support@taro.test", "role": "support_operator" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    let mut sc = w.console();
    assert_eq!(
        w.sign_in(&mut sc, "support@taro.test").await.status,
        StatusCode::OK
    );
    assert_eq!(
        w.call(&mut sc, Method::GET, "/api/v1/ops/mac-resources", None)
            .await
            .status,
        StatusCode::OK
    );
    let res = w
        .call(&mut sc, Method::GET, "/api/v1/ops/mac-resources", None)
        .await;
    let id = res.json["resources"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        w.call(
            &mut sc,
            Method::POST,
            &format!("/api/v1/ops/mac-resources/{id}/hold"),
            Some(json!({ "reason": "test" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.call(
            &mut sc,
            Method::PATCH,
            "/api/v1/ops/feature-flags/rentals.new",
            Some(json!({ "enabled": false }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.call(&mut sc, Method::GET, "/api/v1/ops/audit-events", None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    w.drop_db().await;
}

#[tokio::test]
async fn sessions_csrf_origin_and_sign_in_tokens() {
    let w = world!();
    let mut c = w.user("csrf@acme.test").await;
    // no CSRF token: refused; wrong origin: refused
    let mut no_token = c.clone();
    no_token.csrf = None;
    assert_eq!(
        w.call(
            &mut no_token,
            Method::POST,
            "/api/v1/organizations",
            Some(json!({ "name": "X" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let mut evil = c.clone();
    evil.origin = "https://evil.example";
    assert_eq!(
        w.call(
            &mut evil,
            Method::POST,
            "/api/v1/organizations",
            Some(json!({ "name": "X" }))
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.call(
            &mut c,
            Method::POST,
            "/api/v1/organizations",
            Some(json!({ "name": "X" }))
        )
        .await
        .status,
        StatusCode::CREATED
    );
    // the link works once
    let mut d = w.browser();
    w.call(
        &mut d,
        Method::POST,
        "/api/v1/auth/email/start",
        Some(json!({ "email": "link@acme.test" })),
    )
    .await;
    let token = w.mail.link_token("link@acme.test");
    assert_eq!(
        w.call(
            &mut d,
            Method::POST,
            "/api/v1/auth/email/verify",
            Some(json!({ "token": token }))
        )
        .await
        .status,
        StatusCode::OK
    );
    let mut e = w.browser();
    assert_eq!(
        w.call(
            &mut e,
            Method::POST,
            "/api/v1/auth/email/verify",
            Some(json!({ "token": token }))
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a used link"
    );
    // five wrong codes burn the request; the right one then fails too
    let mut f = w.browser();
    w.call(
        &mut f,
        Method::POST,
        "/api/v1/auth/email/start",
        Some(json!({ "email": "guess@acme.test" })),
    )
    .await;
    let code = w.mail.code("guess@acme.test");
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        assert_eq!(
            w.call(
                &mut f,
                Method::POST,
                "/api/v1/auth/email/verify",
                Some(json!({ "email": "guess@acme.test", "code": wrong }))
            )
            .await
            .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(
        w.call(
            &mut f,
            Method::POST,
            "/api/v1/auth/email/verify",
            Some(json!({ "email": "guess@acme.test", "code": code }))
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // an unknown address gets the same answer as a known one
    let mut g = w.browser();
    assert_eq!(
        w.call(
            &mut g,
            Method::POST,
            "/api/v1/auth/email/start",
            Some(json!({ "email": "nobody@nowhere.test" }))
        )
        .await
        .status,
        StatusCode::ACCEPTED
    );
    // sign-out ends the session on the server; so does revoking it from another
    let old = c.cookie.clone();
    assert_eq!(
        w.call(&mut c, Method::POST, "/api/v1/auth/sign-out", None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let mut replay = w.browser();
    replay.cookie = old;
    assert_eq!(
        w.call(&mut replay, Method::GET, "/api/v1/me", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let mut one = w.user("two@acme.test").await;
    let mut two = w.user("two@acme.test").await;
    assert_eq!(
        w.call(
            &mut one,
            Method::POST,
            "/api/v1/auth/sessions/revoke-others",
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        w.call(&mut two, Method::GET, "/api/v1/me", None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        w.call(&mut one, Method::GET, "/api/v1/me", None)
            .await
            .status,
        StatusCode::OK
    );
    // no code or token in the log of mails' subjects is the concern of the mailer; the audit
    // trail has none
    let (details,): (Option<String>,) =
        sqlx::query_as("SELECT string_agg(details::text, ' ') FROM audit_events")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert!(!details.unwrap_or_default().contains(&code));
    w.drop_db().await;
}

#[tokio::test]
async fn provider_failures_never_look_ready() {
    let w = world!();
    let mut owner = w.user("f@acme.test").await;
    let org = w.org(&mut owner, "Failing").await;
    // a Mac that fails its health check: failed, not ready; the Mac is held, an incident opens
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("fail-day").await, "idempotency_key": "fail-0001" })),
        )
        .await;
    let rental = r.json["id"].as_str().unwrap().to_string();
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/rentals/{rental}/checkout"),
        None,
    )
    .await;
    w.stripe_event("checkout.session.completed", w.completed_for(&rental), true)
        .await;
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "failed");
    let (incidents,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM incidents WHERE component = 'provisioning' AND status = 'open'",
    )
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!(incidents, 1);
    let (state, hold): (String, Option<String>) = sqlx::query_as(
        "SELECT state, hold_reason FROM mac_resources WHERE external_id = 'mock-fail-1'",
    )
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!(state, "maintenance");
    assert!(hold.is_some());
    // a Mac that fails once: retried with the same job and key, ready, one Mac only
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("flaky-day").await, "idempotency_key": "flaky-0001" })),
        )
        .await;
    let rental = r.json["id"].as_str().unwrap().to_string();
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/rentals/{rental}/checkout"),
        None,
    )
    .await;
    w.stripe_event("checkout.session.completed", w.completed_for(&rental), true)
        .await;
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "ready");
    let (attempts, jobs): (i32, i64) = sqlx::query_as(
        "SELECT max(attempts), count(*) FROM provisioning_jobs WHERE rental_id = $1::uuid",
    )
    .bind(&rental)
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!(jobs, 1);
    assert!(attempts >= 2, "{attempts}");
    let (assigned,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM mac_resources WHERE state = 'assigned'")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!(assigned, 1);
    w.drop_db().await;
}

/// A rental of `plan` with its checkout started, by the billing test's owner.
async fn new_rental(w: &World, org: &str, key: &str, plan: &str) -> String {
    let mut c = w.browser();
    w.sign_in(&mut c, "bill@acme.test").await;
    let r = w
        .call(
            &mut c,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan(plan).await, "idempotency_key": key })),
        )
        .await;
    let id = r.json["id"].as_str().unwrap().to_string();
    assert_eq!(
        w.call(
            &mut c,
            Method::POST,
            &format!("/api/v1/rentals/{id}/checkout"),
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    id
}

#[tokio::test]
async fn billing_edge_cases() {
    let w = world!();
    let mut owner = w.user("bill@acme.test").await;
    let org = w.org(&mut owner, "Billing").await;
    // a checkout for the wrong amount: not provisioned, an incident
    let a = new_rental(&w, &org, "edge-amount", "m4-day").await;
    let mut ev = w.completed_for(&a);
    ev["amount_total"] = json!(1);
    w.stripe_event("checkout.session.completed", ev, true).await;
    w.settle().await;
    assert_eq!(w.rental_status(&a).await, "pending_payment");
    let (n,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM incidents WHERE title LIKE 'A checkout for rental%'")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!(n, 1);

    // a monthly rental whose subscription.deleted arrives before checkout.completed: the delete
    // waits (retried), then applies once the rental is ready
    let m = new_rental(&w, &org, "edge-order", "m4-month").await;
    let done = w.completed_for(&m);
    let sub = done["subscription"].as_str().unwrap().to_string();
    let t = chrono::Utc::now().timestamp();
    w.stripe_event_with_id("evt_del", "customer.subscription.deleted", json!({ "id": sub, "status": "canceled", "metadata": { "organization_id": org, "rental_id": m } }), true, t + 10).await;
    taro_enterprise::billing::process_once(&w.state)
        .await
        .unwrap();
    let (st,): (String,) =
        sqlx::query_as("SELECT state FROM stripe_webhook_events WHERE id = 'evt_del'")
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!(st, "failed", "not applied before the checkout");
    w.stripe_event_with_id("evt_done", "checkout.session.completed", done, true, t)
        .await;
    w.settle().await;
    // the checkout made it ready, then the late deletion cancelled it and released the Mac
    assert_eq!(w.rental_status(&m).await, "cancelled");
    // an older subscription update arriving after never overwrites the newer state
    w.stripe_event_with_id("evt_old", "customer.subscription.updated", json!({ "id": sub, "status": "active", "metadata": { "organization_id": org, "rental_id": m } }), true, t - 100).await;
    w.settle().await;
    let (status,): (String,) = sqlx::query_as("SELECT status FROM subscriptions WHERE id = $1")
        .bind(&sub)
        .fetch_one(&w.state.db)
        .await
        .unwrap();
    assert_eq!(status, "canceled");

    // a payment failure: past due, owners told, no automatic suspension
    let p = new_rental(&w, &org, "edge-pastdue", "m4-month").await;
    let done = w.completed_for(&p);
    let sub = done["subscription"].as_str().unwrap().to_string();
    w.stripe_event("checkout.session.completed", done, true)
        .await;
    w.settle().await;
    w.stripe_event("invoice.payment_failed", json!({ "id": "in_1", "status": "open", "subscription": sub, "amount_due": 1900, "amount_paid": 0, "currency": "usd", "customer": format!("cus_{}", uuid::Uuid::parse_str(&org).unwrap().simple()) }), true).await;
    w.settle().await;
    let (st, billing): (String, String) =
        sqlx::query_as("SELECT status, billing_state FROM rentals WHERE id = $1::uuid")
            .bind(&p)
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!((st.as_str(), billing.as_str()), ("ready", "past_due"));
    let s = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/api/v1/organizations/{org}/billing/summary"),
            None,
        )
        .await;
    assert_eq!(s.json["past_due"], true, "{}", s.text);
    let inv = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/api/v1/organizations/{org}/billing/invoices"),
            None,
        )
        .await;
    assert_eq!(inv.json["invoices"][0]["id"], "in_1");

    // a refund while in use: refunded, an incident, the Mac stays (people decide)
    let o = new_rental(&w, &org, "edge-refund", "m4-day").await;
    let done = w.completed_for(&o);
    let pi = done["payment_intent"].as_str().unwrap().to_string();
    w.stripe_event("checkout.session.completed", done, true)
        .await;
    w.settle().await;
    w.stripe_event(
        "charge.refunded",
        json!({ "id": "ch_1", "payment_intent": pi }),
        true,
    )
    .await;
    w.settle().await;
    let (st, billing): (String, String) =
        sqlx::query_as("SELECT status, billing_state FROM rentals WHERE id = $1::uuid")
            .bind(&o)
            .fetch_one(&w.state.db)
            .await
            .unwrap();
    assert_eq!((st.as_str(), billing.as_str()), ("ready", "refunded"));

    // cancelling a monthly rental: at the period's end, at Stripe
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/rentals/{p}/cancel"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(w.stripe.cancelled.lock().unwrap().as_slice(), [sub]);
    // the billing portal is only for this organization's own customer
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/billing/portal-session"),
            None,
        )
        .await;
    assert!(
        r.json["url"].as_str().unwrap().ends_with(&format!(
            "cus_{}",
            uuid::Uuid::parse_str(&org).unwrap().simple()
        )),
        "{}",
        r.text
    );
    w.drop_db().await;
}

#[tokio::test]
async fn kill_switches_and_capacity() {
    let w = world!();
    let mut owner = w.user("k@acme.test").await;
    let org = w.org(&mut owner, "Kill").await;
    sqlx::query("UPDATE feature_flags SET enabled = false WHERE key = 'rentals.new'")
        .execute(&w.state.db)
        .await
        .unwrap();
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "kill-0001" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    sqlx::query("UPDATE feature_flags SET enabled = true WHERE key = 'rentals.new'")
        .execute(&w.state.db)
        .await
        .unwrap();
    // no Mac of a plan free: no rental
    sqlx::query("UPDATE mac_resources SET state = 'unavailable' WHERE chip = 'M4'")
        .execute(&w.state.db)
        .await
        .unwrap();
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "kill-0002" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // an idempotency key is required
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    w.drop_db().await;
}

#[tokio::test]
async fn reconciliation_reports_drift() {
    let w = world!();
    let mut owner = w.user("r@acme.test").await;
    let org = w.org(&mut owner, "Drift").await;
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/rentals"),
            Some(json!({ "plan_id": w.plan("m4-day").await, "idempotency_key": "drift-0001" })),
        )
        .await;
    let rental = r.json["id"].as_str().unwrap().to_string();
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/rentals/{rental}/checkout"),
        None,
    )
    .await;
    w.stripe_event("checkout.session.completed", w.completed_for(&rental), true)
        .await;
    w.settle().await;
    let (ext,): (String,) = sqlx::query_as("SELECT m.external_id FROM rentals r JOIN mac_resources m ON m.id = r.resource_id WHERE r.id = $1::uuid").bind(&rental).fetch_one(&w.state.db).await.unwrap();
    // the provider loses the Mac: an incident, but the rental is not touched on a guess
    w.mock.set_offline(&ext);
    taro_enterprise::reconcile::sync_inventory(&w.state)
        .await
        .unwrap();
    assert_eq!(w.rental_status(&rental).await, "ready");
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM incidents WHERE component = 'reconciliation' AND severity = 'sev2'",
    )
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!(n, 1);
    // seen again: the same incident, bumped
    taro_enterprise::reconcile::sync_inventory(&w.state)
        .await
        .unwrap();
    let (n, occ): (i64, i32) = sqlx::query_as(
        "SELECT count(*), max(occurrences) FROM incidents WHERE component = 'reconciliation'",
    )
    .fetch_one(&w.state.db)
    .await
    .unwrap();
    assert_eq!((n, occ), (1, 2));
    // one-time rentals expire at their end and give their Mac back
    sqlx::query("UPDATE rentals SET ends_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(&rental)
        .execute(&w.state.db)
        .await
        .unwrap();
    taro_enterprise::reconcile::expire(&w.state).await.unwrap();
    w.settle().await;
    assert_eq!(w.rental_status(&rental).await, "expired");
    w.drop_db().await;
}

#[tokio::test]
async fn domains_branding_and_the_audit_trail() {
    let w = world!();
    let mut owner = w.user("d@acme.test").await;
    let org = w.org(&mut owner, "Domains").await;
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/domains"),
            Some(json!({ "hostname": "Portal.Acme-Test.example" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["domain"]["id"].as_str().unwrap().to_string();
    assert_eq!(r.json["domain"]["state"], "pending_verification");
    assert!(r.json["dns"]["records"][0]["name"]
        .as_str()
        .unwrap()
        .starts_with("_taro-verify.portal.acme-test.example"));
    // the same hostname for another tenant: refused
    let mut other = w.user("x@other.test").await;
    let org2 = w.org(&mut other, "Other").await;
    assert_eq!(
        w.call(
            &mut other,
            Method::POST,
            &format!("/api/v1/organizations/{org2}/domains"),
            Some(json!({ "hostname": "portal.acme-test.example" }))
        )
        .await
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        w.call(
            &mut other,
            Method::POST,
            &format!("/api/v1/organizations/{org2}/domains"),
            Some(json!({ "hostname": "portal.test" }))
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the platform's own"
    );
    // unverified: no certificate; verified: yes
    let mut caddy = w.console();
    assert_eq!(
        w.call(
            &mut caddy,
            Method::GET,
            "/internal/tls/ask?domain=portal.acme-test.example",
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE domain_assignments SET state = 'provisioning_tls', verified_at = now() WHERE id = $1::uuid").bind(&id).execute(&w.state.db).await.unwrap();
    assert_eq!(
        w.call(
            &mut caddy,
            Method::GET,
            "/internal/tls/ask?domain=portal.acme-test.example",
            None
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        w.call(
            &mut caddy,
            Method::GET,
            "/internal/tls/ask?domain=unknown.example",
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );

    // branding: a draft, published; a bad one refused; rollback by publishing the old version
    let good = json!({ "displayName": "Acme Macs", "primaryColor": "#0A66C2", "themeMode": "light", "density": "comfortable", "enabledModules": ["my_macs", "billing"], "supportContact": "help@acme.test", "logoAssetId": null, "faviconAssetId": null });
    let r = w
        .call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/branding"),
            Some(good.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mut bad = good.clone();
    bad["customCss"] = json!("body { display: none }");
    assert_eq!(
        w.call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/branding"),
            Some(bad)
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        w.call(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/branding/1/publish"),
            None
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    let mut v2 = good.clone();
    v2["displayName"] = json!("Acme Macs 2");
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/organizations/{org}/branding"),
        Some(v2),
    )
    .await;
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/organizations/{org}/branding/2/publish"),
        None,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    w.call(
        &mut owner,
        Method::POST,
        &format!("/api/v1/organizations/{org}/branding/1/publish"),
        None,
    )
    .await;
    let r = w
        .call(
            &mut owner,
            Method::GET,
            &format!("/api/v1/organizations/{org}/branding"),
            None,
        )
        .await;
    assert_eq!(
        r.json["current"]["displayName"], "Acme Macs",
        "rolled back: {}",
        r.text
    );
    // an SVG logo is refused
    let r = w
        .raw(
            &mut owner,
            Method::POST,
            &format!("/api/v1/organizations/{org}/assets?kind=logo"),
            Some(("image/svg+xml", b"<svg onload='alert(1)'/>".to_vec())),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    // the audit trail cannot be rewritten
    let e = sqlx::query("UPDATE audit_events SET action = 'nothing'")
        .execute(&w.state.db)
        .await;
    assert!(e.is_err());
    let e = sqlx::query("DELETE FROM audit_events")
        .execute(&w.state.db)
        .await;
    assert!(e.is_err());
    w.drop_db().await;
}

#[tokio::test]
async fn google_id_tokens_are_verified() {
    use jsonwebtoken::{encode, EncodingKey, Header};
    // a JWKS served locally, with the test key
    let n = include_str!("fixtures/test-rsa.n").trim().to_string();
    let jwks = json!({ "keys": [{ "kty": "RSA", "kid": "k1", "alg": "RS256", "use": "sig", "n": n, "e": "AQAB" }] });
    let app = axum::Router::new().route(
        "/certs",
        axum::routing::get(move || {
            let j = jwks.clone();
            async move { axum::Json(j) }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let Some(w) = world_with(|c| {
        c.google = Some(taro_enterprise::config::Google {
            client_id: "client-123".into(),
            client_secret: taro_enterprise::config::Secret::new("s"),
        });
        c.google_jwks_url = format!("http://{addr}/certs");
    })
    .await
    else {
        return;
    };
    let key = EncodingKey::from_rsa_pem(include_bytes!("fixtures/test-rsa.pem")).unwrap();
    let mut h = Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = Some("k1".into());
    let now = chrono::Utc::now().timestamp();
    let claims = |aud: &str, iss: &str, exp: i64, nonce: &str| json!({ "iss": iss, "sub": "g-1", "aud": aud, "exp": exp, "iat": now, "nonce": nonce, "email": "g@acme.test", "email_verified": true });
    let ok = encode(
        &h,
        &claims("client-123", "https://accounts.google.com", now + 600, "n1"),
        &key,
    )
    .unwrap();
    let c = taro_enterprise::identity::verify_id_token(&w.state, &ok, "n1")
        .await
        .unwrap();
    assert_eq!(c.sub, "g-1");
    for (why, tok, nonce) in [
        (
            "another audience",
            encode(
                &h,
                &claims(
                    "someone-else",
                    "https://accounts.google.com",
                    now + 600,
                    "n1",
                ),
                &key,
            )
            .unwrap(),
            "n1",
        ),
        (
            "another issuer",
            encode(
                &h,
                &claims("client-123", "https://evil.example", now + 600, "n1"),
                &key,
            )
            .unwrap(),
            "n1",
        ),
        (
            "expired",
            encode(
                &h,
                &claims("client-123", "https://accounts.google.com", now - 600, "n1"),
                &key,
            )
            .unwrap(),
            "n1",
        ),
        ("another nonce", ok.clone(), "n2"),
        (
            "a broken signature",
            format!("{}x", &ok[..ok.len() - 1]),
            "n1",
        ),
    ] {
        assert!(
            taro_enterprise::identity::verify_id_token(&w.state, &tok, nonce)
                .await
                .is_err(),
            "{why}"
        );
    }
    // HS256 with the public key as a secret (the classic confusion) is refused
    let hs = encode(
        &Header {
            alg: jsonwebtoken::Algorithm::HS256,
            kid: Some("k1".into()),
            ..Header::default()
        },
        &claims("client-123", "https://accounts.google.com", now + 600, "n1"),
        &EncodingKey::from_secret(n.as_bytes()),
    )
    .unwrap();
    assert!(
        taro_enterprise::identity::verify_id_token(&w.state, &hs, "n1")
            .await
            .is_err()
    );
    // the start of a sign-in sets a state bound to this browser and uses PKCE
    let mut b = w.browser();
    let r = w
        .call(
            &mut b,
            Method::GET,
            "/api/v1/auth/google/start?return_to=//evil.example",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let loc = r.headers.get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("code_challenge_method=S256") && loc.contains("client_id=client-123"));
    assert!(r
        .headers
        .get_all("set-cookie")
        .iter()
        .any(|v| v.to_str().unwrap().starts_with("taro_oidc=")));
    let (ret,): (String,) = sqlx::query_as("SELECT return_to FROM oidc_states")
        .fetch_one(&w.state.db)
        .await
        .unwrap();
    assert_eq!(ret, "/", "an off-site return is not kept");
    // a callback without the browser's state cookie is refused
    let st = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let r = w
        .call(
            &mut w.browser(),
            Method::GET,
            &format!("/api/v1/auth/google/callback?code=c&state={st}"),
            None,
        )
        .await;
    assert!(r
        .headers
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("error=invalid"));
    w.drop_db().await;
}
