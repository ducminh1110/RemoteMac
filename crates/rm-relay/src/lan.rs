//! Connecting on the same network without a relay. The Mac answers a broadcast for its ID and
//! takes the same join line a relay would, straight on its own TCP port:
//!
//!   viewer  -- UDP broadcast  "RMLAN?rm-123456789"             --> Mac (port 7471)
//!   viewer <-- UDP            "RMLAN!rm-123456789 <tcp port>"  --  Mac
//!   viewer  -- TCP join line (session, token), as to a relay   --> Mac
//!   viewer <-- "READY"                                          --  Mac (token checked there)
//!
//! After READY the stream is the same as a paired relay stream. The token is the hash of ID and
//! password (`rm_protocol::session::token`): only someone who knows the password gets in.
//!
//! A viewer that typed the Mac's address goes straight there, as Moonlight goes to Sunshine: no
//! ID, no discovery, no relay. It joins the session `direct` on the same TCP port, and its secret
//! is made from the password alone (`rm_protocol::session::direct_token`).

use crate::{Join, Role};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

/// The Mac's discovery port (UDP), and its default TCP port.
pub const PORT: u16 = 7471;

pub fn query(session: &str) -> Vec<u8> {
    format!("RMLAN?{session}").into_bytes()
}

/// The answer to a query for `session`: the TCP port to join on.
pub fn answer(session: &str, tcp_port: u16) -> Vec<u8> {
    format!("RMLAN!{session} {tcp_port}").into_bytes()
}

/// The session a query asks for.
pub fn parse_query(p: &[u8]) -> Option<&str> {
    std::str::from_utf8(p.strip_prefix(b"RMLAN?")?).ok().filter(|s| !s.is_empty() && s.len() <= 64)
}

fn parse_answer(p: &[u8], session: &str) -> Option<u16> {
    let s = std::str::from_utf8(p.strip_prefix(b"RMLAN!")?).ok()?;
    let (sess, port) = s.split_once(' ')?;
    (sess == session).then(|| port.trim().parse().ok()).flatten()
}

/// Where queries go: the broadcast address, each local network's /24 broadcast (some systems
/// send 255.255.255.255 out of one interface only), and this machine (a Mac on the same host).
fn targets(port: u16) -> Vec<SocketAddr> {
    let mut t = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port), SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)];
    if let Some(IpAddr::V4(ip)) = local_ipv4() {
        let o = ip.octets();
        t.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], 255)), port));
    }
    t
}

fn local_ipv4() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:53").ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified() && !ip.is_loopback())
}

/// Look for the Mac with this relay session ("rm-<ID>") on the local network for up to `wait`.
pub fn discover(session: &str, wait: Duration) -> Option<SocketAddr> {
    discover_on(session, wait, PORT)
}

pub fn discover_on(session: &str, wait: Duration, port: u16) -> Option<SocketAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
    let q = query(session);
    let start = Instant::now();
    let mut last_sent = None::<Instant>;
    let mut buf = [0u8; 256];
    while start.elapsed() < wait {
        if last_sent.is_none_or(|t| t.elapsed() >= Duration::from_millis(150)) {
            for t in targets(port) {
                let _ = sock.send_to(&q, t);
            }
            last_sent = Some(Instant::now());
        }
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if let Some(tcp) = parse_answer(&buf[..n], session) {
                return Some(SocketAddr::new(from.ip(), tcp));
            }
        }
    }
    None
}

/// Join the Mac at `addr` directly, with the same line a relay takes; the stream after READY is
/// the session.
pub fn join_direct(addr: SocketAddr, session: &str, token: &str) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    let _ = s.set_nodelay(true);
    let j = serde_json::to_string(&Join { session_id: session.into(), role: Role::Client, token: token.into(), key: None, wait: true, owner: None }).unwrap();
    s.write_all(j.as_bytes())?;
    s.write_all(b"\n")?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.last() != Some(&b'\n') {
        if line.len() > 64 || s.read(&mut byte)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the Mac closed the connection"));
        }
        line.push(byte[0]);
    }
    s.set_read_timeout(None)?;
    let reply = String::from_utf8_lossy(&line).trim().to_string();
    if reply == "READY" {
        Ok(s)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, reply))
    }
}

/// How a viewer reached the Mac.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Straight to the Mac on this network.
    Lan(SocketAddr),
    /// Straight to the address the user typed (an IP or a host name, any network).
    Direct(SocketAddr),
    /// Through this relay.
    Relay(String),
}

impl Route {
    /// Where the UDP path registers: the relay, or (straight to the Mac, where the direct path
    /// is set up from the Mac's offer) a port of the Mac that ignores it.
    pub fn udp_relay(&self) -> String {
        match self {
            Route::Lan(a) | Route::Direct(a) => SocketAddr::new(a.ip(), 9).to_string(),
            Route::Relay(r) => r.clone(),
        }
    }

    /// Straight to the Mac (no relay in between).
    pub fn is_direct(&self) -> bool {
        !matches!(self, Route::Relay(_))
    }
}

/// "host", "host:port", "1.2.3.4:7471", "[fe80::1]:7471" or a bare IPv6 address, checked:
/// (host, port), the port 7471 when none is given.
pub fn parse_address(s: &str) -> Result<(String, u16), String> {
    let s = s.trim();
    let bad = |why: &str| Err(format!("\"{s}\" is not a Mac address: {why}"));
    if s.is_empty() || s.len() > 260 {
        return bad("type an IP address or a host name, with :port if it is not 7471");
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control() || "/\\@?#".contains(c)) {
        return bad("no spaces, slashes or URLs");
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let Some((h, after)) = rest.split_once(']') else { return bad("a ] is missing") };
        match after {
            "" => (h.to_string(), None),
            p if p.starts_with(':') => (h.to_string(), Some(&p[1..])),
            _ => return bad("unexpected text after ]"),
        }
    } else if s.matches(':').count() > 1 {
        (s.to_string(), None) // a bare IPv6 address
    } else if let Some((h, p)) = s.rsplit_once(':') {
        (h.to_string(), Some(p))
    } else {
        (s.to_string(), None)
    };
    let port = match port {
        None => PORT,
        Some(p) => match p.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return bad("the port must be a number from 1 to 65535"),
        },
    };
    if host.parse::<IpAddr>().is_err() {
        let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if host.is_empty() || host.len() > 253 || !host.trim_end_matches('.').split('.').all(label_ok) {
            return bad("not an IP address or host name");
        }
    }
    Ok((host, port))
}

/// Connect straight to the Mac at `address` (typed by the user: an IP address or a host name,
/// IPv4 or IPv6, with :port when it is not 7471), any network. The Mac takes the same join line
/// as on the local network, and the end-to-end handshake follows as always: there is no
/// unencrypted way in. Every address the name resolves to is tried in turn.
pub fn connect_direct(address: &str, session: &str, token: &str) -> Result<(TcpStream, Route), String> {
    use std::net::ToSocketAddrs;
    let (host, port) = parse_address(address)?;
    let addrs: Vec<SocketAddr> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("{host}: the name could not be resolved ({e})"))?.collect();
    if addrs.is_empty() {
        return Err(format!("{host}: no address found"));
    }
    let mut errors = vec![];
    for a in addrs {
        match join_direct(a, session, token) {
            Ok(s) => return Ok((s, Route::Direct(a))),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(format!("the Mac refused: {e}")),
            Err(e) => errors.push(format!("{a}: {e}")),
        }
    }
    Err(format!("The Mac did not answer at {}. Check the address, that MacBridge runs there, and that TCP port {port} is open. ({})", address.trim(), errors.join("; ")))
}

/// Reach the Mac of `session` (token from ID and password): on this network if it answers there
/// (RM_NO_LAN=1: never looked for), else through `relay`. Errors are in words for the user.
pub fn connect(relay: Option<&str>, session: &str, token: &str, wait: bool) -> Result<(TcpStream, Route), String> {
    if std::env::var_os("RM_NO_LAN").is_none() {
        if let Some(at) = discover(session, Duration::from_millis(800)) {
            return match join_direct(at, session, token) {
                Ok(s) => Ok((s, Route::Lan(at))),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(format!("the Mac refused: {e}")),
                Err(e) => Err(format!("the Mac at {at} on this network: {e}")),
            };
        }
    }
    let Some(relay) = relay.map(str::trim).filter(|r| !r.is_empty()) else {
        return Err("This Mac was not found on this network. To reach it over the internet, enter a relay server (host:port).".into());
    };
    let s = crate::join_with(relay, session, Role::Client, token, wait).map_err(|e| format!("relay: {e}"))?;
    Ok((s, Route::Relay(relay.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    #[test]
    fn addresses_are_checked() {
        assert_eq!(parse_address("192.168.1.20").unwrap(), ("192.168.1.20".into(), 7471));
        assert_eq!(parse_address(" mac.example.com:9000 ").unwrap(), ("mac.example.com".into(), 9000));
        assert_eq!(parse_address("[fe80::1]:7000").unwrap(), ("fe80::1".into(), 7000));
        assert_eq!(parse_address("[::1]").unwrap(), ("::1".into(), 7471));
        assert_eq!(parse_address("2001:db8::5").unwrap(), ("2001:db8::5".into(), 7471));
        assert_eq!(parse_address("my-mac.local").unwrap(), ("my-mac.local".into(), 7471));
        for bad in ["", "host:0", "host:70000", "host:abc", "http://mac", "a b", "-bad.com", "[::1", "[::1]x", "user@mac", "x".repeat(300).as_str()] {
            assert!(parse_address(bad).is_err(), "{bad:?}");
        }
    }

    /// A Mac that takes the join line on a TCP port, reached by IPv4, by IPv6 and by name.
    #[test]
    fn connects_straight_to_a_typed_address() {
        let serve = |l: TcpListener| {
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { continue };
                    let mut line = String::new();
                    std::io::BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
                    let j: Join = serde_json::from_str(line.trim()).unwrap();
                    let _ = s.write_all(if j.token == "good" { b"READY\n" } else { b"ERR no such session\n" });
                }
            })
        };
        let v4 = TcpListener::bind("127.0.0.1:0").unwrap();
        let p4 = v4.local_addr().unwrap().port();
        serve(v4);
        let (_, route) = connect_direct(&format!("127.0.0.1:{p4}"), "rm-1", "good").unwrap();
        assert_eq!(route, Route::Direct(format!("127.0.0.1:{p4}").parse().unwrap()));
        assert!(route.is_direct() && route.udp_relay() == "127.0.0.1:9");
        assert!(connect_direct(&format!("localhost:{p4}"), "rm-1", "good").is_ok(), "by name");
        assert!(connect_direct(&format!("127.0.0.1:{p4}"), "rm-1", "bad").unwrap_err().contains("refused"));
        if let Ok(v6) = TcpListener::bind("[::1]:0") {
            let p6 = v6.local_addr().unwrap().port();
            serve(v6);
            let (_, r) = connect_direct(&format!("[::1]:{p6}"), "rm-1", "good").unwrap();
            assert!(matches!(r, Route::Direct(a) if a.is_ipv6()));
        }
        // nothing listening: a clear error, quickly
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let t = Instant::now();
        assert!(connect_direct(&format!("127.0.0.1:{closed}"), "rm-1", "good").unwrap_err().contains("did not answer"));
        assert!(t.elapsed() < Duration::from_secs(6));
    }

    #[test]
    fn messages() {
        assert_eq!(parse_query(&query("rm-123456789")), Some("rm-123456789"));
        assert_eq!(parse_answer(&answer("rm-123456789", 7471), "rm-123456789"), Some(7471));
        assert_eq!(parse_answer(&answer("rm-123456789", 7471), "rm-987654321"), None);
        assert_eq!(parse_query(b"RMLAN!x 1"), None);
    }

    /// A stand-in for the Mac: answers the query on a UDP port, checks the join line's token.
    #[test]
    fn discovers_and_joins() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let uport = udp.local_addr().unwrap().port();
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let tport = tcp.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            loop {
                let Ok((n, from)) = udp.recv_from(&mut buf) else { return };
                if parse_query(&buf[..n]) == Some("rm-123456789") {
                    let _ = udp.send_to(&answer("rm-123456789", tport), from);
                }
            }
        });
        std::thread::spawn(move || {
            for s in tcp.incoming() {
                let Ok(mut s) = s else { continue };
                let mut line = String::new();
                std::io::BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
                let j: Join = serde_json::from_str(line.trim()).unwrap();
                let ok = j.session_id == "rm-123456789" && j.token == "good";
                let _ = s.write_all(if ok { b"READY\n" } else { b"ERR wrong password\n" });
            }
        });
        let at = discover_on("rm-123456789", Duration::from_secs(2), uport).expect("found on the network");
        assert_eq!(at.port(), tport);
        assert!(discover_on("rm-000000000", Duration::from_millis(300), uport).is_none());
        assert!(join_direct(at, "rm-123456789", "good").is_ok());
        let e = join_direct(at, "rm-123456789", "bad").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
