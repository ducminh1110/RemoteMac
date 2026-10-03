//! The viewer's stream settings, as Moonlight offers them: frame rate, bitrate, sharpness,
//! decoder, frame pacing, pointer. Kept in `%APPDATA%\RemoteMac\settings.json` (next to the
//! log); environment variables still win for testing.

use rm_protocol::Message;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// frames per second the Mac captures and sends
    pub fps: u32,
    /// fixed bitrate in Mbit/s (the Mac never goes above it); 0: Auto (adapts to the link)
    pub bitrate_mbps: u32,
    /// 0 Native (pixels as this screen's, sharpest), 1 Balanced (1 px per Mac point), 2 Fast
    pub quality: u8,
    /// 0 Auto, 1 GPU, 2 CPU (Windows' software decoder)
    pub decoder: u8,
    /// show pictures on the display's refresh (smoother motion, up to a frame more delay)
    pub pacing: bool,
    /// this PC's pointer over the picture too (the Mac's is in the video)
    pub local_cursor: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { fps: 60, bitrate_mbps: 0, quality: 0, decoder: 0, pacing: false, local_cursor: false }
    }
}

pub const FPS: [u32; 5] = [30, 60, 90, 120, 144];
pub const BITRATES: [u32; 8] = [0, 5, 10, 20, 30, 50, 80, 120];
pub const QUALITY: [&str; 3] = ["Native — sharpest (this screen's pixels)", "Balanced (1 pixel per Mac point)", "Fast (lower resolution, least bandwidth)"];
pub const DECODERS: [&str; 3] = ["Auto (GPU when it works)", "GPU (hardware)", "CPU (software)"];

fn path() -> std::path::PathBuf {
    crate::log_path().with_file_name("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        if let Some(v) = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
            let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
            let b = |k: &str| v.get(k).and_then(|x| x.as_bool());
            s.fps = n("fps").map_or(s.fps, |x| x.clamp(10, 144) as u32);
            s.bitrate_mbps = n("bitrate_mbps").map_or(s.bitrate_mbps, |x| x.min(500) as u32);
            s.quality = n("quality").map_or(s.quality, |x| x.min(2) as u8);
            s.decoder = n("decoder").map_or(s.decoder, |x| x.min(2) as u8);
            s.pacing = b("pacing").unwrap_or(s.pacing);
            s.local_cursor = b("local_cursor").unwrap_or(s.local_cursor);
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let v = serde_json::json!({
            "fps": self.fps, "bitrate_mbps": self.bitrate_mbps, "quality": self.quality,
            "decoder": self.decoder, "pacing": self.pacing, "local_cursor": self.local_cursor,
        });
        if let Some(d) = path().parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path(), serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    /// Mac pixels per point for this quality, on a screen of `display_scale`.
    pub fn scale(&self, display_scale: f64) -> f64 {
        match self.quality {
            0 => display_scale.max(1.0),
            1 => 1.0,
            _ => 0.75,
        }
    }

    /// What the Mac needs to know.
    pub fn message(&self, display_scale: f64) -> Message {
        Message::StreamSettings { fps: self.fps, bitrate_kbps: (self.bitrate_mbps > 0).then(|| self.bitrate_mbps * 1000), scale: Some(self.scale(display_scale)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_and_scale() {
        let s = Settings { fps: 120, bitrate_mbps: 30, quality: 1, ..Default::default() };
        assert_eq!(s.message(1.5), Message::StreamSettings { fps: 120, bitrate_kbps: Some(30_000), scale: Some(1.0) });
        assert_eq!(Settings::default().message(2.0), Message::StreamSettings { fps: 60, bitrate_kbps: None, scale: Some(2.0) });
    }
}
