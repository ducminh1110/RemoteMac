//! The test harness: a fresh database per test (TARO_TEST_DATABASE_URL names a server), the real
//! routers driven in memory, a fake Stripe that records what it was asked, a mailbox.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use taro_enterprise::billing::stripe::{Billing, CheckoutRequest, CheckoutSession};
use taro_enterprise::config::{Config, Secret, Stripe};
use taro_enterprise::mail::Mailer;
use taro_enterprise::provider::mock::Mock;
use taro_enterprise::state::AppState;
use tower::ServiceExt;

pub const WEBHOOK_SECRET: &str = concat!("wh", "sec_test_secret");

#[derive(Default)]
pub struct Mailbox(pub Mutex<Vec<(String, String, String)>>);

#[async_trait]
impl Mailer for Mailbox {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push((to.into(), subject.into(), body.into()));
        Ok(())
    }
    fn kind(&self) -> &'static str {
        "test"
    }
}

impl Mailbox {
    /// The last sign-in code sent to `to`.
    pub fn code(&self, to: &str) -> String {
        let m = self.0.lock().unwrap();
        let (_, _, body) = m
            .iter()
            .rev()
            .find(|(t, s, _)| t == to && s.contains("code"))
            .expect("a sign-in mail");
        body.split("type this code: ").nth(1).unwrap()[..6].to_string()
    }
    pub fn link_token(&self, to: &str) -> String {
        let m = self.0.lock().unwrap();
        let (_, _, body) = m
            .iter()
            .rev()
            .find(|(t, s, _)| t == to && s.contains("code"))
            .expect("a sign-in mail");
        body.split("token=")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    }
    pub fn all_text(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(a, b, c)| format!("{a}\n{b}\n{c}\n"))
            .collect()
    }
}

#[derive(Default)]
pub struct FakeStripe {
    pub checkouts: Mutex<Vec<CheckoutRequest>>,
    pub expired: Mutex<Vec<String>>,
    pub cancelled: Mutex<Vec<String>>,
}

#[async_trait]
impl Billing for FakeStripe {
    fn livemode(&self) -> bool {
        false
    }
    fn simulated(&self) -> bool {
        false
    }
    async fn create_customer(&self, org: uuid::Uuid, _: &str, _: &str) -> anyhow::Result<String> {
        Ok(format!("cus_{}", org.simple()))
    }
    async fn create_checkout(&self, r: &CheckoutRequest) -> anyhow::Result<CheckoutSession> {
        let mut c = self.checkouts.lock().unwrap();
        c.push(r.clone());
        let id = format!("cs_test_{}", c.len());
        Ok(CheckoutSession {
            url: Some(format!("https://checkout.stripe.test/{id}")),
            id,
            status: Some("open".into()),
        })
    }
    async fn get_checkout(&self, id: &str) -> anyhow::Result<CheckoutSession> {
        let open = !self.expired.lock().unwrap().contains(&id.to_string());
        Ok(CheckoutSession {
            id: id.into(),
            url: Some(format!("https://checkout.stripe.test/{id}")),
            status: Some(if open { "open" } else { "expired" }.into()),
        })
    }
    async fn expire_checkout(&self, id: &str) -> anyhow::Result<()> {
        self.expired.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn cancel_subscription_at_period_end(&self, id: &str) -> anyhow::Result<()> {
        self.cancelled.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn portal_session(&self, customer: &str, _: &str) -> anyhow::Result<String> {
        Ok(format!("https://billing.stripe.test/{customer}"))
    }
}

pub struct World {
    pub state: AppState,
    pub portal: Router,
    pub ops: Router,
    pub mail: Arc<Mailbox>,
    pub stripe: Arc<FakeStripe>,
    pub mock: Arc<Mock>,
    pub db_name: String,
}

/// None when no test database server is configured (the test is skipped, unless
/// TARO_REQUIRE_DB_TESTS=1 says it must run).
pub async fn world() -> Option<World> {
    world_with(|_| {}).await
}

pub async fn world_with(tweak: impl FnOnce(&mut Config)) -> Option<World> {
    let Ok(base) = std::env::var("TARO_TEST_DATABASE_URL") else {
        if std::env::var("TARO_REQUIRE_DB_TESTS").as_deref() == Ok("1") {
            panic!("TARO_TEST_DATABASE_URL is required");
        }
        eprintln!("skipped: TARO_TEST_DATABASE_URL is not set");
        return None;
    };
    let name = format!("taro_test_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&base)
        .await
        .expect("test database server");
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let url = match base.rsplit_once('/') {
        Some((server, _)) => format!("{server}/{name}"),
        None => panic!("TARO_TEST_DATABASE_URL is postgres://user@host:port/db"),
    };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    taro_enterprise::db::migrate(&db).await.unwrap();
    let mut cfg = Config::for_tests(&url);
    cfg.stripe = Some(Stripe {
        secret_key: Secret::new(concat!("sk", "_test_x")),
        webhook_secret: Secret::new(WEBHOOK_SECRET),
        live: false,
        api_base: "http://stripe.invalid".into(),
    });
    cfg.bootstrap_admin_email = Some("admin@taro.test".into());
    tweak(&mut cfg);
    let mock = Arc::new(Mock::new(6, std::time::Duration::ZERO)); // M4 in eu-west: mock-001 and mock-005
    mock.add("mock-fail-1", "M1 Fail", "eu-west");
    mock.add("mock-flaky-1", "M1 Flaky", "eu-west");
    let mail = Arc::new(Mailbox::default());
    let stripe = Arc::new(FakeStripe::default());
    let state = taro_enterprise::build_state(
        cfg,
        db,
        mock.clone(),
        Some(stripe.clone()),
        Some(mail.clone()),
    )
    .await
    .unwrap();
    taro_enterprise::reconcile::sync_inventory(&state)
        .await
        .unwrap();
    // plans for the four kinds of Mac the mock has (and the two that misbehave)
    for (slug, chip, region, billing, hours) in [
        ("m4-day", "M4", "eu-west", "one_time", Some(24)),
        ("m4-month", "M4", "eu-west", "monthly", None),
        ("fail-day", "M1 Fail", "eu-west", "one_time", Some(1)),
        ("flaky-day", "M1 Flaky", "eu-west", "one_time", Some(1)),
    ] {
        sqlx::query("INSERT INTO mac_plans (id, slug, name, chip, cpu_cores, memory_gb, storage_gb, region, billing, duration_hours, price_minor, currency) VALUES ($1, $2, $2, $3, 10, 24, 512, $4, $5, $6, 1900, 'usd')")
            .bind(uuid::Uuid::new_v4())
            .bind(slug)
            .bind(chip)
            .bind(region)
            .bind(billing)
            .bind(hours)
            .execute(&state.db)
            .await
            .unwrap();
    }
    let portal = taro_enterprise::http::customer(&state);
    let ops = taro_enterprise::http::ops(&state);
    Some(World {
        state,
        portal,
        ops,
        mail,
        stripe,
        mock,
        db_name: name,
    })
}

/// A browser: its cookies and CSRF token, for one app.
#[derive(Clone)]
pub struct Client {
    pub cookie: Option<String>,
    pub csrf: Option<String>,
    pub origin: &'static str,
    pub ops: bool,
}

pub struct Res {
    pub status: StatusCode,
    pub json: Value,
    pub headers: axum::http::HeaderMap,
    pub text: String,
}

impl World {
    pub fn browser(&self) -> Client {
        Client {
            cookie: None,
            csrf: None,
            origin: "http://portal.test",
            ops: false,
        }
    }
    pub fn console(&self) -> Client {
        Client {
            cookie: None,
            csrf: None,
            origin: "http://ops.test",
            ops: true,
        }
    }

    pub async fn call(
        &self,
        c: &mut Client,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Res {
        self.raw(
            c,
            method,
            path,
            body.map(|b| ("application/json", b.to_string().into_bytes())),
            &[],
        )
        .await
    }

    pub async fn raw(
        &self,
        c: &mut Client,
        method: Method,
        path: &str,
        body: Option<(&str, Vec<u8>)>,
        extra: &[(&str, &str)],
    ) -> Res {
        let mut req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::HOST, if c.ops { "ops.test" } else { "portal.test" });
        if method != Method::GET && !c.origin.is_empty() {
            req = req.header(header::ORIGIN, c.origin);
            if let Some(t) = &c.csrf {
                req = req.header("x-csrf-token", t);
            }
        }
        if let Some(k) = &c.cookie {
            req = req.header(header::COOKIE, k);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let body = match body {
            Some((ct, b)) => {
                req = req.header(header::CONTENT_TYPE, ct);
                Body::from(b)
            }
            None => Body::empty(),
        };
        let mut req = req.body(body).unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo::<SocketAddr>(
                "127.0.0.1:40000".parse().unwrap(),
            ));
        let router = if c.ops {
            self.ops.clone()
        } else {
            self.portal.clone()
        };
        let res = router.oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        for v in headers.get_all(header::SET_COOKIE) {
            let v = v.to_str().unwrap();
            let kv = v.split(';').next().unwrap();
            if kv.starts_with("taro_session=") || kv.starts_with("taro_ops_session=") {
                c.cookie = if kv.ends_with('=') {
                    None
                } else {
                    Some(kv.to_string())
                };
            }
        }
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Res {
            status,
            json,
            headers,
            text,
        }
    }

    /// Sign in by email code; the client then holds the session and its CSRF token.
    pub async fn sign_in(&self, c: &mut Client, email: &str) -> Res {
        let r = self
            .call(
                c,
                Method::POST,
                "/api/v1/auth/email/start",
                Some(json!({ "email": email })),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
        let code = self.mail.code(email);
        let r = self
            .call(
                c,
                Method::POST,
                "/api/v1/auth/email/verify",
                Some(json!({ "email": email, "code": code })),
            )
            .await;
        if r.status == StatusCode::OK {
            let me = self.call(c, Method::GET, "/api/v1/me", None).await;
            c.csrf = me.json["csrf_token"].as_str().map(String::from);
        }
        r
    }

    pub async fn user(&self, email: &str) -> Client {
        let mut c = self.browser();
        let r = self.sign_in(&mut c, email).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        c
    }

    pub async fn org(&self, c: &mut Client, name: &str) -> String {
        let r = self
            .call(
                c,
                Method::POST,
                "/api/v1/organizations",
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["id"].as_str().unwrap().to_string()
    }

    pub async fn plan(&self, slug: &str) -> String {
        let (id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM mac_plans WHERE slug = $1")
            .bind(slug)
            .fetch_one(&self.state.db)
            .await
            .unwrap();
        id.to_string()
    }

    /// POST a Stripe event to the webhook, signed (or not).
    pub async fn stripe_event(&self, kind: &str, object: Value, signed: bool) -> Res {
        let id = format!("evt_{}", uuid::Uuid::new_v4().simple());
        self.stripe_event_with_id(&id, kind, object, signed, chrono::Utc::now().timestamp())
            .await
    }

    pub async fn stripe_event_with_id(
        &self,
        id: &str,
        kind: &str,
        object: Value,
        signed: bool,
        created: i64,
    ) -> Res {
        let body = json!({ "id": id, "type": kind, "livemode": false, "created": created, "data": { "object": object } }).to_string();
        let sig = if signed {
            taro_enterprise::billing::stripe::sign(
                body.as_bytes(),
                WEBHOOK_SECRET,
                chrono::Utc::now().timestamp(),
            )
        } else {
            taro_enterprise::billing::stripe::sign(
                body.as_bytes(),
                concat!("wh", "sec_wrong"),
                chrono::Utc::now().timestamp(),
            )
        };
        let mut machine = Client {
            cookie: None,
            csrf: None,
            origin: "",
            ops: false,
        };
        let mut req = Request::builder()
            .method(Method::POST)
            .uri("/webhooks/stripe")
            .header(header::HOST, "portal.test")
            .header("stripe-signature", sig)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo::<SocketAddr>(
                "127.0.0.1:40001".parse().unwrap(),
            ));
        let _ = &mut machine;
        let res = self.portal.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Res {
            status,
            json: Value::Null,
            headers,
            text: String::from_utf8_lossy(&bytes).to_string(),
        }
    }

    /// Process the Stripe inbox and the provisioning queue until both are idle.
    pub async fn settle(&self) {
        for _ in 0..200 {
            let a = taro_enterprise::billing::process_once(&self.state)
                .await
                .unwrap();
            let b = taro_enterprise::provisioning::work_once(&self.state)
                .await
                .unwrap();
            if !a && !b {
                // jobs waiting on the provider are due again shortly
                let (due,): (i64,) = sqlx::query_as("SELECT (SELECT count(*) FROM provisioning_jobs WHERE state IN ('queued', 'waiting_provider', 'retryable_failed')) + (SELECT count(*) FROM stripe_webhook_events WHERE state = 'failed')").fetch_one(&self.state.db).await.unwrap();
                if due == 0 {
                    return;
                }
                sqlx::query("UPDATE provisioning_jobs SET run_after = now() WHERE state IN ('queued', 'waiting_provider', 'retryable_failed')").execute(&self.state.db).await.unwrap();
                sqlx::query("UPDATE stripe_webhook_events SET next_attempt_at = now() WHERE state = 'failed'").execute(&self.state.db).await.unwrap();
            }
        }
        panic!("the queues did not settle");
    }

    pub async fn rental_status(&self, id: &str) -> String {
        let (s,): (String,) = sqlx::query_as("SELECT status FROM rentals WHERE id = $1::uuid")
            .bind(id)
            .fetch_one(&self.state.db)
            .await
            .unwrap();
        s
    }

    /// The checkout.session.completed Stripe would send for the last checkout.
    pub fn completed_for(&self, rental: &str) -> Value {
        let c = self.stripe.checkouts.lock().unwrap();
        let (n, r) = c
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| r.rental_id.to_string() == rental)
            .expect("a checkout for the rental");
        json!({
            "id": format!("cs_test_{}", n + 1), "object": "checkout.session", "payment_status": "paid",
            "amount_total": r.amount_minor, "currency": r.currency, "customer": r.customer,
            "subscription": if r.mode == "subscription" { json!(format!("sub_{}", r.rental_id.simple())) } else { Value::Null },
            "payment_intent": if r.mode == "payment" { json!(format!("pi_{}", r.rental_id.simple())) } else { Value::Null },
            "client_reference_id": r.rental_id, "metadata": { "rental_id": r.rental_id, "organization_id": r.organization_id },
        })
    }

    pub async fn drop_db(self) {
        let base = std::env::var("TARO_TEST_DATABASE_URL").unwrap();
        self.state.db.close().await;
        if let Ok(admin) = sqlx::PgPool::connect(&base).await {
            let _ = sqlx::query(&format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.db_name
            ))
            .execute(&admin)
            .await;
        }
    }
}
