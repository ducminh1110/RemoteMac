//! Raw bindings to moonlight-common-c (Moonlight's streaming core: RTSP handshake, ENet
//! control and input, RTP video with Reed-Solomon FEC, audio), compiled from
//! `third_party/moonlight-common-c`. The library keeps one connection per process.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code, clippy::all)]

mod bindings {
    include!("bindings.rs");
}
pub use bindings::*;

pub mod crypto;
