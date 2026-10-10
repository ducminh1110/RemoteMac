// The Mac's own menu bar on Windows ("menu_bar_stream", feature "menubar").
//
// With windows shown as the Mac draws them (exact windows: their own title bar, no frame of the
// viewer's), their menus are the Mac's menu bar's: it is streamed as one more window
// (menuBarWindowID), the top strip of the Mac's main display, as it is (the front app's menus, the
// Apple menu, the status items and the clock). App windows never cover the menu bar, so the
// strip shows nothing else. Clicks go to the menu bar at that spot; the menus that open from it
// are shown under it as popups of it (WindowTracker). A menu bar that hides itself is left
// alone: MacBridge never changes that setting.
import Foundation
import AppKit

/// The reserved window id the menu bar is streamed as (next to the Mac Desktop's and the Dock's).
let menuBarWindowID: CGWindowID = 0x7FFF_0003

final class MenuBarMirror {
    private let lock = NSLock()
    private var region: CGRect?
    private var wanted = false
    private var stream: WindowStream?
    /// a status to send to the viewer ("menu_bar_status")
    var onStatus: (([String: Any]) -> Void)?
    var onPacket: ((VideoPacket) -> Void)?
    /// the menu bar is shown on Windows: its region, for the window tracker (its menus), or nil
    var onShown: ((CGRect?) -> Void)?

    /// The region of the screen streamed as the menu bar (screen points), while it is.
    var rect: CGRect? { lock.lock(); defer { lock.unlock() }; return wanted ? region : nil }

    /// Where the menu bar is: the main display's top strip, as tall as the window server's
    /// menu bar window (with a notch it is taller), or why it cannot be shown.
    func locate() -> Result<CGRect, WireError> {
        let main = CGMainDisplayID(), b = CGDisplayBounds(main)
        if (CFPreferencesCopyAppValue("_HIHideMenuBar" as CFString, kCFPreferencesAnyApplication) as? Bool) == true {
            return .failure(WireError(description: "The Mac's menu bar hides itself. To see it on Windows, turn off \"Automatically hide and show the menu bar\" in System Settings > Control Center on the Mac."))
        }
        let level = Int(CGWindowLevelForKey(.mainMenuWindow))
        let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
        var height: CGFloat = 0
        for w in all where (w[kCGWindowLayer as String] as? Int) == level {
            guard let d = w[kCGWindowBounds as String] as? [String: Any], let r = CGRect(dictionaryRepresentation: d as CFDictionary) else { continue }
            if abs(r.minY - b.minY) < 1, r.intersects(b), r.height > height, r.height < 80 { height = r.height }
        }
        if height < 10 {
            // no menu bar window found: what the menu bar takes of the main screen
            if let scr = NSScreen.screens.first(where: { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == main }) {
                height = scr.frame.maxY - scr.visibleFrame.maxY
            }
        }
        guard height >= 10 else { return .failure(WireError(description: "the Mac's menu bar is not on its screen (a full-screen app, or it hides itself)")) }
        var r = CGRect(x: b.minX, y: b.minY, width: b.width, height: height).integral
        // even sizes (an odd one leaves a row of nothing in the video)
        if Int(r.width) % 2 == 1 { r.size.width -= 1 }
        if Int(r.height) % 2 == 1 { r.size.height += 1 }
        return .success(r)
    }

    func start() {
        lock.lock(); wanted = true; lock.unlock()
        refresh(force: true)
    }

    func stop() {
        lock.lock(); wanted = false; let s = stream; stream = nil; region = nil; lock.unlock()
        onShown?(nil)
        if let s = s { Task { await s.stop() } }
    }

    /// Follow the menu bar (the main display changed, the bar's height with it): every few seconds.
    func refresh(force: Bool = false) {
        lock.lock(); let on = wanted; let old = region; lock.unlock()
        guard on else { return }
        switch locate() {
        case .failure(let e):
            let had = old != nil || force
            lock.lock(); let s = stream; stream = nil; region = nil; lock.unlock()
            if let s = s { Task { await s.stop() } }
            onShown?(nil)
            if had {
                log("Mac menu bar not shown: \(e.description)")
                onStatus?(["type": "menu_bar_status", "available": false, "window_id": Int(menuBarWindowID), "bounds": rectJSON(.zero), "reason": e.description])
            }
        case .success(let r):
            guard force || r != old else { return }
            lock.lock(); region = r; let s = stream; lock.unlock()
            if let s = s { Task { await s.stop() } }
            let ws = WindowStream(windowID: menuBarWindowID) { [weak self] pkt in self?.onPacket?(pkt) }
            // the whole strip as the display shows it (nothing but the menu bar is ever there)
            ws.region = (CGMainDisplayID(), r, [])
            lock.lock(); stream = ws; lock.unlock()
            onShown?(r)
            onStatus?(["type": "menu_bar_status", "available": true, "window_id": Int(menuBarWindowID), "bounds": rectJSON(r)])
            log("Mac menu bar on Windows: \(Int(r.width))x\(Int(r.height)) points")
            Task { do { try await ws.start() } catch { log("Mac menu bar stream failed: \(error)") } }
        }
    }

    func requestKeyframe() { lock.lock(); let s = stream; lock.unlock(); s?.requestKeyframe() }
    func setBitrate(_ b: Int) { lock.lock(); let s = stream; lock.unlock(); s?.setBitrate(b) }
}
