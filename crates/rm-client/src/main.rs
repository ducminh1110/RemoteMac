use rm_client::Session;
use rm_relay::Role;

fn usage() -> ! {
    eprintln!("usage: remote-mac --relay HOST:PORT --session ID [--launch APP_ID | --e2e APP_ID]\n       token from $RM_SESSION_TOKEN");
    std::process::exit(2)
}

fn main() {
    let (mut relay, mut session, mut launch, mut e2e) = (None, None, None, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--relay" => relay = args.next(),
            "--session" => session = args.next(),
            "--launch" => launch = args.next(),
            "--e2e" => e2e = args.next(),
            _ => usage(),
        }
    }
    let (Some(relay), Some(session)) = (relay, session) else { usage() };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_else(|_| usage());
    let stream = rm_relay::join(&relay, &session, Role::Client, &token).unwrap_or_else(|e| fail("relay", e));
    if e2e.is_some() {
        stream.set_read_timeout(Some(std::time::Duration::from_secs(1))).ok();
    }
    let mut s = Session::handshake(stream).unwrap_or_else(|e| fail("handshake", e));
    if let Some(app) = e2e {
        let r = rm_client::e2e::run(&mut s, &app);
        println!("E2E {}: {}/{} checks passed; frames={} decoded={} keyframes={} bytes={} fps={:.1} firstFrameMs={:?} titles={:?}",
            if r.all_ok() { "PASS" } else { "FAIL" }, r.checks.iter().filter(|c| c.1).count(), r.checks.len(),
            r.video_frames, r.decoded, r.keyframes, r.video_bytes, r.fps(), r.first_frame_ms, r.titles);
        std::process::exit(if r.all_ok() { 0 } else { 1 });
    }
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
