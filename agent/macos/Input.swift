// Input injection using the recipes proven on GitHub runners (docs/SPEC.md §2, M1):
//  - keyboard / unicode text: CGEvent.postToPid (no focus needed)
//  - mouse: warp the real cursor, CGEvent with a nil source, HID tap (app must be frontmost)
import Foundation
import CoreGraphics
import AppKit

private let keyCodes: [String: CGKeyCode] = {
    var m: [String: CGKeyCode] = [:]
    let letters: [(String, CGKeyCode)] = [("A",0),("S",1),("D",2),("F",3),("H",4),("G",5),("Z",6),("X",7),("C",8),("V",9),("B",11),("Q",12),("W",13),("E",14),("R",15),("Y",16),("T",17),("O",31),("U",32),("I",34),("P",35),("L",37),("J",38),("K",40),("N",45),("M",46)]
    for (l, c) in letters { m["Key" + l] = c }
    let digits: [(String, CGKeyCode)] = [("1",18),("2",19),("3",20),("4",21),("6",22),("5",23),("9",25),("7",26),("8",28),("0",29)]
    for (d, c) in digits { m["Digit" + d] = c }
    let other: [(String, CGKeyCode)] = [("Enter",36),("Tab",48),("Space",49),("Backspace",51),("Escape",53),("Delete",117),
        ("ArrowLeft",123),("ArrowRight",124),("ArrowDown",125),("ArrowUp",126),("Minus",27),("Equal",24),("BracketLeft",33),
        ("BracketRight",30),("Semicolon",41),("Quote",39),("Comma",43),("Period",47),("Slash",44),("Backslash",42),("Backquote",50),
        ("Home",115),("End",119),("PageUp",116),("PageDown",121),
        ("F1",122),("F2",120),("F3",99),("F4",118),("F5",96),("F6",97),("F7",98),("F8",100),("F9",101),("F10",109),("F11",103),("F12",111)]
    for (n, c) in other { m[n] = c }
    return m
}()

private func flags(_ mods: [String]) -> CGEventFlags {
    var f = CGEventFlags()
    for m in mods {
        switch m {
        case "command": f.insert(.maskCommand)
        case "option": f.insert(.maskAlternate)
        case "control": f.insert(.maskControl)
        case "shift": f.insert(.maskShift)
        case "fn": f.insert(.maskSecondaryFn)
        case "caps_lock": f.insert(.maskAlphaShift)
        default: break
        }
    }
    return f
}

final class InputInjector {
    private let tracker: WindowTracker
    private var lastPoint = CGPoint.zero
    private var activatedPid: pid_t = 0
    private var leftDown = false, rightDown = false
    /// Mac Desktop: presses and releases received per path and button, and acted on per button
    private var received: [String: Int] = [:], injected: [String: Int] = [:]

    /// The region the Mac's Dock is streamed from, while it is (set by main).
    var dockRect: () -> CGRect? = { nil }
    /// The region the Mac's menu bar is streamed from, while it is (set by main).
    var menuBarRect: () -> CGRect? = { nil }

    /// A new Mac Desktop session: its clicks are counted afresh.
    func resetDesktopClicks() { received = [:]; injected = [:] }
    private let desktop: DesktopSession
    init(tracker: WindowTracker, desktop: DesktopSession) { self.tracker = tracker; self.desktop = desktop }

    /// Returns an error string if the message could not be delivered.
    func handle(_ msg: [String: Any]) -> String? {
        let wid = CGWindowID(int(msg["window_id"]))
        // Mac Desktop: coordinates are on the display, keys go to whatever app is in front (pid 0)
        let w: WinInfo
        if wid == desktopWindowID && desktop.isActive {
            w = WinInfo(id: wid, pid: 0, title: "Mac Desktop", rect: desktop.bounds)
        } else if wid == dockWindowID, let r = dockRect() {
            // the Mac's Dock (Fusion.swift): coordinates are within the region streamed
            w = WinInfo(id: wid, pid: 0, title: "Dock", rect: r)
        } else if wid == menuBarWindowID, let r = menuBarRect() {
            // the Mac's menu bar (MenuStrip.swift): coordinates are within its strip
            w = WinInfo(id: wid, pid: 0, title: "Menu Bar", rect: r)
        } else {
            guard let found = tracker.current(wid) else { return "unknown window \(wid)" }
            w = found
        }
        switch msg["type"] as? String ?? "" {
        case "text_input":
            typeUnicode(msg["text"] as? String ?? "", pid: w.pid)
        case "key":
            guard let name = msg["physical_key"] as? String, let code = keyCodes[name] else { return "unsupported key \(msg["physical_key"] ?? "?")" }
            let src = CGEventSource(stateID: .hidSystemState)
            guard let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: (msg["down"] as? Bool) ?? true) else { return "event failed" }
            e.flags = flags(msg["modifiers"] as? [String] ?? [])
            if w.pid == 0 { e.post(tap: .cghidEventTap) } else { e.postToPid(w.pid) }
            usleep(2_000)
        case "mouse_move":
            let p = CGPoint(x: w.content.minX + num(msg["x"]), y: w.content.minY + num(msg["y"]))
            lastPoint = p
            // with a button held this is a drag (selecting text, moving windows, resizing)
            let type: CGEventType = leftDown ? .leftMouseDragged : rightDown ? .rightMouseDragged : .mouseMoved
            post(type, p, button: rightDown && !leftDown ? .right : .left, pid: w.pid)
        case "mouse_button":
            // the Mac Desktop's presses and releases come twice (GameStream's input stream and
            // the viewer's own input path, both in order): only the first copy of each acts
            if wid == desktopWindowID {
                let key = msg["button"] as? String ?? "left", path = msg["path"] as? String ?? "link"
                let n = (received[path + key] ?? 0) + 1
                received[path + key] = n
                if n <= (injected[key] ?? 0) { return nil }
                injected[key] = n
            }
            let p = CGPoint(x: w.content.minX + num(msg["x"]), y: w.content.minY + num(msg["y"]))
            lastPoint = p
            let down = (msg["down"] as? Bool) ?? true
            let (type, button): (CGEventType, CGMouseButton)
            switch msg["button"] as? String ?? "left" {
            case "right": (type, button) = (down ? .rightMouseDown : .rightMouseUp, .right)
            case "middle": (type, button) = (down ? .otherMouseDown : .otherMouseUp, .center)
            default: (type, button) = (down ? .leftMouseDown : .leftMouseUp, .left)
            }
            if button == .left { leftDown = down } else if button == .right { rightDown = down }
            post(type, p, button: button, pid: w.pid)
        case "scroll":
            // Unverified on runners (docs/SPEC.md): pixel units, y positive = content moves down.
            if let e = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 2, wheel1: Int32(num(msg["dy"])), wheel2: Int32(num(msg["dx"])), wheel3: 0) {
                e.location = lastPoint; e.post(tap: .cghidEventTap)
            }
        default: return "not an input message"
        }
        return nil
    }

    private func post(_ type: CGEventType, _ p: CGPoint, button: CGMouseButton, pid: pid_t) {
        // bring the app forward only when it is not already (waiting on every click made each
        // one land 150 ms late)
        if pid != 0 && (activatedPid != pid || (type == .leftMouseDown && NSWorkspace.shared.frontmostApplication?.processIdentifier != pid)) {
            let front = NSWorkspace.shared.frontmostApplication?.processIdentifier == pid
            NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
            activatedPid = pid
            if !front { usleep(60_000) }
        }
        // No CGWarpMouseCursorPosition: a warp makes macOS hold back mouse events for 0.25 s, so a
        // button-up right after it came late and the Dock took the click for a press-and-hold
        // (its Quit / Options menu). A mouse event posted at a point moves the pointer itself.
        guard let e = CGEvent(mouseEventSource: Self.source, mouseType: type, mouseCursorPosition: p, mouseButton: button) else { return }
        // double and triple clicks: a press soon after the last one, close to it, counts up
        // (Finder opens on a double click, text selects words and lines)
        if type == .leftMouseDown || type == .rightMouseDown || type == .otherMouseDown {
            let now = CFAbsoluteTimeGetCurrent()
            let near = abs(p.x - lastDown.x) <= 4 && abs(p.y - lastDown.y) <= 4
            clicks = (near && now - lastDownAt <= NSEvent.doubleClickInterval && button == lastButton) ? clicks + 1 : 1
            lastDown = p; lastDownAt = now; lastButton = button
        }
        // no modifiers from the system's state (a Control left down would turn a click into a
        // Control-click: the Dock's Options / Quit menu)
        e.flags = []
        e.setIntegerValueField(.mouseEventClickState, value: Int64(type == .mouseMoved ? 0 : clicks))
        e.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button.rawValue))
        e.post(tap: .cghidEventTap)
    }

    /// Our own event source with no suppression of local events after synthetic ones.
    private static let source: CGEventSource? = {
        let s = CGEventSource(stateID: .hidSystemState)
        s?.localEventsSuppressionInterval = 0
        return s
    }()
    private var clicks: Int64 = 1
    private var lastDown = CGPoint(x: -100, y: -100), lastDownAt: CFAbsoluteTime = 0
    private var lastButton: CGMouseButton = .left

    private func typeUnicode(_ s: String, pid: pid_t) {
        for ch in s {
            let units = Array(String(ch).utf16)
            for down in [true, false] {
                guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 0, keyDown: down) else { continue }
                e.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                if pid == 0 { e.post(tap: .cghidEventTap) } else { e.postToPid(pid) }
                usleep(2_000)
            }
        }
    }
}
