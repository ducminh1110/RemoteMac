//! The Dock: a column of Liquid Glass at the left or right edge of the screen (Settings) with
//! the Mac Desktop, the Mac apps open here and Open an App. It stays out of the way until the
//! pointer is pushed against that edge (the mouse keeps moving outward while the pointer cannot
//! go further, as on the Mac when its Dock hides itself): passing by the edge, or resting there,
//! leaves it hidden. It slides out, its icons grow a little under the pointer with their name
//! beside them, open apps have a dot on the edge's side; it slides away once the pointer has left
//! it. It is drawn here (no picture of the Mac's own Dock), and changes nothing of Windows.

use std::time::{Duration, Instant};

/// One item of the Dock: an icon, or a line between groups (as the Mac's Dock has).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DockItem {
    pub id: String,
    pub name: String,
    /// its dot (the app runs)
    pub running: bool,
    pub separator: bool,
    /// where its picture is found (the viewer's icons: an app's id, or the Dock item's)
    pub icon: String,
}

/// The side of the screen the Dock is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

impl Edge {
    /// From the setting (0 off, 1 left, 2 right).
    pub fn from_setting(v: u8) -> Option<Edge> {
        match v {
            1 => Some(Edge::Left),
            2 => Some(Edge::Right),
            _ => None,
        }
    }
}

/// Icon size, spacing, padding and the room of a line between groups, at their largest
/// (logical px); the Dock's distance from the edge; the smallest icons it shrinks to.
pub const ICON: f32 = 46.0;
pub const GAP: f32 = 8.0;
pub const PAD: f32 = 10.0;
pub const DIVIDER: f32 = 12.0;
pub const INSET: f32 = 8.0;
pub const MIN_ICON: f32 = 20.0;

/// Where everything of a Dock goes along its edge (logical px from its top): its icon size, where
/// each item starts, its length and its thickness.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub icon: f32,
    pub starts: Vec<f32>,
    pub length: f32,
    pub thick: f32,
}

/// The Dock of `items` within `room` along the edge: icons as large as the Mac's are by
/// default, smaller (as on the Mac) when there are many of them or the screen is short.
pub fn layout(items: &[DockItem], room: f32) -> Layout {
    let n = items.len();
    let lines = items.iter().filter(|i| i.separator).count();
    let icons = n - lines;
    let natural = icons as f32 * ICON + lines as f32 * DIVIDER + n.saturating_sub(1) as f32 * GAP;
    let k = if natural <= 0.0 || 2.0 * PAD + natural <= room { 1.0 } else { ((room - 2.0 * PAD) / natural).clamp(MIN_ICON / ICON, 1.0) };
    let mut starts = Vec::with_capacity(n);
    let mut pos = PAD;
    for it in items {
        starts.push(pos);
        pos += if it.separator { DIVIDER } else { ICON } * k + GAP * k;
    }
    let length = if n == 0 { 2.0 * PAD } else { pos - GAP * k + PAD };
    Layout { icon: ICON * k, starts, length, thick: ICON * k + 2.0 * PAD }
}

/// The icon at `along` (logical px from the Dock's top); never a line.
pub fn icon_at(items: &[DockItem], l: &Layout, along: f32) -> Option<usize> {
    let gap = GAP * l.icon / ICON;
    (0..items.len()).find(|&i| !items[i].separator && along >= l.starts[i] - gap / 2.0 && along < l.starts[i] + l.icon + gap / 2.0)
}

/// How much each icon grows with the pointer over icon `hover`: the icon itself, its
/// neighbours a little.
pub fn growth(n: usize, hover: Option<usize>) -> Vec<f32> {
    (0..n)
        .map(|i| match hover {
            Some(h) if h == i => 1.22,
            Some(h) if (h as isize - i as isize).abs() == 1 => 1.08,
            _ => 1.0,
        })
        .collect()
}

/// A push against the edge: mouse movement outward (raw counts, before Windows' pointer speed)
/// while the pointer is held at the edge, added up. A pause, or the pointer leaving the edge,
/// starts it again: only a deliberate push brings the Dock out.
#[derive(Debug, Default)]
pub struct Push {
    total: f32,
    last: Option<Instant>,
}

impl Push {
    /// How far to push (about 2 mm of a usual mouse, a flick on a touchpad).
    pub const NEEDED: f32 = 70.0;
    /// A longer pause between moves starts the push again.
    pub const PAUSE: Duration = Duration::from_millis(300);

    /// A move of `outward` counts past the edge (0 or less: along it, or back) at `now`.
    /// Returns whether the push is now deep enough (it starts again after).
    pub fn add(&mut self, outward: f32, now: Instant) -> bool {
        if self.last.is_some_and(|t| now.duration_since(t) > Self::PAUSE) {
            self.total = 0.0;
        }
        if outward <= 0.0 {
            // along the edge keeps it; back into the screen ends it
            if outward < 0.0 {
                self.reset();
            }
            return false;
        }
        self.last = Some(now);
        self.total += outward;
        if self.total >= Self::NEEDED {
            self.reset();
            return true;
        }
        false
    }

    pub fn reset(&mut self) {
        self.total = 0.0;
        self.last = None;
    }
}

#[cfg(windows)]
pub use win::{close, first_item, hold, hwnd, is_out, register, reveal_now, sync};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::motion::{tokens, Anim, Curve};
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{self, Surface};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::Input::{GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE};
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("TaroDock");
    /// animation and hover, while it is out
    const TIMER: usize = 1;
    /// a pointer that moves the cursor directly (a pen, a remote session): resting at the edge
    const TIMER_REST: usize = 2;

    type Mask = (Vec<u8>, usize, usize);
    /// An item's picture, looked up when the Dock first needs it: (size, straight RGBA).
    type IconOf = Box<dyn Fn(&str) -> Option<(u32, Vec<u8>)>>;

    struct Dock {
        surf: Surface,
        edge: Edge,
        items: Vec<DockItem>,
        icons: HashMap<String, Canvas>,
        placeholders: HashMap<String, Canvas>,
        names: HashMap<String, Mask>,
        icon_of: IconOf,
        /// its scale, its layout (logical px) and its rectangle on the screen (device px: x, y,
        /// w, h) where it last came out
        scale: f32,
        layout: Layout,
        rect: (i32, i32, usize, usize),
        /// 0 hidden past the edge .. 1 out
        shown: Anim,
        grow: Vec<Anim>,
        hover: Option<usize>,
        /// the glass made when it comes out (what is behind it then), and its margin
        glass: Option<(Canvas, usize)>,
        left_at: Option<Instant>,
        /// kept out until then whatever the pointer does (a menu of it is open, the gallery)
        held: Option<Instant>,
        push: Push,
        /// the pointer moves the cursor directly (no relative moves to add up): resting at the
        /// edge brings the Dock out instead, since when
        absolute: bool,
        resting: Option<Instant>,
        on_click: Box<dyn Fn(String)>,
        /// a right-click on an item: its id and where its menu goes (screen px, beside it)
        on_menu: Box<dyn Fn(String, POINT)>,
        dark: bool,
    }

    thread_local! {
        static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_HAND);
    }

    /// The Dock's window, while there is one (a menu of it is owned by it).
    pub fn hwnd() -> Option<HWND> {
        DOCK.with(|d| d.borrow().as_ref().map(|k| k.surf.hwnd))
    }

    /// Its first icon while it is out, and where that icon's menu goes (the gallery's picture of
    /// a right-click).
    pub fn first_item() -> Option<(String, POINT)> {
        DOCK.with(|d| {
            let b = d.borrow();
            let k = b.as_ref()?;
            let i = k.items.iter().position(|i| !i.separator)?;
            Some((k.items[i].id.clone(), k.beside(i)))
        })
    }

    /// The Dock is out (or coming out): the guided tour waits for it.
    pub fn is_out() -> bool {
        DOCK.with(|d| d.borrow().as_ref().is_some_and(|k| k.shown.target() > 0.5))
    }

    /// Keep it out (a menu of it is open) or let it go again.
    pub fn hold(on: bool) {
        DOCK.with(|d| {
            if let Some(k) = d.borrow_mut().as_mut() {
                k.held = on.then(|| Instant::now() + Duration::from_secs(600));
                k.left_at = None;
            }
        });
    }

    /// The Dock at `edge` with these items (made the first time; moved to another edge when the
    /// setting changes). `icon_of(key)` gives an item's picture; `on_click(id)` opens it,
    /// `on_menu(id, at)` shows its menu.
    pub fn sync(hinst: HINSTANCE, edge: Edge, items: Vec<DockItem>, icon_of: impl Fn(&str) -> Option<(u32, Vec<u8>)> + 'static, on_click: impl Fn(String) + 'static, on_menu: impl Fn(String, POINT) + 'static) {
        let exists = DOCK.with(|d| d.borrow().as_ref().map(|k| k.edge));
        if exists.is_some_and(|e| e != edge) {
            close();
        }
        if exists == Some(edge) {
            DOCK.with(|d| {
                if let Some(k) = d.borrow_mut().as_mut() {
                    if k.items != items {
                        k.grow = items.iter().map(|_| Anim::at(1.0)).collect();
                        k.items = items;
                        k.hover = None;
                        // another length: placed and its glass made again (at once when it is out)
                        if k.shown.target() > 0.5 {
                            k.place_and_glass();
                        } else {
                            k.glass = None;
                        }
                    }
                    // pictures that came since
                    let missing: Vec<String> = k.items.iter().filter(|i| !i.separator && !k.icons.contains_key(&i.icon)).map(|i| i.icon.clone()).collect();
                    for key in missing {
                        if let Some(c) = (k.icon_of)(&key).filter(|(s, px)| px.len() == (*s * *s * 4) as usize).map(|(s, px)| Canvas::from_rgba(s as usize, s as usize, &px)) {
                            k.icons.insert(key, c);
                        }
                    }
                }
            });
            return;
        }
        let Some(surf) = Surface::new(hinst, CLASS, &format!("{} Dock", crate::brand::name()), None, true, false) else { return };
        let hwnd = surf.hwnd;
        DOCK.with(|d| {
            *d.borrow_mut() = Some(Dock {
                surf,
                edge,
                grow: items.iter().map(|_| Anim::at(1.0)).collect(),
                layout: layout(&items, 10_000.0),
                items,
                icons: HashMap::new(),
                placeholders: HashMap::new(),
                names: HashMap::new(),
                icon_of: Box::new(icon_of),
                scale: 1.0,
                rect: (0, 0, 1, 1),
                shown: Anim::at(0.0),
                hover: None,
                glass: None,
                left_at: None,
                held: None,
                push: Push::default(),
                absolute: false,
                resting: None,
                on_click: Box::new(on_click),
                on_menu: Box::new(on_menu),
                dark: false,
            })
        });
        // the mouse's own movement, also while another window is in front: pushes are measured
        // past where the pointer can go
        let dev = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        unsafe {
            if RegisterRawInputDevices(&[dev], std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err() {
                // no raw mouse input: resting at the edge brings it out instead
                DOCK.with(|d| d.borrow_mut().as_mut().map(|k| k.absolute = true));
                SetTimer(Some(hwnd), TIMER_REST, 100, None);
            }
        }
    }

    /// Remove the Dock (the setting was turned off, or the Mac is gone).
    pub fn close() {
        let d = DOCK.with(|d| d.borrow_mut().take());
        if d.is_some() {
            // the mouse's own movement is no longer wanted
            let dev = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_REMOVE, hwndTarget: HWND::default() };
            unsafe {
                let _ = RegisterRawInputDevices(&[dev], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
            }
        }
        drop(d);
    }

    /// Bring it out now on the monitor at `at` (screen px): the gallery's picture of it.
    pub fn reveal_now(at: POINT) {
        DOCK.with(|d| {
            if let Some(k) = d.borrow_mut().as_mut() {
                k.come_out(at);
                k.held = Some(Instant::now() + Duration::from_secs(4));
            }
        });
    }

    fn monitor(at: POINT) -> (HMONITOR, RECT, RECT) {
        unsafe {
            let mon = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut info);
            (mon, info.rcMonitor, info.rcWork)
        }
    }

    impl Dock {
        /// Whether `p` is pressed against this Dock's edge of the screen (an edge with no other
        /// screen past it).
        fn at_edge(&self, p: POINT) -> bool {
            let (_, m, _) = monitor(p);
            let (x, past) = match self.edge {
                Edge::Left => (p.x <= m.left, POINT { x: m.left - 1, y: p.y }),
                Edge::Right => (p.x >= m.right - 1, POINT { x: m.right, y: p.y }),
            };
            x && unsafe { MonitorFromPoint(past, MONITOR_DEFAULTTONULL) }.is_invalid()
        }

        /// Its place on the monitor it comes out on (centred along the edge, within the work
        /// area, its icons as large as fit), and its glass from what is behind it now.
        fn place_and_glass(&mut self) {
            let (x, y, w, h) = self.rect;
            let at = POINT { x: x + w as i32 / 2, y: y + h as i32 / 2 };
            let (_, m, wa) = monitor(at);
            let s = surface::scale_at(at.x, at.y);
            self.scale = s;
            self.layout = layout(&self.items, (wa.bottom - wa.top) as f32 / s - 16.0);
            let len = (self.layout.length * s).round() as i32;
            let thick = (self.layout.thick * s).round() as i32;
            let inset = (INSET * s).round() as i32;
            let top = wa.top + ((wa.bottom - wa.top) - len) / 2;
            let left = match self.edge {
                Edge::Left => m.left + inset,
                Edge::Right => m.right - inset - thick,
            };
            self.rect = (left, top, thick as usize, len as usize);
            // what is behind it, without itself (its items changed while it was out)
            let shown = unsafe { IsWindowVisible(self.surf.hwnd).as_bool() };
            if shown {
                unsafe {
                    let _ = ShowWindow(self.surf.hwnd, SW_HIDE);
                }
            }
            let radius = (self.layout.thick * 0.36).min(18.0) * s;
            let (c, margin) = surface::glass_panel(left, top, thick as usize, len as usize, radius, crate::glass::Kind::Tabs, s);
            if shown {
                unsafe {
                    let _ = ShowWindow(self.surf.hwnd, SW_SHOWNOACTIVATE);
                }
            }
            self.glass = Some((c, margin));
            self.dark = surface::glass_look().0;
        }

        fn come_out(&mut self, at: POINT) {
            // on the monitor of the push
            let (_, m, _) = monitor(at);
            self.rect = (m.left, m.top + (m.bottom - m.top) / 2, 1, 1);
            self.place_and_glass();
            self.shown.retarget(1.0, tokens::pick(tokens::WINDOW, 0.3), Curve::ARRIVE);
            self.left_at = None;
            crate::stage::lower(true); // Stage Manager strip slides down while the Dock is out
            self.push.reset();
            unsafe {
                let _ = ShowWindow(self.surf.hwnd, SW_SHOWNOACTIVATE);
                SetTimer(Some(self.surf.hwnd), TIMER, 16, None);
            }
        }

        fn go_away(&mut self) {
            crate::stage::lower(false); // Stage Manager strip comes back up
            self.shown.retarget(0.0, tokens::pick(tokens::WINDOW, 0.2), Curve::Accelerate);
            self.hover = None;
            self.held = None;
        }

        /// Screen point beside icon `i`, on the screen's side: where its menu opens.
        fn beside(&self, i: usize) -> POINT {
            let (x, y, w, _) = self.rect;
            let s = self.scale;
            let cy = y + ((self.layout.starts[i] + self.layout.icon / 2.0) * s).round() as i32;
            match self.edge {
                Edge::Left => POINT { x: x + w as i32 + (6.0 * s) as i32, y: cy },
                Edge::Right => POINT { x: x - (6.0 * s) as i32, y: cy },
            }
        }
    }

    /// A raw mouse move: added to the push when the pointer is held at the edge.
    fn on_raw(lp: LPARAM) {
        let mut raw = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        let got = unsafe { GetRawInputData(HRAWINPUT(lp.0 as *mut _), RID_INPUT, Some(&mut raw as *mut _ as *mut _), &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32) };
        if got == u32::MAX || raw.header.dwType != RIM_TYPEMOUSE.0 {
            return;
        }
        let m = unsafe { raw.data.mouse };
        if m.lLastX == 0 && m.usFlags.0 & 1 == 0 {
            return; // only up or down: nothing for or against a push
        }
        // MOUSE_MOVE_ABSOLUTE: a pen, a remote session, a virtual machine's pointer
        if m.usFlags.0 & 1 != 0 {
            let start = DOCK.with(|d| d.borrow_mut().as_mut().map(|k| !std::mem::replace(&mut k.absolute, true)).unwrap_or(false));
            if start {
                if let Some(h) = hwnd() {
                    unsafe {
                        SetTimer(Some(h), TIMER_REST, 100, None);
                    }
                }
            }
            return;
        }
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let now = Instant::now();
        DOCK.with(|d| {
            let mut b = d.borrow_mut();
            let Some(k) = b.as_mut() else { return };
            if k.shown.target() > 0.5 {
                return;
            }
            if !k.at_edge(p) {
                k.push.reset();
                return;
            }
            let outward = match k.edge {
                Edge::Left => -m.lLastX,
                Edge::Right => m.lLastX,
            } as f32;
            if k.push.add(outward, now) {
                k.come_out(p);
            }
        });
    }

    /// A pointer that moves the cursor directly: resting at the edge for a while brings it out.
    fn rest_tick() {
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        DOCK.with(|d| {
            let mut b = d.borrow_mut();
            let Some(k) = b.as_mut() else { return };
            if k.shown.target() > 0.5 || !k.at_edge(p) {
                k.resting = None;
                return;
            }
            let since = *k.resting.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(500) {
                k.resting = None;
                k.come_out(p);
            }
        });
    }

    /// While it is out: hover, going away once the pointer has left it, the slide.
    fn tick() {
        let mut cur = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cur);
        }
        DOCK.with(|d| {
            let mut b = d.borrow_mut();
            let Some(k) = b.as_mut() else { return };
            let (x, y, w, h) = k.rect;
            let s = k.scale;
            let reach = (24.0 * s) as i32;
            let over = cur.y >= y - reach
                && cur.y < y + h as i32 + reach
                && match k.edge {
                    Edge::Left => cur.x < x + w as i32 + reach,
                    Edge::Right => cur.x >= x - reach,
                };
            let held = k.held.is_some_and(|t| Instant::now() < t);
            // a menu of an item is open: the pointer may be on it as well as on the Dock; far from
            // both, the menu closes and the Dock goes (a menu still being asked of the Mac stays)
            let menu = crate::glassmenu::rect();
            let over_menu = menu.is_some_and(|r| cur.x >= r.left - reach && cur.x < r.right + reach && cur.y >= r.top - reach && cur.y < r.bottom + reach);
            if k.shown.target() > 0.5 && !(held && menu.is_none()) {
                if over || over_menu {
                    k.left_at = None;
                } else if k.left_at.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(if held { 250 } else { 450 }) {
                    if held {
                        crate::glassmenu::cancel();
                    }
                    k.go_away();
                }
            }
            let inside = cur.x >= x && cur.x < x + w as i32;
            let hover = if inside && k.shown.target() > 0.5 { icon_at(&k.items, &k.layout, (cur.y - y) as f32 / s) } else if held { k.hover } else { None };
            if hover != k.hover {
                k.hover = hover;
                let g = growth(k.items.len(), hover);
                for (a, t) in k.grow.iter_mut().zip(g) {
                    a.retarget(t, tokens::pick(tokens::PRESS, 0.6), Curve::SNAP);
                }
            }
        });
        draw();
    }

    fn draw() {
        DOCK.with(|d| {
            let mut b = d.borrow_mut();
            let Some(k) = b.as_mut() else { return };
            let now = Instant::now();
            let t = k.shown.value_at(now);
            if t <= 0.001 && k.shown.done_at(now) {
                unsafe {
                    let _ = ShowWindow(k.surf.hwnd, SW_HIDE);
                    let _ = KillTimer(Some(k.surf.hwnd), TIMER);
                }
                k.glass = None; // made again next time: what is behind may have changed
                return;
            }
            let Some((base, margin)) = k.glass.as_ref() else { return };
            let (x, y, _, _) = k.rect;
            let s = k.scale;
            let lay = k.layout.clone();
            // room beside it for the name of the icon under the pointer (on the screen's side)
            let room = (230.0 * s) as usize;
            let left = k.edge == Edge::Left;
            let mut c = Canvas::new(base.w + room, base.h);
            let bx = if left { 0 } else { room };
            c.composite(base, bx as isize, 0, 1.0);
            let (mx, my) = ((bx + *margin) as f32, *margin as f32);
            let (fg, fg2) = surface::glass_text(k.dark);
            let thick = lay.thick * s;
            let icon = lay.icon * s;
            for (i, it) in k.items.iter().enumerate() {
                let top = my + lay.starts[i] * s;
                if it.separator {
                    // the line between groups, across the Dock
                    let ly = top + DIVIDER * lay.icon / ICON * s / 2.0;
                    c.fill_round_rect(mx + 10.0 * s, ly - 0.5 * s, thick - 20.0 * s, 1.0 * s, 0.5 * s, fg2.fade(0.6));
                    continue;
                }
                let g = k.grow.get(i).map_or(1.0, |a| a.value_at(now));
                let size = icon * g;
                let cy = top + icon / 2.0;
                // grows away from the edge, as the Mac's Dock grows up from the bottom
                let ix = if left { mx + PAD * s } else { mx + thick - PAD * s - size };
                let iy = cy - size / 2.0;
                match k.icons.get(&it.icon) {
                    Some(ic) => c.draw(ic, ix, iy, size, size, 0.0, 1.0),
                    None if it.id.starts_with('\u{1}') => {
                        // Open an App: a magnifying glass on a disc of glass
                        c.fill_circle(ix + size / 2.0, iy + size / 2.0, size * 0.42, fg.fade(0.12));
                        crate::look::symbol(&mut c, crate::look::Symbol::Search, ix + size / 2.0, iy + size / 2.0, size * 0.42, fg.fade(0.85));
                    }
                    None => {
                        // until its picture comes (the Mac Desktop: a display)
                        let px = (ICON * s).round() as usize;
                        let ph = k.placeholders.entry(it.icon.clone()).or_insert_with(|| crate::launchui::placeholder(&it.icon, &it.name, px, s));
                        c.draw(ph, ix, iy, size, size, 0.0, 1.0);
                    }
                }
                if it.running {
                    let dx = if left { mx + 4.0 * s } else { mx + thick - 4.0 * s };
                    c.fill_circle(dx, cy, 2.0 * s, fg.fade(0.8));
                }
                if k.hover == Some(i) {
                    let mask = k.names.entry(it.name.clone()).or_insert_with(|| surface::text_mask(&it.name, (12.0 * s).round() as i32, 500, (200.0 * s) as usize));
                    let (mw, mh) = (mask.1 as f32, mask.2 as f32);
                    let (pw, ph) = (mw + 20.0 * s, mh + 10.0 * s);
                    let px = if left { mx + thick + 10.0 * s } else { mx - 10.0 * s - pw };
                    let py = cy - ph / 2.0;
                    c.fill_round_rect(px, py, pw, ph, ph / 2.0, if k.dark { Rgba::rgba(40, 40, 46, 230) } else { Rgba::rgba(250, 250, 252, 235) });
                    c.fill_mask(&mask.0, mask.1, (px + 10.0 * s).round() as isize, (py + 5.0 * s).round() as isize, fg);
                }
            }
            // slides in from past the edge
            let travel = (lay.thick + INSET) * s + *margin as f32;
            let off = ((1.0 - t) * travel).round() as i32;
            let a = (t.clamp(0.0, 1.0).sqrt() * 255.0) as u8;
            let wx = if left { x - *margin as i32 - off } else { x - *margin as i32 - room as i32 + off };
            k.surf.present(&c, wx, y - *margin as i32, a, None);
        });
    }

    /// The item under the pointer, and where its menu goes.
    fn picked() -> Option<(String, POINT)> {
        DOCK.with(|d| d.borrow().as_ref().and_then(|k| k.hover.and_then(|i| k.items.get(i).map(|it| (it.id.clone(), k.beside(i))))))
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_INPUT => {
                on_raw(lp);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_TIMER if wp.0 == TIMER_REST => {
                rest_tick();
                LRESULT(0)
            }
            WM_TIMER => {
                tick();
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONUP => {
                if let Some((id, _)) = picked() {
                    DOCK.with(|d| d.borrow_mut().as_mut().map(|k| k.go_away()));
                    // called with nothing borrowed: it may open windows
                    let cb = DOCK.with(|d| d.borrow_mut().as_mut().map(|k| std::mem::replace(&mut k.on_click, Box::new(|_| {}))));
                    if let Some(cb) = cb {
                        cb(id);
                        DOCK.with(|d| {
                            if let Some(k) = d.borrow_mut().as_mut() {
                                k.on_click = cb;
                            }
                        });
                    }
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                if let Some((id, at)) = picked() {
                    // its menu, as the Mac shows it: the Dock stays out meanwhile
                    hold(true);
                    let cb = DOCK.with(|d| d.borrow_mut().as_mut().map(|k| std::mem::replace(&mut k.on_menu, Box::new(|_, _| {}))));
                    if let Some(cb) = cb {
                        cb(id, at);
                        DOCK.with(|d| {
                            if let Some(k) = d.borrow_mut().as_mut() {
                                k.on_menu = cb;
                            }
                        });
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(spec: &str) -> Vec<DockItem> {
        spec.chars().enumerate().map(|(i, c)| DockItem { id: i.to_string(), separator: c == '|', ..Default::default() }).collect()
    }

    #[test]
    fn icons_and_lines_are_laid_out_and_found() {
        let it = items("ab|c");
        let l = layout(&it, 2000.0);
        assert_eq!(l.icon, ICON);
        assert_eq!(l.starts, vec![PAD, PAD + ICON + GAP, PAD + 2.0 * (ICON + GAP), PAD + 2.0 * (ICON + GAP) + DIVIDER + GAP]);
        assert_eq!(l.length, 2.0 * PAD + 3.0 * ICON + DIVIDER + 3.0 * GAP);
        assert_eq!(icon_at(&it, &l, PAD + 1.0), Some(0));
        assert_eq!(icon_at(&it, &l, l.starts[2] + 2.0), None, "never the line");
        assert_eq!(icon_at(&it, &l, l.starts[3] + ICON / 2.0), Some(3));
        assert_eq!(icon_at(&it, &l, l.length + 20.0), None);
    }

    #[test]
    fn many_icons_shrink_to_fit_the_screen() {
        let it = items(&"a".repeat(30));
        let l = layout(&it, 900.0);
        assert!(l.icon < ICON && l.icon >= MIN_ICON, "{}", l.icon);
        assert!((l.length - 900.0).abs() < 0.5, "{}", l.length);
        assert_eq!(l.thick, l.icon + 2.0 * PAD);
        // never below the smallest size, even if it then runs past
        assert_eq!(layout(&items(&"a".repeat(200)), 900.0).icon, MIN_ICON);
    }

    #[test]
    fn the_icon_under_the_pointer_grows_most() {
        assert_eq!(growth(4, Some(1)), vec![1.08, 1.22, 1.08, 1.0]);
        assert_eq!(growth(2, None), vec![1.0, 1.0]);
    }

    #[test]
    fn only_a_deliberate_push_brings_it_out() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // passing by: a little push, then moving along the edge and away
        let mut p = Push::default();
        assert!(!p.add(30.0, ms(0)));
        assert!(!p.add(0.0, ms(10)));
        assert!(!p.add(-5.0, ms(20)), "back into the screen ends it");
        assert!(!p.add(60.0, ms(30)), "started again: not deep enough");
        // a push in steps, without a pause, is
        let mut p = Push::default();
        let mut out = false;
        for i in 0..3 {
            out |= p.add(25.0, ms(i * 16));
        }
        assert!(out, "a short push, three moves");
        // the same amount with pauses between is not
        let mut p = Push::default();
        let mut out = false;
        for i in 0..8 {
            out |= p.add(25.0, ms(i * 400));
        }
        assert!(!out);
        assert_eq!(Edge::from_setting(0), None);
        assert_eq!(Edge::from_setting(2), Some(Edge::Right));
    }
}
