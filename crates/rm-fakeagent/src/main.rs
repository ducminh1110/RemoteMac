fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let (Some(relay), Some(session)) = (get("--relay"), get("--session")) else {
        eprintln!("usage: RM_SESSION_TOKEN=.. rm-fakeagent --relay HOST:PORT --session ID");
        std::process::exit(2)
    };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_default();
    if let Err(e) = rm_fakeagent::serve_via_relay(&relay, &session, &token) {
        eprintln!("fake agent: {e}");
        std::process::exit(1);
    }
}
