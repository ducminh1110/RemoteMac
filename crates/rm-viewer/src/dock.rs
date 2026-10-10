//! Desktop Fusion's Dock (experimental, off unless chosen in Settings): the Mac apps open from
//! this PC, the Mac Desktop and MacBridge Search, in a glass bar that slides up from the bottom
//! of the screen when the pointer rests at the bottom edge (as the Mac's Dock does when it hides
//! itself), and slides away again. Icons grow a little under the pointer; open apps have a dot.
//! It changes nothing of Windows: no taskbar, no wallpaper, no reserved screen space, and it is
//! gone when MacBridge closes or the setting is turned off.

/// One icon of the Dock.
#[derive(Debug, Clone, PartialEq)]
pub struct DockItem {
    pub id: String,
    pub name: String,
    pub running: bool,
}

/// Icon size and spacing (logical px).
pub const ICON: f32 = 46.0;
pub const GAP: f32 = 10.0;
pub const PAD: f32 = 12.0;

/// The Dock's width for `n` icons (logical px).
pub fn width(n: usize) -> f32 {
    PAD * 2.0 + n as f32 * ICON + n.saturating_sub(1) as f32 * GAP
}

/// The icon under `x` (logical px from the Dock's left).
pub fn icon_at(n: usize, x: f32) -> Option<usize> {
    let rel = x - PAD;
    if rel < 0.0 {
        return None;
    }
    let i = (rel / (ICON + GAP)) as usize;
    (i < n && rel - i as f32 * (ICON + GAP) <= ICON).then_some(i)
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

#[cfg(windows)]
pub use win::{close, register, sync};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::motion::{tokens, Anim, Curve};
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{self, Surface};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("RmDock");
    const TIMER: usize = 1;

    type Mask = (Vec<u8>, usize, usize);

    struct Dock {
        surf: Surface,
        items: Vec<DockItem>,
        icons: HashMap<String, Canvas>,
        names: HashMap<String, Mask>,
        scale: f32,
        /// the work area's bottom centre it sits at
        anchor: (i32, i32),
        /// 0 hidden below the edge .. 1 shown
        shown: Anim,
        grow: Vec<Anim>,
        hover: Option<usize>,
        /// the glass for the current width (made when it is revealed)
        glass: Option<(usize, Canvas, usize)>,
        left_at: Option<Instant>,
        on_click: Box<dyn Fn(String)>,
        dark: bool,
    }

    thread_local! {
        static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_HAND);
    }

    /// Show the Dock with these apps (creating it the first time); `on_click(id)` opens or
    /// switches to one.
    pub fn sync(hinst: HINSTANCE, items: Vec<DockItem>, icons: &HashMap<String, (u32, Vec<u8>)>, on_click: impl Fn(String) + 'static) {
        let canvas_of = |id: &str| icons.get(id).filter(|(s, px)| px.len() == (*s * *s * 4) as usize).map(|(s, px)| Canvas::from_rgba(*s as usize, *s as usize, px));
        let exists = DOCK.with(|d| d.borrow().is_some());
        if exists {
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
                        k.glass = None;
                        k.hover = None;
                    }
                }
            });
            return;
        }
        let Some(surf) = Surface::new(hinst, CLASS, "MacBridge Dock", None, true, false) else { return };
        let primary = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let _ = GetMonitorInfoW(primary, &mut info);
        }
        let wa = info.rcWork;
        let anchor = ((wa.left + wa.right) / 2, wa.bottom);
        let scale = surface::scale_at(anchor.0, anchor.1 - 10);
        let icons_c = items.iter().filter_map(|it| canvas_of(&it.id).map(|c| (it.id.clone(), c))).collect();
        let (dark, _, _) = surface::glass_look();
        let hwnd = surf.hwnd;
        DOCK.with(|d| {
            *d.borrow_mut() = Some(Dock {
                surf,
                grow: items.iter().map(|_| Anim::at(1.0)).collect(),
                items,
                icons: icons_c,
                names: HashMap::new(),
                scale,
                anchor,
                shown: Anim::at(0.0),
                hover: None,
                glass: None,
                left_at: None,
                on_click: Box::new(on_click),
                dark,
            })
        });
        unsafe {
            SetTimer(Some(hwnd), TIMER, 40, None);
        }
    }

    /// Remove the Dock (the setting was turned off, or the Mac is gone).
    pub fn close() {
        let d = DOCK.with(|d| d.borrow_mut().take());
        drop(d);
    }

    /// The Dock's rectangle when shown (screen px): (x, y, w, h).
    fn rect(k: &Dock) -> (i32, i32, usize, usize) {
        let s = k.scale;
        let w = (width(k.items.len()) * s).round() as usize;
        let h = ((ICON + 2.0 * PAD) * s).round() as usize;
        (k.anchor.0 - w as i32 / 2, k.anchor.1 - h as i32 - (8.0 * s) as i32, w, h)
    }

    fn tick() {
        let mut cur = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cur);
        }
        let click = DOCK.with(|d| {
            let mut b = d.borrow_mut();
            let k = b.as_mut()?;
            let (x, y, w, h) = rect(k);
            let s = k.scale;
            let over = cur.x >= x && cur.x < x + w as i32 && cur.y >= y - (40.0 * s) as i32 && cur.y <= k.anchor.1 + 2;
            let at_edge = cur.y >= k.anchor.1 - 2 && cur.y <= k.anchor.1 + 2 && (cur.x - k.anchor.0).abs() < (w as i32 / 2 + 40);
            let showing = k.shown.target() > 0.5;
            if !showing && at_edge {
                // reveal: the glass is made from what is behind it now
                if k.glass.is_none() {
                    let (c, m) = surface::glass_panel(x, y, w, h, 18.0 * s, crate::glass::Kind::Tabs, s);
                    k.glass = Some((w, c, m));
                }
                k.shown.retarget(1.0, tokens::pick(tokens::WINDOW, 0.3), Curve::ARRIVE);
                k.left_at = None;
                unsafe {
                    let _ = ShowWindow(k.surf.hwnd, SW_SHOWNOACTIVATE);
                }
            } else if showing && !over {
                let since = *k.left_at.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_millis(700) {
                    k.shown.retarget(0.0, tokens::pick(tokens::WINDOW, 0.2), Curve::Accelerate);
                    k.glass = None; // made again next time (what is behind may have changed)
                }
            } else if over {
                k.left_at = None;
            }
            // which icon the pointer is over
            let hover = if over && cur.y >= y { icon_at(k.items.len(), (cur.x - x) as f32 / s) } else { None };
            if hover != k.hover {
                k.hover = hover;
                let g = growth(k.items.len(), hover);
                for (a, t) in k.grow.iter_mut().zip(g) {
                    a.retarget(t, tokens::pick(tokens::PRESS, 0.6), Curve::SNAP);
                }
            }
            None::<String>
        });
        let _ = click;
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
                }
                return;
            }
            let Some((_, base, margin)) = k.glass.as_ref() else { return };
            let (x, y, w, _) = rect(k);
            let s = k.scale;
            let label_room = (34.0 * s) as usize;
            let mut c = Canvas::new(base.w, base.h + label_room);
            c.composite(base, 0, label_room as isize, 1.0);
            let (mx, my) = (*margin as f32, *margin as f32 + label_room as f32);
            let (fg, _) = surface::glass_text(k.dark);
            for (i, it) in k.items.iter().enumerate() {
                let g = k.grow.get(i).map_or(1.0, |a| a.value_at(now));
                let size = ICON * s * g;
                let cx = mx + (PAD + i as f32 * (ICON + GAP) + ICON / 2.0) * s;
                let bottom = my + (PAD + ICON) * s;
                let (ix, iy) = (cx - size / 2.0, bottom - size);
                match k.icons.get(&it.id) {
                    Some(ic) => c.draw(ic, ix, iy, size, size, 0.0, 1.0),
                    None => c.fill_round_rect(ix, iy, size, size, size * 0.22, fg.fade(0.25)),
                }
                if it.running {
                    c.fill_circle(cx, bottom + 5.0 * s, 2.0 * s, fg.fade(0.8));
                }
                if k.hover == Some(i) {
                    let key = it.name.clone();
                    let mask = k.names.entry(key).or_insert_with(|| surface::text_mask(&it.name, (12.0 * s).round() as i32, 500, (220.0 * s) as usize));
                    let (mw, mh) = (mask.1 as f32, mask.2 as f32);
                    let (px, py, pw, ph) = (cx - mw / 2.0 - 10.0 * s, my - 6.0 * s - mh - 10.0 * s, mw + 20.0 * s, mh + 10.0 * s);
                    c.fill_round_rect(px, py, pw, ph, ph / 2.0, if k.dark { Rgba::rgba(40, 40, 46, 230) } else { Rgba::rgba(250, 250, 252, 235) });
                    c.fill_mask(&mask.0, mask.1, (px + 10.0 * s).round() as isize, (py + 5.0 * s).round() as isize, fg);
                }
            }
            // slides up from below the edge
            let off = ((1.0 - t) * (c.h as f32 - label_room as f32)).round() as i32;
            let a = (t.clamp(0.0, 1.0).sqrt() * 255.0) as u8;
            k.surf.present(&c, x - *margin as i32, y - *margin as i32 - label_room as i32 + off, a, None);
            let _ = w;
        });
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_TIMER => {
                tick();
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONUP => {
                let pick = DOCK.with(|d| d.borrow().as_ref().and_then(|k| k.hover.and_then(|i| k.items.get(i).map(|it| it.id.clone()))));
                if let Some(id) = pick {
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
        assert_eq!(width(0), PAD * 2.0);
        assert_eq!(width(3), PAD * 2.0 + 3.0 * ICON + 2.0 * GAP);
        assert_eq!(icon_at(3, PAD + 1.0), Some(0));
        assert_eq!(icon_at(3, PAD + ICON + GAP / 2.0), None, "between icons");
        assert_eq!(icon_at(3, PAD + 2.0 * (ICON + GAP) + 5.0), Some(2));
        assert_eq!(icon_at(3, PAD + 3.0 * (ICON + GAP) + 5.0), None);
        assert_eq!(icon_at(3, 2.0), None);
    }

    #[test]
    fn the_icon_under_the_pointer_grows_most() {
        assert_eq!(growth(4, Some(1)), vec![1.08, 1.22, 1.08, 1.0]);
        assert_eq!(growth(2, None), vec![1.0, 1.0]);
    }
}
