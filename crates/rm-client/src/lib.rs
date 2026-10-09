//! Client-side control session. The native Windows window compositor is not
//! built yet: it is gated on docs/SPEC.md §2 (feasibility gate).

use rm_core::{Event, SessionState};
use rm_protocol::{negotiate, read_frame, read_message, write_message, AppInfo, CapabilityReport, Frame, Hello, Message, Negotiated, ProtocolError};

pub mod e2e;
pub mod record;
pub mod udp;
use std::io::{Read, Write};

pub struct Session<S: Read + Write> {
    stream: S,
    pub state: SessionState,
    pub negotiated: Negotiated,
    pub capabilities: CapabilityReport,
    /// UDP video (frames rebuilt from FEC shards) when attached
    udp: Option<(std::sync::Arc<udp::UdpVideo>, std::sync::mpsc::Receiver<rm_protocol::udp::Out>)>,
    /// our direct-path offer, once the UDP thread knows our addresses (sent by `recv`)
    offer: Option<std::sync::mpsc::Receiver<Message>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("agent error {code}: {message}")]
    Agent { code: String, message: String },
    #[error("unexpected message: {0:?}")]
    Unexpected(Box<Message>),
    #[error("connection closed")]
    Closed,
}

impl<S: Read + Write> Session<S> {
    /// Runs the handshake. Only returns once the agent has sent ServerHello
    /// AND its capability report; `state` is then `Ready`, never earlier.
    pub fn handshake(mut stream: S) -> Result<Self, ClientError> {
        let ours = Hello::ours(&format!("remote-mac {}", env!("CARGO_PKG_VERSION")), &["h264"], &["control", "audio", "open_file"]);
        write_message(&mut stream, &Message::ClientHello(ours.clone()))?;
        let theirs = match next(&mut stream)? {
            Message::ServerHello(h) => h,
            m => return Err(unexpected(m)),
        };
        let negotiated = negotiate(&ours, &theirs)?;
        let capabilities = match next(&mut stream)? {
            Message::CapabilityReport(c) => c,
            m => return Err(unexpected(m)),
        };
        let state = SessionState::Connecting.next(Event::HandshakeComplete).expect("valid transition");
        Ok(Self { stream, state, negotiated, capabilities, udp: None, offer: None })
    }

    pub fn list_apps(&mut self) -> Result<Vec<AppInfo>, ClientError> {
        write_message(&mut self.stream, &Message::ListApps)?;
        match self.next_msg()? {
            Message::Apps { apps } => Ok(apps),
            m => Err(unexpected(m)),
        }
    }

    pub fn launch(&mut self, application_id: &str, arguments: Vec<String>) -> Result<u32, ClientError> {
        write_message(
            &mut self.stream,
            &Message::AppLaunch { application_id: application_id.into(), arguments, working_directory: None, environment: Default::default() },
        )?;
        match self.next_msg()? {
            Message::AppLaunched { pid, .. } => Ok(pid),
            m => Err(unexpected(m)),
        }
    }

    /// The next control message; the agent's direct-path offer is taken on the way.
    fn next_msg(&mut self) -> Result<Message, ClientError> {
        loop {
            match next(&mut self.stream)? {
                Message::P2pOffer { secret, candidates } => {
                    if let Some((u, _)) = &self.udp {
                        u.peer_offer(&secret, &candidates);
                    }
                }
                m => return Ok(m),
            }
        }
    }

    pub fn send(&mut self, m: &Message) -> Result<(), ProtocolError> {
        write_message(&mut self.stream, m)
    }

    /// Receive video over UDP too (through the same relay). Use a short read timeout on the
    /// TCP stream so UDP frames are not held up behind it.
    /// `keys`: the session's, from its secure handshake (None: plain, for tests without one).
    pub fn attach_udp(&mut self, relay: &str, session: &str, keys: Option<&rm_protocol::secure::Keys>) -> std::io::Result<()> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (otx, orx) = std::sync::mpsc::channel();
        let u = udp::start(
            relay,
            session,
            &rm_protocol::session::relay_token(session),
            keys,
            false,
            move |o| {
                let _ = tx.send(o);
            },
            move |secret, candidates| {
                let _ = otx.send(Message::P2pOffer { secret, candidates });
            },
        )?;
        self.udp = Some((std::sync::Arc::new(u), rx));
        self.offer = Some(orx);
        Ok(())
    }

    /// The UDP path (GameStream tunnel datagrams ride it too).
    pub fn udp(&self) -> Option<std::sync::Arc<udp::UdpVideo>> {
        self.udp.as_ref().map(|(u, _)| u.clone())
    }

    pub fn udp_stats(&self) -> Option<udp::LinkStats> {
        self.udp.as_ref().and_then(|(u, _)| u.stats.lock().ok().map(|s| s.clone()))
    }

    /// Next frame of any kind (control message or video, from TCP or UDP).
    pub fn recv(&mut self) -> Result<Option<Frame>, ProtocolError> {
        while let Some(o) = self.udp.as_ref().and_then(|(_, rx)| rx.try_recv().ok()) {
            match o {
                rm_protocol::udp::Out::Frame(v) => return Ok(Some(Frame::Video(v))),
                rm_protocol::udp::Out::Audio(a) => return Ok(Some(Frame::Audio(a))),
                // a frame lost even with FEC: ask for a keyframe and carry on
                rm_protocol::udp::Out::Lost(id) => write_message(&mut self.stream, &Message::RequestKeyframe { window_id: id })?,
            }
        }
        if let Some(m) = self.offer.as_ref().and_then(|o| o.try_recv().ok()) {
            write_message(&mut self.stream, &m)?;
        }
        let f = read_frame(&mut self.stream)?;
        if let (Some(Frame::Msg(Message::P2pOffer { secret, candidates })), Some((u, _))) = (&f, &self.udp) {
            u.peer_offer(secret, candidates);
        }
        Ok(f)
    }

    pub fn terminate(&mut self, application_id: &str) -> Result<(), ClientError> {
        write_message(&mut self.stream, &Message::AppTerminate { application_id: application_id.into() })?;
        match self.next_msg()? {
            Message::AppExited { .. } => Ok(()),
            m => Err(unexpected(m)),
        }
    }
}

/// The next control message, riding out short read timeouts (the stream may have one so that
/// UDP video is not held up) for up to 15 s.
fn next<S: Read>(s: &mut S) -> Result<Message, ClientError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match read_message(s) {
            Err(ProtocolError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) && std::time::Instant::now() < deadline => continue,
            r => return r?.ok_or(ClientError::Closed),
        }
    }
}

fn unexpected(m: Message) -> ClientError {
    match m {
        Message::Error { code, message } => ClientError::Agent { code, message },
        m => ClientError::Unexpected(Box::new(m)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_agent::{serve, Launcher};
    use rm_core::{AppRegistry, ValidatedLaunch};
    use rm_relay::{join, Role};
    use std::net::TcpListener;

    struct Fake;
    impl Launcher for Fake {
        fn launch(&mut self, _: &ValidatedLaunch) -> std::io::Result<u32> {
            Ok(77)
        }
        fn terminate(&mut self, _: u32) -> std::io::Result<()> {
            Ok(())
        }
        fn exists(&self, _: &std::path::Path) -> bool {
            true
        }
    }

    /// client <-> relay <-> agent over real sockets.
    #[test]
    fn end_to_end_through_relay() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let tok = "e2e-token-0123456789";

        let a = addr.clone();
        let agent = std::thread::spawn(move || {
            let mut s = join(&a, "e2e-1", Role::Agent, tok).unwrap();
            let reg = AppRegistry::default_macos("/tmp/rm-e2e");
            serve(&mut s, &reg, &CapabilityReport::unknown("e2e"), &mut Fake).unwrap();
        });
        std::thread::sleep(std::time::Duration::from_millis(100));

        let stream = join(&addr, "e2e-1", Role::Client, tok).unwrap();
        let mut sess = Session::handshake(stream).unwrap();
        assert!(sess.state.is_connected());
        assert!(!sess.capabilities.can_stream_apps(), "unknown caps must not claim streaming");
        assert!(sess.list_apps().unwrap().iter().any(|a| a.id == "textedit"));
        assert_eq!(sess.launch("textedit", vec![]).unwrap(), 77);
        assert!(matches!(sess.launch("bash", vec![]), Err(ClientError::Agent { .. })));
        sess.terminate("textedit").unwrap();
        drop(sess);
        agent.join().unwrap();
    }

    /// The whole client scenario (launch, window, H.264 frames decoded, keyboard, mouse,
    /// modifiers, close) against the scripted fake agent over a real relay.
    #[test]
    fn e2e_scenario_against_fake_agent() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let tok = "fake-e2e-token-0123456789";
        let a = addr.clone();
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "fake-1", tok); });
        std::thread::sleep(std::time::Duration::from_millis(150));

        // as the CLI runs it: end-to-end encrypted, video over UDP beside a short-timeout TCP stream
        let stream = join(&addr, "fake-1", Role::Client, &rm_protocol::session::relay_token("fake-1")).unwrap();
        let (stream, keys) = rm_protocol::secure::client_tcp(stream, "fake-1", tok).unwrap();
        stream.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(20))).unwrap();
        let mut sess = Session::handshake(stream).unwrap();
        sess.attach_udp(&addr, "fake-1", Some(&keys)).unwrap();
        let report = crate::e2e::run(&mut sess, "testapp");
        for c in &report.checks {
            assert!(c.1, "check failed: {} -> {}", c.0, c.2);
        }
        assert!(report.decoded >= 30 && report.fps() > 5.0, "decoded={} fps={}", report.decoded, report.fps());
        let udp = sess.udp_stats().unwrap();
        assert!(udp.frames >= 30, "video must have come over UDP: {udp:?}");
    }

    /// Record a session with the fake agent, then replay it: the replayed client sees the same
    /// windows, menu bar and decodable video.
    #[test]
    fn record_then_replay() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let tok = "record-token-0123456789";
        let a = addr.clone();
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "rec-1", tok); });
        std::thread::sleep(std::time::Duration::from_millis(150));
        let stream = join(&addr, "rec-1", Role::Client, &rm_protocol::session::relay_token("rec-1")).unwrap();
        let (stream, _) = rm_protocol::secure::client_tcp(stream, "rec-1", tok).unwrap();
        stream.get_ref().set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
        let mut sess = Session::handshake(stream).unwrap();
        let mut file = vec![];
        let plan = crate::record::Plan { apps: vec!["testapp".into(), "notes".into()], settle: std::time::Duration::from_secs(2), max: std::time::Duration::from_secs(10), on_segment_end: Box::new(|_| {}) };
        let sum = crate::record::record(&mut sess, &mut file, plan).unwrap();
        assert!(sum.apps.iter().all(|a| a.1 >= 1 && a.2 >= 10), "{:?}", sum.apps);

        let rec = rm_fakeagent::replay::Recording::from_records(rm_protocol::recording::read_all(&mut file.as_slice()).unwrap());
        let a = addr.clone();
        let rec = std::sync::Arc::new(rec);
        std::thread::spawn(move || {
            let s = join(&a, "rep-1", Role::Agent, tok).unwrap();
            let w = s.try_clone().unwrap();
            let _ = rm_fakeagent::replay::serve_replay(s, w, rec);
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        let stream = join(&addr, "rep-1", Role::Client, tok).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_millis(500))).unwrap();
        let mut sess = Session::handshake(stream).unwrap();
        let apps = sess.list_apps().unwrap();
        assert!(apps.iter().any(|a| a.id == "notes" && a.available), "{apps:?}");
        sess.send(&Message::AppLaunch { application_id: "notes".into(), arguments: vec![], working_directory: None, environment: Default::default() }).unwrap();
        let mut dec = rm_decode::H264Decoder::new().unwrap();
        let (mut window, mut pictures, mut menu) = (None, 0, false);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < until && !(window.is_some() && pictures >= 5 && menu) {
            match sess.recv() {
                Ok(Some(Frame::Msg(Message::WindowCreated { application_id, .. }))) => window = Some(application_id),
                Ok(Some(Frame::Msg(Message::MenuBar { menus, .. }))) => menu = menus.iter().any(|m| m.title == "File"),
                Ok(Some(Frame::Video(v))) => pictures += dec.decode(&v.data).ok().flatten().is_some() as usize,
                _ => {}
            }
        }
        assert_eq!(window.as_deref(), Some("notes"));
        assert!(pictures >= 5 && menu, "pictures={pictures} menu={menu}");
    }
}
