fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let has = |k: &str| args.iter().any(|a| a == k);
    let (Some(relay), Some(session)) = (get("--relay"), get("--session")) else {
        eprintln!("usage: RM_SESSION_TOKEN=.. remote-mac-viewer --relay HOST:PORT --session ID [--app testapp] [--raw-ctrl] [--smoke]");
        std::process::exit(2)
    };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_default();
    #[cfg(windows)]
    {
        let opts = rm_viewer::ui::Options { relay, session, token, app: get("--app"), ctrl_as_command: !has("--raw-ctrl"), smoke: has("--smoke") };
        std::process::exit(rm_viewer::ui::run(opts));
    }
    #[cfg(not(windows))]
    {
        let _ = (relay, session, token, has("--smoke"));
        eprintln!("remote-mac-viewer needs Windows (the portable parts are covered by `cargo test -p rm-viewer`)");
        std::process::exit(2);
    }
}
