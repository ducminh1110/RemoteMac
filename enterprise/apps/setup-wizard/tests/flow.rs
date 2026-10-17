//! End-to-end run of all nine steps against the local fake GitHub/Stripe/portal, asserting
//! that no credential ever appears in a response, the diagnostic log or the summary, and that
//! each credential is only sent to its own service.

mod common;
use common::*;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use taro_enterprise_setup::branding::fixtures;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_flow_and_no_secret_leaks() {
    let w = wizard().await;
    let base = w.fake.base();

    // 1. Preflight
    let (s, r) = w.call("POST", "/api/preflight/run", Some(json!({}))).await;
    assert_eq!(s, 200, "{r}");
    let checks = r["state"]["preflight"]["checks"]
        .as_array()
        .unwrap()
        .clone();
    let by = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap()["status"].clone();
    assert_eq!(by("github"), "ok");
    assert_eq!(by("stripe"), "ok");
    assert_eq!(by("loopback"), "ok");

    // 2. Deployment (development allows the fake's http:// URL)
    let (s, r) = w
        .call(
            "POST",
            "/api/deployment",
            Some(json!({
                "environment": "development", "profile": "single_server_compose",
                "portal_url": format!("{base}/portal/"), "ops_url": format!("{base}/ops"),
                "provider_adapter": "mock", "admin_email": "admin@example.com"
            })),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(
        r["state"]["deployment"]["portal_url"],
        format!("{base}/portal")
    );

    // 3. Identity
    let (s, r) = w
        .call(
            "POST",
            "/api/identity",
            Some(json!({
                "deployment_name": "Acme Remote Macs", "operator_name": "Acme Ltd",
                "support_contact": "support@example.com", "modules": ["billing", "my_macs", "team"]
            })),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(
        r["state"]["identity"]["modules"],
        json!(["my_macs", "billing", "team"])
    );

    // 4. Branding
    let (s, r) = w.upload("logo", fixtures::png(256, 128)).await;
    assert_eq!(s, 200, "{r}");
    let (s, _) = w.upload("favicon", fixtures::webp_vp8x(64, 64)).await;
    assert_eq!(s, 200);
    let (s, r) = w
        .call(
            "POST",
            "/api/branding",
            Some(json!({"primary_color": "#ffcc00", "theme": "dark", "density": "compact"})),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert!(
        r["state"]["branding"]["contrast_warning"].is_string(),
        "yellow on white must warn"
    );
    let (s, _) = w
        .call(
            "POST",
            "/api/branding",
            Some(json!({"primary_color": "#0a66c2", "theme": "light", "density": "comfortable"})),
        )
        .await;
    assert_eq!(s, 200);

    // 5. Domain
    let (s, r) = w
        .call("POST", "/api/domain", Some(json!({"strategy": "custom", "domain": "Portal.Example.com", "target": "203.0.113.10"})))
        .await;
    assert_eq!(s, 200, "{r}");
    let token = r["state"]["domain"]["verify_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        r["state"]["domain"]["records"][1]["name"],
        "_taro-verify.portal.example.com"
    );
    // Not yet in DNS -> verification fails honestly.
    let (_, r) = w.call("POST", "/api/domain/verify", Some(json!({}))).await;
    assert_eq!(r["state"]["domain"]["verification"]["dns_ok"], false);
    assert_eq!(r["state"]["steps"][4]["status"], "fail");
    w.dns.txt.lock().unwrap().insert(
        "_taro-verify.portal.example.com".into(),
        vec![format!("taro-verify={token}")],
    );
    w.dns.ips.lock().unwrap().insert(
        "portal.example.com".into(),
        vec!["203.0.113.10".parse().unwrap()],
    );
    let (_, r) = w.call("POST", "/api/domain/verify", Some(json!({}))).await;
    assert_eq!(r["state"]["domain"]["verification"]["dns_ok"], true, "{r}");
    assert_eq!(
        r["state"]["domain"]["verification"]["https_ok"], true,
        "{r}"
    );
    assert_eq!(r["state"]["steps"][4]["status"], "ok");

    // 6. Billing
    let (s, r) = w
        .call("POST", "/api/billing/key", Some(json!({"key": STRIPE_KEY})))
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["state"]["billing"]["account"]["id"], "acct_1Fake");
    assert_eq!(r["state"]["billing"]["mode"], "test");
    let (s, _) = w
        .call(
            "POST",
            "/api/billing/webhook/create",
            Some(json!({"confirm": false})),
        )
        .await;
    assert_eq!(s, 400, "webhook creation needs explicit confirmation");
    assert!(!w.fake.webhook_created.load(Ordering::SeqCst));
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/webhook/create",
            Some(json!({"confirm": true})),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["state"]["billing"]["webhook_secret_present"], true);
    assert_eq!(r["state"]["billing"]["webhook_secret_source"], "created");
    // Second create must not duplicate.
    let (s, r) = w
        .call(
            "POST",
            "/api/billing/webhook/create",
            Some(json!({"confirm": true})),
        )
        .await;
    assert_eq!(s, 200);
    assert!(r["state"]["billing"]["webhook_note"]
        .as_str()
        .unwrap()
        .contains("already exists"));
    let (s, r) = w
        .call("POST", "/api/billing/smoke", Some(json!({"confirm": true})))
        .await;
    assert_eq!(s, 200, "{r}");
    let smoke = &r["state"]["billing"]["smoke"];
    assert_eq!(smoke["ok"], true, "{smoke}");
    assert_eq!(smoke["session_id"], "cs_test_a1B2c3");
    assert_eq!(smoke["expired"], true);
    assert!(w
        .fake
        .reqs()
        .iter()
        .any(|q| q.path == "/stripe/v1/checkout/sessions/cs_test_a1B2c3/expire"));

    // 7. GitHub
    let (s, r) = w
        .call(
            "POST",
            "/api/github/pat",
            Some(json!({"token": format!("  {GH_TOKEN}\n")})),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    assert_eq!(r["message"], "Authenticated as octo.");
    let (s, r) = w.call("POST", "/api/github/orgs", Some(json!({}))).await;
    assert_eq!(s, 200);
    let orgs = r["state"]["github"]["orgs"].as_array().unwrap();
    assert_eq!(orgs[0]["can_create_repos"], true);
    assert_eq!(orgs[1]["can_create_repos"], false);
    // Bootstrap refuses without a reviewed plan / confirmation.
    let req = json!({"owner": "octo", "repo": "taro-prod", "template": "tmpl/template", "confirm": false});
    let (s, _) = w
        .call("POST", "/api/github/bootstrap", Some(req.clone()))
        .await;
    assert_eq!(s, 400);
    let mut req2 = req.clone();
    req2["confirm"] = json!(true);
    let (s, _) = w
        .call("POST", "/api/github/bootstrap", Some(req2.clone()))
        .await;
    assert_eq!(s, 409, "plan must be reviewed first");
    assert!(!w.fake.repo_created.load(Ordering::SeqCst));
    let (s, r) = w.call("POST", "/api/github/plan", Some(req.clone())).await;
    assert_eq!(s, 200, "{r}");
    let labels: Vec<String> = r["state"]["github"]["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["label"].as_str().unwrap().to_string())
        .collect();
    assert!(
        labels
            .iter()
            .any(|l| l.contains("PRIVATE repository octo/taro-prod")),
        "{labels:?}"
    );
    assert!(labels.iter().any(|l| l.contains("deploy/deployment.json")));
    assert!(labels
        .iter()
        .any(|l| l.contains("deploy/branding/logo.png")));
    assert!(labels
        .iter()
        .any(|l| l.contains("deploy/branding/favicon.webp")));
    let (s, r) = w
        .call("POST", "/api/github/bootstrap", Some(req2.clone()))
        .await;
    assert_eq!(s, 200, "{r}");
    let st = w
        .wait_for("bootstrap", |s| {
            !s["github"]["bootstrap_running"].as_bool().unwrap()
        })
        .await;
    assert_eq!(st["github"]["bootstrap_done"], true, "{}", st["github"]);
    for a in st["github"]["plan"].as_array().unwrap() {
        assert_eq!(a["status"], "ok", "{a}");
    }

    // Secrets were sealed to the repository key and decrypt to the right values.
    let secrets = w.fake.secrets.lock().unwrap().clone();
    let open = |name: &str| {
        let (ct, kid) = &secrets[name];
        assert_eq!(kid, "kid-1");
        String::from_utf8(
            w.fake
                .sk
                .unseal(&taro_enterprise_setup::util::b64_decode(ct).unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(open("STRIPE_SECRET_KEY"), STRIPE_KEY);
    assert_eq!(open("STRIPE_WEBHOOK_SECRET"), CREATED_WHSEC);
    let taro_key = open("TARO_SECRET_KEY");
    assert_eq!(taro_key.len(), 64);
    assert!(taro_key.chars().all(|c| c.is_ascii_hexdigit()));

    // Committed files: config without secrets, branding bytes as uploaded.
    let contents = w.fake.contents.lock().unwrap().clone();
    let dep: Value = serde_json::from_slice(&contents["deploy/deployment.json"]).unwrap();
    assert_eq!(dep["deployment_name"], "Acme Remote Macs");
    assert_eq!(dep["domain"]["name"], "portal.example.com");
    assert_eq!(dep["branding"]["logo"], "deploy/branding/logo.png");
    assert_eq!(
        contents["deploy/branding/logo.png"],
        fixtures::png(256, 128)
    );
    assert_eq!(
        contents["deploy/branding/favicon.webp"],
        fixtures::webp_vp8x(64, 64)
    );

    // 8. Release: the release is shown only once it exists AND has an asset.
    let (s, r) = w
        .call(
            "POST",
            "/api/release/start",
            Some(json!({"version": "0.1.0"})),
        )
        .await;
    assert_eq!(s, 200, "{r}");
    let (s, _) = w
        .call(
            "POST",
            "/api/release/start",
            Some(json!({"version": "0.1.0"})),
        )
        .await;
    assert_eq!(s, 409, "no second dispatch while one is followed");
    let mut saw_no_asset_phase = false;
    let st = loop {
        let st = w.state().await;
        let rel = &st["release"];
        if !rel["release"].is_null() {
            assert!(
                !rel["release"]["assets"].as_array().unwrap().is_empty(),
                "release shown without assets"
            );
            assert!(w.fake.release_visible_polls.load(Ordering::SeqCst) >= 1);
        }
        if rel["phase"]
            .as_str()
            .unwrap_or("")
            .contains("no assets yet")
        {
            saw_no_asset_phase = true;
        }
        if !rel["running"].as_bool().unwrap() {
            break st;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(st["release"]["error"].is_null(), "{}", st["release"]);
    assert_eq!(
        st["release"]["release"]["assets"][0]["name"],
        "taro-enterprise-0.1.0.tar.gz"
    );
    assert!(
        w.fake.release_polls.load(Ordering::SeqCst) >= 4,
        "release existed without assets first"
    );
    assert!(saw_no_asset_phase || w.fake.release_polls.load(Ordering::SeqCst) >= 4);
    assert_eq!(st["release"]["run"]["id"], 2);
    let tail = st["release"]["log_tail"].as_str().unwrap();
    assert!(tail.contains("[REDACTED]"), "{tail}");
    assert!(!tail.contains(LOG_LOOKALIKE), "{tail}");
    assert!(!tail.contains("abcdefghijklmnopqrstuvwxyz"), "{tail}");
    assert!(!tail.contains(STRIPE_KEY), "{tail}");
    assert_eq!(st["steps"][7]["status"], "ok");

    // 9. Verification + exports
    let (s, r) = w.call("POST", "/api/verify/run", Some(json!({}))).await;
    assert_eq!(s, 200);
    for c in r["state"]["verification"]["checks"].as_array().unwrap() {
        assert_eq!(c["status"], "ok", "{c}");
    }
    let prov = r["state"]["verification"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "providers")
        .unwrap()
        .clone();
    assert_eq!(prov["detail"], "Enabled: password, github");
    let (s, summary_json) = w.call("GET", "/api/summary.json", None).await;
    assert_eq!(s, 200);
    assert_eq!(
        summary_json["github"]["repository"]["full_name"],
        "octo/taro-prod"
    );
    let (s, md) = w.call("GET", "/api/summary.md", None).await;
    assert_eq!(s, 200);
    assert!(md.as_str().unwrap().contains("octo/taro-prod (private)"));
    let (s, diag) = w.call("GET", "/api/diagnostics.txt", None).await;
    assert_eq!(s, 200);
    let diag = diag.as_str().unwrap().to_string();
    assert!(diag.contains("authenticated as octo"));

    // Delete: wrong confirmation is refused; GitHub's 403 is explained.
    let (s, _) = w
        .call(
            "POST",
            "/api/github/delete-repo",
            Some(json!({"confirm_name": "octo/other"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, r) = w
        .call(
            "POST",
            "/api/github/delete-repo",
            Some(json!({"confirm_name": "octo/taro-prod"})),
        )
        .await;
    assert_eq!(s, 403);
    assert!(r["error"].as_str().unwrap().contains("delete_repo"), "{r}");

    // ---- Leak checks -------------------------------------------------------------------
    let secrets_list = [
        GH_TOKEN,
        STRIPE_KEY,
        CREATED_WHSEC,
        taro_key.as_str(),
        ACCESS,
        LOG_LOOKALIKE,
    ];
    let responses = w.responses.lock().unwrap().clone();
    assert!(responses.len() > 30);
    for (i, body) in responses.iter().enumerate() {
        for sec in secrets_list {
            assert!(
                !body.contains(sec),
                "response #{i} leaks a secret: {}",
                &body[..body.len().min(300)]
            );
        }
    }
    let log_text = w.app.log.export_text();
    // The response scrubber is only a safety net: it must never have had to act.
    assert!(
        !log_text.contains("was scrubbed"),
        "a handler tried to return a secret"
    );
    for sec in secrets_list {
        assert!(!log_text.contains(sec), "diagnostic log leaks a secret");
        assert!(!diag.contains(sec));
    }
    // Each credential went only to its own service; the log download had no credential.
    for q in w.fake.reqs() {
        let a = q.auth.clone().unwrap_or_default();
        if a.contains(GH_TOKEN) {
            assert!(
                q.path.starts_with("/gh/"),
                "GitHub token sent to {}",
                q.path
            );
        }
        if a.contains(STRIPE_KEY) {
            assert!(
                q.path.starts_with("/stripe/"),
                "Stripe key sent to {}",
                q.path
            );
        }
        if q.path.starts_with("/blob/")
            || q.path.starts_with("/portal/")
            || q.path.starts_with("/web/")
        {
            assert!(q.auth.is_none(), "credential sent to {}", q.path);
        }
        assert!(
            !q.path.contains(GH_TOKEN) && !q.path.contains(STRIPE_KEY),
            "secret in a URL"
        );
        if !q.path.contains("/actions/secrets/") && !q.path.starts_with("/stripe/") {
            assert!(
                !q.body.contains(GH_TOKEN),
                "GitHub token in a request body to {}",
                q.path
            );
        }
        if !q.path.starts_with("/stripe/") {
            assert!(
                !q.body.contains(STRIPE_KEY),
                "Stripe key in plaintext to {}",
                q.path
            );
            assert!(
                !q.body.contains(CREATED_WHSEC),
                "webhook secret in plaintext to {}",
                q.path
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn template_that_is_not_a_template_fails_clearly() {
    let w = wizard().await;
    w.fake.template_is_template.store(false, Ordering::SeqCst);
    let base = w.fake.base();
    w.call("POST", "/api/deployment", Some(json!({"environment": "development", "profile": "single_server_compose", "portal_url": format!("{base}/portal"), "ops_url": format!("{base}/ops"), "provider_adapter": "mock", "admin_email": "a@example.com"}))).await;
    w.call("POST", "/api/identity", Some(json!({"deployment_name": "X", "operator_name": "Y", "support_contact": "s@example.com", "modules": ["my_macs"]}))).await;
    let (s, _) = w
        .call("POST", "/api/github/pat", Some(json!({"token": GH_TOKEN})))
        .await;
    assert_eq!(s, 200);
    let req =
        json!({"owner": "octo", "repo": "taro-x", "template": "tmpl/template", "confirm": true});
    let (s, r) = w.call("POST", "/api/github/plan", Some(req.clone())).await;
    assert_eq!(s, 200);
    // Billing disabled -> Stripe secrets are planned as skipped, not faked.
    let plan = r["state"]["github"]["plan"].as_array().unwrap();
    let sk = plan
        .iter()
        .find(|a| a["id"] == "secret:STRIPE_SECRET_KEY")
        .unwrap();
    assert_eq!(sk["status"], "skipped");
    let (s, _) = w.call("POST", "/api/github/bootstrap", Some(req)).await;
    assert_eq!(s, 200);
    let st = w
        .wait_for("bootstrap", |s| {
            !s["github"]["bootstrap_running"].as_bool().unwrap()
        })
        .await;
    assert_eq!(st["github"]["bootstrap_done"], false);
    let err = st["github"]["bootstrap_error"].as_str().unwrap();
    assert!(err.contains("not marked as a template"), "{err}");
    assert!(
        !w.fake.repo_created.load(Ordering::SeqCst),
        "must not generate from a non-template"
    );
    assert_eq!(st["steps"][6]["status"], "fail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_github_token_is_rejected_and_not_kept() {
    let w = wizard().await;
    let bad = concat!(
        "gi",
        "thub_pat_11WRONG000_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
    );
    let (s, r) = w
        .call("POST", "/api/github/pat", Some(json!({"token": bad})))
        .await;
    assert_eq!(s, 502);
    assert!(r["error"].as_str().unwrap().contains("401"), "{r}");
    let st = w.state().await;
    assert!(st["github"]["user"].is_null());
    for body in w.responses.lock().unwrap().iter() {
        assert!(!body.contains(bad));
    }
    assert!(!w.app.log.export_text().contains(bad));
}
