//! Sound from the Mac: the packets, and the viewer's jitter buffer.
//!
//! The Mac captures what the session's apps play (ScreenCaptureKit) as 16-bit PCM, 48 kHz
//! stereo, cut into 5 ms packets ([`PACKET_FRAMES`]). While the UDP path is alive a packet goes
//! as the datagram `"RM" 26 0 | payload`, encrypted like every session datagram; otherwise it
//! is a frame on the Audio channel (7) of the encrypted stream. The Mac sends sound only after
//! the viewer asked for it (`Message::AudioControl`), so a viewer that does not know the Audio
//! channel never sees one.
//!
//! Payload (header big endian, samples little endian):
//! ```text
//!  0 seq u32   4 pts_us u64 (agent clock, first sample)   12 format u8 (1 = s16le interleaved)
//! 13 channels u8   14 frames u16   16 samples (frames * channels * 2 bytes)
//! ```
//!
//! The viewer plays packets through a [`JitterBuffer`]: it waits for a small cushion before it
//! starts, plays packets in sequence order, hides a lost packet with a short fade instead of a
//! click, cuts the delay back when packets bunch up, and follows the small difference between
//! the Mac's and the PC's sample clocks by skipping or repeating a single frame now and then.

use crate::ProtocolError;
use std::collections::BTreeMap;

/// Datagram type of an audio packet.
pub const T_AUDIO: u8 = 26;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 5 ms at 48 kHz: one packet stays far below the path's datagram size (960 bytes of samples).
pub const PACKET_FRAMES: usize = 240;
/// Longest packet accepted (20 ms).
pub const MAX_PACKET_FRAMES: usize = 960;
pub const FORMAT_S16LE: u8 = 1;
pub const HEADER: usize = 16;

/// One packet of sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    pub seq: u32,
    /// capture time of the first sample, on the agent clock (microseconds)
    pub pts_us: u64,
    pub channels: u8,
    /// interleaved samples, `frames * channels` of them
    pub samples: Vec<i16>,
}

impl AudioPacket {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    pub fn encode_payload(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(HEADER + self.samples.len() * 2);
        v.extend_from_slice(&self.seq.to_be_bytes());
        v.extend_from_slice(&self.pts_us.to_be_bytes());
        v.push(FORMAT_S16LE);
        v.push(self.channels);
        v.extend_from_slice(&(self.frames() as u16).to_be_bytes());
        for s in &self.samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    pub fn decode_payload(p: &[u8]) -> Result<Self, ProtocolError> {
        let bad = |w: &str| ProtocolError::Malformed(format!("audio packet: {w}"));
        if p.len() < HEADER {
            return Err(bad("header truncated"));
        }
        if p[12] != FORMAT_S16LE {
            return Err(bad("unknown sample format"));
        }
        let channels = p[13];
        if !(1..=2).contains(&channels) {
            return Err(bad("channel count"));
        }
        let frames = u16::from_be_bytes([p[14], p[15]]) as usize;
        if frames == 0 || frames > MAX_PACKET_FRAMES {
            return Err(bad("frame count"));
        }
        let need = HEADER + frames * channels as usize * 2;
        if p.len() != need {
            return Err(bad("length does not match the frame count"));
        }
        Ok(AudioPacket {
            seq: u32::from_be_bytes(p[0..4].try_into().unwrap()),
            pts_us: u64::from_be_bytes(p[4..12].try_into().unwrap()),
            channels,
            samples: p[HEADER..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
        })
    }

    /// The datagram (before encryption).
    pub fn datagram(&self) -> Vec<u8> {
        let mut d = vec![crate::udp::MAGIC[0], crate::udp::MAGIC[1], T_AUDIO, 0];
        d.extend(self.encode_payload());
        d
    }

    /// An audio datagram (after decryption), else None.
    pub fn from_datagram(d: &[u8]) -> Option<Self> {
        if d.len() < 4 || d[..2] != crate::udp::MAGIC || d[2] != T_AUDIO {
            return None;
        }
        Self::decode_payload(&d[4..]).ok()
    }
}

/// What the jitter buffer saw, for the stats overlay and the logs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStats {
    pub received: u64,
    /// arrived after their turn had passed (dropped)
    pub late: u64,
    pub duplicates: u64,
    /// never arrived (a short fade played instead)
    pub lost: u64,
    /// the buffer ran dry (it waits for its cushion again)
    pub underruns: u64,
    /// packets dropped to bring the delay back down after a burst
    pub trimmed: u64,
    /// frames repeated (+) or skipped (-) to follow the Mac's clock
    pub drift_frames: i64,
    pub buffered_ms: f64,
    pub target_ms: f64,
    pub jitter_ms: f64,
    pub playing: bool,
}

/// Shortest and longest cushion the buffer aims for.
pub const MIN_TARGET_MS: f64 = 30.0;
pub const MAX_TARGET_MS: f64 = 150.0;
/// Delay above the cushion that is cut back at once.
const TRIM_ABOVE_MS: f64 = 80.0;
/// A sequence jump this large is a new stream (the Mac restarted capture).
const RESET_JUMP: i64 = 2_000;

fn ms_to_frames(ms: f64) -> usize {
    (ms * SAMPLE_RATE as f64 / 1000.0).round() as usize
}

fn frames_to_ms(f: usize) -> f64 {
    f as f64 * 1000.0 / SAMPLE_RATE as f64
}

pub struct JitterBuffer {
    queue: BTreeMap<u64, Vec<i16>>,
    queued_samples: usize,
    next: Option<u64>,
    highest: Option<u64>,
    cur: Vec<i16>,
    pos: usize,
    playing: bool,
    last: [i16; CHANNELS],
    target: usize,
    jitter_us: f64,
    last_transit: Option<i64>,
    fill_avg: f64,
    since_adjust: usize,
    /// frames played since playing (re)started: the clock is followed only once settled
    played: usize,
    gain: f32,
    gain_target: f32,
    volume: f32,
    muted: bool,
    pub stats: AudioStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    pub fn new() -> Self {
        let target = ms_to_frames(40.0);
        JitterBuffer {
            queue: BTreeMap::new(),
            queued_samples: 0,
            next: None,
            highest: None,
            cur: vec![],
            pos: 0,
            playing: false,
            last: [0; CHANNELS],
            target,
            jitter_us: 0.0,
            last_transit: None,
            fill_avg: target as f64,
            since_adjust: 0,
            played: 0,
            gain: 1.0,
            gain_target: 1.0,
            volume: 1.0,
            muted: false,
            stats: AudioStats { target_ms: 40.0, ..Default::default() },
        }
    }

    /// Volume 0..=1 as the slider shows it (heard as about even steps).
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.update_gain();
    }

    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        self.update_gain();
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn muted(&self) -> bool {
        self.muted
    }

    fn update_gain(&mut self) {
        self.gain_target = if self.muted { 0.0 } else { self.volume * self.volume };
    }

    /// Start over (a new connection): nothing queued, waiting for the cushion.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.queued_samples = 0;
        self.next = None;
        self.highest = None;
        self.cur.clear();
        self.pos = 0;
        self.playing = false;
        self.last_transit = None;
    }

    /// Frames waiting to be played.
    pub fn buffered_frames(&self) -> usize {
        (self.cur.len().saturating_sub(self.pos) + self.queued_samples) / CHANNELS
    }

    pub fn target_frames(&self) -> usize {
        self.target
    }

    pub fn buffered_ms(&self) -> f64 {
        frames_to_ms(self.buffered_frames())
    }

    fn extend(&self, seq: u32) -> u64 {
        match self.highest {
            None => seq as u64 + (1 << 32),
            Some(h) => (h as i64 + seq.wrapping_sub(h as u32) as i32 as i64).max(0) as u64,
        }
    }

    /// A packet arrived at `arrival_us` (the viewer's clock, microseconds).
    pub fn push(&mut self, p: &AudioPacket, arrival_us: u64) {
        let samples: Vec<i16> = match p.channels {
            1 => p.samples.iter().flat_map(|&s| [s, s]).collect(),
            2 => p.samples.clone(),
            _ => return,
        };
        if samples.is_empty() {
            return;
        }
        self.stats.received += 1;
        let ext = self.extend(p.seq);
        if let Some(h) = self.highest {
            if (ext as i64 - h as i64).abs() > RESET_JUMP {
                self.reset();
                return self.push(p, arrival_us);
            }
        }
        // interarrival jitter (RFC 3550): how much the delay of packets varies
        let transit = arrival_us as i64 - p.pts_us as i64;
        if let Some(lt) = self.last_transit {
            let d = (transit - lt).unsigned_abs() as f64;
            if d < 1_000_000.0 {
                self.jitter_us += (d - self.jitter_us) / 16.0;
            }
        }
        self.last_transit = Some(transit);
        self.retarget();
        if self.next.is_some_and(|n| ext < n) {
            self.stats.late += 1;
            return;
        }
        if self.queue.contains_key(&ext) {
            self.stats.duplicates += 1;
            return;
        }
        self.queued_samples += samples.len();
        self.queue.insert(ext, samples);
        self.highest = Some(self.highest.map_or(ext, |h| h.max(ext)));
        if self.next.is_none() {
            self.next = Some(ext);
        }
        if self.next.is_some_and(|n| ext < n) {
            self.next = Some(ext);
        }
    }

    /// The cushion follows the measured jitter: up at once, down slowly.
    fn retarget(&mut self) {
        let want = (20.0 + 2.5 * self.jitter_us / 1000.0).clamp(MIN_TARGET_MS, MAX_TARGET_MS);
        let now = frames_to_ms(self.target);
        let ms = if want > now { want } else { now + (want - now) * 0.01 };
        self.target = ms_to_frames(ms);
        self.stats.target_ms = ms;
        self.stats.jitter_ms = self.jitter_us / 1000.0;
    }

    /// Move on to the next packet; false when there is nothing to play.
    fn advance(&mut self) -> bool {
        let Some(n) = self.next else { return false };
        if let Some(pkt) = self.queue.remove(&n) {
            self.queued_samples -= pkt.len();
            self.cur = pkt;
            self.pos = 0;
            self.next = Some(n + 1);
            return true;
        }
        if self.highest.is_some_and(|h| h > n) {
            // packets after it are here: this one is lost; a fade from the last sample hides it
            self.stats.lost += 1;
            self.next = Some(n + 1);
            let mut fade = Vec::with_capacity(PACKET_FRAMES * CHANNELS);
            for i in 0..PACKET_FRAMES {
                let k = 1.0 - (i as f32 + 1.0) / (PACKET_FRAMES as f32 / 4.0);
                for c in 0..CHANNELS {
                    fade.push((self.last[c] as f32 * k.max(0.0)) as i16);
                }
            }
            self.cur = fade;
            self.pos = 0;
            return true;
        }
        false
    }

    fn take_frame(&mut self) -> Option<[i16; CHANNELS]> {
        if self.pos + CHANNELS > self.cur.len() && !self.advance() {
            return None;
        }
        let f = [self.cur[self.pos], self.cur[self.pos + 1]];
        self.pos += CHANNELS;
        Some(f)
    }

    /// Drop whole packets from the front until the delay is back near the cushion.
    fn trim(&mut self) {
        while self.buffered_frames() > self.target + PACKET_FRAMES {
            let Some((&k, _)) = self.queue.iter().next() else { break };
            let pkt = self.queue.remove(&k).unwrap();
            self.queued_samples -= pkt.len();
            self.next = Some(k + 1);
            self.stats.trimmed += 1;
        }
    }

    /// Fill `out` (interleaved stereo) with what plays next.
    pub fn pull(&mut self, out: &mut [i16]) {
        let frames = out.len() / CHANNELS;
        let buffered = self.buffered_frames();
        // the fill level averaged over about half a second
        self.fill_avg += (buffered as f64 - self.fill_avg) * (frames as f64 / 24_000.0).min(1.0);
        if self.playing && buffered > self.target + ms_to_frames(TRIM_ABOVE_MS) {
            self.trim();
            self.fill_avg = self.buffered_frames() as f64;
        }
        if !self.playing && buffered >= self.target.max(1) {
            self.playing = true;
            self.played = 0;
            self.fill_avg = buffered as f64;
        }
        // follow the Mac's clock once playing has settled (a second): at most one frame in 480
        // (0.2 %), inaudible
        let mut adjust = 0i8;
        self.since_adjust += frames;
        if self.playing {
            self.played += frames;
        }
        if self.playing && self.played >= SAMPLE_RATE as usize && self.since_adjust >= 480 {
            self.since_adjust = 0;
            let slack = ms_to_frames(10.0) as f64;
            if self.fill_avg > self.target as f64 + slack {
                adjust = -1;
            } else if self.fill_avg < self.target as f64 - slack && buffered > 0 {
                adjust = 1;
            }
        }
        for i in 0..frames {
            let f = if !self.playing {
                None
            } else if adjust == 1 && i == frames / 2 {
                adjust = 0;
                self.stats.drift_frames += 1;
                Some(self.last)
            } else {
                if adjust == -1 && i == frames / 2 {
                    adjust = 0;
                    if self.take_frame().is_some() {
                        self.stats.drift_frames -= 1;
                    }
                }
                self.take_frame()
            };
            let f = match f {
                Some(f) => f,
                None => {
                    if self.playing {
                        self.playing = false;
                        self.stats.underruns += 1;
                    }
                    // silence, faded from the last sample (no click)
                    [(self.last[0] as f32 * 0.9) as i16, (self.last[1] as f32 * 0.9) as i16]
                }
            };
            self.last = f;
            self.gain += (self.gain_target - self.gain).clamp(-1.0 / 480.0, 1.0 / 480.0);
            for c in 0..CHANNELS {
                out[i * CHANNELS + c] = (f[c] as f32 * self.gain).round().clamp(-32768.0, 32767.0) as i16;
            }
        }
        self.stats.playing = self.playing;
        self.stats.buffered_ms = frames_to_ms(self.buffered_frames());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A packet whose samples count up from `base` (left) and down (right).
    fn pkt(seq: u32, base: i16) -> AudioPacket {
        let mut s = Vec::with_capacity(PACKET_FRAMES * 2);
        for i in 0..PACKET_FRAMES as i16 {
            s.push(base.wrapping_add(i));
            s.push(base.wrapping_add(i).wrapping_neg());
        }
        AudioPacket { seq, pts_us: seq as u64 * 5_000, channels: 2, samples: s }
    }

    fn pull_frames(jb: &mut JitterBuffer, frames: usize) -> Vec<i16> {
        let mut out = vec![0i16; frames * 2];
        jb.pull(&mut out);
        out
    }

    #[test]
    fn packet_roundtrip_and_validation() {
        let p = pkt(7, 100);
        let d = p.datagram();
        assert_eq!(&d[..4], &[b'R', b'M', T_AUDIO, 0]);
        assert_eq!(d.len(), 4 + HEADER + PACKET_FRAMES * 4);
        assert_eq!(AudioPacket::from_datagram(&d).unwrap(), p);
        assert_eq!(AudioPacket::decode_payload(&p.encode_payload()).unwrap(), p);
        let mut bad = p.encode_payload();
        bad[12] = 9; // format
        assert!(AudioPacket::decode_payload(&bad).is_err());
        let mut bad = p.encode_payload();
        bad[13] = 3; // channels
        assert!(AudioPacket::decode_payload(&bad).is_err());
        let mut bad = p.encode_payload();
        bad.pop(); // length
        assert!(AudioPacket::decode_payload(&bad).is_err());
        let mut bad = p.encode_payload();
        bad[14..16].copy_from_slice(&2000u16.to_be_bytes());
        assert!(AudioPacket::decode_payload(&bad).is_err());
        assert!(AudioPacket::decode_payload(&[0; 4]).is_err());
        assert!(AudioPacket::from_datagram(b"RM\x10\x00").is_none());
    }

    #[test]
    fn waits_for_its_cushion_then_plays_in_order() {
        let mut jb = JitterBuffer::new();
        jb.push(&pkt(0, 0), 0);
        // 5 ms queued, the cushion is 40 ms: silence
        assert!(pull_frames(&mut jb, 240).iter().all(|&s| s == 0));
        for i in 1..12 {
            jb.push(&pkt(i, (i * 240) as i16), i as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240 * 3);
        assert!(jb.stats.playing);
        let left: Vec<i16> = out.chunks(2).map(|c| c[0]).collect();
        assert_eq!(left, (0..720).map(|i| i as i16).collect::<Vec<_>>());
    }

    #[test]
    fn reordered_packets_play_in_sequence_and_late_ones_are_dropped() {
        let mut jb = JitterBuffer::new();
        for s in [0, 2, 1, 3, 5, 4, 6, 7, 8, 9] {
            jb.push(&pkt(s, (s * 240) as i16), s as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240 * 6);
        let left: Vec<i16> = out.chunks(2).map(|c| c[0]).collect();
        assert_eq!(left, (0..1440).map(|i| i as i16).collect::<Vec<_>>());
        jb.push(&pkt(2, 0), 60_000);
        assert_eq!(jb.stats.late, 1);
        jb.push(&pkt(8, 0), 60_000);
        assert_eq!(jb.stats.duplicates, 1);
    }

    #[test]
    fn a_lost_packet_fades_instead_of_stopping() {
        let mut jb = JitterBuffer::new();
        for s in (0..14).filter(|&s| s != 3) {
            jb.push(&pkt(s, 1000), s as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240 * 5);
        assert_eq!(jb.stats.lost, 1);
        assert_eq!(jb.stats.underruns, 0);
        // the lost packet's slot fades towards zero, then packet 4 plays as sent
        let slot: Vec<i16> = out[240 * 3 * 2..240 * 4 * 2].chunks(2).map(|c| c[0]).collect();
        assert!(slot[0].abs() <= 1239 && slot[100] == 0, "{:?}", &slot[..4]);
        assert_eq!(out[240 * 4 * 2], 1000);
    }

    #[test]
    fn running_dry_counts_an_underrun_and_starts_again_after_the_cushion() {
        let mut jb = JitterBuffer::new();
        for s in 0..10 {
            jb.push(&pkt(s, 500), s as u64 * 5_000);
        }
        let _ = pull_frames(&mut jb, 240 * 12);
        assert_eq!(jb.stats.underruns, 1);
        assert!(!jb.stats.playing);
        // back: silence until the cushion is there again
        for s in 10..20 {
            jb.push(&pkt(s, 500), s as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240);
        assert!(jb.stats.playing);
        assert_eq!(out[0], 500);
    }

    #[test]
    fn a_burst_is_trimmed_back_to_the_cushion() {
        let mut jb = JitterBuffer::new();
        for s in 0..10 {
            jb.push(&pkt(s, 0), s as u64 * 5_000);
        }
        let _ = pull_frames(&mut jb, 240);
        // a second of sound arrives at once (the network held it back)
        for s in 10..210 {
            jb.push(&pkt(s, 0), 50_000);
        }
        let _ = pull_frames(&mut jb, 240);
        assert!(jb.stats.trimmed > 150, "{:?}", jb.stats);
        assert!(jb.buffered_ms() <= jb.stats.target_ms + 10.0, "{:?}", jb.stats);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut jb = JitterBuffer::new();
        let start = u32::MAX - 4;
        for i in 0..12u32 {
            jb.push(&pkt(start.wrapping_add(i), (i * 240) as i16), i as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240 * 10);
        let left: Vec<i16> = out.chunks(2).map(|c| c[0]).collect();
        assert_eq!(left, (0..2400).map(|i| i as i16).collect::<Vec<_>>());
        assert_eq!(jb.stats.lost + jb.stats.late, 0);
    }

    #[test]
    fn a_restarted_stream_is_followed() {
        let mut jb = JitterBuffer::new();
        for s in 0..10 {
            jb.push(&pkt(5_000_000 + s, 0), s as u64 * 5_000);
        }
        for s in 0..10 {
            jb.push(&pkt(s, 77), 100_000 + s as u64 * 5_000);
        }
        let out = pull_frames(&mut jb, 240);
        assert_eq!(out[0], 77);
    }

    #[test]
    fn mono_is_played_on_both_sides() {
        let mut jb = JitterBuffer::new();
        for s in 0..10u32 {
            jb.push(&AudioPacket { seq: s, pts_us: 0, channels: 1, samples: vec![300; 240] }, 0);
        }
        let out = pull_frames(&mut jb, 10);
        assert_eq!(&out[..4], &[300, 300, 300, 300]);
    }

    #[test]
    fn volume_and_mute_ramp_without_clicks() {
        let mut jb = JitterBuffer::new();
        for s in 0..200 {
            jb.push(&AudioPacket { seq: s, pts_us: 0, channels: 2, samples: vec![10_000; 480] }, 0);
        }
        jb.set_volume(0.5);
        let out = pull_frames(&mut jb, 960);
        // ramps down from full to a quarter (0.5 squared) within 10 ms, never jumping
        assert!(out.windows(2).all(|w| (w[0] - w[1]).abs() < 30));
        assert_eq!(*out.last().unwrap(), 2_500);
        jb.set_muted(true);
        let out = pull_frames(&mut jb, 960);
        assert_eq!(*out.last().unwrap(), 0);
        assert!(jb.muted());
    }

    /// The Mac's clock runs 0.1 % fast against the PC's: the buffer stays near its cushion by
    /// dropping single frames, without trimming whole packets or running dry.
    #[test]
    fn follows_a_drifting_clock() {
        for (rate, sign) in [(1.001, -1), (0.999, 1)] {
            let mut jb = JitterBuffer::new();
            let (mut sent, mut t_us) = (0u32, 0f64);
            let mut out = vec![0i16; 480 * 2];
            for _ in 0..3000 {
                // 30 s of pulls, 10 ms each
                t_us += 10_000.0;
                while (sent as f64) * 5_000.0 / rate <= t_us {
                    jb.push(&pkt(sent, 0), t_us as u64);
                    sent += 1;
                }
                jb.pull(&mut out);
            }
            let st = &jb.stats;
            assert_eq!(st.trimmed, 0, "{st:?}");
            assert!(st.underruns <= 1, "{st:?}");
            // 0.1 % of 30 s is 1440 frames; the first ~20 ms of it is absorbed by the cushion
            assert!(st.drift_frames.signum() == sign && st.drift_frames.abs() > 300, "{st:?}");
            assert!((st.buffered_ms - st.target_ms).abs() < 25.0, "{st:?}");
        }
    }

    #[test]
    fn jitter_raises_the_cushion() {
        let mut jb = JitterBuffer::new();
        for s in 0..400u32 {
            // packets arrive with up to 30 ms of jitter
            let wobble = if s % 2 == 0 { 0 } else { 30_000 };
            jb.push(&pkt(s, 0), s as u64 * 5_000 + wobble);
        }
        assert!(jb.stats.jitter_ms > 15.0 && jb.stats.target_ms > 60.0, "{:?}", jb.stats);
        assert!(jb.stats.target_ms <= MAX_TARGET_MS);
    }
}
