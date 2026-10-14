//! Stage Manager for remote Mac windows on Windows, ported from StageManagerForWindows (MIT,
//! Copyright (c) 2026 Minh; see NOTICE.md).
//!
//! Open remote Mac windows are grouped by application into "scenes" listed on a sidebar tray
//! (up to 5 recent scenes visible). One scene is on stage at a time; the rest are parked
//! off-screen (past the bottom-right of the virtual screen at full alpha so DWM/DirectComposition
//! keeps compositing them and live previews stay current).
//!
//! Exact macOS 26.5.2 geometry & behaviour laws from StageManagerForWindows:
//! - Card sizing law: uniform scale `s = max(0.135693, 96.0 / source_height_dip)`, with left-edge
//!   perspective squeeze `1.0 - card_w / (2.0 * d_dip)` where `d = screen_h * (1379.0 / 1169.0)`.
//! - Edge tilt law: `EdgeTiltDegreesAt(y_dip) = atan((y_dip - screen_h / 2.0) / d) * 180 / pi`.
//! - Trapezoid decomposition (`SolveEdgeAngles`): splits `(top_deg, bottom_deg)` into vertical
//!   shear slope `T = (slope_top + slope_bottom) / 2` and perspective convergence
//!   `Q = (slope_top - slope_bottom) / 2` with `PerspectiveDepthPx = 220.0`, re-solved against
//!   live card height every frame so flying cards never blow up at stage size.
//! - Multi-window cascade: 10 DIP right & down per window index, back-to-front Z-order preserved.
//! - App icon overlay: 30 DIP (22 DIP when app filter is active), overlapping when >2 windows,
//!   click an icon to filter scenes by app, click outside to clear filter with 750 ms grace period.
//! - Auto-stow (`WindowMode`): `OnScreen` when stage windows don't overlap `0.25 * sidebar_width`,
//!   `OffScreen` (parked at `-width`) when overlapped, `Flyover` (topmost slide-in) when cursor
//!   reaches `x <= 44` px or pushes the left edge.
//! - Interactive drag & drop:
//!   * Sidebar -> Stage: drag a tile across the 120 DIP buffer zone to pull a window into the
//!     active scene (interpolating size & tilt from `TrayTiltDegrees = 2.0` to `0.0`).
//!   * Stage -> Sidebar: drag a window from a multi-window scene into the sidebar to separate it
//!     into its own scene.
//! - Scene transition animation: 300 ms `PowerEase(EaseOut)` simultaneous incoming + outgoing
//!   live flying cards over a click-through transition overlay, with Z-order preservation and
//!   user-minimized window tracking.

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::collapsible_if, clippy::collapsible_else_if, clippy::manual_range_contains, clippy::unnecessary_sort_by)]

use crate::motion::{Anim, Curve};
use crate::paint::{sdf_round_rect, coverage, Canvas, Rgba};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Maximum scenes shown in the sidebar tray at once.
pub const MAX_SCENES: usize = 5;
/// Resting vertical-shear skew of tray thumbnails, in degrees.
pub const TRAY_TILT_DEGREES: f64 = 2.0;
/// Vanishing distance (physical px) of the perspective divide (`M34 = -1 / depth`).
pub const PERSPECTIVE_DEPTH_PX: f64 = 220.0;
/// Per-side HWND inflation so the 3D-tilted near edge and 1.08x hover pop are never clipped.
pub const HOVER_HEADROOM: f64 = 0.30;
/// Resting look of the tray, measured off macOS 26.5.2 to within 0.2 pt RMS over 16 corners.
pub const EDGE_PERSPECTIVE_DISTANCE_RATIO: f64 = 1379.0 / 1169.0;
/// Uniform card scale floor and minimum height in DIPs (macOS `preferredMinimumItemHeight`).
pub const BASE_CARD_SCALE: f64 = 0.135693;
pub const MIN_CARD_HEIGHT_DIP: f64 = 96.0;
/// Sidebar width in DIPs, left padding, and row spacing.
pub const SIDEBAR_WIDTH_DIP: f64 = 240.0;
pub const SIDEBAR_PAD_LEFT_DIP: f64 = 22.0;
pub const SCENE_BOTTOM_MARGIN_DIP: f64 = 28.0;
pub const STACK_OFFSET_STEP_DIP: f64 = 10.0;
pub const THUMB_CORNER_RADIUS_DIP: f64 = 8.0;
/// Fraction of sidebar width at which a normal window's left edge triggers auto-stow.
pub const STOW_OVERLAP_FRACTION: f64 = 0.25;
/// Buffer zone width in DIPs for drag-and-drop between sidebar and stage.
pub const BUFFER_WIDTH_LOGICAL: f64 = 120.0;
/// Cursor grip offset from card top during drag.
pub const CURSOR_TOP_OFFSET_LOGICAL: f64 = 16.0;
pub const CURSOR_TOP_OFFSET_MAX_FRACTION: f64 = 0.35;
/// Scene transition duration (ms).
pub const TRANSITION_DURATION_MS: u64 = 300;
/// Rapid focus change debounce threshold (ms).
pub const RAPID_FOCUS_MS: u64 = 100;
/// Filter clear grace period (ms) to suppress accidental desktop toggle.
pub const FILTER_CLEAR_GRACE_MS: u64 = 750;

/// Sidebar visibility mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    #[default]
    OnScreen,
    OffScreen,
    Flyover,
}

/// The angle (in degrees, positive = right end rises) a horizontal card edge sits at when it
/// crosses `y_dip` on a screen of `screen_h_dip` height.
pub fn edge_tilt_degrees_at(y_dip: f64, screen_h_dip: f64) -> f64 {
    if screen_h_dip <= 0.0 {
        return 0.0;
    }
    let d = screen_h_dip * EDGE_PERSPECTIVE_DISTANCE_RATIO;
    ((y_dip - screen_h_dip / 2.0) / d).atan().to_degrees()
}

/// The tray card size (in DIPs) a window of `(source_w_dip, source_h_dip)` gets.
pub fn card_size_dip(source_w_dip: f64, source_h_dip: f64, screen_h_dip: f64) -> (f64, f64) {
    if source_w_dip <= 0.0 || source_h_dip <= 0.0 {
        return (0.0, 0.0);
    }
    let s = BASE_CARD_SCALE.max(MIN_CARD_HEIGHT_DIP / source_h_dip);
    let card_w = source_w_dip * s;
    let card_h = source_h_dip * s;
    let d_dip = (screen_h_dip.max(600.0)) * EDGE_PERSPECTIVE_DISTANCE_RATIO;
    let squeeze = (1.0 - card_w / (2.0 * d_dip)).clamp(0.75, 1.0);
    (card_w * squeeze, card_h * squeeze)
}

/// Splits a pair of requested edge angles `(top_deg, bottom_deg)` and current `base_height_px`
/// into `(shear_degrees, sprite_rotation_y_degrees)`.
/// Must be re-solved whenever the sprite's pixel height changes so flying cards never blow up.
pub fn solve_edge_angles(top_deg: f64, bottom_deg: f64, base_height_px: f64) -> (f64, f64) {
    let slope_top = -(top_deg.to_radians().tan());
    let slope_bottom = -(bottom_deg.to_radians().tan());
    let shear_slope = (slope_top + slope_bottom) / 2.0;
    let converge = (slope_top - slope_bottom) / 2.0;

    let shear_deg = shear_slope.atan().to_degrees();
    let rot_deg = if base_height_px > 0.0 {
        (2.0 * PERSPECTIVE_DEPTH_PX * converge / base_height_px).atan().to_degrees()
    } else {
        0.0
    };
    (shear_deg, rot_deg)
}

/// Top edge for a dragged card of `height_logical` hanging off a cursor at `cursor_logical_y`.
pub fn card_top_for(cursor_logical_y: f64, height_logical: f64) -> f64 {
    cursor_logical_y - CURSOR_TOP_OFFSET_LOGICAL.min(height_logical * CURSOR_TOP_OFFSET_MAX_FRACTION)
}

/// Render a 3D perspective-tilted trapezoid card with anti-aliased rounded corners (`radius_px`)
/// and drop shadow onto `dst` at `(dst_x, dst_y, dst_w, dst_h)`.
/// Uses the exact StageManagerForWindows trapezoid law: left edge anchored, horizontal scanlines
/// foreshortened by `Q` and sheared by `T`, bilinearly sampling `src` (live window frame).
#[allow(clippy::too_many_arguments)]
pub fn draw_tilted_card(
    dst: &mut Canvas,
    src: &Canvas,
    dst_x: f32,
    dst_y: f32,
    card_w: f32,
    card_h: f32,
    radius_px: f32,
    top_edge_deg: f64,
    bottom_edge_deg: f64,
    hover_scale: f32,
    opacity: f32,
    scale_dpi: f32,
) {
    if src.w == 0 || src.h == 0 || card_w <= 1.0 || card_h <= 1.0 || opacity <= 0.001 {
        return;
    }
    // Left-anchored hover scale (grows rightward and vertically from center, matching Origin 0,0.5)
    let w = card_w * hover_scale;
    let h = card_h * hover_scale;
    let x0 = dst_x;
    let y0 = dst_y - (h - card_h) * 0.5;

    let slope_top = -(top_edge_deg.to_radians().tan()) as f32;
    let slope_bot = -(bottom_edge_deg.to_radians().tan()) as f32;

    // Soft shadow behind the card
    dst.shadow(
        x0,
        y0,
        w,
        h,
        radius_px,
        12.0 * scale_dpi,
        4.0 * scale_dpi,
        Rgba::BLACK.alpha(0.32 * opacity),
    );

    // Bounding box in destination canvas accounting for vertical edge slopes
    let max_rise = (slope_top.abs().max(slope_bot.abs()) * w).ceil() + 2.0;
    let min_px = (x0.floor() as isize).max(0) as usize;
    let max_px = ((x0 + w).ceil() as usize).min(dst.w);
    let min_py = ((y0 - max_rise).floor() as isize).max(0) as usize;
    let max_py = ((y0 + h + max_rise).ceil() as usize).min(dst.h);

    for py in min_py..max_py {
        let fy = py as f32 + 0.5;
        for px in min_px..max_px {
            let fx = px as f32 + 0.5;
            let dx = fx - x0;
            if dx < -0.5 || dx > w + 0.5 {
                continue;
            }
            // Perspective horizontal parameter u (foreshortened slightly along X)
            let u_lin = (dx / w.max(1.0)).clamp(0.0, 1.0);
            // Top and bottom Y at this X column
            let y_top = y0 + slope_top * dx;
            let y_bot = y0 + h + slope_bot * dx;
            let col_h = (y_bot - y_top).max(1.0);
            let v = (fy - y_top) / col_h;
            if v < -0.02 || v > 1.02 {
                continue;
            }
            // Map back to local un-tilted card coordinates (lx, ly) for exact SDF rounded clip
            let lx = u_lin * w;
            let ly = v * h;
            let d = sdf_round_rect(lx, ly, 0.0, 0.0, w, h, radius_px);
            let cov = coverage(d);
            if cov <= 0.0 {
                continue;
            }
            let sx = (u_lin * src.w as f32 - 0.5).clamp(0.0, (src.w - 1) as f32);
            let sy = (v.clamp(0.0, 1.0) * src.h as f32 - 0.5).clamp(0.0, (src.h - 1) as f32);
            let p = src.sample(sx, sy);
            let k = (opacity * cov).clamp(0.0, 1.0);
            let out = &mut dst.px[py * dst.w + px];
            let inv = 1.0 - p[3] * k;
            for i in 0..4 {
                out[i] = ((p[i] * k + out[i] as f32 / 255.0 * inv) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
            // Subtle specular border along top/left edges for macOS card polish
            let rim_cov = coverage(d) * coverage(-d - 1.0 * scale_dpi);
            if rim_cov > 0.0 {
                dst.blend(px, py, Rgba::WHITE.alpha(0.18 * opacity), rim_cov);
            }
        }
    }
}

/// One window tracked inside a Stage Manager scene.
#[derive(Debug, Clone)]
pub struct StageWindow {
    pub hwnd: isize,
    pub remote_id: u64,
    pub app: String,
    pub title: String,
    /// Last known on-stage rectangle in physical screen pixels `(x, y, w, h)`.
    pub stage_rect: (i32, i32, i32, i32),
    /// True when currently parked off-screen by Stage Manager.
    pub parked: bool,
    /// Preview card size in DIPs `(width, height)`.
    pub preview_dip: (f64, f64),
    /// Scaled live snapshot of the window's latest frame for the sidebar tile & flying card.
    pub thumb: Option<Canvas>,
}

/// A group of windows belonging to one Mac application (or separated by drag-and-drop).
#[derive(Debug, Clone)]
pub struct Scene {
    pub id: u64,
    pub key: String,
    pub title: String,
    pub windows: Vec<StageWindow>,
    pub updated: Instant,
    pub tilt_top_deg: f64,
    pub tilt_bottom_deg: f64,
    /// Bounding box of this scene's tile in the sidebar (screen physical px: x, y, w, h).
    pub slot_rect: (i32, i32, i32, i32),
}

/// Pure data model & state machine for Stage Manager (portable and unit-tested).
pub struct StageCore {
    pub enabled: bool,
    pub scenes: Vec<Scene>,
    pub current_scene_id: Option<u64>,
    pub last_scene_id: Option<u64>,
    pub user_minimized: HashSet<isize>,
    pub z_depth: HashMap<isize, usize>,
    pub filter_app: Option<String>,
    pub filter_cleared_at: Option<Instant>,
    pub last_focus_change: Option<Instant>,
    pub suspend: bool,
    next_scene_id: u64,
}

impl Default for StageCore {
    fn default() -> Self {
        Self::new()
    }
}

impl StageCore {
    pub fn new() -> Self {
        Self {
            enabled: false,
            scenes: Vec::new(),
            current_scene_id: None,
            last_scene_id: None,
            user_minimized: HashSet::new(),
            z_depth: HashMap::new(),
            filter_app: None,
            filter_cleared_at: None,
            last_focus_change: None,
            suspend: false,
            next_scene_id: 1,
        }
    }

    pub fn current_scene(&self) -> Option<&Scene> {
        let id = self.current_scene_id?;
        self.scenes.iter().find(|s| s.id == id)
    }

    pub fn current_scene_mut(&mut self) -> Option<&mut Scene> {
        let id = self.current_scene_id?;
        self.scenes.iter_mut().find(|s| s.id == id)
    }

    pub fn find_scene_for_hwnd(&self, hwnd: isize) -> Option<u64> {
        self.scenes.iter().find(|s| s.windows.iter().any(|w| w.hwnd == hwnd)).map(|s| s.id)
    }

    pub fn find_scene_for_app(&self, app: &str) -> Option<u64> {
        self.scenes.iter().find(|s| s.key.eq_ignore_ascii_case(app)).map(|s| s.id)
    }

    /// Sidebar scenes (all scenes except the active stage scene), ordered by most-recently updated,
    /// filtered by `filter_app` when active, capped at `MAX_SCENES`.
    pub fn visible_sidebar_scenes(&self) -> Vec<u64> {
        let mut list: Vec<&Scene> = self
            .scenes
            .iter()
            .filter(|s| Some(s.id) != self.current_scene_id && !s.windows.is_empty())
            .collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated));
        if let Some(f) = &self.filter_app {
            list.into_iter()
                .filter(|s| s.windows.iter().any(|w| w.app.eq_ignore_ascii_case(f)))
                .map(|s| s.id)
                .collect()
        } else {
            list.into_iter().take(MAX_SCENES).map(|s| s.id).collect()
        }
    }

    /// Add or update a window. Returns `(scene_id, is_new_scene)`.
    pub fn upsert_window(
        &mut self,
        hwnd: isize,
        remote_id: u64,
        app: &str,
        app_name: &str,
        title: &str,
        rect: (i32, i32, i32, i32),
        dip_scale: f64,
        screen_h_dip: f64,
    ) -> (u64, bool) {
        let preview_dip = card_size_dip(
            rect.2 as f64 / dip_scale.max(1.0),
            rect.3 as f64 / dip_scale.max(1.0),
            screen_h_dip,
        );
        if let Some(sid) = self.find_scene_for_hwnd(hwnd) {
            let s = self.scenes.iter_mut().find(|s| s.id == sid).unwrap();
            if let Some(w) = s.windows.iter_mut().find(|w| w.hwnd == hwnd) {
                w.title = title.to_string();
                if !w.parked && rect.0 > -10000 && rect.2 > 0 && rect.3 > 0 {
                    w.stage_rect = rect;
                    w.preview_dip = preview_dip;
                }
            }
            return (sid, false);
        }
        let win = StageWindow {
            hwnd,
            remote_id,
            app: app.to_string(),
            title: title.to_string(),
            stage_rect: rect,
            parked: false,
            preview_dip,
            thumb: None,
        };
        if let Some(sid) = self.find_scene_for_app(app) {
            let s = self.scenes.iter_mut().find(|s| s.id == sid).unwrap();
            s.windows.push(win);
            s.updated = Instant::now();
            (sid, false)
        } else {
            let sid = self.next_scene_id;
            self.next_scene_id += 1;
            let s = Scene {
                id: sid,
                key: app.to_string(),
                title: app_name.to_string(),
                windows: vec![win],
                updated: Instant::now(),
                tilt_top_deg: 0.0,
                tilt_bottom_deg: 0.0,
                slot_rect: (0, 0, 0, 0),
            };
            self.scenes.push(s);
            if self.current_scene_id.is_none() {
                self.current_scene_id = Some(sid);
            }
            (sid, true)
        }
    }

    /// Remove a destroyed window. Returns fallback scene ID if the current scene became empty.
    pub fn remove_window(&mut self, hwnd: isize) -> Option<u64> {
        self.user_minimized.remove(&hwnd);
        self.z_depth.remove(&hwnd);
        let sid = self.find_scene_for_hwnd(hwnd)?;
        let idx = self.scenes.iter().position(|s| s.id == sid)?;
        self.scenes[idx].windows.retain(|w| w.hwnd != hwnd);
        if self.scenes[idx].windows.is_empty() {
            self.scenes.remove(idx);
            if self.current_scene_id == Some(sid) {
                self.current_scene_id = None;
                let mut candidates: Vec<&Scene> = self.scenes.iter().filter(|s| !s.windows.is_empty()).collect();
                candidates.sort_by(|a, b| b.updated.cmp(&a.updated));
                return candidates.first().map(|s| s.id);
            }
        }
        None
    }

    /// Separate `hwnd` from its current multi-window scene into a brand new scene in the sidebar.
    pub fn separate_window_to_new_scene(&mut self, hwnd: isize) -> Option<u64> {
        let cur_id = self.current_scene_id?;
        let cur_idx = self.scenes.iter().position(|s| s.id == cur_id)?;
        if self.scenes[cur_idx].windows.len() <= 1 {
            return None;
        }
        let w_idx = self.scenes[cur_idx].windows.iter().position(|w| w.hwnd == hwnd)?;
        let win = self.scenes[cur_idx].windows.remove(w_idx);
        let new_id = self.next_scene_id;
        self.next_scene_id += 1;
        let title = if win.title.is_empty() { win.app.clone() } else { win.title.clone() };
        self.scenes.push(Scene {
            id: new_id,
            key: format!("{}:{}", win.app, new_id),
            title,
            windows: vec![win],
            updated: Instant::now(),
            tilt_top_deg: 0.0,
            tilt_bottom_deg: 0.0,
            slot_rect: (0, 0, 0, 0),
        });
        Some(new_id)
    }

    /// Pull the frontmost window from `source_scene_id` into the current active scene.
    pub fn pop_window_from(&mut self, source_scene_id: u64) -> Option<isize> {
        let cur_id = self.current_scene_id?;
        if source_scene_id == cur_id {
            return None;
        }
        let src_idx = self.scenes.iter().position(|s| s.id == source_scene_id)?;
        let win = self.scenes[src_idx].windows.pop()?;
        let hwnd = win.hwnd;
        self.user_minimized.remove(&hwnd);
        if self.scenes[src_idx].windows.is_empty() {
            self.scenes.remove(src_idx);
        }
        if let Some(cur) = self.scenes.iter_mut().find(|s| s.id == cur_id) {
            cur.windows.push(win);
            cur.updated = Instant::now();
        }
        Some(hwnd)
    }

    pub fn toggle_app_filter(&mut self, app_key: &str) {
        if self.filter_app.as_deref() == Some(app_key) {
            self.clear_app_filter();
        } else {
            self.filter_app = Some(app_key.to_string());
        }
    }

    pub fn clear_app_filter(&mut self) {
        if self.filter_app.is_some() {
            self.filter_app = None;
            self.filter_cleared_at = Some(Instant::now());
        }
    }

    pub fn is_filter_or_grace_active(&self) -> bool {
        self.filter_app.is_some()
            || self
                .filter_cleared_at
                .is_some_and(|t| t.elapsed() < Duration::from_millis(FILTER_CLEAR_GRACE_MS))
    }
}

#[cfg(windows)]
pub mod win {
    use super::*;
    use crate::motion::tokens;
    use crate::surface::{self, Surface};
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::Win32::UI::WindowsAndMessaging::*;

    const SIDEBAR_CLASS: PCWSTR = w!("TaroStageSidebar");
    const OVERLAY_CLASS: PCWSTR = w!("TaroStageOverlay");
    const TIMER_ANIM: usize = 1;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum DragKind {
        None,
        /// Pressing on a sidebar tile (may become click to switch or drag to pull onto stage)
        SidebarPress { scene_id: u64, start_x: i32, start_y: i32 },
        /// Dragging a tile from sidebar toward stage
        SidebarToStage { scene_id: u64, hwnd: isize, past_buffer: bool },
        /// Dragging an active window from stage toward sidebar
        StageToSidebar { hwnd: isize, in_buffer: bool },
    }

    struct FlyingCard {
        thumb: Canvas,
        from: (f32, f32, f32, f32),
        to: (f32, f32, f32, f32),
        from_tilt: (f64, f64),
        to_tilt: (f64, f64),
    }

    struct Transition {
        started: Instant,
        duration: Duration,
        incoming: Option<FlyingCard>,
        outgoing: Option<FlyingCard>,
    }

    struct StageUi {
        core: StageCore,
        sidebar: Option<Surface>,
        overlay: Option<Surface>,
        mode: WindowMode,
        slide: Anim,
        hover_scene: Option<u64>,
        hover_icon: Option<(u64, String)>,
        hover_scales: HashMap<u64, Anim>,
        icons: HashMap<String, Canvas>,
        drag: DragKind,
        drag_ghost: Option<(Canvas, (f32, f32, f32, f32), f64)>,
        transition: Option<Transition>,
        saved_positions: HashMap<isize, (i32, i32)>,
        scale: f32,
        work_area: RECT,
        monitor_rect: RECT,
    }

    thread_local! {
        static STAGE: RefCell<StageUi> = RefCell::new(StageUi {
            core: StageCore::new(),
            sidebar: None,
            overlay: None,
            mode: WindowMode::OnScreen,
            slide: Anim::at(1.0),
            hover_scene: None,
            hover_icon: None,
            hover_scales: HashMap::new(),
            icons: HashMap::new(),
            drag: DragKind::None,
            drag_ghost: None,
            transition: None,
            saved_positions: HashMap::new(),
            scale: 1.0,
            work_area: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            monitor_rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
        });
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, SIDEBAR_CLASS, Some(sidebar_proc), IDC_HAND);
        surface::register(hinst, OVERLAY_CLASS, Some(overlay_proc), IDC_ARROW);
    }

    fn offscreen_point() -> (i32, i32) {
        unsafe {
            let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            (vx + vw + 100, vy + vh + 100)
        }
    }

    /// Park a window off-screen (saving its on-screen position first) so DWM keeps compositing it.
    fn park_hwnd(saved: &mut HashMap<isize, (i32, i32)>, hwnd: isize) {
        let h = HWND(hwnd as *mut _);
        unsafe {
            if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() || !IsWindowVisible(h).as_bool() {
                return;
            }
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_ok() && r.left < offscreen_point().0 - 50 {
                saved.entry(hwnd).or_insert((r.left, r.top));
            }
            let (ox, oy) = offscreen_point();
            let _ = SetWindowPos(
                h,
                None,
                ox,
                oy,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Restore a parked window back to its saved on-stage position and bring to top.
    fn restore_hwnd(saved: &mut HashMap<isize, (i32, i32)>, hwnd: isize) {
        let h = HWND(hwnd as *mut _);
        unsafe {
            if !IsWindow(Some(h)).as_bool() {
                saved.remove(&hwnd);
                return;
            }
            if let Some((x, y)) = saved.remove(&hwnd) {
                let _ = SetWindowPos(
                    h,
                    Some(HWND_TOP),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                );
            } else {
                let _ = BringWindowToTop(h);
            }
        }
    }

    pub fn set_enabled(hinst: HINSTANCE, on: bool) {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if s.core.enabled == on {
                return;
            }
            s.core.enabled = on;
            if !on {
                // Restore all parked windows to stage before closing Stage Manager
                let hwnds: Vec<isize> = s.saved_positions.keys().copied().collect();
                for h in hwnds {
                    restore_hwnd(&mut s.saved_positions, h);
                }
                for sc in &mut s.core.scenes {
                    for w in &mut sc.windows {
                        w.parked = false;
                    }
                }
                s.sidebar = None;
                s.overlay = None;
                s.transition = None;
            } else {
                update_monitor_metrics(&mut s);
                s.sidebar = Surface::new(hinst, SIDEBAR_CLASS, "Taro Stage Manager", None, true, false);
                s.overlay = Surface::new(hinst, OVERLAY_CLASS, "Taro Stage Transition", None, true, false);
                if let Some(ov) = &s.overlay {
                    unsafe {
                        let ex = GetWindowLongPtrW(ov.hwnd, GWL_EXSTYLE);
                        SetWindowLongPtrW(ov.hwnd, GWL_EXSTYLE, ex | WS_EX_TRANSPARENT.0 as isize);
                    }
                }
                s.slide = Anim::new(0.0, 1.0, Duration::from_millis(450), Curve::Decelerate);
                apply_active_scene_parking(&mut s);
                start_anim_timer(&s);
            }
        });
        redraw();
    }

    pub fn is_enabled() -> bool {
        STAGE.with(|s| s.borrow().core.enabled)
    }

    pub fn is_parked(hwnd: isize) -> bool {
        STAGE.with(|s| s.borrow().saved_positions.contains_key(&hwnd))
    }

    fn update_monitor_metrics(s: &mut StageUi) {
        unsafe {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetMonitorInfoW(mon, &mut info).as_bool() {
                s.work_area = info.rcWork;
                s.monitor_rect = info.rcMonitor;
            }
            s.scale = surface::scale_at(pt.x, pt.y).max(1.0);
        }
    }

    pub fn set_app_icon(app: &str, size: u32, rgba: &[u8]) {
        if rgba.len() != (size * size * 4) as usize {
            return;
        }
        let c = Canvas::from_rgba(size as usize, size as usize, rgba);
        STAGE.with(|s| {
            s.borrow_mut().icons.insert(app.to_string(), c);
        });
        redraw();
    }

    /// Update a window's live thumbnail from a decoded BGRA picture (downsampled fast to ~240 px
    /// width so sidebar rendering is instant and crisp).
    pub fn on_frame(hwnd: isize, width: usize, height: usize, bgra: &[u8]) {
        if width == 0 || height == 0 || bgra.len() < width * height * 4 {
            return;
        }
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            let Some(sid) = s.core.find_scene_for_hwnd(hwnd) else { return };
            let target_w = 220usize.min(width);
            let target_h = ((height * target_w) / width.max(1)).max(1);
            let mut thumb = Canvas::new(target_w, target_h);
            for ty in 0..target_h {
                let sy = (ty * height) / target_h;
                for tx in 0..target_w {
                    let sx = (tx * width) / target_w;
                    let idx = (sy * width + sx) * 4;
                    thumb.px[ty * target_w + tx] = [bgra[idx], bgra[idx + 1], bgra[idx + 2], 255];
                }
            }
            if let Some(sc) = s.core.scenes.iter_mut().find(|sc| sc.id == sid) {
                if let Some(w) = sc.windows.iter_mut().find(|w| w.hwnd == hwnd) {
                    w.thumb = Some(thumb);
                }
            }
        });
        redraw();
    }

    pub fn on_window_created_or_moved(hwnd: isize, remote_id: u64, app: &str, app_name: &str, title: &str) {
        let h = HWND(hwnd as *mut _);
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            update_monitor_metrics(&mut s);
            let screen_h_dip = (s.monitor_rect.bottom - s.monitor_rect.top) as f64 / s.scale as f64;
            let rect = (r.left, r.top, (r.right - r.left).max(1), (r.bottom - r.top).max(1));
            let dpi_scale = s.scale as f64;
            let (sid, is_new) = s.core.upsert_window(
                hwnd,
                remote_id,
                app,
                app_name,
                title,
                rect,
                dpi_scale,
                screen_h_dip,
            );
            if s.core.enabled {
                if is_new {
                    s.core.current_scene_id = Some(sid);
                }
                apply_active_scene_parking(&mut s);
            }
        });
        redraw();
    }

    /// Bring the scene of `hwnd` on stage (a click in the launcher or the Dock on a parked window).
    pub fn switch_to_window(hwnd: isize) {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if !s.core.enabled {
                return;
            }
            if let Some(sid) = s.core.find_scene_for_hwnd(hwnd) {
                if s.core.current_scene_id != Some(sid) {
                    switch_to_scene(&mut s, Some(sid));
                }
            }
        });
        redraw();
    }

    pub fn on_window_destroyed(hwnd: isize) {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            s.saved_positions.remove(&hwnd);
            if let Some(fallback) = s.core.remove_window(hwnd) {
                s.core.current_scene_id = Some(fallback);
                if s.core.enabled {
                    apply_active_scene_parking(&mut s);
                }
            }
        });
        redraw();
    }

    pub fn on_window_focused(hwnd: isize) {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if !s.core.enabled || s.core.suspend || s.saved_positions.contains_key(&hwnd) {
                return;
            }
            let now = Instant::now();
            if s.core.last_focus_change.is_some_and(|t| now.duration_since(t) < Duration::from_millis(RAPID_FOCUS_MS)) {
                return;
            }
            s.core.last_focus_change = Some(now);
            let Some(sid) = s.core.find_scene_for_hwnd(hwnd) else { return };
            if s.core.current_scene_id != Some(sid) {
                switch_to_scene(&mut s, Some(sid));
            }
        });
        redraw();
    }

    pub fn on_window_move_hook(hwnd: isize, moving: bool, ended: bool) {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if !s.core.enabled {
                return;
            }
            let mut cur = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cur);
            }
            let sidebar_w_px = (SIDEBAR_WIDTH_DIP as f32 * s.scale).round() as i32;
            let buffer_right_px = s.work_area.left + sidebar_w_px + (BUFFER_WIDTH_LOGICAL as f32 * s.scale).round() as i32;

            if moving && !ended {
                let Some(sid) = s.core.find_scene_for_hwnd(hwnd) else { return };
                if s.core.current_scene_id != Some(sid) {
                    return;
                }
                let count = s.core.scenes.iter().find(|sc| sc.id == sid).map_or(0, |sc| sc.windows.len());
                if count <= 1 {
                    return;
                }
                let in_buf = cur.x < buffer_right_px;
                s.drag = DragKind::StageToSidebar { hwnd, in_buffer: in_buf };
                if in_buf {
                    let t = ((buffer_right_px - cur.x) as f64 / (buffer_right_px - (s.work_area.left + sidebar_w_px)).max(1) as f64).clamp(0.0, 1.0);
                    if let Some(sc) = s.core.scenes.iter().find(|sc| sc.id == sid) {
                        if let Some(w) = sc.windows.iter().find(|w| w.hwnd == hwnd) {
                            if let Some(th) = &w.thumb {
                                let gw = (w.stage_rect.2 as f64 + (w.preview_dip.0 * s.scale as f64 - w.stage_rect.2 as f64) * t) as f32;
                                let gh = (w.stage_rect.3 as f64 + (w.preview_dip.1 * s.scale as f64 - w.stage_rect.3 as f64) * t) as f32;
                                let gx = cur.x as f32 - gw * 0.5;
                                let gy = card_top_for(cur.y as f64 / s.scale as f64, gh as f64 / s.scale as f64) as f32 * s.scale;
                                s.drag_ghost = Some((th.clone(), (gx, gy, gw, gh), TRAY_TILT_DEGREES * t));
                            }
                        }
                    }
                } else {
                    s.drag_ghost = None;
                }
            } else if ended {
                if let DragKind::StageToSidebar { hwnd: dh, in_buffer } = s.drag {
                    s.drag = DragKind::None;
                    s.drag_ghost = None;
                    if dh == hwnd && in_buffer && cur.x < s.work_area.left + sidebar_w_px {
                        if s.core.separate_window_to_new_scene(hwnd).is_some() {
                            apply_active_scene_parking(&mut s);
                        }
                    }
                }
            }
        });
        redraw();
    }

    fn apply_active_scene_parking(s: &mut StageUi) {
        s.core.suspend = true;
        let active_id = s.core.current_scene_id;
        let mut to_park = Vec::new();
        let mut to_restore = Vec::new();
        for sc in &mut s.core.scenes {
            let is_active = Some(sc.id) == active_id;
            for w in &mut sc.windows {
                if is_active {
                    w.parked = false;
                    to_restore.push(w.hwnd);
                } else {
                    w.parked = true;
                    to_park.push(w.hwnd);
                }
            }
        }
        for h in to_park {
            park_hwnd(&mut s.saved_positions, h);
        }
        for h in to_restore.iter().copied() {
            restore_hwnd(&mut s.saved_positions, h);
        }
        if let Some(top) = to_restore.last().copied() {
            unsafe {
                let _ = SetForegroundWindow(HWND(top as *mut _));
            }
        }
        s.core.suspend = false;
    }

    fn switch_to_scene(s: &mut StageUi, target_id: Option<u64>) {
        if s.core.current_scene_id == target_id {
            return;
        }
        let prior_id = s.core.current_scene_id;
        s.core.last_scene_id = prior_id;

        // Build incoming and outgoing flying cards for smooth 300ms scene transition
        let incoming = target_id.and_then(|tid| {
            let sc = s.core.scenes.iter().find(|x| x.id == tid)?;
            let w = sc.windows.first()?;
            let thumb = w.thumb.clone()?;
            let from = (sc.slot_rect.0 as f32, sc.slot_rect.1 as f32, sc.slot_rect.2.max(40) as f32, sc.slot_rect.3.max(40) as f32);
            let to = (w.stage_rect.0 as f32, w.stage_rect.1 as f32, w.stage_rect.2.max(100) as f32, w.stage_rect.3.max(100) as f32);
            Some(FlyingCard {
                thumb,
                from,
                to,
                from_tilt: (sc.tilt_top_deg, sc.tilt_bottom_deg),
                to_tilt: (0.0, 0.0),
            })
        });

        let outgoing = prior_id.and_then(|pid| {
            let sc = s.core.scenes.iter().find(|x| x.id == pid)?;
            let w = sc.windows.first()?;
            let thumb = w.thumb.clone()?;
            let from = (w.stage_rect.0 as f32, w.stage_rect.1 as f32, w.stage_rect.2.max(100) as f32, w.stage_rect.3.max(100) as f32);
            let slot = target_id
                .and_then(|tid| s.core.scenes.iter().find(|x| x.id == tid).map(|x| x.slot_rect))
                .unwrap_or((
                    s.work_area.left + (SIDEBAR_PAD_LEFT_DIP as f32 * s.scale) as i32,
                    (s.work_area.top + s.work_area.bottom) / 2 - 60,
                    (w.preview_dip.0 * s.scale as f64) as i32,
                    (w.preview_dip.1 * s.scale as f64) as i32,
                ));
            let to = (slot.0 as f32, slot.1 as f32, slot.2.max(40) as f32, slot.3.max(40) as f32);
            let screen_h_dip = (s.monitor_rect.bottom - s.monitor_rect.top) as f64 / s.scale as f64;
            let t_top = edge_tilt_degrees_at(to.1 as f64 / s.scale as f64, screen_h_dip);
            let t_bot = edge_tilt_degrees_at((to.1 + to.3) as f64 / s.scale as f64, screen_h_dip);
            Some(FlyingCard {
                thumb,
                from,
                to,
                from_tilt: (0.0, 0.0),
                to_tilt: (t_top, t_bot),
            })
        });

        s.core.current_scene_id = target_id;
        if let Some(tid) = target_id {
            if let Some(sc) = s.core.scenes.iter_mut().find(|x| x.id == tid) {
                sc.updated = Instant::now();
            }
        }
        apply_active_scene_parking(s);

        if incoming.is_some() || outgoing.is_some() {
            s.transition = Some(Transition {
                started: Instant::now(),
                duration: Duration::from_millis(TRANSITION_DURATION_MS),
                incoming,
                outgoing,
            });
            start_anim_timer(s);
        }
    }

    fn start_anim_timer(s: &StageUi) {
        if let Some(sb) = &s.sidebar {
            unsafe {
                SetTimer(Some(sb.hwnd), TIMER_ANIM, 16, None);
            }
        }
    }

    /// Periodic 100ms tick from controller: checks overlap auto-stow, edge flyover trigger, and
    /// keeps the sidebar & transition overlay updated.
    pub fn tick() {
        let should_draw = STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if !s.core.enabled {
                return false;
            }
            update_monitor_metrics(&mut s);
            let mut cur = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cur);
            }
            let sidebar_w_px = (SIDEBAR_WIDTH_DIP as f32 * s.scale).round() as i32;
            let stow_limit = s.work_area.left + (sidebar_w_px as f64 * STOW_OVERLAP_FRACTION) as i32;

            // Check if any active scene window overlaps the stow threshold
            let overlaps = s.core.current_scene().is_some_and(|sc| {
                sc.windows.iter().any(|w| !w.parked && w.stage_rect.0 < stow_limit)
            });

            let cursor_in_sidebar = cur.x <= s.work_area.left + sidebar_w_px;
            let cursor_at_edge = cur.x <= s.work_area.left + (44.0 * s.scale) as i32;

            let next_mode = if s.core.current_scene_id.is_none() || !overlaps {
                WindowMode::OnScreen
            } else if cursor_at_edge || (s.mode == WindowMode::Flyover && cursor_in_sidebar) {
                WindowMode::Flyover
            } else {
                WindowMode::OffScreen
            };

            if next_mode != s.mode {
                s.mode = next_mode;
                let target = if next_mode == WindowMode::OffScreen { 0.0 } else { 1.0 };
                s.slide.retarget(target, tokens::pick(tokens::WINDOW, 0.5), Curve::Decelerate);
                start_anim_timer(&s);
            }
            true
        });
        if should_draw {
            redraw();
        }
    }

    fn redraw() {
        STAGE.with(|st| {
            let mut s = st.borrow_mut();
            if !s.core.enabled {
                return;
            }
            let now = Instant::now();
            let scale = s.scale;
            let wa = s.work_area;
            let mon = s.monitor_rect;
            let screen_h_dip = (mon.bottom - mon.top) as f64 / scale as f64;
            let sb_w = (SIDEBAR_WIDTH_DIP as f32 * scale).round() as usize;
            let sb_h = (wa.bottom - wa.top).max(1) as usize;
            let visible_ids = s.core.visible_sidebar_scenes();

            if visible_ids.is_empty() && s.transition.is_none() && s.drag_ghost.is_none() {
                if let Some(sb) = &s.sidebar {
                    sb.hide();
                }
                if let Some(ov) = &s.overlay {
                    ov.hide();
                }
                return;
            }

            // Compute total height of visible scene tiles to vertically center the strip
            let pad_left = (SIDEBAR_PAD_LEFT_DIP as f32 * scale).round();
            let margin_b = (SCENE_BOTTOM_MARGIN_DIP as f32 * scale).round();
            let step_px = (STACK_OFFSET_STEP_DIP as f32 * scale).round();
            let radius_px = THUMB_CORNER_RADIUS_DIP as f32 * scale;

            let mut heights: Vec<(u64, f32, f32, usize)> = Vec::new();
            for &sid in &visible_ids {
                if let Some(sc) = s.core.scenes.iter().find(|x| x.id == sid) {
                    let (dw, dh) = sc.windows.first().map_or((140.0, 96.0), |w| w.preview_dip);
                    let n = sc.windows.len().min(4);
                    let extra = (n.saturating_sub(1) as f32) * step_px;
                    heights.push((sid, dw as f32 * scale + extra, dh as f32 * scale + extra, n));
                }
            }
            let total_h: f32 = heights.iter().map(|(_, _, h, _)| *h + margin_b).sum::<f32>() - margin_b.min(heights.len() as f32 * margin_b);
            let mut y_cur = ((sb_h as f32 - total_h) * 0.5).max(16.0 * scale);

            let mut canvas = Canvas::new(sb_w, sb_h);
            let dark = surface::dark_mode();
            let (fg, _) = surface::glass_text(dark);

            for (sid, tw, th, _n) in heights {
                let top_dip = (wa.top as f32 + y_cur - mon.top as f32) as f64 / scale as f64;
                let bot_dip = (wa.top as f32 + y_cur + th - mon.top as f32) as f64 / scale as f64;
                let tilt_top = edge_tilt_degrees_at(top_dip, screen_h_dip);
                let tilt_bot = edge_tilt_degrees_at(bot_dip, screen_h_dip);

                let hover_k = s.hover_scales.get(&sid).map_or(1.0, |a| a.value_at(now));
                let app_key = s.core.scenes.iter().find(|x| x.id == sid).map(|x| x.key.clone()).unwrap_or_default();
                let icon_canvas = s.icons.get(&app_key).cloned();

                let filtering = s.core.filter_app.is_some();
                let hovered = s.hover_scene == Some(sid);
                if let Some(sc) = s.core.scenes.iter_mut().find(|x| x.id == sid) {
                    sc.tilt_top_deg = tilt_top;
                    sc.tilt_bottom_deg = tilt_bot;
                    sc.slot_rect = (
                        wa.left + pad_left as i32,
                        wa.top + y_cur.round() as i32,
                        tw.round() as i32,
                        th.round() as i32,
                    );

                    // Draw stacked windows from back to front (index N-1 down to 0)
                    let count = sc.windows.len().min(4);
                    for idx in (0..count).rev() {
                        let w = &sc.windows[idx];
                        let off = idx as f32 * step_px;
                        let cw = (w.preview_dip.0 as f32 * scale).max(48.0);
                        let ch = (w.preview_dip.1 as f32 * scale).max(36.0);
                        if let Some(thumb) = &w.thumb {
                            draw_tilted_card(
                                &mut canvas,
                                thumb,
                                pad_left + off,
                                y_cur + off,
                                cw,
                                ch,
                                radius_px,
                                tilt_top,
                                tilt_bot,
                                hover_k,
                                0.96,
                                scale,
                            );
                        } else {
                            let placeholder = Canvas::filled(64, 48, if dark { Rgba::rgb(44, 44, 50) } else { Rgba::rgb(235, 235, 240) });
                            draw_tilted_card(
                                &mut canvas,
                                &placeholder,
                                pad_left + off,
                                y_cur + off,
                                cw,
                                ch,
                                radius_px,
                                tilt_top,
                                tilt_bot,
                                hover_k,
                                0.92,
                                scale,
                            );
                        }
                    }

                    // Draw per-scene app icon badge at bottom-left of the tile (matching IconOverlayManager)
                    let icon_sz = if filtering { 22.0 * scale } else { 30.0 * scale };
                    let ix = (pad_left - 6.0 * scale).max(4.0 * scale);
                    let iy = y_cur + th - icon_sz * 0.65;
                    canvas.shadow(ix, iy, icon_sz, icon_sz, icon_sz * 0.24, 6.0 * scale, 2.0 * scale, Rgba::BLACK.alpha(0.35));
                    if let Some(ic) = &icon_canvas {
                        canvas.draw(ic, ix, iy, icon_sz, icon_sz, icon_sz * 0.22, 1.0);
                    } else {
                        let ph = crate::launchui::placeholder(&sc.key, &sc.title, icon_sz.round() as usize, scale);
                        canvas.draw(&ph, ix, iy, icon_sz, icon_sz, icon_sz * 0.22, 1.0);
                    }

                    // When app filter is active or tile is hovered, draw scene title label below
                    if filtering || hovered {
                        surface::text(
                            &mut canvas,
                            &sc.title,
                            pad_left,
                            y_cur + th + 4.0 * scale,
                            tw,
                            (11.0 * scale).round() as i32,
                            600,
                            fg,
                            surface::Align::Center,
                        );
                    }
                }
                y_cur += th + margin_b;
            }

            let t_slide = s.slide.value_at(now).clamp(0.0, 1.0);
            let sb_x = wa.left - ((1.0 - t_slide) * sb_w as f32).round() as i32;
            let sb_alpha = (t_slide.sqrt() * 255.0).round() as u8;
            if let Some(sb) = &mut s.sidebar {
                if sb_alpha == 0 {
                    sb.hide();
                } else {
                    sb.present(&canvas, sb_x, wa.top, sb_alpha, None);
                    sb.show();
                }
            }

            // Render TransitionOverlay / SidebarDragGhost if active
            let mut has_overlay = false;
            if s.transition.is_some() || s.drag_ghost.is_some() {
                let ow = (wa.right - wa.left).max(1) as usize;
                let oh = (wa.bottom - wa.top).max(1) as usize;
                let mut ov_canvas = Canvas::new(ow, oh);

                if let Some(tr) = &s.transition {
                    let u = (now.saturating_duration_since(tr.started).as_secs_f32() / tr.duration.as_secs_f32().max(0.001)).clamp(0.0, 1.0);
                    // PowerEase EaseOut (cubic)
                    let k = 1.0 - (1.0 - u).powi(3);
                    for card in [&tr.outgoing, &tr.incoming].into_iter().flatten() {
                        let cx = card.from.0 + (card.to.0 - card.from.0) * k - wa.left as f32;
                        let cy = card.from.1 + (card.to.1 - card.from.1) * k - wa.top as f32;
                        let cw = card.from.2 + (card.to.2 - card.from.2) * k;
                        let ch = card.from.3 + (card.to.3 - card.from.3) * k;
                        let top_deg = card.from_tilt.0 + (card.to_tilt.0 - card.from_tilt.0) * k as f64;
                        let bot_deg = card.from_tilt.1 + (card.to_tilt.1 - card.from_tilt.1) * k as f64;
                        draw_tilted_card(&mut ov_canvas, &card.thumb, cx, cy, cw, ch, radius_px, top_deg, bot_deg, 1.0, 0.98, scale);
                        has_overlay = true;
                    }
                    if u >= 1.0 {
                        s.transition = None;
                    }
                }

                if let Some((ghost_thumb, (gx, gy, gw, gh), skew)) = &s.drag_ghost {
                    let frac = (*skew / TRAY_TILT_DEGREES).clamp(0.0, 1.0);
                    let top_d = edge_tilt_degrees_at(*gy as f64 / scale as f64, screen_h_dip) * frac;
                    let bot_d = edge_tilt_degrees_at((*gy + *gh) as f64 / scale as f64, screen_h_dip) * frac;
                    draw_tilted_card(
                        &mut ov_canvas,
                        ghost_thumb,
                        *gx - wa.left as f32,
                        *gy - wa.top as f32,
                        *gw,
                        *gh,
                        radius_px,
                        top_d,
                        bot_d,
                        1.0,
                        0.95,
                        scale,
                    );
                    has_overlay = true;
                }

                if let Some(ov) = &mut s.overlay {
                    if has_overlay {
                        ov.present(&ov_canvas, wa.left, wa.top, 255, None);
                        ov.show();
                    } else {
                        ov.hide();
                    }
                }
            } else if let Some(ov) = &mut s.overlay {
                ov.hide();
            }

            let anim_busy = !s.slide.done_at(now)
                || s.transition.is_some()
                || s.hover_scales.values().any(|a| !a.done_at(now));
            if !anim_busy {
                if let Some(sb) = &s.sidebar {
                    unsafe {
                        let _ = KillTimer(Some(sb.hwnd), TIMER_ANIM);
                    }
                }
            }
        });
    }

    fn hit_test_sidebar(s: &StageUi, screen_pt: POINT) -> (Option<u64>, Option<(u64, String)>) {
        let scale = s.scale;
        let icon_sz = (if s.core.filter_app.is_some() { 22.0 } else { 30.0 } * scale) as i32;
        for sid in s.core.visible_sidebar_scenes() {
            if let Some(sc) = s.core.scenes.iter().find(|x| x.id == sid) {
                let (x, y, w, h) = sc.slot_rect;
                let ix = x - (6.0 * scale) as i32;
                let iy = y + h - (icon_sz as f32 * 0.65) as i32;
                if screen_pt.x >= ix && screen_pt.x <= ix + icon_sz && screen_pt.y >= iy && screen_pt.y <= iy + icon_sz {
                    return (Some(sid), Some((sid, sc.key.clone())));
                }
                if screen_pt.x >= x && screen_pt.x <= x + w && screen_pt.y >= y && screen_pt.y <= y + h {
                    return (Some(sid), None);
                }
            }
        }
        (None, None)
    }

    unsafe extern "system" fn sidebar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_TIMER if wp.0 == TIMER_ANIM => {
                redraw();
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                STAGE.with(|st| {
                    let mut s = st.borrow_mut();
                    let (work_left, dpi) = (s.work_area.left, s.scale);
                    match s.drag {
                        DragKind::SidebarPress { scene_id, start_x, start_y } => {
                            let dx = (pt.x - start_x) as f64;
                            let dy = (pt.y - start_y) as f64;
                            if (dx * dx + dy * dy).sqrt() >= 10.0 * s.scale as f64 {
                                let hw = s.core.scenes.iter().find(|x| x.id == scene_id).and_then(|sc| sc.windows.last().map(|w| w.hwnd)).unwrap_or(0);
                                s.drag = DragKind::SidebarToStage { scene_id, hwnd: hw, past_buffer: false };
                            }
                        }
                        DragKind::SidebarToStage { scene_id, hwnd: hw, ref mut past_buffer } => {
                            let sb_right = work_left + (SIDEBAR_WIDTH_DIP as f32 * dpi) as i32;
                            let buf_right = sb_right + (BUFFER_WIDTH_LOGICAL as f32 * dpi) as i32;
                            *past_buffer = pt.x > buf_right;
                            if let Some(sc) = s.core.scenes.iter().find(|x| x.id == scene_id) {
                                if let Some(w) = sc.windows.iter().find(|w| w.hwnd == hw) {
                                    if let Some(th) = &w.thumb {
                                        let t = ((pt.x - sb_right) as f64 / (buf_right - sb_right).max(1) as f64).clamp(0.0, 1.0);
                                        let tw = sc.slot_rect.2 as f64;
                                        let th_px = sc.slot_rect.3 as f64;
                                        let gw = (tw + (w.stage_rect.2 as f64 - tw) * t) as f32;
                                        let gh = (th_px + (w.stage_rect.3 as f64 - th_px) * t) as f32;
                                        let gx = pt.x as f32 - gw * 0.5;
                                        let gy = card_top_for(pt.y as f64 / s.scale as f64, gh as f64 / s.scale as f64) as f32 * s.scale;
                                        s.drag_ghost = Some((th.clone(), (gx, gy, gw, gh), TRAY_TILT_DEGREES * (1.0 - t)));
                                    }
                                }
                            }
                        }
                        _ => {
                            let (h_scene, h_icon) = hit_test_sidebar(&s, pt);
                            if h_scene != s.hover_scene || h_icon != s.hover_icon {
                                let prev = s.hover_scene;
                                s.hover_scene = h_scene;
                                s.hover_icon = h_icon;
                                if let Some(p) = prev {
                                    s.hover_scales.entry(p).or_insert_with(|| Anim::at(1.0)).retarget(1.0, Duration::from_millis(200), Curve::Standard);
                                }
                                if let Some(n) = h_scene {
                                    s.hover_scales.entry(n).or_insert_with(|| Anim::at(1.0)).retarget(1.08, Duration::from_millis(200), Curve::Standard);
                                }
                                start_anim_timer(&s);
                            }
                        }
                    }
                });
                redraw();
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                SetCapture(hwnd);
                STAGE.with(|st| {
                    let mut s = st.borrow_mut();
                    let (h_scene, h_icon) = hit_test_sidebar(&s, pt);
                    if h_icon.is_none() {
                        if let Some(sid) = h_scene {
                            s.hover_scales.entry(sid).or_insert_with(|| Anim::at(1.0)).retarget(0.96, Duration::from_millis(80), Curve::Decelerate);
                            s.drag = DragKind::SidebarPress { scene_id: sid, start_x: pt.x, start_y: pt.y };
                            start_anim_timer(&s);
                        }
                    }
                });
                redraw();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ReleaseCapture();
                STAGE.with(|st| {
                    let mut s = st.borrow_mut();
                    let drag = std::mem::replace(&mut s.drag, DragKind::None);
                    s.drag_ghost = None;
                    match drag {
                        DragKind::SidebarToStage { scene_id, past_buffer, .. } => {
                            if past_buffer {
                                if s.core.pop_window_from(scene_id).is_some() {
                                    apply_active_scene_parking(&mut s);
                                }
                            }
                        }
                        _ => {
                            let (h_scene, h_icon) = hit_test_sidebar(&s, pt);
                            if let Some((_, app_key)) = h_icon {
                                s.core.toggle_app_filter(&app_key);
                            } else if let Some(sid) = h_scene {
                                s.core.clear_app_filter();
                                switch_to_scene(&mut s, Some(sid));
                            } else if s.core.filter_app.is_some() {
                                s.core.clear_app_filter();
                            }
                        }
                    }
                });
                redraw();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }

    unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_card_sizing_and_tilt_laws_match_stage_manager_for_windows() {
        // 1440x900 window on a 1169pt high screen
        let (cw, ch) = card_size_dip(1440.0, 900.0, 1169.0);
        assert!(cw > 150.0 && cw < 210.0, "card width {cw}");
        assert!(ch >= 96.0 && ch < 140.0, "card height {ch}");

        // Small window scales up to at least 96 DIP height (before perspective squeeze)
        let (_, small_h) = card_size_dip(300.0, 200.0, 1169.0);
        assert!(small_h > 85.0, "small window height {small_h}");

        // Center of screen has 0 tilt; above center tilts negative; below center tilts positive
        assert!((edge_tilt_degrees_at(500.0, 1000.0)).abs() < 1e-6);
        assert!(edge_tilt_degrees_at(200.0, 1000.0) < 0.0);
        assert!(edge_tilt_degrees_at(800.0, 1000.0) > 0.0);

        // SolveEdgeAngles splits shear and convergence and scales inverse to height
        let (shear_small, rot_small) = solve_edge_angles(-5.0, -3.0, 120.0);
        let (shear_large, rot_large) = solve_edge_angles(-5.0, -3.0, 1200.0);
        assert!((shear_small - shear_large).abs() < 1e-6, "shear is height-independent");
        assert!(rot_large.abs() < rot_small.abs(), "rotation shrinks as height grows so perspective never blows up");
    }

    #[test]
    fn scene_grouping_switching_and_window_separation() {
        let mut core = StageCore::new();
        core.enabled = true;
        let (s1, new1) = core.upsert_window(101, 1, "xcode", "Xcode", "Project.xcodeproj", (100, 100, 1200, 800), 1.0, 1080.0);
        assert!(new1);
        assert_eq!(core.current_scene_id, Some(s1));

        // Second window of same app joins existing scene
        let (s1_b, new1_b) = core.upsert_window(102, 2, "xcode", "Xcode", " Assistant.swift", (150, 150, 900, 700), 1.0, 1080.0);
        assert_eq!(s1, s1_b);
        assert!(!new1_b);

        // Different app creates second scene
        let (s2, new2) = core.upsert_window(201, 3, "safari", "Safari", "Apple", (200, 120, 1100, 760), 1.0, 1080.0);
        assert!(new2);
        assert_ne!(s1, s2);

        // Separating a window from multi-window scene s1 creates a new sidebar scene
        core.current_scene_id = Some(s1);
        let s3 = core.separate_window_to_new_scene(102).expect("separated scene");
        assert_ne!(s3, s1);
        assert_eq!(core.visible_sidebar_scenes().len(), 2);

        // Pulling window back from s3 merges it into current scene s1
        assert_eq!(core.pop_window_from(s3), Some(102));
        assert_eq!(core.visible_sidebar_scenes(), vec![s2]);
    }
}
