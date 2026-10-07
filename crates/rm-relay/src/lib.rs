//! Session rendezvous relay. It pairs one `agent` and one `client` per
//! session id and then forwards opaque bytes. It never parses application
//! data, so an end-to-end encrypted layer can sit on top without changes.
//!
//! SECURITY STATUS: this transport is plaintext TCP. It is a development
//! transport only. Before any real use, wrap the relay leg in TLS and put an
//! end-to-end Noise/QUIC session between client and agent (docs/SPEC.md §7).

pub mod lan;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_HELLO_LINE: u64 = 512;
const MAX_PENDING: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    Client,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Join {
    pub session_id: String,
    pub role: Role,
    pub token: String,
    /// Admission key of a relay that is reachable from the internet (`RM_RELAY_KEY`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// False: do not wait for the peer; if nobody is waiting under this session the relay answers
    /// `ERR no such session` at once (a viewer asking for a Mac that is not online).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub wait: bool,
}

fn yes() -> bool {
    true
}
fn is_true(b: &bool) -> bool {
    *b
}

/// Admission key for joins: the environment (`RM_RELAY_KEY`), else the key built into this
/// binary (release builds set `RM_RELAY_KEY` at compile time, so users need not configure it).
pub fn env_key() -> Option<String> {
    std::env::var("RM_RELAY_KEY").ok().filter(|k| !k.is_empty()).or_else(|| option_env!("RM_RELAY_KEY").filter(|k| !k.is_empty()).map(String::from))
}

/// Wrong tokens allowed per session before it is locked (stops password guessing).
pub const MAX_FAILURES: u32 = 5;
pub const LOCKOUT: Duration = Duration::from_secs(60);

struct Pending {
    token: String,
    role: Role,
    stream: TcpStream,
}

#[derive(Clone)]
pub struct Config {
    pub pair_timeout: Duration,
    pub hello_timeout: Duration,
    /// When set, only joins presenting this key are paired (anyone else is refused before
    /// taking a slot): a relay on a public address is not an open pipe.
    pub key: Option<String>,
    /// Test aid: limit each direction to this many kilobits per second (a slow, far link).
    pub throttle_kbps: Option<u32>,
    /// Test aid: drop this share (0..1) of forwarded UDP datagrams (a lossy link).
    pub udp_loss: Option<f64>,
}

impl Default for Config {
    fn default() -> Self {
        Self { pair_timeout: Duration::from_secs(300), hello_timeout: Duration::from_secs(10), key: None, throttle_kbps: None, udp_loss: None }
    }
}

// ---- UDP: the same port also forwards datagrams (video) between the two peers of a session
// that is paired over TCP. Datagram: "RM" | type | ...; types below 16 are the relay's own.

pub const UDP_REGISTER: u8 = 1;
pub const UDP_STATUS: u8 = 2;
pub const UDP_KEEPALIVE: u8 = 3;
/// UDP_STATUS values
pub const UDP_WAITING: u8 = 0;
pub const UDP_PEER_READY: u8 = 1;
pub const UDP_REFUSED: u8 = 0xFF;

/// Big socket buffers for UDP video: a keyframe is a burst of hundreds of datagrams, and the
/// defaults (macOS: ~42 KB) drop part of it even on loopback.
pub fn big_udp_buffers(sock: &UdpSocket) {
    let s = socket2::SockRef::from(sock);
    let _ = s.set_recv_buffer_size(4 << 20);
    let _ = s.set_send_buffer_size(4 << 20);
}

/// "I am `role` of `session` (token, admission key)": sent until the relay answers UDP_STATUS,
/// then now and then to keep NAT bindings open.
pub fn udp_register(session_id: &str, role: Role, token: &str, key: Option<&str>) -> Vec<u8> {
    let mut d = vec![b'R', b'M', UDP_REGISTER, matches!(role, Role::Client) as u8];
    for f in [session_id, token, key.unwrap_or("")] {
        d.push(f.len().min(255) as u8);
        d.extend_from_slice(&f.as_bytes()[..f.len().min(255)]);
    }
    d
}

fn parse_register(p: &[u8]) -> Option<(Role, String, String, String)> {
    if p.len() < 5 || &p[..3] != b"RM\x01" {
        return None;
    }
    let role = if p[3] == 1 { Role::Client } else { Role::Agent };
    let mut fields = vec![];
    let mut i = 4;
    for _ in 0..3 {
        let n = *p.get(i)? as usize;
        fields.push(String::from_utf8(p.get(i + 1..i + 1 + n)?.to_vec()).ok()?);
        i += 1 + n;
    }
    let key = fields.pop()?;
    let token = fields.pop()?;
    Some((role, fields.pop()?, token, key))
}

struct UdpPair {
    token: String,
    agent: Option<SocketAddr>,
    client: Option<SocketAddr>,
}

#[derive(Default)]
struct UdpState {
    pairs: HashMap<String, UdpPair>,
    by_addr: HashMap<SocketAddr, (String, Role)>,
}

type Udp = Arc<Mutex<UdpState>>;

impl UdpState {
    fn forget(&mut self, session: &str) {
        if let Some(p) = self.pairs.remove(session) {
            for a in [p.agent, p.client].into_iter().flatten() {
                self.by_addr.remove(&a);
            }
        }
    }
}

fn udp_loop(sock: UdpSocket, udp: Udp, cfg: Config) {
    let mut buf = vec![0u8; 2048];
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15 ^ std::process::id() as u64;
    // token bucket per destination when throttled (drops what exceeds ~100 ms of burst)
    let mut buckets: HashMap<SocketAddr, (f64, Instant)> = HashMap::new();
    let rate = cfg.throttle_kbps.map(|k| k as f64 * 1000.0 / 8.0);
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let p = &buf[..n];
        if n < 3 || &p[..2] != b"RM" {
            continue;
        }
        match p[2] {
            UDP_REGISTER => {
                let Some((role, session, token, key)) = parse_register(p) else { continue };
                let admitted = cfg.key.as_ref().is_none_or(|k| constant_time_eq(k, &key));
                let mut st = udp.lock().unwrap();
                let status = match st.pairs.get_mut(&session) {
                    Some(pair) if admitted && constant_time_eq(&pair.token, &token) => {
                        let slot = if role == Role::Agent { &mut pair.agent } else { &mut pair.client };
                        let old = slot.replace(from);
                        let ready = pair.agent.is_some() && pair.client.is_some();
                        if let Some(o) = old.filter(|o| *o != from) {
                            st.by_addr.remove(&o);
                        }
                        st.by_addr.insert(from, (session.clone(), role));
                        if ready { UDP_PEER_READY } else { UDP_WAITING }
                    }
                    _ => UDP_REFUSED,
                };
                drop(st);
                let _ = sock.send_to(&[b'R', b'M', UDP_STATUS, status], from);
            }
            UDP_KEEPALIVE | UDP_STATUS => {}
            _ => {
                let to = {
                    let st = udp.lock().unwrap();
                    st.by_addr.get(&from).and_then(|(s, role)| st.pairs.get(s).and_then(|p| if *role == Role::Agent { p.client } else { p.agent }))
                };
                let Some(to) = to else { continue };
                if let Some(loss) = cfg.udp_loss {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    if (rng % 10_000) as f64 / 10_000.0 < loss {
                        continue;
                    }
                }
                if let Some(rate) = rate {
                    let now = Instant::now();
                    let b = buckets.entry(to).or_insert((rate * 0.1, now));
                    b.0 = (b.0 + now.duration_since(b.1).as_secs_f64() * rate).min(rate * 0.1);
                    b.1 = now;
                    if b.0 < n as f64 {
                        continue;
                    }
                    b.0 -= n as f64;
                }
                let _ = sock.send_to(p, to);
            }
        }
    }
}

type Table = Arc<Mutex<HashMap<String, Pending>>>;
/// session -> (wrong tokens, since)
type Failures = Arc<Mutex<HashMap<String, (u32, Instant)>>>;

pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

fn valid_session_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn serve(listener: TcpListener, cfg: Config) {
    let table: Table = Arc::new(Mutex::new(HashMap::new()));
    let failures: Failures = Arc::new(Mutex::new(HashMap::new()));
    let udp: Udp = Arc::new(Mutex::new(UdpState::default()));
    // UDP on the same address and port number as the TCP listener
    match listener.local_addr().and_then(UdpSocket::bind) {
        Ok(sock) => {
            big_udp_buffers(&sock);
            let (u, c) = (udp.clone(), cfg.clone());
            thread::spawn(move || udp_loop(sock, u, c));
        }
        Err(e) => eprintln!("rm-relay: no UDP forwarding ({e}); video falls back to TCP"),
    }
    for conn in listener.incoming().flatten() {
        let (table, failures, udp, cfg) = (table.clone(), failures.clone(), udp.clone(), cfg.clone());
        thread::spawn(move || {
            let _ = handle(conn, table, failures, udp, cfg);
        });
    }
}

fn reject(mut s: TcpStream, why: &str) -> std::io::Result<()> {
    s.write_all(format!("ERR {why}\n").as_bytes())
}

fn handle(conn: TcpStream, table: Table, failures: Failures, udp: Udp, cfg: Config) -> std::io::Result<()> {
    // small control/input messages must not wait for Nagle
    let _ = conn.set_nodelay(true);
    conn.set_read_timeout(Some(cfg.hello_timeout))?;
    // Read the join line byte-by-byte-ish via a limited BufReader, taking care
    // not to swallow payload bytes that follow it.
    let mut line = String::new();
    {
        let mut r = BufReader::with_capacity(1, (&conn).take(MAX_HELLO_LINE));
        if r.read_line(&mut line).is_err() || !line.ends_with('\n') {
            return reject(conn, "bad hello");
        }
    }
    conn.set_read_timeout(None)?;
    let join: Join = match serde_json::from_str(line.trim()) {
        Ok(j) => j,
        Err(_) => return reject(conn, "bad hello"),
    };
    if let Some(k) = &cfg.key {
        if !constant_time_eq(k, join.key.as_deref().unwrap_or("")) {
            return reject(conn, "not admitted");
        }
    }
    if !valid_session_id(&join.session_id) || join.token.len() < 16 || join.token.len() > 128 {
        return reject(conn, "bad session or token");
    }

    {
        let mut f = failures.lock().unwrap();
        f.retain(|_, (_, since)| since.elapsed() < LOCKOUT);
        if f.get(&join.session_id).is_some_and(|(n, _)| *n >= MAX_FAILURES) {
            drop(f);
            return reject(conn, "locked");
        }
    }
    let peer = {
        let mut t = table.lock().unwrap();
        match t.remove(&join.session_id) {
            Some(p) => {
                if !constant_time_eq(&p.token, &join.token) || p.role == join.role {
                    // Put the legitimate waiter back; refuse the intruder, and count the attempt.
                    t.insert(join.session_id.clone(), p);
                    drop(t);
                    let mut f = failures.lock().unwrap();
                    let e = f.entry(join.session_id.clone()).or_insert((0, Instant::now()));
                    e.0 += 1;
                    return reject(conn, "session mismatch");
                }
                failures.lock().unwrap().remove(&join.session_id);
                Some(p)
            }
            None => {
                if !join.wait {
                    drop(t);
                    return reject(conn, "no such session");
                }
                if t.len() >= MAX_PENDING {
                    drop(t);
                    return reject(conn, "relay busy");
                }
                let stream = conn.try_clone()?;
                t.insert(join.session_id.clone(), Pending { token: join.token.clone(), role: join.role, stream });
                None
            }
        }
    };

    match peer {
        None => {
            // We are the waiter. The pairing thread of the second peer drives
            // the pipe; we only enforce the timeout by evicting stale entries.
            thread::sleep(cfg.pair_timeout);
            let mut t = table.lock().unwrap();
            if let Some(p) = t.get(&join.session_id) {
                if constant_time_eq(&p.token, &join.token) && p.role == join.role {
                    let p = t.remove(&join.session_id).unwrap();
                    drop(t);
                    let _ = reject(p.stream, "pair timeout");
                }
            }
            Ok(())
        }
        Some(waiter) => {
            let mut a = waiter.stream;
            let mut b = conn;
            a.write_all(b"READY\n")?;
            b.write_all(b"READY\n")?;
            // while the pair lives, its two peers may also exchange UDP datagrams
            udp.lock().unwrap().forget(&join.session_id);
            udp.lock().unwrap().pairs.insert(join.session_id.clone(), UdpPair { token: join.token.clone(), agent: None, client: None });
            pipe(a, b, cfg.throttle_kbps);
            udp.lock().unwrap().forget(&join.session_id);
            Ok(())
        }
    }
}

/// Copy bytes, at most `kbps` kilobits per second when set (token bucket, 20 ms quanta).
fn copy_limited(r: &mut TcpStream, w: &mut TcpStream, kbps: Option<u32>) {
    let Some(kbps) = kbps else {
        let _ = std::io::copy(r, w);
        return;
    };
    let rate = kbps as f64 * 1000.0 / 8.0; // bytes per second
    let mut buf = vec![0u8; 16 * 1024];
    let start = Instant::now();
    let mut sent = 0f64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        sent += n as f64;
        let due = Duration::from_secs_f64(sent / rate);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            thread::sleep(wait);
        }
        if w.write_all(&buf[..n]).is_err() {
            return;
        }
    }
}

fn pipe(a: TcpStream, b: TcpStream, kbps: Option<u32>) {
    let (mut a_r, mut b_w) = (a.try_clone().unwrap(), b.try_clone().unwrap());
    let (mut b_r, mut a_w) = (b, a);
    let t1 = thread::spawn(move || {
        copy_limited(&mut a_r, &mut b_w, kbps);
        let _ = b_w.shutdown(std::net::Shutdown::Both);
    });
    let t2 = thread::spawn(move || {
        copy_limited(&mut b_r, &mut a_w, kbps);
        let _ = a_w.shutdown(std::net::Shutdown::Both);
    });
    let _ = t1.join();
    let _ = t2.join();
}

/// Client/agent helper: connect, send join line, wait for READY.
pub fn join(addr: &str, session_id: &str, role: Role, token: &str) -> std::io::Result<TcpStream> {
    join_with(addr, session_id, role, token, true)
}

/// [`join`], optionally failing at once (`ERR no such session`) when the peer is not waiting.
pub fn join_with(addr: &str, session_id: &str, role: Role, token: &str, wait: bool) -> std::io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "relay address not found"))?;
    let mut s = TcpStream::connect_timeout(&target, Duration::from_secs(10))?;
    let _ = s.set_nodelay(true);
    let j = serde_json::to_string(&Join { session_id: session_id.into(), role, token: token.into(), key: env_key(), wait }).unwrap();
    s.write_all(j.as_bytes())?;
    s.write_all(b"\n")?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.last() != Some(&b'\n') {
        if line.len() > 64 || s.read(&mut byte)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "relay closed"));
        }
        line.push(byte[0]);
    }
    let reply = String::from_utf8_lossy(&line).trim().to_string();
    if reply == "READY" {
        Ok(s)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(cfg: Config) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        thread::spawn(move || serve(l, cfg));
        addr
    }
    const TOK: &str = "0123456789abcdef-token";

    #[test]
    fn pairs_and_forwards_both_ways() {
        let addr = start(Config::default());
        let a2 = addr.clone();
        let agent = thread::spawn(move || {
            let mut s = join(&a2, "sess-1", Role::Agent, TOK).unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"hello");
            s.write_all(b"world").unwrap();
        });
        thread::sleep(Duration::from_millis(100));
        let mut c = join(&addr, "sess-1", Role::Client, TOK).unwrap();
        c.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
        agent.join().unwrap();
    }

    #[test]
    fn wrong_token_rejected_and_waiter_survives() {
        let addr = start(Config::default());
        let a2 = addr.clone();
        let agent = thread::spawn(move || join(&a2, "sess-2", Role::Agent, TOK).map(|_| ()));
        thread::sleep(Duration::from_millis(100));
        let bad = join(&addr, "sess-2", Role::Client, "ffffffffffffffff-wrong");
        assert_eq!(bad.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
        // legitimate client still pairs
        assert!(join(&addr, "sess-2", Role::Client, TOK).is_ok());
        assert!(agent.join().unwrap().is_ok());
    }

    #[test]
    fn same_role_twice_rejected() {
        let addr = start(Config::default());
        let a2 = addr.clone();
        thread::spawn(move || {
            let _ = join(&a2, "sess-3", Role::Agent, TOK);
        });
        thread::sleep(Duration::from_millis(100));
        assert!(join(&addr, "sess-3", Role::Agent, TOK).is_err());
    }

    #[test]
    fn rejects_garbage_hello_and_bad_ids() {
        let addr = start(Config::default());
        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.starts_with("ERR"), "{out}");
        assert!(join(&addr, "../../etc", Role::Agent, TOK).is_err());
        assert!(join(&addr, "ok-id", Role::Agent, "short").is_err());
    }

    #[test]
    fn pair_timeout_evicts() {
        let addr = start(Config { pair_timeout: Duration::from_millis(200), hello_timeout: Duration::from_secs(2), ..Default::default() });
        let r = join(&addr, "sess-4", Role::Agent, TOK);
        // the waiter is told READY never comes; it receives ERR pair timeout
        assert!(r.is_err());
    }

    #[test]
    fn ct_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }

    #[test]
    fn admission_key_keeps_strangers_out() {
        let addr = start(Config { key: Some("relay-admission-key".into()), ..Default::default() });
        let line = |key: Option<&str>| {
            let mut s = TcpStream::connect(&addr).unwrap();
            let j = serde_json::to_string(&Join { session_id: "k1".into(), role: Role::Agent, token: TOK.into(), key: key.map(Into::into), wait: true }).unwrap();
            s.write_all(format!("{j}\n").as_bytes()).unwrap();
            s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).unwrap_or(0);
            (s, String::from_utf8_lossy(&buf[..n]).to_string())
        };
        assert_eq!(line(None).1, "ERR not admitted\n");
        assert_eq!(line(Some("wrong")).1, "ERR not admitted\n");
        let (_waiter, reply) = line(Some("relay-admission-key"));
        assert_eq!(reply, "", "an admitted agent waits for its client");
    }

    #[test]
    fn password_guessing_locks_the_session() {
        let addr = start(Config::default());
        let _agent = std::thread::spawn({
            let a = addr.clone();
            move || join(&a, "lock-1", Role::Agent, TOK)
        });
        thread::sleep(Duration::from_millis(100));
        let attempt = |tok: &str| join(&addr, "lock-1", Role::Client, tok).err().map(|e| e.to_string());
        for _ in 0..MAX_FAILURES {
            assert_eq!(attempt("wrong-token-0123456789").as_deref(), Some("ERR session mismatch"));
        }
        // now even the right token is refused for a while
        assert_eq!(attempt(TOK).as_deref(), Some("ERR locked"));
    }

    #[test]
    fn client_need_not_wait_for_an_offline_mac() {
        let addr = start(Config::default());
        let t = Instant::now();
        let e = join_with(&addr, "offline-1", Role::Client, TOK, false).unwrap_err();
        assert_eq!(e.to_string(), "ERR no such session");
        assert!(t.elapsed() < Duration::from_secs(2));
        // with the Mac waiting, the same join pairs
        let a = addr.clone();
        let agent = thread::spawn(move || join(&a, "offline-1", Role::Agent, TOK).map(|_| ()));
        thread::sleep(Duration::from_millis(100));
        assert!(join_with(&addr, "offline-1", Role::Client, TOK, false).is_ok());
        assert!(agent.join().unwrap().is_ok());
    }

    #[test]
    fn udp_flows_between_the_paired_peers_only() {
        let addr = start(Config::default());
        let a = addr.clone();
        let agent = thread::spawn(move || join(&a, "udp-1", Role::Agent, TOK).unwrap());
        thread::sleep(Duration::from_millis(100));
        let _client_tcp = join(&addr, "udp-1", Role::Client, TOK).unwrap();
        let _agent_tcp = agent.join().unwrap();
        let sock = |s: &str| {
            let u = UdpSocket::bind("127.0.0.1:0").unwrap();
            u.connect(s).unwrap();
            u.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
            u
        };
        let (ua, uc, intruder) = (sock(&addr), sock(&addr), sock(&addr));
        let status = |u: &UdpSocket| {
            let mut b = [0u8; 16];
            let n = u.recv(&mut b).unwrap();
            b[..n].to_vec()
        };
        intruder.send(&udp_register("udp-1", Role::Client, "wrong-token-0123456789", None)).unwrap();
        assert_eq!(status(&intruder), [b'R', b'M', UDP_STATUS, UDP_REFUSED]);
        ua.send(&udp_register("udp-1", Role::Agent, TOK, None)).unwrap();
        assert_eq!(status(&ua), [b'R', b'M', UDP_STATUS, UDP_WAITING]);
        uc.send(&udp_register("udp-1", Role::Client, TOK, None)).unwrap();
        assert_eq!(status(&uc), [b'R', b'M', UDP_STATUS, UDP_PEER_READY]);
        ua.send(b"RM\x10video").unwrap();
        assert_eq!(status(&uc), b"RM\x10video");
        uc.send(b"RM\x11feedback").unwrap();
        assert_eq!(status(&ua), b"RM\x11feedback");
        // the refused socket cannot inject anything
        intruder.send(b"RM\x10evil").unwrap();
        let mut b = [0u8; 16];
        assert!(ua.recv(&mut b).is_err() && uc.recv(&mut b).is_err());
    }

    #[test]
    fn throttled_link_is_slow() {
        let addr = start(Config { throttle_kbps: Some(800), ..Default::default() }); // 100 KB/s
        let a = addr.clone();
        let agent = thread::spawn(move || {
            let mut s = join(&a, "slow-1", Role::Agent, TOK).unwrap();
            s.write_all(&vec![7u8; 50_000]).unwrap();
        });
        thread::sleep(Duration::from_millis(100));
        let mut c = join(&addr, "slow-1", Role::Client, TOK).unwrap();
        let t = Instant::now();
        let mut got = vec![0u8; 50_000];
        c.read_exact(&mut got).unwrap();
        assert!(t.elapsed() >= Duration::from_millis(400), "50 KB at 100 KB/s took {:?}", t.elapsed());
        agent.join().unwrap();
    }
}
