//! Video packets as Sunshine's videoBroadcastThread builds them (stream.cpp), which is what
//! moonlight-common-c's RtpVideoQueue / VideoDepacketizer expect:
//!
//! each packet is `blocksize = packetSize + 16` bytes:
//! `RTP (12, BE) | 4 reserved (RTP extension) | NV_VIDEO_PACKET (16, LE) | payload`;
//! the frame's payload starts with an 8-byte short frame header (type 0x01, frame type,
//! last-packet length); data packets are grouped in up to 4 FEC blocks, each block gets
//! Reed-Solomon parity (nanors, the same library the client decodes with) over whole packets.

use moonlight_sys::host as ffi;
use std::sync::Once;

pub const RTP_HEADER: usize = 16; // MAX_RTP_HEADER_SIZE: 12 fixed + 4 extension
pub const NV_VIDEO_PACKET: usize = 16;
pub const RAW_HEADER: usize = RTP_HEADER + NV_VIDEO_PACKET; // video_packet_raw_t
const SHORT_FRAME_HEADER: usize = 8;
const FLAG_CONTAINS_PIC_DATA: u8 = 0x1;
const FLAG_EOF: u8 = 0x2;
const FLAG_SOF: u8 = 0x4;
const FLAG_EXTENSION: u8 = 0x10;
const DATA_SHARDS_MAX: usize = 255;
const MAX_FEC_BLOCKS: usize = 4;

/// Per-session sender state.
pub struct Packetizer {
    /// `x-nv-video[0].packetSize` from the client's ANNOUNCE
    pub packet_size: usize,
    pub fec_percentage: usize,
    pub min_fec_packets: usize,
    /// next RTP sequence number
    pub lowseq: u16,
    /// RTP SSRC: which stream (RemoteMac: the window) these packets belong to; 0 for GameStream
    pub ssrc: u32,
}

/// Concatenate `a` and `b`, leaving `insert` bytes of room before every `slice` bytes
/// (Sunshine's concat_and_insert).
fn concat_and_insert(insert: usize, slice: usize, a: &[u8], b: &[u8]) -> Vec<u8> {
    let total = a.len() + b.len();
    let elements = total.div_ceil(slice);
    let mut out = vec![0u8; elements * insert + total];
    let joined = a.iter().chain(b.iter());
    let mut it = joined.copied();
    for x in 0..elements {
        let n = if x == elements - 1 { total - x * slice } else { slice };
        let base = x * (insert + slice) + insert;
        for i in 0..n {
            out[base + i] = it.next().unwrap();
        }
    }
    out
}

fn rs_init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe { ffi::reed_solomon_init() });
}

/// Parity for `data` (all `bs` bytes long), appended as `parity` new shards.
fn fec_encode(data: &mut Vec<Vec<u8>>, parity: usize, bs: usize) {
    if parity == 0 {
        return;
    }
    rs_init();
    let k = data.len();
    data.extend((0..parity).map(|_| vec![0u8; bs]));
    unsafe {
        let rs = ffi::reed_solomon_new(k as i32, parity as i32);
        if rs.is_null() {
            data.truncate(k);
            return;
        }
        let mut ptrs: Vec<*mut u8> = data.iter_mut().map(|s| s.as_mut_ptr()).collect();
        ffi::reed_solomon_encode(rs, ptrs.as_mut_ptr(), (k + parity) as i32, bs as i32);
        ffi::reed_solomon_release(rs);
    }
}

impl Packetizer {
    pub fn new(packet_size: usize, fec_percentage: usize, min_fec_packets: usize) -> Self {
        Self { packet_size, fec_percentage, min_fec_packets, lowseq: 0, ssrc: 0 }
    }

    /// All packets of one encoded frame (Annex-B), in send order.
    /// `frame_type`: 2 IDR, 1 P-frame. `timestamp`: 90 kHz RTP clock.
    /// `latency_tenth_ms`: capture-to-send time (Sunshine's frame processing latency).
    pub fn packetize(&mut self, data: &[u8], frame_index: u32, idr: bool, timestamp: u32, latency_tenth_ms: u16) -> Vec<Vec<u8>> {
        let blocksize = self.packet_size + RTP_HEADER;
        let payload_blocksize = blocksize - RAW_HEADER;
        let mut fh = [0u8; SHORT_FRAME_HEADER];
        fh[0] = 0x01;
        fh[1..3].copy_from_slice(&latency_tenth_ms.to_le_bytes());
        fh[3] = if idr { 2 } else { 1 };
        let mut last = ((data.len() + SHORT_FRAME_HEADER) % (self.packet_size - NV_VIDEO_PACKET)) as u16;
        if last == 0 {
            last = (self.packet_size - NV_VIDEO_PACKET) as u16;
        }
        fh[4..6].copy_from_slice(&last.to_le_bytes());
        let payload = concat_and_insert(RAW_HEADER, payload_blocksize, &fh, data);

        let mut fec_pct = self.fec_percentage;
        let max_data_shards = (DATA_SHARDS_MAX * 100) / (100 + fec_pct);
        let max_data_per_block = max_data_shards * blocksize;
        let mut blocks_needed = payload.len().div_ceil(max_data_per_block).max(1);
        if blocks_needed > MAX_FEC_BLOCKS {
            fec_pct = 0;
            blocks_needed = MAX_FEC_BLOCKS;
        }
        let unaligned = payload.len() / blocks_needed;
        let aligned = unaligned.div_ceil(blocksize) * blocksize;

        let mut out = Vec::new();
        for block in 0..blocks_needed {
            let start = block * aligned;
            let end = if block == blocks_needed - 1 { payload.len() } else { ((block + 1) * aligned).min(payload.len()) };
            if start >= end {
                continue;
            }
            let cur = &payload[start..end];
            let packets = cur.len().div_ceil(blocksize);
            let multi_fec_blocks = ((block as u8) << 4) | (((blocks_needed - 1) as u8) << 6);
            // data shards, each padded to blocksize, with their NV_VIDEO_PACKET filled in
            let mut shards: Vec<Vec<u8>> = (0..packets)
                .map(|x| {
                    let mut s = vec![0u8; blocksize];
                    let src = &cur[x * blocksize..((x + 1) * blocksize).min(cur.len())];
                    s[..src.len()].copy_from_slice(src);
                    let nv = &mut s[RTP_HEADER..RAW_HEADER];
                    nv[0..4].copy_from_slice(&((self.lowseq as u32 + x as u32) << 8).to_le_bytes());
                    nv[4..8].copy_from_slice(&frame_index.to_le_bytes());
                    let mut flags = FLAG_CONTAINS_PIC_DATA;
                    if x == 0 {
                        flags |= FLAG_SOF;
                    }
                    if x == packets - 1 {
                        flags |= FLAG_EOF;
                    }
                    nv[8] = flags;
                    nv[10] = 0x10; // multiFecFlags
                    nv[11] = multi_fec_blocks;
                    s
                })
                .collect();
            let data_shards = shards.len();
            let mut parity = (data_shards * fec_pct).div_ceil(100);
            let mut pct = fec_pct;
            if parity < self.min_fec_packets && fec_pct != 0 {
                parity = self.min_fec_packets;
                pct = (100 * parity) / data_shards;
            }
            fec_encode(&mut shards, parity, blocksize);
            let n = shards.len();
            for (x, s) in shards.iter_mut().enumerate() {
                let fec_info = ((x as u32) << 12) | ((data_shards as u32) << 22) | ((pct as u32) << 4);
                s[RTP_HEADER + 12..RTP_HEADER + 16].copy_from_slice(&fec_info.to_le_bytes());
                s[0] = 0x80 | FLAG_EXTENSION;
                s[1] = 0;
                s[2..4].copy_from_slice(&self.lowseq.wrapping_add(x as u16).to_be_bytes());
                s[4..8].copy_from_slice(&timestamp.to_be_bytes());
                s[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
                s[RTP_HEADER + 11] = multi_fec_blocks;
                s[RTP_HEADER + 4..RTP_HEADER + 8].copy_from_slice(&frame_index.to_le_bytes());
            }
            self.lowseq = self.lowseq.wrapping_add(n as u16);
            out.extend(shards);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_room_between_slices() {
        let v = concat_and_insert(2, 3, &[1, 2], &[3, 4, 5, 6]);
        assert_eq!(v, vec![0, 0, 1, 2, 3, 0, 0, 4, 5, 6]);
    }

    #[test]
    fn packets_carry_headers_and_parity() {
        let mut p = Packetizer::new(1024, 20, 2);
        let frame: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let pk = p.packetize(&frame, 7, true, 900, 0);
        let data = (5000 + 8usize).div_ceil(1024 + 16 - 32);
        assert_eq!(pk.len(), data + 2, "min 2 parity packets");
        assert!(pk.iter().all(|x| x.len() == 1040));
        assert_eq!(pk[0][0], 0x90);
        assert_eq!(u16::from_be_bytes([pk[1][2], pk[1][3]]), 1);
        assert_eq!(pk[0][RTP_HEADER + 8] & (FLAG_SOF | FLAG_CONTAINS_PIC_DATA), FLAG_SOF | FLAG_CONTAINS_PIC_DATA);
        assert_eq!(pk[data - 1][RTP_HEADER + 8] & FLAG_EOF, FLAG_EOF);
        // frame header then the frame's first bytes
        assert_eq!(&pk[0][RAW_HEADER..RAW_HEADER + 4], &[1, 0, 0, 2]);
        assert_eq!(&pk[0][RAW_HEADER + 8..RAW_HEADER + 11], &[0, 1, 2]);
        assert_eq!(p.lowseq as usize, pk.len());
    }
}
