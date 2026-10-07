//! Windows input -> platform-neutral protocol events. Pure functions, tested on any OS.

use rm_protocol::Modifier;

/// Windows virtual-key code -> protocol physical key name (layout independent).
pub fn vk_to_physical(vk: u32) -> Option<&'static str> {
    const LETTERS: [&str; 26] = ["KeyA","KeyB","KeyC","KeyD","KeyE","KeyF","KeyG","KeyH","KeyI","KeyJ","KeyK","KeyL","KeyM","KeyN","KeyO","KeyP","KeyQ","KeyR","KeyS","KeyT","KeyU","KeyV","KeyW","KeyX","KeyY","KeyZ"];
    const DIGITS: [&str; 10] = ["Digit0","Digit1","Digit2","Digit3","Digit4","Digit5","Digit6","Digit7","Digit8","Digit9"];
    const FKEYS: [&str; 12] = ["F1","F2","F3","F4","F5","F6","F7","F8","F9","F10","F11","F12"];
    Some(match vk {
        0x41..=0x5A => LETTERS[(vk - 0x41) as usize],
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x70..=0x7B => FKEYS[(vk - 0x70) as usize],
        0x0D => "Enter", 0x09 => "Tab", 0x20 => "Space", 0x08 => "Backspace", 0x1B => "Escape", 0x2E => "Delete",
        0x25 => "ArrowLeft", 0x26 => "ArrowUp", 0x27 => "ArrowRight", 0x28 => "ArrowDown",
        0x24 => "Home", 0x23 => "End", 0x21 => "PageUp", 0x22 => "PageDown",
        0xBA => "Semicolon", 0xBB => "Equal", 0xBC => "Comma", 0xBD => "Minus", 0xBE => "Period", 0xBF => "Slash",
        0xC0 => "Backquote", 0xDB => "BracketLeft", 0xDC => "Backslash", 0xDD => "BracketRight", 0xDE => "Quote",
        _ => return None,
    })
}

/// Modifier VKs themselves are never sent as keys; the flags ride on the next key.
pub fn is_modifier_vk(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5 | 0x14)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

/// Ctrl+C on Windows is Command+C on the Mac (shortcut model, docs/SPEC.md §1).
/// With `ctrl_as_command=false` the keys map one-to-one (Win -> Command) for raw access.
pub fn map_modifiers(m: Mods, ctrl_as_command: bool) -> Vec<Modifier> {
    let mut out = vec![];
    if ctrl_as_command {
        if m.ctrl { out.push(Modifier::Command) }
        if m.win { out.push(Modifier::Control) }
    } else {
        if m.ctrl { out.push(Modifier::Control) }
        if m.win { out.push(Modifier::Command) }
    }
    if m.alt { out.push(Modifier::Option) }
    if m.shift { out.push(Modifier::Shift) }
    out
}

/// Keys that produce text are delivered as Unicode `TextInput` (from WM_CHAR, so the Windows
/// layout/IME decides the character); everything else, and anything with Ctrl/Alt/Win held,
/// is delivered as a physical `Key` event. This avoids typing a character twice.
pub fn sends_as_text(vk: u32, m: Mods) -> bool {
    if m.ctrl || m.alt || m.win {
        return false;
    }
    matches!(vk, 0x41..=0x5A | 0x30..=0x39 | 0x20 | 0xBA..=0xC0 | 0xDB..=0xDE | 0x60..=0x6F)
}

/// Can this WM_CHAR code unit be sent as text? (control characters are keys, not text)
pub fn is_text_char(c: u16) -> bool {
    c >= 0x20 && c != 0x7f
}

/// Map a client-area point to remote-window coordinates, clamped to the remote window.
pub fn scale_point(cx: i32, cy: i32, client: (i32, i32), remote: (u32, u32)) -> (f64, f64) {
    // the picture keeps its shape inside the client area (letterboxed), as it is drawn
    let (ox, oy, w, h) = fit_rect((client.0.max(1), client.1.max(1)), remote);
    let x = ((cx - ox) as f64 * remote.0 as f64 / w.max(1) as f64).clamp(0.0, remote.0.saturating_sub(1) as f64);
    let y = ((cy - oy) as f64 * remote.1 as f64 / h.max(1) as f64).clamp(0.0, remote.1.saturating_sub(1) as f64);
    (x, y)
}

/// Where a `source`-sized picture goes in `area`: as large as fits without changing its shape,
/// centred (Moonlight's scaleSourceToDestinationSurface). (x, y, w, h) in area pixels.
pub fn fit_rect(area: (i32, i32), source: (u32, u32)) -> (i32, i32, i32, i32) {
    let (aw, ah) = (area.0.max(1) as f64, area.1.max(1) as f64);
    let (sw, sh) = (source.0.max(1) as f64, source.1.max(1) as f64);
    let s = (aw / sw).min(ah / sh);
    let (w, h) = ((sw * s).round() as i32, (sh * s).round() as i32);
    // within a pixel or two of the area: use it all (rounding, not a different shape)
    let (w, h) = (if (area.0 - w).abs() <= 2 { area.0 } else { w }, if (area.1 - h).abs() <= 2 { area.1 } else { h });
    ((area.0 - w) / 2, (area.1 - h) / 2, w, h)
}

/// Combine a UTF-16 high surrogate with the following low surrogate (WM_CHAR delivers them separately).
pub fn push_utf16(pending_high: &mut Option<u16>, unit: u16) -> Option<String> {
    match unit {
        0xD800..=0xDBFF => { *pending_high = Some(unit); None }
        0xDC00..=0xDFFF => {
            let hi = pending_high.take()?;
            String::from_utf16(&[hi, unit]).ok()
        }
        _ => { *pending_high = None; String::from_utf16(&[unit]).ok() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(vk_to_physical(0x41), Some("KeyA"));
        assert_eq!(vk_to_physical(0x5A), Some("KeyZ"));
        assert_eq!(vk_to_physical(0x35), Some("Digit5"));
        assert_eq!(vk_to_physical(0x7B), Some("F12"));
        assert_eq!(vk_to_physical(0x08), Some("Backspace"));
        assert_eq!(vk_to_physical(0xFF), None);
        assert!(is_modifier_vk(0x11) && is_modifier_vk(0xA0) && !is_modifier_vk(0x41));
    }

    #[test]
    fn ctrl_becomes_command_by_default() {
        let m = Mods { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(map_modifiers(m, true), vec![Modifier::Command, Modifier::Shift]);
        assert_eq!(map_modifiers(m, false), vec![Modifier::Control, Modifier::Shift]);
        let w = Mods { win: true, alt: true, ..Default::default() };
        assert_eq!(map_modifiers(w, true), vec![Modifier::Control, Modifier::Option]);
        assert_eq!(map_modifiers(w, false), vec![Modifier::Command, Modifier::Option]);
    }

    #[test]
    fn text_vs_key_routing_never_double_types() {
        let none = Mods::default();
        assert!(sends_as_text(0x41, none));                       // plain 'a' -> text
        assert!(sends_as_text(0x41, Mods { shift: true, ..none })); // 'A' -> text
        assert!(sends_as_text(0x20, none));
        assert!(!sends_as_text(0x41, Mods { ctrl: true, ..none })); // Ctrl+A -> Key(Command)
        assert!(!sends_as_text(0x41, Mods { alt: true, ..none }));
        assert!(!sends_as_text(0x0D, none));                      // Enter -> Key
        assert!(!sends_as_text(0x08, none));
        assert!(!sends_as_text(0x25, none));
        assert!(is_text_char(b'a' as u16) && !is_text_char(0x0d) && !is_text_char(0x08) && !is_text_char(0x7f));
    }

    #[test]
    fn pointer_scaling_and_clamp() {
        assert_eq!(scale_point(0, 0, (480, 352), (480, 352)), (0.0, 0.0));
        assert_eq!(scale_point(240, 176, (960, 704), (480, 352)), (120.0, 88.0));
        assert_eq!(scale_point(-50, 9999, (100, 100), (480, 352)), (0.0, 351.0));
        assert_eq!(scale_point(5, 5, (0, 0), (480, 352)).0, 479.0); // degenerate client does not divide by zero
        // a 16:10 Mac screen on a 16:9 monitor: bars left and right, no stretching
        assert_eq!(fit_rect((1920, 1080), (1440, 900)), (96, 0, 1728, 1080));
        assert_eq!(scale_point(96, 540, (1920, 1080), (1440, 900)), (0.0, 450.0));
        assert_eq!(scale_point(10, 540, (1920, 1080), (1440, 900)), (0.0, 450.0));
    }

    #[test]
    fn surrogate_pairs() {
        let mut p = None;
        assert_eq!(push_utf16(&mut p, 0x61).as_deref(), Some("a"));
        assert_eq!(push_utf16(&mut p, 0xD83D), None);
        assert_eq!(push_utf16(&mut p, 0xDE00).as_deref(), Some("😀"));
        assert_eq!(push_utf16(&mut p, 0xDE00), None); // orphan low surrogate is dropped
    }
}
