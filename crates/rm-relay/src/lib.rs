//! Session rendezvous relay. It pairs one `agent` and one `client` per
//! session id and then forwards opaque bytes. It never parses application
//! data, so an end-to-end encrypted layer can sit on top without changes.
//!
//! SECURITY STATUS: this transport is plaintext TCP. It is a development
//! transport only. Before any real use, wrap the relay leg in TLS and put an
//! end-to-end Noise/QUIC session between client and agent (docs/SPEC.md §7).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
}

/// Admission key for joins, from the environment (`RM_RELAY_KEY`), if set.
pub fn env_key() -> Option<String> {
    std::env::var("RM_RELAY_KEY").ok().filter(|k| !k.is_empty())
}

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
}

impl Default for Config {
    fn default() -> Self {
        Self { pair_timeout: Duration::from_secs(300), hello_timeout: Duration::from_secs(10), key: None }
    }
}

type Table = Arc<Mutex<HashMap<String, Pending>>>;

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
    for conn in listener.incoming().flatten() {
        let (table, cfg) = (table.clone(), cfg.clone());
        thread::spawn(move || {
            let _ = handle(conn, table, cfg);
        });
    }
}

fn reject(mut s: TcpStream, why: &str) -> std::io::Result<()> {
    s.write_all(format!("ERR {why}\n").as_bytes())
}

fn handle(conn: TcpStream, table: Table, cfg: Config) -> std::io::Result<()> {
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

    let peer = {
        let mut t = table.lock().unwrap();
        match t.remove(&join.session_id) {
            Some(p) => {
                if !constant_time_eq(&p.token, &join.token) || p.role == join.role {
                    // Put the legitimate waiter back; refuse the intruder.
                    t.insert(join.session_id.clone(), p);
                    drop(t);
                    return reject(conn, "session mismatch");
                }
                Some(p)
            }
            None => {
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
            pipe(a, b);
            Ok(())
        }
    }
}

fn pipe(a: TcpStream, b: TcpStream) {
    let (mut a_r, mut b_w) = (a.try_clone().unwrap(), b.try_clone().unwrap());
    let (mut b_r, mut a_w) = (b, a);
    let t1 = thread::spawn(move || {
        let _ = std::io::copy(&mut a_r, &mut b_w);
        let _ = b_w.shutdown(std::net::Shutdown::Both);
    });
    let t2 = thread::spawn(move || {
        let _ = std::io::copy(&mut b_r, &mut a_w);
        let _ = a_w.shutdown(std::net::Shutdown::Both);
    });
    let _ = t1.join();
    let _ = t2.join();
}

/// Client/agent helper: connect, send join line, wait for READY.
pub fn join(addr: &str, session_id: &str, role: Role, token: &str) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect(addr)?;
    let j = serde_json::to_string(&Join { session_id: session_id.into(), role, token: token.into(), key: env_key() }).unwrap();
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
        let addr = start(Config { pair_timeout: Duration::from_millis(200), hello_timeout: Duration::from_secs(2), key: None });
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
            let j = serde_json::to_string(&Join { session_id: "k1".into(), role: Role::Agent, token: TOK.into(), key: key.map(Into::into) }).unwrap();
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
}
