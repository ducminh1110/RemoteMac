//! MacBridge Search: a glass search field over the screen (Ctrl+Alt+Space anywhere, the
//! navigation ball, or the launcher) to open a Mac app or switch to one already open, by typing
//! part of its name. Enter opens the first match, the arrow keys move, Escape closes; a click
//! elsewhere closes it too. The matching is ours and plain (no system search is involved).

/// One thing the search can open.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// it has a window open now (Enter switches to it)
    pub running: bool,
}

/// How well `query` matches `name` (higher is better), None when it does not: the start of the
/// name, then the start of a word, then anywhere, then the letters in order (initials).
pub fn score(name: &str, query: &str) -> Option<u32> {
    let q: Vec<char> = query.trim().to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(1);
    }
    let n = name.to_lowercase();
    let qs: String = q.iter().collect();
    if n.starts_with(&qs) {
        return Some(1000 - n.len().min(500) as u32);
    }
    if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&qs)) {
        return Some(700 - n.len().min(500) as u32);
    }
    if n.contains(&qs) {
        return Some(500 - n.len().min(400) as u32);
    }
    // letters in order ("vsc" for Visual Studio Code), closer together is better
    let nc: Vec<char> = n.chars().collect();
    let (mut i, mut gaps, mut last) = (0usize, 0usize, None::<usize>);
    for (k, c) in nc.iter().enumerate() {
        if i < q.len() && *c == q[i] {
            if let Some(l) = last {
                gaps += k - l - 1;
            }
            last = Some(k);
            i += 1;
        }
    }
    (i == q.len()).then(|| 300u32.saturating_sub(gaps as u32 * 4).max(10))
}

/// The entries matching `query`, best first; open apps first among equals; at most `max`.
pub fn search(entries: &[Entry], query: &str, max: usize) -> Vec<Entry> {
    let mut scored: Vec<(u32, &Entry)> = entries.iter().filter_map(|e| score(&e.name, query).map(|s| (s + if e.running { 50 } else { 0 }, e))).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase())));
    scored.into_iter().take(max).map(|(_, e)| e.clone()).collect()
}

/// A single-line text field's contents and caret (in characters).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub text: String,
    pub caret: usize,
}

impl Field {
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte(self.caret);
        self.text.insert_str(at, &s);
        self.caret += s.chars().count();
    }

    /// Delete the character before the caret, or (`word`, Ctrl+Backspace) the word before it.
    pub fn backspace(&mut self, word: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.caret.min(chars.len());
        if word {
            while i > 0 && !chars[i - 1].is_alphanumeric() {
                i -= 1;
            }
            while i > 0 && chars[i - 1].is_alphanumeric() {
                i -= 1;
            }
        } else {
            i = i.saturating_sub(1);
        }
        let (a, b) = (self.byte(i), self.byte(self.caret));
        self.text.replace_range(a..b, "");
        self.caret = i;
    }

    pub fn delete(&mut self) {
        if self.caret < self.text.chars().count() {
            let (a, b) = (self.byte(self.caret), self.byte(self.caret + 1));
            self.text.replace_range(a..b, "");
        }
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.text.chars().count();
    }

    fn byte(&self, chars: usize) -> usize {
        self.text.char_indices().nth(chars).map_or(self.text.len(), |(i, _)| i)
    }
}

#[cfg(windows)]
pub use win::{close, register, show, showing};

#[cfg(windows)]
mod win {
    use super::*;
    use crate::motion::{tokens, Anim, Curve};
    use crate::paint::{Canvas, Rgba};
    use crate::surface::{self, Surface};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::Instant;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    const CLASS: PCWSTR = w!("RmSearch");
    const TIMER: usize = 1;
    const WIDTH: f32 = 640.0;
    const FIELD: f32 = 56.0;
    const ROW: f32 = 46.0;
    const ROWS: usize = 7;

    type Mask = (Vec<u8>, usize, usize);

    struct Open {
        surf: Surface,
        entries: Vec<Entry>,
        icons: HashMap<String, Canvas>,
        field: Field,
        results: Vec<Entry>,
        sel: usize,
        scale: f32,
        pos: (i32, i32),
        /// what is behind the whole panel at its tallest, captured once when it opened
        backdrop: Option<Canvas>,
        reach: usize,
        /// the glass of the current height, and that height
        glass: Option<(usize, Canvas, usize)>,
        texts: HashMap<String, Mask>,
        pop: Anim,
        alpha: Anim,
        closing: bool,
        since: Instant,
        dark: bool,
        accent: Rgba,
        on_choose: Box<dyn Fn(String)>,
    }

    thread_local! {
        static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    }

    pub fn register(hinst: HINSTANCE) {
        surface::register(hinst, CLASS, Some(proc), IDC_ARROW);
    }

    pub fn showing() -> bool {
        OPEN.with(|o| o.borrow().is_some())
    }

    /// Open the search with these entries (and their icons, square RGBA); `on_choose(id)` runs
    /// on the UI thread with what was chosen.
    pub fn show(hinst: HINSTANCE, entries: Vec<Entry>, icons: HashMap<String, (u32, Vec<u8>)>, on_choose: impl Fn(String) + 'static) {
        if let Some(h) = OPEN.with(|o| o.borrow().as_ref().map(|m| m.surf.hwnd)) {
            unsafe {
                let _ = SetForegroundWindow(h);
            }
            return;
        }
        let mut cur = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cur);
        }
        let scale = surface::scale_at(cur.x, cur.y);
        let wa = surface::work_area_at(cur.x, cur.y);
        let w = (WIDTH * scale).min((wa.right - wa.left) as f32 * 0.9);
        let tallest = ((FIELD + ROW * ROWS as f32 + 12.0) * scale).round() as usize;
        let x = wa.left + ((wa.right - wa.left) as f32 - w) as i32 / 2;
        let y = wa.top + ((wa.bottom - wa.top) as f32 * 0.2) as i32;
        let (dark, accent, level) = surface::glass_look();
        let m = crate::glass::Material::for_kind(crate::glass::Kind::Sheet, dark, accent);
        let reach = crate::glass::margin(&m, scale, level, w as usize, tallest, 16.0 * scale);
        let backdrop = if level == crate::glass::Level::Off { None } else { surface::capture(x - reach as i32, y - reach as i32, w as usize + 2 * reach, tallest + 2 * reach) };
        let Some(surf) = Surface::new(hinst, CLASS, "MacBridge Search", None, true, true) else { return };
        let hwnd = surf.hwnd;
        let icons = icons.into_iter().filter(|(_, (s, px))| px.len() == (*s * *s * 4) as usize).map(|(k, (s, px))| (k, Canvas::from_rgba(s as usize, s as usize, &px))).collect();
        let results = search(&entries, "", ROWS);
        OPEN.with(|o| {
            *o.borrow_mut() = Some(Open {
                surf,
                entries,
                icons,
                field: Field::default(),
                results,
                sel: 0,
                scale,
                pos: (x, y),
                backdrop,
                reach,
                glass: None,
                texts: HashMap::new(),
                pop: Anim::new(0.0, 1.0, tokens::pick(tokens::MENU, 0.4), Curve::ARRIVE),
                alpha: Anim::new(0.0, 1.0, tokens::pick(tokens::MENU, 0.2), Curve::Decelerate),
                closing: false,
                since: Instant::now(),
                dark,
                accent,
                on_choose: Box::new(on_choose),
            })
        });
        draw();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(hwnd));
            SetTimer(Some(hwnd), TIMER, 16, None);
        }
    }

    /// Close (fading out).
    pub fn close() {
        OPEN.with(|o| {
            if let Some(m) = o.borrow_mut().as_mut() {
                if !m.closing {
                    m.closing = true;
                    m.alpha.retarget(0.0, tokens::pick(tokens::POPOVER, 0.3), Curve::Accelerate);
                    unsafe {
                        SetTimer(Some(m.surf.hwnd), TIMER, 16, None);
                    }
                }
            }
        });
    }

    fn text<'a>(texts: &'a mut HashMap<String, Mask>, key: &str, s: &str, px: f32, weight: i32, max_w: f32) -> &'a Mask {
        texts.entry(format!("{key}\u{1}{s}\u{1}{px}")).or_insert_with(|| surface::text_mask(s, px.round() as i32, weight, max_w.max(8.0) as usize))
    }

    fn panel_height(m: &Open) -> usize {
        let rows = m.results.len().max(1);
        (((FIELD + ROW * rows as f32 + 12.0) * m.scale).round()) as usize
    }

    fn draw() {
        OPEN.with(|o| {
            let mut b = o.borrow_mut();
            let Some(m) = b.as_mut() else { return };
            let s = m.scale;
            let h = panel_height(m);
            let w = m.backdrop.as_ref().map_or((WIDTH * s) as usize, |b| b.w - 2 * m.reach);
            let w = if m.backdrop.is_none() { (WIDTH * s).round() as usize } else { w };
            // the glass for this height (made again only when the number of rows changes)
            if m.glass.as_ref().is_none_or(|g| g.0 != h) {
                let (dark, accent, level) = surface::glass_look();
                let mat = crate::glass::Material::for_kind(crate::glass::Kind::Sheet, dark, accent);
                let back = m.backdrop.as_ref().map(|b| {
                    let mut c = Canvas::new(w + 2 * m.reach, h + 2 * m.reach);
                    c.composite(b, 0, 0, 1.0);
                    c
                });
                let body = crate::glass::render(back.as_ref(), m.reach, w, h, 16.0 * s, &mat, level, s);
                let margin = (30.0 * s).ceil() as usize;
                let mut c = Canvas::new(w + 2 * margin, h + 2 * margin);
                let (mf, wf, hf) = (margin as f32, w as f32, h as f32);
                c.shadow(mf, mf, wf, hf, 16.0 * s, 24.0 * s, 10.0 * s, Rgba::BLACK.alpha(0.22));
                c.composite(&body, margin as isize, margin as isize, 1.0);
                m.glass = Some((h, c, margin));
            }
            let (_, base, margin) = m.glass.as_ref().unwrap();
            let (margin, mut c) = (*margin, base.clone());
            let (mx, my) = (margin as f32, margin as f32);
            let wf = w as f32;
            let (fg, fg2) = surface::glass_text(m.dark);
            // the field: a magnifier, the text or the placeholder, the caret
            let (gx, gy) = (mx + 24.0 * s, my + FIELD * s / 2.0);
            c.stroke_round_rect_with(gx - 8.0 * s, gy - 9.0 * s, 15.0 * s, 15.0 * s, 7.5 * s, 2.0 * s, |_, _| fg2);
            c.fill_capsule(gx + 4.0 * s, gy + 3.0 * s, gx + 9.0 * s, gy + 8.0 * s, 1.3 * s, fg2);
            let tx = mx + 46.0 * s;
            let max_w = wf - 70.0 * s;
            let typed = m.field.text.clone();
            let (shown, col) = if typed.is_empty() { ("Open a Mac app".to_string(), fg2) } else { (typed.clone(), fg) };
            let (mask, mw, mh) = text(&mut m.texts, "field", &shown, 22.0 * s, 400, max_w).clone();
            c.fill_mask(&mask, mw, tx as isize, (gy - mh as f32 / 2.0).round() as isize, col);
            if (m.since.elapsed().as_millis() / 530) % 2 == 0 && !m.closing {
                let before: String = typed.chars().take(m.field.caret).collect();
                let cw = if before.is_empty() { 0.0 } else { text(&mut m.texts, "field", &before, 22.0 * s, 400, max_w).1 as f32 };
                c.fill_round_rect(tx + cw + 1.0 * s, gy - 12.0 * s, 2.0 * s, 24.0 * s, 1.0 * s, m.accent);
            }
            c.fill_round_rect(mx + 12.0 * s, my + FIELD * s, wf - 24.0 * s, s.max(1.0), 0.0, if m.dark { Rgba::WHITE.alpha(0.12) } else { Rgba::BLACK.alpha(0.08) });
            // the results
            let results = m.results.clone();
            if results.is_empty() {
                let (mask, mw, _) = text(&mut m.texts, "none", "No app by that name on the Mac", 14.0 * s, 400, wf).clone();
                c.fill_mask(&mask, mw, (mx + (wf - mw as f32) / 2.0) as isize, (my + (FIELD + 14.0) * s) as isize, fg2);
            }
            for (i, e) in results.iter().enumerate() {
                let top = my + (FIELD + 6.0) * s + i as f32 * ROW * s;
                let lit = i == m.sel;
                if lit {
                    c.fill_round_rect(mx + 8.0 * s, top, wf - 16.0 * s, (ROW - 4.0) * s, 10.0 * s, m.accent);
                }
                let isz = 30.0 * s;
                let iy = top + ((ROW - 4.0) * s - isz) / 2.0;
                if let Some(ic) = m.icons.get(&e.id) {
                    c.draw(ic, mx + 18.0 * s, iy, isz, isz, 0.0, 1.0);
                } else {
                    c.fill_round_rect(mx + 18.0 * s, iy, isz, isz, 7.0 * s, if lit { Rgba::WHITE.alpha(0.3) } else { fg2.fade(0.4) });
                }
                let (mask, mw, mh) = text(&mut m.texts, "name", &e.name, 15.0 * s, 500, wf * 0.6).clone();
                c.fill_mask(&mask, mw, (mx + 60.0 * s) as isize, (top + ((ROW - 4.0) * s - mh as f32) / 2.0).round() as isize, if lit { Rgba::WHITE } else { fg });
                let hint = if e.running { "Switch to it" } else if lit { "Open" } else { "" };
                if !hint.is_empty() {
                    let (mask, hw, hh) = text(&mut m.texts, "hint", hint, 12.0 * s, 400, wf * 0.3).clone();
                    c.fill_mask(&mask, hw, (mx + wf - 20.0 * s - hw as f32) as isize, (top + ((ROW - 4.0) * s - hh as f32) / 2.0).round() as isize, if lit { Rgba::WHITE.alpha(0.85) } else { fg2 });
                }
            }
            // opening: a spring from slightly smaller; closing: a fade
            let now = Instant::now();
            let k = m.pop.value_at(now);
            let a = (m.alpha.value_at(now).clamp(0.0, 1.0) * 255.0).round() as u8;
            let shown = if (k - 1.0).abs() > 0.002 {
                let mut f = Canvas::new(c.w, c.h);
                let z = 0.95 + 0.05 * k;
                let (cw, ch) = (c.w as f32, c.h as f32);
                f.draw(&c, cw * (1.0 - z) / 2.0, ch * (1.0 - z) / 4.0, cw * z, ch * z, 0.0, 1.0);
                f
            } else {
                c
            };
            m.surf.present(&shown, m.pos.0 - margin as i32, m.pos.1 - margin as i32, a, None);
        });
    }

    fn refilter() {
        OPEN.with(|o| {
            if let Some(m) = o.borrow_mut().as_mut() {
                m.results = search(&m.entries, &m.field.text, ROWS);
                m.sel = 0;
                m.since = Instant::now();
            }
        });
        draw();
    }

    fn choose(i: Option<usize>) {
        let picked = OPEN.with(|o| o.borrow().as_ref().and_then(|m| m.results.get(i.unwrap_or(m.sel)).map(|e| e.id.clone())));
        if let Some(id) = picked {
            close();
            // the callback may open windows: called with nothing borrowed
            let cb = OPEN.with(|o| o.borrow_mut().as_mut().map(|m| std::mem::replace(&mut m.on_choose, Box::new(|_| {}))));
            if let Some(cb) = cb {
                cb(id);
            }
        }
    }

    fn row_at(lp: LPARAM) -> Option<usize> {
        let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
        OPEN.with(|o| {
            let b = o.borrow();
            let m = b.as_ref()?;
            let margin = m.glass.as_ref().map_or(0, |g| g.2) as f32;
            let r = ((y - margin) / m.scale - FIELD - 6.0) / ROW;
            (r >= 0.0 && (r as usize) < m.results.len()).then_some(r as usize)
        })
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_TIMER => {
                let gone = OPEN.with(|o| o.borrow().as_ref().is_some_and(|m| m.closing && m.alpha.done()));
                if gone {
                    let m = OPEN.with(|o| o.borrow_mut().take());
                    if let Some(m) = m {
                        let _ = KillTimer(Some(m.surf.hwnd), TIMER);
                        drop(m);
                    }
                } else {
                    draw(); // the caret blinks, the opening springs
                }
                LRESULT(0)
            }
            WM_CHAR => {
                let c = char::from_u32(wp.0 as u32).unwrap_or('\0');
                if !c.is_control() {
                    OPEN.with(|o| o.borrow_mut().as_mut().map(|m| m.field.insert(&c.to_string())));
                    refilter();
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let vk = wp.0 as u16;
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let edit = |f: &dyn Fn(&mut Field)| {
                    OPEN.with(|o| o.borrow_mut().as_mut().map(|m| f(&mut m.field)));
                };
                match VIRTUAL_KEY(vk) {
                    VK_ESCAPE => close(),
                    VK_RETURN => choose(None),
                    VK_DOWN | VK_UP => {
                        OPEN.with(|o| {
                            if let Some(m) = o.borrow_mut().as_mut() {
                                let n = m.results.len().max(1);
                                m.sel = if vk == VK_DOWN.0 { (m.sel + 1) % n } else { (m.sel + n - 1) % n };
                            }
                        });
                        draw();
                    }
                    VK_BACK => {
                        edit(&|f| f.backspace(ctrl));
                        refilter();
                    }
                    VK_DELETE => {
                        edit(&|f| f.delete());
                        refilter();
                    }
                    VK_LEFT => {
                        edit(&|f| f.left());
                        draw();
                    }
                    VK_RIGHT => {
                        edit(&|f| f.right());
                        draw();
                    }
                    VK_HOME => {
                        edit(&|f| f.home());
                        draw();
                    }
                    VK_END => {
                        edit(&|f| f.end());
                        draw();
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if let Some(r) = row_at(lp) {
                    let changed = OPEN.with(|o| o.borrow_mut().as_mut().is_some_and(|m| std::mem::replace(&mut m.sel, r) != r));
                    if changed {
                        draw();
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(r) = row_at(lp) {
                    choose(Some(r));
                }
                LRESULT(0)
            }
            WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
                close();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, name: &str, running: bool) -> Entry {
        Entry { id: id.into(), name: name.into(), running }
    }

    #[test]
    fn matching_ranks_starts_words_and_initials() {
        assert!(score("Safari", "saf").unwrap() > score("Visual Studio Code", "stu").unwrap());
        assert!(score("Visual Studio Code", "stu").unwrap() > score("Instruments", "stu").unwrap());
        assert!(score("Visual Studio Code", "vsc").is_some(), "initials");
        assert!(score("Calculator", "xyz").is_none());
        assert_eq!(score("Notes", "  "), Some(1));
        assert!(score("TextEdit", "TEXT").is_some(), "any case");
    }

    #[test]
    fn search_orders_and_limits() {
        let all = vec![e("calc", "Calculator", false), e("cal", "Calendar", true), e("x", "Xcode", false), e("ce", "Chess", false)];
        let r = search(&all, "ca", 5);
        assert_eq!(r[0].id, "cal", "an open app first among equal matches");
        assert_eq!(r.len(), 2);
        assert_eq!(search(&all, "", 3).len(), 3);
        assert_eq!(search(&all, "", 9)[0].id, "cal", "open apps first when nothing is typed");
        assert!(search(&all, "zzz", 5).is_empty());
    }

    #[test]
    fn the_field_edits_text() {
        let mut f = Field::default();
        f.insert("safari");
        assert_eq!((f.text.as_str(), f.caret), ("safari", 6));
        f.backspace(false);
        assert_eq!(f.text, "safar");
        f.home();
        f.insert("é");
        assert_eq!((f.text.as_str(), f.caret), ("ésafar", 1));
        f.delete();
        assert_eq!(f.text, "éafar");
        f.end();
        f.insert(" pro");
        f.backspace(true);
        assert_eq!(f.text, "éafar ", "Ctrl+Backspace deletes the word");
        f.insert("\u{7}x");
        assert_eq!(f.text, "éafar x", "control characters are not typed");
    }
}
