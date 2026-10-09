//! This PC's wallpaper, for Desktop Fusion: the picture Windows shows (its file, also when
//! Windows made its own copy of it), how it is fitted to the screen, and the colour around it.

use std::path::PathBuf;
use windows::core::HSTRING;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcWallpaper {
    /// the picture (None: a plain colour)
    pub path: Option<PathBuf>,
    /// "fill", "fit", "stretch", "center", "tile" or "span"
    pub style: String,
    /// "#RRGGBB": the desktop colour around a fitted picture, or the whole desktop
    pub color: String,
    /// what it is made of (path, size, time, style, colour): it changed when this did
    pub key: String,
}

fn reg_sz(key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 260];
    let mut n = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut n)) };
    r.is_ok().then(|| String::from_utf16_lossy(&buf[..(n as usize / 2).saturating_sub(1)]))
}

/// The wallpaper now.
pub fn current() -> PcWallpaper {
    let mut buf = [0u16; 520];
    let ok = unsafe { SystemParametersInfoW(SPI_GETDESKWALLPAPER, buf.len() as u32, Some(buf.as_mut_ptr() as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) }.is_ok();
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let mut path = ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len]))).filter(|p| !p.as_os_str().is_empty() && p.is_file());
    // a picture Windows chose itself (Spotlight, a slideshow): its own copy is what is shown
    if path.is_none() {
        let t = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Microsoft\\Windows\\Themes\\TranscodedWallpaper"));
        path = t.filter(|p| p.is_file() && reg_sz("Control Panel\\Desktop", "WallPaper").is_some_and(|w| !w.is_empty()));
    }
    let tile = reg_sz("Control Panel\\Desktop", "TileWallpaper").is_some_and(|v| v.trim() == "1");
    let style = match reg_sz("Control Panel\\Desktop", "WallpaperStyle").unwrap_or_default().trim() {
        _ if tile => "tile",
        "6" => "fit",
        "2" => "stretch",
        "0" => "center",
        "22" => "span",
        _ => "fill",
    }
    .to_string();
    let rgb: Vec<u8> = reg_sz("Control Panel\\Colors", "Background").unwrap_or_else(|| "0 0 0".into()).split_whitespace().filter_map(|v| v.parse().ok()).collect();
    let color = if rgb.len() == 3 { format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]) } else { "#000000".into() };
    let stamp = path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| format!("{}:{:?}", m.len(), m.modified().ok())).unwrap_or_default();
    let key = format!("{:?}|{stamp}|{style}|{color}", path);
    PcWallpaper { path, style, color, key }
}

/// The name to send the picture under: its kind from its first bytes (Windows' own copy has
/// no extension).
pub fn upload_name(path: &std::path::Path) -> Option<&'static str> {
    use std::io::Read;
    let mut head = [0u8; 12];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let h = &head[..n];
    if h.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("pc-wallpaper.jpg")
    } else if h.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("pc-wallpaper.png")
    } else if h.starts_with(b"BM") {
        Some("pc-wallpaper.bmp")
    } else if h.len() >= 12 && &h[8..12] == b"WEBP" {
        Some("pc-wallpaper.webp")
    } else if h.len() >= 12 && (&h[4..12] == b"ftypheic" || &h[4..12] == b"ftypmif1") {
        Some("pc-wallpaper.heic")
    } else {
        None
    }
}
