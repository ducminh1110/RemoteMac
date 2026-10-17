//! Logo / favicon validation.
//!
//! The file type is decided by magic bytes, never by the file name or the browser-supplied
//! content type. Only PNG, JPEG and WebP are accepted; SVG (and therefore embedded script/CSS)
//! is always rejected. Dimensions are read from the image headers:
//! PNG `IHDR`, JPEG `SOFn` markers, WebP `VP8 `/`VP8L`/`VP8X` chunks.

use serde::Serialize;

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_DIM: u32 = 1024;
pub const MIN_DIM: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

impl ImageKind {
    pub fn ext(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpg",
            ImageKind::Webp => "webp",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Webp => "image/webp",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageInfo {
    pub kind: ImageKind,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

/// Validate an uploaded image and return its type and size.
pub fn validate_image(data: &[u8]) -> Result<ImageInfo, String> {
    if data.is_empty() {
        return Err("The file is empty.".into());
    }
    if data.len() > MAX_BYTES {
        return Err(format!(
            "The file is {:.1} MB; the maximum is 1 MB.",
            data.len() as f64 / (1024.0 * 1024.0)
        ));
    }
    if looks_like_markup(data) {
        return Err("SVG and other text/markup files are not accepted (they can carry scripts or styles). Export the logo as PNG, JPEG or WebP.".into());
    }
    let (kind, (width, height)) = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        (ImageKind::Png, png_dims(data)?)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        (ImageKind::Jpeg, jpeg_dims(data)?)
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        (ImageKind::Webp, webp_dims(data)?)
    } else {
        return Err("Unsupported file type. Use PNG, JPEG or WebP.".into());
    };
    if width == 0 || height == 0 {
        return Err("The image has zero width or height.".into());
    }
    if width > MAX_DIM || height > MAX_DIM {
        return Err(format!(
            "The image is {width}x{height} pixels; the maximum is {MAX_DIM}x{MAX_DIM}."
        ));
    }
    if width < MIN_DIM || height < MIN_DIM {
        return Err(format!(
            "The image is {width}x{height} pixels; use at least {MIN_DIM}x{MIN_DIM}."
        ));
    }
    Ok(ImageInfo {
        kind,
        width,
        height,
        bytes: data.len(),
    })
}

fn looks_like_markup(data: &[u8]) -> bool {
    let head = &data[..data.len().min(512)];
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let trimmed: Vec<u8> = head
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .copied()
        .collect();
    if trimmed.first() == Some(&b'<') {
        return true;
    }
    let lower = String::from_utf8_lossy(head).to_ascii_lowercase();
    lower.contains("<svg") || lower.contains("<?xml") || lower.contains("<script")
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
fn be16(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}
fn le16(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}
fn le24(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 3)
        .map(|b| b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
}
fn le32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn corrupt(kind: &str) -> String {
    format!("The {kind} file is damaged or truncated.")
}

fn png_dims(d: &[u8]) -> Result<(u32, u32), String> {
    // Signature (8) + length (4) + "IHDR" (4) + width (4) + height (4).
    if d.get(12..16) != Some(b"IHDR") {
        return Err(corrupt("PNG"));
    }
    match (be32(d, 16), be32(d, 20)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err(corrupt("PNG")),
    }
}

fn jpeg_dims(d: &[u8]) -> Result<(u32, u32), String> {
    let mut i = 2;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            return Err(corrupt("JPEG"));
        }
        let marker = d[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2; // standalone markers
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            break; // end of image / start of scan without a frame header
        }
        let len = be16(d, i + 2).ok_or_else(|| corrupt("JPEG"))? as usize;
        if len < 2 {
            return Err(corrupt("JPEG"));
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let h = be16(d, i + 5).ok_or_else(|| corrupt("JPEG"))?;
            let w = be16(d, i + 7).ok_or_else(|| corrupt("JPEG"))?;
            return Ok((w as u32, h as u32));
        }
        i += 2 + len;
    }
    Err(corrupt("JPEG"))
}

fn webp_dims(d: &[u8]) -> Result<(u32, u32), String> {
    let chunk = d.get(12..16).ok_or_else(|| corrupt("WebP"))?;
    match chunk {
        b"VP8 " => {
            // Frame tag (3) + start code 9D 01 2A, then 14-bit width/height.
            if d.get(23..26) != Some(&[0x9D, 0x01, 0x2A]) {
                return Err(corrupt("WebP"));
            }
            let w = le16(d, 26).ok_or_else(|| corrupt("WebP"))? & 0x3FFF;
            let h = le16(d, 28).ok_or_else(|| corrupt("WebP"))? & 0x3FFF;
            Ok((w as u32, h as u32))
        }
        b"VP8L" => {
            if d.get(20) != Some(&0x2F) {
                return Err(corrupt("WebP"));
            }
            let bits = le32(d, 21).ok_or_else(|| corrupt("WebP"))?;
            Ok(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => {
            let w = le24(d, 24).ok_or_else(|| corrupt("WebP"))? + 1;
            let h = le24(d, 27).ok_or_else(|| corrupt("WebP"))? + 1;
            Ok((w, h))
        }
        _ => Err(corrupt("WebP")),
    }
}

/// Test helpers that build minimal, header-valid images.
pub mod fixtures {
    pub fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v.extend_from_slice(&[0, 0, 0, 0]); // CRC (not checked)
        v.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
        v
    }

    pub fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        // APP0 JFIF
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        v.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        // SOF0
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    pub fn webp_vp8x(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&22u32.to_le_bytes());
        v.extend_from_slice(b"WEBPVP8X");
        v.extend_from_slice(&10u32.to_le_bytes());
        v.extend_from_slice(&[0, 0, 0, 0]);
        v.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        v.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        v
    }

    pub fn webp_vp8l(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&13u32.to_le_bytes());
        v.extend_from_slice(b"WEBPVP8L");
        v.extend_from_slice(&5u32.to_le_bytes());
        v.push(0x2F);
        let bits = (w - 1) | ((h - 1) << 14);
        v.extend_from_slice(&bits.to_le_bytes());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn accepts_valid_images() {
        let p = validate_image(&png(512, 256)).unwrap();
        assert_eq!((p.kind, p.width, p.height), (ImageKind::Png, 512, 256));
        let j = validate_image(&jpeg(64, 48)).unwrap();
        assert_eq!((j.kind, j.width, j.height), (ImageKind::Jpeg, 64, 48));
        let w = validate_image(&webp_vp8x(1024, 1024)).unwrap();
        assert_eq!((w.kind, w.width, w.height), (ImageKind::Webp, 1024, 1024));
        let l = validate_image(&webp_vp8l(32, 300)).unwrap();
        assert_eq!((l.kind, l.width, l.height), (ImageKind::Webp, 32, 300));
    }

    #[test]
    fn rejects_svg_and_markup() {
        for svg in [
            &b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>"[..],
            b"  <?xml version=\"1.0\"?><svg/>",
            b"\xEF\xBB\xBF<svg/>",
            b"<html><script>x</script></html>",
        ] {
            let e = validate_image(svg).unwrap_err();
            assert!(e.contains("SVG"), "{e}");
        }
    }

    #[test]
    fn rejects_css_js_and_unknown() {
        assert!(validate_image(b"body{background:url(javascript:alert(1))}").is_err());
        assert!(validate_image(b"alert(document.cookie)").is_err());
        assert!(validate_image(b"GIF89a\x01\x00\x01\x00").is_err());
        assert!(validate_image(b"").is_err());
    }

    #[test]
    fn rejects_oversize() {
        let e = validate_image(&png(2048, 100)).unwrap_err();
        assert!(e.contains("maximum is 1024x1024"), "{e}");
        let e = validate_image(&jpeg(100, 1025)).unwrap_err();
        assert!(e.contains("1024"), "{e}");
        let mut big = png(100, 100);
        big.resize(MAX_BYTES + 1, 0);
        let e = validate_image(&big).unwrap_err();
        assert!(e.contains("1 MB"), "{e}");
        assert!(validate_image(&png(8, 8)).is_err());
    }

    #[test]
    fn rejects_truncated() {
        let p = png(100, 100);
        assert!(validate_image(&p[..14]).is_err());
        let j = jpeg(100, 100);
        assert!(validate_image(&j[..8]).is_err());
    }

    #[test]
    fn spoofed_extension_is_irrelevant() {
        // A PNG signature followed by SVG text is still not a valid PNG header.
        let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
        fake.extend_from_slice(b"<svg onload=alert(1)>");
        assert!(validate_image(&fake).is_err());
    }
}
