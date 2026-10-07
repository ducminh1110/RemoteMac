//! Moonlight's client core (moonlight-common-c) driven from Rust: connect to a GameStream
//! host (here: RemoteMac's, through [`crate::tunnel::ClientTunnel`]), get Annex-B frames,
//! send input. moonlight-common-c keeps one connection per process.

use crate::Input;
use moonlight_sys as ml;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct Params {
    pub rtsp_port: u16,
    pub key: [u8; 16],
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub packet_size: u32,
    /// Moonlight's "remote" profile (more FEC, smaller packets)
    pub remote: bool,
}

/// (Annex-B, IDR)
type Sink = Box<dyn Fn(Vec<u8>, bool) + Send>;
static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static IDRS: AtomicU64 = AtomicU64::new(0);
/// moonlight-common-c is not reentrant: one connect/stop at a time
static LOCK: Mutex<()> = Mutex::new(());

pub fn frames() -> (u64, u64) {
    (FRAMES.load(Ordering::Relaxed), IDRS.load(Ordering::Relaxed))
}

unsafe extern "C" fn dr_setup(_: i32, _: i32, _: i32, _: i32, _: *mut std::ffi::c_void, _: i32) -> i32 {
    0
}

unsafe extern "C" fn dr_submit(du: ml::PDECODE_UNIT) -> i32 {
    let du = &*du;
    let mut data = Vec::with_capacity(du.fullLength.max(0) as usize);
    let mut e = du.bufferList;
    while !e.is_null() {
        let x = &*e;
        data.extend_from_slice(std::slice::from_raw_parts(x.data as *const u8, x.length.max(0) as usize));
        e = x.next;
    }
    let idr = du.frameType == ml::FRAME_TYPE_IDR as i32;
    FRAMES.fetch_add(1, Ordering::Relaxed);
    IDRS.fetch_add(idr as u64, Ordering::Relaxed);
    if let Some(s) = SINK.lock().unwrap().as_ref() {
        s(data, idr);
    }
    ml::DR_OK as i32
}

unsafe extern "C" fn ar_init(_: i32, _: ml::POPUS_MULTISTREAM_CONFIGURATION, _: *mut std::ffi::c_void, _: i32) -> i32 {
    0
}

unsafe extern "C" fn cl_terminated(code: i32) {
    eprintln!("GameStream connection ended (code {code})");
}

unsafe extern "C" fn cl_stage_failed(stage: i32, code: i32) {
    eprintln!("GameStream stage {stage} failed ({code})");
}

/// Connect (blocks through the handshake). Frames go to `sink` until [`stop`].
pub fn connect(p: Params, sink: impl Fn(Vec<u8>, bool) + Send + 'static) -> Result<(), i32> {
    let _g = LOCK.lock().unwrap();
    *SINK.lock().unwrap() = Some(Box::new(sink));
    unsafe {
        let mut si: ml::_SERVER_INFORMATION = std::mem::zeroed();
        ml::LiInitializeServerInformation(&mut si);
        let addr = CString::new("127.0.0.1").unwrap();
        // what Sunshine reports: GameStream 7.1.431 (encrypted control V2, multi-FEC)
        let app = CString::new("7.1.431.-1").unwrap();
        let gfe = CString::new("3.23.0.74").unwrap();
        let url = CString::new(format!("rtsp://127.0.0.1:{}", p.rtsp_port)).unwrap();
        si.address = addr.as_ptr();
        si.serverInfoAppVersion = app.as_ptr();
        si.serverInfoGfeVersion = gfe.as_ptr();
        si.rtspSessionUrl = url.as_ptr();
        si.serverCodecModeSupport = ml::SCM_H264 as i32;
        let mut sc: ml::_STREAM_CONFIGURATION = std::mem::zeroed();
        ml::LiInitializeStreamConfiguration(&mut sc);
        sc.width = p.width as i32;
        sc.height = p.height as i32;
        sc.fps = p.fps as i32;
        sc.bitrate = p.bitrate_kbps as i32;
        sc.packetSize = p.packet_size as i32;
        sc.streamingRemotely = if p.remote { ml::STREAM_CFG_REMOTE } else { ml::STREAM_CFG_LOCAL } as i32;
        sc.audioConfiguration = (0x3 << 16) | (2 << 8) | 0xCA; // MAKE_AUDIO_CONFIGURATION(2, 0x3): stereo
        sc.supportedVideoFormats = ml::VIDEO_FORMAT_H264 as i32;
        for (d, s) in sc.remoteInputAesKey.iter_mut().zip(p.key) {
            *d = s as _;
        }
        let mut dr: ml::_DECODER_RENDERER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeVideoCallbacks(&mut dr);
        dr.setup = Some(dr_setup);
        dr.submitDecodeUnit = Some(dr_submit);
        let mut ar: ml::_AUDIO_RENDERER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeAudioCallbacks(&mut ar);
        ar.init = Some(ar_init);
        let mut cl: ml::_CONNECTION_LISTENER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeConnectionCallbacks(&mut cl);
        cl.connectionTerminated = Some(cl_terminated);
        cl.stageFailed = Some(cl_stage_failed);
        let r = ml::LiStartConnection(&mut si, &mut sc, &mut cl, &mut dr, &mut ar, std::ptr::null_mut(), 0, std::ptr::null_mut(), 0);
        if r != 0 {
            *SINK.lock().unwrap() = None;
            return Err(r);
        }
    }
    Ok(())
}

/// Abort a [`connect`] in progress (it then returns an error soon). Safe at any time.
pub fn interrupt() {
    unsafe { ml::LiInterruptConnection() };
}

pub fn stop() {
    let _g = LOCK.lock().unwrap();
    unsafe { ml::LiStopConnection() };
    *SINK.lock().unwrap() = None;
}

pub fn request_idr() {
    unsafe { ml::LiRequestIdrFrame() };
}

/// Input through Moonlight's input stream (ENet, encrypted).
pub fn send_input(i: &Input) {
    unsafe {
        match i {
            Input::MouseAbs { x, y, width, height } => {
                ml::LiSendMousePositionEvent(*x, *y, *width, *height);
            }
            Input::MouseRel { dx, dy } => {
                ml::LiSendMouseMoveEvent(*dx, *dy);
            }
            Input::Button { button, down } => {
                ml::LiSendMouseButtonEvent(if *down { ml::BUTTON_ACTION_PRESS } else { ml::BUTTON_ACTION_RELEASE } as i8, *button as i32);
            }
            Input::Scroll { amount } => {
                ml::LiSendHighResScrollEvent(*amount);
            }
            Input::HScroll { amount } => {
                ml::LiSendHighResHScrollEvent(*amount);
            }
            Input::Key { vk, down, modifiers } => {
                ml::LiSendKeyboardEvent((0x8000 | *vk) as i16, if *down { ml::KEY_ACTION_DOWN } else { ml::KEY_ACTION_UP } as i8, *modifiers as i8);
            }
            Input::Text(t) => {
                let c = CString::new(t.as_str()).unwrap_or_default();
                ml::LiSendUtf8TextEvent(c.as_ptr(), t.len() as u32);
            }
        }
    }
}
