use rm_agent::{serve, ProcessLauncher};
use rm_core::AppRegistry;
use rm_protocol::CapabilityReport;
use rm_relay::Role;

fn usage() -> ! {
    eprintln!("usage: remote-agent --relay HOST:PORT --session ID [--capabilities probe.json] [--root DIR]\n       token is read from $RM_SESSION_TOKEN (never from argv)");
    std::process::exit(2)
}

fn main() {
    let (mut relay, mut session, mut caps_path, mut root) = (None, None, None, "/tmp/remote-mac-session".to_string());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--relay" => relay = args.next(),
            "--session" => session = args.next(),
            "--capabilities" => caps_path = args.next(),
            "--root" => root = args.next().unwrap_or_else(|| usage()),
            _ => usage(),
        }
    }
    let (Some(relay), Some(session)) = (relay, session) else { usage() };
    let token = std::env::var("RM_SESSION_TOKEN").unwrap_or_else(|_| usage());

    let caps = match caps_path {
        Some(p) => std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<CapabilityReport>(&s).ok())
            .unwrap_or_else(|| CapabilityReport::unknown("probe output missing or unparsable")),
        None => CapabilityReport::unknown("no probe run"),
    };
    eprintln!("remote-agent {} session={session} (token not logged)", env!("CARGO_PKG_VERSION"));
    eprintln!("can_stream_apps={}", caps.can_stream_apps());

    let registry = AppRegistry::default_macos(root);
    let mut launcher = ProcessLauncher::new();
    let mut stream = rm_relay::join(&relay, &session, Role::Agent, &token).unwrap_or_else(|e| {
        eprintln!("relay join failed: {e}");
        std::process::exit(1)
    });
    eprintln!("READY");
    if let Err(e) = serve(&mut stream, &registry, &caps, &mut launcher) {
        eprintln!("session ended: {e}");
        std::process::exit(1);
    }
}
