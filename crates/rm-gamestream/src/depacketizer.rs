//! Client side of the video packets [`crate::video`] sends, after moonlight-common-c's
//! RtpVideoQueue.c (FEC blocks: data shards + Reed-Solomon parity rebuilt with nanors) and
//! VideoDepacketizer.c (frame header, last-packet length, frames in order, IDR resync).
//!
//! Many streams can run at once (one per remote window): each has its own [`Depacketizer`];
//! the RTP SSRC names the stream.

use crate::video::{RAW_HEADER, RTP_HEADER};
use moonlight_sys::host as ffi;
use std::collections::BTreeMap;

const FLAG_EXTENSION: u8 = 0x10;
const FRAME_HEADER: usize = 8;
/// Frames older than this many behind the newest seen are given up.
const MAX_FRAME_LAG: u32 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub index: u32,
    pub idr: bool,
    /// Annex-B
    pub data: Vec<u8>,
    /// RTP 90 kHz capture timestamp
    pub timestamp: u32,
    /// host processing latency, 1/10 ms
    pub host_latency: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Frame(Frame),
    /// A frame could not be rebuilt: ask the host for an IDR (frames are dropped until one).
    Lost(u32),
}

#[derive(Default)]
struct Block {
    data_shards: usize,
    parity_shards: usize,
    shards: BTreeMap<usize, Vec<u8>>,
    done: Option<Vec<Vec<u8>>>,
}

#[derive(Default)]
struct PendingFrame {
    blocks: Vec<Block>,
    last_block: usize,
    timestamp: u32,
}

/// Fields of one video packet.
pub struct Header {
    pub ssrc: u32,
    pub frame_index: u32,
    pub block: usize,
    pub last_block: usize,
    pub shard: usize,
    pub data_shards: usize,
    pub fec_pct: usize,
    pub timestamp: u32,
}

pub fn header(p: &[u8]) -> Option<Header> {
    let off = if p.first()? & FLAG_EXTENSION != 0 { RTP_HEADER } else { 12 };
    let nv = p.get(off..off + 16)?;
    let fec = u32::from_le_bytes(nv[12..16].try_into().ok()?);
    Some(Header {
        ssrc: u32::from_be_bytes(p.get(8..12)?.try_into().ok()?),
        frame_index: u32::from_le_bytes(nv[4..8].try_into().ok()?),
        block: ((nv[11] >> 4) & 3) as usize,
        last_block: ((nv[11] >> 6) & 3) as usize,
        shard: ((fec >> 12) & 0x3ff) as usize,
        data_shards: ((fec >> 22) & 0x3ff) as usize,
        fec_pct: ((fec >> 4) & 0xff) as usize,
        timestamp: u32::from_be_bytes(p.get(4..8)?.try_into().ok()?),
    })
}

/// Rebuild missing data shards of a block from parity (in place). False if not possible.
fn recover(b: &mut Block, bs: usize) -> bool {
    let total = b.data_shards + b.parity_shards;
    if b.shards.len() < b.data_shards {
        return false;
    }
    if (0..b.data_shards).all(|i| b.shards.contains_key(&i)) {
        return true;
    }
    let mut bufs: Vec<Vec<u8>> = (0..total).map(|i| b.shards.get(&i).map(|s| { let mut v = s.clone(); v.resize(bs, 0); v }).unwrap_or_else(|| vec![0u8; bs])).collect();
    let mut marks: Vec<u8> = (0..total).map(|i| (!b.shards.contains_key(&i)) as u8).collect();
    unsafe {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| ffi::reed_solomon_init());
        let rs = ffi::reed_solomon_new(b.data_shards as i32, b.parity_shards as i32);
        if rs.is_null() {
            return false;
        }
        let mut ptrs: Vec<*mut u8> = bufs.iter_mut().map(|v| v.as_mut_ptr()).collect();
        let r = ffi::reed_solomon_decode(rs, ptrs.as_mut_ptr(), marks.as_mut_ptr(), total as i32, bs as i32);
        ffi::reed_solomon_release(rs);
        if r != 0 {
            return false;
        }
    }
    for (i, s) in bufs.into_iter().enumerate().take(b.data_shards) {
        b.shards.entry(i).or_insert(s);
    }
    true
}

pub struct Depacketizer {
    packet_size: usize,
    frames: BTreeMap<u32, PendingFrame>,
    /// next frame to deliver
    next: Option<u32>,
    /// a frame was lost: deliver nothing until an IDR
    waiting_idr: bool,
    pub recovered: u64,
    pub lost: u64,
}

impl Depacketizer {
    /// `packet_size`: the stream's packet size (packets are `packet_size + 16` bytes).
    pub fn new(packet_size: usize) -> Self {
        Self { packet_size, frames: BTreeMap::new(), next: None, waiting_idr: true, recovered: 0, lost: 0 }
    }

    fn before(a: u32, b: u32) -> bool {
        (a.wrapping_sub(b) as i32) < 0
    }

    /// One received packet; returns finished frames (in order) and losses.
    pub fn push(&mut self, p: &[u8]) -> Vec<Out> {
        let mut out = vec![];
        let bs = self.packet_size + RTP_HEADER;
        let Some(h) = header(p) else { return out };
        if p.len() > bs || h.block > 3 || h.data_shards == 0 {
            return out;
        }
        if self.next.is_some_and(|n| Self::before(h.frame_index, n)) {
            return out; // already delivered or given up
        }
        let f = self.frames.entry(h.frame_index).or_default();
        f.last_block = h.last_block;
        f.timestamp = h.timestamp;
        while f.blocks.len() <= h.last_block {
            f.blocks.push(Block::default());
        }
        let b = &mut f.blocks[h.block];
        if b.done.is_none() {
            b.data_shards = h.data_shards;
            b.parity_shards = (h.data_shards * h.fec_pct).div_ceil(100);
            b.shards.entry(h.shard).or_insert_with(|| p.to_vec());
            if b.shards.len() >= b.data_shards {
                let had_all = (0..b.data_shards).all(|i| b.shards.contains_key(&i));
                if recover(b, bs) {
                    if !had_all {
                        self.recovered += 1;
                    }
                    b.done = Some((0..b.data_shards).map(|i| b.shards[&i].clone()).collect());
                    b.shards.clear();
                }
            }
        }
        // deliver what is complete, in order; give up on frames left behind
        let newest = *self.frames.keys().next_back().unwrap();
        while let Some((&idx, f)) = self.frames.iter().next() {
            let complete = f.blocks.len() == f.last_block + 1 && f.blocks.iter().all(|b| b.done.is_some());
            if complete {
                let f = self.frames.remove(&idx).unwrap();
                self.next = Some(idx.wrapping_add(1));
                if let Some(fr) = self.assemble(idx, f) {
                    if fr.idr {
                        self.waiting_idr = false;
                    }
                    if !self.waiting_idr {
                        out.push(Out::Frame(fr));
                    }
                }
                continue;
            }
            // an older frame still incomplete while newer ones arrive: lost
            if newest.wrapping_sub(idx) >= MAX_FRAME_LAG {
                self.frames.remove(&idx);
                self.next = Some(idx.wrapping_add(1));
                self.lost += 1;
                if !self.waiting_idr {
                    self.waiting_idr = true;
                }
                out.push(Out::Lost(idx));
                continue;
            }
            break;
        }
        out
    }

    fn assemble(&self, index: u32, f: PendingFrame) -> Option<Frame> {
        let shards: Vec<Vec<u8>> = f.blocks.into_iter().flat_map(|b| b.done.unwrap()).collect();
        let mut payload = Vec::with_capacity(shards.len() * self.packet_size);
        let n = shards.len();
        for s in &shards {
            payload.extend_from_slice(&s[RAW_HEADER.min(s.len())..]);
        }
        if payload.len() < FRAME_HEADER || payload[0] != 0x01 {
            return None;
        }
        let per = self.packet_size - 16;
        let last = u16::from_le_bytes([payload[4], payload[5]]) as usize;
        let len = (n - 1) * per + last.min(per);
        payload.truncate(len.min(payload.len()));
        Some(Frame { index, idr: payload[3] == 2, host_latency: u16::from_le_bytes([payload[1], payload[2]]), timestamp: f.timestamp, data: payload.split_off(FRAME_HEADER) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Packetizer;

    fn frame(n: usize, seed: u8) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, 0x65];
        v.extend((0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
        v
    }

    #[test]
    fn frames_come_back_whole_and_in_order() {
        let (mut p, mut d) = (Packetizer::new(1024, 20, 1), Depacketizer::new(1024));
        let mut got = vec![];
        for i in 1..=5u32 {
            let f = frame(3000 * i as usize, i as u8);
            for pk in p.packetize(&f, i, i == 1, i * 1500, 3) {
                got.extend(d.push(&pk));
            }
        }
        assert_eq!(got.len(), 5);
        for (i, o) in got.iter().enumerate() {
            let Out::Frame(f) = o else { panic!("{o:?}") };
            assert_eq!(f.index, i as u32 + 1);
            assert_eq!(f.data, frame(3000 * (i + 1), (i + 1) as u8));
        }
    }

    #[test]
    fn parity_rebuilds_lost_packets_even_across_blocks() {
        let (mut p, mut d) = (Packetizer::new(1024, 20, 2), Depacketizer::new(1024));
        // big enough for several FEC blocks
        let f = frame(600_000, 9);
        let pk = p.packetize(&f, 1, true, 0, 0);
        let mut got = vec![];
        for (i, x) in pk.iter().enumerate() {
            if i % 9 != 4 {
                got.extend(d.push(x)); // ~11% lost
            }
        }
        assert_eq!(got, vec![Out::Frame(Frame { index: 1, idr: true, data: f, timestamp: 0, host_latency: 0 })]);
        assert!(d.recovered > 0);
    }

    #[test]
    fn a_lost_frame_waits_for_an_idr() {
        let (mut p, mut d) = (Packetizer::new(1024, 0, 0), Depacketizer::new(1024));
        let mut got = vec![];
        for i in 1..=10u32 {
            let pk = p.packetize(&frame(5000, i as u8), i, i == 1 || i == 9, 0, 0);
            for (j, x) in pk.iter().enumerate() {
                if !(i == 3 && j == 1) {
                    got.extend(d.push(x));
                }
            }
        }
        let frames: Vec<u32> = got.iter().filter_map(|o| if let Out::Frame(f) = o { Some(f.index) } else { None }).collect();
        assert!(got.contains(&Out::Lost(3)), "{got:?}");
        assert_eq!(frames, vec![1, 2, 9, 10]);
    }
}
