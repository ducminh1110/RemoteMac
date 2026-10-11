// The Mac's own Dock, read through Accessibility, for the Dock Taro draws on Windows (the
// viewer's dock.rs): its items as the Mac shows them, in its order (the apps kept in it and
// the open ones, the lines between, folders, the Trash), opening one as a click on it does,
// and its menu (a right-click on an item) read, then chosen from. Nothing of the Dock is
// changed: its menu opens on the Mac for a moment to be read, and again to choose from it.
import Foundation
import AppKit
import ApplicationServices

private func dAX(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
private func dKids(_ el: AXUIElement) -> [AXUIElement] { dAX(el, kAXChildrenAttribute as String) as? [AXUIElement] ?? [] }

/// Apps shown as Launchpad (macOS 14, 15) or Apps (macOS 26): on Windows, Taro's own launcher.
private let launcherBundles: Set<String> = ["com.apple.launchpad.launcher", "com.apple.apps.launcher"]

/// One item of the Dock, and its element (to press it, or open its menu).
struct DockItem {
    let entry: [String: Any]
    let id: String
    let element: AXUIElement
    let path: String?
}

/// The Dock's items now, in its order (the children of its list; minimised windows left out).
func readDock(apps: AppManager) -> [DockItem] {
    guard let dock = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.dock").first else { return [] }
    let ax = AXUIElementCreateApplication(dock.processIdentifier)
    AXUIElementSetMessagingTimeout(ax, 1.0)
    guard let list = dKids(ax).first(where: { (dAX($0, kAXRoleAttribute as String) as? String) == (kAXListRole as String) }) else { return [] }
    var out: [DockItem] = []
    var lines = 0
    for el in dKids(list) {
        let sub = dAX(el, kAXSubroleAttribute as String) as? String ?? ""
        let title = dAX(el, kAXTitleAttribute as String) as? String ?? ""
        let url = dAX(el, kAXURLAttribute as String) as? URL
        let running = (dAX(el, "AXIsApplicationRunning") as? Bool) ?? false
        switch sub {
        case "AXSeparatorDockItem":
            lines += 1
            let id = "separator:\(lines)"
            out.append(DockItem(entry: ["id": id, "kind": "separator", "title": ""], id: id, element: el, path: nil))
        case "AXApplicationDockItem":
            let bid = url.flatMap { Bundle(url: $0)?.bundleIdentifier }
            let id = "app:" + (bid ?? url?.path ?? title)
            var e: [String: Any] = ["id": id, "kind": bid.map { launcherBundles.contains($0) } == true ? "launcher" : "app", "title": title, "running": running]
            // an app the viewer opens itself (its loading window, its windows shown): one of the
            // apps it lists
            if let b = bid {
                let appID = knownIDs[b] ?? b.lowercased()
                if apps.descriptor(appID) != nil { e["app_id"] = appID }
            }
            out.append(DockItem(entry: e, id: id, element: el, path: url?.path))
        case "AXTrashDockItem":
            out.append(DockItem(entry: ["id": "trash", "kind": "trash", "title": title], id: "trash", element: el, path: NSHomeDirectory() + "/.Trash"))
        case "AXFolderDockItem", "AXDocumentDockItem", "AXURLDockItem":
            guard let p = url?.path, !p.isEmpty else { continue }
            let id = "path:" + p
            out.append(DockItem(entry: ["id": id, "kind": sub == "AXFolderDockItem" ? "folder" : "file", "title": title], id: id, element: el, path: p))
        default:
            continue // minimised windows, the Downloads progress…
        }
    }
    return out
}

/// The icon of a Dock item that is not an app (a folder, a document, the Trash), as the Mac
/// draws it: straight-alpha RGBA, `size` square.
func dockIconRGBA(_ item: DockItem, size: Int) -> Data? {
    if item.id == "trash" {
        let full = ((try? FileManager.default.contentsOfDirectory(atPath: NSHomeDirectory() + "/.Trash"))?.isEmpty == false)
        if let img = NSImage(named: full ? NSImage.trashFullName : NSImage.trashEmptyName) {
            return rgba(of: img, size: size)
        }
    }
    guard let p = item.path else { return nil }
    return appIconRGBA(path: p, size: size)
}

private func rgba(of img: NSImage, size: Int) -> Data? {
    var rect = NSRect(x: 0, y: 0, width: size, height: size)
    guard let cg = img.cgImage(forProposedRect: &rect, context: nil, hints: nil) else { return nil }
    var buf = [UInt8](repeating: 0, count: size * size * 4)
    guard let ctx = CGContext(data: &buf, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
                              space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
    ctx.draw(cg, in: CGRect(x: 0, y: 0, width: size, height: size))
    for i in stride(from: 0, to: buf.count, by: 4) {
        let a = Int(buf[i + 3])
        if a > 0 && a < 255 { for k in 0..<3 { buf[i + k] = UInt8(min(255, Int(buf[i + k]) * 255 / a)) } }
    }
    return Data(buf)
}

/// Open a Dock item as a click on it does.
func openDockItem(_ item: DockItem) -> Bool {
    AXUIElementPerformAction(item.element, kAXPressAction as CFString) == .success
}

/// The menu open under a Dock item (after its "show menu" action), if it is there yet.
private func openMenu(of item: DockItem) -> AXUIElement? {
    for _ in 0..<10 {
        if let m = dKids(item.element).first(where: { (dAX($0, kAXRoleAttribute as String) as? String) == (kAXMenuRole as String) }) { return m }
        usleep(40_000)
    }
    return nil
}

private func closeMenu(_ menu: AXUIElement, of item: DockItem) {
    if AXUIElementPerformAction(menu, kAXCancelAction as CFString) != .success {
        // Escape, as a person would
        let src = CGEventSource(stateID: .hidSystemState)
        CGEvent(keyboardEventSource: src, virtualKey: 53, keyDown: true)?.post(tap: .cghidEventTap)
        CGEvent(keyboardEventSource: src, virtualKey: 53, keyDown: false)?.post(tap: .cghidEventTap)
    }
    // gone before anything else of the Dock is asked (a menu of another item would not open
    // while this one still closes)
    for _ in 0..<12 where dKids(item.element).contains(where: { (dAX($0, kAXRoleAttribute as String) as? String) == (kAXMenuRole as String) }) {
        usleep(40_000)
    }
}

/// Open the item's menu (once more, a moment later, if it did not come: the Dock may still be
/// closing another).
private func showMenu(of item: DockItem) -> AXUIElement? {
    for attempt in 0..<2 {
        if attempt > 0 { usleep(350_000) }
        if AXUIElementPerformAction(item.element, kAXShowMenuAction as CFString) == .success, let m = openMenu(of: item) { return m }
    }
    return nil
}

private func menuNodes(_ menu: AXUIElement, depth: Int = 0) -> [[String: Any]] {
    dKids(menu).map { el in
        let title = String((dAX(el, kAXTitleAttribute as String) as? String ?? "").prefix(200))
        var n: [String: Any] = ["title": title, "enabled": (dAX(el, kAXEnabledAttribute as String) as? Bool) ?? true, "separator": title.isEmpty]
        if depth < 3, let sub = dKids(el).first(where: { (dAX($0, kAXRoleAttribute as String) as? String) == (kAXMenuRole as String) }) {
            let children = menuNodes(sub, depth: depth + 1)
            if !children.isEmpty { n["children"] = children }
        }
        return n
    }
}

/// The item's menu as the Mac shows it (opened for a moment, read, closed).
func readDockMenu(_ item: DockItem) -> [[String: Any]] {
    guard let menu = showMenu(of: item) else { return [] }
    let nodes = menuNodes(menu)
    closeMenu(menu, of: item)
    return nodes
}

/// Choose the entry at `path` of the item's menu (opened again, the entry pressed).
func invokeDockMenu(_ item: DockItem, path: [Int]) -> Bool {
    guard !path.isEmpty, let menu = showMenu(of: item) else { return false }
    var level = menu
    var target: AXUIElement?
    for (i, k) in path.enumerated() {
        let entries = dKids(level)
        guard k >= 0, k < entries.count else { closeMenu(menu, of: item); return false }
        if i == path.count - 1 {
            target = entries[k]
        } else {
            // a submenu: open it (its entries are there once it is)
            _ = AXUIElementPerformAction(entries[k], kAXPressAction as CFString)
            usleep(80_000)
            guard let sub = dKids(entries[k]).first(where: { (dAX($0, kAXRoleAttribute as String) as? String) == (kAXMenuRole as String) }) else { closeMenu(menu, of: item); return false }
            level = sub
        }
    }
    guard let t = target, AXUIElementPerformAction(t, kAXPressAction as CFString) == .success else { closeMenu(menu, of: item); return false }
    return true
}
