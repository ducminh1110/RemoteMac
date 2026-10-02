//! Mac menu bar -> Windows menu text and command table. Pure functions, tested on any OS; the
//! Win32 `HMENU` is built from this in `ui`.

use rm_protocol::MenuNode;

/// First command id handed to menu items (below are reserved for system use).
pub const FIRST_ID: u16 = 1000;

/// "Cmd+Shift+S" on the Mac -> what the same keys are on this keyboard ("Ctrl+Shift+S").
pub fn translate_shortcut(mac: &str, ctrl_as_command: bool) -> String {
    mac.split('+')
        .map(|p| match (p, ctrl_as_command) {
            ("Cmd", true) => "Ctrl",
            ("Cmd", false) => "Win",
            ("Control", true) => "Win",
            ("Control", false) => "Ctrl",
            ("Option", _) => "Alt",
            (other, _) => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Menu item text: '&' escaped (Win32 mnemonic marker), shortcut right-aligned after a tab.
pub fn label(n: &MenuNode, ctrl_as_command: bool) -> String {
    let t = n.title.replace('&', "&&");
    match &n.shortcut {
        Some(s) if n.children.is_empty() => format!("{t}\t{}", translate_shortcut(s, ctrl_as_command)),
        _ => t,
    }
}

/// Command id -> path, for every selectable leaf, in menu order.
pub fn commands(menus: &[MenuNode]) -> Vec<(u16, Vec<u32>)> {
    fn walk(nodes: &[MenuNode], prefix: &mut Vec<u32>, next: &mut u16, out: &mut Vec<(u16, Vec<u32>)>) {
        for (i, n) in nodes.iter().enumerate() {
            if n.separator || *next == u16::MAX {
                continue;
            }
            prefix.push(i as u32);
            if n.children.is_empty() {
                out.push((*next, prefix.clone()));
                *next += 1;
            } else {
                walk(&n.children, prefix, next, out);
            }
            prefix.pop();
        }
    }
    let mut out = vec![];
    let mut next = FIRST_ID;
    for (i, top) in menus.iter().enumerate() {
        let mut p = vec![i as u32];
        walk(&top.children, &mut p, &mut next, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(t: &str, sc: Option<&str>) -> MenuNode {
        MenuNode { title: t.into(), enabled: true, shortcut: sc.map(Into::into), ..Default::default() }
    }

    #[test]
    fn shortcuts_follow_the_keyboard_mapping() {
        assert_eq!(translate_shortcut("Cmd+S", true), "Ctrl+S");
        assert_eq!(translate_shortcut("Shift+Cmd+K", true), "Shift+Ctrl+K");
        assert_eq!(translate_shortcut("Control+Option+Cmd+F", true), "Win+Alt+Ctrl+F");
        assert_eq!(translate_shortcut("Cmd+S", false), "Win+S");
    }

    #[test]
    fn labels_escape_mnemonics() {
        assert_eq!(label(&item("Save & Close", Some("Cmd+W")), true), "Save && Close\tCtrl+W");
        assert_eq!(label(&item("Recent", None), true), "Recent");
    }

    #[test]
    fn command_paths_skip_separators_and_cover_submenus() {
        let menus = vec![
            MenuNode { title: "App".into(), children: vec![item("About", None)], ..Default::default() },
            MenuNode {
                title: "File".into(),
                children: vec![
                    item("Open…", Some("Cmd+O")),
                    MenuNode { separator: true, ..Default::default() },
                    MenuNode { title: "Recent".into(), children: vec![item("a.swift", None), item("b.swift", None)], ..Default::default() },
                ],
                ..Default::default()
            },
        ];
        let c = commands(&menus);
        assert_eq!(c, vec![(1000, vec![0, 0]), (1001, vec![1, 0]), (1002, vec![1, 2, 0]), (1003, vec![1, 2, 1])]);
    }
}
