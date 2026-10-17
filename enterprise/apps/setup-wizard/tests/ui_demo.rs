//! Manual UI harness (ignored by default): serves the real wizard UI on 127.0.0.1 backed by the
//! local fake GitHub releases API and asset hosts (component manifest signed with a test key),
//! so the whole flow can be clicked through, or driven by `tests/ui/wizard1.mjs`.
//!
//!   cargo test -p taro-enterprise-setup --test ui_demo -- --ignored --nocapture
//!
//! Prints `UI_DEMO_URL=…` and `UI_DEMO_OUT=…` (the output folder). Runs for UI_DEMO_SECS
//! seconds (default 600). UI_DEMO_SLOW=1 streams downloads slowly (to watch the progress).

mod common;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn ui_demo() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let w = wizard_at("ui-demo", addr).await;
    if std::env::var("UI_DEMO_SLOW").is_ok_and(|v| v == "1") {
        w.fake.slow.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    println!("UI_DEMO_URL=http://{addr}/?t={ACCESS}");
    println!("UI_DEMO_OUT={}", w.out.display());
    let secs: u64 = std::env::var("UI_DEMO_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let router = taro_enterprise_setup::server::router(w.app.clone());
    let _ =
        tokio::time::timeout(std::time::Duration::from_secs(secs), axum::serve(l, router)).await;
    let _ = std::fs::remove_dir_all(&w.out);
}
