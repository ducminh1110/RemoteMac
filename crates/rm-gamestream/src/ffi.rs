//! C interface for the Swift agent (agent/macos/GameStream.h).

use crate::video::Packetizer;

/// A packetizer for one stream (`ssrc`: the window).
#[no_mangle]
pub extern "C" fn rm_gs_packetizer_new(packet_size: u32, fec_percentage: u32, min_fec_packets: u32, ssrc: u32) -> *mut Packetizer {
    let mut p = Packetizer::new(packet_size as usize, fec_percentage as usize, min_fec_packets as usize);
    p.ssrc = ssrc;
    Box::into_raw(Box::new(p))
}

/// # Safety
/// `p` from [`rm_gs_packetizer_new`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_packetizer_free(p: *mut Packetizer) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

/// # Safety
/// `p` from [`rm_gs_packetizer_new`].
#[no_mangle]
pub unsafe extern "C" fn rm_gs_packetizer_set_fec(p: *mut Packetizer, fec_percentage: u32) {
    if let Some(p) = p.as_mut() {
        p.fec_percentage = fec_percentage as usize;
    }
}

/// Packets of one frame, back to back, each `packet_size + 16` bytes; `*count` of them.
/// Free with [`rm_gs_free`] (`count * (packet_size + 16)` bytes).
///
/// # Safety
/// `p` from [`rm_gs_packetizer_new`]; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_packetize(p: *mut Packetizer, data: *const u8, len: usize, frame_index: u32, idr: bool, timestamp: u32, latency: u16, count: *mut usize) -> *mut u8 {
    let Some(p) = p.as_mut() else { return std::ptr::null_mut() };
    let pk = p.packetize(std::slice::from_raw_parts(data, len), frame_index, idr, timestamp, latency);
    *count = pk.len();
    let mut flat: Vec<u8> = pk.concat();
    flat.shrink_to_fit();
    let ptr = flat.as_mut_ptr();
    std::mem::forget(flat);
    ptr
}

/// # Safety
/// `ptr`/`len` as returned by [`rm_gs_packetize`].
#[no_mangle]
pub unsafe extern "C" fn rm_gs_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

// ---- Mac Desktop over full GameStream (tunnel.rs) ------------------------------------------------

use crate::tunnel::{HostTunnel, Outbound};
use crate::{Event, Input};
use std::ffi::c_void;
use std::sync::Arc;

/// kind: 0..2 a UDP flow (video, audio, control), 10 RTSP data on `id`, 11 RTSP `id` closed
pub type OutCb = extern "C" fn(ctx: *mut c_void, kind: i32, id: u32, data: *const u8, len: usize);

#[repr(C)]
pub struct RmGsEvent {
    /// 1 started (a w, b h, c fps, d kbit/s), 2 IDR wanted, 3 ended, 10 key (a vk, b down,
    /// c modifiers), 11 mouse moved by (a, b), 12 mouse at (a, b) in (c x d), 13 button (a, b
    /// down), 14 scroll a, 15 horizontal scroll a, 16 text (in `text`, NUL terminated)
    pub kind: i32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub text: [u8; 128],
}

struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

/// Start a host session behind the tunnel; `ports` gets rtsp, video, audio, control.
///
/// # Safety
/// `key` is 16 bytes; `ports` has room for 4; `cb(ctx, ...)` may be called from any thread
/// until [`rm_gs_desktop_stop`].
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_start(key: *const u8, fec_percentage: u32, cb: OutCb, ctx: *mut c_void, ports: *mut u16) -> *const HostTunnel {
    let k: [u8; 16] = std::slice::from_raw_parts(key, 16).try_into().unwrap();
    let c = SendPtr(ctx);
    let out = Arc::new(move |o: Outbound| {
        let c = &c;
        match o {
            Outbound::Udp { kind, data } => cb(c.0, kind as i32, 0, data.as_ptr(), data.len()),
            Outbound::TcpData { id, data } => cb(c.0, 10, id, data.as_ptr(), data.len()),
            Outbound::TcpClose { id } => cb(c.0, 11, id, std::ptr::null(), 0),
        }
    });
    match HostTunnel::start(k, fec_percentage as usize, out) {
        Ok(t) => {
            let p = std::slice::from_raw_parts_mut(ports, 4);
            p.copy_from_slice(&[t.session.rtsp_port, t.session.video_port, t.session.audio_port, t.session.control_port]);
            Arc::into_raw(t)
        }
        Err(_) => std::ptr::null(),
    }
}

/// # Safety
/// `t` from [`rm_gs_desktop_start`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_stop(t: *const HostTunnel) {
    if !t.is_null() {
        let t = Arc::from_raw(t);
        t.session.stop();
    }
}

/// One encoded frame; `age_us` since its capture. False while the client receives no video.
///
/// # Safety
/// `t` from [`rm_gs_desktop_start`]; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_frame(t: *const HostTunnel, data: *const u8, len: usize, idr: bool, age_us: u64) -> bool {
    let Some(t) = t.as_ref() else { return false };
    let captured = std::time::Instant::now() - std::time::Duration::from_micros(age_us.min(1_000_000));
    t.session.send_frame(std::slice::from_raw_parts(data, len), idr, captured)
}

/// The next event, if any.
///
/// # Safety
/// `t` from [`rm_gs_desktop_start`]; `e` valid.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_poll(t: *const HostTunnel, e: *mut RmGsEvent) -> bool {
    let Some(t) = t.as_ref() else { return false };
    let Ok(ev) = t.events.lock().unwrap().try_recv() else { return false };
    let e = &mut *e;
    (e.a, e.b, e.c, e.d) = (0, 0, 0, 0);
    e.text = [0; 128];
    e.kind = match ev {
        Event::Started { width, height, fps, bitrate_kbps } => {
            (e.a, e.b, e.c, e.d) = (width as i32, height as i32, fps as i32, bitrate_kbps as i32);
            1
        }
        Event::RequestIdr => 2,
        Event::Ended => 3,
        Event::Input(i) => match i {
            Input::Key { vk, down, modifiers } => {
                (e.a, e.b, e.c) = (vk as i32, down as i32, modifiers as i32);
                10
            }
            Input::MouseRel { dx, dy } => {
                (e.a, e.b) = (dx as i32, dy as i32);
                11
            }
            Input::MouseAbs { x, y, width, height } => {
                (e.a, e.b, e.c, e.d) = (x as i32, y as i32, width as i32, height as i32);
                12
            }
            Input::Button { button, down } => {
                (e.a, e.b) = (button as i32, down as i32);
                13
            }
            Input::Scroll { amount } => {
                e.a = amount as i32;
                14
            }
            Input::HScroll { amount } => {
                e.a = amount as i32;
                15
            }
            Input::Text(s) => {
                let b = s.as_bytes();
                let n = b.len().min(127);
                e.text[..n].copy_from_slice(&b[..n]);
                16
            }
        },
    };
    true
}

/// # Safety
/// `t` from [`rm_gs_desktop_start`]; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_udp_in(t: *const HostTunnel, kind: u8, data: *const u8, len: usize) {
    if let Some(t) = t.as_ref() {
        t.udp_in(kind, std::slice::from_raw_parts(data, len));
    }
}

/// # Safety
/// `t` from [`rm_gs_desktop_start`].
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_tcp_open(t: *const HostTunnel, id: u32) {
    if let Some(t) = t.as_ref() {
        t.tcp_open(id);
    }
}

/// # Safety
/// `t` from [`rm_gs_desktop_start`]; `data` valid for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_tcp_data(t: *const HostTunnel, id: u32, data: *const u8, len: usize) {
    if let Some(t) = t.as_ref() {
        t.tcp_data(id, std::slice::from_raw_parts(data, len));
    }
}

/// # Safety
/// `t` from [`rm_gs_desktop_start`].
#[no_mangle]
pub unsafe extern "C" fn rm_gs_desktop_tcp_close(t: *const HostTunnel, id: u32) {
    if let Some(t) = t.as_ref() {
        t.tcp_close(id);
    }
}

/// RemoteMac's physical key name for a Windows virtual-key code (NUL-terminated, static), or NULL.
#[no_mangle]
pub extern "C" fn rm_gs_vk_name(vk: u16) -> *const std::os::raw::c_char {
    static NAMES: std::sync::OnceLock<std::collections::HashMap<u16, std::ffi::CString>> = std::sync::OnceLock::new();
    let m = NAMES.get_or_init(|| (0u16..256).filter_map(|v| crate::tunnel::vk_name(v).map(|n| (v, std::ffi::CString::new(n).unwrap()))).collect());
    m.get(&vk).map_or(std::ptr::null(), |c| c.as_ptr())
}
