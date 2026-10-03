//! Mac Desktop over full GameStream as RemoteMac carries it: moonlight-common-c on the PC talks
//! to the client tunnel's local ports, the tunnel's messages cross to the host tunnel (here
//! directly; in the product over the ID/relay/P2P connection), which feeds the host session.

mod common;
use rm_gamestream::tunnel::{ClientTunnel, HostTunnel, Outbound, ToHost};
use std::sync::{Arc, Mutex, Weak};

#[test]
fn moonlight_client_streams_through_the_tunnel() {
    let client: Arc<Mutex<Weak<ClientTunnel>>> = Arc::new(Mutex::new(Weak::new()));
    let c2 = client.clone();
    let host = HostTunnel::start(
        common::KEY,
        20,
        Arc::new(move |o: Outbound| {
            let Some(c) = c2.lock().unwrap().upgrade() else { return };
            match o {
                Outbound::Udp { kind, data } => c.udp_from_host(kind, data),
                Outbound::TcpData { id, data } => c.tcp_from_host(id, data),
                Outbound::TcpClose { id } => c.tcp_close_from_host(id),
            }
        }),
    )
    .unwrap();
    let h = Arc::downgrade(&host);
    let ct = ClientTunnel::start(Arc::new(move |m: ToHost| {
        let Some(h) = h.upgrade() else { return };
        match m {
            ToHost::Udp { kind, data } => h.udp_in(kind, data),
            ToHost::TcpOpen { id } => h.tcp_open(id),
            ToHost::TcpData { id, data } => h.tcp_data(id, data),
            ToHost::TcpClose { id } => h.tcp_close(id),
        }
    }))
    .unwrap();
    *client.lock().unwrap() = Arc::downgrade(&ct);
    // the session's events go to the test's host loop
    let (tx, rx) = std::sync::mpsc::channel();
    let hh = host.clone();
    std::thread::spawn(move || loop {
        let e = hh.events.lock().unwrap().recv();
        match e {
            Ok(e) => {
                if tx.send(e).is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    });
    common::stream_and_check(host.session.clone(), rx, ct.rtsp_port);
}
