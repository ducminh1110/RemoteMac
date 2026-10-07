//! The real Moonlight client core (moonlight-common-c, vendored) connects to this host: RTSP
//! handshake, encrypted control stream, video with FEC, input back. If this passes, any
//! Moonlight client speaks to RemoteMac's host.

mod common;
use rm_gamestream::{Config, Session};

#[test]
fn moonlight_client_streams_from_this_host() {
    let (session, events) = Session::start(Config { key: common::KEY, bind: "127.0.0.1".parse().unwrap(), fec_percentage: 20 }).unwrap();
    let port = session.rtsp_port;
    common::stream_and_check(session, events, port);
}
