//! Client end of the UDP video path: registers with the relay (same host and port as TCP),
//! rebuilds frames from FEC-protected shards, reports what arrived every 200 ms (the agent
//! sends video over UDP only while these reports come in, so a blocked UDP path falls back to
//! TCP by itself) and measures the round trip and the agent's clock offset with pings.
//!
//! Like Moonlight, the picture should not take a detour: once both sides have swapped their
//! addresses (`Message::P2pOffer`) they punch through to each other and, if that works, video,
//! reports, pings and input go straight between the PC and the Mac (same LAN, or across NATs).
//! The relay stays the meeting point and the fallback.

use rm_gamestream::depacketizer::Depacketizer;
use rm_protocol::udp::{self, InputQueue, Out, Reassembler};
use std::collections::HashMap;
use rm_relay::Role;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
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
    /// the Mac's address when UDP goes straight to it (None: through the relay)
    pub direct: Option<SocketAddr>,
}

/// Called once with our secret and candidates.
type OnOffer = Box<dyn FnOnce(String, Vec<String>) + Send>;

/// Direct-path state shared by the socket thread and the session.
struct P2p {
    secret: [u8; 16],
    /// the agent's secret and candidates, from its offer
    peer: Option<([u8; 16], Vec<SocketAddr>, Instant)>,
    direct: Option<SocketAddr>,
    last_direct: Instant,
    /// every address a valid punch came from (the agent may answer from another than we use)
    verified: Vec<SocketAddr>,
    input: InputQueue,
}

pub struct UdpVideo {
    pub stats: Arc<Mutex<LinkStats>>,
    stop: Arc<AtomicBool>,
    epoch: Instant,
    p2p: Arc<Mutex<P2p>>,
    sock: UdpSocket,
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

    /// The agent's `P2pOffer` arrived: start punching to its candidates.
    pub fn peer_offer(&self, secret: &str, candidates: &[String]) {
        let Some(sec) = udp::unhex16(secret) else { return };
        let ours_v4 = self.sock.local_addr().is_ok_and(|a| a.is_ipv4());
        let cands: Vec<SocketAddr> = candidates.iter().filter_map(|c| c.parse().ok()).filter(|a: &SocketAddr| a.is_ipv4() == ours_v4).collect();
        if let Ok(mut p) = self.p2p.lock() {
            p.peer = Some((sec, cands, Instant::now()));
        }
    }

    /// Send an input message straight to the Mac (reliable, in order). False when there is no
    /// direct path: send it over TCP then.
    pub fn send_input(&self, json: Vec<u8>, pointer_move: bool) -> bool {
        let Ok(mut p) = self.p2p.lock() else { return false };
        let Some(to) = p.direct else { return false };
        let d = p.input.push(json, pointer_move, Instant::now());
        let _ = self.sock.send_to(&d, to);
        true
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
/// `on_offer(secret, candidates)` is called once our addresses are known: send them to the agent
/// as `Message::P2pOffer` (RM_NO_P2P=1: never, everything stays on the relay).
pub fn start(relay: &str, session: &str, token: &str, on_out: impl FnMut(Out) + Send + 'static, on_offer: impl FnOnce(String, Vec<String>) + Send + 'static) -> std::io::Result<UdpVideo> {
    let addr = relay.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "relay address"))?;
    let bind = if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" };
    let sock = UdpSocket::bind(bind)?;
    rm_relay::big_udp_buffers(&sock);
    sock.set_read_timeout(Some(Duration::from_millis(5)))?;
    let stats = Arc::new(Mutex::new(LinkStats::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let epoch = Instant::now();
    let p2p = Arc::new(Mutex::new(P2p { secret: udp::random_secret(), peer: None, direct: None, last_direct: Instant::now(), verified: vec![], input: InputQueue::default() }));
    let register = rm_relay::udp_register(session, Role::Client, token, rm_relay::env_key().as_deref());
    let (s2, st2, stop2, p2) = (sock.try_clone()?, stats.clone(), stop.clone(), p2p.clone());
    let offer: Option<OnOffer> = std::env::var_os("RM_NO_P2P").is_none().then(|| Box::new(on_offer) as OnOffer);
    std::thread::Builder::new().name("rm-udp".into()).spawn(move || run(s2, addr, register, st2, stop2, epoch, p2, on_out, offer))?;
    Ok(UdpVideo { stats, stop, epoch, p2p, sock })
}

/// This machine's LAN address toward the internet (no packet is sent: connecting a UDP socket
/// only picks the route).
pub fn lan_ip(v4: bool) -> Option<std::net::IpAddr> {
    let s = UdpSocket::bind(if v4 { "0.0.0.0:0" } else { "[::]:0" }).ok()?;
    s.connect(if v4 { "8.8.8.8:53" } else { "[2001:4860:4860::8888]:53" }).ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

fn stun_servers(v4: bool) -> Vec<SocketAddr> {
    ["stun.l.google.com:19302", "stun.cloudflare.com:3478"]
        .iter()
        .filter_map(|h| h.to_socket_addrs().ok()?.find(|a| a.is_ipv4() == v4))
        .collect()
}

/// A private (LAN) address: preferred over a public one when both work.
fn is_private(a: &SocketAddr) -> bool {
    match a.ip() {
        std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
        std::net::IpAddr::V6(ip) => (ip.segments()[0] & 0xfe00) == 0xfc00 || ip.is_loopback(),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(sock: UdpSocket, relay: SocketAddr, register: Vec<u8>, stats: Arc<Mutex<LinkStats>>, stop: Arc<AtomicBool>, epoch: Instant, p2p: Arc<Mutex<P2p>>, mut on_out: impl FnMut(Out), mut offer: Option<OnOffer>) {
    let mut r = Reassembler::new();
    // GameStream (Sunshine-format) streams, one per window (RTP SSRC), and their sequence
    // numbers for the loss report
    let mut gs: HashMap<u32, (Depacketizer, Option<u16>)> = HashMap::new();
    let (mut gs_expected, mut gs_received, mut gs_recovered, mut gs_lost, mut gs_frames) = (0u32, 0u32, 0u64, 0u32, 0u32);
    let mut buf = vec![0u8; 2048];
    let (mut last_reg, mut last_fb, mut last_ping, mut last_video) = (None::<Instant>, Instant::now(), None::<Instant>, None::<Instant>);
    let mut registered = false;
    let (mut frames, mut bytes) = (0u64, 0u64);
    // our addresses: the LAN one now, the public one when STUN answers (or not, after a moment)
    let v4 = relay.is_ipv4();
    let port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
    let mut cands: Vec<String> = lan_ip(v4).map(|ip| SocketAddr::new(ip, port).to_string()).into_iter().collect();
    if relay.ip().is_loopback() {
        cands.push(SocketAddr::new(relay.ip(), port).to_string()); // tests: everything on one host
    }
    let stun = if offer.is_some() { stun_servers(v4) } else { vec![] };
    let stun_tx: [u8; 12] = udp::random_secret()[..12].try_into().unwrap();
    let (started, mut stun_sent) = (Instant::now(), 0u32);
    let mut last_punch = None::<Instant>;
    let dest = |p: &Mutex<P2p>| p.lock().ok().and_then(|p| p.direct).unwrap_or(relay);
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if offer.is_some() {
            if stun_sent < 3 && now.duration_since(started) >= Duration::from_millis(300 * stun_sent as u64) {
                for s in &stun {
                    let _ = sock.send_to(&udp::stun_request(&stun_tx), s);
                }
                stun_sent += 1;
            }
            if now.duration_since(started) >= Duration::from_millis(1200) {
                let secret = p2p.lock().map(|p| udp::hex(&p.secret)).unwrap_or_default();
                (offer.take().unwrap())(secret, std::mem::take(&mut cands));
            }
        }
        // punch to the agent's candidates until a direct path answers, then keep it open
        if let Ok(mut p) = p2p.lock() {
            let every = if p.direct.is_some() { Duration::from_secs(1) } else { Duration::from_millis(100) };
            if let Some((sec, list, since)) = &p.peer {
                if last_punch.is_none_or(|t| now.duration_since(t) >= every) {
                    // after 20 s without an answer, try again only now and then
                    let trying = p.direct.is_some() || now.duration_since(*since) < Duration::from_secs(20) || now.duration_since(*since).as_secs().is_multiple_of(10);
                    if trying {
                        let to: Vec<SocketAddr> = p.direct.map_or_else(|| list.clone(), |d| vec![d]);
                        for a in to {
                            let _ = sock.send_to(&udp::punch(sec, false), a);
                        }
                    }
                    last_punch = Some(now);
                }
            }
            if p.direct.is_some() && now.duration_since(p.last_direct) > Duration::from_secs(3) {
                eprintln!("direct path to the Mac lost; back through the relay");
                p.direct = None;
                p.verified.clear();
                if let Ok(mut s) = stats.lock() {
                    s.direct = None;
                }
            }
            let to = p.direct.unwrap_or(relay);
            if let Some(d) = p.input.resend(now, Duration::from_millis(40)) {
                let _ = sock.send_to(&d, to);
            }
        }
        // register until the relay answers, then refresh every 2 s (NAT bindings, relay restarts)
        let every = if registered { Duration::from_secs(2) } else { Duration::from_millis(300) };
        if last_reg.is_none_or(|t| now.duration_since(t) >= every) {
            let _ = sock.send_to(&register, relay);
            last_reg = Some(now);
        }
        if registered && now.duration_since(last_fb) >= Duration::from_millis(200) {
            let mut fb = r.take_stats();
            let rec_now: u64 = gs.values().map(|(d, _)| d.recovered).sum();
            fb.expected += gs_expected;
            fb.received += gs_received.min(gs_expected);
            fb.recovered += (rec_now - gs_recovered) as u32;
            fb.lost += gs_lost;
            fb.frames += gs_frames;
            (gs_expected, gs_received, gs_recovered, gs_lost, gs_frames) = (0, 0, rec_now, 0, 0);
            fb.rtt_ms = stats.lock().ok().and_then(|s| s.rtt_ms).map_or(0, |r| r.round() as u32);
            let _ = sock.send_to(&fb.encode(), dest(&p2p));
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
            let _ = sock.send_to(&udp::ping(epoch.elapsed().as_micros() as u64), dest(&p2p));
            last_ping = Some(now);
        }
        match sock.recv_from(&mut buf) {
            Ok((n, _)) if offer.is_some() && udp::parse_stun(&buf[..n], &stun_tx).is_some() => {
                let public = udp::parse_stun(&buf[..n], &stun_tx).unwrap().to_string();
                if !cands.contains(&public) {
                    cands.push(public);
                }
            }
            Ok((n, from)) if n >= 20 && buf[..2] == udp::MAGIC && buf[2] == udp::T_PUNCH => {
                let Some((sec, ack)) = udp::parse_punch(&buf[..n]) else { continue };
                let Ok(mut p) = p2p.lock() else { continue };
                if sec != p.secret {
                    continue;
                }
                if !ack {
                    if let Some((peer, ..)) = &p.peer {
                        let _ = sock.send_to(&udp::punch(peer, true), from);
                    }
                }
                // an ack proves both directions work: the first address that acks, or a LAN one
                // over a public one, becomes the path
                if ack && (p.direct.is_none() || (p.direct != Some(from) && is_private(&from) && !p.direct.is_some_and(|d| is_private(&d)))) {
                    eprintln!("direct path to the Mac: {from}");
                    p.direct = Some(from);
                    if let Ok(mut s) = stats.lock() {
                        s.direct = Some(from);
                        s.rtt_ms = None; // a new path: measure again
                    }
                }
                if !p.verified.contains(&from) {
                    p.verified.push(from);
                }
                p.last_direct = now;
            }
            Ok((n, from)) if n >= 3 && buf[..2] == udp::MAGIC && (from == relay || p2p.lock().is_ok_and(|p| p.verified.contains(&from))) => {
                let p = &buf[..n];
                if from != relay {
                    if let Ok(mut q) = p2p.lock() {
                        q.last_direct = now;
                    }
                }
                match p[2] {
                    udp::T_INPUT_ACK => {
                        if let (Some(seq), Ok(mut q)) = (udp::parse_input_ack(p), p2p.lock()) {
                            q.input.ack(seq);
                        }
                    }
                    rm_relay::UDP_STATUS if n >= 4 && from == relay => {
                        registered = p[3] != rm_relay::UDP_REFUSED;
                        if let Ok(mut s) = stats.lock() {
                            s.ready = p[3] == rm_relay::UDP_PEER_READY;
                        }
                    }
                    udp::T_GS_VIDEO if n > udp::GS_TAG => {
                        last_video = Some(now);
                        bytes += n as u64;
                        let (w, h) = (u16::from_be_bytes([p[4], p[5]]), u16::from_be_bytes([p[6], p[7]]));
                        let pkt = &p[udp::GS_TAG..];
                        let Some(hd) = rm_gamestream::depacketizer::header(pkt) else { continue };
                        let seq = u16::from_be_bytes([pkt[2], pkt[3]]);
                        let e = gs.entry(hd.ssrc).or_insert_with(|| (Depacketizer::new(udp::GS_PACKET_SIZE), None));
                        gs_received += 1;
                        match e.1 {
                            None => gs_expected += 1,
                            Some(hi) => {
                                let d = seq.wrapping_sub(hi) as i16;
                                if d > 0 {
                                    gs_expected += d as u32;
                                }
                            }
                        }
                        if e.1.is_none_or(|hi| (seq.wrapping_sub(hi) as i16) > 0) {
                            e.1 = Some(seq);
                        }
                        // the frame's capture time on the agent clock, from the 90 kHz RTP stamp
                        let agent_now = stats.lock().ok().and_then(|s| s.offset_us).map(|o| (epoch.elapsed().as_micros() as i64 + o).max(0) as u64);
                        for o in e.0.push(pkt) {
                            match o {
                                rm_gamestream::depacketizer::Out::Frame(f) => {
                                    frames += 1;
                                    gs_frames += 1;
                                    let pts_us = agent_now.map_or(0, |a| {
                                        let back = ((a.wrapping_mul(9) / 100) as u32).wrapping_sub(f.timestamp) as u64;
                                        a.saturating_sub(back * 100 / 9)
                                    });
                                    on_out(Out::Frame(rm_protocol::VideoFrame { window_id: hd.ssrc as u64, pts_us, keyframe: f.idr, codec: rm_protocol::CODEC_H264, width: w, height: h, data: f.data }));
                                }
                                rm_gamestream::depacketizer::Out::Lost(_) => {
                                    gs_lost += 1;
                                    on_out(Out::Lost(hd.ssrc as u64));
                                }
                            }
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
