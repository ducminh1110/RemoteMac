//! Software H.264 (Annex-B) decoder producing BGRA pictures. This is the portable
//! baseline decoder: it lets every platform (and CI) verify the agent's real
//! VideoToolbox output. A hardware path (Media Foundation / D3D11) is a later,
//! drop-in replacement behind the same `Picture` type.

use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    /// Tightly packed BGRA, `width * height * 4` bytes (what a Windows DIB expects).
    pub bgra: Vec<u8>,
}

impl Picture {
    /// Number of distinct pixel values among a sample of the image (0 for empty).
    pub fn distinct_colors(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        for px in self.bgra.chunks_exact(4).step_by(7) {
            seen.insert([px[0], px[1], px[2]]);
            if seen.len() > 4096 {
                break;
            }
        }
        seen.len()
    }
}

pub struct H264Decoder {
    dec: Decoder,
}

impl H264Decoder {
    pub fn new() -> Result<Self, String> {
        Ok(Self { dec: Decoder::new().map_err(|e| format!("decoder init: {e}"))? })
    }

    /// Decode one access unit (Annex-B). `Ok(None)` means the decoder produced no picture yet
    /// (e.g. waiting for a keyframe). Errors are non-fatal: keep feeding frames, the next
    /// keyframe resynchronises the decoder.
    pub fn decode(&mut self, annexb: &[u8]) -> Result<Option<Picture>, String> {
        let yuv = self.dec.decode(annexb).map_err(|e| format!("decode: {e}"))?;
        let Some(yuv) = yuv else { return Ok(None) };
        let (w, h) = yuv.dimensions();
        let mut rgba = vec![0u8; w * h * 4];
        yuv.write_rgba8(&mut rgba);
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2); // RGBA -> BGRA
        }
        Ok(Some(Picture { width: w, height: h, bgra: rgba }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::encoder::Encoder;
    use openh264::formats::{RgbaSliceU8, YUVBuffer};

    fn frame(w: usize, h: usize, shift: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[(((x + shift) * 255 / w).min(255)) as u8, (y * 255 / h) as u8, 128, 255]);
            }
        }
        v
    }

    fn encode(frames: &[Vec<u8>], w: usize, h: usize) -> Vec<Vec<u8>> {
        let mut enc = Encoder::new().unwrap();
        frames
            .iter()
            .map(|f| {
                let yuv = YUVBuffer::from_rgb_source(RgbaSliceU8::new(f, (w, h)));
                enc.encode(&yuv).unwrap().to_vec()
            })
            .collect()
    }

    #[test]
    fn roundtrip_dimensions_and_content() {
        let (w, h) = (64, 48);
        let au = encode(&[frame(w, h, 0), frame(w, h, 8), frame(w, h, 16)], w, h);
        let mut d = H264Decoder::new().unwrap();
        let mut last = None;
        for a in &au {
            if let Some(p) = d.decode(a).unwrap() {
                last = Some(p);
            }
        }
        let p = last.expect("a picture");
        assert_eq!((p.width, p.height), (w, h));
        assert_eq!(p.bgra.len(), w * h * 4);
        assert!(p.distinct_colors() > 50, "gradient must survive: {}", p.distinct_colors());
        // red channel (index 2 in BGRA) increases left to right
        let at = |x: usize, y: usize| p.bgra[(y * w + x) * 4 + 2];
        assert!(at(w - 2, 10) > at(2, 10) + 100);
    }

    #[test]
    fn garbage_does_not_panic_and_decoder_recovers() {
        let (w, h) = (32, 32);
        let au = encode(&[frame(w, h, 0)], w, h);
        let mut d = H264Decoder::new().unwrap();
        let _ = d.decode(&[0, 0, 0, 1, 0x65, 1, 2, 3]);
        let _ = d.decode(&[0xde, 0xad, 0xbe, 0xef]);
        let _ = d.decode(&[]);
        let p = d.decode(&au[0]).unwrap().expect("recovers on the next keyframe");
        assert_eq!((p.width, p.height), (w, h));
    }
}
