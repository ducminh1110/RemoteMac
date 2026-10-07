//! A host [`Session`] reached through RemoteMac's own connection instead of open ports: the
//! client's moonlight-common-c talks to local ports on the PC, the PC side
//! (rm-client `gstunnel`) carries RTSP over the authenticated TCP link and the UDP flows
//! (video, audio, ENet control) over the NAT-punched UDP path, and this side hands them to the
//! session's real sockets on the Mac's loopback.

use crate::{Config, Event, Session};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

/// UDP flows, in the order of [`Ports`]
pub const VIDEO: u8 = 0;
pub const AUDIO: u8 = 1;
pub const CONTROL: u8 = 2;

/// Something to send to the client.
pub enum Outbound<'a> {
    Udp { kind: u8, data: &'a [u8] },
    TcpData { id: u32, data: &'a [u8] },
    TcpClose { id: u32 },
}

pub type Out = Arc<dyn Fn(Outbound) + Send + Sync>;

pub struct HostTunnel {
    pub session: Arc<Session>,
    pub events: Mutex<Receiver<Event>>,
    udp: Vec<UdpSocket>,
    tcp: Mutex<HashMap<u32, TcpStream>>,
    out: Out,
}

impl HostTunnel {
    pub fn start(key: [u8; 16], fec_percentage: usize, out: Out) -> std::io::Result<Arc<Self>> {
        let (session, events) = Session::start(Config { key, bind: "127.0.0.1".parse().unwrap(), fec_percentage })?;
        let mut udp = vec![];
        for port in [session.video_port, session.audio_port, session.control_port] {
            let s = UdpSocket::bind("127.0.0.1:0")?;
            s.connect(SocketAddr::from(([127, 0, 0, 1], port)))?;
            udp.push(s);
        }
        let t = Arc::new(Self { session, events: Mutex::new(events), udp, tcp: Mutex::new(HashMap::new()), out });
        for kind in [VIDEO, AUDIO, CONTROL] {
            let (s, o, weak) = (t.udp[kind as usize].try_clone()?, t.out.clone(), Arc::downgrade(&t));
            std::thread::Builder::new().name(format!("gs-tunnel-{kind}")).spawn(move || {
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut buf = vec![0u8; 65536];
                while weak.strong_count() > 0 {
                    if let Ok(n) = s.recv(&mut buf) {
                        o(Outbound::Udp { kind, data: &buf[..n] });
                    }
                }
            })?;
        }
        Ok(t)
    }

    /// A datagram from the client's moonlight-common-c for flow `kind`.
    pub fn udp_in(&self, kind: u8, data: &[u8]) {
        if let Some(s) = self.udp.get(kind as usize) {
            let _ = s.send(data);
        }
    }

    /// The client opened RTSP connection `id`.
    pub fn tcp_open(&self, id: u32) {
        let Ok(s) = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.session.rtsp_port))) else {
            (self.out)(Outbound::TcpClose { id });
            return;
        };
        let _ = s.set_nodelay(true);
        if let Ok(mut r) = s.try_clone() {
            let o = self.out.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 16384];
                loop {
                    match r.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => o(Outbound::TcpData { id, data: &buf[..n] }),
                    }
                }
                o(Outbound::TcpClose { id });
            });
        }
        self.tcp.lock().unwrap().insert(id, s);
    }

    pub fn tcp_data(&self, id: u32, data: &[u8]) {
        if let Some(s) = self.tcp.lock().unwrap().get_mut(&id) {
            let _ = s.write_all(data);
        }
    }

    /// The client finished sending on `id` (its request is complete).
    pub fn tcp_close(&self, id: u32) {
        if let Some(s) = self.tcp.lock().unwrap().remove(&id) {
            let _ = s.shutdown(std::net::Shutdown::Write);
        }
    }
}

/// Windows virtual-key code -> RemoteMac's physical key name (as the viewer's keymap).
pub fn vk_name(vk: u16) -> Option<&'static str> {
    const LETTERS: [&str; 26] = ["KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ"];
    const DIGITS: [&str; 10] = ["Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9"];
    const FKEYS: [&str; 12] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];
    Some(match vk {
        0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x70..=0x7B => FKEYS[(vk - 0x70) as usize],
        0x0D => "Enter",
        0x09 => "Tab",
        0x20 => "Space",
        0x08 => "Backspace",
        0x1B => "Escape",
        0x2E => "Delete",
        0x25 => "ArrowLeft",
        0x26 => "ArrowUp",
        0x27 => "ArrowRight",
        0x28 => "ArrowDown",
        0x24 => "Home",
        0x23 => "End",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0xBA => "Semicolon",
        0xBB => "Equal",
        0xBC => "Comma",
        0xBD => "Minus",
        0xBE => "Period",
        0xBF => "Slash",
        0xC0 => "Backquote",
        0xDB => "BracketLeft",
        0xDC => "Backslash",
        0xDD => "BracketRight",
        0xDE => "Quote",
        _ => return None,
    })
}

/// Something to send to the host side.
pub enum ToHost<'a> {
    Udp { kind: u8, data: &'a [u8] },
    TcpOpen { id: u32 },
    TcpData { id: u32, data: &'a [u8] },
    TcpClose { id: u32 },
}

pub type ToHostFn = Arc<dyn Fn(ToHost) + Send + Sync>;

/// The PC end: local ports moonlight-common-c connects to (`rtsp://127.0.0.1:rtsp_port`).
/// SETUP answers name the host's own ports; they are rewritten to the local ones here.
pub struct ClientTunnel {
    pub rtsp_port: u16,
    udp: Vec<UdpSocket>,
    local_ports: [u16; 3],
    peers: Mutex<[Option<SocketAddr>; 3]>,
    /// RTSP connection id -> (local socket, the flow its SETUP names)
    conns: Mutex<HashMap<u32, (TcpStream, Option<u8>)>>,
    to_host: ToHostFn,
}

fn setup_kind(req: &[u8]) -> Option<u8> {
    let line = std::str::from_utf8(req.split(|&b| b == b'\n').next()?).ok()?;
    if !line.starts_with("SETUP") {
        return None;
    }
    Some(if line.contains("audio") { AUDIO } else if line.contains("video") { VIDEO } else if line.contains("control") { CONTROL } else { return None })
}

impl ClientTunnel {
    pub fn start(to_host: ToHostFn) -> std::io::Result<Arc<Self>> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let mut udp = vec![];
        let mut local_ports = [0u16; 3];
        for p in local_ports.iter_mut() {
            let s = UdpSocket::bind("127.0.0.1:0")?;
            *p = s.local_addr()?.port();
            udp.push(s);
        }
        let t = Arc::new(Self { rtsp_port: listener.local_addr()?.port(), udp, local_ports, peers: Mutex::new([None; 3]), conns: Mutex::new(HashMap::new()), to_host });
        // RTSP: one connection per request, as moonlight-common-c does
        let weak = Arc::downgrade(&t);
        std::thread::Builder::new().name("gs-tunnel-rtsp".into()).spawn(move || {
            let mut next = 1u32;
            for s in listener.incoming() {
                let Some(t) = weak.upgrade() else { return };
                let Ok(s) = s else { continue };
                let id = next;
                next += 1;
                let _ = s.set_nodelay(true);
                let Ok(mut r) = s.try_clone() else { continue };
                t.conns.lock().unwrap().insert(id, (s, None));
                (t.to_host)(ToHost::TcpOpen { id });
                let w = Arc::downgrade(&t);
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 16384];
                    while let Ok(n) = r.read(&mut buf) {
                        let Some(t) = w.upgrade() else { return };
                        if n == 0 {
                            (t.to_host)(ToHost::TcpClose { id });
                            return;
                        }
                        if let Some(k) = setup_kind(&buf[..n]) {
                            if let Some(c) = t.conns.lock().unwrap().get_mut(&id) {
                                c.1 = Some(k);
                            }
                        }
                        (t.to_host)(ToHost::TcpData { id, data: &buf[..n] });
                    }
                    if let Some(t) = w.upgrade() {
                        (t.to_host)(ToHost::TcpClose { id });
                    }
                });
            }
        })?;
        for kind in [VIDEO, AUDIO, CONTROL] {
            let (s, weak) = (t.udp[kind as usize].try_clone()?, Arc::downgrade(&t));
            std::thread::Builder::new().name(format!("gs-ctunnel-{kind}")).spawn(move || {
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                let mut buf = vec![0u8; 65536];
                loop {
                    let r = s.recv_from(&mut buf);
                    let Some(t) = weak.upgrade() else { return };
                    if let Ok((n, from)) = r {
                        t.peers.lock().unwrap()[kind as usize] = Some(from);
                        (t.to_host)(ToHost::Udp { kind, data: &buf[..n] });
                    }
                }
            })?;
        }
        Ok(t)
    }

    /// A datagram from the host for flow `kind`: to the local moonlight-common-c.
    pub fn udp_from_host(&self, kind: u8, data: &[u8]) {
        let peer = self.peers.lock().unwrap().get(kind as usize).copied().flatten();
        if let (Some(p), Some(s)) = (peer, self.udp.get(kind as usize)) {
            let _ = s.send_to(data, p);
        }
    }

    /// RTSP bytes from the host for connection `id` (SETUP answers get the local port).
    pub fn tcp_from_host(&self, id: u32, data: &[u8]) {
        let mut conns = self.conns.lock().unwrap();
        let Some((s, kind)) = conns.get_mut(&id) else { return };
        let mut out = data.to_vec();
        if let (Some(k), Ok(text)) = (kind, std::str::from_utf8(data)) {
            if let Some(i) = text.find("server_port=") {
                let start = i + "server_port=".len();
                let end = text[start..].find(|c: char| !c.is_ascii_digit()).map_or(text.len(), |e| start + e);
                out = format!("{}{}{}", &text[..start], self.local_ports[*k as usize], &text[end..]).into_bytes();
            }
        }
        let _ = s.write_all(&out);
    }

    pub fn tcp_close_from_host(&self, id: u32) {
        if let Some((s, _)) = self.conns.lock().unwrap().remove(&id) {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}
