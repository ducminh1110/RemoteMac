//! Step 5 against the fake GitHub: Start makes the signing key, seals it into the Actions
//! secret, commits its public half next to the keys relays already trust, backs it up, starts
//! the release workflow and follows it to the end; a second session reuses the key; the private
//! key is never in a response, the log, the summary, or any request body except sealed.

mod common;
use common::*;
use serde_json::{json, Value};
use taro_update::sig;

async fn connect(w: &Wiz) -> (axum::http::StatusCode, Value) {
    w.post(
        "/api/signing/connect",
        json!({ "token": GH_TOKEN, "repo": REPO }),
    )
    .await
}

async fn wait_signing(w: &Wiz) -> Value {
    w.wait_for("signing to finish", |s| {
        let g = &s["signing"];
        !g["running"].as_bool().unwrap_or(false)
            && (g["done"].as_bool().unwrap_or(false) || !g["error"].is_null())
    })
    .await
}

fn opened_secret(f: &Fake) -> String {
    let g = f.gh.lock().unwrap();
    let sealed = &g.secrets["TARO_ENTERPRISE_SIGNING_KEY"];
    let raw = taro_enterprise_setup::util::b64_decode(sealed).unwrap();
    String::from_utf8(
        g.secrets_key
            .unseal(&raw)
            .expect("sealed for the repository key"),
    )
    .unwrap()
}

#[tokio::test]
async fn start_sets_up_signing_and_releases() {
    let w = wizard("signing-start").await;

    // a bad token is refused and not kept
    let (s, v) = w
        .post(
            "/api/signing/connect",
            json!({ "token": "nope-not-a-token", "repo": REPO }),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert!(w.state().await["signing"]["login"].is_null());

    let (s, v) = connect(&w).await;
    assert_eq!(s, 200, "{v}");
    let st = w.state().await;
    assert_eq!(st["signing"]["login"], "octo");
    assert_eq!(st["signing"]["has_secret"], false);
    assert_eq!(st["signing"]["version"], "0.3.0");
    assert_eq!(st["signing"]["repo_keys"], json!([OLD_KEY]));

    // the workflow releases Cargo.toml's version; an existing tag is refused
    let (s, v) = w
        .post("/api/signing/start", json!({ "version": "0.4.0" }))
        .await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("0.3.0"), "{v}");

    let (s, v) = w
        .post("/api/signing/start", json!({ "version": "0.3.0" }))
        .await;
    assert_eq!(s, 200, "{v}");
    let st = wait_signing(&w).await;
    let g = &st["signing"];
    assert_eq!(g["done"], true, "{g}");
    let statuses: Vec<(String, String)> = g["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().into(),
                i["status"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        ["key", "secret", "pub", "backup", "dispatch", "run"]
            .iter()
            .map(|i| (i.to_string(), "ok".to_string()))
            .collect::<Vec<_>>()
    );

    // the secret opens (only with the repository's key) to a key whose public half was committed
    let secret = opened_secret(&w.fake);
    let k = sig::parse_secret(&secret).unwrap();
    let public = sig::public_text(&k.verifying_key());
    assert_eq!(g["key_public"], public);
    let (keys_text, _) = w.fake.gh.lock().unwrap().files[KEYS_PATH].clone();
    // releases exist, so the old key stays trusted next to the new one
    assert_eq!(
        taro_enterprise_setup::signing::keys_in(&keys_text),
        vec![OLD_KEY.to_string(), public.clone()]
    );

    // the workflow was started with the version, and the run followed to its end
    let d = w.fake.gh.lock().unwrap().dispatches.clone();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["ref"], "main");
    assert_eq!(d[0]["inputs"]["version"], "0.3.0");
    assert_eq!(g["run"]["conclusion"], "success");

    // the backup on this PC holds the same key
    let backup = std::fs::read_to_string(g["backup_path"].as_str().unwrap()).unwrap();
    assert_eq!(backup.trim(), secret);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(g["backup_path"].as_str().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o077, 0, "the backup is readable by its owner only");
    }

    // Step 6 trusts the repository's new key (and still the wizard's own)
    let bytes = b"a component manifest";
    let signature = sig::sign(&k, bytes);
    assert!(taro_enterprise_setup::signing::repo_trust(&w.app)
        .verify(bytes, &signature)
        .is_ok());
    assert!(w.app.keys.verify(bytes, &signature).is_err());

    // the private key and the token appear nowhere they should not
    let seed = secret.split(':').next_back().unwrap().to_string();
    let state = w.state().await.to_string();
    let responses = w.responses.lock().unwrap().join("\n");
    let bodies = w.fake.gh.lock().unwrap().bodies.join("\n");
    for (what, text) in [
        ("responses", responses),
        ("log", w.app.log.export_text()),
        ("state", state),
        ("fake request bodies", bodies),
    ] {
        assert!(!text.contains(&seed), "the private key is in the {what}");
        assert!(!text.contains(GH_TOKEN), "the token is in the {what}");
    }
    let (_, summary) = w.call("GET", "/api/summary.json", None).await;
    assert!(!summary.to_string().contains(&seed));

    // a second release of the same version is refused (the tag exists now)
    let (s, _) = connect(&w).await;
    assert_eq!(s, 200);
    let (s, v) = w
        .post("/api/signing/start", json!({ "version": "0.3.0" }))
        .await;
    assert_eq!(s, 400);
    assert!(
        v["error"].as_str().unwrap().contains("exists already"),
        "{v}"
    );
}

#[tokio::test]
async fn a_new_session_reuses_the_key() {
    let w = wizard("signing-reuse").await;
    assert_eq!(connect(&w).await.0, 200);
    assert_eq!(
        w.post("/api/signing/start", json!({ "version": "0.3.0" }))
            .await
            .0,
        200
    );
    assert_eq!(wait_signing(&w).await["signing"]["done"], true);
    let first = opened_secret(&w.fake);

    // the next release, from a fresh wizard on the same PC (same backup folder)
    {
        let mut g = w.fake.gh.lock().unwrap();
        g.files.insert(
            "enterprise/Cargo.toml".into(),
            (
                "[workspace.package]\nversion = \"0.3.1\"\n".into(),
                "sha-cargo-1".into(),
            ),
        );
    }
    let w2 = wizard_sharing("signing-reuse", &w).await;
    assert_eq!(connect(&w2).await.0, 200);
    let st = w2.state().await;
    assert_eq!(st["signing"]["has_secret"], true);
    assert_eq!(st["signing"]["key_source"], "the backup on this PC");
    assert_eq!(
        w2.post("/api/signing/start", json!({ "version": "0.3.1" }))
            .await
            .0,
        200
    );
    let st = wait_signing(&w2).await;
    let items = st["signing"]["items"].as_array().unwrap().clone();
    assert_eq!(items[0]["id"], "key");
    assert_eq!(items[0]["status"], "skipped", "{items:?}");
    let g = w.fake.gh.lock().unwrap();
    assert_eq!(g.secret_puts, 1, "the secret was not replaced");
    assert_eq!(g.dispatches.len(), 2);
    drop(g);
    assert_eq!(opened_secret(&w.fake), first);
}

#[tokio::test]
async fn an_existing_unknown_secret_is_kept_unless_asked() {
    let w = wizard("signing-keep").await;
    w.fake.gh.lock().unwrap().secrets.insert(
        "TARO_ENTERPRISE_SIGNING_KEY".into(),
        "set-by-someone-else".into(),
    );
    assert_eq!(connect(&w).await.0, 200);
    assert_eq!(
        w.post("/api/signing/start", json!({ "version": "0.3.0" }))
            .await
            .0,
        200
    );
    let st = wait_signing(&w).await;
    assert_eq!(st["signing"]["items"][0]["status"], "skipped");
    assert_eq!(w.fake.gh.lock().unwrap().secret_puts, 0);

    // a failing run is reported with its link
    let w = wizard("signing-fail").await;
    w.fake.gh.lock().unwrap().run_conclusion = "failure".into();
    assert_eq!(connect(&w).await.0, 200);
    assert_eq!(
        w.post(
            "/api/signing/start",
            json!({ "version": "0.3.0", "new_key": true })
        )
        .await
        .0,
        200
    );
    let st = wait_signing(&w).await;
    let e = st["signing"]["error"].as_str().unwrap();
    assert!(e.contains("failure") && e.contains("/actions/runs/"), "{e}");
}
