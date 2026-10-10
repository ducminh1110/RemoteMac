//! Window shapes (`window_mask`): one alpha byte per picture pixel, sent as runs, since a
//! window's mask is almost all 255 with a few rows of rounding at the top and bottom (a whole
//! interior of rows is one run).
//!
//! Format: (u16 LE length 1..=65535, u8 alpha) pairs covering `width * height` pixels, row by
//! row from the top. The Mac makes them (Shape.swift), the viewer applies them.

/// Runs of `alpha`.
pub fn encode(alpha: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < alpha.len() {
        let v = alpha[i];
        let mut n = 1;
        while i + n < alpha.len() && alpha[i + n] == v && n < u16::MAX as usize {
            n += 1;
        }
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.push(v);
        i += n;
    }
    out
}

/// The `len` alpha bytes of `runs`, or None when they do not add up to exactly `len`.
pub fn decode(runs: &[u8], len: usize) -> Option<Vec<u8>> {
    if !runs.len().is_multiple_of(3) {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for r in runs.chunks_exact(3) {
        let n = u16::from_le_bytes([r[0], r[1]]) as usize;
        if n == 0 || out.len() + n > len {
            return None;
        }
        out.resize(out.len() + n, r[2]);
    }
    (out.len() == len).then_some(out)
}

/// The mask of a `window_mask` message (`rle` base64), None when it is fully opaque (empty) or
/// does not fit `width` x `height`.
pub fn from_message(width: u32, height: u32, rle: &str) -> Option<Vec<u8>> {
    if rle.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let runs = crate::base64_decode(rle).ok()?;
    let m = decode(&runs, width as usize * height as usize)?;
    (!m.iter().all(|a| *a == 255)).then_some(m)
}

/// Radius (pixels) of the rounding at a corner of a `width`-wide mask, measured along its
/// top or bottom row: how far in the first opaque pixel is, as a circle of radius r leaves
/// r - sqrt(r - 1/4) clear on its outer row. 0 for a square corner.
pub fn corner_radius(mask: &[u8], width: usize, height: usize, top: bool, left: bool) -> f32 {
    if width == 0 || height == 0 || mask.len() < width * height {
        return 0.0;
    }
    let row = if top { 0 } else { height - 1 };
    let line = &mask[row * width..(row + 1) * width];
    let clear = if left { line.iter().take_while(|a| **a < 128).count() } else { line.iter().rev().take_while(|a| **a < 128).count() };
    if clear == 0 || clear >= width / 2 {
        return 0.0;
    }
    // invert t = r - sqrt(r - 0.25) for r
    let t = clear as f32;
    let mut r = t + t.sqrt();
    for _ in 0..8 {
        r = t + (r - 0.25).max(0.0).sqrt();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A w x h window with rounded corners of radius r (anti-aliased by coverage).
    fn rounded(w: usize, h: usize, r: f32) -> Vec<u8> {
        let mut m = vec![255u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let cx = px.clamp(r, w as f32 - r);
                let cy = py.clamp(r, h as f32 - r);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                m[y * w + x] = (a * 255.0).round() as u8;
            }
        }
        m
    }

    #[test]
    fn runs_round_trip_and_stay_small() {
        let m = rounded(800, 600, 26.0);
        let runs = encode(&m);
        assert_eq!(decode(&runs, m.len()).unwrap(), m);
        assert!(runs.len() < 3000, "a window's mask is small: {} bytes", runs.len());
        // wrong sizes are refused, not padded
        assert!(decode(&runs, m.len() + 1).is_none());
        assert!(decode(&runs[..runs.len() - 3], m.len()).is_none());
        assert!(decode(&[0, 0, 9], 0).is_none());
        // a run longer than 65535 is split
        let big = vec![255u8; 200_000];
        assert_eq!(decode(&encode(&big), big.len()).unwrap(), big);
    }

    #[test]
    fn messages_give_the_mask_or_nothing() {
        let m = rounded(40, 30, 8.0);
        let b64 = crate::base64_encode(&encode(&m));
        assert_eq!(from_message(40, 30, &b64).unwrap(), m);
        assert!(from_message(41, 30, &b64).is_none(), "another size is not used");
        assert!(from_message(40, 30, "").is_none());
        assert!(from_message(2, 2, &crate::base64_encode(&encode(&[255; 4]))).is_none(), "opaque: nothing to apply");
    }

    #[test]
    fn corner_radius_is_measured_from_the_mask() {
        for r in [10.0f32, 16.0, 26.0, 52.0] {
            let m = rounded(400, 300, r);
            for (top, left) in [(true, true), (true, false), (false, true), (false, false)] {
                let got = corner_radius(&m, 400, 300, top, left);
                assert!((got - r).abs() <= 1.5, "radius {r}: measured {got}");
            }
        }
        assert_eq!(corner_radius(&[255; 100], 10, 10, true, true), 0.0);
    }
}
