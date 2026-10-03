//! The real Moonlight client core (moonlight-common-c, vendored) connects to this host: RTSP
//! handshake, encrypted control stream, video with FEC, input back. If this passes, any
//! Moonlight client speaks to RemoteMac's host.

use moonlight_sys as ml;
use openh264::encoder::Encoder;
use openh264::formats::{RgbaSliceU8, YUVBuffer};
use rm_gamestream::{Config, Event, Input, Session};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static FRAMES: AtomicUsize = AtomicUsize::new(0);
static IDR_FRAMES: AtomicUsize = AtomicUsize::new(0);
static BAD: AtomicUsize = AtomicUsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static TERMINATED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn submit(du: ml::PDECODE_UNIT) -> i32 {
    let du = &*du;
    let first = &*du.bufferList;
    let data = std::slice::from_raw_parts(first.data as *const u8, first.length as usize);
    if data.len() < 4 || data[..3] != [0, 0, 0] {
        BAD.fetch_add(1, Ordering::SeqCst);
    }
    if du.frameType == ml::FRAME_TYPE_IDR as i32 {
        IDR_FRAMES.fetch_add(1, Ordering::SeqCst);
    }
    FRAMES.fetch_add(1, Ordering::SeqCst);
    ml::DR_OK as i32
}

unsafe extern "C" fn setup(_: i32, _: i32, _: i32, _: i32, _: *mut std::ffi::c_void, _: i32) -> i32 {
    0
}
unsafe extern "C" fn audio_init(_: i32, _: ml::POPUS_MULTISTREAM_CONFIGURATION, _: *mut std::ffi::c_void, _: i32) -> i32 {
    0
}
unsafe extern "C" fn started() {
    STARTED.store(true, Ordering::SeqCst);
}
unsafe extern "C" fn terminated(code: i32) {
    eprintln!("connection terminated: {code}");
    TERMINATED.store(true, Ordering::SeqCst);
}
unsafe extern "C" fn stage_failed(stage: i32, code: i32) {
    eprintln!("stage {stage} failed: {code}");
}

fn frame(w: usize, h: usize, t: usize) -> Vec<u8> {
    (0..w * h).flat_map(|i| [((i % w + t * 4) % 256) as u8, ((i / w) % 256) as u8, 128, 255]).collect()
}

#[test]
fn moonlight_client_streams_from_this_host() {
    let key = *b"0123456789abcdef";
    let (session, events) = Session::start(Config { key, bind: "127.0.0.1".parse().unwrap(), fec_percentage: 20 }).unwrap();

    // the host side: encode and send frames once the client has announced its stream
    let s2 = session.clone();
    let (itx, irx) = std::sync::mpsc::channel::<Input>();
    std::thread::spawn(move || {
        let (w, h) = (320, 192);
        let mut enc = Encoder::new().unwrap();
        let mut started = false;
        let mut t = 0;
        let deadline = Instant::now() + Duration::from_secs(25);
        while Instant::now() < deadline {
            while let Ok(e) = events.try_recv() {
                match e {
                    Event::Started { .. } => started = true,
                    Event::RequestIdr => enc.force_intra_frame(),
                    Event::Input(i) => {
                        let _ = itx.send(i);
                    }
                    Event::Ended => return,
                }
            }
            if started && s2.client_ready() {
                let rgba = frame(w, h, t);
                let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(&rgba, (w, h)));
                let bs = enc.encode(&yuv).unwrap().to_vec();
                let idr = bs.windows(5).any(|x| x[..4] == [0, 0, 0, 1] && x[4] & 0x1f == 5);
                s2.send_frame(&bs, idr, Instant::now());
                t += 1;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    });

    unsafe {
        let mut si: ml::_SERVER_INFORMATION = std::mem::zeroed();
        ml::LiInitializeServerInformation(&mut si);
        let addr = CString::new("127.0.0.1").unwrap();
        let app = CString::new("7.1.431.-1").unwrap();
        let gfe = CString::new("3.23.0.74").unwrap();
        let url = CString::new(format!("rtsp://127.0.0.1:{}", session.rtsp_port)).unwrap();
        si.address = addr.as_ptr();
        si.serverInfoAppVersion = app.as_ptr();
        si.serverInfoGfeVersion = gfe.as_ptr();
        si.rtspSessionUrl = url.as_ptr();
        si.serverCodecModeSupport = ml::SCM_H264 as i32;

        let mut sc: ml::_STREAM_CONFIGURATION = std::mem::zeroed();
        ml::LiInitializeStreamConfiguration(&mut sc);
        sc.width = 320;
        sc.height = 192;
        sc.fps = 60;
        sc.bitrate = 5000;
        sc.packetSize = 1024;
        sc.streamingRemotely = ml::STREAM_CFG_LOCAL as i32;
        sc.audioConfiguration = (0x3 << 16) | (2 << 8) | 0xCA; // MAKE_AUDIO_CONFIGURATION(2, 0x3)
        sc.supportedVideoFormats = ml::VIDEO_FORMAT_H264 as i32;
        sc.encryptionFlags = 0;
        for (d, s) in sc.remoteInputAesKey.iter_mut().zip(key) {
            *d = s as _;
        }

        let mut dr: ml::_DECODER_RENDERER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeVideoCallbacks(&mut dr);
        dr.setup = Some(setup);
        dr.submitDecodeUnit = Some(submit);
        let mut ar: ml::_AUDIO_RENDERER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeAudioCallbacks(&mut ar);
        ar.init = Some(audio_init);
        let mut cl: ml::_CONNECTION_LISTENER_CALLBACKS = std::mem::zeroed();
        ml::LiInitializeConnectionCallbacks(&mut cl);
        cl.connectionStarted = Some(started);
        cl.connectionTerminated = Some(terminated);
        cl.stageFailed = Some(stage_failed);

        let r = ml::LiStartConnection(&mut si, &mut sc, &mut cl, &mut dr, &mut ar, std::ptr::null_mut(), 0, std::ptr::null_mut(), 0);
        assert_eq!(r, 0, "LiStartConnection failed");
        assert!(STARTED.load(Ordering::SeqCst));

        // video must flow
        let deadline = Instant::now() + Duration::from_secs(10);
        while FRAMES.load(Ordering::SeqCst) < 60 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        // input back to the host
        ml::LiSendMouseMoveEvent(5, -3);
        ml::LiSendMouseButtonEvent(ml::BUTTON_ACTION_PRESS as i8, ml::BUTTON_LEFT as i32);
        ml::LiSendKeyboardEvent(0x41, ml::KEY_ACTION_DOWN as i8, ml::MODIFIER_SHIFT as i8);
        ml::LiSendHighResScrollEvent(-120);
        let mut got = vec![];
        let deadline = Instant::now() + Duration::from_secs(5);
        let all = |g: &Vec<Input>| g.contains(&Input::Key { vk: 0x41, down: true, modifiers: 1 }) && g.iter().any(|i| matches!(i, Input::Scroll { .. }));
        while !all(&got) && Instant::now() < deadline {
            if let Ok(i) = irx.recv_timeout(Duration::from_millis(100)) {
                got.push(i);
            }
        }
        ml::LiStopConnection();

        let frames = FRAMES.load(Ordering::SeqCst);
        eprintln!("frames={frames} idr={} bad={} input={got:?}", IDR_FRAMES.load(Ordering::SeqCst), BAD.load(Ordering::SeqCst));
        assert!(frames >= 60, "video frames through moonlight-common-c: {frames}");
        assert!(IDR_FRAMES.load(Ordering::SeqCst) >= 1);
        assert_eq!(BAD.load(Ordering::SeqCst), 0);
        assert!(got.contains(&Input::MouseRel { dx: 5, dy: -3 }), "{got:?}");
        assert!(got.contains(&Input::Button { button: 1, down: true }), "{got:?}");
        assert!(got.contains(&Input::Key { vk: 0x41, down: true, modifiers: 1 }), "{got:?}");
        assert!(got.iter().any(|i| matches!(i, Input::Scroll { amount: -120 })), "{got:?}");
    }
}
