// Choosing a file in an app's Open panel on the user's behalf (after the file was picked in the
// Windows file dialog and uploaded). Driven through Accessibility where possible: the "Go to
// Folder" field gets its value set directly and the panel's Open button is pressed, so it does not
// depend on keystrokes reaching the right window.
import Foundation
import AppKit
import ApplicationServices

private func pAX(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
private func pRole(_ el: AXUIElement) -> String { pAX(el, kAXRoleAttribute as String) as? String ?? "?" }

/// Bounded depth-first search below `root`.
private func findAX(_ root: AXUIElement, maxNodes: Int = 600, _ pred: (AXUIElement) -> Bool) -> AXUIElement? {
    var stack = [(root, 0)], visited = 0
    while let (el, depth) = stack.popLast(), visited < maxNodes {
        visited += 1
        if pred(el) { return el }
        if depth < 9, let kids = pAX(el, kAXChildrenAttribute as String) as? [AXUIElement] { for k in kids.reversed() { stack.append((k, depth + 1)) } }
    }
    return nil
}

private func hidKey(_ code: CGKeyCode, _ flags: CGEventFlags = []) {
    for down in [true, false] {
        guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: code, keyDown: down) else { continue }
        e.flags = flags
        e.post(tap: .cghidEventTap)
        usleep(20_000)
    }
}

/// The app's windows that look like a file panel (Cancel + Open/Choose buttons).
private func panelWindows(_ pid: pid_t) -> [AXUIElement] {
    let wins = pAX(AXUIElementCreateApplication(pid), kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    return wins.filter { w in
        findAX(w) { pRole($0) == (kAXButtonRole as String) && (pAX($0, kAXTitleAttribute as String) as? String) == "Cancel" } != nil
    }
}

private func openButton(_ panel: AXUIElement) -> AXUIElement? {
    findAX(panel) { el in
        guard pRole(el) == (kAXButtonRole as String), let t = pAX(el, kAXTitleAttribute as String) as? String else { return false }
        return t == "Open" || t == "Choose"
    }
}

private func focusedTextField(_ pid: pid_t) -> AXUIElement? {
    guard let f = pAX(AXUIElementCreateApplication(pid), kAXFocusedUIElementAttribute as String) else { return nil }
    let el = f as! AXUIElement
    return [kAXTextFieldRole as String, kAXComboBoxRole as String, "AXSearchField"].contains(pRole(el)) ? el : nil
}

/// Returns true once the panel has gone away (the app took the file).
@discardableResult
func chooseInPanel(pid: pid_t, rect: CGRect, path: String) -> Bool {
    let name = path.split(separator: "/").last.map(String.init) ?? ""
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
    usleep(300_000)
    if let w = axWindowMatching(pid: pid, rect: rect) {
        AXUIElementPerformAction(w, kAXRaiseAction as CFString)
        AXUIElementSetAttributeValue(w, kAXMainAttribute as CFString, kCFBooleanTrue)
    }
    usleep(200_000)
    let panelsBefore = panelWindows(pid).count
    log("panel choose \(name): panels=\(panelsBefore)")

    // 1. "Go to Folder" (Cmd+Shift+G), as a user would; the sheet focuses its path field.
    hidKey(5, [.maskCommand, .maskShift])
    var field: AXUIElement?
    for _ in 0..<20 where field == nil { usleep(100_000); field = focusedTextField(pid) }
    if field == nil {
        keyTo(pid, 5, [.maskCommand, .maskShift]) // the app may not be frontmost: direct to the process
        for _ in 0..<15 where field == nil { usleep(100_000); field = focusedTextField(pid) }
    }
    if let field = field {
        let r = AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, path as CFString)
        let now = pAX(field, kAXValueAttribute as String) as? String ?? ""
        log("panel choose: go-to field \(pRole(field)) set=\(r.rawValue) valueMatches=\(now == path)")
        if now != path { // some fields ignore AXValue: type it instead
            AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, "" as CFString)
            for ch in path {
                let units = Array(String(ch).utf16)
                for down in [true, false] {
                    guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 0, keyDown: down) else { continue }
                    e.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                    e.postToPid(pid); usleep(8_000)
                }
            }
        }
        usleep(300_000)
        hidKey(36) // Return: go to (and select) the file
    } else {
        log("panel choose: no go-to field appeared (focused role=\(pAX(AXUIElementCreateApplication(pid), kAXFocusedUIElementAttribute as String).map { pRole($0 as! AXUIElement) } ?? "none"))")
    }

    // 2. Open: some macOS versions open a file straight from "Go to"; otherwise press the button.
    for attempt in 0..<30 {
        usleep(200_000)
        let panels = panelWindows(pid)
        if panels.count < panelsBefore || panels.isEmpty { log("panel choose: panel closed"); return true }
        if attempt % 5 == 4, let p = panels.first, let b = openButton(p) {
            let enabled = (pAX(b, kAXEnabledAttribute as String) as? Bool) ?? false
            log("panel choose: Open button enabled=\(enabled)")
            if enabled { AXUIElementPerformAction(b, kAXPressAction as CFString) }
        }
    }
    log("panel choose: panel still open")
    return false
}
