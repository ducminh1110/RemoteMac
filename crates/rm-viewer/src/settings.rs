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
    /// 0 Ultra (app windows drawn at 2x on the Mac, scaled down here), 1 Native (pixels as this
    /// screen's), 2 Balanced (1 px per Mac point), 3 Fast
    pub quality: u8,
    /// 0 Auto, 1 GPU, 2 CPU (Windows' software decoder)
    pub decoder: u8,
    /// show pictures on the display's refresh (smoother motion, up to a frame more delay)
    pub pacing: bool,
    /// this PC's pointer over the picture too (the Mac's is in the video)
    pub local_cursor: bool,
    /// Mac Desktop drawn at 2 pixels per point and streamed so (scaled down here), else 1
    pub desktop_2x: bool,
    /// How large the Mac's screen is in points: 0 as this laptop, each step 1/8 more room
    /// (macOS's "More Space" steps): 1920x1200 at 150 % gives 1280x800, 1440x900, 1600x1000...
    pub workspace: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self { fps: 60, bitrate_mbps: 0, quality: 0, decoder: 0, pacing: false, local_cursor: false, desktop_2x: true, workspace: 1 }
    }
}

pub const FPS: [u32; 5] = [30, 60, 90, 120, 144];
pub const BITRATES: [u32; 8] = [0, 5, 10, 20, 30, 50, 80, 120];
pub const QUALITY: [&str; 4] = ["Ultra — sharpest (apps drawn at 2x, scaled down here)", "Native (this screen's pixels)", "Balanced (1 pixel per Mac point)", "Fast (lower resolution, least bandwidth)"];
pub const DESKTOP_SCALES: [&str; 2] = ["1x (lighter on the connection)", "2x (Retina, sharpest)"];
pub const WORKSPACES: usize = 4;
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
            // "sharpness" since Ultra came first; an older "quality" is one step down
            s.quality = n("sharpness").map_or_else(|| n("quality").map_or(s.quality, |x| (x + 1).min(3) as u8), |x| x.min(3) as u8);
            s.decoder = n("decoder").map_or(s.decoder, |x| x.min(2) as u8);
            s.pacing = b("pacing").unwrap_or(s.pacing);
            s.local_cursor = b("local_cursor").unwrap_or(s.local_cursor);
            s.desktop_2x = b("desktop_2x").unwrap_or(s.desktop_2x);
            s.workspace = n("workspace").map_or(s.workspace, |x| x.min(WORKSPACES as u64 - 1) as u8);
        }
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let v = serde_json::json!({
            "fps": self.fps, "bitrate_mbps": self.bitrate_mbps, "sharpness": self.quality,
            "decoder": self.decoder, "pacing": self.pacing, "local_cursor": self.local_cursor, "desktop_2x": self.desktop_2x, "workspace": self.workspace,
        });
        if let Some(d) = path().parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path(), serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    /// Mac pixels per point for this quality, on a screen of `display_scale`.
    pub fn scale(&self, display_scale: f64) -> f64 {
        match self.quality {
            0 => display_scale.max(2.0),
            1 => display_scale.max(1.0),
            2 => 1.0,
            _ => 0.75,
        }
    }

    /// The layout the Mac gives app windows, as `VideoDecoder::screen` ("W,H,S"; "" none) for a
    /// screen of `px` pixels at `display_scale`. Ultra: this screen's size in points, at 2x
    /// whatever this screen's scale (apps render more pixels, the picture is scaled down here);
    /// otherwise 2x only for a HiDPI screen (125% and up), as it is.
    pub fn app_screen(&self, px: (i32, i32), display_scale: f64) -> String {
        if px.0 <= 0 || px.1 <= 0 || (self.quality != 0 && display_scale < 1.25) || self.quality >= 2 {
            return String::new();
        }
        let (w, h) = self.workspace_points(px, display_scale);
        let s = if self.quality == 0 || display_scale >= 1.5 { 2 } else { 1 };
        format!("{},{},{s}", w * s, h * s)
    }

    /// The Mac's screen in points for a screen of `px` pixels at `display_scale`, at workspace
    /// step `self.workspace` (0: as large as this laptop shows things).
    pub fn workspace_points(&self, px: (i32, i32), display_scale: f64) -> (u32, u32) {
        Self::points_at(px, display_scale, self.workspace)
    }

    pub fn points_at(px: (i32, i32), display_scale: f64, step: u8) -> (u32, u32) {
        let s = display_scale.max(1.0) / (1.0 + step.min(WORKSPACES as u8 - 1) as f64 / 8.0);
        let pts = |v: i32| (((v.max(1) as f64 / s) / 2.0).round() as u32 * 2).max(2);
        (pts(px.0), pts(px.1))
    }

    /// The Mac Desktop's display, "W,H,S" (pixels at Mac scale S), for a screen of `px` pixels at
    /// `display_scale`: the Mac's screen in points (the workspace chosen), at 1x or 2x as chosen;
    /// the Mac streams it at exactly that many pixels.
    pub fn desktop_screen(&self, px: (i32, i32), display_scale: f64) -> String {
        let (w, h) = self.workspace_points(px, display_scale);
        let s = if self.desktop_2x { 2 } else { 1 };
        format!("{},{},{s}", w * s, h * s)
    }

    /// What the Mac needs to know (`px`: this screen in pixels).
    pub fn message(&self, display_scale: f64, px: (i32, i32)) -> Message {
        Message::StreamSettings { fps: self.fps, bitrate_kbps: (self.bitrate_mbps > 0).then(|| self.bitrate_mbps * 1000), scale: Some(self.scale(display_scale)), screen: Some(self.app_screen(px, display_scale)), mac_cursor: Some(!self.local_cursor) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_and_scale() {
        let s = Settings { fps: 120, bitrate_mbps: 30, quality: 2, ..Default::default() };
        assert_eq!(s.message(1.5, (1920, 1080)), Message::StreamSettings { fps: 120, bitrate_kbps: Some(30_000), scale: Some(1.0), screen: Some(String::new()), mac_cursor: Some(true) });
        assert_eq!(Settings { workspace: 0, ..Default::default() }.message(2.0, (2560, 1600)), Message::StreamSettings { fps: 60, bitrate_kbps: None, scale: Some(2.0), screen: Some("2560,1600,2".into()), mac_cursor: Some(true) });
    }

    #[test]
    fn app_screen_layouts() {
        let ultra = Settings { workspace: 0, ..Default::default() };
        // a 1920x1080 laptop at 125%: 1536x864 points, drawn at 2x on the Mac
        assert_eq!(ultra.app_screen((1920, 1080), 1.25), "3072,1728,2");
        assert_eq!(ultra.app_screen((1920, 1080), 1.0), "3840,2160,2");
        let native = Settings { quality: 1, workspace: 0, ..Default::default() };
        assert_eq!(native.app_screen((1920, 1080), 1.0), "");
        assert_eq!(native.app_screen((1920, 1080), 1.25), "1536,864,1");
        assert_eq!(native.app_screen((2560, 1600), 2.0), "2560,1600,2");
        assert_eq!(Settings { quality: 3, ..Default::default() }.app_screen((2560, 1600), 2.0), "");
        // the Mac Desktop: the laptop's size in points, at the scale chosen for it
        let laptop = Settings { workspace: 0, ..Default::default() };
        assert_eq!(laptop.desktop_screen((1920, 1080), 1.25), "3072,1728,2");
        assert_eq!(Settings { desktop_2x: false, ..laptop }.desktop_screen((1920, 1080), 1.25), "1536,864,1");
        assert_eq!(Settings { desktop_2x: false, ..laptop }.desktop_screen((2560, 1600), 2.0), "1280,800,1");
        // a step more room (the default): 1920x1200 at 150 % is 1440x900, drawn at 2x
        assert_eq!(Settings::default().desktop_screen((1920, 1200), 1.5), "2880,1800,2");
        assert_eq!(Settings::default().app_screen((1920, 1200), 1.5), "2880,1800,2");
        assert_eq!(Settings::points_at((1920, 1200), 1.5, 2), (1600, 1000));
    }
}
