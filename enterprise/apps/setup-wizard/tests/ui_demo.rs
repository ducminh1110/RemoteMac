//! Manual UI harness (ignored by default): serves the real wizard UI on 127.0.0.1 backed by the
//! local fake GitHub/Stripe/portal, so the whole flow can be clicked through (or driven by a
//! browser automation script) without any real credentials.
//!
//!   cargo test --test ui_demo -- --ignored --nocapture
//!
//! Fake credentials: GitHub token = common::GH_TOKEN, Stripe key = common::STRIPE_KEY,
//! template = tmpl/template. Runs for UI_DEMO_SECS seconds (default 600).

mod common;
use common::*;
use std::sync::Arc;
use taro_enterprise_setup::config::Timings;
use taro_enterprise_setup::state::{AppState, Options};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ui_demo() {
    let fake = start_fake().await;
    let dns = Arc::new(FakeDns::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let mut timings = Timings::fast();
    timings.device_unit = std::time::Duration::from_millis(500);
    timings.release_poll = std::time::Duration::from_millis(700);
    let app = AppState::new(
        addr,
        ACCESS.to_string(),
        Options {
            endpoints: endpoints(&fake.base()),
            timings,
            dns: dns.clone(),
            remember_dir: None,
            github_client_id: None,
            managed_zone: None,
            template: "tmpl/template".into(),
        },
    );
    // Pre-seed DNS so domain verification can pass for portal.example.com -> 203.0.113.10
    // once the generated TXT token is known (polled below).
    let app2 = app.clone();
    let dns2 = dns.clone();
    tokio::spawn(async move {
        loop {
            let (d, t) = {
                let w = app2.w();
                (w.domain.domain.clone(), w.domain.verify_token.clone())
            };
            if let (Some(d), Some(t)) = (d, t) {
                dns2.txt.lock().unwrap().insert(
                    format!("_taro-verify.{d}"),
                    vec![format!("taro-verify={t}")],
                );
                dns2.ips
                    .lock()
                    .unwrap()
                    .insert(d, vec!["203.0.113.10".parse().unwrap()]);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
    println!("UI_DEMO_URL=http://{addr}/?t={ACCESS}");
    println!("UI_DEMO_FAKE={}", fake.base());
    let secs: u64 = std::env::var("UI_DEMO_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let router = taro_enterprise_setup::server::router(app);
    let _ =
        tokio::time::timeout(std::time::Duration::from_secs(secs), axum::serve(l, router)).await;
}
