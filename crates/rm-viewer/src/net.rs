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
    WindowCreated { id: u64, title: String, w: u32, h: u32 },
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
                Message::WindowCreated { window_id, title, bounds, .. } => emit(UiEvent::WindowCreated { id: window_id, title, w: bounds.w, h: bounds.h }),
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
}
