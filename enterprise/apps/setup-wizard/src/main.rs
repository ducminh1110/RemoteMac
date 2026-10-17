//! `taro-enterprise-setup` (shipped on Windows as `TaroEnterpriseSetup.exe`).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use taro_enterprise_setup::{config, domain, platform, remember, secret, server, state};

const HELP: &str = "\
Taro Enterprise Setup Wizard

USAGE:
    taro-enterprise-setup [--no-browser] [--port <PORT>]

Starts a local wizard on http://127.0.0.1:<PORT>/ (a random free port by default) and opens
it in the default browser. The wizard only listens on the loopback interface and every
request needs the one-time access token contained in the printed link.

OPTIONS:
    --no-browser     Print the link but do not open a browser
    --port <PORT>    Use this port instead of a random one (still 127.0.0.1 only)
    -h, --help       Show this help
    -V, --version    Show the version

ENVIRONMENT:
    TARO_SETUP_GITHUB_CLIENT_ID   OAuth/GitHub App client id for the device flow
    TARO_SETUP_TEMPLATE           Template repository (default ducminh1110/taro-enterprise-template)
    TARO_SETUP_MANAGED_ZONE       DNS zone for Taro-managed subdomains (if Taro provides one)
";

fn main() {
    let mut no_browser = false;
    let mut port: u16 = 0;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--no-browser" => no_browser = true,
            "--port" => match args.next().and_then(|p| p.parse().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("--port needs a number");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("taro-enterprise-setup {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => {
                eprintln!("unknown argument {other:?}\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(run(port, no_browser)) {
        eprintln!("error: {e}");
        #[cfg(windows)]
        {
            eprintln!("Press Enter to close this window.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

async fn run(port: u16, no_browser: bool) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let token = secret::random_hex(32);
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let app = state::AppState::new(
        addr,
        token.clone(),
        state::Options {
            endpoints: config::Endpoints::default(),
            timings: config::Timings::default(),
            dns: Arc::new(domain::HickoryDns),
            remember_dir: remember::default_dir(),
            github_client_id: env("TARO_SETUP_GITHUB_CLIENT_ID"),
            managed_zone: env("TARO_SETUP_MANAGED_ZONE"),
            template: env("TARO_SETUP_TEMPLATE").unwrap_or_else(|| config::DEFAULT_TEMPLATE.into()),
        },
    );
    let url = format!("http://{addr}/?t={token}");
    println!("Taro Enterprise Setup {}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Open this link in your browser (it contains a one-time access token):");
    println!("  {url}");
    println!();
    println!("Keep this window open while you use the wizard. Press Ctrl+C to quit.");
    app.log.info(format!("wizard listening on {addr}"));
    if !no_browser {
        if let Err(e) = platform::open_browser(&url) {
            println!("(Could not open a browser automatically: {e}. Copy the link above.)");
        }
    }

    let shutdown = app.shutdown.clone();
    let router = server::router(app);
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = shutdown.notified() => {},
            }
        })
        .await
        .map_err(|e| e.to_string())?;
    println!("Taro Enterprise Setup stopped. Nothing was saved on this PC unless you chose \"remember\".");
    Ok(())
}
