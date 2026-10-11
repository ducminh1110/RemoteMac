//! The Dock: a column of Liquid Glass at the left or right edge of the screen (Settings) with
//! the Mac Desktop, the Mac apps open here and Open an App. It stays out of the way until the
//! pointer is pushed against that edge (the mouse keeps moving outward while the pointer cannot
//! go further, as on the Mac when its Dock hides itself): passing by the edge, or resting there,
//! leaves it hidden. It slides out, its icons grow a little under the pointer with their name
//! beside them, open apps have a dot on the edge's side; it slides away once the pointer has left
//! it. It is drawn here (no picture of the Mac's own Dock), and changes nothing of Windows.

use std::time::{Duration, Instant};

/// One icon of the Dock.
#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    pub id: String,
    pub name: String,
    pub running: bool,
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

/// Icon size, spacing, padding, and the room of the line before the last icon (logical px).
pub const ICON: f32 = 46.0;
pub const GAP: f32 = 10.0;
pub const PAD: f32 = 12.0;
pub const DIVIDER: f32 = 12.0;
/// The Dock's thickness across the edge (logical px), and its distance from the edge.
pub const THICK: f32 = ICON + 2.0 * PAD;
pub const INSET: f32 = 8.0;

/// The Dock's length along the edge for `n` icons, the last one after a dividing line.
pub fn length(n: usize) -> f32 {
    PAD * 2.0 + n as f32 * ICON + n.saturating_sub(1) as f32 * GAP + if n > 1 { DIVIDER } else { 0.0 }
}

/// Where icon `i` of `n` starts along the Dock.
pub fn start(n: usize, i: usize) -> f32 {
    PAD + i as f32 * (ICON + GAP) + if n > 1 && i == n - 1 { DIVIDER } else { 0.0 }
}

/// The icon at `along` (logical px from the Dock's top).
pub fn icon_at(n: usize, along: f32) -> Option<usize> {
    (0..n).find(|&i| {
        let s = start(n, i);
        along >= s - GAP / 2.0 && along < s + ICON + GAP / 2.0
    })
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
    /// How far to push (about 4 mm of a usual mouse, a short swipe on a touchpad).
    pub const NEEDED: f32 = 160.0;
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
pub use win::{close, register, reveal_now, sync};

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

    struct Dock {
        surf: Surface,
        edge: Edge,
        items: Vec<DockItem>,
        icons: HashMap<String, Canvas>,
        placeholders: HashMap<String, Canvas>,
        names: HashMap<String, Mask>,
        /// the monitor it is out on, its scale, and its rectangle there (device px: x, y, w, h)
        scale: f32,
        rect: (i32, i32, usize, usize),
        /// 0 hidden past the edge .. 1 out
        shown: Anim,
        grow: Vec<Anim>,
        hover: Option<usize>,
        /// the glass made when it comes out (what is behind it then), and its margin
        glass: Option<(Canvas, usize)>,
        left_at: Option<Instant>,
        /// kept out until then whatever the pointer does (the gallery's picture)
        held: Option<Instant>,
        push: Push,
        /// the pointer moves the cursor directly (no relative moves to add up): resting at the
        /// edge brings the Dock out instead, since when
        absolute: bool,
        resting: Option<Instant>,
        on_click: Box<dyn Fn(String)>,
        dark: bool,
    }

    thread_local! {
        static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_HAND);
    }

    /// The Dock at `edge` with these apps (made the first time; moved to another edge when the
    /// setting changes); `on_click(id)` opens or switches to one.
    pub fn sync(hinst: HINSTANCE, edge: Edge, items: Vec<DockItem>, icons: &HashMap<String, (u32, Vec<u8>)>, on_click: impl Fn(String) + 'static) {
        let canvas_of = |id: &str| icons.get(id).filter(|(s, px)| px.len() == (*s * *s * 4) as usize).map(|(s, px)| Canvas::from_rgba(*s as usize, *s as usize, px));
        let exists = DOCK.with(|d| d.borrow().as_ref().map(|k| k.edge));
        if exists.is_some_and(|e| e != edge) {
            close();
        }
        if exists == Some(edge) {
            DOCK.with(|d| {
                if let Some(k) = d.borrow_mut().as_mut() {
                    for it in &items {
                        if !k.icons.contains_key(&it.id) {
                            if let Some(c) = canvas_of(&it.id) {
                                k.icons.insert(it.id.clone(), c);
                            }
                        }
                    }
                    if k.items != items {
                        k.grow = items.iter().map(|_| Anim::at(1.0)).collect();
                        k.items = items;
                        k.hover = None;
                        // a new length: its glass is made again (when it is out, at once)
                        if k.shown.target() > 0.5 {
                            k.place_and_glass();
                        } else {
                            k.glass = None;
                        }
                    }
                }
            });
            return;
        }
        let Some(surf) = Surface::new(hinst, CLASS, "Taro Dock", None, true, false) else { return };
        let hwnd = surf.hwnd;
        let icons_c = items.iter().filter_map(|it| canvas_of(&it.id).map(|c| (it.id.clone(), c))).collect();
        DOCK.with(|d| {
            *d.borrow_mut() = Some(Dock {
                surf,
                edge,
                grow: items.iter().map(|_| Anim::at(1.0)).collect(),
                items,
                icons: icons_c,
                placeholders: HashMap::new(),
                names: HashMap::new(),
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
        /// area), and its glass from what is behind it now.
        fn place_and_glass(&mut self) {
            let (x, y, w, h) = self.rect;
            let at = POINT { x: x + w as i32 / 2, y: y + h as i32 / 2 };
            let (_, m, wa) = monitor(at);
            let s = surface::scale_at(at.x, at.y);
            self.scale = s;
            let len = ((length(self.items.len()) * s).round() as i32).min(wa.bottom - wa.top - (16.0 * s) as i32).max(1);
            let thick = (THICK * s).round() as i32;
            let inset = (INSET * s).round() as i32;
            let top = wa.top + ((wa.bottom - wa.top) - len) / 2;
            let left = match self.edge {
                Edge::Left => m.left + inset,
                Edge::Right => m.right - inset - thick,
            };
            self.rect = (left, top, thick as usize, len as usize);
            // what is behind it, without itself (its apps changed while it was out)
            let shown = unsafe { IsWindowVisible(self.surf.hwnd).as_bool() };
            if shown {
                unsafe {
                    let _ = ShowWindow(self.surf.hwnd, SW_HIDE);
                }
            }
            let (c, margin) = surface::glass_panel(left, top, thick as usize, len as usize, 18.0 * s, crate::glass::Kind::Tabs, s);
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
            self.push.reset();
            unsafe {
                let _ = ShowWindow(self.surf.hwnd, SW_SHOWNOACTIVATE);
                SetTimer(Some(self.surf.hwnd), TIMER, 16, None);
            }
        }

        fn go_away(&mut self) {
            self.shown.retarget(0.0, tokens::pick(tokens::WINDOW, 0.2), Curve::Accelerate);
            self.hover = None;
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
                if let Some(h) = DOCK.with(|d| d.borrow().as_ref().map(|k| k.surf.hwnd)) {
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
            if since.elapsed() > Duration::from_millis(900) {
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
            if k.shown.target() > 0.5 && !held {
                if over {
                    k.left_at = None;
                } else if k.left_at.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(450) {
                    k.go_away();
                }
            }
            let inside = cur.x >= x && cur.x < x + w as i32;
            let hover = if inside && k.shown.target() > 0.5 { icon_at(k.items.len(), (cur.y - y) as f32 / s) } else { None };
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
            let n = k.items.len();
            // room beside it for the name of the icon under the pointer (on the screen's side)
            let room = (230.0 * s) as usize;
            let left = k.edge == Edge::Left;
            let mut c = Canvas::new(base.w + room, base.h);
            let bx = if left { 0 } else { room };
            c.composite(base, bx as isize, 0, 1.0);
            let (mx, my) = ((bx + *margin) as f32, *margin as f32);
            let (fg, fg2) = surface::glass_text(k.dark);
            let thick = THICK * s;
            for (i, it) in k.items.iter().enumerate() {
                let g = k.grow.get(i).map_or(1.0, |a| a.value_at(now));
                let size = ICON * s * g;
                let cy = my + (start(n, i) + ICON / 2.0) * s;
                // grows away from the edge, as the Mac's Dock grows up from the bottom
                let base_x = if left { mx + PAD * s } else { mx + thick - PAD * s - ICON * s };
                let ix = if left { base_x } else { base_x + ICON * s - size };
                let iy = cy - size / 2.0;
                match k.icons.get(&it.id) {
                    Some(ic) => c.draw(ic, ix, iy, size, size, 0.0, 1.0),
                    None if it.id.starts_with('\u{1}') => {
                        // Open an App: a magnifying glass on a disc of glass
                        c.fill_circle(ix + size / 2.0, iy + size / 2.0, size * 0.42, fg.fade(0.12));
                        crate::look::symbol(&mut c, crate::look::Symbol::Search, ix + size / 2.0, iy + size / 2.0, size * 0.42, fg.fade(0.85));
                    }
                    None => {
                        // until the Mac's icon comes (the Mac Desktop: a display)
                        let px = (ICON * s).round() as usize;
                        let ph = k.placeholders.entry(it.id.clone()).or_insert_with(|| crate::launchui::placeholder(&it.id, &it.name, px, s));
                        c.draw(ph, ix, iy, size, size, 0.0, 1.0);
                    }
                }
                if it.running {
                    let dx = if left { mx + 4.0 * s } else { mx + thick - 4.0 * s };
                    c.fill_circle(dx, cy, 2.0 * s, fg.fade(0.8));
                }
                if n > 1 && i == n - 1 {
                    let ly = my + (start(n, i) - GAP / 2.0 - DIVIDER / 2.0) * s;
                    c.fill_round_rect(mx + 14.0 * s, ly - 0.5 * s, thick - 28.0 * s, 1.0 * s, 0.5 * s, fg2.fade(0.6));
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
            let travel = (THICK + INSET) * s + *margin as f32;
            let off = ((1.0 - t) * travel).round() as i32;
            let a = (t.clamp(0.0, 1.0).sqrt() * 255.0) as u8;
            let wx = if left { x - *margin as i32 - off } else { x - *margin as i32 - room as i32 + off };
            k.surf.present(&c, wx, y - *margin as i32, a, None);
        });
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
                let pick = DOCK.with(|d| d.borrow().as_ref().and_then(|k| k.hover.and_then(|i| k.items.get(i).map(|it| it.id.clone()))));
                if let Some(id) = pick {
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
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_laid_out_and_found() {
        assert_eq!(length(1), PAD * 2.0 + ICON);
        assert_eq!(length(3), PAD * 2.0 + 3.0 * ICON + 2.0 * GAP + DIVIDER);
        assert_eq!(icon_at(3, PAD + 1.0), Some(0));
        assert_eq!(icon_at(3, start(3, 1) + 2.0), Some(1));
        // the last one comes after the line
        assert_eq!(start(3, 2), PAD + 2.0 * (ICON + GAP) + DIVIDER);
        assert_eq!(icon_at(3, start(3, 2) + ICON / 2.0), Some(2));
        assert_eq!(icon_at(3, length(3) + 20.0), None);
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
        assert!(!p.add(140.0, ms(30)), "started again: not deep enough");
        // a push in steps, without a pause, is
        let mut p = Push::default();
        let mut out = false;
        for i in 0..8 {
            out |= p.add(25.0, ms(i * 16));
        }
        assert!(out);
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
