//! Wizard 2 against a fake machine: the guard, a first run, a second run that changes settings
//! without resetting anything, a failure midway, the real binary's first-launch behaviour and
//! health check, and an Enterprise update of the core pages that keeps the provider's branding.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use taro_relay::checks::{Fut, Net};
use taro_relay::config::{Runtime, UpdateMode};
use taro_relay::setup::{self, Probe, Setup};
use taro_relay::system::Fake;
use taro_update::policy::Layout;
use tower::ServiceExt;

const PORT: u16 = 47480;

struct FakeNet;
impl Net for FakeNet {
    fn resolve<'a>(&'a self, host: &'a str) -> Fut<'a, Result<Vec<IpAddr>, String>> {
        Box::pin(async move {
            Ok(if host == "relay.acme.example" {
                vec!["203.0.113.7".parse().unwrap()]
            } else {
                vec![]
            })
        })
    }
    fn public_ips(&self) -> Fut<'_, Vec<IpAddr>> {
        Box::pin(async { vec!["203.0.113.7".parse().unwrap()] })
    }
    fn reach<'a>(&'a self, _: &'a str) -> Fut<'a, Result<u16, String>> {
        Box::pin(async { Ok(200) })
    }
}

struct OkProbe;
impl Probe for OkProbe {
    fn health(&self, port: u16, v: &str) -> Result<String, String> {
        Ok(format!("{v} on {port}"))
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("taro-wizard2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn provider(relay_port: u16) -> rm_brand::Provider {
    rm_brand::Provider {
        schema: rm_brand::SCHEMA.into(),
        provider_id: "acme-cloud".into(),
        product: rm_brand::Product {
            name: "Acme Connect".into(),
            short_name: "Acme".into(),
            vendor: "Acme Cloud Ltd".into(),
            support_url: None,
            support_email: Some("help@acme.example".into()),
        },
        branding: rm_brand::Branding {
            primary_color: "#0F766E".into(),
            tokens: None,
            theme: rm_brand::Theme::System,
            logo: Some("logo.png".into()),
            icon: None,
        },
        relay: rm_brand::Relay {
            host: "relay.acme.example".into(),
            port: relay_port,
            key: "acme-admission-key-0123456789".into(),
        },
        account: None,
        updates: Default::default(),
        generated: None,
    }
    .normalized()
}

/// An installed relay (core recorded) with the provider folder from Wizard 1.
fn installed(base: &Path, relay_port: u16) -> Layout {
    let l = Layout::under(base);
    for rel in [
        "setup/index.html",
        "setup/app.css",
        "setup/app.js",
        "status/index.html",
        "status/app.css",
        "status/app.js",
    ] {
        let p = l.core().join("web/relay").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, taro_relay::web::embedded(rel).unwrap()).unwrap();
    }
    std::fs::create_dir_all(l.core().join("bin")).unwrap();
    std::fs::write(l.core().join("bin/taro-relay"), b"binary 0.1.0").unwrap();
    taro_update::apply::record_fresh_install(
        &l,
        "relay",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        "t0",
    )
    .unwrap();
    std::fs::create_dir_all(&l.provider).unwrap();
    std::fs::write(
        l.provider.join("provider.json"),
        provider(relay_port).to_json(),
    )
    .unwrap();
    std::fs::write(l.provider.join("logo.png"), b"\x89PNG\r\n\x1a\nacme logo").unwrap();
    l
}

fn setup_for(l: &Layout, sys: Arc<Fake>) -> Arc<Setup> {
    Arc::new(Setup {
        layout: l.clone(),
        sys,
        net: Arc::new(FakeNet),
        probe: Arc::new(OkProbe),
        token: "t0k3n".into(),
        port: PORT,
        draft: Mutex::new(Default::default()),
        challenges: Default::default(),
        log: Mutex::new(vec![]),
        done: Default::default(),
        root_override: None,
    })
}

async fn call(
    s: &Arc<Setup>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("127.0.0.1:{PORT}"))
        .header("x-setup-token", "t0k3n");
    if method != "GET" {
        b = b
            .header("origin", format!("http://127.0.0.1:{PORT}"))
            .header("content-type", "application/json");
    }
    let req = b
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let r = setup::router(s.clone()).oneshot(req).await.unwrap();
    let (status, headers) = (r.status(), r.headers().clone());
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into())),
        headers,
    )
}

#[tokio::test]
async fn the_setup_page_is_guarded() {
    let base = tmp("guard");
    let l = installed(&base, free_port());
    let s = setup_for(&l, Arc::new(Fake::new(base.join("units"))));
    let send = |req: Request<Body>| {
        let s = s.clone();
        async move { setup::router(s).oneshot(req).await.unwrap() }
    };
    // DNS rebinding: another Host
    let r = send(
        Request::get("/api/state")
            .header("host", "evil.example")
            .header("x-setup-token", "t0k3n")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::MISDIRECTED_REQUEST);
    // no token
    let r = send(
        Request::get("/api/state")
            .header("host", format!("127.0.0.1:{PORT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    // a change from another origin
    let r = send(
        Request::post("/api/apply")
            .header("host", format!("127.0.0.1:{PORT}"))
            .header("x-setup-token", "t0k3n")
            .header("origin", "https://evil.example")
            .header("content-type", "application/json")
            .body(Body::from("{\"confirm\":true}"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // cross-site fetch
    let r = send(
        Request::get("/api/state")
            .header("host", format!("127.0.0.1:{PORT}"))
            .header("x-setup-token", "t0k3n")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // the page itself needs the token in the address
    let r = send(
        Request::get("/")
            .header("host", format!("127.0.0.1:{PORT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = send(
        Request::get("/?t=t0k3n")
            .header("host", format!("127.0.0.1:{PORT}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let csp = r
        .headers()
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"));
    // the admission key is never in the state
    let (_, st, _) = call(&s, "GET", "/api/state", None).await;
    assert!(!st.to_string().contains("acme-admission-key"));
}

#[tokio::test]
async fn first_run_then_reconfigure_keeps_everything() {
    let base = tmp("flow");
    let relay_port = free_port();
    let l = installed(&base, relay_port);
    let sys = Arc::new(Fake::new(base.join("units")));
    let s = setup_for(&l, sys.clone());

    let (_, st, _) = call(&s, "GET", "/api/state", None).await;
    assert_eq!(st["configured"], false);
    assert_eq!(st["provider"]["product"], "Acme Connect");
    assert_eq!(st["settings"]["port"], relay_port);

    // ACME needs an email and the terms
    let (code, v, _) = call(
        &s,
        "POST",
        "/api/settings",
        Some(json!({"tls_mode":"acme","update_mode":"manual"})),
    )
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);
    let fields: Vec<&str> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["field"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"email") && fields.contains(&"agree_tos"),
        "{v}"
    );
    // auto updates need a valid window
    let (code, _, _) = call(
        &s,
        "POST",
        "/api/settings",
        Some(json!({"tls_mode":"off","update_mode":"auto","window":"whenever"})),
    )
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);

    let (code, _, _) = call(
        &s,
        "POST",
        "/api/settings",
        Some(json!({"tls_mode":"off","update_mode":"manual"})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let (_, v, _) = call(&s, "POST", "/api/checks/system", Some(json!({}))).await;
    assert!(
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == format!("port-{relay_port}") && c["status"] == "ok"),
        "{v}"
    );
    let (_, v, _) = call(&s, "POST", "/api/checks/network", Some(json!({}))).await;
    assert_eq!(v["checks"][0]["status"], "ok", "{v}");
    let (_, v, _) = call(&s, "POST", "/api/tls", Some(json!({}))).await;
    assert_eq!(v["status"], "skip");
    let (_, v, _) = call(&s, "POST", "/api/apply", Some(json!({"confirm": true}))).await;
    assert_eq!(v["status"], "ok", "{v}");

    // what it did
    let log = sys.log();
    assert!(
        log.iter().any(|c| c.starts_with("useradd --system")),
        "{log:?}"
    );
    for c in [
        "systemctl daemon-reload",
        "systemctl enable taro-relay",
        "systemctl restart taro-relay",
    ] {
        assert!(log.iter().any(|x| x == c), "{c} missing in {log:?}");
    }
    assert!(log
        .iter()
        .any(|c| c.starts_with("chown root:taro-relay") && c.ends_with("provider.json")));
    let unit = std::fs::read_to_string(base.join("units/taro-relay.service")).unwrap();
    assert!(
        unit.contains(&format!(
            "ExecStart={} run",
            l.core().join("bin/taro-relay").display()
        )) && unit.contains("User=taro-relay")
            && unit.contains("RestartPreventExitStatus=78")
            && unit.contains("Acme Connect")
    );
    let rt = Runtime::load(&l).unwrap().unwrap();
    assert_eq!(rt.relay.public_host, "relay.acme.example");
    assert_eq!(rt.relay_port(), relay_port);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(l.provider.join("provider.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(Runtime::path(&l))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    // the relay ran for a while: data and a certificate exist
    std::fs::write(l.state.join("relay/ids.json"), "{\"123456789\":\"x\"}").unwrap();
    std::fs::write(l.state.join("tls/privkey.pem"), "KEY").unwrap();
    let provider_before = std::fs::read(l.provider.join("provider.json")).unwrap();
    let mut toml = std::fs::read_to_string(Runtime::path(&l)).unwrap();
    toml.push_str("\n# operator note: do not remove\n");
    std::fs::write(Runtime::path(&l), &toml).unwrap();

    // a second run: it starts from the saved settings and resets nothing
    let sys2 = Arc::new(Fake::new(base.join("units")));
    *sys2.user_exists.lock().unwrap() = true;
    let s2 = setup_for(&l, sys2.clone());
    let (_, st, _) = call(&s2, "GET", "/api/state", None).await;
    assert_eq!(st["configured"], true);
    assert_eq!(st["settings"]["tls_mode"], "off");
    let (_, _, _) = call(
        &s2,
        "POST",
        "/api/settings",
        Some(json!({"tls_mode":"off","update_mode":"notify"})),
    )
    .await;
    let (_, v, _) = call(&s2, "POST", "/api/apply", Some(json!({"confirm": true}))).await;
    assert_eq!(v["status"], "ok", "{v}");
    assert!(
        !sys2.log().iter().any(|c| c.starts_with("useradd")),
        "no second account"
    );
    assert!(sys2
        .log()
        .iter()
        .any(|c| c == "systemctl enable --now taro-update.timer"));
    assert_eq!(
        Runtime::load(&l).unwrap().unwrap().updates.mode,
        UpdateMode::Notify
    );
    assert!(std::fs::read_to_string(Runtime::path(&l))
        .unwrap()
        .contains("# operator note"));
    assert_eq!(
        std::fs::read_to_string(l.state.join("relay/ids.json")).unwrap(),
        "{\"123456789\":\"x\"}"
    );
    assert_eq!(
        std::fs::read_to_string(l.state.join("tls/privkey.pem")).unwrap(),
        "KEY"
    );
    assert_eq!(
        std::fs::read(l.provider.join("provider.json")).unwrap(),
        provider_before
    );
    let backups: Vec<_> = std::fs::read_dir(&l.runtime)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
        .collect();
    assert_eq!(backups.len(), 1, "the previous settings are kept");
}

#[tokio::test]
async fn a_failure_says_where_and_how_to_recover() {
    let base = tmp("fail");
    let l = installed(&base, free_port());
    let sys = Arc::new(Fake::new(base.join("units")));
    sys.failing.lock().unwrap().push("systemctl restart".into());
    let s = setup_for(&l, sys.clone());
    call(
        &s,
        "POST",
        "/api/settings",
        Some(json!({"tls_mode":"off","update_mode":"manual"})),
    )
    .await;
    let (_, v, _) = call(&s, "POST", "/api/apply", Some(json!({"confirm": true}))).await;
    assert_eq!(v["status"], "fail");
    assert!(
        v["error"].as_str().unwrap().starts_with("start the relay"),
        "{v}"
    );
    assert!(v["recovery"].as_str().unwrap().contains("again"));
    let steps: Vec<&str> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert!(
        steps.contains(&"systemctl daemon-reload")
            && !steps.iter().any(|s| s.starts_with("health"))
    );
    // diagnostics carry the failure and never the admission key
    let req = Request::get("/api/diagnostics.txt")
        .header("host", format!("127.0.0.1:{PORT}"))
        .header("x-setup-token", "t0k3n")
        .body(Body::empty())
        .unwrap();
    let r = setup::router(s.clone()).oneshot(req).await.unwrap();
    let text =
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(text.contains("apply failed") && !text.contains("acme-admission-key"));
}

#[test]
fn the_binary_knows_whether_setup_is_done() {
    let bin = env!("CARGO_BIN_EXE_taro-relay");
    // not set up: the service exits with 78 (systemd does not restart it in a loop)
    let base = tmp("bin-unconfigured");
    let _ = installed(&base, free_port());
    let out = std::process::Command::new(bin)
        .arg("run")
        .env("TARO_ROOT", &base)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
    assert!(String::from_utf8_lossy(&out.stderr).contains("taro-relay setup"));
    // the first launch without arguments opens the setup and prints how to reach it
    let mut child = std::process::Command::new(bin)
        .env("TARO_ROOT", &base)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut out = String::new();
    {
        use std::io::BufRead;
        let rd = std::io::BufReader::new(child.stdout.take().unwrap());
        for line in rd.lines() {
            let line = line.unwrap();
            out.push_str(&line);
            out.push('\n');
            if line.contains("?t=") {
                break;
            }
        }
    }
    child.kill().unwrap();
    let _ = child.wait();
    assert!(
        out.contains("ssh -L")
            && out.contains("YOU@relay.acme.example")
            && out.contains("http://127.0.0.1:"),
        "{out}"
    );
}

#[test]
fn the_configured_relay_runs_and_answers_its_health_check() {
    let bin = env!("CARGO_BIN_EXE_taro-relay");
    let base = tmp("bin-run");
    let port = free_port();
    let l = installed(&base, port);
    let mut r = Runtime::defaults(&l, "relay.acme.example", port);
    r.tls.mode = taro_relay::config::TlsMode::Off;
    r.save(&l, "1").unwrap();
    // configured: no setup on first launch, a status instead
    let out = std::process::Command::new(bin)
        .env("TARO_ROOT", &base)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("This relay is set up"));
    let mut child = std::process::Command::new(bin)
        .arg("run")
        .env("TARO_ROOT", &base)
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut res = Err(String::new());
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        res = taro_relay::serve::probe(port, Some(env!("CARGO_PKG_VERSION")));
        if res.is_ok() {
            break;
        }
    }
    let health = std::process::Command::new(bin)
        .arg("health")
        .env("TARO_ROOT", &base)
        .output()
        .unwrap();
    child.kill().unwrap();
    let _ = child.wait();
    let msg = res.expect("health");
    assert!(msg.contains("enforces the admission key"), "{msg}");
    assert!(
        health.status.success(),
        "{}",
        String::from_utf8_lossy(&health.stderr)
    );
}

#[tokio::test]
async fn an_update_of_the_core_pages_keeps_the_branding() {
    let base = tmp("update");
    let l = installed(&base, free_port());
    std::fs::write(l.provider.join("theme.css"), ".card{border-radius:2px}").unwrap();
    let s = setup_for(&l, Arc::new(Fake::new(base.join("units"))));
    let fetch = |path: &'static str| {
        let s = s.clone();
        async move {
            let r = setup::router(s)
                .oneshot(
                    Request::get(path)
                        .header("host", format!("127.0.0.1:{PORT}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            String::from_utf8_lossy(&r.into_body().collect().await.unwrap().to_bytes()).into_owned()
        }
    };
    let tokens_before = fetch("/brand/tokens.css").await;
    assert!(
        tokens_before.contains("--primary:#0F766E"),
        "{tokens_before}"
    );
    let css_before = fetch("/assets/app.css").await;

    // release N+1 changes the shared stylesheet and adds a component
    let key = taro_update::sig::generate();
    let keys = taro_update::sig::TrustedKeys::from_keys(vec![key.verifying_key()]);
    let rel = |v: &str, css: &str| {
        let d = base.join(format!("rel-{v}"));
        let _ = std::fs::remove_dir_all(&d);
        for f in [
            "setup/index.html",
            "setup/app.js",
            "status/index.html",
            "status/app.css",
            "status/app.js",
        ] {
            let p = d.join("web/relay").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::copy(l.core().join("web/relay").join(f), p).unwrap();
        }
        std::fs::write(d.join("web/relay/setup/app.css"), css).unwrap();
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("bin/taro-relay"), format!("binary {v}")).unwrap();
        d
    };
    let arch = std::env::consts::ARCH.to_string();
    let v1 = taro_update::pack::pack(
        &taro_update::pack::Input {
            component: "relay".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            released_at: "a".into(),
            cores: vec![(arch.clone(), rel("1", &css_before))],
            base: None,
            migrations: None,
            restart: vec![],
            source_commit: None,
        },
        &key,
        &base.join("u1.zip"),
    )
    .unwrap();
    let new_css = format!("{css_before}\n/* shared navigation, release N+1 */ .steps{{gap:2px}}\n");
    let zip = base.join("enterprise-update.zip");
    taro_update::pack::pack(
        &taro_update::pack::Input {
            component: "relay".into(),
            version: "9.9.9".into(),
            released_at: "b".into(),
            cores: vec![(arch, rel("2", &new_css))],
            base: Some(v1.core_files),
            migrations: None,
            restart: vec!["taro-relay".into()],
            source_commit: None,
        },
        &key,
        &zip,
    )
    .unwrap();

    struct H;
    impl taro_update::apply::Hooks for H {
        fn restart(&mut self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn health(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }
    let provider_before = std::fs::read(l.provider.join("provider.json")).unwrap();
    // the installed core matches release 1 except the binary: record it as release 1's
    std::fs::write(l.core().join("bin/taro-relay"), "binary 1").unwrap();
    taro_update::apply::record_fresh_install(
        &l,
        "relay",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH,
        "t",
    )
    .unwrap();
    let r = taro_update::apply::apply(&l, &zip, &keys, &mut H, &Default::default()).unwrap();
    assert!(r
        .written
        .contains(&"core/web/relay/setup/app.css".to_string()));

    // the served page has the new shared CSS; the branding is the provider's, unchanged
    assert!(fetch("/assets/app.css").await.contains("release N+1"));
    assert_eq!(fetch("/brand/tokens.css").await, tokens_before);
    assert_eq!(fetch("/brand/theme.css").await, ".card{border-radius:2px}");
    assert_eq!(
        std::fs::read(l.provider.join("provider.json")).unwrap(),
        provider_before
    );
    assert!(fetch("/brand/logo.png").await.contains("acme logo"));
}
