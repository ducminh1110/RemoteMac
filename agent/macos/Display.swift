// The client's monitor as a virtual display on the Mac, and fullscreen on it: an app made
// fullscreen from the client gets exactly the client's resolution (not the Mac's own screen).
import Foundation
import AppKit
import ApplicationServices

/// Room left above the content on the virtual display: its menu bar plus the window's title bar.
private let headroom: UInt32 = 64

final class DisplayManager {
    private(set) var displayID: CGDirectDisplayID = 0
    /// Content size the client asked for (its monitor), in points.
    private(set) var target = CGSize.zero
    private var saved: [CGWindowID: CGRect] = [:]
    private let lock = NSLock()

    func configure(width: Int, height: Int, scale: Int) -> [String: Any] {
        let hidpi = scale >= 2
        let w = UInt32(max(320, hidpi ? width / 2 : width)), h = UInt32(max(240, hidpi ? height / 2 : height))
        var err = [CChar](repeating: 0, count: 256)
        let id = rm_virtual_display_create(w, h + headroom, hidpi ? 1 : 0, &err, Int32(err.count))
        guard id != 0 else {
            return ["type": "display_status", "available": false, "display_id": 0, "width": 0, "height": 0, "reason": String(cString: err)]
        }
        // wait until the window server has it online
        for _ in 0..<40 {
            var ids = [CGDirectDisplayID](repeating: 0, count: 16), n: UInt32 = 0
            CGGetOnlineDisplayList(16, &ids, &n)
            if ids.prefix(Int(n)).contains(id) && CGDisplayBounds(id).width > 0 { break }
            usleep(100_000)
        }
        lock.lock(); displayID = id; target = CGSize(width: Int(w), height: Int(h)); lock.unlock()
        let b = CGDisplayBounds(id)
        log("virtual display \(id): \(Int(b.width))x\(Int(b.height)) at \(Int(b.minX)),\(Int(b.minY)) hidpi=\(hidpi)")
        return ["type": "display_status", "available": true, "display_id": Int(id), "width": Int(w), "height": Int(h), "reason": NSNull()]
    }

    /// Usable area of a display (global, top-left origin): below its menu bar.
    func usable(_ id: CGDirectDisplayID) -> CGRect {
        let b = CGDisplayBounds(id)
        let key = NSDeviceDescriptionKey("NSScreenNumber")
        guard let scr = NSScreen.screens.first(where: { ($0.deviceDescription[key] as? NSNumber)?.uint32Value == id }) else { return b }
        let mainH = CGDisplayBounds(CGMainDisplayID()).height
        let vf = scr.visibleFrame
        return CGRect(x: vf.minX, y: mainH - vf.maxY, width: vf.width, height: vf.height)
    }

    /// Fullscreen on the virtual display (content = the client's monitor size), or back.
    func fullscreen(_ w: WinInfo, on: Bool) -> Bool {
        guard let aw = axWindowFor(pid: w.pid, id: w.id, rect: w.rect) else { return false }
        lock.lock(); let id = displayID, size = target; let back = on ? nil : saved.removeValue(forKey: w.id)
        if on && saved[w.id] == nil { saved[w.id] = w.rect }
        lock.unlock()
        let frame: CGRect
        if on && id != 0 {
            // straight from the display's bounds (NSScreen can be stale without an AppKit run loop):
            // below its menu bar, the content exactly the client's monitor
            let b = CGDisplayBounds(id)
            let top = b.minY + CGFloat(headroom) - w.inset - 2
            frame = CGRect(x: b.minX, y: top, width: size.width, height: size.height + w.inset)
        } else if on {
            let area = usable(CGMainDisplayID())
            let cw = id != 0 ? size.width : area.width, ch = id != 0 ? size.height : area.height - w.inset
            frame = CGRect(x: area.minX, y: area.minY, width: min(cw, area.width), height: min(ch + w.inset, area.height))
        } else {
            guard let r = back else { return false }
            frame = r
        }
        NSRunningApplication(processIdentifier: w.pid)?.activate(options: [.activateIgnoringOtherApps])
        // the app may not know the new display yet (screens are re-read on its run loop):
        // set, check where the window really is, retry for a few seconds
        for attempt in 0..<12 {
            setFrame(aw, frame)
            usleep(250_000)
            var p = CGPoint.zero, sz = CGSize.zero
            if let pv = axAttr(aw, kAXPositionAttribute as String), let sv = axAttr(aw, kAXSizeAttribute as String),
               AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &sz) {
                if abs(p.x - frame.minX) < 4 && abs(p.y - frame.minY) < 4 && abs(sz.width - frame.width) < 4 && abs(sz.height - frame.height) < 4 {
                    if attempt > 0 { log("window \(w.id) took its new frame after \(attempt + 1) tries") }
                    break
                }
                if attempt == 11 { log("window \(w.id) stays at \(Int(p.x)),\(Int(p.y)) \(Int(sz.width))x\(Int(sz.height))") }
            }
            usleep(250_000)
        }
        log("window \(w.id) fullscreen=\(on) -> \(Int(frame.width))x\(Int(frame.height)) at \(Int(frame.minX)),\(Int(frame.minY))")
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.7) { logWindowState(w.id, display: id) }
        return true
    }

    private func setFrame(_ aw: AXUIElement, _ f: CGRect) {
        var p = f.origin, s = f.size
        // move first (another display), then size, then move again (size can shift it)
        if let v = AXValueCreate(.cgPoint, &p) { AXUIElementSetAttributeValue(aw, kAXPositionAttribute as CFString, v) }
        if let v = AXValueCreate(.cgSize, &s) { AXUIElementSetAttributeValue(aw, kAXSizeAttribute as CFString, v) }
        if let v = AXValueCreate(.cgPoint, &p) { AXUIElementSetAttributeValue(aw, kAXPositionAttribute as CFString, v) }
    }
}

/// Diagnostics: where the window server has a window, and the virtual display's state.
func logWindowState(_ id: CGWindowID, display: CGDirectDisplayID) {
    let info = (CGWindowListCopyWindowInfo([.optionIncludingWindow], id) as? [[String: Any]])?.first
    let onscreen = info?[kCGWindowIsOnscreen as String] as? Bool ?? false
    let bounds = (info?[kCGWindowBounds as String] as? NSDictionary).flatMap { CGRect(dictionaryRepresentation: $0 as CFDictionary) } ?? .zero
    log("window \(id) state: exists=\(info != nil) onscreen=\(onscreen) bounds=\(Int(bounds.minX)),\(Int(bounds.minY)) \(Int(bounds.width))x\(Int(bounds.height)) display \(display): active=\(CGDisplayIsActive(display) != 0) online=\(CGDisplayIsOnline(display) != 0) mirrors=\(CGDisplayMirrorsDisplay(display))")
}
