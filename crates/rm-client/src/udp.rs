//! Client end of the UDP video path: registers with the relay (same host and port as TCP),
//! rebuilds frames from FEC-protected shards, reports what arrived every 200 ms (the agent
//! sends video over UDP only while these reports come in, so a blocked UDP path falls back to
//! TCP by itself) and measures the round trip and the agent's clock offset with pings.

use rm_protocol::udp::{self, Out, Reassembler};
use rm_relay::Role;
use std::net::{ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Link numbers for the stats overlay / logs.
#[derive(Debug, Clone, Default)]
pub struct LinkStats {
    /// the relay accepted us and the agent is registered too
    pub ready: bool,
    /// UDP video seen in the last second
    pub active: bool,
    pub frames: u64,
    pub bytes: u64,
    /// frames rebuilt from parity, frames lost anyway
    pub recovered: u64,
    pub lost: u64,
    /// packet loss on the link (before FEC) over the last report period
    pub loss: f64,
    /// round trip to the agent (through the relay), smoothed
    pub rtt_ms: Option<f64>,
    /// agent clock minus ours, microseconds (frame pts are agent capture times)
    pub offset_us: Option<i64>,
}

pub struct UdpVideo {
    pub stats: Arc<Mutex<LinkStats>>,
    stop: Arc<AtomicBool>,
    epoch: Instant,
}

impl UdpVideo {
    /// Our clock as the pings use it (microseconds since start).
    pub fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    /// The agent's clock now, if the offset is known (to turn a frame's pts into its age).
    pub fn agent_now_us(&self) -> Option<i64> {
        self.stats.lock().ok()?.offset_us.map(|o| self.now_us() as i64 + o)
    }
}

impl Drop for UdpVideo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Start the UDP path for a paired session. `on_out` gets every rebuilt frame and every
/// unrecoverable loss (ask the agent for a keyframe then). Never fails the session: if UDP is
/// blocked, the agent just keeps sending video over TCP.
pub fn start(relay: &str, session: &str, token: &str, on_out: impl FnMut(Out) + Send + 'static) -> std::io::Result<UdpVideo> {
    let addr = relay.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "relay address"))?;
    let bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let sock = UdpSocket::bind(bind)?;
    sock.connect(addr)?;
    rm_relay::big_udp_buffers(&sock);
    sock.set_read_timeout(Some(Duration::from_millis(5)))?;
    let stats = Arc::new(Mutex::new(LinkStats::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let epoch = Instant::now();
    let register = rm_relay::udp_register(session, Role::Client, token, rm_relay::env_key().as_deref());
    let (s2, st2, stop2) = (sock.try_clone()?, stats.clone(), stop.clone());
    std::thread::Builder::new().name("rm-udp".into()).spawn(move || run(s2, register, st2, stop2, epoch, on_out))?;
    Ok(UdpVideo { stats, stop, epoch })
}

fn run(sock: UdpSocket, register: Vec<u8>, stats: Arc<Mutex<LinkStats>>, stop: Arc<AtomicBool>, epoch: Instant, mut on_out: impl FnMut(Out)) {
    let mut r = Reassembler::new();
    let mut buf = vec![0u8; 2048];
    let (mut last_reg, mut last_fb, mut last_ping, mut last_video) = (None::<Instant>, Instant::now(), None::<Instant>, None::<Instant>);
    let mut registered = false;
    let (mut frames, mut bytes) = (0u64, 0u64);
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        // register until the relay answers, then refresh every 2 s (NAT bindings, relay restarts)
        let every = if registered { Duration::from_secs(2) } else { Duration::from_millis(300) };
        if last_reg.is_none_or(|t| now.duration_since(t) >= every) {
            let _ = sock.send(&register);
            last_reg = Some(now);
        }
        if registered && now.duration_since(last_fb) >= Duration::from_millis(200) {
            let mut fb = r.take_stats();
            fb.rtt_ms = stats.lock().ok().and_then(|s| s.rtt_ms).map_or(0, |r| r.round() as u32);
            let _ = sock.send(&fb.encode());
            last_fb = now;
            if let Ok(mut s) = stats.lock() {
                s.recovered += fb.recovered as u64;
                s.lost += fb.lost as u64;
                if fb.expected > 0 {
                    s.loss = fb.loss();
                }
                s.frames = frames;
                s.bytes = bytes;
                s.active = last_video.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1));
            }
        }
        if registered && last_ping.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(250)) {
            let _ = sock.send(&udp::ping(epoch.elapsed().as_micros() as u64));
            last_ping = Some(now);
        }
        match sock.recv(&mut buf) {
            Ok(n) if n >= 3 && buf[..2] == udp::MAGIC => {
                let p = &buf[..n];
                match p[2] {
                    rm_relay::UDP_STATUS if n >= 4 => {
                        registered = p[3] != rm_relay::UDP_REFUSED;
                        if let Ok(mut s) = stats.lock() {
                            s.ready = p[3] == rm_relay::UDP_PEER_READY;
                        }
                    }
                    udp::T_VIDEO => {
                        last_video = Some(now);
                        bytes += n as u64;
                        for o in r.push(p, now) {
                            frames += matches!(o, Out::Frame(_)) as u64;
                            on_out(o);
                        }
                    }
                    udp::T_PONG => {
                        if let Some((t, agent)) = udp::parse_pong(p) {
                            let ours = epoch.elapsed().as_micros() as u64;
                            let rtt = ours.saturating_sub(t);
                            if let Ok(mut s) = stats.lock() {
                                let ms = rtt as f64 / 1000.0;
                                s.rtt_ms = Some(s.rtt_ms.map_or(ms, |r| r * 0.5 + ms * 0.5));
                                // the agent stamped its clock about half a round trip ago
                                let off = agent as i64 - (t as i64 + rtt as i64 / 2);
                                // keep the estimate from the fastest round trips (least queueing)
                                if s.offset_us.is_none() || rtt < 2 * s.rtt_ms.map_or(u64::MAX, |r| (r * 1000.0) as u64) {
                                    s.offset_us = Some(s.offset_us.map_or(off, |o| (o * 7 + off) / 8));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
        for o in r.tick(Instant::now()) {
            on_out(o);
        }
    }
}
