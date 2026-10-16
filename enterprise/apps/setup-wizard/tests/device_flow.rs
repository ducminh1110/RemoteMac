//! OAuth device flow polling: authorization_pending, slow_down and success, plus the API path.

mod common;
use common::*;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;
use taro_enterprise_setup::github::DeviceFlow;

fn flow(f: &Fake) -> DeviceFlow {
    DeviceFlow::new(
        &format!("{}/web", f.base()),
        reqwest::Client::new(),
        "Iv1.testclient",
    )
}

#[tokio::test]
async fn polling_honours_pending_and_slow_down() {
    let f = start_fake().await;
    *f.device_script.lock().unwrap() = vec!["pending", "slow_down", "pending"];
    let df = flow(&f);
    let code = df.start().await.unwrap();
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(code.interval, 1);
    let mut waits = vec![];
    let out = df
        .poll_until(&code, Duration::from_millis(2), |i| waits.push(i))
        .await
        .unwrap();
    assert_eq!(out.token.expose(), DEVICE_TOKEN);
    assert_eq!(out.polls, 4);
    assert_eq!(f.device_polls.load(Ordering::SeqCst), 4);
    // 1 s, then slow_down -> interval grows by >= 5 (GitHub says 6) and stays there.
    assert_eq!(waits, vec![1, 1, 6, 6]);
    assert_eq!(out.final_interval, 6);
}

#[tokio::test]
async fn polling_stops_on_expired_and_denied() {
    let f = start_fake().await;
    let df = flow(&f);
    let code = df.start().await.unwrap();
    *f.device_script.lock().unwrap() = vec!["pending", "expired"];
    let e = df
        .poll_until(&code, Duration::from_millis(1), |_| {})
        .await
        .unwrap_err();
    assert!(e.contains("expired"), "{e}");
    *f.device_script.lock().unwrap() = vec!["denied"];
    let e = df
        .poll_until(&code, Duration::from_millis(1), |_| {})
        .await
        .unwrap_err();
    assert!(e.contains("denied"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_flow_through_the_api() {
    let w = wizard().await;
    *w.fake.device_script.lock().unwrap() = vec!["pending", "slow_down"];
    let (s, r) = w
        .call("POST", "/api/github/device/start", Some(json!({})))
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["state"]["github"]["device"]["phase"], "waiting");
    assert_eq!(r["state"]["github"]["device"]["user_code"], "WDJB-MJHT");
    let st = w
        .wait_for("device flow", |s| {
            s["github"]["device"]["phase"] != "waiting"
        })
        .await;
    assert_eq!(st["github"]["device"]["phase"], "done", "{}", st["github"]);
    assert_eq!(st["github"]["user"]["login"], "octo");
    assert_eq!(st["github"]["auth_method"], "device");
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(DEVICE_TOKEN), "token leaked");
        assert!(!body.contains(DEVICE_CODE), "device code leaked");
    }
    let log = w.app.log.export_text();
    assert!(!log.contains(DEVICE_TOKEN) && !log.contains(DEVICE_CODE));
    // The device token is only sent to the API base.
    for q in w.fake.reqs() {
        if q.auth.clone().unwrap_or_default().contains(DEVICE_TOKEN) {
            assert!(q.path.starts_with("/gh/"), "{}", q.path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_flow_cancel() {
    let w = wizard().await;
    *w.fake.device_script.lock().unwrap() = vec!["pending"; 50];
    let (s, _) = w
        .call("POST", "/api/github/device/start", Some(json!({})))
        .await;
    assert_eq!(s, 200);
    let (s, r) = w
        .call("POST", "/api/github/device/cancel", Some(json!({})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(r["state"]["github"]["device"]["phase"], "idle");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let st = w.state().await;
    assert!(st["github"]["user"].is_null());
    assert_eq!(st["github"]["device"]["phase"], "idle");
}
