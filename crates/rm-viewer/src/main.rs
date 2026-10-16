// Release builds are a GUI app (no console window); errors go to the connect window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    // before anything can go wrong: crash.log and minidumps, with or without --logs-enabled
    rm_viewer::crash::install();
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let has = |k: &str| args.iter().any(|a| a == k);
    if has("-h") || has("--help") {
        eprintln!("usage: Taro.exe                      (asks for the Mac's ID and password)\n       Taro.exe --id 123456789 --password PASS\n       RM_SESSION_TOKEN=.. Taro.exe --session NAME   (legacy)\n  [--relay HOST:PORT | --direct HOST[:PORT]] [--app testapp] [--logs-enabled] [--raw-ctrl] [--no-clipboard] [--renderer d3d11|gdi] [--mac-file-panel] [--no-shortcuts] [--smoke] [--showcase DIR --apps xcode,textedit --settle SECS --type TEXT]");
        std::process::exit(0)
    }
    // started again by Taro itself (the guided tour leaving its demo Mac): once the old one is
    // gone, so that this one is not handed over to it as a second copy
    #[cfg(windows)]
    if let Some(pid) = get("--wait-pid").and_then(|p| p.parse::<u32>().ok()) {
        rm_viewer::native::wait_for_process(pid, std::time::Duration::from_secs(10));
    }
    // Taro's icon as an .ico file (the installer and its shortcuts take it)
    if let Some(path) = get("--write-icon") {
        let ico = rm_viewer::ico::rgba_to_ico(256, &rm_viewer::logo::rgba(256)).expect("icon");
        std::process::exit(if std::fs::write(&path, ico).is_ok() { 0 } else { 1 })
    }
    // a Mac on this network is found by its ID; for one elsewhere: --relay, $RM_RELAY, the relay
    // built into this build (none in the source), or what the connect window is given
    let relay = get("--relay").or_else(|| std::env::var("RM_RELAY").ok().filter(|r| !r.is_empty())).or_else(|| rm_protocol::session::default_relay().map(String::from));
    // or straight to the Mac at an address (IP or host name, IPv4 or IPv6), any network
    rm_viewer::net::set_direct(get("--direct"));
    // a taro:// link from a Taro Enterprise portal: its grant is traded once for the rented
    // Mac's ID, relay and password, and Taro connects with them
    #[cfg(windows)]
    let mut relay = relay;
    #[cfg(windows)]
    let from_link = match get("--url") {
        Some(url) => {
            let quiet = has("--smoke") || has("--showcase");
            let fail = |why: String| -> ! {
                if !quiet {
                    rm_viewer::native::message_box("Taro", &format!("This link could not be opened:\n\n{why}"));
                }
                std::process::exit(1)
            };
            // never spend the one-time grant while another Taro holds the session
            if rm_viewer::launcher::running() {
                fail("Taro is already connected to a Mac. Disconnect first (the red Disconnect button in Taro), then open the link again from the portal.".into());
            }
            let link = rm_viewer::enterprise::parse_link(&url).unwrap_or_else(|e| fail(e));
            let mac = rm_viewer::enterprise::redeem(&link).unwrap_or_else(|e| fail(e));
            eprintln!("Taro Enterprise: connecting to {} ({})", mac.name, rm_protocol::session::display_id(&mac.taro_id));
            if let Some(r) = &mac.relay {
                relay = Some(r.clone());
            }
            Some((rm_protocol::session::relay_session(&mac.taro_id), rm_protocol::session::token(&mac.taro_id, &mac.password)))
        }
        None => None,
    };
    #[cfg(not(windows))]
    let from_link: Option<(String, String)> = None;
    // --session (legacy, token from RM_SESSION_TOKEN) | --id + --password | the connect window
    let (session, token, prompt) = match (get("--session"), get("--id"), get("--password").or_else(|| std::env::var("RM_PASSWORD").ok())) {
        _ if from_link.is_some() => {
            let (s, t) = from_link.clone().unwrap();
            (s, t, false)
        }
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
        // no console in the release build: a log file only when asked for (--logs-enabled), and
        // never die without a word
        if rm_viewer::logs_enabled() {
            rm_viewer::native::log_to_file(&rm_viewer::log_path());
        }
        if !has("--smoke") && !has("--showcase") {
            rm_viewer::native::register_url_scheme();
        }
        let quiet = has("--smoke") || has("--showcase");
        // crash.log's hook first, so the record is written before the message box waits
        let crash_log = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            crash_log(info);
            let msg = format!("Taro stopped because of an internal error:\n\n{info}\n\n{}", rm_viewer::log_hint());
            eprintln!("{msg}");
            if !quiet {
                rm_viewer::native::message_box("Taro", &msg);
            }
        }));
        eprintln!("Taro {} starting (relay {})", env!("CARGO_PKG_VERSION"), relay.as_deref().unwrap_or("none: this network only"));
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
        eprintln!("Taro needs Windows (the portable parts are covered by `cargo test -p rm-viewer`)");
        std::process::exit(2);
    }
}
