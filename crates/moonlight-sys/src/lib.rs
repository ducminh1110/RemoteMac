//! Raw bindings to moonlight-common-c (Moonlight's streaming core: RTSP handshake, ENet
//! control and input, RTP video with Reed-Solomon FEC, audio), compiled from
//! `third_party/moonlight-common-c`. The library keeps one connection per process.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, clippy::all)]

mod bindings {
    include!("bindings.rs");
}
pub use bindings::*;

pub mod crypto;

/// ENet (host side), through shim/enet_shim.c, and nanors' Reed-Solomon encoder.
pub mod host {
    use std::os::raw::{c_int, c_uchar, c_uint, c_ushort, c_void};

    #[repr(C)]
    pub struct ENetHost {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct ENetPeer {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct ENetPacket {
        _p: [u8; 0],
    }
    #[repr(C)]
    pub struct ReedSolomon {
        _p: [u8; 0],
    }

    extern "C" {
        pub fn rm_enet_init() -> c_int;
        pub fn rm_enet_server(port: c_ushort, ipv6: c_int, peers: usize, channels: usize) -> *mut ENetHost;
        pub fn rm_enet_port(host: *mut ENetHost) -> c_ushort;
        pub fn rm_enet_service(host: *mut ENetHost, timeout_ms: c_uint, peer: *mut *mut ENetPeer, data: *mut c_uint, packet: *mut *mut ENetPacket) -> c_int;
        pub fn rm_enet_packet_data(p: *mut ENetPacket, len: *mut usize) -> *const c_uchar;
        pub fn rm_enet_packet_free(p: *mut ENetPacket);
        pub fn rm_enet_send(peer: *mut ENetPeer, channel: c_uchar, data: *const c_void, len: usize, reliable: c_int) -> c_int;
        pub fn rm_enet_flush(host: *mut ENetHost);
        pub fn rm_enet_disconnect_now(peer: *mut ENetPeer);
        pub fn rm_enet_destroy(host: *mut ENetHost);
        pub fn rm_enet_client(ip: *const std::os::raw::c_char, port: c_ushort, channels: usize, connect_data: c_uint, timeout_ms: c_uint, peer: *mut *mut ENetPeer) -> *mut ENetHost;

        pub fn reed_solomon_init();
        pub fn reed_solomon_new(data_shards: c_int, parity_shards: c_int) -> *mut ReedSolomon;
        pub fn reed_solomon_release(rs: *mut ReedSolomon);
        pub fn reed_solomon_encode(rs: *mut ReedSolomon, shards: *mut *mut u8, nr_shards: c_int, bs: c_int) -> c_int;
        /// `marks[i]` = 1 for a missing shard (rebuilt in place)
        pub fn reed_solomon_decode(rs: *mut ReedSolomon, shards: *mut *mut u8, marks: *mut u8, nr_shards: c_int, bs: c_int) -> c_int;
    }
}
