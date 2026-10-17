//! `taro-enterprise-setup` (shipped on Windows as `TaroEnterpriseSetup.exe`): the provider's
//! package builder. On Windows it is an app with its own window (WebView2); elsewhere, and with
//! `--browser`, the same pages open in the default browser.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use taro_enterprise_setup::{config, platform, secret, server, state};

const HELP: &str = "\
Taro Enterprise Setup: the package builder (Wizard 1)

USAGE:
    taro-enterprise-setup [--browser] [--no-browser] [--port <PORT>] [--output <FOLDER>]

Builds your branded Taro packages (Windows, macOS, Linux relay) from the signed official
release files. On Windows it opens in its own window. Its pages are served on 127.0.0.1 only,
and every request needs a one-time access token.

OPTIONS:
    --browser          Open the wizard in the default browser instead of its own window
    --no-browser       Print the link only (implies --browser)
    --port <PORT>      Use this port instead of a random one (still 127.0.0.1 only)
    --output <FOLDER>  Default output folder (Documents\\Taro Enterprise on Windows, ./dist elsewhere)
    -h, --help         Show this help
    -V, --version      Show the version
";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ui {
    Window,
    Browser,
    Link,
}

fn main() {
    #[cfg(windows)]
    platform::attach_parent_console();
    let mut ui = if cfg!(windows) {
        Ui::Window
    } else {
        Ui::Browser
    };
    let mut port: u16 = 0;
    let mut output: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--browser" => ui = Ui::Browser,
            "--no-browser" => ui = Ui::Link,
            "--port" => match args.next().and_then(|p| p.parse().ok()) {
                Some(p) => port = p,
                None => fail("--port needs a number"),
            },
            "--output" => match args.next() {
                Some(o) => output = Some(PathBuf::from(o)),
                None => fail("--output needs a folder"),
            },
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("taro-enterprise-setup {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            other => fail(&format!("unknown argument {other:?}\n\n{HELP}")),
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    // `stopped` turns true once: from the page's Quit, the window's close, Ctrl+C or the message
    // box; the server, the window and the wait below all follow it
    let (stop_tx, stopped) = tokio::sync::watch::channel(false);
    let started = rt.block_on(start(port, output, ui, stopped.clone()));
    let (url, app, server) = match started {
        Ok(x) => x,
        Err(e) => fail(&e),
    };
    {
        let app = app.clone();
        rt.spawn(async move {
            app.shutdown.notified().await;
            let _ = stop_tx.send(true);
        });
    }

    #[cfg(windows)]
    if ui == Ui::Window {
        let mut stopped = stopped.clone();
        let handle = rt.handle().clone();
        let shown = taro_enterprise_setup::window::run(&format!("{url}&shell=app"), move |shell| {
            handle.spawn(async move {
                let _ = stopped.wait_for(|v| *v).await;
                shell.close();
            });
        });
        match shown {
            Ok(()) => app.shutdown.notify_one(),
            Err(e) => {
                app.log.warn(format!("no app window: {e}"));
                platform::message_box(
                    "Taro Enterprise Setup",
                    &format!("{e}\n\nThe wizard opens in your browser instead."),
                );
                ui = Ui::Browser;
            }
        }
    }
    if ui != Ui::Window {
        println!("Open this link (it contains a one-time access token):");
        println!("  {url}");
        println!();
        println!("Keep this program running while you use the wizard. Press Ctrl+C to quit.");
        if ui == Ui::Browser {
            if let Err(e) = platform::open_browser(&url) {
                println!("(Could not open a browser automatically: {e}. Copy the link above.)");
            }
        }
        #[cfg(windows)]
        if ui == Ui::Browser {
            // a GUI app has no console to stop it from: this box keeps it running
            let app = app.clone();
            std::thread::spawn(move || {
                platform::message_box(
                    "Taro Enterprise Setup",
                    "Taro Enterprise Setup is running in your browser.\n\nClose this message to quit it.",
                );
                app.shutdown.notify_one();
            });
        }
        let mut stopped = stopped.clone();
        rt.block_on(async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => app.shutdown.notify_one(),
                _ = stopped.wait_for(|v| *v) => {},
            }
        });
    }
    let _ = rt.block_on(server);
    println!("Taro Enterprise Setup stopped. The packages you built stay in the output folder; nothing else was saved.");
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    #[cfg(windows)]
    platform::message_box("Taro Enterprise Setup", msg);
    std::process::exit(2)
}

type Server = tokio::task::JoinHandle<Result<(), std::io::Error>>;

/// Start the server; returns the page's address (with its token), the state and the server task
/// (which ends after `app.shutdown`).
async fn start(
    port: u16,
    output: Option<PathBuf>,
    ui: Ui,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) -> Result<(String, Arc<state::AppState>, Server), String> {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let token = secret::random_hex(32);
    let output_dir = match output {
        Some(o) if o.is_absolute() => o,
        Some(o) => std::env::current_dir().map_err(|e| e.to_string())?.join(o),
        None => platform::default_output_dir(),
    };
    let app = state::AppState::new(
        addr,
        token.clone(),
        state::Options {
            endpoints: config::Endpoints::default(),
            timings: config::Timings::default(),
            // the shipped wizard trusts only the official release keys; there is no switch
            // to change that (tests construct their own Options)
            keys: taro_update::official_keys(),
            output_dir,
            open_folder: Arc::new(platform::open_folder),
            key_dir: platform::default_key_dir(),
        },
    );
    println!(
        "Taro Enterprise Setup {}: package builder",
        env!("CARGO_PKG_VERSION")
    );
    app.log.info(format!(
        "wizard listening on {addr} ({})",
        match ui {
            Ui::Window => "app window",
            Ui::Browser => "browser",
            Ui::Link => "link only",
        }
    ));
    let router = server::router(app.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|v| *v).await;
            })
            .await
    });
    Ok((format!("http://{addr}/?t={token}"), app, server))
}
