//! Network side of the viewer: relay join, handshake, a receive thread that decodes video
//! into BGRA pictures, and a thread-safe sender. The UI only sees `UiEvent`s.

use rm_client::Session;
use rm_decode::{H264Decoder, Picture};
use rm_protocol::{write_message, Frame, Message};
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum UiEvent {
    WindowCreated { id: u64, app: String, title: String, x: i32, y: i32, w: u32, h: u32, parent: Option<u64>, role: rm_protocol::WindowRole },
    Apps(Vec<rm_protocol::AppInfo>),
    Uploaded { transfer_id: u64, remote_path: String },
    UploadFailed { transfer_id: u64, reason: String },
    /// Square RGBA icon (straight alpha) for an application id.
    Icon { app: String, size: u32, rgba: Vec<u8> },
    /// The remote clipboard now holds this text.
    Clipboard(String),
    Resized { id: u64, w: u32, h: u32 },
    Title { id: u64, title: String },
    Destroyed { id: u64 },
    Frame { id: u64, picture: Picture },
    AppExited(String),
    Notice(String),
    Disconnected(String),
}

#[derive(Clone)]
pub struct Link {
    writer: Arc<Mutex<TcpStream>>,
}

impl Link {
    pub fn send(&self, m: &Message) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = write_message(&mut *w, m);
        }
    }
}

/// Connect, handshake, optionally launch `app`, and start the receive thread.
/// `wake` is called (from the receive thread) after events are queued.
pub fn connect(relay: &str, session: &str, token: &str, app: Option<&str>, wake: impl Fn() + Send + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    let stream = rm_relay::join(relay, session, rm_relay::Role::Client, token).map_err(|e| format!("relay: {e}"))?;
    let writer = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
    let sess = Session::handshake(stream).map_err(|e| format!("handshake: {e}"))?;
    let link = Link { writer };
    if !sess.capabilities.can_stream_apps() {
        eprintln!("warning: host reports it cannot stream apps: {:?}", sess.capabilities);
    }
    let (tx, rx) = channel();
    if let Some(app) = app {
        link.send(&Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() });
    }
    std::thread::spawn(move || recv_loop(sess, tx, wake));
    Ok((link, rx))
}

fn recv_loop(mut sess: Session<TcpStream>, tx: Sender<UiEvent>, wake: impl Fn()) {
    let mut decoders: HashMap<u64, H264Decoder> = HashMap::new();
    let emit = |e: UiEvent| {
        if tx.send(e).is_ok() {
            wake();
        }
    };
    loop {
        match sess.recv() {
            Ok(Some(Frame::Video(v))) => {
                let d = decoders.entry(v.window_id).or_insert_with(|| H264Decoder::new().expect("decoder"));
                if let Ok(Some(picture)) = d.decode(&v.data) {
                    emit(UiEvent::Frame { id: v.window_id, picture });
                }
            }
            Ok(Some(Frame::Msg(m))) => match m {
                Message::WindowCreated { window_id, application_id, title, bounds, parent_id, role } => emit(UiEvent::WindowCreated { id: window_id, app: application_id, title, x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h, parent: parent_id, role }),
                Message::Apps { apps } => emit(UiEvent::Apps(apps)),
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
                    decoders.remove(&window_id);
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
