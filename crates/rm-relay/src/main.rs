use std::net::TcpListener;

fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:47900".into());
    let l = TcpListener::bind(&addr).unwrap_or_else(|e| {
        eprintln!("bind {addr}: {e}");
        std::process::exit(1)
    });
    let key = rm_relay::env_key();
    eprintln!("rm-relay listening on {addr} (plaintext transport; admission key {})", if key.is_some() { "required" } else { "off" });
    rm_relay::serve(l, rm_relay::Config { key, ..Default::default() });
}
