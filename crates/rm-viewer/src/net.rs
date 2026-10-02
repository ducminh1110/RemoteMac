//! Network side of the viewer: relay join, handshake, a receive thread that decodes video
//! into BGRA pictures, and a thread-safe sender. The UI only sees `UiEvent`s.

use rm_client::Session;
use rm_decode::{H264Decoder, Picture};
use rm_protocol::{write_message, Frame, Message};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// Encoded frames buffered per window before the viewer gives up on catching up, drops them
/// and asks the Mac for a fresh keyframe (Moonlight's "frame queue overflow -> IDR").
const DECODE_QUEUE: usize = 4;

/// Timing of one picture, for the stats overlay.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameMeta {
    /// agent capture time (agent clock, microseconds)
    pub pts_us: u64,
    /// the agent's clock when the frame was fully received, if known (offset from pings)
    pub received_agent_us: Option<i64>,
    pub decode_us: u32,
    pub via_udp: bool,
    pub bytes: u32,
}

#[derive(Debug)]
pub enum UiEvent {
    WindowCreated { id: u64, app: String, title: String, x: i32, y: i32, w: u32, h: u32, parent: Option<u64>, role: rm_protocol::WindowRole },
    Apps(Vec<rm_protocol::AppInfo>),
    MenuBar { app: String, menus: Vec<rm_protocol::MenuNode> },
    /// The Mac's virtual display (Mac points), or why there is none.
    Display { available: bool, width: u32, height: u32, reason: Option<String> },
    Uploaded { transfer_id: u64, remote_path: String },
    UploadFailed { transfer_id: u64, reason: String },
    /// Square RGBA icon (straight alpha) for an application id.
    Icon { app: String, size: u32, rgba: Vec<u8> },
    /// The remote clipboard now holds this text.
    Clipboard(String),
    Resized { id: u64, w: u32, h: u32 },
    Title { id: u64, title: String },
    Destroyed { id: u64 },
    Frame { id: u64, picture: Picture, meta: FrameMeta },
    AppExited(String),
    Notice(String),
    Disconnected(String),
}

#[derive(Clone)]
pub struct Link {
    writer: Arc<Mutex<TcpStream>>,
    /// UDP video path (FEC); None when it could not be started
    pub udp: Option<Arc<rm_client::udp::UdpVideo>>,
}

impl Link {
    pub fn send(&self, m: &Message) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = write_message(&mut *w, m);
        }
    }
}

/// Why connecting failed, in words for the user (the relay's `ERR ...` replies mapped).
pub fn friendly_error(e: &str) -> String {
    let m = if e.contains("no such session") {
        "This Mac is not online. Start remotemac on the Mac and check the ID."
    } else if e.contains("session mismatch") {
        "Wrong password (or the Mac is already in use)."
    } else if e.contains("locked") {
        "Too many wrong passwords. Wait a minute and try again."
    } else if e.contains("not admitted") {
        "The relay refused this viewer (relay key mismatch). Use the current RemoteMac build."
    } else if e.contains("pair timeout") {
        "The Mac did not answer in time. Try again."
    } else if e.contains("relay busy") {
        "The relay is busy. Try again shortly."
    } else if e.contains("handshake") {
        "Connected, but the Mac did not complete the handshake. Update remotemac on the Mac."
    } else {
        return format!("Cannot reach the RemoteMac server: {e}");
    };
    m.to_string()
}

/// Connect, handshake, optionally launch `app`, and start the receive thread.
/// `wake` is called (from the receive thread) after events are queued.
pub fn connect(relay: &str, session: &str, token: &str, app: Option<&str>, wake: impl Fn() + Send + Sync + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    connect_with(relay, session, token, app, true, wake)
}

/// [`connect`]; `wait: false` fails at once when the Mac is not waiting at the relay.
pub fn connect_with(relay: &str, session: &str, token: &str, app: Option<&str>, wait: bool, wake: impl Fn() + Send + Sync + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    let stream = rm_relay::join_with(relay, session, rm_relay::Role::Client, token, wait).map_err(|e| format!("relay: {e}"))?;
    let writer = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
    let sess = Session::handshake(stream).map_err(|e| format!("handshake: {e}"))?;
    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let mut link = Link { writer, udp: None };
    let video = Arc::new(Video { decoders: Mutex::new(HashMap::new()), link: Mutex::new(link.clone()), tx: tx.clone(), wake: wake.clone(), udp: Mutex::new(None) });
    // UDP video beside the TCP connection (RM_NO_UDP=1 keeps everything on TCP)
    if std::env::var_os("RM_NO_UDP").is_none() {
        let v = video.clone();
        match rm_client::udp::start(relay, session, token, move |o| v.on_udp(o)) {
            Ok(u) => {
                let u = Arc::new(u);
                *video.udp.lock().unwrap() = Some(u.clone());
                link.udp = Some(u);
                *video.link.lock().unwrap() = link.clone();
            }
            Err(e) => eprintln!("UDP video unavailable ({e}); using TCP"),
        }
    }
    if !sess.capabilities.can_stream_apps() {
        eprintln!("warning: host reports it cannot stream apps: {:?}", sess.capabilities);
    }
    if let Some(app) = app {
        link.send(&Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() });
    }
    let l2 = link.clone();
    std::thread::spawn(move || recv_loop(sess, l2, video, tx, wake));
    Ok((link, rx))
}

/// One window's decoder on its own thread: the socket keeps being read (input echoes, menus,
/// other windows) while a big frame decodes, and the UI only ever gets finished pictures.
struct DecodeWorker {
    tx: SyncSender<(rm_protocol::VideoFrame, FrameMeta)>,
    /// frames are being dropped until the next keyframe
    resync: bool,
}

/// Where video goes, from TCP or UDP: one decode worker per window.
struct Video {
    decoders: Mutex<HashMap<u64, DecodeWorker>>,
    link: Mutex<Link>,
    tx: Sender<UiEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    udp: Mutex<Option<Arc<rm_client::udp::UdpVideo>>>,
}

impl Video {
    fn link(&self) -> Link {
        self.link.lock().unwrap().clone()
    }

    fn on_udp(&self, o: rm_protocol::udp::Out) {
        match o {
            rm_protocol::udp::Out::Frame(v) => self.push(v, true),
            // lost even with FEC: the decoder needs a fresh keyframe
            rm_protocol::udp::Out::Lost(id) => self.link().send(&Message::RequestKeyframe { window_id: id }),
        }
    }

    fn push(&self, v: rm_protocol::VideoFrame, via_udp: bool) {
        let id = v.window_id;
        let received_agent_us = self.udp.lock().unwrap().as_ref().and_then(|u| u.agent_now_us());
        let meta = FrameMeta { pts_us: v.pts_us, received_agent_us, decode_us: 0, via_udp, bytes: v.data.len() as u32 };
        let mut decoders = self.decoders.lock().unwrap();
        let w = decoders.entry(id).or_insert_with(|| spawn_decoder(id, self.link(), self.tx.clone(), self.wake.clone()));
        if w.resync && !v.keyframe {
            return;
        }
        w.resync = false;
        match w.tx.try_send((v, meta)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // decoding cannot keep up: skip ahead to a fresh keyframe instead of showing an
                // ever older picture
                w.resync = true;
                self.link().send(&Message::RequestKeyframe { window_id: id });
            }
            Err(TrySendError::Disconnected(_)) => {
                decoders.remove(&id);
            }
        }
    }

    fn forget(&self, id: u64) {
        self.decoders.lock().unwrap().remove(&id);
    }
}

fn spawn_decoder(id: u64, link: Link, tx: Sender<UiEvent>, wake: Arc<dyn Fn() + Send + Sync>) -> DecodeWorker {
    let (ftx, frx) = sync_channel::<(rm_protocol::VideoFrame, FrameMeta)>(DECODE_QUEUE);
    std::thread::Builder::new()
        .name(format!("rm-decode-{id}"))
        .spawn(move || {
            let Ok(mut d) = H264Decoder::new() else { return };
            let mut last_ask = std::time::Instant::now() - std::time::Duration::from_secs(1);
            for (v, mut meta) in frx {
                let t = std::time::Instant::now();
                match d.decode(&v.data) {
                    Ok(Some(picture)) => {
                        meta.decode_us = t.elapsed().as_micros() as u32;
                        if tx.send(UiEvent::Frame { id, picture, meta }).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => {}
                    Err(_) => {
                        // a broken reference chain: ask for a keyframe (at most a few per second)
                        if last_ask.elapsed().as_millis() > 300 {
                            link.send(&Message::RequestKeyframe { window_id: id });
                            last_ask = std::time::Instant::now();
                        }
                    }
                }
            }
        })
        .expect("decode thread");
    DecodeWorker { tx: ftx, resync: false }
}

fn recv_loop(mut sess: Session<TcpStream>, _link: Link, video: Arc<Video>, tx: Sender<UiEvent>, wake: Arc<dyn Fn() + Send + Sync>) {
    let emit = |e: UiEvent| {
        if tx.send(e).is_ok() {
            wake();
        }
    };
    loop {
        match sess.recv() {
            Ok(Some(Frame::Video(v))) => video.push(v, false),
            Ok(Some(Frame::Msg(m))) => match m {
                Message::WindowCreated { window_id, application_id, title, bounds, parent_id, role } => emit(UiEvent::WindowCreated { id: window_id, app: application_id, title, x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h, parent: parent_id, role }),
                Message::Apps { apps } => emit(UiEvent::Apps(apps)),
                Message::DisplayStatus { available, width, height, reason, .. } => emit(UiEvent::Display { available, width, height, reason }),
                Message::MenuBar { application_id, menus } if rm_protocol::MenuNode::count(&menus) <= rm_protocol::MAX_MENU_ITEMS => {
                    emit(UiEvent::MenuBar { app: application_id, menus })
                }
                Message::FileUploaded { transfer_id, remote_path } => emit(UiEvent::Uploaded { transfer_id, remote_path }),
                Message::FileUploadFailed { transfer_id, reason } => emit(UiEvent::UploadFailed { transfer_id, reason }),
                Message::AppIcon { application_id, size, rgba_base64 } => {
                    if let Ok(rgba) = rm_protocol::base64_decode(&rgba_base64) {
                        if rgba.len() == (size * size * 4) as usize {
                            emit(UiEvent::Icon { app: application_id, size, rgba });
                        }
                    }
                }
                Message::ClipboardSet { text, .. } => emit(UiEvent::Clipboard(text)),
                Message::WindowMoved { window_id, bounds } => emit(UiEvent::Resized { id: window_id, w: bounds.w, h: bounds.h }),
                Message::WindowTitleChanged { window_id, title } => emit(UiEvent::Title { id: window_id, title }),
                Message::WindowDestroyed { window_id } => {
                    video.forget(window_id);
                    emit(UiEvent::Destroyed { id: window_id });
                }
                Message::AppExited { application_id, .. } => emit(UiEvent::AppExited(application_id)),
                Message::Error { code, message } => emit(UiEvent::Notice(format!("{code}: {message}"))),
                Message::CapabilityUnavailable { capability, reason } => emit(UiEvent::Notice(format!("{capability} unavailable: {reason}"))),
                _ => {}
            },
            Ok(None) => return emit(UiEvent::Disconnected("connection closed".into())),
            Err(e) => return emit(UiEvent::Disconnected(e.to_string())),
        }
    }
}

/// Send a local file to the agent in protocol-sized chunks. Runs on the caller's thread
/// (the UI spawns one); the agent answers with `FileUploaded` / `FileUploadFailed`.
pub fn upload_file(link: &Link, transfer_id: u64, path: &std::path::Path) -> Result<u64, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    if size > rm_protocol::MAX_UPLOAD {
        return Err(format!("{} is larger than the {} byte limit", path.display(), rm_protocol::MAX_UPLOAD));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "upload".into());
    link.send(&Message::FileUploadBegin { transfer_id, name, size });
    let mut buf = vec![0u8; rm_protocol::UPLOAD_CHUNK];
    let mut offset = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        link.send(&Message::FileUploadChunk { transfer_id, offset, data_base64: rm_protocol::base64_encode(&buf[..n]) });
        offset += n as u64;
    }
    link.send(&Message::FileUploadEnd { transfer_id });
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn viewer_net_layer_against_fake_agent() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let (a, tok) = (addr.clone(), "viewer-test-token-0123456789");
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "v-1", tok); });
        std::thread::sleep(Duration::from_millis(150));

        let (link, rx) = connect(&addr, "v-1", tok, Some("testapp"), || {}).unwrap();
        let (mut created, mut frames, mut titled) = (None, 0, false);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline && !(frames >= 20 && titled) {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(UiEvent::WindowCreated { id, w, h, .. }) => {
                    created = Some((id, w, h));
                    link.send(&Message::TextInput { window_id: id, text: "héllo".into() });
                }
                Ok(UiEvent::Frame { picture, .. }) => {
                    frames += 1;
                    assert_eq!((picture.width, picture.height), (480, 352));
                    assert_eq!(picture.bgra.len(), 480 * 352 * 4);
                }
                Ok(UiEvent::Title { title, .. }) => titled |= title.contains("[5 chars]"),
                Ok(_) | Err(_) => {}
            }
        }
        assert_eq!(created.map(|c| (c.1, c.2)), Some((480, 352)));
        assert!(frames >= 20, "decoded frames: {frames}");
        assert!(titled, "text typed through the viewer must reach the (fake) app");
        link.send(&Message::WindowClose { window_id: created.unwrap().0 });
        let mut destroyed = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !destroyed {
            if let Ok(UiEvent::Destroyed { .. }) = rx.recv_timeout(Duration::from_millis(300)) { destroyed = true }
        }
        assert!(destroyed);
    }

    /// Video moves to UDP once the path works, and FEC carries it through a lossy relay.
    fn udp_run(loss: Option<f64>, session: &str) -> (usize, usize, rm_client::udp::LinkStats) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, rm_relay::Config { udp_loss: loss, ..Default::default() }));
        let (a, tok, s2) = (addr.clone(), "viewer-udp-token-0123456789", session.to_string());
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, &s2, tok); });
        std::thread::sleep(Duration::from_millis(150));
        let (link, rx) = connect(&addr, session, tok, Some("testapp"), || {}).unwrap();
        let (mut tcp, mut udp) = (0, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        while std::time::Instant::now() < deadline && udp < 60 {
            if let Ok(UiEvent::Frame { picture, meta, .. }) = rx.recv_timeout(Duration::from_millis(300)) {
                assert_eq!(picture.bgra.len(), 480 * 352 * 4);
                if meta.via_udp { udp += 1 } else { tcp += 1 }
            }
        }
        let stats = link.udp.as_ref().unwrap().stats.lock().unwrap().clone();
        (tcp, udp, stats)
    }

    #[test]
    fn video_switches_to_udp() {
        let (tcp, udp, stats) = udp_run(None, "v-udp-1");
        assert!(udp >= 60, "frames over udp={udp} tcp={tcp} {stats:?}");
        assert!(stats.rtt_ms.is_some() && stats.offset_us.is_some(), "{stats:?}");
    }

    #[test]
    fn fec_carries_video_through_packet_loss() {
        let (tcp, udp, stats) = udp_run(Some(0.10), "v-udp-2");
        assert!(udp >= 60, "frames over a 10% lossy link: udp={udp} tcp={tcp} {stats:?}");
        assert!(stats.recovered > 0, "parity must have rebuilt some frames: {stats:?}");
    }

    #[test]
    fn several_apps_open_panel_and_upload() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let (a, tok) = (addr.clone(), "viewer-test-token-multi-0123456789");
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "v-2", tok); });
        std::thread::sleep(Duration::from_millis(150));
        let (link, rx) = connect(&addr, "v-2", tok, Some("testapp"), || {}).unwrap();
        link.send(&Message::AppLaunch { application_id: "notes".into(), arguments: vec![], working_directory: None, environment: Default::default() });

        let file = std::env::temp_dir().join(format!("rm-upload-{}.bin", std::process::id()));
        std::fs::write(&file, vec![42u8; 700 * 1024]).unwrap(); // three chunks
        let (mut mains, mut panel, mut uploaded, mut opened) = (HashMap::new(), None, None, false);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline && !opened {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(UiEvent::WindowCreated { id, app, role, parent, .. }) => match role {
                    rm_protocol::WindowRole::Window => {
                        mains.insert(app.clone(), id);
                        if app == "testapp" {
                            link.send(&Message::Key { window_id: id, physical_key: "KeyO".into(), modifiers: vec![rm_protocol::Modifier::Command], down: true });
                        }
                    }
                    rm_protocol::WindowRole::OpenPanel => {
                        assert_eq!(parent, mains.get("testapp").copied());
                        panel = Some(id);
                        upload_file(&link, 7, &file).unwrap();
                    }
                    _ => {}
                },
                Ok(UiEvent::Uploaded { transfer_id: 7, remote_path }) => {
                    assert!(remote_path.ends_with(&*file.file_name().unwrap().to_string_lossy()));
                    link.send(&Message::PanelChooseFile { window_id: panel.unwrap(), remote_path: remote_path.clone() });
                    uploaded = Some(remote_path);
                }
                Ok(UiEvent::Title { title, .. }) if title.contains("[opened ") => {
                    assert!(title.contains("716800 bytes"), "{title}");
                    opened = true;
                }
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&file);
        assert_eq!(mains.len(), 2, "two apps running side by side: {mains:?}");
        assert!(uploaded.is_some() && opened, "upload + panel choose: {uploaded:?}");
    }
}
