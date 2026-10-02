// The application's menu bar, read through Accessibility, so the client can show it as a native
// menu on the app's windows; and choosing an item by path. The Apple menu (index 0) is left out.
import Foundation
import ApplicationServices

private func mAX(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
private func kids(_ el: AXUIElement) -> [AXUIElement] { mAX(el, kAXChildrenAttribute as String) as? [AXUIElement] ?? [] }

private func topItems(_ pid: pid_t) -> [AXUIElement] {
    guard let bar = mAX(AXUIElementCreateApplication(pid), kAXMenuBarAttribute as String) else { return [] }
    return Array(kids(bar as! AXUIElement).dropFirst()) // drop the Apple menu
}

/// Items of the menu attached to a menu bar item or a submenu item.
private func submenuItems(_ el: AXUIElement) -> [AXUIElement] {
    guard let menu = kids(el).first else { return [] }
    return kids(menu)
}

private func shortcut(_ el: AXUIElement) -> String? {
    guard let ch = mAX(el, kAXMenuItemCmdCharAttribute as String) as? String, !ch.isEmpty else { return nil }
    let mods = (mAX(el, kAXMenuItemCmdModifiersAttribute as String) as? Int) ?? 0
    var parts: [String] = []
    if mods & 4 != 0 { parts.append("Control") }
    if mods & 2 != 0 { parts.append("Option") }
    if mods & 1 != 0 { parts.append("Shift") }
    if mods & 8 == 0 { parts.append("Cmd") }
    let key = ch == " " ? "Space" : ch.uppercased()
    return (parts + [key]).joined(separator: "+")
}

func readMenuBar(pid: pid_t) -> [[String: Any]] {
    var budget = 3000
    func node(_ el: AXUIElement, depth: Int) -> [String: Any] {
        budget -= 1
        let title = mAX(el, kAXTitleAttribute as String) as? String ?? ""
        var n: [String: Any] = ["title": title, "enabled": (mAX(el, kAXEnabledAttribute as String) as? Bool) ?? true, "separator": title.isEmpty]
        if let s = shortcut(el) { n["shortcut"] = s }
        if depth < 4 {
            var children: [[String: Any]] = []
            for k in submenuItems(el) where budget > 0 { children.append(node(k, depth: depth + 1)) }
            if !children.isEmpty { n["children"] = children }
        }
        return n
    }
    return topItems(pid).compactMap { budget > 0 ? node($0, depth: 0) : nil }
}

/// Press the menu item at `path` (first index into the menu bar without the Apple menu).
func invokeMenu(pid: pid_t, path: [Int]) -> Bool {
    guard let first = path.first else { return false }
    let top = topItems(pid)
    guard first >= 0, first < top.count else { return false }
    var el = top[first]
    for i in path.dropFirst() {
        let items = submenuItems(el)
        guard i >= 0, i < items.count else { return false }
        el = items[i]
    }
    return AXUIElementPerformAction(el, kAXPressAction as CFString) == .success
}
