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
    init(tracker: WindowTracker) { self.tracker = tracker }

    /// Returns an error string if the message could not be delivered.
    func handle(_ msg: [String: Any]) -> String? {
        let wid = CGWindowID(int(msg["window_id"]))
        guard let w = tracker.current(wid) else { return "unknown window \(wid)" }
        switch msg["type"] as? String ?? "" {
        case "text_input":
            typeUnicode(msg["text"] as? String ?? "", pid: w.pid)
        case "key":
            guard let name = msg["physical_key"] as? String, let code = keyCodes[name] else { return "unsupported key \(msg["physical_key"] ?? "?")" }
            let src = CGEventSource(stateID: .hidSystemState)
            guard let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: (msg["down"] as? Bool) ?? true) else { return "event failed" }
            e.flags = flags(msg["modifiers"] as? [String] ?? [])
            e.postToPid(w.pid)
            usleep(15_000)
        case "mouse_move":
            let p = CGPoint(x: w.rect.minX + num(msg["x"]), y: w.rect.minY + num(msg["y"]))
            lastPoint = p
            post(.mouseMoved, p, button: .left, pid: w.pid)
        case "mouse_button":
            let p = CGPoint(x: w.rect.minX + num(msg["x"]), y: w.rect.minY + num(msg["y"]))
            lastPoint = p
            let down = (msg["down"] as? Bool) ?? true
            let (type, button): (CGEventType, CGMouseButton)
            switch msg["button"] as? String ?? "left" {
            case "right": (type, button) = (down ? .rightMouseDown : .rightMouseUp, .right)
            case "middle": (type, button) = (down ? .otherMouseDown : .otherMouseUp, .center)
            default: (type, button) = (down ? .leftMouseDown : .leftMouseUp, .left)
            }
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
        if activatedPid != pid || type == .leftMouseDown {
            NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
            activatedPid = pid; usleep(150_000)
        }
        CGWarpMouseCursorPosition(p); CGAssociateMouseAndMouseCursorPosition(1)
        guard let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: button) else { return }
        e.setIntegerValueField(.mouseEventClickState, value: 1)
        e.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button.rawValue))
        e.post(tap: .cghidEventTap)
    }

    private func typeUnicode(_ s: String, pid: pid_t) {
        for ch in s {
            let units = Array(String(ch).utf16)
            for down in [true, false] {
                guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 0, keyDown: down) else { continue }
                e.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                e.postToPid(pid)
                usleep(15_000)
            }
        }
    }
}
