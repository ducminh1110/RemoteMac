//! End-to-end encryption between the viewer and the Mac, whatever carries the bytes (a relay or
//! the local network). Kept byte for byte identical in Swift (`agent/macos/Secure.swift`).
//!
//! **Handshake** (right after the relay's or the Mac's `READY`): a password-authenticated key
//! exchange in the manner of CPace, on P-256 with x-coordinates only (what CryptoKit's ECDH
//! gives). Both sides turn the session secret (made from ID and password) into a curve point
//! `G`; each sends `x(y·G)` for a fresh random `y`; both get `K = x(y_mac·y_viewer·G)`, and keys
//! from it. Someone who does not know the password, the relay included, learns nothing it could
//! test passwords against offline; a wrong guess costs one connection (and the Mac locks out
//! after five in a row).
//!
//! ```text
//!   Mac    -> viewer   "RMK2" | Ym (32)            ("RMKL": locked, try again later)
//!   viewer -> Mac      Yc (32) | Tc (32)            Tc = HMAC(kc, "client" | Ym | Yc)
//!   Mac    -> viewer   1 | Ta (32)                  Ta = HMAC(kc, "agent"  | Ym | Yc)
//!                      0                            (wrong password)
//!   keys = HKDF-SHA256(salt = session, ikm = K, info = "remotemac/v2 keys" | Ym | Yc), 160 bytes:
//!          kc | Mac->viewer stream | viewer->Mac stream | Mac->viewer UDP | viewer->Mac UDP
//! ```
//!
//! **Stream:** records `u32 BE length | ChaCha20-Poly1305(data)`, nonce = 4 zero bytes and a
//! 64-bit record counter per direction.
//!
//! **UDP:** datagrams of type 16 and up (everything but the relay's own and the hole punch)
//! become `"RM" | type | u64 BE seq | ChaCha20-Poly1305(rest)`, the 3-byte header as associated
//! data, nonce = 4 zero bytes and `seq`.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{AffinePoint, EncodedPoint, NonZeroScalar, ProjectivePoint};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const HELLO: &[u8; 4] = b"RMK2";
pub const LOCKED: &[u8; 4] = b"RMKL";
/// Largest plaintext in one stream record.
pub const MAX_RECORD: usize = 64 * 1024;
const TAG: usize = 16;

#[derive(Debug)]
pub enum HandshakeError {
    WrongPassword,
    Locked,
    /// The other side does not speak this handshake (an older MacBridge).
    OldPeer,
    Failed(String),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "wrong password"),
            Self::Locked => write!(f, "locked: too many wrong passwords, try again in a minute"),
            Self::OldPeer => write!(f, "the other side runs an older MacBridge without encryption: update both"),
            Self::Failed(e) => write!(f, "secure handshake: {e}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<io::Error> for HandshakeError {
    fn from(e: io::Error) -> Self {
        Self::Failed(e.to_string())
    }
}

/// The keys of one session.
#[derive(Clone)]
pub struct Keys {
    pub stream_tx: [u8; 32],
    pub stream_rx: [u8; 32],
    pub udp_tx: [u8; 32],
    pub udp_rx: [u8; 32],
}

/// The curve point both sides make from the session and its secret: the first of
/// SHA-256("remotemac/v2/G" | u16 len | session | u16 len | secret | counter) that is the
/// x-coordinate of a point (taken with even y).
pub fn generator(session: &str, secret: &str) -> AffinePoint {
    for ctr in 0..=255u8 {
        let mut h = Sha256::new();
        h.update(b"remotemac/v2/G");
        for f in [session.as_bytes(), secret.as_bytes()] {
            h.update((f.len() as u16).to_be_bytes());
            h.update(f);
        }
        h.update([ctr]);
        if let Some(p) = lift(&h.finalize().into()) {
            return p;
        }
    }
    unreachable!("about half of all x-coordinates are on the curve")
}

/// The point with x-coordinate `x` (even y), if there is one.
fn lift(x: &[u8; 32]) -> Option<AffinePoint> {
    let mut b = [0u8; 33];
    b[0] = 2;
    b[1..].copy_from_slice(x);
    let ep = EncodedPoint::from_bytes(b).ok()?;
    Option::from(AffinePoint::from_encoded_point(&ep))
}

/// x-coordinate of `k·p` (None for the point at infinity).
fn x_mul(k: &NonZeroScalar, p: &AffinePoint) -> Option<[u8; 32]> {
    let q = (ProjectivePoint::from(*p) * **k).to_affine();
    let ep = q.to_encoded_point(false);
    ep.x().map(|x| {
        let mut o = [0u8; 32];
        o.copy_from_slice(x);
        o
    })
}

fn random_scalar() -> NonZeroScalar {
    NonZeroScalar::random(&mut rand_core::OsRng)
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// (kc, keys as seen by the Mac)
fn derive(session: &str, k: &[u8; 32], ym: &[u8; 32], yc: &[u8; 32]) -> ([u8; 32], [[u8; 32]; 4]) {
    let hk = hkdf::Hkdf::<Sha256>::new(Some(session.as_bytes()), k);
    let mut okm = [0u8; 160];
    let mut info = b"remotemac/v2 keys".to_vec();
    info.extend_from_slice(ym);
    info.extend_from_slice(yc);
    hk.expand(&info, &mut okm).expect("160 bytes is a valid length");
    let part = |i: usize| <[u8; 32]>::try_from(&okm[i * 32..i * 32 + 32]).unwrap();
    (part(0), [part(1), part(2), part(3), part(4)])
}

fn eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// The viewer's side: after `READY`, prove the password and agree on the keys.
pub fn client<S: Read + Write>(s: &mut S, session: &str, secret: &str) -> Result<Keys, HandshakeError> {
    let mut hello = [0u8; 36];
    s.read_exact(&mut hello).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => HandshakeError::OldPeer,
        _ => e.into(),
    })?;
    if &hello[..4] == LOCKED {
        return Err(HandshakeError::Locked);
    }
    if &hello[..4] != HELLO {
        return Err(HandshakeError::OldPeer);
    }
    let ym: [u8; 32] = hello[4..].try_into().unwrap();
    let g = generator(session, secret);
    let y = random_scalar();
    let yc = x_mul(&y, &g).ok_or_else(|| HandshakeError::Failed("bad point".into()))?;
    let peer = lift(&ym).ok_or_else(|| HandshakeError::Failed("the Mac sent a bad point".into()))?;
    let k = x_mul(&y, &peer).ok_or_else(|| HandshakeError::Failed("bad point".into()))?;
    let (kc, [a2c, c2a, ua2c, uc2a]) = derive(session, &k, &ym, &yc);
    let mut out = yc.to_vec();
    out.extend_from_slice(&hmac(&kc, &[b"client", &ym, &yc]));
    s.write_all(&out)?;
    s.flush()?;
    let mut status = [0u8; 1];
    match s.read_exact(&mut status) {
        Ok(()) if status[0] == 1 => {}
        Ok(()) => return Err(HandshakeError::WrongPassword),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(HandshakeError::WrongPassword),
        Err(e) => return Err(e.into()),
    }
    let mut ta = [0u8; 32];
    s.read_exact(&mut ta)?;
    if !eq(&ta, &hmac(&kc, &[b"agent", &ym, &yc])) {
        return Err(HandshakeError::Failed("the Mac could not prove the password".into()));
    }
    Ok(Keys { stream_tx: c2a, stream_rx: a2c, udp_tx: uc2a, udp_rx: ua2c })
}

/// The Mac's side (the Rust test agents; the real one is in Swift). `locked`: refuse at once.
pub fn agent<S: Read + Write>(s: &mut S, session: &str, secret: &str, locked: bool) -> Result<Keys, HandshakeError> {
    if locked {
        s.write_all(LOCKED)?;
        s.write_all(&[0u8; 32])?;
        return Err(HandshakeError::Locked);
    }
    let g = generator(session, secret);
    let y = random_scalar();
    let ym = x_mul(&y, &g).ok_or_else(|| HandshakeError::Failed("bad point".into()))?;
    let mut hello = HELLO.to_vec();
    hello.extend_from_slice(&ym);
    s.write_all(&hello)?;
    s.flush()?;
    let mut reply = [0u8; 64];
    s.read_exact(&mut reply)?;
    let yc: [u8; 32] = reply[..32].try_into().unwrap();
    let peer = lift(&yc).ok_or_else(|| HandshakeError::Failed("the viewer sent a bad point".into()))?;
    let k = x_mul(&y, &peer).ok_or_else(|| HandshakeError::Failed("bad point".into()))?;
    let (kc, [a2c, c2a, ua2c, uc2a]) = derive(session, &k, &ym, &yc);
    if !eq(&reply[32..], &hmac(&kc, &[b"client", &ym, &yc])) {
        let _ = s.write_all(&[0]);
        return Err(HandshakeError::WrongPassword);
    }
    let mut ok = vec![1u8];
    ok.extend_from_slice(&hmac(&kc, &[b"agent", &ym, &yc]));
    s.write_all(&ok)?;
    s.flush()?;
    Ok(Keys { stream_tx: a2c, stream_rx: c2a, udp_tx: ua2c, udp_rx: uc2a })
}

fn read_full<R: Read>(r: &mut R, mut buf: &mut [u8]) -> io::Result<()> {
    use io::ErrorKind::*;
    while !buf.is_empty() {
        match r.read(buf) {
            Ok(0) => return Err(io::Error::new(UnexpectedEof, "eof mid-record")),
            Ok(n) => buf = &mut buf[n..],
            Err(e) if matches!(e.kind(), Interrupted | WouldBlock | TimedOut) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn nonce(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    n
}

struct Tx {
    aead: ChaCha20Poly1305,
    counter: u64,
}

struct Rx {
    aead: ChaCha20Poly1305,
    counter: u64,
    buf: Vec<u8>,
    pos: usize,
}

/// An encrypted byte stream over `S`. Clones (of a TCP stream) share the cipher states, so one
/// thread may read while another writes.
pub struct SecureStream<S> {
    inner: S,
    tx: Arc<Mutex<Tx>>,
    rx: Arc<Mutex<Rx>>,
}

impl<S> SecureStream<S> {
    pub fn new(inner: S, keys: &Keys) -> Self {
        let tx = Tx { aead: ChaCha20Poly1305::new(Key::from_slice(&keys.stream_tx)), counter: 0 };
        let rx = Rx { aead: ChaCha20Poly1305::new(Key::from_slice(&keys.stream_rx)), counter: 0, buf: vec![], pos: 0 };
        Self { inner, tx: Arc::new(Mutex::new(tx)), rx: Arc::new(Mutex::new(rx)) }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl SecureStream<std::net::TcpStream> {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { inner: self.inner.try_clone()?, tx: self.tx.clone(), rx: self.rx.clone() })
    }
}

impl<S: Read> Read for SecureStream<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut rx = self.rx.lock().unwrap();
        if rx.pos >= rx.buf.len() {
            let mut len = [0u8; 4];
            // a clean end of the stream between records is an EOF, not an error; a read timeout
            // can only surface before a record, never in the middle of one
            match self.inner.read(&mut len[..1])? {
                0 => return Ok(0),
                _ => read_full(&mut self.inner, &mut len[1..])?,
            }
            let n = u32::from_be_bytes(len) as usize;
            if !(TAG..=MAX_RECORD + TAG).contains(&n) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "bad encrypted record length"));
            }
            let mut ct = vec![0u8; n];
            read_full(&mut self.inner, &mut ct)?;
            let nc = nonce(rx.counter);
            let pt = rx.aead.decrypt(Nonce::from_slice(&nc), ct.as_slice()).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "encrypted record failed authentication"))?;
            rx.counter += 1;
            rx.buf = pt;
            rx.pos = 0;
        }
        let n = out.len().min(rx.buf.len() - rx.pos);
        out[..n].copy_from_slice(&rx.buf[rx.pos..rx.pos + n]);
        rx.pos += n;
        Ok(n)
    }
}

impl<S: Write> Write for SecureStream<S> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut tx = self.tx.lock().unwrap();
        for chunk in data.chunks(MAX_RECORD) {
            let nc = nonce(tx.counter);
            let ct = tx.aead.encrypt(Nonce::from_slice(&nc), chunk).map_err(|_| io::Error::other("encryption failed"))?;
            tx.counter += 1;
            let mut rec = Vec::with_capacity(4 + ct.len());
            rec.extend_from_slice(&(ct.len() as u32).to_be_bytes());
            rec.extend_from_slice(&ct);
            // under the lock: records go out in counter order
            self.inner.write_all(&rec)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Whether a datagram of this type is encrypted (all but the relay's own and the hole punch).
pub fn sealed_type(t: u8) -> bool {
    t >= 16 && t != crate::udp::T_PUNCH
}

/// Encryption of the UDP datagrams of a session.
pub struct Datagrams {
    tx: ChaCha20Poly1305,
    rx: ChaCha20Poly1305,
    seq: AtomicU64,
}

impl Datagrams {
    pub fn new(keys: &Keys) -> Self {
        Self { tx: ChaCha20Poly1305::new(Key::from_slice(&keys.udp_tx)), rx: ChaCha20Poly1305::new(Key::from_slice(&keys.udp_rx)), seq: AtomicU64::new(0) }
    }

    /// The datagram as sent (unchanged when its type is not encrypted).
    pub fn seal(&self, d: &[u8]) -> Vec<u8> {
        if d.len() < 3 || d[..2] != crate::udp::MAGIC || !sealed_type(d[2]) {
            return d.to_vec();
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let ct = self.tx.encrypt(Nonce::from_slice(&nonce(seq)), Payload { msg: &d[3..], aad: &d[..3] }).expect("encryption does not fail");
        let mut o = Vec::with_capacity(11 + ct.len());
        o.extend_from_slice(&d[..3]);
        o.extend_from_slice(&seq.to_be_bytes());
        o.extend_from_slice(&ct);
        o
    }

    /// The datagram as received, opened; None when it is forged or damaged. Types that are not
    /// encrypted pass as they are.
    pub fn open(&self, d: &[u8]) -> Option<Vec<u8>> {
        if d.len() < 3 || d[..2] != crate::udp::MAGIC || !sealed_type(d[2]) {
            return Some(d.to_vec());
        }
        if d.len() < 11 + TAG {
            return None;
        }
        let seq = u64::from_be_bytes(d[3..11].try_into().unwrap());
        let pt = self.rx.decrypt(Nonce::from_slice(&nonce(seq)), Payload { msg: &d[11..], aad: &d[..3] }).ok()?;
        let mut o = d[..3].to_vec();
        o.extend_from_slice(&pt);
        Some(o)
    }
}

/// A UDP socket that seals what it sends and opens what it receives ([`Datagrams`]); without
/// a cipher it is a plain socket. Forged or damaged datagrams come back as `InvalidData`.
/// Clones share the cipher (and so its sequence numbers: a nonce is never used twice).
pub struct SealedUdp {
    pub sock: std::net::UdpSocket,
    cipher: Option<Arc<Datagrams>>,
}

impl SealedUdp {
    pub fn new(sock: std::net::UdpSocket, keys: Option<&Keys>) -> Self {
        Self { sock, cipher: keys.map(|k| Arc::new(Datagrams::new(k))) }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { sock: self.sock.try_clone()?, cipher: self.cipher.clone() })
    }

    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.sock.local_addr()
    }

    pub fn set_read_timeout(&self, t: Option<std::time::Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(t)
    }

    pub fn send_to<A: std::net::ToSocketAddrs>(&self, d: &[u8], to: A) -> io::Result<usize> {
        match &self.cipher {
            Some(c) => self.sock.send_to(&c.seal(d), to),
            None => self.sock.send_to(d, to),
        }
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, std::net::SocketAddr)> {
        let (n, from) = self.sock.recv_from(buf)?;
        let Some(c) = &self.cipher else { return Ok((n, from)) };
        let d = c.open(&buf[..n]).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "forged datagram"))?;
        buf[..d.len()].copy_from_slice(&d);
        Ok((d.len(), from))
    }
}

/// The two halves of the stream handshake run over a TCP connection, with a time limit.
pub fn client_tcp(mut s: std::net::TcpStream, session: &str, secret: &str) -> Result<(SecureStream<std::net::TcpStream>, Keys), HandshakeError> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let keys = client(&mut s, session, secret)?;
    s.set_read_timeout(None)?;
    Ok((SecureStream::new(s, &keys), keys))
}

pub fn agent_tcp(mut s: std::net::TcpStream, session: &str, secret: &str) -> Result<(SecureStream<std::net::TcpStream>, Keys), HandshakeError> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let keys = agent(&mut s, session, secret, false)?;
    s.set_read_timeout(None)?;
    Ok((SecureStream::new(s, &keys), keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (b, _) = l.accept().unwrap();
        (a, b)
    }

    #[test]
    fn the_right_password_gets_matching_keys_and_a_working_stream() {
        let (c, m) = pair();
        let mac = std::thread::spawn(move || agent_tcp(m, "rm-123456789", "secret-a"));
        let (mut cs, ck) = client_tcp(c, "rm-123456789", "secret-a").unwrap();
        let (mut ms, mk) = mac.join().unwrap().unwrap();
        assert_eq!(ck.stream_tx, mk.stream_rx);
        assert_eq!(ck.udp_rx, mk.udp_tx);
        assert_ne!(ck.stream_tx, ck.stream_rx);
        // big writes are split into records and come out whole
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
        let b2 = big.clone();
        let w = std::thread::spawn(move || {
            ms.write_all(&b2).unwrap();
            ms.write_all(b"done").unwrap();
            ms
        });
        let mut got = vec![0u8; big.len() + 4];
        cs.read_exact(&mut got).unwrap();
        assert_eq!(&got[..big.len()], &big[..]);
        assert_eq!(&got[big.len()..], b"done");
        let ms = w.join().unwrap();
        // what goes over the wire is not the plaintext
        drop(ms);
        assert_eq!(cs.read(&mut [0u8; 8]).unwrap(), 0, "clean EOF after the last record");
    }

    #[test]
    fn a_wrong_password_is_refused_on_both_sides() {
        let (c, m) = pair();
        let mac = std::thread::spawn(move || agent_tcp(m, "rm-1", "right"));
        let e = client_tcp(c, "rm-1", "wrong").err().unwrap();
        assert!(matches!(e, HandshakeError::WrongPassword), "{e}");
        assert!(matches!(mac.join().unwrap().err().unwrap(), HandshakeError::WrongPassword));
    }

    #[test]
    fn locked_and_old_peers_are_told_apart() {
        let (mut c, mut m) = pair();
        std::thread::spawn(move || agent(&mut m, "s", "x", true));
        assert!(matches!(client(&mut c, "s", "x").err().unwrap(), HandshakeError::Locked));
        let (c, m) = pair();
        drop(m);
        assert!(matches!(client_tcp(c, "s", "x").err().unwrap(), HandshakeError::OldPeer));
    }

    #[test]
    fn tampered_records_are_rejected() {
        let k = Keys { stream_tx: [1; 32], stream_rx: [1; 32], udp_tx: [2; 32], udp_rx: [2; 32] };
        let mut w = SecureStream::new(Vec::<u8>::new(), &k);
        w.write_all(b"hello").unwrap();
        let mut wire = w.inner.clone();
        assert!(!wire.windows(5).any(|x| x == b"hello"));
        wire[6] ^= 1;
        let mut r = SecureStream::new(std::io::Cursor::new(wire), &k);
        assert!(r.read(&mut [0u8; 8]).is_err());
    }

    #[test]
    fn datagrams_round_trip_and_keep_relay_types_readable() {
        let a = Keys { stream_tx: [0; 32], stream_rx: [0; 32], udp_tx: [3; 32], udp_rx: [4; 32] };
        let b = Keys { stream_tx: [0; 32], stream_rx: [0; 32], udp_tx: [4; 32], udp_rx: [3; 32] };
        let (da, db) = (Datagrams::new(&a), Datagrams::new(&b));
        let video = [b'R', b'M', crate::udp::T_VIDEO, 9, 9, 9];
        let s = da.seal(&video);
        assert_eq!(&s[..3], &video[..3]);
        assert_ne!(&s[3..], &video[3..]);
        assert_eq!(db.open(&s).unwrap(), video);
        let mut bad = s.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(db.open(&bad).is_none());
        let punch = crate::udp::punch(&[7; 16], false);
        assert_eq!(da.seal(&punch), punch, "the hole punch stays as it is");
        let register = [b'R', b'M', 1, 0];
        assert_eq!(da.seal(&register), register);
    }

    /// Fixed values shared with the Swift agent (the same computations in Secure.swift).
    #[test]
    fn shared_vectors() {
        let g = generator("rm-123456789", "3a6365467c85f122da38bf3b7192b081049bbf94ace2a9e0");
        let x = g.to_encoded_point(true);
        let hex: String = x.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "0257182b0a31970df563ea4f030bab772b0b852eccaf68d3b651d737058a8b05f8");
        assert_eq!(crate::session::relay_token("rm-123456789"), "9ad6e704b2c652cfafac52f6da98942bdb9fbb428b2894fc");
    }
}
