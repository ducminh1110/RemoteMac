//! `taro-relay run`: the relay as the systemd service runs it.
//!
//! - the relay itself (rm-relay) on `relay.listen`, TCP and UDP, admitting only the provider's
//!   clients (the key in provider.json)
//! - a health endpoint on 127.0.0.1:7479 (the updater and `taro-relay health` ask it)
//! - with TLS: the status page on 443 and port 80 for ACME challenges and a redirect; the
//!   certificate is renewed 30 days before it expires and swapped in without a restart

use crate::config::{Runtime, TlsMode, HEALTH_LISTEN};
use crate::tls;
use crate::web;
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, Response, StatusCode, Uri};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::Duration;
use taro_update::policy::Layout;

/// Exit code when the relay is not configured (systemd does not restart it then).
pub const NOT_CONFIGURED: i32 = 78;

struct Run {
    layout: Layout,
    runtime: Runtime,
    provider: rm_brand::Provider,
    challenges: tls::Challenges,
    cert: std::sync::RwLock<Option<tls::CertInfo>>,
}

type R = Arc<Run>;

/// Ask a running relay: the health endpoint answers with `version` (if given), and the relay
/// port refuses a join without the key (admission enforced).
pub fn probe(relay_port: u16, version: Option<&str>) -> Result<String, String> {
    use std::net::TcpStream;
    let mut s = TcpStream::connect_timeout(&HEALTH_LISTEN.parse().unwrap(), Duration::from_secs(2))
        .map_err(|e| format!("health endpoint {HEALTH_LISTEN}: {e}"))?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok();
    s.write_all(b"GET /healthz HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut body = String::new();
    std::io::Read::read_to_string(&mut s, &mut body).map_err(|e| e.to_string())?;
    let json = body.split("\r\n\r\n").nth(1).unwrap_or("");
    let v: serde_json::Value = serde_json::from_str(json).map_err(|_| {
        format!(
            "unexpected health answer: {}",
            body.lines().next().unwrap_or("")
        )
    })?;
    if v["status"] != "ok" {
        return Err(format!("health: {v}"));
    }
    if let Some(want) = version {
        if v["version"] != want {
            return Err(format!("version {} answers, {want} expected", v["version"]));
        }
    }
    let mut r = TcpStream::connect_timeout(
        &format!("127.0.0.1:{relay_port}")
            .parse()
            .map_err(|e| format!("{e}"))?,
        Duration::from_secs(2),
    )
    .map_err(|e| format!("relay port {relay_port}: {e}"))?;
    r.set_read_timeout(Some(Duration::from_secs(3))).ok();
    r.write_all(b"{\"session_id\":\"probe\",\"role\":\"agent\",\"token\":\"0123456789abcdef0\"}\n")
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(r)
        .read_line(&mut line)
        .map_err(|e| format!("relay: {e}"))?;
    if !line.starts_with("ERR not admitted") {
        return Err(format!(
            "the relay admitted a join without the key ({})",
            line.trim()
        ));
    }
    Ok(format!(
        "version {} up, relay on {relay_port} enforces the admission key",
        v["version"]
    ))
}

async fn healthz(State(r): State<R>) -> Response<Body> {
    let cert = r.cert.read().unwrap().clone();
    web::bytes(
        StatusCode::OK,
        "application/json",
        json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION"), "product": r.provider.product.name, "relay": r.runtime.relay.listen, "tls_days_left": cert.map(|c| c.days_left) }).to_string(),
    )
}

/// Public: what clients and monitoring may know (never the key).
async fn relay_json(State(r): State<R>) -> Response<Body> {
    let installed = taro_update::apply::installed(&r.layout)
        .ok()
        .map(|i| i.version);
    web::bytes(StatusCode::OK, "application/json", json!({ "product": r.provider.product.name, "vendor": r.provider.product.vendor, "support_url": r.provider.product.support_url, "host": r.provider.relay.host, "port": r.provider.relay.port, "version": installed.unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()) }).to_string())
}

async fn status_page(State(r): State<R>) -> Response<Body> {
    web::core_asset(&r.layout, "status/index.html")
}

async fn status_asset(State(r): State<R>, UrlPath(f): UrlPath<String>) -> Response<Body> {
    web::core_asset(&r.layout, &format!("status/{f}"))
}

async fn common(State(r): State<R>, UrlPath(f): UrlPath<String>) -> Response<Body> {
    web::core_asset(&r.layout, &format!("common/{f}"))
}

async fn brand(State(r): State<R>, UrlPath(f): UrlPath<String>) -> Response<Body> {
    web::brand(&r.layout, &f)
}

async fn challenge(State(r): State<R>, UrlPath(t): UrlPath<String>) -> Response<Body> {
    match r.challenges.get(&t) {
        Some(v) => web::bytes(StatusCode::OK, "text/plain", v),
        None => web::not_found(),
    }
}

async fn redirect(State(r): State<R>, uri: Uri) -> Response<Body> {
    let to = format!(
        "https://{}{}",
        r.runtime.relay.public_host,
        uri.path_and_query().map(|p| p.as_str()).unwrap_or("/")
    );
    web::secure(
        Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(header::LOCATION, to)
            .body(Body::empty())
            .unwrap(),
    )
}

fn public_router(r: R) -> Router {
    Router::new()
        .route("/", get(status_page))
        .route("/healthz", get(healthz))
        .route("/.well-known/taro/relay.json", get(relay_json))
        .route("/assets/{file}", get(status_asset))
        .route("/common/{file}", get(common))
        .route("/brand/{file}", get(brand))
        .with_state(r)
}

async fn serve_tls(listener: tokio::net::TcpListener, cfg: Arc<rustls::ServerConfig>, app: Router) {
    let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await
            else {
                return;
            };
            let svc =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let app = app.clone();
                    async move {
                        Ok::<_, std::convert::Infallible>(
                            tower::ServiceExt::oneshot(app, req.map(Body::new))
                                .await
                                .unwrap(),
                        )
                    }
                });
            let _ =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                    .await;
        });
    }
}

/// Renew when due; the certificate in use is swapped by `reloadable`.
async fn renewal(r: R, reloadable: Option<Arc<tls::Reloadable>>) {
    loop {
        let due = tls::check_pair(
            &r.runtime.tls.cert,
            &r.runtime.tls.key,
            &r.runtime.relay.public_host,
        )
        .map(|i| i.days_left <= tls::RENEW_DAYS)
        .unwrap_or(true);
        if due && r.runtime.tls.mode == TlsMode::Acme {
            let store = tls::Store {
                dir: r
                    .runtime
                    .tls
                    .key
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| r.layout.state.join("tls")),
            };
            let email = r.runtime.tls.email.clone().unwrap_or_default();
            match tls::issue(
                &r.runtime.tls.directory,
                &email,
                &r.runtime.relay.public_host,
                &store,
                &r.challenges,
                &r.runtime.tls.cert,
                &r.runtime.tls.key,
            )
            .await
            {
                Ok(i) => {
                    eprintln!("taro-relay: certificate renewed, {} days", i.days_left);
                    match &reloadable {
                        Some(rl) => {
                            if let Err(e) = rl.reload(&r.runtime.tls.cert, &r.runtime.tls.key) {
                                eprintln!(
                                    "taro-relay: the renewed certificate could not be loaded: {e}"
                                );
                            }
                        }
                        None => {
                            // HTTPS was not started for want of a certificate: systemd starts us again
                            eprintln!(
                                "taro-relay: first certificate obtained; restarting to serve HTTPS"
                            );
                            std::process::exit(0);
                        }
                    }
                    *r.cert.write().unwrap() = Some(i);
                }
                Err(e) => eprintln!(
                    "taro-relay: certificate renewal failed (tried again in 6 hours): {e}"
                ),
            }
        }
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    }
}

pub fn run(layout: Layout) -> ! {
    let runtime = match Runtime::load(&layout) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("taro-relay: this server is not set up yet. Run: sudo taro-relay setup");
            std::process::exit(NOT_CONFIGURED)
        }
        Err(e) => {
            eprintln!("taro-relay: {e}. Fix it, or run: sudo taro-relay setup");
            std::process::exit(NOT_CONFIGURED)
        }
    };
    let provider = match web::provider(&layout) {
        Ok(Some(p)) => p,
        Ok(None) => {
            eprintln!(
                "taro-relay: {} is missing",
                layout.provider.join(rm_brand::FILE).display()
            );
            std::process::exit(NOT_CONFIGURED)
        }
        Err(e) => {
            eprintln!("taro-relay: {e}");
            std::process::exit(NOT_CONFIGURED)
        }
    };

    // the relay (blocking, its own threads)
    let listener = std::net::TcpListener::bind(&runtime.relay.listen).unwrap_or_else(|e| {
        eprintln!("taro-relay: {}: {e}", runtime.relay.listen);
        std::process::exit(1)
    });
    let ids_path = layout.state.join("relay/ids.json");
    let key = provider.relay.key.clone();
    eprintln!(
        "taro-relay {}: {} relay on {} (TCP+UDP), admission key required",
        env!("CARGO_PKG_VERSION"),
        provider.product.name,
        runtime.relay.listen
    );
    std::thread::spawn(move || {
        rm_relay::serve(
            listener,
            rm_relay::Config {
                key: Some(key),
                ids_path: Some(ids_path),
                ..Default::default()
            },
        )
    });

    let rt = tokio::runtime::Runtime::new().expect("tokio");
    rt.block_on(async move {
        let cert = tls::check_pair(&runtime.tls.cert, &runtime.tls.key, &runtime.relay.public_host).ok();
        let r: R = Arc::new(Run { layout, runtime: runtime.clone(), provider, challenges: tls::Challenges::default(), cert: std::sync::RwLock::new(cert) });
        let health = Router::new().route("/healthz", get(healthz)).with_state(r.clone());
        match tokio::net::TcpListener::bind(HEALTH_LISTEN).await {
            Ok(l) => {
                tokio::spawn(async move {
                    let _ = axum::serve(l, health).await;
                });
            }
            Err(e) => eprintln!("taro-relay: health endpoint {HEALTH_LISTEN}: {e}"),
        }
        if runtime.tls.mode != TlsMode::Off {
            let http = Router::new().route("/.well-known/acme-challenge/{token}", get(challenge)).fallback(redirect).with_state(r.clone());
            match tokio::net::TcpListener::bind(&runtime.tls.http_listen).await {
                Ok(l) => {
                    tokio::spawn(async move {
                        let _ = axum::serve(l, http).await;
                    });
                }
                Err(e) => eprintln!("taro-relay: {}: {e} (certificate renewal needs port 80)", runtime.tls.http_listen),
            }
            let reloadable = match tls::Reloadable::load(&runtime.tls.cert, &runtime.tls.key) {
                Ok(rl) => Some(rl),
                Err(e) => {
                    eprintln!("taro-relay: no usable certificate yet ({e}); HTTPS starts once one is obtained");
                    None
                }
            };
            if let Some(rl) = &reloadable {
                match tokio::net::TcpListener::bind(&runtime.tls.https_listen).await {
                    Ok(l) => {
                        let cfg = rl.server_config();
                        let app = public_router(r.clone());
                        tokio::spawn(serve_tls(l, cfg, app));
                    }
                    Err(e) => eprintln!("taro-relay: {}: {e}", runtime.tls.https_listen),
                }
            }
            tokio::spawn(renewal(r.clone(), reloadable));
        }
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        eprintln!("taro-relay: stopping");
    });
    std::process::exit(0)
}
