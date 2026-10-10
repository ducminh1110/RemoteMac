use rm_client::Session;

fn usage() -> ! {
    eprintln!("usage: remote-mac [--relay HOST:PORT] (--id ID --password PASS | --session NAME)\n       remote-mac --direct HOST[:PORT] --password PASS     (straight to the Mac: no ID, no relay) [--launch APP_ID | --e2e APP_ID | --record FILE --apps a,b [--settle SECS] [--shots DIR]]\n       --session takes its token from $RM_SESSION_TOKEN; a Mac on this network is found by its ID; otherwise the relay is --relay, $RM_RELAY or the one built in");
    std::process::exit(2)
}

fn main() {
    let (mut relay, mut session, mut launch, mut e2e) = (None, None, None, None);
    let (mut record, mut apps, mut settle, mut shots) = (None, None, None, None);
    let mut vanish = false;
    let mut direct: Option<String> = None;
    let (mut id, mut password) = (None, std::env::var("RM_PASSWORD").ok());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--relay" => relay = args.next(),
            // straight to the Mac at this address (IP or name, IPv4 or IPv6), any network
            "--direct" => direct = args.next(),
            "--session" => session = args.next(),
            "--id" => id = args.next(),
            "--password" => password = args.next(),
            "--launch" => launch = args.next(),
            "--e2e" => e2e = args.next(),
            "--record" => record = args.next(),
            "--apps" => apps = args.next(),
            "--settle" => settle = args.next().and_then(|s| s.parse::<u64>().ok()),
            "--shots" => shots = args.next(),
            // test aid: a viewer whose network goes away without a word (one ping, then silence
            // with the connection left open)
            "--vanish" => vanish = true,
            _ => usage(),
        }
    }
    let relay = relay.or_else(|| std::env::var("RM_RELAY").ok().filter(|r| !r.is_empty())).or_else(|| rm_protocol::session::default_relay().map(String::from));
    // ID + password (what the Mac prints) or the legacy session name + RM_SESSION_TOKEN
    let (session, token, wait) = match (session, id, password) {
        (Some(s), _, _) => (s, std::env::var("RM_SESSION_TOKEN").unwrap_or_else(|_| usage()), true),
        // straight to an address: the password alone, no ID (as Moonlight to Sunshine)
        (None, None, Some(pw)) if direct.is_some() => (rm_protocol::session::DIRECT.to_string(), rm_protocol::session::direct_token(&pw), false),
        (None, Some(id), Some(pw)) => {
            let id = rm_protocol::session::normalize_id(&id).unwrap_or_else(|| fail("--id", "expected the 9-digit ID the Mac prints"));
            (rm_protocol::session::relay_session(&id), rm_protocol::session::token(&id, &pw), false)
        }
        _ => usage(),
    };
    let rt = rm_protocol::session::relay_token(&session);
    let (stream, route) = match &direct {
        Some(d) => rm_relay::lan::connect_direct(d, &session, &rt),
        None => rm_relay::lan::connect(relay.as_deref(), &session, &rt, wait),
    }
    .unwrap_or_else(|e| fail("connect", e));
    eprintln!("connected {}", match &route {
        rm_relay::lan::Route::Lan(a) => format!("on this network ({a})"),
        rm_relay::lan::Route::Direct(a) => format!("straight to {a}"),
        rm_relay::lan::Route::Relay(r) => format!("through the relay {r}"),
    });
    // the password proved and the keys agreed end to end: everything after this is encrypted
    let (stream, keys) = rm_protocol::secure::client_tcp(stream, &session, &token).unwrap_or_else(|e| fail("secure handshake", e));
    eprintln!("end-to-end encrypted (ChaCha20-Poly1305)");
    let udp = std::env::var_os("RM_NO_UDP").is_none();
    let timed = e2e.is_some() || record.is_some();
    // the handshake gets a patient timeout (the Mac probes its encoder first) ...
    stream.get_ref().set_read_timeout(timed.then(|| std::time::Duration::from_secs(10))).ok();
    let sock = stream.get_ref().try_clone().unwrap_or_else(|e| fail("socket", e));
    let mut s = Session::handshake(stream).unwrap_or_else(|e| fail("handshake", e));
    if vanish {
        s.send(&rm_protocol::Message::Ping { nonce: 1 }).unwrap_or_else(|e| fail("ping", e));
        eprintln!("connected; now silent, as a viewer whose network is gone");
        std::thread::sleep(std::time::Duration::from_secs(60));
        std::process::exit(0);
    }
    if timed {
        // ... then a short one when UDP video comes in beside the TCP stream
        sock.set_read_timeout(Some(std::time::Duration::from_millis(if udp { 20 } else { 1000 }))).ok();
    }
    if udp && timed {
        if let Err(e) = s.attach_udp(&route.udp_relay(), &session, Some(&keys)) {
            eprintln!("UDP video unavailable: {e}");
        }
    }
    if let Some(app) = e2e {
        let r = rm_client::e2e::run(&mut s, &app);
        println!("E2E {}: {}/{} checks passed; frames={} decoded={} keyframes={} bytes={} fps={:.1} firstFrameMs={:?} titles={:?}",
            if r.all_ok() { "PASS" } else { "FAIL" }, r.checks.iter().filter(|c| c.1).count(), r.checks.len(),
            r.video_frames, r.decoded, r.keyframes, r.video_bytes, r.fps(), r.first_frame_ms, r.titles);
        if let Some(u) = s.udp_stats() {
            println!("UDP frames={} bytes={} recovered={} lost={} loss={:.1}% rtt_ms={} ready={} path={}", u.frames, u.bytes, u.recovered, u.lost, u.loss * 100.0,
                u.rtt_ms.map_or("-".into(), |r| format!("{r:.1}")), u.ready, u.direct.map_or("relay".into(), |d| format!("direct:{d}")));
        }
        std::process::exit(if r.all_ok() { 0 } else { 1 });
    }
    if let Some(path) = record {
        let apps: Vec<String> = apps.unwrap_or_else(|| "xcode".into()).split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap_or_else(|e| fail("create", e)));
        let plan = rm_client::record::Plan {
            apps,
            settle: std::time::Duration::from_secs(settle.unwrap_or(15)),
            max: std::time::Duration::from_secs(150),
            on_segment_end: Box::new(move |app| {
                // what the Mac's screen looks like at that moment, to compare with the Windows side
                if let Some(dir) = &shots {
                    let _ = std::process::Command::new("screencapture").args(["-x", &format!("{dir}/mac-{app}.png")]).status();
                }
            }),
        };
        let sum = rm_client::record::record(&mut s, &mut file, plan).unwrap_or_else(|e| fail("record", e));
        let empty: Vec<&str> = sum.apps.iter().filter(|a| a.1 == 0 || a.2 == 0).map(|a| a.0.as_str()).collect();
        println!("RECORD {}: {:?}", if empty.is_empty() { "OK" } else { "INCOMPLETE" }, sum.apps);
        std::process::exit(if empty.is_empty() { 0 } else { 1 });
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
