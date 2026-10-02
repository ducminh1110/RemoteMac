fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let (Some(relay), Some(session)) = (get("--relay"), get("--session")) else {
        eprintln!("usage: RM_SESSION_TOKEN=.. rm-fakeagent --relay HOST:PORT --session ID [--replay FILE.rmrec]");
        std::process::exit(2)
    };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_default();
    let r = match get("--replay") {
        Some(path) => rm_fakeagent::replay::Recording::load(&path).and_then(|rec| rm_fakeagent::replay::replay_via_relay(&relay, &session, &token, rec)),
        None => rm_fakeagent::serve_via_relay(&relay, &session, &token),
    };
    if let Err(e) = r {
        eprintln!("fake agent: {e}");
        std::process::exit(1);
    }
}
