//! Network side of the viewer: relay join, handshake, a receive thread that decodes video
//! into BGRA pictures, and a thread-safe sender. The UI only sees `UiEvent`s.

use rm_client::Session;
use rm_decode::{H264Decoder, Picture};
use rm_protocol::{write_message, Frame, Message};
use std::collections::HashMap;
use std::net::TcpStream;
/// The connection to the Mac, end-to-end encrypted.
type Secure = rm_protocol::secure::SecureStream<TcpStream>;
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

/// Encoded frames buffered per window before the viewer gives up on catching up, drops them
/// and asks the Mac for a fresh keyframe (Moonlight's "frame queue overflow -> IDR").
const DECODE_QUEUE: usize = 4;

/// A decoded picture: BGRA in memory (openh264 or Media Foundation's software decoder) or an
/// NV12 texture on the shared GPU device (hardware decoding).
#[derive(Debug)]
pub enum Pic {
    Cpu(Picture),
    #[cfg(windows)]
    Gpu(crate::gpu::GpuPic),
}

impl Pic {
    pub fn size(&self) -> (usize, usize) {
        match self {
            Pic::Cpu(p) => (p.width, p.height),
            #[cfg(windows)]
            Pic::Gpu(g) => (g.width as usize, g.height as usize),
        }
    }
}

/// Which decoder new windows get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DecoderKind {
    /// openh264 (portable; Main profile at most)
    Software = 0,
    /// Media Foundation, system memory (multithreaded; High profile)
    Platform = 1,
    /// Media Foundation on the GPU (DXVA), pictures stay in video memory
    Hardware = 2,
}

static SCALE_X100: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// This PC's display scale (set by the UI): the Mac sends pictures at that many pixels per point.
pub fn set_display_scale(s: f64) {
    SCALE_X100.store((s.clamp(1.0, 3.0) * 100.0).round() as u32, std::sync::atomic::Ordering::SeqCst);
}

static SCREEN_FIT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

static DIRECT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Connect straight to this address (an IP or host name, with :port) instead of looking for the
/// Mac on this network or going through a relay (None: the usual way). Set by the connect window.
pub fn set_direct(addr: Option<String>) {
    *DIRECT.lock().unwrap() = addr.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
}

pub fn direct() -> Option<String> {
    DIRECT.lock().unwrap().clone()
}

/// This PC's screen as "W,H,S" (see `Message::VideoDecoder::screen`), set by the UI.
pub fn set_screen_fit(s: Option<String>) {
    *SCREEN_FIT.lock().unwrap() = s;
}

static SCREEN_PX: std::sync::Mutex<(i32, i32)> = std::sync::Mutex::new((0, 0));

/// This PC's screen in pixels (set by the UI).
pub fn set_screen_px(px: (i32, i32)) {
    *SCREEN_PX.lock().unwrap() = px;
}

pub fn screen_px() -> (i32, i32) {
    *SCREEN_PX.lock().unwrap()
}

fn screen_fit() -> Option<String> {
    SCREEN_FIT.lock().unwrap().clone()
}

pub fn display_scale() -> f64 {
    SCALE_X100.load(std::sync::atomic::Ordering::SeqCst) as f64 / 100.0
}

/// Current decoder (0 software, 1 platform, 2 hardware) and a generation bumped on fallback.
static DECODER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static DECODER_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Choose the decoder before connecting (the UI knows whether it can show GPU pictures).
pub fn set_decoder(k: DecoderKind) {
    DECODER.store(k as u8, std::sync::atomic::Ordering::SeqCst);
}

pub fn decoder_kind() -> DecoderKind {
    match DECODER.load(std::sync::atomic::Ordering::SeqCst) {
        2 => DecoderKind::Hardware,
        1 => DecoderKind::Platform,
        _ => DecoderKind::Software,
    }
}

/// The GPU path does not work on this PC (decoding or showing its pictures failed): every
/// window's decoder is rebuilt on Windows' software decoder (still H.264 High), from a fresh
/// keyframe. Never below that: the stream may be High profile, which openh264 cannot take.
pub fn hardware_failed(why: &str) {
    if DECODER.compare_exchange(DecoderKind::Hardware as u8, DecoderKind::Platform as u8, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst).is_ok() {
        eprintln!("hardware video decoding off: {why}; using Windows' software decoder");
        DECODER_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

enum Dec {
    Open(H264Decoder),
    #[cfg(windows)]
    Mf(crate::mfdec::MfDecoder),
}

impl Dec {
    fn new() -> Option<Self> {
        #[cfg(windows)]
        {
            let gpu = match decoder_kind() {
                DecoderKind::Hardware => Some(crate::gpu::shared()),
                DecoderKind::Platform => Some(None),
                DecoderKind::Software => None,
            };
            if let Some(g) = gpu {
                match crate::mfdec::MfDecoder::new(g) {
                    Ok(d) => return Some(Dec::Mf(d)),
                    Err(e) => eprintln!("Media Foundation decoder unavailable ({e}); using openh264"),
                }
            }
        }
        H264Decoder::new().ok().map(Dec::Open)
    }

    fn decode(&mut self, v: &rm_protocol::VideoFrame) -> Result<Option<Pic>, String> {
        match self {
            Dec::Open(d) => d.decode(&v.data).map(|p| p.map(Pic::Cpu)),
            #[cfg(windows)]
            Dec::Mf(d) => d.decode(&v.data, (v.width as u32, v.height as u32)).map(|p| {
                p.map(|p| match p {
                    crate::mfdec::Decoded::Gpu(g) => Pic::Gpu(g),
                    crate::mfdec::Decoded::Cpu(c) => Pic::Cpu(c),
                })
            }),
        }
    }
}

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
    ClipboardImage(Vec<u8>),
    Resized { id: u64, x: i32, y: i32, w: u32, h: u32 },
    Title { id: u64, title: String },
    Destroyed { id: u64 },
    Frame { id: u64, picture: Pic, meta: FrameMeta },
    AppExited(String),
    /// The Mac started the app (the launch card moves on).
    Launched(String),
    /// The Mac's own Dock (Desktop Fusion): streamed as window `id` from this region of the
    /// Mac's screen (points), at `edge`; or why it cannot be shown.
    Dock { available: bool, id: u64, x: i32, y: i32, w: u32, h: u32, edge: String, reason: Option<String> },
    /// The title bar of exact window `id` (Mac points from its picture's top-left).
    Chrome { id: u64, chrome: crate::chrome::MacChrome },
    /// The shape of window `id`'s pictures of `width`x`height` (alpha per pixel; None: opaque).
    Mask { id: u64, width: u32, height: u32, alpha: Option<std::sync::Arc<Vec<u8>>> },
    Notice(String),
    Disconnected(String),
}

#[derive(Clone)]
pub struct Link {
    /// messages for the Mac, written by their own thread: a Mac that is slow to read (busy, or
    /// the link stalling) must never hold up the window that sends (Windows then marks the
    /// viewer "Not Responding")
    writer: Sender<Message>,
    /// messages queued and not written yet (pointer moves are dropped while many are)
    pending: Arc<std::sync::atomic::AtomicUsize>,
    /// UDP video path (FEC); None when it could not be started
    pub udp: Option<Arc<rm_client::udp::UdpVideo>>,
}

impl Link {
    pub fn send(&self, m: &Message) {
        // the Mac Desktop in full GameStream mode takes its input through Moonlight's stream
        if crate::gsdesktop::intercept(m) {
            return;
        }
        // input goes straight to the Mac when there is a direct path (reliable UDP, no detour
        // through the relay, no TCP head-of-line wait)
        let pointer = matches!(m, Message::MouseMove { .. });
        if matches!(m, Message::MouseMove { .. } | Message::MouseButton { .. } | Message::Scroll { .. } | Message::Key { .. } | Message::TextInput { .. }) {
            if let (Some(u), Ok(json)) = (&self.udp, serde_json::to_vec(m)) {
                if u.send_input(json, pointer) {
                    return;
                }
            }
        }
        use std::sync::atomic::Ordering::Relaxed;
        if pointer && self.pending.load(Relaxed) > 32 {
            return; // stale pointer positions behind a stalled link: the next one says where it is
        }
        self.pending.fetch_add(1, Relaxed);
        if self.writer.send(m.clone()).is_err() {
            self.pending.fetch_sub(1, Relaxed);
        }
    }

    /// The writer thread for `stream`.
    fn start(stream: Secure) -> Link {
        let (tx, rx) = channel::<Message>();
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = pending.clone();
        let _ = std::thread::Builder::new().name("rm-link-writer".into()).spawn(move || {
            let mut w = stream;
            for m in rx {
                p.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                if write_message(&mut w, &m).is_err() {
                    break;
                }
            }
        });
        Link { writer: tx, pending, udp: None }
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
        "The server refused this app (key mismatch). Use the current MacBridge build."
    } else if e.contains("pair timeout") {
        "The Mac did not answer in time. Try again."
    } else if e.contains("relay busy") {
        "The server is busy. Try again shortly."
    } else if e.contains("handshake") {
        "Connected, but the Mac did not complete the handshake. Update remotemac on the Mac."
    } else if e.contains("wrong password") {
        "Wrong password."
    } else if e.contains("older MacBridge") {
        "The Mac runs an older MacBridge without encryption. Update the Mac and this PC to the same version."
    } else if e.contains("not found on this network") || e.contains("did not answer at") || e.contains("is not a Mac address") || e.contains("could not be resolved") {
        return e.to_string();
    } else {
        return format!("Cannot reach the MacBridge server: {e}");
    };
    m.to_string()
}

/// Connect, handshake, optionally launch `app`, and start the receive thread.
/// `wake` is called (from the receive thread) after events are queued.
pub fn connect(relay: Option<&str>, session: &str, token: &str, app: Option<&str>, wake: impl Fn() + Send + Sync + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    connect_with(relay, session, token, app, true, wake)
}

/// [`connect`]; `wait: false` fails at once when the Mac is not waiting at the relay. A Mac on
/// this network (found by its ID) is joined straight; else `relay` is used.
pub fn connect_with(relay: Option<&str>, session: &str, token: &str, app: Option<&str>, wait: bool, wake: impl Fn() + Send + Sync + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    use crate::lifecycle::{set, Phase};
    set(Phase::Connecting);
    let r = connect_steps(relay, session, token, app, wait, wake);
    match &r {
        Ok(_) => set(Phase::Connected),
        Err(e) => set(Phase::Error(friendly_error(e))),
    };
    r
}

fn connect_steps(relay: Option<&str>, session: &str, token: &str, app: Option<&str>, wait: bool, wake: impl Fn() + Send + Sync + 'static) -> Result<(Link, Receiver<UiEvent>), String> {
    use crate::lifecycle::{set, Phase};
    let rt = rm_protocol::session::relay_token(session);
    let (stream, route) = match direct() {
        Some(addr) => rm_relay::lan::connect_direct(&addr, session, &rt)?,
        None => rm_relay::lan::connect(relay, session, &rt, wait)?,
    };
    eprintln!("connected {}", match &route {
        rm_relay::lan::Route::Lan(a) => format!("on this network ({a})"),
        rm_relay::lan::Route::Direct(a) => format!("straight to {a}"),
        rm_relay::lan::Route::Relay(r) => format!("through the relay {r}"),
    });
    *ROUTE.lock().unwrap() = match &route {
        rm_relay::lan::Route::Lan(_) => "This network".into(),
        rm_relay::lan::Route::Direct(a) => format!("Direct to {}", a.ip()),
        rm_relay::lan::Route::Relay(_) => "Through the relay".into(),
    };
    *SESSION.lock().unwrap() = session.to_string();
    // the password proved and the keys agreed end to end (the relay sees only ciphertext)
    set(Phase::Authenticating);
    let (stream, keys) = rm_protocol::secure::client_tcp(stream, session, token).map_err(|e| e.to_string())?;
    eprintln!("end-to-end encrypted (ChaCha20-Poly1305)");
    let lan = route.is_direct();
    let relay = route.udp_relay();
    let relay = relay.as_str();
    let writer = stream.try_clone().map_err(|e| e.to_string())?;
    // the raw socket, to cut a connection that went silent (see the heartbeat below)
    let raw = stream.get_ref().try_clone().map_err(|e| e.to_string())?;
    set(Phase::Negotiating);
    let sess = Session::handshake(stream).map_err(|e| format!("handshake: {e}"))?;
    set(Phase::EstablishingMedia);
    let (tx, rx) = channel();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
    let mut link = Link::start(writer);
    let video = Arc::new(Video { decoders: Mutex::new(HashMap::new()), link: Mutex::new(link.clone()), tx: tx.clone(), wake: wake.clone(), udp: Mutex::new(None) });
    // UDP video beside the TCP connection (RM_NO_UDP=1 keeps everything on TCP)
    if std::env::var_os("RM_NO_UDP").is_none() {
        let v = video.clone();
        let l = link.clone();
        let offer = move |secret, candidates| l.send(&Message::P2pOffer { secret, candidates });
        match rm_client::udp::start(relay, session, &rm_protocol::session::relay_token(session), Some(&keys), lan, move |o| v.on_udp(o), offer) {
            Ok(u) => {
                u.set_tunnel_handler(crate::gsdesktop::from_host_udp);
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
    let kind = decoder_kind();
    link.send(&Message::VideoDecoder { high_profile: kind != DecoderKind::Software, hardware: kind == DecoderKind::Hardware, scale: Some(display_scale()), screen: screen_fit() });
    // the user's settings (frame rate, bitrate, sharpness)
    let settings = crate::settings::Settings::load();
    link.send(&settings.message(display_scale(), screen_px()));
    *MAC_FEATURES.lock().unwrap() = sess.negotiated.features.clone();
    // the Mac's sound, unless muted (a Mac without the feature is never asked)
    let audio = crate::audio::audio();
    audio.reset();
    audio.set_supported(sess.negotiated.features.iter().any(|f| f == "audio"));
    if audio.supported() && settings.audio {
        link.send(&Message::AudioControl { enabled: true });
    }
    // Mac windows as the Mac draws them (before any window opens), when it can and it is wanted
    let exact = mac_has("exact") && settings.frame == 0 && !std::env::var_os("RM_FRAMED").is_some_and(|v| v != "0");
    EXACT.store(exact, std::sync::atomic::Ordering::Relaxed);
    if mac_has("exact") {
        link.send(&Message::WindowStyle { exact });
    }
    if let Some(app) = app {
        link.send(&Message::AppLaunch { application_id: app.into(), arguments: vec![], working_directory: None, environment: Default::default() });
    }
    *GS_VIDEO.lock().unwrap() = Some(video.clone());
    let l2 = link.clone();
    let alive = Heartbeat::start(link.clone(), raw);
    std::thread::spawn(move || recv_loop(sess, l2, video, tx, wake, alive));
    Ok((link, rx))
}

static GS_VIDEO: Mutex<Option<Arc<Video>>> = Mutex::new(None);

static EXACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ROUTE: Mutex<String> = Mutex::new(String::new());
static SESSION: Mutex<String> = Mutex::new(String::new());

/// How the Mac is reached, in words ("This network", "Through the relay"…); empty before.
pub fn route_label() -> String {
    ROUTE.lock().unwrap().clone()
}

/// The Mac as people know it: "Mac 123 456 789" from its ID (the session), else "Your Mac".
pub fn mac_label() -> String {
    let s = SESSION.lock().unwrap().clone();
    match s.strip_prefix("rm-").filter(|id| id.len() == 9 && id.chars().all(|c| c.is_ascii_digit())) {
        Some(id) => format!("Mac {} {} {}", &id[..3], &id[3..6], &id[6..]),
        None => "Your Mac".into(),
    }
}

/// Mac windows are shown as the Mac draws them (their own title bar and buttons, the Mac's menu
/// bar at the top of the screen) rather than in MacBridge's frame.
pub fn exact_windows() -> bool {
    EXACT.load(std::sync::atomic::Ordering::Relaxed)
}

/// What the connected Mac supports beyond the basics ("audio", "open_file"), from its hello.
static MAC_FEATURES: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn mac_has(feature: &str) -> bool {
    MAC_FEATURES.lock().unwrap().iter().any(|f| f == feature)
}

/// Start the Mac Desktop in full GameStream mode for window `id` (see gsdesktop.rs): the tunnel
/// rides this connection, decoded pictures take the usual path.
pub fn start_gamestream_desktop(link: &Link, id: u64, key: [u8; 16], points: (u32, u32), pixels: (u16, u16)) -> Result<(), String> {
    let video = GS_VIDEO.lock().unwrap().clone().ok_or("not connected")?;
    let (l, u) = (link.clone(), link.udp.clone());
    let to_host = move |m: rm_gamestream::tunnel::ToHost| {
        use rm_gamestream::tunnel::ToHost;
        match m {
            ToHost::Udp { kind, data } => {
                if let Some(u) = &u {
                    u.send_tunnel(kind, data);
                }
            }
            ToHost::TcpOpen { id } => l.send(&Message::GsTunnel { id, op: "open".into(), data_base64: String::new() }),
            ToHost::TcpData { id, data } => l.send(&Message::GsTunnel { id, op: "data".into(), data_base64: rm_protocol::base64_encode(data) }),
            ToHost::TcpClose { id } => l.send(&Message::GsTunnel { id, op: "close".into(), data_base64: String::new() }),
        }
    };
    // once GameStream's pictures arrive, the Mac stops sending the usual ones (until then the
    // desktop shows at once, as an app window does)
    let ready = link.clone();
    crate::gsdesktop::start(id, key, points, pixels, to_host, move |f| video.push(f, true), move || ready.send(&Message::GsTunnel { id: 0, op: "ready".into(), data_base64: String::new() }))
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
            rm_protocol::udp::Out::Frame(v) if !crate::gsdesktop::owns(v.window_id) => self.push(v, true),
            rm_protocol::udp::Out::Frame(_) => {}
            // lost even with FEC: the decoder needs a fresh keyframe
            rm_protocol::udp::Out::Lost(id) => self.link().send(&Message::RequestKeyframe { window_id: id }),
            rm_protocol::udp::Out::Audio(a) => crate::audio::audio().push(&a),
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
            use std::sync::atomic::Ordering;
            let mut gen = DECODER_GEN.load(Ordering::SeqCst);
            let Some(mut d) = Dec::new() else { return };
            let mut last_ask = std::time::Instant::now() - std::time::Duration::from_secs(1);
            // watchdog: frames fed / pictures out / errors in a row since this decoder was made
            let (mut fed, mut produced, mut errors, mut waiting_key) = (0u32, 0u32, 0u32, false);
            for (v, mut meta) in frx {
                let now_gen = DECODER_GEN.load(Ordering::SeqCst);
                if now_gen != gen {
                    // fell back from the GPU: a new decoder, starting at a keyframe
                    gen = now_gen;
                    match Dec::new() {
                        Some(n) => d = n,
                        None => return,
                    }
                    (fed, produced, errors, waiting_key) = (0, 0, 0, true);
                    link.send(&Message::RequestKeyframe { window_id: id });
                }
                if waiting_key && !v.keyframe {
                    continue;
                }
                waiting_key = false;
                fed += 1;
                let t = std::time::Instant::now();
                match d.decode(&v) {
                    Ok(Some(picture)) => {
                        produced += 1;
                        errors = 0;
                        meta.decode_us = t.elapsed().as_micros() as u32;
                        if tx.send(UiEvent::Frame { id, picture, meta }).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        errors += 1;
                        if errors <= 3 {
                            eprintln!("window {id}: decode error ({:?}): {e}", decoder_kind());
                        }
                        // a broken reference chain: ask for a keyframe (at most a few per second)
                        if last_ask.elapsed().as_millis() > 300 {
                            link.send(&Message::RequestKeyframe { window_id: id });
                            last_ask = std::time::Instant::now();
                        }
                    }
                }
                if decoder_kind() == DecoderKind::Hardware && produced == 0 && (fed >= 30 || errors >= 8) {
                    hardware_failed(&format!("window {id}: {fed} frames in, no picture out"));
                }
            }
        })
        .expect("decode thread");
    DecodeWorker { tx: ftx, resync: false }
}

/// Keeps the connection honest: a ping to the Mac every 2 s (the Mac ends a session it has not
/// heard from in a while, and waits for the next one), and a connection that has carried
/// nothing from the Mac for 10 s is cut: a network gone without a word (Wi-Fi off, cable out)
/// otherwise looks like a quiet Mac forever.
pub struct Heartbeat {
    heard: std::sync::atomic::AtomicU64,
    stop: std::sync::atomic::AtomicBool,
    epoch: std::time::Instant,
}

/// Silence from the Mac that ends the connection.
const SILENT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

impl Heartbeat {
    fn start(link: Link, raw: TcpStream) -> Arc<Heartbeat> {
        let hb = Arc::new(Heartbeat { heard: Default::default(), stop: Default::default(), epoch: std::time::Instant::now() });
        let h = hb.clone();
        let _ = std::thread::Builder::new().name("rm-heartbeat".into()).spawn(move || {
            let mut nonce = 0u64;
            while !h.stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_secs(2));
                nonce += 1;
                link.send(&Message::Ping { nonce });
                let silent = h.epoch.elapsed().saturating_sub(std::time::Duration::from_millis(h.heard.load(std::sync::atomic::Ordering::Relaxed)));
                if silent > SILENT_LIMIT {
                    eprintln!("nothing from the Mac for {} s: the connection is gone", silent.as_secs());
                    let _ = raw.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        });
        hb
    }

    fn heard(&self) {
        self.heard.store(self.epoch.elapsed().as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

fn recv_loop(mut sess: Session<Secure>, _link: Link, video: Arc<Video>, tx: Sender<UiEvent>, wake: Arc<dyn Fn() + Send + Sync>, alive: Arc<Heartbeat>) {
    let emit = |e: UiEvent| {
        if tx.send(e).is_ok() {
            wake();
        }
    };
    struct StopOnExit(Arc<Heartbeat>);
    impl Drop for StopOnExit {
        fn drop(&mut self) {
            self.0.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let _stop = StopOnExit(alive.clone());
    loop {
        let r = sess.recv();
        if matches!(r, Ok(Some(_))) {
            alive.heard();
        }
        match r {
            // the Mac Desktop's picture comes the usual way until GameStream has it
            Ok(Some(Frame::Video(v))) if !crate::gsdesktop::owns(v.window_id) => video.push(v, false),
            Ok(Some(Frame::Video(_))) => {}
            Ok(Some(Frame::Audio(a))) => crate::audio::audio().push(&a),
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
                Message::ClipboardImage { bmp_base64, .. } => {
                    if let Ok(b) = rm_protocol::base64_decode(&bmp_base64) {
                        emit(UiEvent::ClipboardImage(b));
                    }
                }
                Message::WindowMoved { window_id, bounds } => emit(UiEvent::Resized { id: window_id, x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h }),
                Message::WindowTitleChanged { window_id, title } => emit(UiEvent::Title { id: window_id, title }),
                Message::WindowDestroyed { window_id } => {
                    video.forget(window_id);
                    emit(UiEvent::Destroyed { id: window_id });
                }
                Message::AppExited { application_id, .. } => emit(UiEvent::AppExited(application_id)),
                Message::AppLaunched { application_id, .. } => emit(UiEvent::Launched(application_id)),
                Message::AudioStatus { state, reason } => crate::audio::audio().set_mac_status(&state, reason.as_deref()),
                Message::DockStatus { available, window_id, bounds, edge, reason } => emit(UiEvent::Dock { available, id: window_id, x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h, edge, reason }),
                Message::WindowChrome { window_id, title_height, close, minimize, zoom, controls } => {
                    let r = |r: rm_protocol::Rect| (r.x, r.y, r.w, r.h);
                    emit(UiEvent::Chrome { id: window_id, chrome: crate::chrome::MacChrome { title_height, lights: [close.map(r), minimize.map(r), zoom.map(r)], controls: controls.into_iter().map(r).collect() } })
                }
                Message::WindowMask { window_id, width, height, rle } => {
                    let alpha = rm_protocol::mask::from_message(width, height, &rle).map(std::sync::Arc::new);
                    emit(UiEvent::Mask { id: window_id, width, height, alpha })
                }
                Message::WallpaperStatus { applied, reason } => eprintln!("wallpaper on the Mac: {}", if applied { "this PC's".to_string() } else { reason.unwrap_or_else(|| "the Mac's own".into()) }),
                Message::Error { code, message } => emit(UiEvent::Notice(format!("{code}: {message}"))),
                Message::CapabilityUnavailable { capability, reason } => emit(UiEvent::Notice(format!("{capability} unavailable: {reason}"))),
                Message::P2pOffer { secret, candidates } => {
                    if let Some(u) = video.udp.lock().unwrap().as_ref() {
                        u.peer_offer(&secret, &candidates);
                    }
                }
                Message::GsTunnel { id, op, data_base64 } => {
                    let data = rm_protocol::base64_decode(&data_base64).unwrap_or_default();
                    crate::gsdesktop::from_host_tcp(id, &op, &data);
                }
                _ => {}
            },
            Ok(None) => return emit(UiEvent::Disconnected("connection closed".into())),
            Err(e) => return emit(UiEvent::Disconnected(e.to_string())),
        }
    }
}

/// [`upload_file`] under another name on the Mac.
pub fn upload_file_as(link: &Link, transfer_id: u64, path: &std::path::Path, name: &str) -> Result<u64, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    if size > rm_protocol::MAX_UPLOAD {
        return Err(format!("{} is larger than the {} byte limit", path.display(), rm_protocol::MAX_UPLOAD));
    }
    link.send(&Message::FileUploadBegin { transfer_id, name: name.into(), size });
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

/// Send a local file to the agent in protocol-sized chunks. Runs on the caller's thread
/// (the UI spawns one); the agent answers with `FileUploaded` / `FileUploadFailed`.
pub fn upload_file(link: &Link, transfer_id: u64, path: &std::path::Path) -> Result<u64, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "upload".into());
    upload_file_as(link, transfer_id, path, &name)
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

        let (link, rx) = connect(Some(&addr), "v-1", tok, Some("testapp"), || {}).unwrap();
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
                    assert_eq!(picture.size(), (480, 352));
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

    /// The Mac's sound reaches the jitter buffer (over TCP first, then UDP) and plays.
    /// (The buffer is the process's one sound output: other tests' scripted Macs send their
    /// tones into it too, so only what holds for any mix of them is checked here; the jitter
    /// buffer's own tests check order, loss and timing.)
    #[test]
    fn sound_reaches_the_jitter_buffer() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let (a, tok) = (addr.clone(), "viewer-audio-token-0123456789");
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "v-audio", tok); });
        std::thread::sleep(Duration::from_millis(150));
        let (_link, _rx) = connect(Some(&addr), "v-audio", tok, None, || {}).unwrap();
        let sound = crate::audio::audio();
        assert!(sound.supported(), "the fake Mac offers sound");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut out = vec![0i16; 480];
        let mut heard = 0;
        while std::time::Instant::now() < deadline && heard < 50 {
            std::thread::sleep(Duration::from_millis(5));
            sound.pull(&mut out);
            heard += out.iter().any(|&s| s.abs() > 1000) as usize;
        }
        let st = sound.stats();
        assert!(heard >= 50 && st.received >= 40, "tone played {heard} times: {st:?}");
    }

    /// A file uploaded from this PC opens on the Mac with the app asked for; a path that was not
    /// uploaded is refused.
    #[test]
    fn dropped_file_opens_on_the_mac() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let (a, tok) = (addr.clone(), "viewer-open-token-0123456789");
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "v-open", tok); });
        std::thread::sleep(Duration::from_millis(150));
        let (link, rx) = connect(Some(&addr), "v-open", tok, None, || {}).unwrap();
        assert!(mac_has("open_file"));
        link.send(&Message::OpenFile { path: "/etc/passwd".into(), application_id: None });
        let file = std::env::temp_dir().join(format!("rm-drop-{}.txt", std::process::id()));
        std::fs::write(&file, b"hello mac").unwrap();
        upload_file(&link, 9, &file).unwrap();
        let (mut refused, mut opened) = (false, None);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline && !(refused && opened.is_some()) {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(UiEvent::Notice(n)) if n.starts_with("open_rejected") => refused = true,
                Ok(UiEvent::Uploaded { transfer_id: 9, remote_path }) => link.send(&Message::OpenFile { path: remote_path, application_id: Some("testapp".into()) }),
                Ok(UiEvent::Title { title, .. }) if title.contains("[opened rm-drop") => opened = Some(title),
                _ => {}
            }
        }
        let _ = std::fs::remove_file(&file);
        assert!(refused, "a file that was not uploaded is refused");
        let t = opened.expect("the uploaded file opens");
        assert!(t.starts_with("RM Test App") && t.ends_with("9 bytes]"), "{t}");
    }

    /// Video moves to UDP once the path works, and FEC carries it through a lossy relay.
    fn udp_run(loss: Option<f64>, session: &str) -> (usize, usize, rm_client::udp::LinkStats) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, rm_relay::Config { udp_loss: loss, ..Default::default() }));
        let (a, tok, s2) = (addr.clone(), "viewer-udp-token-0123456789", session.to_string());
        // a lossy relay must stay in the path: no direct path here
        let p2p = loss.is_none();
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay_with(&a, &s2, tok, p2p); });
        std::thread::sleep(Duration::from_millis(150));
        let (link, rx) = connect(Some(&addr), session, tok, Some("testapp"), || {}).unwrap();
        let (mut tcp, mut udp) = (0, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(12);
        while std::time::Instant::now() < deadline && udp < 60 {
            if let Ok(UiEvent::Frame { picture, meta, .. }) = rx.recv_timeout(Duration::from_millis(300)) {
                assert_eq!(picture.size(), (480, 352));
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
        // a clean link must not read as lossy (that would push the bitrate down for nothing)
        assert!(stats.loss < 0.02 && stats.lost == 0, "{stats:?}");
    }

    #[test]
    fn goes_direct_and_input_takes_it() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || rm_relay::serve(l, Default::default()));
        let (a, tok) = (addr.clone(), "viewer-p2p-token-0123456789");
        std::thread::spawn(move || { let _ = rm_fakeagent::serve_via_relay(&a, "v-p2p", tok); });
        std::thread::sleep(Duration::from_millis(150));
        let (link, rx) = connect(Some(&addr), "v-p2p", tok, Some("testapp"), || {}).unwrap();
        let u = link.udp.clone().unwrap();
        let (mut win, mut typed, mut titled, mut frames_after) = (None, false, false, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline && !(titled && frames_after >= 20) {
            let direct = u.stats.lock().unwrap().direct;
            if let (Some(id), Some(_), false) = (win, direct, typed) {
                // the direct path is up: input goes over it, not over TCP
                link.send(&Message::TextInput { window_id: id, text: "direct".into() });
                typed = true;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(UiEvent::WindowCreated { id, .. }) => win = Some(id),
                Ok(UiEvent::Title { .. }) if typed => titled = true, // the window counts what was typed
                Ok(UiEvent::Frame { meta, .. }) if typed && meta.via_udp => frames_after += 1,
                _ => {}
            }
        }
        let st = u.stats.lock().unwrap().clone();
        let direct = st.direct.expect("a direct path between two sockets on one host");
        assert_ne!(direct.port(), addr.parse::<std::net::SocketAddr>().unwrap().port(), "{st:?}");
        assert!(typed && titled, "text sent over the direct path must arrive (typed={typed} titled={titled})");
        assert!(frames_after >= 20, "video keeps coming over the direct path: {frames_after} {st:?}");
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
        let (link, rx) = connect(Some(&addr), "v-2", tok, Some("testapp"), || {}).unwrap();
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
