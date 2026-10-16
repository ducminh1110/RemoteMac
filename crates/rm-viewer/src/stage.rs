//! Stage Manager, as on the Mac, after StageManagerForWindows (MIT, depoledna / Andreas Wäscher):
//! one app's windows (a scene) stand on stage; the other scenes wait in a sidebar at the left as
//! live cards, tilted in perspective, the most recently used first. Its parts, ported:
//!
//! - `OpacityWindowStrategy`: a window leaving the stage is parked past the bottom-right of the
//!   screens, not hidden, so DWM keeps composing it and its card stays live.
//! - `CompositionThumbnail`: each card is a Windows.Graphics.Capture of its window (tiles.rs),
//!   sized and tilted by the macOS laws (cards.rs); a scene's windows cascade 10 dip apart; the
//!   scene under the pointer pops to 1.08 (0.2 s), and a press sinks it to 0.96 (0.08 s).
//! - `SceneTransitionAnimator`: a switch flies the cards over an overlay for 0.3 s (PowerEase
//!   out): the incoming scene's from the sidebar to where its windows stand, straightening out;
//!   the outgoing scene's from the stage into the sidebar, leaning in. The real windows are
//!   parked only once their cards have a picture, and come back while the cards still cover
//!   them, so nothing flashes.
//! - `MainWindow`'s modes: the sidebar is on screen, parked past the left edge (0.5 s) while a
//!   window on stage reaches into it, and out again over everything (Flyover) when the pointer
//!   touches the screen's left edge.
//! - `DragDropManager` and the sidebar's drag: a card dragged out past a 120 dip buffer puts its
//!   window on stage where it is let go (it joins the scene on stage); a window of a scene of
//!   several dragged into the buffer becomes a card that shrinks as it nears the sidebar, and
//!   let go there it is a scene of its own.
//! - `IconOverlayManager`: each card's app icon sits on its bottom edge; a click on one filters the
//!   sidebar to that app's scenes (and back), the others dimmed.
//!
//! The strip also slides down out of the way while the Mac's Dock is out (`lower`). Without
//! capture or composition the cards are DWM thumbnails, without the motion. scenes.rs keeps the
//! scenes; ui.rs says which windows there are and which one the user wants.
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cards::{self, card_size_dip, edge_tilt_at};
use crate::paint::{Canvas, Rgba};
use crate::scenes::Scenes;
use crate::tiles::{Host, Tile};

const KEY: COLORREF = COLORREF(0x00FF_00FF);
/// while something moves: the frame tick
const TICK: usize = 1;
/// the pointer at the edge (every tick of it), the overlap of the stage (every tenth)
const POLL: usize = 2;
const POLL_MS: u32 = 50;
const OVERLAP_EVERY: u32 = 10;
const FLIGHT_MS: f32 = 300.0;
const MODE_MS: f32 = 500.0;
const MORPH_MS: f32 = 250.0;
const DOCK_MS: f32 = 260.0;
const HOVER_MS: f32 = 200.0;
const PRESS_MS: f32 = 80.0;
const RELEASE_MS: f32 = 120.0;
const HOVER_SCALE: f32 = 1.08;
const PRESS_SCALE: f32 = 0.96;
/// how far a press moves before it is a drag (dip)
const DRAG_DIP: f64 = 10.0;
/// the buffer zone beside the sidebar (`DragDropManager.BufferWidthLogical`)
const BUFFER_DIP: f64 = 120.0;
/// the pointer this close to the left edge brings the sidebar out (physical px, as theirs)
const EDGE_PX: i32 = 44;
/// a window this far into the sidebar's width parks it (`STOW_OVERLAP_FRACTION`)
const STOW: f64 = 0.25;
/// cards drawn for a scene's windows
const MAX_CARDS: usize = 4;
/// frames a card gets to have a picture before its window is parked (`MaxReadyFrames`)
const READY_TICKS: u32 = 5;
// the icons (`IconOverlayManager`)
const ICON_DIP: f64 = 30.0;
const ICON_GAP: f64 = 4.0;
const OVERLAP_OFFSET: f64 = -14.0;
const SMALL_LEFT_SHIFT: f64 = -8.0;
const COMPACT: usize = 2;
const FILTERED_ICON_DIP: f64 = 22.0;
const ICON_HOVER: f32 = 1.15;
const ICON_HOVER_MS: f32 = 150.0;
const DIM: f32 = 0.45;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ease {
    /// PowerEase out (power 2)
    Out,
    /// PowerEase in
    In,
    /// CubicEase in-out
    InOut,
    /// CubicEase out
    CubicOut,
}

fn ease(e: Ease, u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    match e {
        Ease::Out => 1.0 - (1.0 - u) * (1.0 - u),
        Ease::In => u * u,
        Ease::InOut => {
            if u < 0.5 {
                4.0 * u * u * u
            } else {
                1.0 - (-2.0 * u + 2.0).powi(3) / 2.0
            }
        }
        Ease::CubicOut => 1.0 - (1.0 - u).powi(3),
    }
}

/// A number moving from one value to another (a scale, a slide).
#[derive(Clone, Copy, Debug)]
struct Pop {
    from: f32,
    to: f32,
    start: Instant,
    ms: f32,
    ease: Ease,
}

impl Pop {
    fn at(v: f32) -> Pop {
        Pop { from: v, to: v, start: Instant::now(), ms: 0.0, ease: Ease::Out }
    }

    fn value(&self, now: Instant) -> f32 {
        if self.ms <= 0.0 {
            return self.to;
        }
        let u = now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.ms;
        self.from + (self.to - self.from) * ease(self.ease, u)
    }

    fn done(&self, now: Instant) -> bool {
        self.ms <= 0.0 || now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 >= self.ms
    }

    /// To `to` from wherever it is now.
    fn go(&mut self, to: f32, ms: f32, ease: Ease, now: Instant) {
        if (self.to - to).abs() < 1e-4 {
            return;
        }
        *self = Pop { from: self.value(now), to, start: now, ms, ease };
    }
}

/// Where a card is and how it looks: its rect (in its layer's px), the slopes of its edges at
/// rest in the sidebar, how much of that rest tilt it has (1 in the sidebar, 0 on stage), its
/// opacity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Geom {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    top: f64,
    bottom: f64,
    tray: f64,
    alpha: f64,
}

impl Geom {
    fn lerp(a: Geom, b: Geom, k: f64) -> Geom {
        let l = |p: f64, q: f64| p + (q - p) * k;
        Geom { x: l(a.x, b.x), y: l(a.y, b.y), w: l(a.w, b.w), h: l(a.h, b.h), top: l(a.top, b.top), bottom: l(a.bottom, b.bottom), tray: l(a.tray, b.tray), alpha: l(a.alpha, b.alpha) }
    }

    fn moved(self, dx: f64, dy: f64) -> Geom {
        Geom { x: self.x + dx, y: self.y + dy, ..self }
    }

    fn from_rect(r: RECT, origin: (i32, i32)) -> Geom {
        Geom { x: (r.left - origin.0) as f64, y: (r.top - origin.1) as f64, w: (r.right - r.left) as f64, h: (r.bottom - r.top) as f64, top: 0.0, bottom: 0.0, tray: 0.0, alpha: 1.0 }
    }
}

#[derive(Clone, Copy, Debug)]
struct Motion {
    from: Geom,
    to: Geom,
    start: Instant,
    ms: f32,
    ease: Ease,
}

impl Motion {
    fn at(&self, now: Instant) -> (Geom, bool) {
        let u = now.saturating_duration_since(self.start).as_secs_f32() * 1000.0 / self.ms.max(1.0);
        (Geom::lerp(self.from, self.to, ease(self.ease, u) as f64), u >= 1.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    Sidebar,
    Overlay,
}

/// A window on Stage Manager's books.
struct Item {
    app: String,
    tile: Option<Tile>,
    /// a DWM thumbnail on the plain strip (no composition)
    thumb: isize,
    /// where the window stood before it was parked
    home: RECT,
    parked: bool,
    /// its size when it was parked (px): a card's size comes from it
    size: (i32, i32),
    layer: Layer,
    /// where its card is, where it goes
    at: Geom,
    motion: Option<Motion>,
    /// its place in the sidebar's layout (to see when it changes)
    target: Option<Geom>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    OnScreen,
    OffScreen,
    Flyover,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Out {
    Sidebar,
    Buffer,
    Past,
}

enum Drag {
    None,
    /// a press on a scene's card (screen px)
    Press { scene: u64, x: i32, y: i32 },
    /// a card dragged out of the sidebar (its window's, the scene's last)
    Out { scene: u64, hwnd: isize, phase: Out, thumb: Geom, win: (i32, i32) },
    /// a window of the scene on stage dragged towards the sidebar; `owned`: the drag was taken
    /// from Windows' move loop (the window parked, its card following the pointer)
    In { hwnd: isize, buffer: bool, owned: bool, size: (i32, i32) },
}

struct Flight {
    /// None while the cards wait for a picture
    start: Option<Instant>,
    waited: u32,
    incoming: Vec<isize>,
    outgoing: Vec<isize>,
    /// the windows are back; the cards go at the next tick
    landed: bool,
}

/// An app icon on a card's edge: its scene, its window's index, its app, its centre and size
/// (sidebar px).
type IconSpot = ((u64, usize), String, (f64, f64, f64));

/// What ui.rs gives Stage Manager: bring a window forward (and activate it), an app's icon
/// (straight RGBA, square) and name.
/// An app's icon: its size and straight RGBA pixels.
pub type IconOf = fn(&str) -> Option<(u32, Vec<u8>)>;

pub struct Hooks {
    pub front: fn(HWND),
    pub icon: IconOf,
    pub name: fn(&str) -> String,
}

struct Stage {
    on: bool,
    scenes: Scenes,
    items: HashMap<isize, Item>,
    strip: Option<HWND>,
    host: Option<Host>,
    overlay: Option<HWND>,
    icons: Option<crate::surface::Surface>,
    mode: Mode,
    /// the sidebar's slide: 0 on screen, 1 parked past the left edge
    side: Pop,
    /// down out of the way of the Dock: 0 in place, 1 down
    drop: Pop,
    hover: Option<u64>,
    pops: HashMap<u64, Pop>,
    icon_hover: Option<(u64, usize)>,
    icon_pops: HashMap<(u64, usize), Pop>,
    icon_spots: Vec<IconSpot>,
    icon_cache: HashMap<String, Canvas>,
    /// the icons need drawing again
    icons_dirty: bool,
    drag: Drag,
    flight: Option<Flight>,
    /// each scene's place in the sidebar (client px), for clicks
    spots: Vec<(u64, RECT)>,
    polls: u32,
    ticking: bool,
}

thread_local! {
    static STAGE: RefCell<Stage> = RefCell::new(Stage {
        on: false,
        scenes: Scenes::new(),
        items: HashMap::new(),
        strip: None,
        host: None,
        overlay: None,
        icons: None,
        mode: Mode::OnScreen,
        side: Pop::at(0.0),
        drop: Pop::at(0.0),
        hover: None,
        pops: HashMap::new(),
        icon_hover: None,
        icon_pops: HashMap::new(),
        icon_spots: vec![],
        icon_cache: HashMap::new(),
        icons_dirty: true,
        drag: Drag::None,
        flight: None,
        spots: vec![],
        polls: 0,
        ticking: false,
    });
    /// how deep each window sat in the z-chain when its scene was last on screen
    static Z_DEPTH: RefCell<HashMap<isize, usize>> = RefCell::new(HashMap::new());
}

static HOOKS: std::sync::Mutex<Option<Hooks>> = std::sync::Mutex::new(None);

pub fn set_hooks(h: Hooks) {
    *HOOKS.lock().unwrap() = Some(h);
}

fn front(hwnd: isize) {
    let f = HOOKS.lock().unwrap().as_ref().map(|h| h.front);
    if let Some(f) = f {
        f(HWND(hwnd as *mut _));
    }
}

/// Stage Manager's state, unless it is in use further up (a window message sent from inside
/// it): the caller then does nothing, as a nested event cannot change what is under way.
fn with<R>(f: impl FnOnce(&mut Stage) -> R) -> Option<R> {
    STAGE.with(|s| s.try_borrow_mut().ok().map(|mut g| f(&mut g)))
}

fn dpi() -> f64 {
    unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() }.max(96) as f64 / 96.0
}

fn px(v: f64) -> f64 {
    v * dpi()
}

/// The work area of the main monitor: the stage's, with the sidebar at its left.
pub fn work() -> RECT {
    unsafe {
        let mon = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        info.rcWork
    }
}

fn sidebar_w() -> i32 {
    px(cards::SIDEBAR_WIDTH_DIP).round() as i32
}

/// The part of the work area right of the sidebar, where a new window is put.
pub fn stage_area() -> RECT {
    let mut r = work();
    r.left += sidebar_w();
    r
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn rect_of(hwnd: isize) -> RECT {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(hwnd as *mut _), &mut r);
    }
    r
}

/// Where parked windows go: past the bottom-right of all the screens (`GetOffScreenPoint`).
fn away() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) + 100, GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN) + 100) }
}

/// Note how deep each of `windows` sits in the z-chain (`CaptureZOrder`).
fn capture_z(windows: &[isize]) {
    if windows.is_empty() {
        return;
    }
    let mut found = HashMap::new();
    unsafe {
        let mut h = GetTopWindow(None).ok();
        let mut depth = 0;
        while let Some(w) = h {
            if found.len() >= windows.len() {
                break;
            }
            if windows.contains(&(w.0 as isize)) {
                found.insert(w.0 as isize, depth);
                depth += 1;
            }
            h = GetWindow(w, GW_HWNDNEXT).ok();
        }
    }
    Z_DEPTH.with(|z| z.borrow_mut().extend(found));
}

/// Back to front, windows never seen on screen at the back (`OrderBottomToTop`).
fn bottom_to_top(mut windows: Vec<isize>) -> Vec<isize> {
    Z_DEPTH.with(|z| {
        let z = z.borrow();
        windows.sort_by_key(|w| std::cmp::Reverse(z.get(w).copied().unwrap_or(usize::MAX)));
    });
    windows
}

/// A window parked, its place kept (the first one: a second park keeps it) (`Hide`).
fn park(it: &mut Item, hwnd: isize) {
    let h = HWND(hwnd as *mut _);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() {
            return;
        }
        if !it.parked {
            let r = rect_of(hwnd);
            it.home = r;
            it.size = (r.right - r.left, r.bottom - r.top);
        }
        let (x, y) = away();
        // IgnoreZOrder: parking must not bring it to the front of the z-chain
        let _ = SetWindowPos(h, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        it.parked = true;
    }
}

/// A parked window back where it stood, at the top (`Show`).
fn unpark(it: &mut Item, hwnd: isize) {
    let h = HWND(hwnd as *mut _);
    unsafe {
        if it.parked {
            let _ = SetWindowPos(h, Some(HWND_TOP), it.home.left, it.home.top, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        }
        if !IsWindowVisible(h).as_bool() {
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        }
    }
    it.parked = false;
}

// ------------------------------------------------------------------ the public side (ui.rs)

pub fn active() -> bool {
    with(|s| s.on).unwrap_or(false)
}

/// The window is parked (its scene is in the sidebar).
pub fn is_parked(hwnd: HWND) -> bool {
    with(|s| s.items.get(&(hwnd.0 as isize)).is_some_and(|i| i.parked)).unwrap_or(false)
}

/// Stage Manager on, with these windows (app of each): the scene of `front` on stage, the
/// others in the sidebar at once.
pub fn enable(windows: Vec<(isize, String)>, front_window: isize) {
    with(|s| {
        if s.on {
            return;
        }
        s.on = true;
        ensure_windows(s);
        for (h, app) in &windows {
            add_item(s, *h, app);
            s.scenes.add(*h, app);
        }
        let first = s.scenes.scene_of(front_window).or_else(|| s.scenes.all().first().map(|sc| sc.id));
        if let Some(id) = first {
            instant_switch(s, id);
        }
        // in from the left, as at its start (`OnContentRendered`)
        s.side = Pop::at(1.0);
        s.side.go(0.0, MODE_MS, Ease::Out, Instant::now());
        relayout(s, false);
        start_tick(s);
    });
}

/// Stage Manager off: every window back on screen, the sidebar gone. Returns the windows.
pub fn disable() -> Vec<isize> {
    with(|s| {
        if !s.on {
            return vec![];
        }
        s.on = false;
        land_now(s);
        s.drag = Drag::None;
        let all = s.scenes.clear();
        let items: Vec<(isize, Item)> = s.items.drain().collect();
        for (h, mut it) in items {
            if let (Some(t), Some(host)) = (&it.tile, &s.host) {
                host.drop_card(t);
            }
            if it.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(it.thumb);
                }
            }
            unpark(&mut it, h);
        }
        if let Some(host) = &s.host {
            host.set_order(&[]);
        }
        hide_all(s);
        all
    })
    .unwrap_or_default()
}

/// A new window of `app` (it is on screen): it joins its app's scene, which comes on stage.
pub fn add(hwnd: isize, app: &str) {
    let fronted = with(|s| {
        if !s.on {
            return None;
        }
        if s.items.contains_key(&hwnd) {
            let id = s.scenes.scene_of(hwnd)?;
            if s.scenes.current() != Some(id) {
                switch(s, id, true);
                relayout(s, true);
            }
            return Some(hwnd);
        }
        add_item(s, hwnd, app);
        let (id, _) = s.scenes.add(hwnd, app);
        // a new window clear of the sidebar (in the middle of the stage)
        let r = rect_of(hwnd);
        let wk = work();
        if r.left < wk.left + sidebar_w() {
            let area = stage_area();
            let (w, h) = ((r.right - r.left).min(area.right - area.left), (r.bottom - r.top).min(area.bottom - area.top));
            unsafe {
                let _ = SetWindowPos(HWND(hwnd as *mut _), None, area.left + (area.right - area.left - w) / 2, area.top + (area.bottom - area.top - h) / 2, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        if s.scenes.current() != Some(id) {
            switch(s, id, true);
        }
        relayout(s, true);
        Some(hwnd)
    })
    .flatten();
    if let Some(h) = fronted {
        front(h);
    }
}

/// A window is gone: its card too; with nothing left on stage the most recent scene comes.
pub fn remove(hwnd: isize) {
    with(|s| {
        Z_DEPTH.with(|z| z.borrow_mut().remove(&hwnd));
        if !s.on {
            return;
        }
        if let Some(f) = s.flight.as_ref() {
            if f.incoming.contains(&hwnd) || f.outgoing.contains(&hwnd) {
                land_now(s);
            }
        }
        if matches!(s.drag, Drag::Out { hwnd: h, .. } | Drag::In { hwnd: h, .. } if h == hwnd) {
            s.drag = Drag::None;
            hide_overlay(s);
        }
        if let Some(it) = s.items.remove(&hwnd) {
            if let (Some(t), Some(host)) = (&it.tile, &s.host) {
                host.drop_card(t);
            }
            if it.thumb != 0 {
                unsafe {
                    let _ = DwmUnregisterThumbnail(it.thumb);
                }
            }
        }
        if let Some(next) = s.scenes.remove(hwnd) {
            switch(s, next, true);
        }
        relayout(s, true);
    });
}

/// The user wants `hwnd` of `app` (a click in the launcher or the Dock): its scene comes on
/// stage (a window not on the books yet joins its app's).
pub fn focus(hwnd: isize, app: &str) {
    let go = with(|s| {
        if !s.on {
            return false;
        }
        if !s.items.contains_key(&hwnd) {
            add_item(s, hwnd, app);
            s.scenes.add(hwnd, app);
        }
        let Some(id) = s.scenes.scene_of(hwnd) else { return false };
        if s.scenes.current() != Some(id) {
            s.scenes.clear_filter();
            switch(s, id, true);
            relayout(s, true);
        }
        true
    })
    .unwrap_or(false);
    if go && !is_parked(HWND(hwnd as *mut _)) {
        front(hwnd);
    }
}

/// The strip slides down out of the way (`on`: the Mac's Dock is out), or back up.
pub fn lower(on: bool) {
    with(|s| {
        s.drop.go(if on { 1.0 } else { 0.0 }, DOCK_MS, Ease::InOut, Instant::now());
        start_tick(s);
    });
}

/// A window is being moved by its title bar (WM_MOVING): towards the sidebar, a window of a
/// scene of several becomes a card (`DragDropManager.OnWindowMoveStart`).
pub fn window_moving(hwnd: isize) {
    with(|s| {
        if !s.on || s.host.is_none() || !matches!(s.drag, Drag::None) || s.flight.is_some() {
            return;
        }
        let several = s.scenes.current_scene().is_some_and(|c| c.windows.contains(&hwnd) && c.windows.len() > 1);
        if several {
            let r = rect_of(hwnd);
            s.drag = Drag::In { hwnd, buffer: false, owned: false, size: (r.right - r.left, r.bottom - r.top) };
            start_tick(s);
        }
    });
}

/// The move ended (WM_EXITSIZEMOVE). Ours (the drag taken over) goes on until the button is up.
pub fn window_move_end(hwnd: isize) {
    with(|s| {
        if let Drag::In { hwnd: h, owned: false, .. } = s.drag {
            if h == hwnd {
                s.drag = Drag::None;
            }
        }
    });
}

// ------------------------------------------------------------------ the books

fn add_item(s: &mut Stage, hwnd: isize, app: &str) {
    if s.items.contains_key(&hwnd) {
        return;
    }
    let r = rect_of(hwnd);
    s.items.insert(hwnd, Item { app: app.to_string(), tile: None, thumb: 0, home: r, parked: false, size: (r.right - r.left, r.bottom - r.top), layer: Layer::Sidebar, at: Geom::default(), motion: None, target: None });
}

/// A card for `hwnd`: a live tile (composition), or a DWM thumbnail on the plain strip.
fn ensure_card(s: &mut Stage, hwnd: isize) {
    let strip = s.strip;
    let Some(it) = s.items.get_mut(&hwnd) else { return };
    if it.tile.is_some() || it.thumb != 0 {
        return;
    }
    let source = it.size;
    if let Some(host) = &s.host {
        it.tile = Tile::new(host, HWND(hwnd as *mut _), source);
    } else if let Some(strip) = strip {
        if let Ok(t) = unsafe { DwmRegisterThumbnail(strip, HWND(hwnd as *mut _)) } {
            it.thumb = t;
        }
    }
}

fn drop_card(s: &mut Stage, hwnd: isize) {
    let Some(it) = s.items.get_mut(&hwnd) else { return };
    if let Some(t) = it.tile.take() {
        if let Some(host) = &s.host {
            host.drop_card(&t);
        }
    }
    if it.thumb != 0 {
        unsafe {
            let _ = DwmUnregisterThumbnail(it.thumb);
        }
        it.thumb = 0;
    }
    it.layer = Layer::Sidebar;
    it.motion = None;
    it.target = None;
}

// ------------------------------------------------------------------ switching

/// Scene `id` on stage at once (turning on, no composition).
fn instant_switch(s: &mut Stage, id: u64) {
    let incoming = match s.scenes.switch_to(id) {
        Some(sw) => {
            capture_z(&sw.outgoing);
            sw.incoming
        }
        None => s.scenes.get(id).map(|sc| sc.windows.clone()).unwrap_or_default(),
    };
    // every other scene's windows parked, each with its card
    let others: Vec<isize> = s.scenes.all().iter().filter(|sc| sc.id != id).flat_map(|sc| sc.windows.clone()).collect();
    capture_z(&others);
    for h in others {
        if let Some(it) = s.items.get_mut(&h) {
            if !it.parked {
                park(it, h);
            }
        }
        ensure_card(s, h);
    }
    for h in bottom_to_top(incoming) {
        drop_card(s, h);
        if let Some(it) = s.items.get_mut(&h) {
            unpark(it, h);
        }
    }
}

/// Scene `id` on stage (`AnimatedSwitchTo`): with composition the cards fly; else at once.
fn switch(s: &mut Stage, id: u64, animate: bool) {
    land_now(s);
    if !animate || s.host.is_none() || s.overlay.is_none() {
        instant_switch(s, id);
        return;
    }
    let wk = work();
    let origin = (wk.left, wk.top);
    let side_origin = sidebar_origin(s);
    let now = Instant::now();
    // the incoming cards, from where they are in the sidebar (onto the overlay)
    let incoming_cards: Vec<(isize, Geom)> = s
        .scenes
        .get(id)
        .map(|sc| sc.windows.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| s.items.get(&h).filter(|i| i.parked && i.tile.is_some() && i.layer == Layer::Sidebar).map(|i| (h, i.motion.map(|m| m.at(now).0).unwrap_or(i.at))))
        .collect();
    let Some(sw) = s.scenes.switch_to(id) else { return };
    capture_z(&sw.outgoing);
    // the outgoing windows on screen get their cards, where they stand
    let mut outgoing = vec![];
    for h in &sw.outgoing {
        let visible = unsafe { IsWindowVisible(HWND(*h as *mut _)).as_bool() && !IsIconic(HWND(*h as *mut _)).as_bool() };
        let parked = s.items.get(h).is_none_or(|i| i.parked);
        if !visible || parked {
            continue;
        }
        let r = rect_of(*h);
        if let Some(it) = s.items.get_mut(h) {
            it.size = (r.right - r.left, r.bottom - r.top);
        }
        ensure_card(s, *h);
        let Some(it) = s.items.get_mut(h) else { continue };
        if it.tile.is_none() {
            park(it, *h);
            continue;
        }
        it.layer = Layer::Overlay;
        it.at = Geom::from_rect(r, origin);
        it.motion = None;
        it.target = None;
        outgoing.push(*h);
    }
    let mut incoming = vec![];
    for (h, g) in incoming_cards {
        let Some(it) = s.items.get_mut(&h) else { continue };
        it.layer = Layer::Overlay;
        it.at = g.moved((side_origin.0 - origin.0) as f64, (side_origin.1 - origin.1) as f64);
        it.motion = None;
        it.target = None;
        incoming.push(h);
    }
    // the incoming scene's windows that were not parked (minimised, new) just come
    for h in sw.incoming.iter().filter(|h| !incoming.contains(h)) {
        drop_card(s, *h);
    }
    if let Some(host) = &s.host {
        for h in outgoing.iter().chain(incoming.iter()) {
            if let Some(t) = s.items.get(h).and_then(|i| i.tile.as_ref()) {
                host.lift(t);
            }
        }
    }
    show_overlay(s);
    s.flight = Some(Flight { start: None, waited: 0, incoming, outgoing, landed: false });
    start_tick(s);
}

/// A flight under way ends now: everything where it is going (a new switch, a window gone).
fn land_now(s: &mut Stage) {
    let Some(f) = s.flight.take() else { return };
    if f.start.is_none() {
        for h in &f.outgoing {
            if let Some(it) = s.items.get_mut(h) {
                park(it, *h);
            }
        }
    }
    if !f.landed {
        for h in bottom_to_top(f.incoming.clone()) {
            if let Some(it) = s.items.get_mut(&h) {
                unpark(it, h);
            }
        }
    }
    for h in &f.incoming {
        drop_card(s, *h);
    }
    for h in &f.outgoing {
        if let Some(it) = s.items.get_mut(h) {
            if let (Some(t), Some(host)) = (&it.tile, &s.host) {
                host.drop_card(t);
            }
            it.layer = Layer::Sidebar;
            it.motion = None;
            it.target = None;
        }
    }
    hide_overlay(s);
    relayout(s, false);
}

/// One frame of a flight; returns the window to bring forward once the scene has landed.
fn fly(s: &mut Stage, now: Instant) -> Vec<isize> {
    let mut fronts = vec![];
    let Some(f) = s.flight.as_mut() else { return fronts };
    if f.start.is_none() {
        // the outgoing cards need a picture before their windows go (`WaitForCardsAsync`)
        f.waited += 1;
        let waited = f.waited;
        let (outgoing, incoming) = (f.outgoing.clone(), f.incoming.clone());
        let ready = outgoing.iter().all(|h| s.items.get(h).and_then(|i| i.tile.as_ref()).is_none_or(|t| t.has_frame()));
        if !ready && waited < READY_TICKS {
            return fronts;
        }
        if let Some(f) = s.flight.as_mut() {
            f.start = Some(now);
        }
        for h in &outgoing {
            if let Some(it) = s.items.get_mut(h) {
                park(it, *h);
            }
        }
        // where everything goes: the sidebar after the switch, the windows' own places
        let wk = work();
        let origin = (wk.left, wk.top);
        let side_origin = sidebar_origin(s);
        let (targets, _, _) = layout(s);
        for h in &outgoing {
            let to = targets.iter().find(|(w, _)| w == h).map(|(_, g)| g.moved((side_origin.0 - origin.0) as f64, (side_origin.1 - origin.1) as f64));
            if let Some(it) = s.items.get_mut(h) {
                let from = it.at;
                // a card with no place in the sidebar (beyond the five) fades where it would go
                let to = to.unwrap_or(Geom { alpha: 0.0, tray: 1.0, ..from });
                let from = Geom { top: to.top, bottom: to.bottom, tray: 0.0, ..from };
                it.motion = Some(Motion { from, to, start: now, ms: FLIGHT_MS, ease: Ease::Out });
            }
        }
        for h in &incoming {
            if let Some(it) = s.items.get_mut(h) {
                let from = it.at;
                let to = Geom { tray: 0.0, alpha: 1.0, top: from.top, bottom: from.bottom, ..Geom::from_rect(it.home, origin) };
                it.motion = Some(Motion { from: Geom { tray: 1.0, ..from }, to, start: now, ms: FLIGHT_MS, ease: Ease::Out });
            }
        }
        // the rest of the sidebar moves to its new places alongside
        relayout_with(s, true, FLIGHT_MS, Ease::Out);
        return fronts;
    }
    let start = f.start.unwrap();
    if f.landed {
        // a frame after the windows came back: the cards go (`Release`)
        let f = s.flight.take().unwrap();
        for h in &f.incoming {
            drop_card(s, *h);
        }
        let side_origin = sidebar_origin(s);
        let wk = work();
        for h in &f.outgoing {
            if let Some(it) = s.items.get_mut(h) {
                if let (Some(t), Some(host)) = (&it.tile, &s.host) {
                    host.drop_card(t);
                }
                it.layer = Layer::Sidebar;
                it.at = it.at.moved((wk.left - side_origin.0) as f64, (wk.top - side_origin.1) as f64);
                it.motion = None;
                it.target = None;
            }
        }
        hide_overlay(s);
        relayout(s, false);
        return fronts;
    }
    if now.saturating_duration_since(start).as_secs_f32() * 1000.0 >= FLIGHT_MS {
        // landed: the windows come back while the cards still cover them (`onFlightLanded`)
        f.landed = true;
        let order = bottom_to_top(f.incoming.clone());
        for h in &order {
            if let Some(it) = s.items.get_mut(h) {
                unpark(it, *h);
            }
        }
        if let Some(last) = order.last() {
            fronts.push(*last);
        }
    }
    fronts
}

// ------------------------------------------------------------------ the sidebar's layout

fn sidebar_origin(s: &Stage) -> (i32, i32) {
    let wk = work();
    let (w, h) = (sidebar_w(), wk.bottom - wk.top);
    let now = Instant::now();
    (wk.left - (w as f32 * s.side.value(now)).round() as i32, wk.top + (h as f32 * s.drop.value(now)).round() as i32)
}

/// Where each card goes in the sidebar (client px), each scene's place for clicks, and the
/// icons. The scenes stand one above the other, centred (`StackPanel`), 28 dip apart; a scene's
/// windows cascade 10 dip right and down, the first in front; each scene leans by where its top
/// and bottom edges are on the screen (`AssignRowTiltsCore`).
type Layout = (Vec<(isize, Geom)>, Vec<(u64, RECT)>, Vec<IconSpot>);

fn layout(s: &Stage) -> Layout {
    let wk = work();
    let sh = (wk.bottom - wk.top) as f64;
    let k = dpi();
    let screen_h_dip = unsafe { GetSystemMetrics(SM_CYSCREEN) } as f64 / k;
    let (step, gap, pad) = (cards::STACK_STEP_DIP * k, cards::SCENE_GAP_DIP * k, cards::SIDEBAR_PAD_DIP * k);
    let size = |h: isize| {
        let (w, hh) = s.items.get(&h).map(|i| i.size).unwrap_or((800, 600));
        let (cw, ch) = card_size_dip(w as f64 / k, hh as f64 / k, screen_h_dip);
        (cw * k, ch * k)
    };
    let mut blocks = vec![];
    for id in s.scenes.sidebar() {
        let Some(sc) = s.scenes.get(id) else { continue };
        let wins: Vec<isize> = sc.windows.iter().copied().filter(|h| s.items.get(h).is_some_and(|i| i.parked)).take(MAX_CARDS).collect();
        if wins.is_empty() {
            continue;
        }
        let (mut bw, mut bh) = (0.0f64, 0.0f64);
        for (n, h) in wins.iter().enumerate() {
            let (cw, ch) = size(*h);
            bw = bw.max(n as f64 * step + cw);
            bh = bh.max(n as f64 * step + ch);
        }
        blocks.push((id, wins, bw, bh));
    }
    let total: f64 = blocks.iter().map(|b| b.3).sum::<f64>() + gap * blocks.len().saturating_sub(1) as f64;
    let mut y = ((sh - total) / 2.0).max(gap);
    let filtered = s.scenes.filter().is_some();
    let (mut cards_out, mut spots, mut icons) = (vec![], vec![], vec![]);
    for (id, wins, bw, bh) in blocks {
        if y + bh > sh {
            break;
        }
        let top = edge_tilt_at((wk.top as f64 + y) / k, screen_h_dip);
        let bottom = edge_tilt_at((wk.top as f64 + y + bh) / k, screen_h_dip);
        for (n, h) in wins.iter().enumerate() {
            let (cw, ch) = size(*h);
            cards_out.push((*h, Geom { x: pad + n as f64 * step, y: y + n as f64 * step, w: cw, h: ch, top, bottom, tray: 1.0, alpha: 1.0 }));
        }
        spots.push((id, RECT { left: pad as i32, top: y as i32, right: (pad + bw) as i32, bottom: (y + bh) as i32 }));
        // one icon a window, on the scene's bottom edge (`ComputeLayoutParams`)
        let n = wins.len();
        let size_px = if filtered { FILTERED_ICON_DIP } else { ICON_DIP } * k;
        let stride = if n <= COMPACT { size_px + ICON_GAP * k } else { size_px + OVERLAP_OFFSET * k };
        let row = size_px + n.saturating_sub(1) as f64 * stride;
        let first = if filtered { pad + bw / 2.0 - row / 2.0 + size_px / 2.0 } else { pad + if n <= COMPACT { SMALL_LEFT_SHIFT * k } else { 0.0 } + size_px / 2.0 };
        for (i, h) in wins.iter().enumerate() {
            let app = s.items.get(h).map(|it| it.app.clone()).unwrap_or_default();
            icons.push(((id, i), app, (first + i as f64 * stride, y + bh, size_px)));
        }
        y += bh + gap;
    }
    (cards_out, spots, icons)
}

/// The cards to their places in the sidebar (moving there when `animate`).
fn relayout(s: &mut Stage, animate: bool) {
    relayout_with(s, animate, MORPH_MS, Ease::CubicOut);
}

fn relayout_with(s: &mut Stage, animate: bool, ms: f32, how: Ease) {
    if !s.on {
        return;
    }
    let now = Instant::now();
    let (targets, spots, icons) = layout(s);
    s.spots = spots;
    s.icon_spots = icons;
    s.icons_dirty = true;
    for (h, it) in s.items.iter_mut() {
        if it.layer != Layer::Sidebar {
            continue;
        }
        let to = match targets.iter().find(|(w, _)| w == h).map(|(_, g)| *g) {
            Some(g) => g,
            // not in the sidebar (on stage, or beyond the five): faded out where it was
            None => Geom { alpha: 0.0, ..it.target.unwrap_or(it.at) },
        };
        if it.target == Some(to) {
            continue;
        }
        let first = it.target.is_none();
        it.target = Some(to);
        let from = it.motion.map(|m| m.at(now).0).unwrap_or(it.at);
        it.motion = match (animate, first) {
            (true, false) => Some(Motion { from, to, start: now, ms, ease: how }),
            // a card new to the sidebar fades in at its place
            (true, true) if from == Geom::default() => Some(Motion { from: Geom { alpha: 0.0, ..to }, to, start: now, ms, ease: how }),
            // one that was somewhere (landing from the overlay) moves from there
            (true, true) => Some(Motion { from, to, start: now, ms, ease: how }),
            (false, _) => {
                it.at = to;
                None
            }
        };
    }
    order_sidebar(s, &targets);
    start_tick(s);
}

/// The sidebar's cards back to front: later windows of a scene behind earlier ones.
fn order_sidebar(s: &Stage, targets: &[(isize, Geom)]) {
    let Some(host) = &s.host else { return };
    let order: Vec<&Tile> = targets.iter().rev().filter_map(|(h, _)| s.items.get(h).filter(|i| i.layer == Layer::Sidebar).and_then(|i| i.tile.as_ref())).collect();
    host.set_order(&order);
}

// ------------------------------------------------------------------ the windows

fn ensure_windows(s: &mut Stage) {
    if s.strip.is_some() {
        return;
    }
    unsafe {
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        // the composition window of the live cards; if capture or composition are missing, the
        // layered strip with DWM thumbnails
        let class = w!("TaroStageHost");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), ..Default::default() };
        RegisterClassW(&wc);
        if let Ok(hwnd) = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, class, PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
            if let Some(mut host) = Host::new(hwnd) {
                // the overlay the cards fly over (`TransitionOverlayWindow`): shown only while
                // they do, never activated, letting clicks through
                let oc = w!("TaroStageOverlay");
                let wc = WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: hinst.into(), lpszClassName: oc, ..Default::default() };
                RegisterClassW(&wc);
                if let Ok(o) = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, oc, PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
                    if host.attach_overlay(o) {
                        s.overlay = Some(o);
                    } else {
                        let _ = DestroyWindow(o);
                    }
                }
                s.strip = Some(hwnd);
                s.host = Some(host);
                let ic = w!("TaroStageIcons");
                crate::surface::register(hinst.into(), ic, Some(icons_proc), IDC_HAND);
                s.icons = crate::surface::Surface::new(hinst.into(), ic, "Taro Stage Manager icons", None, true, false);
                SetTimer(Some(hwnd), POLL, POLL_MS, None);
                return;
            }
            let _ = DestroyWindow(hwnd);
        }
        let class = w!("TaroStageStrip");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: hinst.into(), lpszClassName: class, hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), hbrBackground: CreateSolidBrush(KEY), ..Default::default() };
        RegisterClassW(&wc);
        if let Ok(hwnd) = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, class, PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
            let _ = SetLayeredWindowAttributes(hwnd, KEY, 255, LWA_COLORKEY);
            s.strip = Some(hwnd);
            SetTimer(Some(hwnd), POLL, POLL_MS, None);
        }
    }
}

fn show_overlay(s: &Stage) {
    if let Some(o) = s.overlay {
        let wk = work();
        unsafe {
            let _ = SetWindowPos(o, Some(HWND_TOPMOST), wk.left, wk.top, wk.right - wk.left, wk.bottom - wk.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

fn hide_overlay(s: &Stage) {
    if let Some(o) = s.overlay {
        unsafe {
            let _ = ShowWindow(o, SW_HIDE);
        }
    }
}

fn hide_all(s: &mut Stage) {
    unsafe {
        if let Some(h) = s.strip {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
    hide_overlay(s);
    if let Some(i) = &s.icons {
        i.hide();
    }
}

fn start_tick(s: &mut Stage) {
    if s.ticking {
        return;
    }
    if let Some(h) = s.strip {
        s.ticking = true;
        unsafe {
            SetTimer(Some(h), TICK, 15, None);
        }
    }
}

/// The sidebar window where it is now, the cards drawn, the icons; whether anything still moves.
fn frame(s: &mut Stage, now: Instant) -> bool {
    let Some(strip) = s.strip else { return false };
    if !s.on {
        return false;
    }
    let mut moving = false;
    let wk = work();
    let (w, h) = (sidebar_w(), wk.bottom - wk.top);
    let (ox, oy) = sidebar_origin(s);
    let any = s.items.values().any(|i| i.parked) || s.flight.is_some() || !matches!(s.drag, Drag::None);
    unsafe {
        if any {
            // right under the icons (both topmost, the flight's overlay above them): bringing it
            // to the top each frame would cover the icons and the flying cards
            let under = s.icons.as_ref().map(|i| i.hwnd).unwrap_or(HWND_TOPMOST);
            let _ = SetWindowPos(strip, Some(under), ox, oy, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = ShowWindow(strip, SW_HIDE);
        }
    }
    moving |= !s.side.done(now) || !s.drop.done(now);
    let radius = cards::CARD_RADIUS_DIP * dpi();
    // the scenes' hover pops, by window
    let scene_of: HashMap<isize, u64> = s.scenes.all().iter().flat_map(|sc| sc.windows.iter().map(move |h| (*h, sc.id))).collect();
    moving |= s.pops.values().any(|p| !p.done(now));
    let pops = s.pops.clone();
    let mut thumbs = vec![];
    for (hwnd, it) in s.items.iter_mut() {
        let g = match it.motion {
            Some(m) => {
                let (g, done) = m.at(now);
                if done {
                    it.motion = None;
                    it.at = m.to;
                    m.to
                } else {
                    moving = true;
                    it.at = g;
                    g
                }
            }
            None => it.at,
        };
        let shown = (it.parked || it.layer == Layer::Overlay) && g.alpha > 0.01;
        if let Some(t) = it.tile.as_mut() {
            t.scale = if it.layer == Layer::Sidebar { scene_of.get(hwnd).and_then(|id| pops.get(id)).map_or(1.0, |p| p.value(now) as f64) } else { 1.0 };
            t.set_visible(shown);
            if shown {
                t.place(g.x, g.y, g.w, g.h, g.top * g.tray, g.bottom * g.tray, radius);
                t.set_opacity(g.alpha as f32);
            }
        }
        if it.thumb != 0 {
            thumbs.push((it.thumb, g, shown));
        }
    }
    for (thumb, g, shown) in thumbs {
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
            rcDestination: RECT { left: g.x as i32, top: g.y as i32, right: (g.x + g.w) as i32, bottom: (g.y + g.h) as i32 },
            opacity: (g.alpha * 255.0) as u8,
            fVisible: BOOL(shown as i32),
            ..Default::default()
        };
        unsafe {
            let _ = DwmUpdateThumbnailProperties(thumb, &props);
        }
    }
    if s.host.is_none() {
        unsafe {
            let _ = InvalidateRect(Some(strip), None, true);
        }
    }
    moving |= s.icon_pops.values().any(|p| !p.done(now));
    draw_icons(s, now, (ox, oy), any);
    moving
}

// ------------------------------------------------------------------ the icons

fn icon_canvas(s: &mut Stage, app: &str) -> Option<Canvas> {
    if let Some(c) = s.icon_cache.get(app) {
        return Some(c.clone());
    }
    let f = HOOKS.lock().unwrap().as_ref().map(|h| h.icon)?;
    let (size, rgba) = f(app)?;
    let c = Canvas::from_rgba(size as usize, size as usize, &rgba);
    s.icon_cache.insert(app.to_string(), c.clone());
    Some(c)
}

/// The icons on the cards' edges, and under them the scenes' names while the sidebar is filtered
/// (`IconOverlayManager.UpdateIcons`), on a layered window over the sidebar: clicks on an icon go
/// to it, the rest to the cards under it.
fn draw_icons(s: &mut Stage, now: Instant, origin: (i32, i32), any: bool) {
    let Some(surf) = s.icons.as_ref() else { return };
    let hwnd = surf.hwnd;
    if !any || s.host.is_none() {
        surf.hide();
        return;
    }
    let moving = s.icon_pops.values().any(|p| !p.done(now));
    if !s.icons_dirty && !moving {
        // only the place changed (the sidebar sliding)
        unsafe {
            let _ = SetWindowPos(hwnd, None, origin.0, origin.1, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        return;
    }
    s.icons_dirty = false;
    let wk = work();
    let (w, h) = (sidebar_w() as usize, (wk.bottom - wk.top) as usize);
    let mut c = Canvas::new(w, h);
    let filter = s.scenes.filter().map(String::from);
    let spots = s.icon_spots.clone();
    let k = dpi();
    for (key, app, (cx, cy, size)) in &spots {
        let Some(icon) = icon_canvas(s, app) else { continue };
        let hover = s.icon_pops.get(key).map_or(1.0, |p| p.value(now)) as f64;
        let d = size * hover;
        let dim = filter.as_ref().is_some_and(|f| f != app);
        c.shadow((cx - d / 2.0) as f32, (cy - d / 2.0) as f32, d as f32, d as f32, (d * 0.24) as f32, (6.0 * k) as f32, (2.0 * k) as f32, Rgba::BLACK.alpha(0.35));
        c.draw(&icon, (cx - d / 2.0) as f32, (cy - d / 2.0) as f32, d as f32, d as f32, (d * 0.22) as f32, if dim { DIM } else { 1.0 });
    }
    if filter.is_some() {
        // each scene's name under it
        let name = HOOKS.lock().unwrap().as_ref().map(|h| h.name);
        let (text, _) = crate::surface::glass_text(crate::surface::dark_mode());
        for (id, r) in &s.spots {
            let app = s.scenes.get(*id).map(|sc| sc.app.clone()).unwrap_or_default();
            let label = name.map_or(app.clone(), |f| f(&app));
            let y = r.bottom as f32 + (FILTERED_ICON_DIP * k / 2.0 + 2.0 * k) as f32;
            crate::surface::text(&mut c, &label, r.left as f32, y, (r.right - r.left) as f32, (10.0 * k).round() as i32, 500, text, crate::surface::Align::Center);
        }
    }
    let surf = s.icons.as_mut().unwrap();
    surf.present(&c, origin.0, origin.1, 255, None);
    surf.show();
}

fn icon_at(s: &Stage, x: f32, y: f32) -> Option<((u64, usize), String)> {
    s.icon_spots.iter().find(|(_, _, (cx, cy, d))| ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt() <= d / 2.0).map(|(k, a, _)| (*k, a.clone()))
}

unsafe extern "system" fn icons_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    match msg {
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            with(|s| {
                let now = Instant::now();
                let hit = icon_at(s, x, y).map(|(k, _)| k);
                if hit != s.icon_hover {
                    if let Some(old) = s.icon_hover {
                        s.icon_pops.entry(old).or_insert(Pop::at(1.0)).go(1.0, ICON_HOVER_MS, Ease::CubicOut, now);
                    }
                    if let Some(new) = hit {
                        s.icon_pops.entry(new).or_insert(Pop::at(1.0)).go(ICON_HOVER, ICON_HOVER_MS, Ease::CubicOut, now);
                    }
                    s.icon_hover = hit;
                    s.icons_dirty = true;
                    start_tick(s);
                }
            });
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(|s| {
                if let Some(old) = s.icon_hover.take() {
                    s.icon_pops.entry(old).or_insert(Pop::at(1.0)).go(1.0, ICON_HOVER_MS, Ease::CubicOut, Instant::now());
                    s.icons_dirty = true;
                    start_tick(s);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // a click on an app's icon filters the sidebar to it, or back (`ToggleAppFilter`)
            let (x, y) = xy();
            with(|s| {
                if let Some((_, app)) = icon_at(s, x, y) {
                    s.scenes.toggle_filter(&app);
                    relayout(s, true);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------------ the modes

/// `UpdateModeByWindows`: the sidebar parks while a window on stage reaches into it (maximised,
/// or a quarter of the way in), and comes back when none does, unless the pointer is on it.
fn overlap_check(s: &mut Stage) {
    if !matches!(s.drag, Drag::None) || s.flight.is_some() {
        return;
    }
    let wk = work();
    let w = sidebar_w();
    let limit = wk.left + (w as f64 * STOW) as i32;
    let overlaps = s.scenes.current_scene().is_some_and(|c| {
        c.windows.iter().any(|h| unsafe {
            let hw = HWND(*h as *mut _);
            if !IsWindowVisible(hw).as_bool() || IsIconic(hw).as_bool() {
                return false;
            }
            IsZoomed(hw).as_bool() || rect_of(*h).left < limit
        })
    });
    let contains_mouse = cursor().x <= wk.left + w;
    let set = (s.mode == Mode::OnScreen && !contains_mouse) || s.mode == Mode::OffScreen || (s.mode == Mode::Flyover && !contains_mouse);
    if set {
        set_mode(s, if overlaps { Mode::OffScreen } else { Mode::OnScreen });
    }
}

fn set_mode(s: &mut Stage, m: Mode) {
    if s.mode == m {
        return;
    }
    let now = Instant::now();
    s.mode = m;
    match m {
        // in: decelerating; out: accelerating (`ApplyWindowMode`)
        Mode::OffScreen => s.side.go(1.0, MODE_MS, Ease::In, now),
        _ => s.side.go(0.0, MODE_MS, Ease::Out, now),
    }
    if m == Mode::OffScreen {
        reset_hover(s);
    }
    start_tick(s);
}

fn reset_hover(s: &mut Stage) {
    let now = Instant::now();
    for p in s.pops.values_mut() {
        p.go(1.0, HOVER_MS, Ease::InOut, now);
    }
    s.hover = None;
}

/// Every poll: the pointer at the left edge brings the sidebar out over everything (`Flyover`).
fn poll(s: &mut Stage) {
    if !s.on {
        return;
    }
    let wk = work();
    let c = cursor();
    if s.mode == Mode::OffScreen && c.x <= wk.left + EDGE_PX && c.y >= wk.top && c.y < wk.bottom {
        set_mode(s, Mode::Flyover);
    }
    s.polls += 1;
    if s.polls.is_multiple_of(OVERLAP_EVERY) {
        overlap_check(s);
    }
}

// ------------------------------------------------------------------ dragging

fn spot_at(s: &Stage, x: i32, y: i32) -> Option<u64> {
    s.spots.iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(id, _)| *id)
}

/// A card hangs off the pointer by its title bar: 16 dip, or a third of a short card
/// (`CardTopFor`).
fn card_top_for(cursor_y: f64, h: f64) -> f64 {
    cursor_y - (16.0 * dpi()).min(h * 0.35)
}

fn thumb_of(size: (i32, i32)) -> (f64, f64) {
    let k = dpi();
    let screen_h_dip = unsafe { GetSystemMetrics(SM_CYSCREEN) } as f64 / k;
    let (w, h) = card_size_dip(size.0 as f64 / k, size.1 as f64 / k, screen_h_dip);
    (w * k, h * k)
}

/// One frame of a drag (the pointer, in screen px).
fn drag_frame(s: &mut Stage) -> Vec<isize> {
    let mut fronts = vec![];
    let c = cursor();
    let (cx, cy) = (c.x as f64, c.y as f64);
    let wk = work();
    let origin = (wk.left as f64, wk.top as f64);
    let side_right = (wk.left + sidebar_w()) as f64;
    let buffer_right = side_right + px(BUFFER_DIP);
    let held = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } < 0;
    match s.drag {
        Drag::Out { scene, hwnd, phase, thumb, win } => {
            let Some(it) = s.items.get_mut(&hwnd) else {
                s.drag = Drag::None;
                return fronts;
            };
            if phase == Out::Past && cx <= buffer_right {
                // into the buffer again: the window goes, its card comes back
                unsafe {
                    let _ = SetWindowPos(HWND(hwnd as *mut _), Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
                park(it, hwnd);
            }
            let next = if cx <= side_right {
                it.at = Geom { x: cx - thumb.w / 2.0 - origin.0, y: card_top_for(cy, thumb.h) - origin.1, alpha: 1.0, tray: 1.0, ..thumb };
                Out::Sidebar
            } else if cx <= buffer_right {
                let t = ((cx - side_right) / px(BUFFER_DIP)).clamp(0.0, 1.0);
                let (gw, gh) = (thumb.w + (win.0 as f64 - thumb.w) * t, thumb.h + (win.1 as f64 - thumb.h) * t);
                it.at = Geom { x: cx - gw / 2.0 - origin.0, y: card_top_for(cy, gh) - origin.1, w: gw, h: gh, tray: 1.0 - t, alpha: 1.0, ..thumb };
                Out::Buffer
            } else {
                // past the buffer: the real window under the pointer, the card hidden
                it.at.alpha = 0.0;
                let (ww, wh) = win;
                it.parked = false;
                unsafe {
                    let _ = SetWindowPos(HWND(hwnd as *mut _), Some(HWND_TOPMOST), (cx - ww as f64 / 2.0) as i32, card_top_for(cy, wh as f64) as i32, ww, wh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                }
                Out::Past
            };
            it.motion = None;
            s.drag = Drag::Out { scene, hwnd, phase: next, thumb, win };
            if !held {
                // let go where the sidebar did not hear it (a lost capture)
                fronts.extend(drop_out(s));
            }
        }
        Drag::In { hwnd, buffer, owned, size } => {
            if !held && !owned {
                s.drag = Drag::None;
                return fronts;
            }
            let thumb = thumb_of(size);
            let mut buffer = buffer;
            if !buffer && cx < buffer_right {
                // into the buffer: the drag is taken from Windows (`TakeOverFromMoveLoop`), the
                // window parked, its live card under the pointer
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CANCELMODE, WPARAM(0), LPARAM(0));
                }
                if let Some(it) = s.items.get_mut(&hwnd) {
                    it.parked = false;
                    park(it, hwnd);
                    it.size = size;
                }
                ensure_card(s, hwnd);
                let ok = match s.items.get_mut(&hwnd) {
                    Some(it) if it.tile.is_some() => {
                        it.layer = Layer::Overlay;
                        it.target = None;
                        true
                    }
                    Some(it) => {
                        unpark(it, hwnd);
                        false
                    }
                    None => false,
                };
                if !ok {
                    s.drag = Drag::None;
                    return fronts;
                }
                if let (Some(t), Some(host)) = (s.items.get(&hwnd).and_then(|i| i.tile.as_ref()), &s.host) {
                    host.lift(t);
                }
                show_overlay(s);
                buffer = true;
                s.drag = Drag::In { hwnd, buffer, owned: true, size };
            } else if buffer && cx > buffer_right {
                // out of the buffer again: the window back under the pointer, carried by us now
                if let Some(it) = s.items.get_mut(&hwnd) {
                    let (x, y) = ((cx - size.0 as f64 / 2.0) as i32, card_top_for(cy, size.1 as f64) as i32);
                    it.home = RECT { left: x, top: y, right: x + size.0, bottom: y + size.1 };
                    unpark(it, hwnd);
                }
                drop_card(s, hwnd);
                hide_overlay(s);
                buffer = false;
                s.drag = Drag::In { hwnd, buffer, owned, size };
            }
            let owned = matches!(s.drag, Drag::In { owned: true, .. });
            if buffer {
                let t = ((buffer_right - cx) / (buffer_right - side_right)).clamp(0.0, 1.0);
                let (gw, gh) = (size.0 as f64 + (thumb.0 - size.0 as f64) * t, size.1 as f64 + (thumb.1 - size.1 as f64) * t);
                let y = card_top_for(cy, gh);
                let k = dpi();
                let screen_h_dip = unsafe { GetSystemMetrics(SM_CYSCREEN) } as f64 / k;
                let (top, bottom) = (edge_tilt_at(y / k, screen_h_dip), edge_tilt_at((y + gh) / k, screen_h_dip));
                if let Some(it) = s.items.get_mut(&hwnd) {
                    it.at = Geom { x: cx - gw / 2.0 - origin.0, y: y - origin.1, w: gw, h: gh, top, bottom, tray: t, alpha: 1.0 };
                    it.motion = None;
                }
            } else if owned {
                unsafe {
                    let _ = SetWindowPos(HWND(hwnd as *mut _), None, (cx - size.0 as f64 / 2.0) as i32, card_top_for(cy, size.1 as f64) as i32, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
            if !held {
                fronts.extend(drop_in(s, cx < side_right));
            }
        }
        _ => {}
    }
    fronts
}

/// A card dragged out of the sidebar is let go: past the buffer its window joins the scene on
/// stage where it is (`PopWindowFrom`); anywhere else it goes back.
fn drop_out(s: &mut Stage) -> Vec<isize> {
    let Drag::Out { scene, hwnd, phase, .. } = std::mem::replace(&mut s.drag, Drag::None) else { return vec![] };
    let wk = work();
    let side = sidebar_origin(s);
    if phase == Out::Past {
        unsafe {
            let _ = SetWindowPos(HWND(hwnd as *mut _), Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        if let Some(it) = s.items.get_mut(&hwnd) {
            it.parked = false;
        }
        drop_card(s, hwnd);
        if s.scenes.pop_from(scene).is_none() {
            // nothing on stage: the scene comes, the rest of it too
            instant_switch(s, scene);
        }
        hide_overlay(s);
        relayout(s, true);
        return vec![hwnd];
    }
    // back to its place in the sidebar
    if let Some(it) = s.items.get_mut(&hwnd) {
        if let (Some(t), Some(host)) = (&it.tile, &s.host) {
            host.drop_card(t);
        }
        it.layer = Layer::Sidebar;
        it.at = Geom { alpha: 1.0, ..it.at.moved((wk.left - side.0) as f64, (wk.top - side.1) as f64) };
        it.target = None;
        it.motion = None;
    }
    hide_overlay(s);
    relayout_with(s, true, FLIGHT_MS, Ease::Out);
    vec![]
}

/// A window dragged towards the sidebar is let go: in the sidebar it becomes a scene of its own
/// (`SeparateWindowToNewScene`); in the buffer it goes back where it was dragged from.
fn drop_in(s: &mut Stage, in_sidebar: bool) -> Vec<isize> {
    let Drag::In { hwnd, buffer, .. } = std::mem::replace(&mut s.drag, Drag::None) else { return vec![] };
    if !buffer {
        return vec![];
    }
    let wk = work();
    let side = sidebar_origin(s);
    if in_sidebar && s.scenes.separate(hwnd).is_some() {
        if let Some(it) = s.items.get_mut(&hwnd) {
            if let (Some(t), Some(host)) = (&it.tile, &s.host) {
                host.drop_card(t);
            }
            it.layer = Layer::Sidebar;
            it.at = it.at.moved((wk.left - side.0) as f64, (wk.top - side.1) as f64);
            it.target = None;
        }
        hide_overlay(s);
        relayout_with(s, true, FLIGHT_MS, Ease::Out);
        return vec![];
    }
    // cancelled: the window back where it was parked from
    if let Some(it) = s.items.get_mut(&hwnd) {
        unpark(it, hwnd);
    }
    drop_card(s, hwnd);
    hide_overlay(s);
    relayout(s, true);
    vec![hwnd]
}

// ------------------------------------------------------------------ the sidebar's window

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32);
    match msg {
        WM_TIMER if wp.0 == TICK => {
            let now = Instant::now();
            let fronts = with(|s| {
                let mut fronts = fly(s, now);
                if matches!(s.drag, Drag::Out { .. } | Drag::In { .. }) {
                    fronts.extend(drag_frame(s));
                }
                let moving = frame(s, now);
                if !(moving || s.flight.is_some() || !matches!(s.drag, Drag::None)) {
                    s.ticking = false;
                    let _ = KillTimer(Some(hwnd), TICK);
                }
                fronts
            })
            .unwrap_or_default();
            for h in fronts {
                front(h);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == POLL => {
            with(|s| {
                poll(s);
                let now = Instant::now();
                if !s.ticking && (!s.side.done(now) || !s.drop.done(now)) {
                    start_tick(s);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            with(|s| {
                if s.flight.is_some() {
                    return;
                }
                if let Some(scene) = spot_at(s, x, y) {
                    let c = cursor();
                    s.drag = Drag::Press { scene, x: c.x, y: c.y };
                    s.pops.entry(scene).or_insert(Pop::at(1.0)).go(PRESS_SCALE, PRESS_MS, Ease::CubicOut, Instant::now());
                    start_tick(s);
                    SetCapture(hwnd);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            with(|s| {
                let now = Instant::now();
                if let Drag::Press { scene, x: px0, y: py0 } = s.drag {
                    let c = cursor();
                    if (((c.x - px0).pow(2) + (c.y - py0).pow(2)) as f64).sqrt() >= px(DRAG_DIP) {
                        // the drag starts: the scene's last window's card leaves the sidebar
                        // (`ScenesControl_MouseMove`)
                        let wk = work();
                        let side = sidebar_origin(s);
                        let picked = s.scenes.get(scene).and_then(|sc| sc.windows.last().copied());
                        let lifted = picked.and_then(|h| {
                            let it = s.items.get_mut(&h).filter(|i| i.tile.is_some() && i.parked)?;
                            it.layer = Layer::Overlay;
                            let g = it.motion.map(|m| m.at(now).0).unwrap_or(it.at).moved((side.0 - wk.left) as f64, (side.1 - wk.top) as f64);
                            it.at = g;
                            it.motion = None;
                            it.target = None;
                            Some((h, g, (it.home.right - it.home.left, it.home.bottom - it.home.top)))
                        });
                        if let Some((h, g, win)) = lifted {
                            if let (Some(t), Some(host)) = (s.items.get(&h).and_then(|i| i.tile.as_ref()), &s.host) {
                                host.lift(t);
                            }
                            show_overlay(s);
                            s.pops.entry(scene).or_insert(Pop::at(1.0)).go(1.0, HOVER_MS, Ease::InOut, now);
                            s.drag = Drag::Out { scene, hwnd: h, phase: Out::Sidebar, thumb: g, win };
                            start_tick(s);
                        }
                    }
                    return;
                }
                if !matches!(s.drag, Drag::None) {
                    return;
                }
                // the scene under the pointer pops
                let hit = spot_at(s, x, y);
                if hit != s.hover {
                    if let Some(old) = s.hover {
                        s.pops.entry(old).or_insert(Pop::at(1.0)).go(1.0, HOVER_MS, Ease::InOut, now);
                    }
                    if let Some(new) = hit {
                        s.pops.entry(new).or_insert(Pop::at(1.0)).go(HOVER_SCALE, HOVER_MS, Ease::InOut, now);
                    }
                    s.hover = hit;
                    start_tick(s);
                }
            });
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        0x02A3 => {
            // WM_MOUSELEAVE
            with(|s| {
                if matches!(s.drag, Drag::None) {
                    reset_hover(s);
                    start_tick(s);
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            let fronts = with(|s| {
                let now = Instant::now();
                match std::mem::replace(&mut s.drag, Drag::None) {
                    Drag::Press { scene, .. } => {
                        // a click: its scene comes on stage (the filter goes first)
                        s.pops.entry(scene).or_insert(Pop::at(1.0)).go(HOVER_SCALE, RELEASE_MS, Ease::CubicOut, now);
                        s.scenes.clear_filter();
                        switch(s, scene, true);
                        relayout(s, true);
                        vec![]
                    }
                    d @ Drag::Out { .. } => {
                        s.drag = d;
                        let mut f = drag_frame(s);
                        if matches!(s.drag, Drag::Out { .. }) {
                            f.extend(drop_out(s));
                        }
                        f
                    }
                    d => {
                        s.drag = d;
                        vec![]
                    }
                }
            })
            .unwrap_or_default();
            for h in fronts {
                front(h);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => DefWindowProcW(hwnd, msg, wp, lp),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            // the plain strip: a soft card under each DWM thumbnail (the composition one draws
            // nothing of its own)
            let spots = with(|s| if s.host.is_none() { s.spots.clone() } else { vec![] }).unwrap_or_default();
            if !spots.is_empty() {
                let card = CreateSolidBrush(COLORREF(0x002c_2622));
                let old = SelectObject(dc, card.into());
                let pen = SelectObject(dc, GetStockObject(NULL_PEN));
                for (_, r) in spots {
                    let _ = RoundRect(dc, r.left - 5, r.top - 5, r.right + 5, r.bottom + 5, 14, 14);
                }
                SelectObject(dc, pen);
                SelectObject(dc, old);
                let _ = DeleteObject(card.into());
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // clicks go through to the windows under it
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curves_start_and_end_where_they_should() {
        for e in [Ease::Out, Ease::In, Ease::InOut, Ease::CubicOut] {
            assert!(ease(e, 0.0).abs() < 1e-6 && (ease(e, 1.0) - 1.0).abs() < 1e-6, "{e:?}");
        }
        // PowerEase out is ahead of linear, in behind
        assert!(ease(Ease::Out, 0.3) > 0.3 && ease(Ease::In, 0.3) < 0.3);
    }

    #[test]
    fn a_pop_turns_around_from_where_it_is() {
        let t0 = Instant::now();
        let mut p = Pop::at(1.0);
        p.go(1.08, 200.0, Ease::InOut, t0);
        let mid = t0 + std::time::Duration::from_millis(100);
        let v = p.value(mid);
        assert!(v > 1.0 && v < 1.08);
        p.go(1.0, 200.0, Ease::InOut, mid);
        assert!((p.value(mid) - v).abs() < 1e-4, "no jump");
        assert!(p.done(mid + std::time::Duration::from_millis(250)));
    }
}
