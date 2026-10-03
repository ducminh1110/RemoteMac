//! Input packets as moonlight-common-c sends them (Input.h, Gen 7 / Sunshine magics), decoded
//! as Sunshine's input.cpp does. Header: size u32 BE (excluding itself), magic u32 LE.

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Windows virtual-key code (`keyCode & 0xff`), modifiers (MODIFIER_* bits)
    Key { vk: u16, down: bool, modifiers: u8 },
    MouseRel { dx: i16, dy: i16 },
    /// position in a `width` x `height` reference space
    MouseAbs { x: i16, y: i16, width: i16, height: i16 },
    /// 1 left, 2 middle, 3 right, 4 X1, 5 X2
    Button { button: u8, down: bool },
    /// 120 per wheel notch, positive = away from the user
    Scroll { amount: i16 },
    HScroll { amount: i16 },
    Text(String),
}

pub const KEY_DOWN: u32 = 0x03;
pub const KEY_UP: u32 = 0x04;
pub const MOUSE_MOVE_ABS: u32 = 0x05;
pub const MOUSE_MOVE_REL: u32 = 0x06;
pub const MOUSE_MOVE_REL_GEN5: u32 = 0x07;
pub const MOUSE_BUTTON_DOWN_GEN5: u32 = 0x08;
pub const MOUSE_BUTTON_UP_GEN5: u32 = 0x09;
pub const SCROLL_GEN5: u32 = 0x0A;
pub const UTF8_TEXT: u32 = 0x17;
pub const SS_HSCROLL: u32 = 0x5500_0001;

fn be16(p: &[u8], o: usize) -> Option<i16> {
    Some(i16::from_be_bytes(p.get(o..o + 2)?.try_into().ok()?))
}
fn le16(p: &[u8], o: usize) -> Option<i16> {
    Some(i16::from_le_bytes(p.get(o..o + 2)?.try_into().ok()?))
}

/// One input packet (header included).
pub fn parse(p: &[u8]) -> Option<Input> {
    if p.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes(p[0..4].try_into().ok()?) as usize;
    let magic = u32::from_le_bytes(p[4..8].try_into().ok()?);
    let body = p.get(8..(4 + size).min(p.len()))?;
    Some(match magic {
        KEY_DOWN | KEY_UP => Input::Key { vk: (le16(body, 1)? as u16) & 0xff, down: magic == KEY_DOWN, modifiers: *body.get(3)? },
        MOUSE_MOVE_REL | MOUSE_MOVE_REL_GEN5 => Input::MouseRel { dx: be16(body, 0)?, dy: be16(body, 2)? },
        MOUSE_MOVE_ABS => Input::MouseAbs { x: be16(body, 0)?, y: be16(body, 2)?, width: be16(body, 6)?.saturating_add(1), height: be16(body, 8)?.saturating_add(1) },
        MOUSE_BUTTON_DOWN_GEN5 | MOUSE_BUTTON_UP_GEN5 => Input::Button { button: *body.first()?, down: magic == MOUSE_BUTTON_DOWN_GEN5 },
        SCROLL_GEN5 => Input::Scroll { amount: be16(body, 0)? },
        SS_HSCROLL => Input::HScroll { amount: be16(body, 0)? },
        UTF8_TEXT => Input::Text(String::from_utf8_lossy(body).trim_end_matches('\0').to_string()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(magic: u32, body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 4) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(&magic.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn decodes_the_common_packets() {
        assert_eq!(parse(&pkt(KEY_DOWN, &[0, 0x41, 0x80, 0x01, 0, 0])), Some(Input::Key { vk: 0x41, down: true, modifiers: 1 }));
        assert_eq!(parse(&pkt(MOUSE_MOVE_REL_GEN5, &[0, 5, 0xff, 0xfe])), Some(Input::MouseRel { dx: 5, dy: -2 }));
        assert_eq!(parse(&pkt(MOUSE_MOVE_ABS, &[0, 10, 0, 20, 0, 0, 7, 127, 4, 55])), Some(Input::MouseAbs { x: 10, y: 20, width: 1920, height: 1080 }));
        assert_eq!(parse(&pkt(MOUSE_BUTTON_UP_GEN5, &[3])), Some(Input::Button { button: 3, down: false }));
        assert_eq!(parse(&pkt(SCROLL_GEN5, &[0xff, 0x88, 0xff, 0x88, 0, 0])), Some(Input::Scroll { amount: -120 }));
        assert_eq!(parse(&pkt(UTF8_TEXT, "hé".as_bytes())), Some(Input::Text("hé".into())));
    }
}
