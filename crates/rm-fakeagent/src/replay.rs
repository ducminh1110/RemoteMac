//! Replays an `.rmrec` recording of a real Mac session as if it were the Mac agent: the client
//! gets the recorded capabilities and app list, and launching an app plays back what that app's
//! windows showed (real H.264 video, titles, menu bar, icon) with the recorded timing. Windows
//! stay open after the playback with their last picture, until closed or the app is terminated.
//! A recorded `dock` segment (the Mac's Dock, as Desktop Fusion streams it) is played when the
//! client asks for the Dock.

use rm_protocol::recording::{self, Record};
use rm_protocol::*;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Gaps longer than this in the recording are shortened (an idle app sends nothing).
const MAX_GAP: Duration = Duration::from_secs(2);
/// The recorded Mac Dock (see rm-client's recorder).
const DOCK: &str = "dock";

type Writer<W> = Arc<Mutex<W>>;

fn decode_msg(frame: &[u8]) -> Option<Message> {
    match read_frame(&mut &frame[..]) {
        Ok(Some(Frame::Msg(m))) => Some(m),
        _ => None,
    }
}

pub struct Recording {
    session: Vec<Message>,
    apps: HashMap<String, Vec<Record>>,
}

impl Recording {
    pub fn load(path: &str) -> Result<Self, String> {
        let mut f = std::io::BufReader::new(std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?);
        Ok(Self::from_records(recording::read_all(&mut f).map_err(|e| e.to_string())?))
    }

    pub fn from_records(recs: Vec<Record>) -> Self {
        let mut session = vec![];
        let mut apps: HashMap<String, Vec<Record>> = HashMap::new();
        for r in recs {
            if r.app == recording::SESSION {
                session.extend(decode_msg(&r.frame));
            } else {
                apps.entry(r.app.clone()).or_default().push(r);
            }
        }
        Self { session, apps }
    }

    /// Last message of a kind recorded for an app (menu bar, icon).
    fn last(&self, app: &str, pick: impl Fn(&Message) -> bool) -> Option<Message> {
        self.apps.get(app)?.iter().rev().filter_map(|r| decode_msg(&r.frame)).find(|m| pick(m))
    }
}

fn send<W: Write>(w: &Writer<W>, m: &Message) -> Result<(), ProtocolError> {
    let b = encode(m)?;
    w.lock().unwrap().write_all(&b).map_err(Into::into)
}

/// Serve one client from the recording.
pub fn serve_replay<S: Read + Write + Send + 'static>(mut reader: S, writer: S, rec: Arc<Recording>) -> Result<(), ProtocolError> {
    let w: Writer<S> = Arc::new(Mutex::new(writer));
    match read_message(&mut reader)? {
        Some(Message::ClientHello(_)) => {}
        _ => return Ok(()),
    }
    let features: &[&str] = if rec.apps.contains_key(DOCK) { &["control", "video", "fusion"] } else { &["control", "video"] };
    send(&w, &Message::ServerHello(Hello::ours("rm-replay (recorded Mac session)", &["h264"], features)))?;
    let caps = rec.session.iter().find_map(|m| if let Message::CapabilityReport(c) = m { Some(c.clone()) } else { None });
    send(&w, &Message::CapabilityReport(caps.unwrap_or_else(|| CapabilityReport::unknown("replay"))))?;
    // per app: stop flag of its playback, windows it opened
    let playing: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Default::default();
    let open: Arc<Mutex<HashMap<u64, String>>> = Default::default();
    let closed: Arc<Mutex<HashSet<u64>>> = Default::default();
    while let Some(msg) = read_message(&mut reader)? {
        match msg {
            Message::ListApps => {
                let recorded = rec.session.iter().find_map(|m| if let Message::Apps { apps } = m { Some(apps.clone()) } else { None }).unwrap_or_default();
                // only apps that were recorded can be opened
                let apps = recorded.into_iter().map(|mut a| { a.available = a.available && rec.apps.contains_key(&a.id); a }).collect();
                send(&w, &Message::Apps { apps })?;
            }
            Message::AppLaunch { application_id, .. } => {
                let Some(records) = rec.apps.get(&application_id).cloned() else {
                    send(&w, &Message::Error { code: "launch_rejected".into(), message: format!("'{application_id}' was not recorded") })?;
                    continue;
                };
                if playing.lock().unwrap().contains_key(&application_id) {
                    continue; // already open
                }
                let stop = Arc::new(AtomicBool::new(false));
                playing.lock().unwrap().insert(application_id.clone(), stop.clone());
                let (w, open, closed, app) = (w.clone(), open.clone(), closed.clone(), application_id.clone());
                std::thread::spawn(move || play(&w, &app, &records, &stop, &open, &closed));
            }
            Message::GetMenuBar { application_id } => {
                if let Some(m) = rec.last(&application_id, |m| matches!(m, Message::MenuBar { .. })) {
                    send(&w, &m)?;
                }
            }
            Message::GetAppIcon { application_id } => {
                if let Some(m) = rec.last(&application_id, |m| matches!(m, Message::AppIcon { .. })) {
                    send(&w, &m)?;
                }
            }
            Message::WindowClose { window_id } => {
                if open.lock().unwrap().remove(&window_id).is_some() {
                    closed.lock().unwrap().insert(window_id);
                    send(&w, &Message::WindowDestroyed { window_id })?;
                }
            }
            Message::AppTerminate { application_id } => {
                if let Some(stop) = playing.lock().unwrap().remove(&application_id) {
                    stop.store(true, Ordering::SeqCst);
                }
                let ids: Vec<u64> = open.lock().unwrap().iter().filter(|(_, a)| **a == application_id).map(|(k, _)| *k).collect();
                for id in ids {
                    open.lock().unwrap().remove(&id);
                    closed.lock().unwrap().insert(id);
                    send(&w, &Message::WindowDestroyed { window_id: id })?;
                }
                send(&w, &Message::AppExited { application_id, code: Some(0) })?;
            }
            Message::DockStream { enabled } => {
                let Some(records) = rec.apps.get(DOCK).cloned() else {
                    send(&w, &Message::DockStatus { available: false, window_id: 0, bounds: Rect { x: 0, y: 0, w: 0, h: 0 }, edge: String::new(), reason: Some("the Dock was not recorded".into()) })?;
                    continue;
                };
                let running = playing.lock().unwrap().remove(DOCK);
                if let Some(stop) = running {
                    stop.store(true, Ordering::SeqCst);
                }
                if enabled {
                    let stop = Arc::new(AtomicBool::new(false));
                    playing.lock().unwrap().insert(DOCK.to_string(), stop.clone());
                    let (w, open, closed) = (w.clone(), open.clone(), closed.clone());
                    std::thread::spawn(move || play(&w, DOCK, &records, &stop, &open, &closed));
                }
            }
            Message::Ping { nonce } => send(&w, &Message::Pong { nonce })?,
            _ => {} // input, focus, resize: the recording cannot react
        }
    }
    for stop in playing.lock().unwrap().values() {
        stop.store(true, Ordering::SeqCst);
    }
    Ok(())
}

fn play<W: Write>(w: &Writer<W>, app: &str, records: &[Record], stop: &AtomicBool, open: &Mutex<HashMap<u64, String>>, closed: &Mutex<HashSet<u64>>) {
    let start = Instant::now();
    let mut skipped = Duration::ZERO;
    let mut last = 0u32;
    for r in records {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let gap = Duration::from_millis(r.t_ms.saturating_sub(last) as u64);
        last = r.t_ms;
        if gap > MAX_GAP {
            skipped += gap - MAX_GAP;
        }
        let due = Duration::from_millis(r.t_ms as u64).saturating_sub(skipped);
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        // what this record is about
        let window = match read_frame(&mut &r.frame[..]) {
            Ok(Some(Frame::Video(v))) => Some(v.window_id),
            Ok(Some(Frame::Msg(Message::WindowCreated { window_id, .. }))) => {
                open.lock().unwrap().insert(window_id, app.to_string());
                None
            }
            Ok(Some(Frame::Msg(Message::WindowDestroyed { window_id } | Message::WindowMoved { window_id, .. } | Message::WindowTitleChanged { window_id, .. }))) => Some(window_id),
            _ => None,
        };
        if window.is_some_and(|id| closed.lock().unwrap().contains(&id)) {
            continue; // the client closed that window
        }
        if let Ok(Some(Frame::Msg(Message::WindowDestroyed { window_id }))) = read_frame(&mut &r.frame[..]) {
            open.lock().unwrap().remove(&window_id);
        }
        if w.lock().unwrap().write_all(&r.frame).is_err() {
            return;
        }
    }
}

/// Bind to a relay as the agent and replay the recording to one client.
pub fn replay_via_relay(relay: &str, session: &str, token: &str, rec: Recording) -> Result<(), String> {
    let (s, _) = crate::secure_join(relay, session, token)?;
    let w = s.try_clone().map_err(|e| e.to_string())?;
    serve_replay(s, w, Arc::new(rec)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn rec(app: &str, t_ms: u32, m: &Message) -> Record {
        Record { app: app.into(), t_ms, frame: encode(m).unwrap() }
    }

    #[test]
    fn replays_a_recorded_app_to_a_client() {
        let menus = vec![MenuNode { title: "Xcode".into(), enabled: true, ..Default::default() }];
        let video = VideoFrame { window_id: 9, pts_us: 0, keyframe: true, codec: CODEC_H264, width: 4, height: 4, data: vec![0, 0, 0, 1, 0x65] };
        let records = vec![
            rec(recording::SESSION, 0, &Message::Apps { apps: vec![AppInfo { id: "xcode".into(), name: "Xcode".into(), available: true, version: None }, AppInfo { id: "safari".into(), name: "Safari".into(), available: true, version: None }] }),
            rec("xcode", 10, &Message::AppLaunched { application_id: "xcode".into(), pid: 1 }),
            rec("xcode", 20, &Message::WindowCreated { window_id: 9, application_id: "xcode".into(), title: "Welcome to Xcode".into(), bounds: Rect { x: 0, y: 0, w: 4, h: 4 }, parent_id: None, role: WindowRole::Window }),
            Record { app: "xcode".into(), t_ms: 30, frame: encode_video(&video).unwrap() },
            rec("xcode", 5000, &Message::MenuBar { application_id: "xcode".into(), menus: menus.clone() }),
        ];
        let rec = Arc::new(Recording::from_records(records));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let a = l.accept().unwrap().0;
        let a2 = a.try_clone().unwrap();
        std::thread::spawn(move || serve_replay(a, a2, rec));
        let mut c = b;
        write_message(&mut c, &Message::ClientHello(Hello::ours("t", &["h264"], &["control"]))).unwrap();
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::ServerHello(_))));
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::CapabilityReport(_))));
        write_message(&mut c, &Message::ListApps).unwrap();
        let Some(Message::Apps { apps }) = read_message(&mut c).unwrap() else { panic!() };
        assert_eq!(apps.iter().map(|a| (a.id.as_str(), a.available)).collect::<Vec<_>>(), vec![("xcode", true), ("safari", false)]);
        let t = Instant::now();
        write_message(&mut c, &Message::AppLaunch { application_id: "xcode".into(), arguments: vec![], working_directory: None, environment: Default::default() }).unwrap();
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Msg(Message::AppLaunched { .. }))));
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Msg(Message::WindowCreated { window_id: 9, .. }))));
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Video(v)) if v == video));
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Msg(Message::MenuBar { .. }))));
        assert!(t.elapsed() < Duration::from_secs(4), "long idle gaps are shortened: {:?}", t.elapsed());
        write_message(&mut c, &Message::GetMenuBar { application_id: "xcode".into() }).unwrap();
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::MenuBar { menus: m, .. }) if m == menus));
        write_message(&mut c, &Message::AppTerminate { application_id: "xcode".into() }).unwrap();
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::WindowDestroyed { window_id: 9 })));
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::AppExited { .. })));
    }

    #[test]
    fn replays_the_recorded_dock_when_asked_for() {
        let id = 0x7FFF_0002u64;
        let status = Message::DockStatus { available: true, window_id: id, bounds: Rect { x: 25, y: 700, w: 974, h: 64 }, edge: "bottom".into(), reason: None };
        let video = VideoFrame { window_id: id, pts_us: 0, keyframe: true, codec: CODEC_H264, width: 974, height: 64, data: vec![0, 0, 0, 1, 0x65] };
        let records = vec![rec(recording::SESSION, 0, &Message::Apps { apps: vec![] }), rec(DOCK, 5, &status), Record { app: DOCK.into(), t_ms: 9, frame: encode_video(&video).unwrap() }];
        let rec = Arc::new(Recording::from_records(records));
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let b = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let a = l.accept().unwrap().0;
        let a2 = a.try_clone().unwrap();
        std::thread::spawn(move || serve_replay(a, a2, rec));
        let mut c = b;
        write_message(&mut c, &Message::ClientHello(Hello::ours("t", &["h264"], &["control", "fusion"]))).unwrap();
        let Some(Message::ServerHello(h)) = read_message(&mut c).unwrap() else { panic!() };
        assert!(h.features.iter().any(|f| f == "fusion"), "a recording with the Dock offers Fusion: {:?}", h.features);
        assert!(matches!(read_message(&mut c).unwrap(), Some(Message::CapabilityReport(_))));
        write_message(&mut c, &Message::DockStream { enabled: true }).unwrap();
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Msg(m)) if m == status));
        assert!(matches!(read_frame(&mut c).unwrap(), Some(Frame::Video(v)) if v == video));
    }
}
