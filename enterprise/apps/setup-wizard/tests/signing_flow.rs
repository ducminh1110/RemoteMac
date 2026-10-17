//! Step 5 against the fake GitHub: one token, and Start does the rest. It makes the operator's
//! copy of the repository (a fork, Actions turned on), the signing key (sealed into the Actions
//! secret, public half committed, backed up), starts the release workflow and follows it (the
//! fake Actions sign the release with the secret, as the real workflow does), checks the
//! release's signature and builds and validates the packages. Run again it reuses all of it.
//! The private key and the token are never in a response, the log, the state, the summary, or
//! any request body except sealed.

mod common;
use common::*;
use serde_json::{json, Value};
use taro_update::sig;

async fn connect(w: &Wiz, repo: &str) -> (axum::http::StatusCode, Value) {
    w.post(
        "/api/signing/connect",
        json!({ "token": GH_TOKEN, "repo": repo }),
    )
    .await
}

async fn wait_signing(w: &Wiz) -> Value {
    for _ in 0..3000 {
        let s = w.state().await;
        let g = &s["signing"];
        if !g["running"].as_bool().unwrap_or(false)
            && (g["done"].as_bool().unwrap_or(false) || !g["error"].is_null())
        {
            return s;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("Start did not finish: {}", w.state().await["signing"]);
}

fn statuses(st: &Value) -> Vec<(String, String)> {
    st["signing"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().into(),
                i["status"].as_str().unwrap().into(),
            )
        })
        .collect()
}

fn all(ids: &[(&str, &str)]) -> Vec<(String, String)> {
    ids.iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

fn opened_secret(f: &Fake, repo: &str) -> String {
    let mut g = f.gh.lock().unwrap();
    let r = g.repo(repo);
    let raw =
        taro_enterprise_setup::util::b64_decode(&r.secrets["TARO_ENTERPRISE_SIGNING_KEY"]).unwrap();
    String::from_utf8(
        r.secrets_key
            .unseal(&raw)
            .expect("sealed for the repository key"),
    )
    .unwrap()
}

fn no_leaks(w: &Wiz, state: &Value, secret: &str) {
    let seed = secret.split(':').next_back().unwrap().to_string();
    let responses = w.responses.lock().unwrap().join("\n");
    let bodies = w.fake.gh.lock().unwrap().bodies.join("\n");
    for (what, text) in [
        ("responses", responses),
        ("log", w.app.log.export_text()),
        ("state", state.to_string()),
        ("fake request bodies", bodies),
    ] {
        assert!(!text.contains(&seed), "the private key is in the {what}");
        assert!(!text.contains(GH_TOKEN), "the token is in the {what}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_token_does_everything_and_again_reuses_it() {
    let w = wizard("signing-auto").await;
    w.fill().await;

    // a classic token without the workflow scope is refused, with the link that makes one
    let (s, v) = w
        .post(
            "/api/signing/connect",
            json!({ "token": GH_TOKEN_NO_WORKFLOW, "repo": "" }),
        )
        .await;
    assert_eq!(s, 400);
    let e = v["error"].as_str().unwrap();
    assert!(
        e.contains("workflow") && e.contains("settings/tokens/new?scopes=repo,workflow"),
        "{e}"
    );

    // the token alone: the copy does not exist yet, Start will make it
    let (s, v) = connect(&w, "").await;
    assert_eq!(s, 200, "{v}");
    let st = w.state().await;
    assert_eq!(st["signing"]["login"], "octo");
    assert_eq!(st["signing"]["fork_to"], FORK);
    assert_eq!(st["signing"]["version"], "0.3.0");
    assert!(
        !w.fake.gh.lock().unwrap().repos.contains_key(FORK),
        "connect changes nothing"
    );

    let (s, v) = w.post("/api/signing/start", json!({})).await;
    assert_eq!(s, 200, "{v}");
    let st = wait_signing(&w).await;
    assert_eq!(st["signing"]["done"], true, "{}", st["signing"]);
    assert_eq!(
        statuses(&st),
        all(&[
            ("fork", "ok"),
            ("actions", "ok"),
            ("key", "ok"),
            ("secret", "ok"),
            ("pub", "ok"),
            ("backup", "ok"),
            ("perms", "ok"),
            ("release", "ok"),
            ("verify", "ok"),
            ("build", "ok"),
        ])
    );

    // the copy: a fork of the Taro repository with Actions on, holding the new key
    let secret = opened_secret(&w.fake, FORK);
    let k = sig::parse_secret(&secret).unwrap();
    let public = sig::public_text(&k.verifying_key());
    {
        let mut g = w.fake.gh.lock().unwrap();
        let f = g.repo(FORK);
        assert_eq!(f.parent.as_deref(), Some(REPO));
        assert!(f.actions);
        // the copy has no releases of its own: its relays trust only the operator's key
        assert_eq!(
            taro_enterprise_setup::signing::keys_in(&f.files[KEYS_PATH].0),
            vec![public.clone()]
        );
        assert_eq!(f.dispatches.len(), 1);
        assert_eq!(f.dispatches[0]["ref"], "main");
        assert_eq!(f.dispatches[0]["inputs"]["version"], "0.3.0");
        // the Taro repository itself is untouched
        let up = g.repo(REPO);
        assert_eq!(up.secret_puts + up.file_puts, 0);
        assert!(up.dispatches.is_empty());
    }
    assert_eq!(st["signing"]["repo"]["full_name"], FORK);
    assert_eq!(st["signing"]["key_public"], public);

    // Steps 6-8 ran on the release of the copy: signature checked, packages built and valid
    assert_eq!(st["components"]["selected"]["tag"], "enterprise-v0.3.0");
    assert!(!st["build"]["result"].is_null(), "{}", st["build"]);
    assert_eq!(st["validation"]["ok"], true);
    assert!(
        w.fake.reqs().iter().any(|r| r.starts_with(&format!(
            "/gh-web/{FORK}/releases/download/enterprise-v0.3.0/taro-relay-"
        ))),
        "the relays come from the copy's release"
    );
    let backup = std::fs::read_to_string(st["signing"]["backup_path"].as_str().unwrap()).unwrap();
    assert_eq!(backup.trim(), secret);
    no_leaks(&w, &st, &secret);
    let (_, summary) = w.call("GET", "/api/summary.json", None).await;
    assert!(!summary
        .to_string()
        .contains(secret.split(':').next_back().unwrap()));

    // Start again: the copy is synced, the key kept, the release used as it is
    assert_eq!(connect(&w, "").await.0, 200);
    assert!(w.state().await["signing"]["fork_to"].is_null());
    assert_eq!(w.post("/api/signing/start", json!({})).await.0, 200);
    let st = wait_signing(&w).await;
    assert_eq!(
        statuses(&st),
        all(&[
            ("sync", "ok"),
            ("key", "skipped"),
            ("secret", "skipped"),
            ("pub", "skipped"),
            ("backup", "skipped"),
            ("perms", "ok"),
            ("release", "skipped"),
            ("verify", "ok"),
            ("build", "ok"),
        ])
    );
    {
        let mut g = w.fake.gh.lock().unwrap();
        assert_eq!(g.forks, 1);
        let f = g.repo(FORK);
        assert_eq!((f.secret_puts, f.dispatches.len(), f.syncs), (1, 1, 1));
    }

    // a new Taro version upstream; the next session on this PC (same backup) releases it
    w.fake.gh.lock().unwrap().repo(REPO).files.insert(
        "enterprise/Cargo.toml".into(),
        (
            "[workspace.package]\nversion = \"0.3.1\"\n".into(),
            "sha-cargo-1".into(),
        ),
    );
    let w2 = wizard_sharing("signing-auto", &w).await;
    w2.fill().await;
    assert_eq!(connect(&w2, "").await.0, 200);
    let st = w2.state().await;
    assert_eq!(st["signing"]["key_source"], "the backup on this PC");
    assert_eq!(w2.post("/api/signing/start", json!({})).await.0, 200);
    let st = wait_signing(&w2).await;
    assert_eq!(st["signing"]["done"], true, "{}", st["signing"]);
    let s = statuses(&st);
    assert_eq!(s[1], ("key".into(), "skipped".into()), "{s:?}");
    assert_eq!(s[6], ("release".into(), "ok".into()), "{s:?}");
    assert_eq!(st["components"]["selected"]["tag"], "enterprise-v0.3.1");
    assert_eq!(st["validation"]["ok"], true);
    let mut g = w.fake.gh.lock().unwrap();
    let f = g.repo(FORK);
    assert_eq!(f.secret_puts, 1, "the secret was not replaced");
    assert_eq!(f.dispatches.len(), 2);
    drop(g);
    assert_eq!(opened_secret(&w.fake, FORK), secret);
}

#[tokio::test]
async fn the_owner_releases_from_the_repository_itself() {
    let w = wizard("signing-owner").await;
    let (s, v) = connect(&w, REPO).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(w.state().await["signing"]["repo_keys"], json!([OLD_KEY]));
    let (s, v) = w
        .post("/api/signing/start", json!({ "build": false }))
        .await;
    assert_eq!(s, 200, "{v}");
    let st = wait_signing(&w).await;
    assert_eq!(
        statuses(&st),
        all(&[
            ("key", "ok"),
            ("secret", "ok"),
            ("pub", "ok"),
            ("backup", "ok"),
            ("perms", "ok"),
            ("release", "ok"),
        ])
    );
    let secret = opened_secret(&w.fake, REPO);
    let k = sig::parse_secret(&secret).unwrap();
    let public = sig::public_text(&k.verifying_key());
    // releases exist, so the old key stays trusted next to the new one
    let keys = w.fake.gh.lock().unwrap().repo(REPO).files[KEYS_PATH]
        .0
        .clone();
    assert_eq!(
        taro_enterprise_setup::signing::keys_in(&keys),
        vec![OLD_KEY.to_string(), public]
    );
    assert_eq!(w.fake.gh.lock().unwrap().forks, 0);
    // Step 6 trusts the repository's new key (and still the wizard's own)
    let signature = sig::sign(&k, b"m");
    assert!(taro_enterprise_setup::signing::repo_trust(&w.app)
        .verify(b"m", &signature)
        .is_ok());
    assert!(w.app.keys.verify(b"m", &signature).is_err());
    no_leaks(&w, &w.state().await, &secret);
}

#[tokio::test]
async fn an_unknown_secret_is_kept_and_a_failed_run_is_reported() {
    let w = wizard("signing-keep").await;
    w.fake.gh.lock().unwrap().repo(REPO).secrets.insert(
        "TARO_ENTERPRISE_SIGNING_KEY".into(),
        "set-by-someone-else".into(),
    );
    assert_eq!(connect(&w, REPO).await.0, 200);

    // building needs Steps 2-4 first
    let (s, v) = w.post("/api/signing/start", json!({})).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("untick"), "{v}");

    assert_eq!(
        w.post("/api/signing/start", json!({ "build": false }))
            .await
            .0,
        200
    );
    let st = wait_signing(&w).await;
    let s = statuses(&st);
    assert_eq!(s[0], ("key".into(), "skipped".into()));
    assert_eq!(w.fake.gh.lock().unwrap().repo(REPO).secret_puts, 0);
    // the workflow cannot sign with that secret: the run fails, with its link
    let e = st["signing"]["error"].as_str().unwrap();
    assert!(e.contains("failure") && e.contains("/actions/runs/"), "{e}");
    assert_eq!(s[4], ("perms".into(), "ok".into()));
    assert_eq!(s[5], ("release".into(), "fail".into()));
}

#[tokio::test]
async fn a_refused_secret_is_added_by_hand_and_start_goes_on_with_the_same_key() {
    let w = wizard("signing-manual").await;
    w.fake.gh.lock().unwrap().repo(REPO).refuse_secrets = true;
    assert_eq!(connect(&w, REPO).await.0, 200);

    // no key waits to be added: nothing to copy
    assert_eq!(w.post("/api/signing/manual-key", json!({})).await.0, 400);

    let start = json!({ "build": false });
    assert_eq!(w.post("/api/signing/start", start.clone()).await.0, 200);
    let st = wait_signing(&w).await;
    let s = statuses(&st);
    assert_eq!(s[0], ("key".into(), "ok".into()));
    assert_eq!(s[1], ("secret".into(), "manual".into()));
    assert_eq!(st["signing"]["manual_secret"], true);
    let e = st["signing"]["error"].as_str().unwrap();
    assert!(
        e.contains("Add it yourself") && e.contains("/settings/secrets/actions/new"),
        "{e}"
    );
    let public = st["signing"]["items"][0]["detail"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        w.fake.gh.lock().unwrap().repo(REPO).dispatches.is_empty(),
        "no release without the secret"
    );

    // the key for the operator to paste, and only it
    let (c, v) = w.post("/api/signing/manual-key", json!({})).await;
    assert_eq!(c, 200, "{v}");
    assert_eq!(v["name"], "TARO_ENTERPRISE_SIGNING_KEY");
    assert_eq!(v["repo"], REPO);
    let key = v["key"].as_str().unwrap().to_string();
    let k = sig::parse_secret(&key).unwrap();
    assert_eq!(sig::public_text(&k.verifying_key()), public);

    // Start again before it is added: GitHub refuses again, the same key still waits
    assert_eq!(w.post("/api/signing/start", start.clone()).await.0, 200);
    let st = wait_signing(&w).await;
    assert_eq!(statuses(&st)[1], ("secret".into(), "manual".into()));
    assert!(st["signing"]["items"][0]["detail"]
        .as_str()
        .unwrap()
        .starts_with(&public));

    // the operator adds it in GitHub's settings
    {
        let mut g = w.fake.gh.lock().unwrap();
        let r = g.repo(REPO);
        let sealed = taro_enterprise_setup::util::b64(
            &r.secrets_key
                .public_key()
                .seal(&mut crypto_box::aead::OsRng, key.as_bytes())
                .unwrap(),
        );
        r.secrets
            .insert("TARO_ENTERPRISE_SIGNING_KEY".into(), sealed);
        r.secret_changes += 1;
    }
    assert_eq!(w.post("/api/signing/start", start).await.0, 200);
    let st = wait_signing(&w).await;
    assert_eq!(st["signing"]["done"], true, "{}", st["signing"]);
    assert_eq!(
        statuses(&st),
        all(&[
            ("key", "ok"),
            ("secret", "ok"),
            ("pub", "ok"),
            ("backup", "ok"),
            ("perms", "ok"),
            ("release", "ok"),
        ])
    );
    assert_eq!(st["signing"]["manual_secret"], false);
    assert_eq!(st["signing"]["key_public"], public.as_str());
    assert_eq!(opened_secret(&w.fake, REPO), key);
    let (puts, perms) = {
        let g = w.fake.gh.lock().unwrap();
        (
            g.repos[REPO].secret_puts,
            g.repos[REPO].workflow_permissions.clone(),
        )
    };
    assert_eq!(puts, 0);
    assert_eq!(perms.as_deref(), Some("write"));
    // the key cannot be copied any more
    assert_eq!(w.post("/api/signing/manual-key", json!({})).await.0, 400);
}
