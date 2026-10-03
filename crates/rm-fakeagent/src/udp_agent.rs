//! Agent end of the UDP video path, as the Swift agent does it (agent/macos/Udp.swift): register
//! with the relay, send each frame as FEC-protected shards while the client's reports keep
//! coming, answer pings with the agent clock. With the client's offer it punches a direct path
//! and then sends everything there, and takes input from it.

use rm_protocol::udp::{self, InputOrder};
use rm_protocol::{Message, VideoFrame};
use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The agent's clock (microseconds): frame pts and pong answers use it.
pub fn clock_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

#[derive(Default)]
struct Direct {
    peer: Option<([u8; 16], Vec<SocketAddr>)>,
    addr: Option<SocketAddr>,
    last: Option<Instant>,
    /// every address a valid punch came from (the peer may answer from another one than we use)
    verified: Vec<SocketAddr>,
}

pub struct AgentUdp {
    sock: UdpSocket,
    relay: SocketAddr,
    secret: [u8; 16],
    direct: Mutex<Direct>,
    last_report: Mutex<Option<Instant>>,
    seq: Mutex<HashMap<u64, u32>>,
    /// input that came over UDP goes to the session's message loop
    pub inputs: Mutex<Option<Sender<Message>>>,
    /// false: never take a direct path (tests of the relay path)
    pub p2p: std::sync::atomic::AtomicBool,
    pub fec_pct: AtomicU64,
    pub sent_frames: AtomicU64,
    pub input_count: AtomicU64,
}

impl AgentUdp {
    pub fn start(relay: &str, session: &str, token: &str) -> std::io::Result<Arc<Self>> {
        let addr = relay.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "relay address"))?;
        let sock = UdpSocket::bind(if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" })?;
        rm_relay::big_udp_buffers(&sock);
        sock.set_read_timeout(Some(Duration::from_millis(20)))?;
        let me = Arc::new(Self {
            sock,
            relay: addr,
            secret: udp::random_secret(),
            direct: Mutex::new(Direct::default()),
            last_report: Mutex::new(None),
            seq: Mutex::new(HashMap::new()),
            inputs: Mutex::new(None),
            p2p: std::sync::atomic::AtomicBool::new(true),
            fec_pct: AtomicU64::new(20),
            sent_frames: AtomicU64::new(0),
            input_count: AtomicU64::new(0),
        });
        let reg = rm_relay::udp_register(session, rm_relay::Role::Agent, token, rm_relay::env_key().as_deref());
        let m = me.clone();
        std::thread::spawn(move || m.run(reg));
        Ok(me)
    }

    /// Our side of the direct path (LAN address only: the fake agent runs next to its client).
    pub fn offer(&self) -> Message {
        let port = self.sock.local_addr().map(|a| a.port()).unwrap_or(0);
        let mut candidates: Vec<String> = rm_client_lan_ip(self.relay.is_ipv4()).map(|ip| SocketAddr::new(ip, port).to_string()).into_iter().collect();
        if self.relay.ip().is_loopback() {
            candidates.push(SocketAddr::new(self.relay.ip(), port).to_string());
        }
        Message::P2pOffer { secret: udp::hex(&self.secret), candidates }
    }

    pub fn peer_offer(&self, secret: &str, candidates: &[String]) {
        if let Some(s) = udp::unhex16(secret) {
            self.direct.lock().unwrap().peer = Some((s, candidates.iter().filter_map(|c| c.parse().ok()).collect()));
        }
    }

    /// Where UDP goes now: the direct path when there is one.
    pub fn direct(&self) -> Option<SocketAddr> {
        self.direct.lock().unwrap().addr
    }

    fn dest(&self) -> SocketAddr {
        self.direct().unwrap_or(self.relay)
    }

    fn run(self: Arc<Self>, reg: Vec<u8>) {
        let mut buf = vec![0u8; 2048];
        let (mut registered, mut last_reg, mut last_punch) = (false, None::<Instant>, None::<Instant>);
        let mut order = InputOrder::default();
        // the session holds the other reference; when it is gone, stop
        while Arc::strong_count(&self) > 1 {
            let every = if registered { Duration::from_secs(2) } else { Duration::from_millis(300) };
            if last_reg.is_none_or(|t: Instant| t.elapsed() >= every) {
                let _ = self.sock.send_to(&reg, self.relay);
                last_reg = Some(Instant::now());
            }
            {
                let mut d = self.direct.lock().unwrap();
                let every = if d.addr.is_some() { Duration::from_secs(1) } else { Duration::from_millis(100) };
                if let Some((sec, list)) = d.peer.as_ref().filter(|_| self.p2p.load(Ordering::Relaxed)) {
                    if last_punch.is_none_or(|t: Instant| t.elapsed() >= every) {
                        for a in d.addr.map_or_else(|| list.clone(), |a| vec![a]) {
                            let _ = self.sock.send_to(&udp::punch(sec, false), a);
                        }
                        last_punch = Some(Instant::now());
                    }
                }
                if d.last.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                    d.addr = None;
                    d.last = None;
                    d.verified.clear();
                }
            }
            let Ok((n, from)) = self.sock.recv_from(&mut buf) else { continue };
            let p = &buf[..n];
            if n < 3 || p[..2] != udp::MAGIC {
                continue;
            }
            if let Some((sec, ack)) = udp::parse_punch(p).filter(|_| self.p2p.load(Ordering::Relaxed)) {
                let mut d = self.direct.lock().unwrap();
                if sec == self.secret {
                    if !ack {
                        if let Some((peer, _)) = &d.peer {
                            let _ = self.sock.send_to(&udp::punch(peer, true), from);
                        }
                    }
                    if !d.verified.contains(&from) {
                        d.verified.push(from);
                    }
                    if ack && d.addr.is_none() {
                        d.addr = Some(from); // both directions work
                    }
                    d.last = Some(Instant::now());
                }
                continue;
            }
            if from != self.relay && !self.direct.lock().unwrap().verified.contains(&from) {
                continue;
            }
            match p[2] {
                rm_relay::UDP_STATUS if n >= 4 => registered = p[3] != rm_relay::UDP_REFUSED,
                udp::T_FEEDBACK => {
                    if let Some(f) = udp::Feedback::decode(p) {
                        *self.last_report.lock().unwrap() = Some(Instant::now());
                        // stronger FEC on a lossy link (Moonlight-style adaptive parity)
                        let pct = (10.0 + f.loss() * 300.0).clamp(10.0, 50.0) as u64;
                        if f.expected > 0 {
                            self.fec_pct.store(pct, Ordering::Relaxed);
                        }
                    }
                }
                udp::T_PING => {
                    if let Some(t) = udp::parse_ping(p) {
                        let _ = self.sock.send_to(&udp::pong(t, clock_us()), from);
                    }
                }
                udp::T_INPUT => {
                    let (msgs, ack) = order.take(p);
                    for j in msgs {
                        if let (Ok(m), Some(tx)) = (serde_json::from_slice::<Message>(j), self.inputs.lock().unwrap().as_ref()) {
                            self.input_count.fetch_add(1, Ordering::Relaxed);
                            let _ = tx.send(m);
                        }
                    }
                    if let Some(a) = ack {
                        let _ = self.sock.send_to(&a, from);
                    }
                }
                _ => {}
            }
        }
    }

    /// The client's reports are arriving: video can go this way.
    pub fn alive(&self) -> bool {
        self.last_report.lock().unwrap().is_some_and(|t| t.elapsed() < Duration::from_millis(1500))
    }

    pub fn send(&self, v: &VideoFrame) {
        let seq = {
            let mut s = self.seq.lock().unwrap();
            let e = s.entry(v.window_id).or_insert(0);
            *e = e.wrapping_add(1);
            *e
        };
        let to = self.dest();
        for d in udp::packetize(v, seq, self.fec_pct.load(Ordering::Relaxed) as u32) {
            let _ = self.sock.send_to(&d, to);
        }
        self.sent_frames.fetch_add(1, Ordering::Relaxed);
    }
}

/// This machine's LAN address (as rm-client's `lan_ip`; the fake agent does not depend on it).
fn rm_client_lan_ip(v4: bool) -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind(if v4 { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    s.connect(if v4 { "8.8.8.8:53" } else { "[2001:4860:4860::8888]:53" }).ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}
