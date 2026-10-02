//! Agent end of the UDP video path, as the Swift agent does it (agent/macos/Udp.swift): register
//! with the relay, send each frame as FEC-protected shards while the client's reports keep
//! coming, answer pings with the agent clock.

use rm_protocol::udp;
use rm_protocol::VideoFrame;
use std::collections::HashMap;
use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The agent's clock (microseconds): frame pts and pong answers use it.
pub fn clock_us() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as u64
}

pub struct AgentUdp {
    sock: UdpSocket,
    last_report: Mutex<Option<Instant>>,
    seq: Mutex<HashMap<u64, u32>>,
    pub fec_pct: AtomicU64,
    pub sent_frames: AtomicU64,
}

impl AgentUdp {
    pub fn start(relay: &str, session: &str, token: &str) -> std::io::Result<Arc<Self>> {
        let addr = relay.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "relay address"))?;
        let sock = UdpSocket::bind(if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" })?;
        sock.connect(addr)?;
        rm_relay::big_udp_buffers(&sock);
        sock.set_read_timeout(Some(Duration::from_millis(50)))?;
        let me = Arc::new(Self { sock, last_report: Mutex::new(None), seq: Mutex::new(HashMap::new()), fec_pct: AtomicU64::new(20), sent_frames: AtomicU64::new(0) });
        let reg = rm_relay::udp_register(session, rm_relay::Role::Agent, token, rm_relay::env_key().as_deref());
        let m = me.clone();
        std::thread::spawn(move || m.run(reg));
        Ok(me)
    }

    fn run(self: Arc<Self>, reg: Vec<u8>) {
        let mut buf = vec![0u8; 2048];
        let (mut registered, mut last_reg) = (false, None::<Instant>);
        // the session holds the other reference; when it is gone, stop
        while Arc::strong_count(&self) > 1 {
            let every = if registered { Duration::from_secs(2) } else { Duration::from_millis(300) };
            if last_reg.is_none_or(|t: Instant| t.elapsed() >= every) {
                let _ = self.sock.send(&reg);
                last_reg = Some(Instant::now());
            }
            let Ok(n) = self.sock.recv(&mut buf) else { continue };
            let p = &buf[..n];
            if n < 3 || p[..2] != udp::MAGIC {
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
                        let _ = self.sock.send(&udp::pong(t, clock_us()));
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
        for d in udp::packetize(v, seq, self.fec_pct.load(Ordering::Relaxed) as u32) {
            let _ = self.sock.send(&d);
        }
        self.sent_frames.fetch_add(1, Ordering::Relaxed);
    }
}
