use std::net::TcpListener;

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:47900".into());
    let l = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("bind {addr}: {e}");
        std::process::exit(1)
    });
    let key = rm_relay::env_key();
    eprintln!("rm-relay listening on {addr} (sessions are end-to-end encrypted by the two sides; admission key {})", if key.is_some() { "required" } else { "off" });
    let throttle_kbps = std::env::var("RM_RELAY_THROTTLE_KBPS").ok().and_then(|v| v.parse().ok());
    if let Some(k) = throttle_kbps {
        eprintln!("test mode: each direction limited to {k} kbit/s");
    }
    let udp_loss = std::env::var("RM_RELAY_UDP_LOSS_PCT").ok().and_then(|v| v.parse::<f64>().ok()).map(|p| p / 100.0);
    if let Some(p) = udp_loss {
        eprintln!("test mode: dropping {:.1}% of UDP datagrams", p * 100.0);
    }
    eprintln!("UDP forwarding on {addr} (video; open this port for UDP too)");
    let ids_path = rm_relay::ids::default_path();
    match &ids_path {
        Some(p) => eprintln!("Mac IDs handed out are kept in {}", p.display()),
        None => eprintln!("Mac IDs handed out are kept in memory only (set RM_RELAY_IDS=FILE to keep them)"),
    }
    rm_relay::serve(l, rm_relay::Config { key, throttle_kbps, udp_loss, ids_path, ..Default::default() });
}
