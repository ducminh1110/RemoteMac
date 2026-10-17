//! Step 1: preflight checks: the OS, HTTPS reachability of the download hosts, free disk
//! space where the packages will be written, and that the wizard listens on loopback only.

use crate::config::Endpoints;
use crate::platform;
use serde::Serialize;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Debug, Serialize)]
pub struct PreflightCheck {
    pub id: String,
    pub name: String,
    /// ok | warn | fail
    pub status: String,
    pub detail: String,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreflightReport {
    pub os: Option<platform::OsInfo>,
    pub checks: Vec<PreflightCheck>,
    pub at: u64,
}

fn check(id: &str, name: &str, status: &str, detail: String, hint: Option<&str>) -> PreflightCheck {
    PreflightCheck {
        id: id.into(),
        name: name.into(),
        status: status.into(),
        detail,
        hint: hint.map(str::to_string),
    }
}

/// HTTPS reachability: any HTTP response over a verified TLS connection counts (a 404 or a redirect
/// still proves the network path works).
pub async fn reachability(
    http: &reqwest::Client,
    id: &str,
    name: &str,
    url: &str,
) -> PreflightCheck {
    let started = std::time::Instant::now();
    match tokio::time::timeout(Duration::from_secs(10), http.get(url).send()).await {
        Ok(Ok(r)) => check(
            id,
            name,
            "ok",
            format!(
                "HTTPS OK (HTTP {} in {} ms)",
                r.status().as_u16(),
                started.elapsed().as_millis()
            ),
            None,
        ),
        Ok(Err(e)) => {
            let dbg = format!("{e:?}");
            let hint = if dbg.contains("certificate") || dbg.contains("Certificate") {
                "TLS certificate verification failed. A corporate proxy or antivirus that inspects HTTPS may be replacing certificates; its root certificate must be installed in the system store, or run the wizard on a network without TLS interception."
            } else if e.is_timeout() {
                "The request timed out. Check the internet connection, firewall and proxy settings."
            } else {
                "Could not connect. Check the internet connection, DNS, firewall and (if used) the HTTPS_PROXY environment variable."
            };
            check(id, name, "fail", format!("{}", e.without_url()), Some(hint))
        }
        Err(_) => check(
            id,
            name,
            "fail",
            "timed out after 10 s".into(),
            Some("The request timed out. Check the internet connection, firewall and proxy settings."),
        ),
    }
}

/// Free space needed for a typical build (downloads plus packages of all four components).
pub const DISK_WARN: u64 = 1_000_000_000;

pub async fn run(
    http: &reqwest::Client,
    ep: &Endpoints,
    listener: SocketAddr,
    output_dir: &Path,
) -> PreflightReport {
    let mut checks = Vec::new();
    let os = match platform::os_info() {
        Ok(i) => {
            let (status, hint) = if i.family == "windows" {
                let build: u32 = i.build.parse().unwrap_or(0);
                if build < 17763 {
                    (
                        "warn",
                        Some("Windows 10 1809 (build 17763) or later is recommended."),
                    )
                } else {
                    ("ok", None)
                }
            } else {
                ("ok", None)
            };
            checks.push(check(
                "os",
                "Operating system",
                status,
                format!("{} {} (build {}, {})", i.name, i.version, i.build, i.arch),
                hint,
            ));
            Some(i)
        }
        Err(e) => {
            checks.push(check("os", "Operating system", "warn", e, None));
            None
        }
    };

    let probes: Vec<_> = ep
        .reachability
        .iter()
        .map(|(id, name, url)| {
            let (http, id, name, url) = (http.clone(), id.clone(), name.clone(), ep.fetch_url(url));
            tokio::spawn(async move { reachability(&http, &id, &name, &url).await })
        })
        .collect();
    for p in probes {
        if let Ok(c) = p.await {
            checks.push(c);
        }
    }

    match platform::existing_ancestor(output_dir) {
        None => checks.push(check(
            "disk",
            "Free disk space in the output folder",
            "fail",
            format!("No existing folder at or above {}", output_dir.display()),
            Some("Choose an output folder on an existing drive in the Build step."),
        )),
        Some(dir) => match platform::free_disk_bytes(&dir) {
            Ok(b) => {
                let gb = b as f64 / 1e9;
                let (s, h) = if b < DISK_WARN {
                    ("warn", Some("Less than 1 GB free. A build of all four packages downloads and writes a few hundred MB; free some space or pick another output folder."))
                } else {
                    ("ok", None)
                };
                checks.push(check(
                    "disk",
                    "Free disk space in the output folder",
                    s,
                    format!("{gb:.1} GB free on the volume holding {}", dir.display()),
                    h,
                ));
            }
            Err(e) => checks.push(check(
                "disk",
                "Free disk space in the output folder",
                "warn",
                e,
                None,
            )),
        },
    }

    checks.push(if listener.ip().is_loopback() {
        check(
            "loopback",
            "Wizard is loopback-only",
            "ok",
            format!("Listening on {listener} only (not reachable from the network)"),
            None,
        )
    } else {
        check(
            "loopback",
            "Wizard is loopback-only",
            "fail",
            format!("Listening on {listener}"),
            Some("The wizard must listen on 127.0.0.1 only. Restart it without overriding the bind address."),
        )
    });

    PreflightReport {
        os,
        checks,
        at: crate::util::now_secs(),
    }
}
