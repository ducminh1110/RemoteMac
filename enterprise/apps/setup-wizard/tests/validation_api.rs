//! Validation through the API: branding uploads, colours, Stripe key mode, webhook secret.

mod common;
use common::*;
use serde_json::json;
use taro_enterprise_setup::branding::{fixtures, MAX_BYTES};

#[tokio::test]
async fn branding_rejects_svg_oversize_and_bad_colour() {
    let w = wizard().await;
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"><script>fetch('/api/state')</script></svg>"#.to_vec();
    let (s, r) = w.upload("logo", svg).await;
    assert_eq!(s, 400);
    assert!(r["error"].as_str().unwrap().contains("SVG"), "{r}");
    let (s, _) = w
        .upload("favicon", b"body { background: red }".to_vec())
        .await;
    assert_eq!(s, 400);
    let (s, _) = w.upload("logo", b"console.log(1)".to_vec()).await;
    assert_eq!(s, 400);
    let (s, r) = w.upload("logo", fixtures::png(1025, 10)).await;
    assert_eq!(s, 400);
    assert!(r["error"].as_str().unwrap().contains("1024"), "{r}");
    let mut big = fixtures::jpeg(100, 100);
    big.resize(MAX_BYTES + 10, 0);
    let (s, r) = w.upload("logo", big).await;
    assert_eq!(s, 400);
    assert!(r["error"].as_str().unwrap().contains("1 MB"), "{r}");
    let (s, _) = w.upload("banner", fixtures::png(64, 64)).await;
    assert_eq!(s, 400);
    // Nothing was stored.
    let st = w.state().await;
    assert!(st["branding"]["logo"].is_null());
    assert!(st["branding"]["favicon"].is_null());
    // Valid file accepted.
    let (s, r) = w.upload("logo", fixtures::jpeg(512, 512)).await;
    assert_eq!(s, 200);
    assert_eq!(r["state"]["branding"]["logo"]["info"]["kind"], "jpeg");

    for bad in [
        "red",
        "#12345",
        "#1234567",
        "#GGHHII",
        "#fff",
        "#123456;x:y",
        "",
    ] {
        let (s, r) = w
            .call(
                "POST",
                "/api/branding",
                Some(json!({"primary_color": bad, "theme": "light", "density": "compact"})),
            )
            .await;
        assert_eq!(s, 400, "{bad}");
        assert!(r["fields"]["primary_color"].is_string(), "{bad}: {r}");
    }
    let (s, _) = w
        .call(
            "POST",
            "/api/branding",
            Some(json!({"primary_color": "#000000", "theme": "neon", "density": "compact"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, r) = w
        .call(
            "POST",
            "/api/branding",
            Some(json!({"primary_color": "#1a1a1a", "theme": "light", "density": "compact"})),
        )
        .await;
    assert_eq!(s, 200);
    assert!(r["state"]["branding"]["contrast_warning"].is_null());
    assert_eq!(r["state"]["branding"]["primary_color"], "#1A1A1A");
}

#[tokio::test]
async fn live_stripe_key_rejected_without_confirmation_and_never_sent() {
    let w = wizard().await;
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/key",
            Some(json!({"key": STRIPE_LIVE_KEY})),
        )
        .await;
    assert_eq!(s, 400);
    assert!(r["fields"]["key"].as_str().unwrap().contains("LIVE"), "{r}");
    let (s, _) = w
        .call(
            "POST",
            "/api/billing/key",
            Some(json!({"key": STRIPE_LIVE_KEY, "live_confirmed": true, "live_typed": "yes"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = w
        .call(
            "POST",
            "/api/billing/key",
            Some(json!({"key": STRIPE_LIVE_KEY, "live_confirmed": false, "live_typed": "LIVE"})),
        )
        .await;
    assert_eq!(s, 400);
    // The rejected key never left the PC.
    assert!(w.fake.reqs().iter().all(|q| !q
        .auth
        .clone()
        .unwrap_or_default()
        .contains(STRIPE_LIVE_KEY)));
    // With both confirmations it is validated against Stripe.
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/key",
            Some(json!({"key": STRIPE_LIVE_KEY, "live_confirmed": true, "live_typed": "LIVE"})),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["state"]["billing"]["mode"], "live");
    // The smoke test refuses live keys.
    let (s, r) = w
        .call("POST", "/api/billing/smoke", Some(json!({"confirm": true})))
        .await;
    assert_eq!(s, 400);
    assert!(r["error"].as_str().unwrap().contains("test-mode"), "{r}");
    // Publishable keys and garbage are refused.
    for k in [
        concat!("pk", "_test_51Habcdefghijklmnopqrst"),
        "hello",
        concat!("sk", "_test_x"),
    ] {
        let (s, _) = w
            .call("POST", "/api/billing/key", Some(json!({"key": k})))
            .await;
        assert_eq!(s, 400, "{k}");
    }
    // A key Stripe rejects is reported, not stored.
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/key",
            Some(json!({"key": concat!("sk", "_test_51Hwrongwrongwrongwrongwrong")})),
        )
        .await;
    assert_eq!(s, 502);
    assert!(r["error"].as_str().unwrap().contains("401"), "{r}");
    let st = w.state().await;
    assert_eq!(st["billing"]["key_present"], false);
    assert_eq!(st["steps"][5]["status"], "fail");
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(STRIPE_LIVE_KEY));
        assert!(!body.contains(concat!("sk", "_test_51Hwrongwrong")));
    }
}

#[tokio::test]
async fn webhook_secret_format_is_checked() {
    let w = wizard().await;
    for bad in [
        "",
        concat!("wh", "sec_"),
        concat!("wh", "sec_short"),
        concat!("sk", "_test_abcdefghijklmnopqrstu"),
        concat!("wh", "sec_has spaces in it 1234567"),
    ] {
        let (s, r) = w
            .call(
                "POST",
                "/api/billing/webhook-secret",
                Some(json!({"secret": bad})),
            )
            .await;
        assert_eq!(s, 400, "{bad}");
        assert!(r["fields"]["webhook_secret"].is_string());
    }
    let good = concat!("wh", "sec_0123456789abcdefABCDEF0123456789");
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/webhook-secret",
            Some(json!({"secret": good})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(r["state"]["billing"]["webhook_secret_present"], true);
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(good));
        assert!(!body.contains(concat!("wh", "sec_has spaces")));
    }
}

#[tokio::test]
async fn deployment_and_identity_validation() {
    let w = wizard().await;
    let (s, r) = w
        .call("POST", "/api/deployment", Some(json!({
            "environment": "production", "profile": "kubernetes", "portal_url": "http://portal.example.com",
            "ops_url": "ftp://x", "provider_adapter": "aws", "admin_email": "nope"
        })))
        .await;
    assert_eq!(s, 400);
    for f in [
        "profile",
        "portal_url",
        "ops_url",
        "provider_adapter",
        "admin_email",
    ] {
        assert!(r["fields"][f].is_string(), "{f}: {r}");
    }
    let (s, r) = w
        .call("POST", "/api/identity", Some(json!({"deployment_name": "", "operator_name": "x", "support_contact": "not contact", "modules": ["crypto"]})))
        .await;
    assert_eq!(s, 400);
    assert!(r["fields"]["deployment_name"].is_string());
    assert!(r["fields"]["support_contact"].is_string());
    assert!(r["fields"]["modules"].is_string());
    let (s, r) = w
        .call(
            "POST",
            "/api/domain",
            Some(json!({"strategy": "managed", "label": "acme", "target": "203.0.113.1"})),
        )
        .await;
    assert_eq!(s, 400);
    assert!(
        r["error"].as_str().unwrap().starts_with("Needs config"),
        "{r}"
    );
    let (s, _) = w
        .call(
            "POST",
            "/api/domain",
            Some(json!({"strategy": "custom", "domain": "localhost", "target": "127.0.0.1"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = w
        .call(
            "POST",
            "/api/release/start",
            Some(json!({"version": "0.1"})),
        )
        .await;
    assert_eq!(s, 400);
}
