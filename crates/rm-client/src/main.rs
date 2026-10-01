use rm_client::Session;
use rm_relay::Role;

fn usage() -> ! {
    eprintln!("usage: remote-mac --relay HOST:PORT --session ID [--launch APP_ID]\n       token from $RM_SESSION_TOKEN");
    std::process::exit(2)
}

fn main() {
    let (mut relay, mut session, mut launch) = (None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--relay" => relay = args.next(),
            "--session" => session = args.next(),
            "--launch" => launch = args.next(),
            _ => usage(),
        }
    }
    let (Some(relay), Some(session)) = (relay, session) else { usage() };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_else(|_| usage());
    let stream = rm_relay::join(&relay, &session, Role::Client, &token).unwrap_or_else(|e| fail("relay", e));
    let mut s = Session::handshake(stream).unwrap_or_else(|e| fail("handshake", e));
    println!("connected (protocol v{})", s.negotiated.version);
    println!("capabilities: {}", serde_json::to_string_pretty(&s.capabilities).unwrap());
    if !s.capabilities.can_stream_apps() {
        println!("WARNING: host cannot capture+drive app windows; streaming is unavailable.");
    }
    for a in s.list_apps().unwrap_or_else(|e| fail("list", e)) {
        println!("  {} ({}) available={}", a.name, a.id, a.available);
    }
    if let Some(id) = launch {
        let pid = s.launch(&id, vec![]).unwrap_or_else(|e| fail("launch", e));
        println!("launched {id} pid={pid}");
    }
}

fn fail(what: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("{what}: {e}");
    std::process::exit(1)
}
