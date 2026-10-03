//! A GameStream host session, ported from Sunshine (rtsp.cpp, stream.cpp, input.cpp): what
//! Moonlight's moonlight-common-c connects to. One [`Session`] streams one picture (a whole
//! display, or one app window); unlike Sunshine any number of sessions can run at once, each
//! on its own ports.
//!
//! The session key (`remoteInputAesKey`) is not exchanged over GameStream's HTTPS pairing but
//! handed to both ends by RemoteMac's own authenticated connection.

pub mod crypto;
pub mod depacketizer;
pub mod input;
pub mod video;

use moonlight_sys::host as enet;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

pub use input::Input;

const SS_ENC_CONTROL_V2: u32 = 0x01;
const T_START_A: u16 = 0x0305;
const T_START_B: u16 = 0x0307;
const T_INVALIDATE_REF_FRAMES: u16 = 0x0301;
const T_LOSS_STATS: u16 = 0x0201;
const T_INPUT_DATA: u16 = 0x0206;
const T_TERMINATION: u16 = 0x0109;
const T_PERIODIC_PING: u16 = 0x0200;
const T_REQUEST_IDR: u16 = 0x0302;
const T_ENCRYPTED: u16 = 0x0001;

/// What the client asked for in its ANNOUNCE.
#[derive(Debug, Clone, Default)]
pub struct StreamParams {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub packet_size: usize,
    pub min_fec_packets: usize,
    /// 0 H.264, 1 HEVC, 2 AV1
    pub video_format: u32,
    pub encryption_enabled: u32,
    pub control_protocol: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The client finished the RTSP handshake: start capturing at this size and rate.
    Started { width: u32, height: u32, fps: u32, bitrate_kbps: u32 },
    /// The client lost a frame: send an IDR (Sunshine raises idr_events).
    RequestIdr,
    Input(Input),
    /// The client went away (or asked to stop).
    Ended,
}

pub struct Config {
    /// AES-128 key shared with the client (`StreamConfig.remoteInputAesKey`)
    pub key: [u8; 16],
    /// address to bind (0.0.0.0, or 127.0.0.1 behind a tunnel)
    pub bind: IpAddr,
    /// FEC parity, percent (Sunshine's default is 20)
    pub fec_percentage: usize,
}

struct Shared {
    key: [u8; 16],
    stop: AtomicBool,
    params: Mutex<Option<StreamParams>>,
    events: Mutex<Sender<Event>>,
    ping_payload: String,
    connect_data: u32,
    /// where the client receives video (learned from its pings)
    video_peer: Mutex<Option<SocketAddr>>,
}

impl Shared {
    fn emit(&self, e: Event) {
        let _ = self.events.lock().unwrap().send(e);
    }
}

pub struct Session {
    shared: Arc<Shared>,
    pub rtsp_port: u16,
    pub video_port: u16,
    pub audio_port: u16,
    pub control_port: u16,
    video: UdpSocket,
    packetizer: Mutex<video::Packetizer>,
    fec_percentage: usize,
    frame_index: Mutex<u32>,
    epoch: Instant,
}

fn random_alnum(n: usize) -> String {
    let mut b = vec![0u8; n];
    let _ = getrandom::getrandom(&mut b);
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    b.iter().map(|x| A[*x as usize % A.len()] as char).collect()
}

fn enet_init() {
    static I: Once = Once::new();
    I.call_once(|| unsafe {
        enet::rm_enet_init();
    });
}

impl Session {
    /// Bind the RTSP, video, audio and control ports (any free ones) and start serving.
    pub fn start(cfg: Config) -> std::io::Result<(Arc<Session>, Receiver<Event>)> {
        enet_init();
        let rtsp = TcpListener::bind(SocketAddr::new(cfg.bind, 0))?;
        let video = UdpSocket::bind(SocketAddr::new(cfg.bind, 0))?;
        let audio = UdpSocket::bind(SocketAddr::new(cfg.bind, 0))?;
        let host = unsafe { enet::rm_enet_server(0, cfg.bind.is_ipv6() as i32, 4, 0x30) };
        if host.is_null() {
            return Err(std::io::Error::other("ENet host"));
        }
        let control_port = unsafe { enet::rm_enet_port(host) };
        let (tx, rx) = channel();
        let mut cd = [0u8; 4];
        let _ = getrandom::getrandom(&mut cd);
        let shared = Arc::new(Shared {
            key: cfg.key,
            stop: AtomicBool::new(false),
            params: Mutex::new(None),
            events: Mutex::new(tx),
            ping_payload: random_alnum(16),
            connect_data: u32::from_le_bytes(cd),
            video_peer: Mutex::new(None),
        });
        let s = Arc::new(Session {
            rtsp_port: rtsp.local_addr()?.port(),
            video_port: video.local_addr()?.port(),
            audio_port: audio.local_addr()?.port(),
            control_port,
            video: video.try_clone()?,
            packetizer: Mutex::new(video::Packetizer::new(1024, cfg.fec_percentage, 0)),
            fec_percentage: cfg.fec_percentage,
            frame_index: Mutex::new(0),
            epoch: Instant::now(),
            shared: shared.clone(),
        });
        let ports = (s.video_port, s.audio_port, s.control_port);
        let sh = shared.clone();
        std::thread::Builder::new().name("gs-rtsp".into()).spawn(move || rtsp_loop(rtsp, sh, ports))?;
        let sh = shared.clone();
        std::thread::Builder::new().name("gs-video-ping".into()).spawn(move || ping_loop(video, sh, true))?;
        let sh = shared.clone();
        std::thread::Builder::new().name("gs-audio-ping".into()).spawn(move || ping_loop(audio, sh, false))?;
        let sh = shared.clone();
        let host_ptr = host as usize;
        std::thread::Builder::new().name("gs-control".into()).spawn(move || control_loop(host_ptr as *mut enet::ENetHost, sh))?;
        Ok((s, rx))
    }

    pub fn params(&self) -> Option<StreamParams> {
        self.shared.params.lock().unwrap().clone()
    }

    /// The client is receiving video (its pings arrived).
    pub fn client_ready(&self) -> bool {
        self.shared.video_peer.lock().unwrap().is_some()
    }

    /// Send one encoded frame (Annex-B H.264). Returns false while no client receives video.
    pub fn send_frame(&self, annexb: &[u8], idr: bool, captured: Instant) -> bool {
        let Some(peer) = *self.shared.video_peer.lock().unwrap() else { return false };
        let ts = (captured.saturating_duration_since(self.epoch).as_micros() * 9 / 100) as u32; // 90 kHz
        let latency = (captured.elapsed().as_micros().div_ceil(100)).min(u16::MAX as u128) as u16;
        let index = {
            let mut i = self.frame_index.lock().unwrap();
            *i = i.wrapping_add(1);
            *i
        };
        let packets = {
            let mut p = self.packetizer.lock().unwrap();
            if let Some(par) = self.params() {
                p.packet_size = par.packet_size.max(256);
                p.min_fec_packets = par.min_fec_packets;
            }
            p.fec_percentage = self.fec_percentage;
            p.packetize(annexb, index, idr, ts, latency)
        };
        // Sunshine paces within a frame at ~80% of 1 Gbit/s; a few packets in a burst at a time
        for (i, pk) in packets.iter().enumerate() {
            let _ = self.video.send_to(pk, peer);
            if i % 64 == 63 {
                std::thread::sleep(Duration::from_micros(500));
            }
        }
        true
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- RTSP (rtsp.cpp) ---------------------------------------------------------------------------

struct Request {
    method: String,
    target: String,
    seq: String,
    body: String,
}

fn read_request(s: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
        if buf.len() > 1 << 20 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[head_end..(head_end + len).min(buf.len())]).to_string();
    Some(Request { method, target, seq: headers.get("cseq").cloned().unwrap_or_else(|| "0".into()), body })
}

fn respond(s: &mut TcpStream, code: u32, status: &str, options: &[(&str, String)], payload: &str) {
    let mut out = format!("RTSP/1.0 {code} {status}\r\n");
    for (k, v) in options {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(payload);
    let _ = s.write_all(out.as_bytes());
    let _ = s.flush();
}

/// The ANNOUNCE's `a=name:value` lines.
fn sdp_attributes(body: &str) -> HashMap<String, String> {
    body.lines()
        .filter_map(|l| l.trim_end().strip_prefix("a="))
        .filter_map(|a| a.split_once(':'))
        .map(|(k, v)| (k.to_string(), v.trim_end().to_string()))
        .collect()
}

fn parse_announce(body: &str) -> StreamParams {
    let a = sdp_attributes(body);
    let n = |k: &str, d: u64| a.get(k).and_then(|v| v.trim().parse::<i64>().ok()).map_or(d, |v| v.max(0) as u64);
    let configured = n("x-ml-video.configuredBitrateKbps", 0);
    StreamParams {
        width: n("x-nv-video[0].clientViewportWd", 1280) as u32,
        height: n("x-nv-video[0].clientViewportHt", 720) as u32,
        fps: n("x-nv-video[0].maxFPS", 60) as u32,
        bitrate_kbps: if configured > 0 { configured as u32 } else { n("x-nv-vqos[0].bw.maximumBitrateKbps", 20000) as u32 },
        packet_size: n("x-nv-video[0].packetSize", 1024) as usize,
        min_fec_packets: n("x-nv-vqos[0].fec.minRequiredFecPackets", 0) as usize,
        video_format: n("x-nv-vqos[0].bitStreamFormat", 0) as u32,
        encryption_enabled: n("x-ss-general.encryptionEnabled", 0) as u32,
        control_protocol: n("x-nv-general.useReliableUdp", 1) as u32,
    }
}

fn rtsp_loop(l: TcpListener, sh: Arc<Shared>, (video_port, audio_port, control_port): (u16, u16, u16)) {
    let _ = l.set_nonblocking(true);
    while !sh.stop.load(Ordering::SeqCst) {
        let Ok((mut s, _)) = l.accept() else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = s.set_nodelay(true);
        let Some(req) = read_request(&mut s) else { continue };
        let cseq = ("CSeq", req.seq.clone());
        match req.method.as_str() {
            "OPTIONS" | "PLAY" => respond(&mut s, 200, "OK", &[cseq], ""),
            "DESCRIBE" => {
                // what this host supports (cmd_describe): H.264 only, encrypted control stream,
                // no reference frame invalidation (the client asks for an IDR on loss)
                let payload = format!(
                    "a=x-ss-general.featureFlags:0\r\na=x-ss-general.encryptionSupported:{e}\r\na=x-ss-general.encryptionRequested:{e}\r\na=fmtp:97 surround-params=21101\r\n",
                    e = SS_ENC_CONTROL_V2
                );
                respond(&mut s, 200, "OK", &[cseq], &payload);
            }
            "SETUP" => {
                let kind = req.target.split('=').nth(1).and_then(|t| t.split('/').next()).unwrap_or("");
                let port = match kind {
                    "audio" => audio_port,
                    "video" => video_port,
                    "control" => control_port,
                    _ => {
                        respond(&mut s, 404, "NOT FOUND", &[cseq], "");
                        continue;
                    }
                };
                let extra = if kind == "control" { ("X-SS-Connect-Data", sh.connect_data.to_string()) } else { ("X-SS-Ping-Payload", sh.ping_payload.clone()) };
                respond(&mut s, 200, "OK", &[cseq, ("Session", "DEADBEEFCAFE;timeout = 90".into()), ("Transport", format!("server_port={port}")), extra], "");
            }
            "ANNOUNCE" => {
                let p = parse_announce(&req.body);
                if p.video_format != 0 {
                    respond(&mut s, 400, "BAD REQUEST", &[cseq], "");
                    continue;
                }
                let started = Event::Started { width: p.width, height: p.height, fps: p.fps, bitrate_kbps: p.bitrate_kbps };
                *sh.params.lock().unwrap() = Some(p);
                respond(&mut s, 200, "OK", &[cseq], "");
                sh.emit(started);
            }
            _ => respond(&mut s, 404, "NOT FOUND", &[cseq], ""),
        }
    }
}

// ---- video / audio pings (recvThread) ------------------------------------------------------------

fn ping_loop(sock: UdpSocket, sh: Arc<Shared>, video: bool) {
    let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = [0u8; 2048];
    while !sh.stop.load(Ordering::SeqCst) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        // SS_PING: 16-byte payload from SETUP + sequence number; legacy clients send "PING"
        let ok = (n >= 16 && &buf[..16] == sh.ping_payload.as_bytes()) || &buf[..n.min(4)] == b"PING";
        if ok && video {
            let mut p = sh.video_peer.lock().unwrap();
            if *p != Some(from) {
                *p = Some(from);
                drop(p);
                sh.emit(Event::RequestIdr); // a new receiver starts from a keyframe
            }
        }
    }
}

// ---- control stream (controlBroadcastThread) ----------------------------------------------------

fn send_control(peer: *mut enet::ENetPeer, sh: &Shared, seq: &mut u32, ty: u16, payload: &[u8]) {
    let mut plain = ty.to_le_bytes().to_vec();
    plain.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    plain.extend_from_slice(payload);
    let (cipher, tag) = crypto::seal(&sh.key, &crypto::iv(*seq, b'H', b'C'), &plain);
    let mut pkt = T_ENCRYPTED.to_le_bytes().to_vec();
    pkt.extend_from_slice(&((cipher.len() + 16 + 4) as u16).to_le_bytes());
    pkt.extend_from_slice(&seq.to_le_bytes());
    pkt.extend_from_slice(&tag);
    pkt.extend_from_slice(&cipher);
    *seq = seq.wrapping_add(1);
    unsafe {
        enet::rm_enet_send(peer, 0, pkt.as_ptr() as *const _, pkt.len(), 1);
    }
}

fn handle_control(sh: &Shared, ty: u16, payload: &[u8]) {
    match ty {
        T_REQUEST_IDR | T_INVALIDATE_REF_FRAMES => sh.emit(Event::RequestIdr),
        T_INPUT_DATA => {
            if let Some(i) = input::parse(payload) {
                sh.emit(Event::Input(i));
            }
        }
        T_TERMINATION => sh.emit(Event::Ended),
        T_START_A | T_START_B | T_PERIODIC_PING | T_LOSS_STATS => {}
        _ => {}
    }
}

fn control_loop(host: *mut enet::ENetHost, sh: Arc<Shared>) {
    let mut peer: *mut enet::ENetPeer = std::ptr::null_mut();
    let mut seq_out = 0u32;
    let mut last_seen = Instant::now();
    while !sh.stop.load(Ordering::SeqCst) {
        let (mut p, mut data, mut pkt) = (std::ptr::null_mut(), 0u32, std::ptr::null_mut());
        let r = unsafe { enet::rm_enet_service(host, 50, &mut p, &mut data, &mut pkt) };
        match r {
            1 => {
                peer = p;
                last_seen = Instant::now();
            }
            2 => {
                if p == peer {
                    peer = std::ptr::null_mut();
                    sh.emit(Event::Ended);
                }
            }
            3 => {
                last_seen = Instant::now();
                let mut len = 0usize;
                let bytes = unsafe { std::slice::from_raw_parts(enet::rm_enet_packet_data(pkt, &mut len), len) }.to_vec();
                unsafe { enet::rm_enet_packet_free(pkt) };
                if bytes.len() < 2 {
                    continue;
                }
                let ty = u16::from_le_bytes([bytes[0], bytes[1]]);
                if ty == T_ENCRYPTED && bytes.len() >= 8 {
                    // header: type u16, length u16 (seq + tag + cipher), seq u32; then tag, cipher
                    let length = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
                    let seq = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
                    if length < 4 + 16 + 4 || bytes.len() < 4 + length {
                        continue;
                    }
                    let tag = &bytes[8..24];
                    let cipher = &bytes[24..4 + length];
                    let Some(plain) = crypto::open(&sh.key, &crypto::iv(seq, b'C', b'C'), tag, cipher) else { continue };
                    if plain.len() < 4 {
                        continue;
                    }
                    let inner = u16::from_le_bytes([plain[0], plain[1]]);
                    handle_control(&sh, inner, &plain[4..]);
                } else {
                    // unencrypted messages are only valid on the legacy protocol
                    let proto = sh.params.lock().unwrap().as_ref().map_or(0, |p| p.control_protocol);
                    if proto != 13 {
                        handle_control(&sh, ty, &bytes[2..]);
                    }
                }
            }
            _ => {}
        }
        // Sunshine's ping timeout: no word from the client for 10 s ends the session
        if !peer.is_null() && last_seen.elapsed() > Duration::from_secs(10) {
            sh.emit(Event::Ended);
            last_seen = Instant::now();
        }
    }
    if !peer.is_null() {
        // graceful termination (0x80030023), as Sunshine sends when it shuts down
        send_control(peer, &sh, &mut seq_out, T_TERMINATION, &0x8003_0023u32.to_be_bytes());
        unsafe {
            enet::rm_enet_flush(host);
            enet::rm_enet_disconnect_now(peer);
        }
    }
    unsafe { enet::rm_enet_destroy(host) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_parameters() {
        let p = parse_announce("v=0\r\na=x-nv-video[0].clientViewportWd:2560 \r\na=x-nv-video[0].clientViewportHt:1600 \r\na=x-nv-video[0].maxFPS:60 \r\na=x-nv-video[0].packetSize:1392 \r\na=x-ml-video.configuredBitrateKbps:30000 \r\na=x-nv-general.useReliableUdp:13 \r\n");
        assert_eq!((p.width, p.height, p.fps, p.packet_size, p.bitrate_kbps, p.control_protocol), (2560, 1600, 60, 1392, 30000, 13));
    }
}
