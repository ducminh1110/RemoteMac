//! Picture size from an H.264 SPS (with its cropping), for the decoder's visible size when
//! frames arrive without one (GameStream sends only the bitstream).

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let b = (*self.d.get(self.pos / 8)? >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        Some(if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) })
    }
}

/// The SPS NAL (payload after the header byte) of an Annex-B access unit, emulation
/// prevention bytes removed.
fn find_sps(annexb: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    while i + 3 < annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            let start = i + 3;
            if annexb.get(start).is_some_and(|h| h & 0x1f == 7) {
                let mut end = start + 1;
                while end + 2 < annexb.len() && !(annexb[end] == 0 && annexb[end + 1] == 0 && (annexb[end + 2] == 1 || annexb[end + 2] == 0)) {
                    end += 1;
                }
                let raw = &annexb[start + 1..end.max(start + 1).min(annexb.len())];
                let mut out = Vec::with_capacity(raw.len());
                let mut z = 0;
                for &b in raw {
                    if z >= 2 && b == 3 {
                        z = 0;
                        continue;
                    }
                    z = if b == 0 { z + 1 } else { 0 };
                    out.push(b);
                }
                return Some(out);
            }
            i = start;
        } else {
            i += 1;
        }
    }
    None
}

/// (width, height) in pixels of the SPS in `annexb`, if there is one.
pub fn h264_size(annexb: &[u8]) -> Option<(u32, u32)> {
    let sps = find_sps(annexb)?;
    let mut b = Bits { d: &sps, pos: 0 };
    let profile = b.bits(8)?;
    b.bits(16)?; // constraint flags, level
    b.ue()?; // sps id
    let mut chroma = 1;
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = b.ue()?;
        if chroma == 3 {
            b.bit()?;
        }
        b.ue()?;
        b.ue()?;
        b.bit()?;
        if b.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + b.se()? + 256) % 256;
                        }
                        last = if next == 0 { last } else { next };
                    }
                }
            }
        }
    }
    b.ue()?; // log2_max_frame_num
    let poc = b.ue()?;
    if poc == 0 {
        b.ue()?;
    } else if poc == 1 {
        b.bit()?;
        b.se()?;
        b.se()?;
        for _ in 0..b.ue()? {
            b.se()?;
        }
    }
    b.ue()?; // max refs
    b.bit()?;
    let w_mbs = b.ue()? + 1;
    let h_map = b.ue()? + 1;
    let frame_mbs_only = b.bit()?;
    if frame_mbs_only == 0 {
        b.bit()?;
    }
    b.bit()?;
    let (mut cl, mut cr, mut ct, mut cb) = (0, 0, 0, 0);
    if b.bit()? == 1 {
        (cl, cr, ct, cb) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
    }
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let crop_x = if chroma == 0 { 1 } else { sub_w };
    let crop_y = (if chroma == 0 { 1 } else { sub_h }) * (2 - frame_mbs_only);
    let w = w_mbs * 16 - crop_x * (cl + cr);
    let h = (2 - frame_mbs_only) * h_map * 16 - crop_y * (ct + cb);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    #[test]
    fn size_of_a_cropped_1080p_sps() {
        // x264-style SPS for 1920x1080 (1088 coded, cropped by 8 rows), High profile
        let au = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xf0, 0x3c, 0x60, 0xc6, 0x58];
        assert_eq!(super::h264_size(&au), Some((1920, 1080)));
    }
}
