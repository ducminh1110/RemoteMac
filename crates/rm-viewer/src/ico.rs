//! Minimal ICO writer: one 32-bit BGRA image (with alpha) in BMP form, which every Windows
//! version reads. Used for Start-menu shortcut icons of Mac apps.

/// Encode square straight-alpha RGBA pixels as an .ico file.
pub fn rgba_to_ico(size: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if size == 0 || size > 256 || rgba.len() != (size * size * 4) as usize {
        return None;
    }
    let s = size as usize;
    let mask_row = s.div_ceil(32) * 4; // 1-bpp AND mask rows are 32-bit aligned
    let xor_len = s * s * 4;
    let and_len = mask_row * s;
    let bmp_len = 40 + xor_len + and_len;
    let mut out = Vec::with_capacity(6 + 16 + bmp_len);
    // ICONDIR
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    // ICONDIRENTRY (0 in width/height means 256)
    let dim = if size == 256 { 0 } else { size as u8 };
    out.extend_from_slice(&[dim, dim, 0, 0]);
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bit count
    out.extend_from_slice(&(bmp_len as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes()); // offset of image data
    // BITMAPINFOHEADER (height covers XOR + AND masks)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&((xor_len + and_len) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    // XOR bitmap: bottom-up BGRA
    for y in (0..s).rev() {
        for x in 0..s {
            let p = &rgba[(y * s + x) * 4..][..4];
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    // AND mask: all zero (alpha channel decides transparency)
    out.extend(std::iter::repeat_n(0u8, and_len));
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_and_layout() {
        let size = 64u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        rgba[..4].copy_from_slice(&[10, 20, 30, 40]); // top-left pixel
        let ico = rgba_to_ico(size, &rgba).unwrap();
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0]);
        assert_eq!(ico[6], 64);
        let bytes = u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
        assert_eq!(offset, 22);
        assert_eq!(ico.len(), offset + bytes);
        assert_eq!(i32::from_le_bytes(ico[30..34].try_into().unwrap()), 128); // XOR + AND height
        // top-left source pixel is the first pixel of the *last* bottom-up row, stored BGRA
        let last_row = offset + 40 + (63 * 64) * 4;
        assert_eq!(&ico[last_row..last_row + 4], &[30, 20, 10, 40]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(rgba_to_ico(0, &[]).is_none());
        assert!(rgba_to_ico(4, &[0; 10]).is_none());
        assert!(rgba_to_ico(512, &vec![0; 512 * 512 * 4]).is_none());
        assert_eq!(rgba_to_ico(256, &vec![0; 256 * 256 * 4]).unwrap()[6], 0);
    }
}
