//! Client-side control session. The native Windows window compositor is not
//! built yet: it is gated on docs/SPEC.md §2 (feasibility gate).

use rm_core::{Event, SessionState};
use rm_protocol::{negotiate, read_message, write_message, AppInfo, CapabilityReport, Hello, Message, Negotiated, ProtocolError};
use std::io::{Read, Write};

pub struct Session<S: Read + Write> {
    stream: S,
    pub state: SessionState,
    pub negotiated: Negotiated,
    pub capabilities: CapabilityReport,
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
        let ours = Hello::ours(&format!("remote-mac {}", env!("CARGO_PKG_VERSION")), &["h264"], &["control"]);
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
        Ok(Self { stream, state, negotiated, capabilities })
    }

    pub fn list_apps(&mut self) -> Result<Vec<AppInfo>, ClientError> {
        write_message(&mut self.stream, &Message::ListApps)?;
        match next(&mut self.stream)? {
            Message::Apps { apps } => Ok(apps),
            m => Err(unexpected(m)),
        }
    }

    pub fn launch(&mut self, application_id: &str, arguments: Vec<String>) -> Result<u32, ClientError> {
        write_message(
            &mut self.stream,
            &Message::AppLaunch { application_id: application_id.into(), arguments, working_directory: None, environment: Default::default() },
        )?;
        match next(&mut self.stream)? {
            Message::AppLaunched { pid, .. } => Ok(pid),
            m => Err(unexpected(m)),
        }
    }

    pub fn terminate(&mut self, application_id: &str) -> Result<(), ClientError> {
        write_message(&mut self.stream, &Message::AppTerminate { application_id: application_id.into() })?;
        match next(&mut self.stream)? {
            Message::AppExited { .. } => Ok(()),
            m => Err(unexpected(m)),
        }
    }
}

fn next<S: Read>(s: &mut S) -> Result<Message, ClientError> {
    read_message(s)?.ok_or(ClientError::Closed)
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
}
