//! Video over UDP (beside the TCP control connection, both through the relay). Every encoded
//! frame is cut into equal shards, grouped in blocks of at most [`MAX_BLOCK`] data shards, and
//! each block gets Reed-Solomon parity ([`crate::fec`]). A lost packet is rebuilt from parity
//! instead of waiting for a retransmission, which is what makes TCP stall on lossy, far links.
//!
//! Datagram: `"RM" | type u8 | ...`. Types below 16 are for the relay itself (rm-relay); the
//! relay forwards everything else between the paired agent and client unread.
//!
//! Video shard (type 16), big endian, [`HEADER`] bytes then the shard:
//! ```text
//!  0 magic "RM"   2 type=16   3 flags (bit0 keyframe)   4 window u64   12 seq u32
//! 16 pts_us u64  24 width u16 26 height u16  28 frame_len u32
//! 32 block u8    33 blocks u8 34 index u8 (in block: <k data, else parity)  35 k u8  36 m u8
//! 37 reserved    38 shard bytes (same length for every shard of the frame)
//! ```

use crate::{fec, VideoFrame};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

pub const MAGIC: [u8; 2] = *b"RM";
pub const T_VIDEO: u8 = 16;
/// client -> agent, every 200 ms: what arrived (drives FEC strength and bitrate; also "UDP works")
pub const T_FEEDBACK: u8 = 17;
/// either way: `t_us` u64; answered with T_PONG `t_us` (echo) + the answerer's clock
pub const T_PING: u8 = 18;
pub const T_PONG: u8 = 19;
pub const HEADER: usize = 38;
/// Shard bytes per datagram: header + shard + UDP/IP stay under 1280 (no fragmentation).
pub const SHARD: usize = 1200;
pub const MAX_BLOCK: usize = 64;
pub const MAX_FRAME: usize = 16 << 20;
/// An incomplete frame is given up after this long.
pub const FRAME_TIMEOUT: Duration = Duration::from_millis(150);

fn be16(p: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([p[o], p[o + 1]])
}
fn be32(p: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(p[o..o + 4].try_into().unwrap())
}
fn be64(p: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(p[o..o + 8].try_into().unwrap())
}

/// Parity shards for a block of `k` data shards at `fec_pct` percent (at least 1 when on).
pub fn parity_for(k: usize, fec_pct: u32) -> usize {
    if fec_pct == 0 {
        return 0;
    }
    (k * fec_pct as usize).div_ceil(100).clamp(1, fec::MAX_SHARDS - k)
}

/// All datagrams of one frame (data shards first, block by block, then that block's parity).
pub fn packetize(v: &VideoFrame, seq: u32, fec_pct: u32) -> Vec<Vec<u8>> {
    let len = v.data.len();
    let n = len.div_ceil(SHARD).max(1);
    let size = len.div_ceil(n).max(1);
    let blocks = n.div_ceil(MAX_BLOCK);
    let mut out = Vec::new();
    for b in 0..blocks {
        let first = b * MAX_BLOCK;
        let k = MAX_BLOCK.min(n - first);
        let m = parity_for(k, fec_pct);
        let shards: Vec<Vec<u8>> = (0..k)
            .map(|j| {
                let start = ((first + j) * size).min(len);
                let end = (start + size).min(len);
                let mut s = v.data[start..end].to_vec();
                s.resize(size, 0);
                s
            })
            .collect();
        let refs: Vec<&[u8]> = shards.iter().map(|s| s.as_slice()).collect();
        let parity = fec::encode(&refs, m);
        for (i, s) in shards.iter().chain(parity.iter()).enumerate() {
            let mut d = Vec::with_capacity(HEADER + size);
            d.extend_from_slice(&MAGIC);
            d.push(T_VIDEO);
            d.push(v.keyframe as u8);
            d.extend_from_slice(&v.window_id.to_be_bytes());
            d.extend_from_slice(&seq.to_be_bytes());
            d.extend_from_slice(&v.pts_us.to_be_bytes());
            d.extend_from_slice(&v.width.to_be_bytes());
            d.extend_from_slice(&v.height.to_be_bytes());
            d.extend_from_slice(&(len as u32).to_be_bytes());
            d.extend_from_slice(&[b as u8, blocks as u8, i as u8, k as u8, m as u8, 0]);
            d.extend_from_slice(s);
            out.push(d);
        }
    }
    out
}

/// What the client tells the agent every 200 ms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Feedback {
    /// shards that should have arrived / that did (before FEC): the link's loss
    pub expected: u32,
    pub received: u32,
    /// frames rebuilt thanks to parity / frames lost even so
    pub recovered: u32,
    pub lost: u32,
    pub frames: u32,
    /// the client's current round trip to the agent (ms, 0 = unknown): rising above its floor
    /// means queues are filling somewhere on the path, before any packet is lost
    pub rtt_ms: u32,
}

impl Feedback {
    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![MAGIC[0], MAGIC[1], T_FEEDBACK, 0];
        for v in [self.expected, self.received, self.recovered, self.lost, self.frames, self.rtt_ms] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        d
    }
    pub fn decode(p: &[u8]) -> Option<Self> {
        (p.len() >= 24 && p[..2] == MAGIC && p[2] == T_FEEDBACK).then(|| Self {
            expected: be32(p, 4),
            received: be32(p, 8),
            recovered: be32(p, 12),
            lost: be32(p, 16),
            frames: be32(p, 20),
            rtt_ms: if p.len() >= 28 { be32(p, 24) } else { 0 },
        })
    }
    /// Share of packets the link dropped.
    pub fn loss(&self) -> f64 {
        if self.expected == 0 { 0.0 } else { 1.0 - self.received.min(self.expected) as f64 / self.expected as f64 }
    }
}

pub fn ping(t_us: u64) -> Vec<u8> {
    let mut d = vec![MAGIC[0], MAGIC[1], T_PING, 0];
    d.extend_from_slice(&t_us.to_be_bytes());
    d
}

/// (t_us echoed, the answerer's clock)
pub fn pong(t_us: u64, now_us: u64) -> Vec<u8> {
    let mut d = vec![MAGIC[0], MAGIC[1], T_PONG, 0];
    d.extend_from_slice(&t_us.to_be_bytes());
    d.extend_from_slice(&now_us.to_be_bytes());
    d
}

pub fn parse_ping(p: &[u8]) -> Option<u64> {
    (p.len() >= 12 && p[..2] == MAGIC && p[2] == T_PING).then(|| be64(p, 4))
}

pub fn parse_pong(p: &[u8]) -> Option<(u64, u64)> {
    (p.len() >= 20 && p[..2] == MAGIC && p[2] == T_PONG).then(|| (be64(p, 4), be64(p, 12)))
}

struct Block {
    k: usize,
    m: usize,
    shards: Vec<Option<Vec<u8>>>,
    have: usize,
    /// data shards that arrived themselves (not rebuilt)
    data_have: usize,
    done: bool,
}

struct Partial {
    keyframe: bool,
    pts_us: u64,
    width: u16,
    height: u16,
    len: usize,
    size: usize,
    blocks: Vec<Option<Block>>,
    first: Instant,
    recovered: bool,
}

impl Partial {
    fn complete(&self) -> bool {
        self.blocks.iter().all(|b| b.as_ref().is_some_and(|b| b.done))
    }
    /// (data shards expected, data shards that arrived). Parity is left out: a block is done
    /// as soon as any k shards are in, so later parity packets are never seen and would read
    /// as losses. Missing data shards are an unbiased sample of the link's loss.
    fn counts(&self) -> (u32, u32) {
        let mut e = 0;
        let mut r = 0;
        for b in &self.blocks {
            match b {
                Some(b) => {
                    e += b.k as u32;
                    r += b.data_have as u32;
                }
                None => e += 1,
            }
        }
        (e, r)
    }
}

/// What reassembly produced.
#[derive(Debug)]
pub enum Out {
    Frame(VideoFrame),
    /// A frame of this window is gone (even with FEC): the decoder needs a keyframe.
    Lost(u64),
}

#[derive(Default)]
struct WindowState {
    /// next seq to hand out; None until the first keyframe
    next: Option<u32>,
    pending: BTreeMap<u32, Partial>,
    waiting_key: bool,
    /// last time this window asked for a keyframe (Out::Lost), while waiting for one
    asked: Option<Instant>,
}

/// While a window waits for a keyframe (start, or after a loss), ask again this often: the
/// keyframe itself can be lost too, and nothing else would ever bring the picture back.
pub const KEY_RETRY: Duration = Duration::from_millis(500);

/// Rebuilds frames from shards, in order per window. After a loss it hands out nothing but
/// a keyframe (decoding past a hole would only show garbage).
#[derive(Default)]
pub struct Reassembler {
    windows: HashMap<u64, WindowState>,
    pub stats: Feedback,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the counters for a feedback report (and reset them).
    pub fn take_stats(&mut self) -> Feedback {
        std::mem::take(&mut self.stats)
    }

    pub fn push(&mut self, p: &[u8], now: Instant) -> Vec<Out> {
        let mut out = vec![];
        if p.len() <= HEADER || p[..2] != MAGIC || p[2] != T_VIDEO {
            return out;
        }
        let window = be64(p, 4);
        let seq = be32(p, 12);
        let len = be32(p, 28) as usize;
        let (block, blocks, index, k, m) = (p[32] as usize, p[33] as usize, p[34] as usize, p[35] as usize, p[36] as usize);
        let size = p.len() - HEADER;
        if len > MAX_FRAME || blocks == 0 || block >= blocks || k == 0 || k > MAX_BLOCK || index >= k + m || k + m > fec::MAX_SHARDS {
            return out;
        }
        let w = self.windows.entry(window).or_insert_with(|| WindowState { waiting_key: true, asked: Some(now), ..Default::default() });
        if w.next.is_some_and(|n| seq.wrapping_sub(n) > u32::MAX / 2) {
            return out; // older than what was already handed out
        }
        if w.pending.len() > 64 && !w.pending.contains_key(&seq) {
            return out;
        }
        let f = w.pending.entry(seq).or_insert_with(|| Partial {
            keyframe: p[3] & 1 == 1,
            pts_us: be64(p, 16),
            width: be16(p, 24),
            height: be16(p, 26),
            len,
            size,
            blocks: (0..blocks).map(|_| None).collect(),
            first: now,
            recovered: false,
        });
        if f.size != size || f.len != len || f.blocks.len() != blocks {
            return out;
        }
        let b = f.blocks[block].get_or_insert_with(|| Block { k, m, shards: vec![None; k + m], have: 0, data_have: 0, done: false });
        if b.k != k || b.m != m || b.done || b.shards[index].is_some() {
            return out;
        }
        b.shards[index] = Some(p[HEADER..].to_vec());
        b.have += 1;
        b.data_have += (index < k) as usize;
        if b.have >= b.k {
            let had_all_data = b.shards[..b.k].iter().all(|s| s.is_some());
            if fec::reconstruct(b.k, b.m, &mut b.shards) {
                b.done = true;
                f.recovered |= !had_all_data;
            }
        }
        self.flush(window, now, &mut out);
        out
    }

    /// Give up on frames that waited too long (call every few ms).
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        let mut out = vec![];
        let ids: Vec<u64> = self.windows.keys().copied().collect();
        for id in ids {
            self.flush(id, now, &mut out);
        }
        out
    }

    fn flush(&mut self, window: u64, now: Instant, out: &mut Vec<Out>) {
        let Some(w) = self.windows.get_mut(&window) else { return };
        if w.waiting_key && w.asked.is_some_and(|t| now.duration_since(t) >= KEY_RETRY) && !w.pending.values().any(|p| p.keyframe) {
            w.asked = Some(now);
            out.push(Out::Lost(window));
        }
        loop {
            let Some((&seq, f)) = w.pending.iter().next() else { return };
            let in_order = w.next == Some(seq);
            if f.complete() && (if w.waiting_key { f.keyframe } else { in_order }) {
                let f = w.pending.remove(&seq).unwrap();
                let (e, r) = f.counts();
                self.stats.expected += e;
                self.stats.received += r;
                self.stats.frames += 1;
                self.stats.recovered += f.recovered as u32;
                w.next = Some(seq.wrapping_add(1));
                w.waiting_key = false;
                let mut data = Vec::with_capacity(f.len);
                for b in f.blocks.into_iter().flatten() {
                    for s in b.shards.into_iter().take(b.k).flatten() {
                        data.extend_from_slice(&s);
                    }
                }
                data.truncate(f.len);
                out.push(Out::Frame(VideoFrame { window_id: window, pts_us: f.pts_us, keyframe: f.keyframe, codec: 1, width: f.width, height: f.height, data }));
                continue;
            }
            // the oldest frame blocks the rest: wait for it, unless it is hopeless
            let newer_complete = w.pending.values().skip(1).any(|p| p.complete());
            let age = now.duration_since(f.first);
            let late = age > FRAME_TIMEOUT;
            let skippable = w.waiting_key && !f.keyframe; // not decodable anyway
            // a later frame done, or the expected one never showed up at all: a short grace for
            // reordering, then it is a loss
            let overtaken = (newer_complete || (!in_order && w.next.is_some())) && age > Duration::from_millis(30);
            if late || skippable || overtaken {
                let f = w.pending.remove(&seq).unwrap();
                let (e, r) = f.counts();
                self.stats.expected += e;
                self.stats.received += r;
                if !w.waiting_key {
                    // first loss since the last good frame: the decoder needs a keyframe
                    self.stats.lost += 1;
                    w.waiting_key = true;
                    w.asked = Some(now);
                    out.push(Out::Lost(window));
                } else if f.keyframe || w.asked.is_none_or(|t| now.duration_since(t) >= KEY_RETRY) {
                    // still no usable keyframe (it was lost as well): ask again
                    w.asked = Some(now);
                    out.push(Out::Lost(window));
                }
                if w.next.is_some_and(|n| seq.wrapping_sub(n) < u32::MAX / 2) {
                    w.next = Some(seq.wrapping_add(1));
                }
                continue;
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(window: u64, n: usize, key: bool) -> VideoFrame {
        VideoFrame { window_id: window, pts_us: 42, keyframe: key, codec: 1, width: 640, height: 480, data: (0..n).map(|i| (i * 7 % 251) as u8).collect() }
    }

    #[test]
    fn whole_frames_round_trip() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        for (seq, n) in [(0u32, 100usize), (1, 5000), (2, 200_000), (3, 1)] {
            let v = frame(5, n, seq == 0);
            let mut got = vec![];
            for d in packetize(&v, seq, 20) {
                got.extend(r.push(&d, now));
            }
            assert_eq!(got.len(), 1, "seq {seq}");
            match &got[0] {
                Out::Frame(f) => assert_eq!(f, &v),
                o => panic!("{o:?}"),
            }
        }
        assert_eq!(r.stats.frames, 4);
        assert_eq!(r.stats.lost, 0);
        assert_eq!(r.stats.loss(), 0.0, "a clean link reads as no loss even with parity");
    }

    #[test]
    fn parity_rebuilds_lost_packets() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let v = frame(1, 60_000, true); // 50 shards, 10 parity
        let mut d = packetize(&v, 0, 20);
        assert_eq!(d.len(), 60);
        // lose 10 packets spread over the block
        for i in (0..10).rev() {
            d.remove(i * 5);
        }
        let got: Vec<Out> = d.iter().flat_map(|p| r.push(p, now)).collect();
        assert!(matches!(&got[..], [Out::Frame(f)] if f == &v), "{got:?}");
        assert_eq!(r.stats.recovered, 1);
        // the 10 lost packets were all data shards: 10 of 50
        assert!(r.stats.loss() > 0.1 && r.stats.loss() < 0.25, "{}", r.stats.loss());
    }

    #[test]
    fn a_hole_waits_for_the_next_keyframe() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        let send = |r: &mut Reassembler, seq: u32, key: bool, drop: usize, t: Instant| -> Vec<Out> {
            let v = frame(9, 3000, key);
            packetize(&v, seq, 0).iter().skip(drop).flat_map(|p| r.push(p, t)).collect()
        };
        assert!(matches!(send(&mut r, 0, true, 0, t0)[..], [Out::Frame(_)]));
        assert!(send(&mut r, 1, false, 1, t0).is_empty()); // incomplete, no parity
        // frame 2 is complete but 1 is missing: after a moment 1 is declared lost
        let later = t0 + Duration::from_millis(40);
        let out = send(&mut r, 2, false, 0, later);
        assert!(matches!(out[..], [Out::Lost(9)]), "{} outputs", out.len());
        // P-frames are not handed out until a keyframe arrives
        assert!(send(&mut r, 3, false, 0, later).is_empty());
        assert!(matches!(send(&mut r, 4, true, 0, later)[..], [Out::Frame(ref f)] if f.keyframe));
        assert!(matches!(send(&mut r, 5, false, 0, later)[..], [Out::Frame(_)]));
        assert_eq!(r.stats.lost, 1);
    }

    #[test]
    fn a_lost_keyframe_is_asked_for_again() {
        let mut r = Reassembler::new();
        let t0 = Instant::now();
        // the first keyframe arrives incomplete (no parity): it expires
        for p in packetize(&frame(3, 5000, true), 1, 0).iter().skip(1) {
            assert!(r.push(p, t0).is_empty());
        }
        let out = r.tick(t0 + FRAME_TIMEOUT + Duration::from_millis(10));
        assert!(matches!(out[..], [Out::Lost(3)]), "a dropped keyframe must ask for another: {out:?}");
        // nothing arrives at all: still asking, every KEY_RETRY
        assert!(r.tick(t0 + FRAME_TIMEOUT + Duration::from_millis(20)).is_empty());
        let out = r.tick(t0 + FRAME_TIMEOUT + KEY_RETRY + Duration::from_millis(20));
        assert!(matches!(out[..], [Out::Lost(3)]), "{out:?}");
        // the new keyframe brings the picture back
        let got: Vec<Out> = packetize(&frame(3, 5000, true), 9, 0).iter().flat_map(|p| r.push(p, t0 + Duration::from_secs(2))).collect();
        assert!(matches!(got[..], [Out::Frame(ref f)] if f.keyframe));
    }

    #[test]
    fn feedback_and_ping_round_trip() {
        let f = Feedback { expected: 100, received: 93, recovered: 2, lost: 1, frames: 30, rtt_ms: 85 };
        assert_eq!(Feedback::decode(&f.encode()), Some(f));
        assert!((f.loss() - 0.07).abs() < 1e-9);
        assert_eq!(parse_ping(&ping(77)), Some(77));
        assert_eq!(parse_pong(&pong(77, 99)), Some((77, 99)));
    }
}
