//! A Liquid Glass menu (the navigation ball's, and the Mac app's menus in a window's title bar),
//! in place of the plain Windows popup menu: rows with a check mark and a shortcut, rows that
//! open another menu beside them, the row under the pointer or chosen with the arrow keys
//! highlighted in the accent colour, separators. It opens with a short spring from where it was
//! asked for and works as the Mac's menus do: with the mouse (press on a title, drag to an item,
//! let go; or click, then click) and the keyboard (arrows, Enter, Escape); a menu of a menu bar
//! gives way to the next one as the pointer moves over its title, or with Left and Right. The
//! window it belongs to stays the active one. `show` returns what was chosen.

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Action { id: u32, label: String, shortcut: Option<String>, checked: bool, enabled: bool },
    /// a row that opens another menu beside it
    Submenu { label: String, enabled: bool, items: Vec<Item> },
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

    pub fn submenu(label: &str, items: Vec<Item>) -> Item {
        Item::Submenu { label: label.into(), enabled: true, items }
    }

    /// A row that can be chosen (or opened).
    pub fn selectable(&self) -> bool {
        matches!(self, Item::Action { enabled: true, .. } | Item::Submenu { enabled: true, .. })
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
/// the narrowest a menu is; wider when a row needs it
#[cfg_attr(not(windows), allow(dead_code))]
const WIDTH: f32 = 290.0;
/// where a row's label starts (room for the check mark), the least room between it and its
/// shortcut, and the room right of the shortcut
#[cfg_attr(not(windows), allow(dead_code))]
const LABEL_X: f32 = 34.0;
#[cfg_attr(not(windows), allow(dead_code))]
const GAP: f32 = 28.0;
#[cfg_attr(not(windows), allow(dead_code))]
const RIGHT: f32 = 14.0;

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
    rows.iter().position(|(t, h)| y >= *t && y < t + h).filter(|&i| items[i].selectable())
}

/// The next row that can be chosen from `from`, going `dir` (+1 down, -1 up), wrapping.
pub fn step(items: &[Item], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = items.len() as i32;
    let mut i = from.map_or(if dir > 0 { -1 } else { n }, |f| f as i32);
    for _ in 0..n {
        i = (i + dir).rem_euclid(n);
        if items[i as usize].selectable() {
            return Some(i as usize);
        }
    }
    None
}

/// What became of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Chosen(u32),
    Closed,
    /// another menu of the menu bar it came from is wanted (the pointer went to its title, or
    /// Left / Right was pressed)
    Switch(usize),
}

/// The menu bar a menu was opened from: `title_at` says which of its `count` titles is at a
/// screen point; `current` is the one open.
pub struct BarTrack<'a> {
    pub current: usize,
    pub count: usize,
    pub title_at: &'a dyn Fn(i32, i32) -> Option<usize>,
}

/// The menu next to `current` in a bar of `count` (`dir` +1 right, -1 left), wrapping.
pub fn neighbour(current: usize, count: usize, dir: i32) -> usize {
    (current as i32 + dir).rem_euclid(count.max(1) as i32) as usize
}

#[cfg(windows)]
pub use win::{cancel, rect, register, show, show_in_bar};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::motion::{tokens, Anim, Curve};
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{self, Surface};
    use std::cell::RefCell;
    use std::time::{Duration, Instant};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("RmGlassMenu");
    const TIMER: usize = 1;
    /// how long the pointer rests on a row before its menu opens, or away before one closes
    const OPEN_AFTER: Duration = Duration::from_millis(160);
    const CLOSE_AFTER: Duration = Duration::from_millis(300);

    /// A line of text drawn as a coverage mask (mask, width, height).
    type Mask = (Vec<u8>, usize, usize);

    /// One menu on screen (the first, or one opened from a row of the one before it).
    struct Panel {
        surf: Surface,
        items: Vec<Item>,
        hover: Option<usize>,
        /// the glass body with its shadow (still), and where the menu itself starts in it
        base: Canvas,
        margin: usize,
        pos: (i32, i32),
        size: (usize, usize),
        /// each row's label and shortcut, drawn once
        texts: Vec<Option<(Mask, Option<Mask>)>>,
        pop: Anim,
        /// the corner it grows from (0..1 of the width, 0..1 of the height)
        origin: (f32, f32),
        drawn: bool,
        /// the row of the panel before it that it was opened from
        from: Option<usize>,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Later {
        /// open the menu of row `.1` of panel `.0`
        Open(usize, usize),
        /// close the panels from `.0` on
        Close(usize),
    }

    struct Menu {
        panels: Vec<Panel>,
        result: Option<Outcome>,
        scale: f32,
        dark: bool,
        accent: Rgba,
        hinst: HINSTANCE,
        owner: HWND,
        later: Option<(Later, Instant)>,
    }

    thread_local! {
        static OPEN: RefCell<Option<Menu>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_ARROW);
    }

    /// Open the menu at `at` (screen px), to the left of it when `left`; returns the id chosen, or
    /// None when it was closed without a choice.
    pub fn show(hinst: HINSTANCE, owner: HWND, at: POINT, left: bool, items: Vec<Item>) -> Option<u32> {
        match run(hinst, owner, at, left, items, None) {
            Outcome::Chosen(id) => Some(id),
            _ => None,
        }
    }

    /// Open menu `bar.current` of a menu bar at `at` (under its title).
    pub fn show_in_bar(hinst: HINSTANCE, owner: HWND, at: POINT, items: Vec<Item>, bar: &BarTrack) -> Outcome {
        run(hinst, owner, at, false, items, Some(bar))
    }

    /// A menu's panel, `left` of `at` or right of it; `sub`: opened from a row (kept on the
    /// screen downwards, not flipped above).
    fn panel(hinst: HINSTANCE, owner: HWND, items: Vec<Item>, at: POINT, left: bool, scale: f32, sub: bool) -> Option<Panel> {
        let (_, total) = layout(&items);
        let texts: Vec<Option<(Mask, Option<Mask>)>> = items
            .iter()
            .map(|it| match it {
                Item::Action { label, shortcut, .. } => {
                    let l = surface::text_mask(label, (13.0 * scale).round() as i32, 400, (WIDTH * 1.4 * scale) as usize);
                    let s = shortcut.as_ref().map(|s| surface::text_mask(s, (12.0 * scale).round() as i32, 400, (WIDTH * 0.8 * scale) as usize));
                    Some((l, s))
                }
                Item::Submenu { label, .. } => Some((surface::text_mask(label, (13.0 * scale).round() as i32, 400, (WIDTH * 1.4 * scale) as usize), None)),
                Item::Separator => None,
            })
            .collect();
        let wa = surface::work_area_at(at.x, at.y);
        let chevron = |it: &Item| if matches!(it, Item::Submenu { .. }) { GAP * scale } else { 0.0 };
        let need = items
            .iter()
            .zip(&texts)
            .filter_map(|(it, t)| t.as_ref().map(|(l, s)| LABEL_X * scale + l.1 as f32 + s.as_ref().map_or(0.0, |s| GAP * scale + s.1 as f32) + chevron(it) + RIGHT * scale))
            .fold(if sub { WIDTH * 0.7 * scale } else { WIDTH * scale }, f32::max);
        let (w, h) = (need.min((wa.right - wa.left - 8) as f32).round() as usize, (total * scale).round().min((wa.bottom - wa.top - 8) as f32) as usize);
        let mut x = if left { at.x - w as i32 } else { at.x };
        let mut y = at.y;
        x = x.clamp(wa.left + 4, (wa.right - w as i32 - 4).max(wa.left));
        if y + h as i32 > wa.bottom - 4 {
            y = if sub { (wa.bottom - 4 - h as i32).max(wa.top + 4) } else { (at.y - h as i32).max(wa.top + 4) };
        }
        let origin = (if left { 1.0 } else { 0.0 }, if y < at.y && !sub { 1.0 } else { 0.0 });
        // the glass is made from what is behind it now, before it shows
        let (base, margin) = surface::glass_panel(x, y, w, h, 12.0 * scale, crate::glass::Kind::Sheet, scale);
        // never the active window: the window the menu belongs to stays active, as on the Mac
        let surf = Surface::new(hinst, CLASS, "Menu", Some(owner), true, false)?;
        let pop = Anim::new(0.0, 1.0, tokens::pick(if sub { tokens::PRESS } else { tokens::POPOVER }, 0.4), Curve::SNAP);
        Some(Panel { surf, items, hover: None, base, margin, pos: (x, y), size: (w, h), texts, pop, origin, drawn: false, from: None })
    }

    fn run(hinst: HINSTANCE, owner: HWND, at: POINT, left: bool, items: Vec<Item>, bar: Option<&BarTrack>) -> Outcome {
        let scale = surface::scale_at(at.x, at.y);
        let (dark, accent, _) = surface::glass_look();
        let Some(root) = panel(hinst, owner, items, at, left, scale, false) else { return Outcome::Closed };
        let hwnd = root.surf.hwnd;
        OPEN.with(|o| *o.borrow_mut() = Some(Menu { panels: vec![root], result: None, scale, dark, accent, hinst, owner, later: None }));
        draw(0);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // the pointer is the menu's until it closes (a click anywhere else closes it)
            SetCapture(hwnd);
            SetTimer(Some(hwnd), TIMER, 16, None);
            let mut msg = MSG::default();
            while OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| m.result.is_none())) {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    PostQuitMessage(msg.wParam.0 as i32);
                    finish(Outcome::Closed);
                    break;
                }
                match msg.message {
                    // the keyboard is the menu's while it is open
                    WM_KEYDOWN | WM_SYSKEYDOWN => key(msg.wParam.0 as u16, bar),
                    WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR | WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {}
                    WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK => {
                        let mut p = POINT { x: (msg.lParam.0 & 0xffff) as i16 as i32, y: ((msg.lParam.0 >> 16) & 0xffff) as i16 as i32 };
                        let _ = ClientToScreen(msg.hwnd, &mut p);
                        mouse(msg.message, p, bar);
                    }
                    WM_NCMOUSEMOVE | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_NCLBUTTONUP => {
                        let p = POINT { x: (msg.lParam.0 & 0xffff) as i16 as i32, y: ((msg.lParam.0 >> 16) & 0xffff) as i16 as i32 };
                        let kind = match msg.message {
                            WM_NCMOUSEMOVE => WM_MOUSEMOVE,
                            WM_NCLBUTTONUP => WM_LBUTTONUP,
                            _ => WM_LBUTTONDOWN,
                        };
                        mouse(kind, p, bar);
                    }
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
        let open = OPEN.with(|o| o.borrow_mut().take());
        let result = open.as_ref().and_then(|m| m.result).unwrap_or(Outcome::Closed);
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER);
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
        }
        drop(open);
        result
    }

    fn finish(o: Outcome) {
        OPEN.with(|m| {
            if let Some(m) = m.borrow_mut().as_mut() {
                if m.result.is_none() {
                    m.result = Some(o);
                }
            }
        });
    }

    /// Close the open menu without a choice.
    pub fn cancel() {
        finish(Outcome::Closed);
    }

    /// Where the open menu is, with the menus opened from it (for screenshots).
    pub fn rect() -> Option<RECT> {
        OPEN.with(|o| {
            o.borrow().as_ref().and_then(|m| {
                m.panels.iter().filter_map(|p| surface::window_rect(p.surf.hwnd)).reduce(|a, b| RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) })
            })
        })
    }

    /// The deepest panel at screen point `p`, and its row there that can be chosen.
    fn hit(m: &Menu, p: POINT) -> Option<(usize, Option<usize>)> {
        m.panels.iter().enumerate().rev().find_map(|(k, pn)| {
            let (x, y) = (p.x - pn.pos.0, p.y - pn.pos.1);
            (x >= 0 && y >= 0 && (x as usize) < pn.size.0 && (y as usize) < pn.size.1).then(|| (k, row_at(&pn.items, y as f32 / m.scale)))
        })
    }

    fn mouse(kind: u32, p: POINT, bar: Option<&BarTrack>) {
        let Some((at, n)) = OPEN.with(|o| o.borrow().as_ref().map(|m| (hit(m, p), m.panels.len()))) else { return };
        let over_title = bar.and_then(|b| (b.title_at)(p.x, p.y).map(|j| (j, b.current)));
        match (kind, at) {
            (WM_MOUSEMOVE, Some((k, row))) => {
                let changed: Vec<usize> = OPEN.with(|o| {
                    let mut b = o.borrow_mut();
                    let Some(m) = b.as_mut() else { return vec![] };
                    let mut changed = vec![];
                    // the rows the menus up to this one were opened from stay lit
                    for j in (1..=k).rev() {
                        let from = m.panels[j].from;
                        if std::mem::replace(&mut m.panels[j - 1].hover, from) != from {
                            changed.push(j - 1);
                        }
                    }
                    let pn = &mut m.panels[k];
                    if std::mem::replace(&mut pn.hover, row) != row {
                        changed.push(k);
                    }
                    let sub = row.filter(|&r| matches!(pn.items[r], Item::Submenu { enabled: true, .. }));
                    // the menu open beside this one: kept while the pointer is on its row (or
                    // in it), closed a moment after it leaves it; a row's own menu opens after a
                    // moment of rest
                    let child_row = m.panels.get(k + 1).and_then(|c| c.from);
                    m.later = match (sub, child_row) {
                        (Some(r), Some(c)) if r == c => None,
                        (Some(r), _) => Some((Later::Open(k, r), Instant::now() + OPEN_AFTER)),
                        (None, Some(_)) => Some((Later::Close(k + 1), Instant::now() + CLOSE_AFTER)),
                        (None, None) => None,
                    };
                    changed
                });
                for k in changed {
                    draw(k);
                }
            }
            (WM_MOUSEMOVE, None) => {
                // over another title of the menu bar: that menu instead
                if let Some((j, cur)) = over_title {
                    if j != cur {
                        finish(Outcome::Switch(j));
                        return;
                    }
                }
                // away from every menu: the last one has nothing lit (the rows the others were
                // opened from stay lit)
                let k = n - 1;
                let changed = OPEN.with(|o| o.borrow_mut().as_mut().is_some_and(|m| {
                    if matches!(m.later, Some((Later::Open(..), _))) {
                        m.later = None;
                    }
                    m.panels[k].hover.take().is_some()
                }));
                if changed {
                    draw(k);
                }
            }
            (WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK, None) => {
                // a click outside the menus closes them; on another title of the bar, opens that
                finish(match over_title {
                    Some((j, cur)) if j != cur => Outcome::Switch(j),
                    _ => Outcome::Closed,
                });
            }
            (WM_LBUTTONDOWN | WM_RBUTTONDOWN, Some((k, Some(r)))) => {
                if is_sub(k, r) {
                    open_sub(k, r, false);
                }
            }
            (WM_LBUTTONUP | WM_RBUTTONUP, Some((k, Some(r)))) => {
                let id = OPEN.with(|o| o.borrow().as_ref().and_then(|m| match &m.panels[k].items[r] {
                    Item::Action { id, enabled: true, .. } => Some(*id),
                    _ => None,
                }));
                match id {
                    Some(id) => finish(Outcome::Chosen(id)),
                    None if is_sub(k, r) => open_sub(k, r, false),
                    None => {}
                }
            }
            // let go outside the menus (the click on the title that opened it): stays open
            _ => {}
        }
    }

    fn is_sub(k: usize, r: usize) -> bool {
        OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| matches!(m.panels.get(k).map(|p| &p.items[r]), Some(Item::Submenu { enabled: true, .. }))))
    }

    fn key(vk: u16, bar: Option<&BarTrack>) {
        let Some(d) = OPEN.with(|o| o.borrow().as_ref().map(|m| m.panels.len() - 1)) else { return };
        let hover = OPEN.with(|o| o.borrow().as_ref().and_then(|m| m.panels[d].hover));
        let vk = VIRTUAL_KEY(vk);
        if vk == VK_DOWN || vk == VK_UP {
            OPEN.with(|o| {
                if let Some(m) = o.borrow_mut().as_mut() {
                    m.later = None;
                    let pn = &mut m.panels[d];
                    pn.hover = step(&pn.items, pn.hover, if vk == VK_DOWN { 1 } else { -1 });
                }
            });
            draw(d);
        } else if vk == VK_RIGHT {
            match hover.filter(|&r| is_sub(d, r)) {
                Some(r) => open_sub(d, r, true),
                None => {
                    if let Some(b) = bar {
                        finish(Outcome::Switch(neighbour(b.current, b.count, 1)));
                    }
                }
            }
        } else if vk == VK_LEFT {
            if d > 0 {
                close_from(d);
            } else if let Some(b) = bar {
                finish(Outcome::Switch(neighbour(b.current, b.count, -1)));
            }
        } else if vk == VK_RETURN || vk == VK_SPACE {
            let Some(r) = hover else { return };
            let id = OPEN.with(|o| o.borrow().as_ref().and_then(|m| match &m.panels[d].items[r] {
                Item::Action { id, enabled: true, .. } => Some(*id),
                _ => None,
            }));
            match id {
                Some(id) => finish(Outcome::Chosen(id)),
                None if is_sub(d, r) => open_sub(d, r, true),
                None => {}
            }
        } else if vk == VK_ESCAPE {
            if d > 0 {
                close_from(d);
            } else {
                finish(Outcome::Closed);
            }
        } else if vk == VK_MENU || vk == VK_F10 {
            finish(Outcome::Closed);
        }
    }

    /// Close panels `k` and after.
    fn close_from(k: usize) {
        let gone: Vec<Panel> = OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let Some(m) = b.as_mut() else { return vec![] };
            m.later = None;
            if k == 0 || k >= m.panels.len() {
                return vec![];
            }
            m.panels.split_off(k)
        });
        drop(gone);
    }

    /// Open the menu of row `r` of panel `k` beside it (its first row lit when from the keyboard).
    fn open_sub(k: usize, r: usize, from_keys: bool) {
        close_from(k + 1);
        let Some((hinst, owner, items, at, left, scale)) = OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let m = b.as_mut()?;
            m.later = None;
            let pn = &mut m.panels[k];
            pn.hover = Some(r);
            let Item::Submenu { items, .. } = &pn.items[r] else { return None };
            let s = m.scale;
            let (rows, _) = layout(&pn.items);
            let y = pn.pos.1 + ((rows[r].0 - PAD) * s).round() as i32;
            let (right, left_x) = (pn.pos.0 + pn.size.0 as i32 - (4.0 * s) as i32, pn.pos.0 + (4.0 * s) as i32);
            // beside it on the right, or on the left when there is no room
            let wa = surface::work_area_at(pn.pos.0, pn.pos.1);
            let left = right + (WIDTH * 0.7 * s) as i32 > wa.right && left_x - (WIDTH * 0.7 * s) as i32 >= wa.left;
            Some((m.hinst, m.owner, items.clone(), POINT { x: if left { left_x } else { right }, y }, left, s))
        }) else { return };
        draw(k);
        let Some(mut p) = panel(hinst, owner, items, at, left, scale, true) else { return };
        p.from = Some(r);
        if from_keys {
            p.hover = step(&p.items, None, 1);
        }
        let hwnd = p.surf.hwnd;
        let n = OPEN.with(|o| o.borrow_mut().as_mut().map(|m| {
            m.panels.push(p);
            m.panels.len() - 1
        }));
        if let Some(n) = n {
            draw(n);
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    fn draw(k: usize) {
        OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let Some(m) = b.as_mut() else { return };
            let (s, dark, accent) = (m.scale, m.dark, m.accent);
            let Some(pn) = m.panels.get_mut(k) else { return };
            let mut c = pn.base.clone();
            let (mx, my) = (pn.margin as f32, pn.margin as f32);
            let w = c.w as f32 - 2.0 * mx;
            let (rows, _) = layout(&pn.items);
            let (fg, fg2) = surface::glass_text(dark);
            for (i, (it, (top, h))) in pn.items.iter().zip(rows).enumerate() {
                let (top, h) = (my + top * s, h * s);
                if top + h > my + pn.size.1 as f32 {
                    break; // taller than the screen: what fits
                }
                match it {
                    Item::Separator => {
                        c.fill_round_rect(mx + 12.0 * s, top + h / 2.0, w - 24.0 * s, s.max(1.0), 0.0, if dark { Rgba::WHITE.alpha(0.14) } else { Rgba::BLACK.alpha(0.10) });
                    }
                    _ => {
                        let enabled = it.selectable();
                        let lit = pn.hover == Some(i) && enabled;
                        if lit {
                            c.fill_round_rect(mx + PAD * s, top, w - 2.0 * PAD * s, h, 7.0 * s, accent);
                        }
                        let col = if lit { Rgba::WHITE } else if enabled { fg } else { fg2.fade(0.6) };
                        if matches!(it, Item::Action { checked: true, .. }) {
                            // a check mark
                            let (cx, cy) = (mx + 20.0 * s, top + h / 2.0);
                            c.fill_capsule(cx - 4.5 * s, cy, cx - 1.5 * s, cy + 3.5 * s, 0.9 * s, col);
                            c.fill_capsule(cx - 1.5 * s, cy + 3.5 * s, cx + 4.5 * s, cy - 4.0 * s, 0.9 * s, col);
                        }
                        if let Some(Some((label, short))) = pn.texts.get(i) {
                            let (mask, mw, mh) = label;
                            c.fill_mask(mask, *mw, (mx + LABEL_X * s) as isize, (top + (h - *mh as f32) / 2.0).round() as isize, col);
                            if let Some((mask, sw, sh)) = short {
                                let x = mx + w - RIGHT * s - *sw as f32;
                                c.fill_mask(mask, *sw, x.round() as isize, (top + (h - *sh as f32) / 2.0).round() as isize, if lit { Rgba::WHITE.alpha(0.85) } else { fg2 });
                            }
                        }
                        if matches!(it, Item::Submenu { .. }) {
                            // a chevron: another menu opens here
                            let (cx, cy) = (mx + w - RIGHT * s - 3.0 * s, top + h / 2.0);
                            let k = if lit { Rgba::WHITE } else if enabled { fg2 } else { fg2.fade(0.6) };
                            c.fill_capsule(cx - 2.0 * s, cy - 4.0 * s, cx + 2.0 * s, cy, 0.85 * s, k);
                            c.fill_capsule(cx + 2.0 * s, cy, cx - 2.0 * s, cy + 4.0 * s, 0.85 * s, k);
                        }
                    }
                }
            }
            // opening: grows from the corner it was asked at, and fades in
            let now = Instant::now();
            let k = pn.pop.value_at(now).clamp(0.0, 1.0);
            let shown = if k < 0.999 {
                let mut f = Canvas::new(c.w, c.h);
                let z = 0.92 + 0.08 * k;
                let (ow, oh) = (c.w as f32, c.h as f32);
                let (ox, oy) = (mx + pn.origin.0 * w, my + pn.origin.1 * (oh - 2.0 * my));
                f.draw(&c, ox - ox * z, oy - oy * z, ow * z, oh * z, 0.0, k);
                f
            } else {
                c
            };
            let (x, y) = (pn.pos.0 - pn.margin as i32, pn.pos.1 - pn.margin as i32);
            pn.surf.present(&shown, x, y, 255, None);
            pn.drawn = k >= 0.999;
        });
    }

    /// Every 16 ms while open: what was to happen after a moment, and the panels still opening.
    fn tick() {
        let due = OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let m = b.as_mut()?;
            match m.later {
                Some((what, at)) if Instant::now() >= at => {
                    m.later = None;
                    Some(what)
                }
                _ => None,
            }
        });
        match due {
            Some(Later::Open(k, r)) => open_sub(k, r, false),
            Some(Later::Close(k)) => close_from(k),
            None => {}
        }
        let opening: Vec<usize> = OPEN.with(|o| o.borrow().as_ref().map(|m| m.panels.iter().enumerate().filter(|(_, p)| !p.drawn).map(|(i, _)| i).collect()).unwrap_or_default());
        for k in opening {
            draw(k);
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_TIMER => {
                tick();
                LRESULT(0)
            }
            // something else took the pointer (another app came forward): the menu goes
            WM_CAPTURECHANGED => {
                let ours = OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| m.panels.iter().any(|p| p.surf.hwnd.0 as isize == lp.0)));
                if !ours {
                    finish(Outcome::Closed);
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
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
    fn rows_that_open_menus_are_chosen_and_a_bar_wraps() {
        let it = vec![Item::action(1, "New"), Item::submenu("Open Recent", vec![Item::action(2, "a.txt")]), Item::Submenu { label: "Off".into(), enabled: false, items: vec![] }];
        assert_eq!(row_at(&it, PAD + ROW + 1.0), Some(1), "a row with a menu is chosen");
        assert_eq!(row_at(&it, PAD + 2.0 * ROW + 1.0), None, "unless it cannot be used");
        assert_eq!(step(&it, Some(0), 1), Some(1));
        assert_eq!(step(&it, Some(1), 1), Some(0));
        assert_eq!(neighbour(0, 7, -1), 6);
        assert_eq!(neighbour(6, 7, 1), 0);
        assert_eq!(neighbour(2, 7, 1), 3);
        assert_eq!(neighbour(0, 0, 1), 0);
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
