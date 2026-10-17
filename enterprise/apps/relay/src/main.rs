//! taro-relay: the Taro Enterprise relay server.
//!
//!   taro-relay                     first launch: the setup when this server is not set up yet
//!   taro-relay setup [--port N]    Wizard 2 (again: change the settings, keeping the data)
//!   taro-relay run                 the relay (what the systemd service runs)
//!   taro-relay status | health | version
//!   taro-relay update check | apply [FILE | --latest] [--yes] [--overwrite-modified]
//!                     | rollback | status | auto
//!   taro-relay install-core --version V --arch A   (install.sh: record a fresh installation)
//!
//! TARO_ROOT=DIR puts the whole installation under DIR (tests, portable installs).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use taro_relay::config::Runtime;
use taro_relay::setup::{self, RealProbe, Setup};
use taro_relay::system::{Real, System};
use taro_relay::{checks, serve, update, web};
use taro_update::policy::Layout;

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("taro-relay: {msg}");
    std::process::exit(1)
}

fn layout() -> (Layout, Option<PathBuf>) {
    match std::env::var_os("TARO_ROOT") {
        Some(r) => {
            let r = PathBuf::from(r);
            (Layout::under(&r), Some(r))
        }
        None => (Layout::system(), None),
    }
}

fn run_setup(port: Option<u16>) -> ! {
    let (layout, root) = layout();
    let sys = Real;
    if !sys.is_root() && root.is_none() {
        die("setup changes the system (service account, systemd, firewall): run it with sudo:  sudo taro-relay setup");
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio");
    rt.block_on(async move {
        let wanted = port.unwrap_or(7480);
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", wanted)).await {
            Ok(l) => l,
            Err(_) if port.is_none() => tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap_or_else(|e| die(e)),
            Err(e) => die(format!("127.0.0.1:{wanted}: {e}")),
        };
        let port = listener.local_addr().unwrap().port();
        let token = web::token();
        let host = web::provider(&layout)
            .ok()
            .flatten()
            .map(|p| p.relay.host)
            .unwrap_or_else(|| "THIS-SERVER".into());
        let s = Arc::new(Setup {
            layout,
            sys: Arc::new(Real),
            net: Arc::new(checks::RealNet::new()),
            probe: Arc::new(RealProbe),
            token: token.clone(),
            port,
            draft: Mutex::new(Default::default()),
            challenges: Default::default(),
            log: Mutex::new(vec![]),
            done: Default::default(),
            root_override: root,
        });
        let url = format!("http://127.0.0.1:{port}/?t={token}");
        println!();
        println!("  Taro relay setup is waiting for you (only on this server's loopback address).");
        println!();
        println!("  From your computer, open an SSH tunnel to this server and then the page:");
        println!("      ssh -L {port}:127.0.0.1:{port} YOU@{host}");
        println!("      {url}");
        println!();
        println!("  (On this server itself, the same address works in a local browser.)");
        println!("  Setup ends when you press Finish on the page, or with Ctrl+C.");
        println!();
        let done = s.clone();
        let app = setup::router(s.clone());
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            tokio::select! {
                _ = done.done.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        });
        if let Err(e) = server.await {
            die(e);
        }
    });
    std::process::exit(0)
}

fn status() {
    let (layout, _) = layout();
    match taro_update::apply::installed(&layout) {
        Ok(i) => println!("installed: taro-relay {} ({})", i.version, i.arch),
        Err(e) => println!("installed: {e}"),
    }
    match web::provider(&layout) {
        Ok(Some(p)) => println!(
            "provider:  {} ({}), relay {}",
            p.product.name,
            p.provider_id,
            p.relay.address()
        ),
        Ok(None) => println!(
            "provider:  none (no {})",
            layout.provider.join(rm_brand::FILE).display()
        ),
        Err(e) => println!("provider:  {e}"),
    }
    match Runtime::load(&layout) {
        Ok(Some(r)) => println!(
            "settings:  set up {} — TLS {:?}, updates {:?}",
            r.configured_at, r.tls.mode, r.updates.mode
        ),
        Ok(None) => println!("settings:  not set up yet (sudo taro-relay setup)"),
        Err(e) => println!("settings:  {e}"),
    }
    if let Ok(s) = std::fs::read_to_string(layout.update_state().join("available.json")) {
        println!("updates:   {s}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let has = |n: &str| args.iter().any(|a| a == n);
    match args.first().map(String::as_str) {
        None => {
            // first launch: set up if needed
            let (layout, _) = layout();
            match Runtime::load(&layout) {
                Ok(Some(_)) => {
                    status();
                    println!("\nThis relay is set up and runs as the taro-relay service. To change its settings: sudo taro-relay setup");
                }
                Ok(None) => run_setup(None),
                Err(e) => die(format!("{e}\nFix the file or run: sudo taro-relay setup")),
            }
        }
        Some("setup") => {
            run_setup(flag("--port").map(|p| p.parse().unwrap_or_else(|_| die("--port N"))))
        }
        Some("run") => serve::run(layout().0),
        Some("status") => status(),
        Some("version" | "--version") => println!("taro-relay {}", env!("CARGO_PKG_VERSION")),
        Some("health") => {
            let (layout, _) = layout();
            let port = Runtime::load(&layout)
                .ok()
                .flatten()
                .map(|r| r.relay_port())
                .unwrap_or(7470);
            match serve::probe(port, None) {
                Ok(s) => println!("healthy: {s}"),
                Err(e) => die(format!("unhealthy: {e}")),
            }
        }
        Some("install-core") => {
            let (layout, _) = layout();
            let v = flag("--version").unwrap_or_else(|| die("--version V"));
            let a = flag("--arch").unwrap_or_else(|| std::env::consts::ARCH.into());
            if v != env!("CARGO_PKG_VERSION") {
                die(format!(
                    "this binary is {}, the package says {v}",
                    env!("CARGO_PKG_VERSION")
                ));
            }
            let now = setup::stamp();
            match taro_update::apply::record_fresh_install(&layout, "relay", &v, &a, &now) {
                Ok(i) => println!(
                    "recorded taro-relay {} ({}), {} core files",
                    i.version,
                    i.arch,
                    i.files.len()
                ),
                Err(e) => die(e),
            }
        }
        Some("update") => update_cmd(&args[1..], has("--yes"), has("--overwrite-modified")),
        Some(other) => die(format!(
            "unknown command {other}; see the top of `taro-relay --help`"
        )),
    }
}

fn update_cmd(args: &[String], yes: bool, overwrite: bool) {
    let (layout, _) = layout();
    let sys = Real;
    let probe = RealProbe;
    let installed = || taro_update::apply::installed(&layout).unwrap_or_else(|e| die(e));
    let source = || {
        Runtime::load(&layout)
            .ok()
            .flatten()
            .map(|r| r.updates.source)
            .unwrap_or_else(|| taro_relay::config::RELEASES.into())
    };
    match args.first().map(String::as_str) {
        Some("check") => {
            let i = installed();
            let avail = update::discover(&source(), &i.version).unwrap_or_else(|e| die(e));
            if avail.is_empty() {
                println!("taro-relay {} is the newest Enterprise version", i.version);
                return;
            }
            match update::chain(&layout, &avail, &i.version) {
                Ok(c) => println!("available: {} (sudo taro-relay update apply --latest)", c.iter().map(|(a, _)| a.version.as_str()).collect::<Vec<_>>().join(" -> ")),
                Err(e) => die(e),
            }
        }
        Some("apply") => {
            if !sys.is_root() {
                die("run with sudo");
            }
            let port = Runtime::load(&layout).ok().flatten().map(|r| r.relay_port()).unwrap_or(7470);
            let target = args.get(1).filter(|a| !a.starts_with("--")).cloned();
            if !yes {
                die("an update restarts the relay (sessions reconnect): add --yes to go ahead");
            }
            match target {
                Some(f) => match update::apply_file(&layout, &sys, &probe, std::path::Path::new(&f), overwrite, port) {
                    Ok(r) => println!("updated {} -> {}: {} file(s) written, {} removed; backup in {}", r.from, r.to, r.written.len(), r.removed.len(), r.backup.display()),
                    Err(e) => die(e),
                },
                None => {
                    let i = installed();
                    let avail = update::discover(&source(), &i.version).unwrap_or_else(|e| die(e));
                    let ch = update::chain(&layout, &avail, &i.version).unwrap_or_else(|e| die(e));
                    if ch.is_empty() {
                        println!("nothing to update");
                        return;
                    }
                    match update::apply_chain(&layout, &sys, &probe, &ch, overwrite) {
                        Ok(s) => println!("{s}"),
                        Err(e) => die(e),
                    }
                }
            }
        }
        Some("rollback") => {
            if !sys.is_root() {
                die("run with sudo");
            }
            let port = Runtime::load(&layout).ok().flatten().map(|r| r.relay_port()).unwrap_or(7470);
            let mut hooks = update::SystemdHooks { sys: &sys, relay_port: port, probe: &probe };
            match taro_update::apply::rollback_last(&layout, &mut hooks, &setup::stamp()) {
                Ok(s) => println!("{s}"),
                Err(e) => die(e),
            }
        }
        Some("status") => {
            let i = installed();
            println!("installed {} ({}), {} core files", i.version, i.arch, i.files.len());
            for h in taro_update::apply::read_history(&layout).iter().rev().take(10) {
                println!("  {} {} -> {}: {} {}", h.at, h.from, h.to, h.result, h.detail);
            }
        }
        Some("auto") => match update::auto(&layout, &sys, &probe) {
            Ok(s) => println!("{s}"),
            Err(e) => die(e),
        },
        _ => die("update check | apply [FILE | --latest] --yes [--overwrite-modified] | rollback | status | auto"),
    }
}
