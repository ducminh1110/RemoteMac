//! A Liquid Glass menu (the navigation ball's), in place of the plain Windows popup menu: rows
//! with a check mark and a shortcut, the row under the pointer or chosen with the arrow keys
//! highlighted in the accent colour, separators. It opens with a short spring from where it
//! was asked for, works with the mouse and the keyboard (arrows, Enter, Escape), and closes when
//! something else is clicked. `show` returns what was chosen.

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Action { id: u32, label: String, shortcut: Option<String>, checked: bool, enabled: bool },
    Separator,
}

impl Item {
    pub fn action(id: u32, label: &str) -> Item {
        Item::Action { id, label: label.into(), shortcut: None, checked: false, enabled: true }
    }

    pub fn with_shortcut(self, s: &str) -> Item {
        match self {
            Item::Action { id, label, checked, enabled, .. } => Item::Action { id, label, shortcut: Some(s.into()), checked, enabled },
            x => x,
        }
    }

    pub fn checked(self, on: bool) -> Item {
        match self {
            Item::Action { id, label, shortcut, enabled, .. } => Item::Action { id, label, shortcut, checked: on, enabled },
            x => x,
        }
    }
}

/// Row geometry (logical px).
const ROW: f32 = 28.0;
const SEP: f32 = 11.0;
const PAD: f32 = 6.0;
#[cfg_attr(not(windows), allow(dead_code))]
const WIDTH: f32 = 290.0;

/// The rows' tops and heights (logical px, from the top of the menu).
pub fn layout(items: &[Item]) -> (Vec<(f32, f32)>, f32) {
    let mut y = PAD;
    let mut rows = vec![];
    for it in items {
        let h = if matches!(it, Item::Separator) { SEP } else { ROW };
        rows.push((y, h));
        y += h;
    }
    (rows, y + PAD)
}

/// The row at `y` (logical px from the menu's top) that can be chosen.
pub fn row_at(items: &[Item], y: f32) -> Option<usize> {
    let (rows, _) = layout(items);
    rows.iter().position(|(t, h)| y >= *t && y < t + h).filter(|&i| matches!(items[i], Item::Action { enabled: true, .. }))
}

/// The next row that can be chosen from `from`, going `dir` (+1 down, -1 up), wrapping.
pub fn step(items: &[Item], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = items.len() as i32;
    let mut i = from.map_or(if dir > 0 { -1 } else { n }, |f| f as i32);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if matches!(items[i as usize], Item::Action { enabled: true, .. }) {
            return Some(i as usize);
        }
    }
    None
}

#[cfg(windows)]
pub use win::{register, show};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::motion::{tokens, Anim, Curve};
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{self, Surface};
    use windows::Win32::UI::Controls::WM_MOUSELEAVE;
    use std::cell::RefCell;
    use std::time::Instant;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("RmGlassMenu");
    const TIMER: usize = 1;

    /// A line of text drawn as a coverage mask (mask, width, height).
    type Mask = (Vec<u8>, usize, usize);

    struct Open {
        surf: Surface,
        items: Vec<Item>,
        hover: Option<usize>,
        result: Option<Option<u32>>,
        scale: f32,
        /// the glass body with its shadow (still), and where the menu itself starts in it
        base: Canvas,
        margin: usize,
        pos: (i32, i32),
        /// each row's label and shortcut, drawn once
        texts: Vec<Option<(Mask, Option<Mask>)>>,
        pop: Anim,
        /// the corner it grows from (0..1 of the width, 0..1 of the height)
        origin: (f32, f32),
        dark: bool,
        accent: Rgba,
        since: Instant,
    }

    thread_local! {
        static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_ARROW);
    }

    /// Open the menu at `at` (screen px), to the left of it when `left`; returns the id chosen, or
    /// None when it was closed without a choice.
    pub fn show(hinst: HINSTANCE, owner: HWND, at: POINT, left: bool, items: Vec<Item>) -> Option<u32> {
        let scale = surface::scale_at(at.x, at.y);
        let (_, total) = layout(&items);
        let (w, h) = ((WIDTH * scale).round() as usize, (total * scale).round() as usize);
        let wa = surface::work_area_at(at.x, at.y);
        let mut x = if left { at.x - w as i32 } else { at.x };
        let mut y = at.y;
        x = x.clamp(wa.left + 4, (wa.right - w as i32 - 4).max(wa.left));
        if y + h as i32 > wa.bottom - 4 {
            y = (at.y - h as i32).max(wa.top + 4);
        }
        let origin = (if left { 1.0 } else { 0.0 }, if y < at.y { 1.0 } else { 0.0 });
        // the glass is made from what is behind it now, before it shows
        let (base, margin) = surface::glass_panel(x, y, w, h, 12.0 * scale, crate::glass::Kind::Sheet, scale);
        let (dark, accent, _) = surface::glass_look();
        let surf = Surface::new(hinst, CLASS, "Menu", Some(owner), true, true)?;
        let texts = items
            .iter()
            .map(|it| match it {
                Item::Action { label, shortcut, .. } => {
                    let l = surface::text_mask(label, (13.0 * scale).round() as i32, 400, (WIDTH * 0.62 * scale) as usize);
                    let s = shortcut.as_ref().map(|s| surface::text_mask(s, (12.0 * scale).round() as i32, 400, (WIDTH * 0.4 * scale) as usize));
                    Some((l, s))
                }
                Item::Separator => None,
            })
            .collect();
        let hwnd = surf.hwnd;
        OPEN.with(|o| {
            *o.borrow_mut() = Some(Open {
                surf,
                items,
                hover: None,
                result: None,
                scale,
                base,
                margin,
                pos: (x, y),
                texts,
                pop: Anim::new(0.0, 1.0, tokens::pick(tokens::POPOVER, 0.4), Curve::SNAP),
                origin,
                dark,
                accent,
                since: Instant::now(),
            })
        });
        draw();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            SetTimer(Some(hwnd), TIMER, 16, None);
            // the menu's own loop until something is chosen or it is dismissed
            let mut msg = MSG::default();
            while OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| m.result.is_none())) {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let open = OPEN.with(|o| o.borrow_mut().take());
        let result = open.as_ref().and_then(|m| m.result.flatten());
        if let Some(m) = open {
            unsafe {
                let _ = KillTimer(Some(m.surf.hwnd), TIMER);
            }
            drop(m);
        }
        result
    }

    fn draw() {
        OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let Some(m) = b.as_mut() else { return };
            let s = m.scale;
            let mut c = m.base.clone();
            let (mx, my) = (m.margin as f32, m.margin as f32);
            let w = c.w as f32 - 2.0 * mx;
            let (rows, _) = layout(&m.items);
            let (fg, fg2) = surface::glass_text(m.dark);
            for (i, (it, (top, h))) in m.items.iter().zip(rows).enumerate() {
                let (top, h) = (my + top * s, h * s);
                match it {
                    Item::Separator => {
                        c.fill_round_rect(mx + 12.0 * s, top + h / 2.0, w - 24.0 * s, s.max(1.0), 0.0, if m.dark { Rgba::WHITE.alpha(0.14) } else { Rgba::BLACK.alpha(0.10) });
                    }
                    Item::Action { checked, enabled, .. } => {
                        let lit = m.hover == Some(i) && *enabled;
                        if lit {
                            c.fill_round_rect(mx + PAD * s, top, w - 2.0 * PAD * s, h, 7.0 * s, m.accent);
                        }
                        let col = if lit { Rgba::WHITE } else if *enabled { fg } else { fg2.fade(0.6) };
                        if *checked {
                            // a check mark
                            let (cx, cy) = (mx + 20.0 * s, top + h / 2.0);
                            c.fill_capsule(cx - 4.5 * s, cy, cx - 1.5 * s, cy + 3.5 * s, 0.9 * s, col);
                            c.fill_capsule(cx - 1.5 * s, cy + 3.5 * s, cx + 4.5 * s, cy - 4.0 * s, 0.9 * s, col);
                        }
                        if let Some(Some((label, short))) = m.texts.get(i) {
                            let (mask, mw, mh) = label;
                            c.fill_mask(mask, *mw, (mx + 34.0 * s) as isize, (top + (h - *mh as f32) / 2.0).round() as isize, col);
                            if let Some((mask, sw, sh)) = short {
                                let x = mx + w - 14.0 * s - *sw as f32;
                                c.fill_mask(mask, *sw, x.round() as isize, (top + (h - *sh as f32) / 2.0).round() as isize, if lit { Rgba::WHITE.alpha(0.85) } else { fg2 });
                            }
                        }
                    }
                }
            }
            // opening: grows from the corner it was asked at, and fades in
            let now = Instant::now();
            let k = m.pop.value_at(now).clamp(0.0, 1.0);
            let shown = if k < 0.999 {
                let mut f = Canvas::new(c.w, c.h);
                let z = 0.92 + 0.08 * k;
                let (ow, oh) = (c.w as f32, c.h as f32);
                let (ox, oy) = (mx + m.origin.0 * w, my + m.origin.1 * (oh - 2.0 * my));
                f.draw(&c, ox - ox * z, oy - oy * z, ow * z, oh * z, 0.0, k);
                f
            } else {
                c
            };
            let (x, y) = (m.pos.0 - m.margin as i32, m.pos.1 - m.margin as i32);
            m.surf.present(&shown, x, y, 255, None);
            let _ = m.since;
        });
    }

    fn finish(choice: Option<u32>) {
        OPEN.with(|o| {
            if let Some(m) = o.borrow_mut().as_mut() {
                if m.result.is_none() {
                    m.result = Some(choice);
                }
            }
        });
    }

    fn hover_at(lp: LPARAM) -> Option<usize> {
        let (x, y) = ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
        OPEN.with(|o| {
            let b = o.borrow();
            let m = b.as_ref()?;
            let (mx, s) = (m.margin as f32, m.scale);
            let w = m.base.w as f32 - 2.0 * mx;
            if x < mx || x > mx + w {
                return None;
            }
            row_at(&m.items, (y - mx) / s)
        })
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_TIMER => {
                let animating = OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| !m.pop.done()));
                draw();
                if !animating {
                    let _ = KillTimer(Some(hwnd), TIMER);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let row = hover_at(lp);
                let changed = OPEN.with(|o| o.borrow_mut().as_mut().is_some_and(|m| std::mem::replace(&mut m.hover, row) != row));
                if changed {
                    draw();
                }
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                let _ = TrackMouseEvent(&mut t);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                OPEN.with(|o| o.borrow_mut().as_mut().map(|m| m.hover = None));
                draw();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(i) = hover_at(lp) {
                    let id = OPEN.with(|o| o.borrow().as_ref().and_then(|m| match &m.items[i] {
                        Item::Action { id, .. } => Some(*id),
                        _ => None,
                    }));
                    finish(id);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wp.0 as u16;
                if vk == VK_ESCAPE.0 {
                    finish(None);
                } else if vk == VK_DOWN.0 || vk == VK_UP.0 {
                    OPEN.with(|o| {
                        if let Some(m) = o.borrow_mut().as_mut() {
                            m.hover = step(&m.items, m.hover, if vk == VK_DOWN.0 { 1 } else { -1 });
                        }
                    });
                    draw();
                } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                    let id = OPEN.with(|o| o.borrow().as_ref().and_then(|m| m.hover.and_then(|i| match &m.items[i] {
                        Item::Action { id, enabled: true, .. } => Some(*id),
                        _ => None,
                    })));
                    if id.is_some() {
                        finish(id);
                    }
                }
                LRESULT(0)
            }
            WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
                finish(None); // something else was clicked
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![Item::action(1, "Exit full screen").with_shortcut("F11"), Item::action(2, "Minimize"), Item::Separator, Item::Action { id: 3, label: "Off".into(), shortcut: None, checked: false, enabled: false }, Item::action(4, "Disconnect")]
    }

    #[test]
    fn rows_and_hits() {
        let it = items();
        let (rows, total) = layout(&it);
        assert_eq!(rows[0], (PAD, ROW));
        assert_eq!(rows[2].1, SEP);
        assert_eq!(total, PAD + 4.0 * ROW + SEP + PAD);
        assert_eq!(row_at(&it, PAD + 1.0), Some(0));
        assert_eq!(row_at(&it, PAD + ROW * 2.0 + 2.0), None, "a separator is not a row");
        assert_eq!(row_at(&it, PAD + ROW * 2.0 + SEP + 2.0), None, "a disabled row is not chosen");
        assert_eq!(row_at(&it, 1.0), None);
    }

    #[test]
    fn keyboard_skips_separators_and_disabled_rows() {
        let it = items();
        assert_eq!(step(&it, None, 1), Some(0));
        assert_eq!(step(&it, Some(1), 1), Some(4));
        assert_eq!(step(&it, Some(4), 1), Some(0), "wraps");
        assert_eq!(step(&it, None, -1), Some(4));
        assert_eq!(step(&it, Some(0), -1), Some(4));
        assert_eq!(step(&[Item::Separator], None, 1), None);
        let c = Item::action(9, "x").checked(true);
        assert!(matches!(c, Item::Action { checked: true, .. }));
    }
}
