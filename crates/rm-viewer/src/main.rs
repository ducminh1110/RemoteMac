fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let has = |k: &str| args.iter().any(|a| a == k);
    let (Some(relay), Some(session)) = (get("--relay"), get("--session")) else {
        eprintln!("usage: RM_SESSION_TOKEN=.. remote-mac-viewer --relay HOST:PORT --session ID [--app testapp] [--raw-ctrl] [--no-clipboard] [--renderer d3d11|gdi] [--mac-file-panel] [--no-shortcuts] [--smoke] [--showcase DIR --apps xcode,textedit --settle SECS --type TEXT]");
        std::process::exit(2)
    };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_default();
    #[cfg(windows)]
    {
        let opts = rm_viewer::ui::Options { relay, session, token, app: get("--app"), ctrl_as_command: !has("--raw-ctrl"), smoke: has("--smoke"), clipboard: !has("--no-clipboard"), d3d: get("--renderer").as_deref() != Some("gdi"), windows_file_picker: !has("--mac-file-panel") && !has("--showcase"), shortcuts: !has("--no-shortcuts"),
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
        let _ = (relay, session, token, has("--smoke"));
        eprintln!("remote-mac-viewer needs Windows (the portable parts are covered by `cargo test -p rm-viewer`)");
        std::process::exit(2);
    }
}
