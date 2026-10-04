// Release builds are a GUI app (no console window); errors go to the connect window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let has = |k: &str| args.iter().any(|a| a == k);
    if has("-h") || has("--help") {
        eprintln!("usage: RemoteMac.exe                      (asks for the Mac's ID and password)\n       RemoteMac.exe --id 123456789 --password PASS\n       RM_SESSION_TOKEN=.. RemoteMac.exe --session NAME   (legacy)\n  [--relay HOST:PORT] [--app testapp] [--raw-ctrl] [--no-clipboard] [--renderer d3d11|gdi] [--mac-file-panel] [--no-shortcuts] [--smoke] [--showcase DIR --apps xcode,textedit --settle SECS --type TEXT]");
        std::process::exit(0)
    }
    let relay = get("--relay").or_else(|| std::env::var("RM_RELAY").ok().filter(|r| !r.is_empty())).unwrap_or_else(|| rm_protocol::session::DEFAULT_RELAY.into());
    // --session (legacy, token from RM_SESSION_TOKEN) | --id + --password | the connect window
    let (session, token, prompt) = match (get("--session"), get("--id"), get("--password").or_else(|| std::env::var("RM_PASSWORD").ok())) {
        (Some(s), _, _) => (s, std::env::var("RM_SESSION_TOKEN").unwrap_or_default(), false),
        (None, Some(id), Some(pw)) => match rm_protocol::session::normalize_id(&id) {
            Some(id) => (rm_protocol::session::relay_session(&id), rm_protocol::session::token(&id, &pw), false),
            None => {
                eprintln!("--id must be the 9-digit ID the Mac prints");
                std::process::exit(2)
            }
        },
        _ => (String::new(), String::new(), true),
    };
    #[cfg(windows)]
    {
        // no console in the release build: log to a file, and never die without a word
        rm_viewer::native::log_to_file(&rm_viewer::log_path());
        let quiet = has("--smoke") || has("--showcase");
        std::panic::set_hook(Box::new(move |info| {
            let msg = format!("MacBridge stopped because of an internal error:\n\n{info}\n\nLog: {}", rm_viewer::log_path().display());
            eprintln!("{msg}");
            if !quiet {
                rm_viewer::native::message_box("MacBridge", &msg);
            }
        }));
        eprintln!("RemoteMac viewer {} starting (relay {relay})", env!("CARGO_PKG_VERSION"));
        let opts = rm_viewer::ui::Options { relay, session, token, prompt: prompt && !has("--smoke") && !has("--showcase"), app: get("--app"), ctrl_as_command: !has("--raw-ctrl"), smoke: has("--smoke"), clipboard: !has("--no-clipboard"), d3d: get("--renderer").as_deref() != Some("gdi"), windows_file_picker: !has("--mac-file-panel") && !has("--showcase"), shortcuts: !has("--no-shortcuts"),
            showcase: get("--showcase").map(|dir| rm_viewer::ui::ShowcaseOptions {
                apps: get("--apps").unwrap_or_else(|| "xcode".into()).split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                dir: dir.into(),
                settle: std::time::Duration::from_secs(get("--settle").and_then(|s| s.parse().ok()).unwrap_or(12)),
                app_timeout: std::time::Duration::from_secs(get("--app-timeout").and_then(|s| s.parse().ok()).unwrap_or(150)),
                type_text: get("--type"),
            }) };
        std::process::exit(rm_viewer::ui::run(opts));
    }
    #[cfg(not(windows))]
    {
        let _ = (relay, session, token, prompt);
        eprintln!("the RemoteMac viewer needs Windows (the portable parts are covered by `cargo test -p rm-viewer`)");
        std::process::exit(2);
    }
}
