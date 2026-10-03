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
