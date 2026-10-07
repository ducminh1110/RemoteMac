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
    /// One display change at a time: two at once (new settings and the Mac Desktop opening)
    /// each made a display, the second destroying the first, and neither came online
    private let op = NSRecursiveLock()

    /// `forDesktop`: exactly the client's screen (the Mac Desktop is mirrored onto it); otherwise
    /// with room above for a fullscreen window's menu and title bars.
    func configure(width: Int, height: Int, scale: Int, forDesktop: Bool = false) -> [String: Any] {
        op.lock(); defer { op.unlock() }
        var st = makeDisplay(width: width, height: height, scale: scale, forDesktop: forDesktop)
        if st["available"] as? Bool == true, (st["width"] as? Int ?? 0) == 0 || CGDisplayBounds(displayID).width == 0 {
            // made but never came online (too soon after the one it replaced): once more
            log("virtual display did not come online; making it again")
            usleep(800_000)
            st = makeDisplay(width: width, height: height, scale: scale, forDesktop: forDesktop)
        }
        return st
    }

    private func makeDisplay(width: Int, height: Int, scale: Int, forDesktop: Bool) -> [String: Any] {
        let hidpi = scale >= 2
        let w = UInt32(max(320, hidpi ? width / 2 : width)), h = UInt32(max(240, hidpi ? height / 2 : height))
        var err = [CChar](repeating: 0, count: 256)
        let id = rm_virtual_display_create(w, h + (forDesktop ? 0 : headroom), hidpi ? 1 : 0, &err, Int32(err.count))
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
        if hidpi { selectRetina(id, width: Int(w), height: Int(h + (forDesktop ? 0 : headroom))) }
        lock.lock(); displayID = id; target = CGSize(width: Int(w), height: Int(h)); lock.unlock()
        let b = CGDisplayBounds(id)
        let px = CGDisplayCopyDisplayMode(id).map { "\($0.pixelWidth)x\($0.pixelHeight) px" } ?? "?"
        log("virtual display \(id): \(Int(b.width))x\(Int(b.height)) points, \(px), at \(Int(b.minX)),\(Int(b.minY)) hidpi=\(hidpi)")
        return ["type": "display_status", "available": true, "display_id": Int(id), "width": Int(w), "height": Int(h), "reason": NSNull()]
    }

    /// Make sure the display runs its Retina mode (`width`x`height` points at 2x): macOS may pick
    /// the 1x mode of the same size, and everything is then drawn at 1x however it is captured.
    private func selectRetina(_ id: CGDirectDisplayID, width: Int, height: Int) {
        // a new display lists its modes only after a moment
        let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
        var modes: [CGDisplayMode] = []
        for _ in 0..<25 {
            if let cur = CGDisplayCopyDisplayMode(id), cur.width == width, cur.pixelWidth >= width * 2 { return }
            modes = CGDisplayCopyAllDisplayModes(id, opts) as? [CGDisplayMode] ?? []
            if modes.contains(where: { $0.width == width && $0.pixelWidth >= width * 2 }) { break }
            usleep(200_000)
        }
        guard let m = modes.first(where: { $0.width == width && $0.height == height && $0.pixelWidth >= width * 2 }) else {
            log("virtual display \(id): no Retina mode of \(width)x\(height) among \(modes.map { "\($0.width)x\($0.height)@\($0.pixelWidth)" })")
            return
        }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success else { return }
        CGConfigureDisplayWithDisplayMode(cfg, id, m, nil)
        if CGCompleteDisplayConfiguration(cfg, .forSession) != .success { log("virtual display \(id): Retina mode refused") }
        usleep(200_000)
    }

    /// The Mac's own screen(s) mirror the virtual display (what BetterDummy does): the whole
    /// desktop is laid out at the client's resolution, and streaming that display shows it 1:1.
    private var mirrored: [CGDirectDisplayID] = []
    /// what the current mirrored display was made for (width, height, scale)
    private var mirrorSpec: (Int, Int, Int)?
    /// the layout before a fullscreen window made the display taller
    private var fsBase: (Int, Int, Int)?

    /// A virtual display of the client's screen with every other display mirroring it: the one
    /// already there when it fits, else a new one (the old mirroring undone first, or the Mac's
    /// screen would be left mirroring a display that no longer exists). nil if it cannot be done.
    func ensureMirrored(width: Int, height: Int, scale: Int) -> CGDirectDisplayID? {
        op.lock(); defer { op.unlock() }
        lock.lock(); let same = mirrorSpec.map { $0 == (width, height, scale) } ?? false; let id0 = displayID; lock.unlock()
        if same && id0 != 0 && (isMirroredOnto(id0) || CGMainDisplayID() == id0) { return id0 }
        unmirrorDesktop()
        let st = configure(width: width, height: height, scale: scale, forDesktop: true)
        guard st["available"] as? Bool == true else { log("virtual display unavailable: \(st["reason"] ?? "?")"); return nil }
        let id = displayID
        for attempt in 0..<3 {
            if mirrorDesktop(onto: id) && isMirroredOnto(id) {
                lock.lock(); mirrorSpec = (width, height, scale); lock.unlock()
                let mode = CGDisplayCopyDisplayMode(id).map { "\($0.width)x\($0.height) points, \($0.pixelWidth)x\($0.pixelHeight) px" } ?? "mode unknown"
                log("virtual display \(id) mirrored: \(mode)")
                return id
            }
            log("desktop: mirroring onto \(id) not in place yet (try \(attempt + 1))")
            usleep(400_000)
        }
        unmirrorDesktop()
        // macOS refused the mirroring: the next best is our display as the main one (menu bar,
        // Dock and new windows on it), the Mac's own screen beside it
        if makeMain(id) {
            lock.lock(); mirrorSpec = (width, height, scale); lock.unlock()
            log("desktop: mirroring refused; virtual display \(id) is the main display instead")
            return id
        }
        return nil
    }

    /// The current virtual display, as a display_status message (nil: none).
    func status() -> [String: Any]? {
        lock.lock(); let id = displayID, t = target; lock.unlock()
        guard id != 0 else { return nil }
        return ["type": "display_status", "available": true, "display_id": Int(id), "width": Int(t.width), "height": Int(t.height), "reason": NSNull()]
    }

    /// Every other online display mirrors `id` (so streaming `id` shows the Mac's desktop).
    private func isMirroredOnto(_ id: CGDirectDisplayID) -> Bool {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16), n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        let others = ids.prefix(Int(n)).filter { $0 != id }
        return !others.isEmpty && others.allSatisfy { CGDisplayMirrorsDisplay($0) == id }
    }

    func mirrorDesktop(onto id: CGDirectDisplayID) -> Bool {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16), n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        // every other display (the Mac's screen, or a VM's "Apple Virtual" display), also one
        // that still names an older mirror target
        let others = ids.prefix(Int(n)).filter { $0 != id }
        guard !others.isEmpty else { return false }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success else { return false }
        for d in others {
            let e = CGConfigureDisplayMirrorOfDisplay(cfg, d, id)
            if e != .success { log("desktop: display \(d) cannot mirror \(id) (\(e.rawValue))") }
        }
        let done = CGCompleteDisplayConfiguration(cfg, .forSession)
        guard done == .success else { log("desktop: mirroring onto \(id) refused (\(done.rawValue))"); CGCancelDisplayConfiguration(cfg); return false }
        usleep(300_000) // the window server applies it
        lock.lock(); mirrored = others; lock.unlock()
        log("desktop: displays \(others) now mirror virtual display \(id) (main display is \(CGMainDisplayID()))")
        return true
    }

    /// A window closed while fullscreen: the menu bar, Dock and display size come back when it was
    /// the last one.
    func windowGone(_ id: CGWindowID) {
        lock.lock()
        guard saved.removeValue(forKey: id) != nil else { lock.unlock(); return }
        let last = saved.isEmpty, base = last ? fsBase : nil
        if last { fsBase = nil }
        lock.unlock()
        guard last else { return }
        DispatchQueue.global().async { [self] in
            setChromeHidden(false)
            if let b = base { _ = ensureMirrored(width: b.0, height: b.1, scale: b.2) }
        }
    }

    /// Our display at the origin of the global space: the main display (menu bar and Dock).
    private func makeMain(_ id: CGDirectDisplayID) -> Bool {
        if CGMainDisplayID() == id { return true }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success else { return false }
        // the others to its right, ours at (0, 0)
        var x = Int32(CGDisplayBounds(id).width)
        var ids = [CGDirectDisplayID](repeating: 0, count: 16), n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        for d in ids.prefix(Int(n)) where d != id {
            CGConfigureDisplayOrigin(cfg, d, x, 0); x += Int32(CGDisplayBounds(d).width)
        }
        CGConfigureDisplayOrigin(cfg, id, 0, 0)
        let r = CGCompleteDisplayConfiguration(cfg, .forSession)
        usleep(300_000)
        if r != .success { log("desktop: making display \(id) the main one failed (\(r.rawValue))") }
        return CGMainDisplayID() == id
    }

    /// Back to the Mac's own layout.
    func unmirrorDesktop() {
        op.lock(); defer { op.unlock() }
        lock.lock(); let ds = mirrored; mirrored = []; mirrorSpec = nil; lock.unlock()
        guard !ds.isEmpty else { return }
        var cfg: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&cfg) == .success else { return }
        for d in ds { CGConfigureDisplayMirrorOfDisplay(cfg, d, kCGNullDirectDisplay) }
        _ = CGCompleteDisplayConfiguration(cfg, .forSession)
        log("desktop: mirroring undone")
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
        // The Mac's screen is our display (mirrored, the client's size, no room kept above): the
        // window takes the whole display where it is (macOS's own full screen moves it to a new
        // Space, with its animation), the menu bar and Dock hidden while it is there
        lock.lock(); let mirroredLayout = mirrorSpec != nil && displayID != 0; let mid = displayID; lock.unlock()
        if mirroredLayout {
            let frame: CGRect
            if on {
                lock.lock(); if saved[w.id] == nil { saved[w.id] = w.rect }; lock.unlock()
                setChromeHidden(true)
                frame = CGDisplayBounds(mid)
            } else {
                lock.lock(); let back = saved.removeValue(forKey: w.id); let others = !saved.isEmpty; let base = others ? nil : fsBase
                if !others { fsBase = nil }
                lock.unlock()
                if !others { setChromeHidden(false) }
                // the display back to the client's size
                if let b = base { _ = ensureMirrored(width: b.0, height: b.1, scale: b.2); usleep(300_000) }
                guard let r = back else { return false }
                frame = r
            }
            NSRunningApplication(processIdentifier: w.pid)?.activate(options: [.activateIgnoringOtherApps])
            for _ in 0..<8 {
                setFrame(aw, frame)
                usleep(250_000)
                if let r = axFrame(aw), abs(r.minY - frame.minY) < 4, abs(r.height - frame.height) < 4 { break }
            }
            let got = axFrame(aw).map { "\(Int($0.width))x\(Int($0.height)) at \(Int($0.minX)),\(Int($0.minY))" } ?? "?"
            log("window \(w.id) fullscreen=\(on) -> asked \(Int(frame.width))x\(Int(frame.height)) at \(Int(frame.minX)),\(Int(frame.minY)), has \(got)")
            lock.lock(); let spec = mirrorSpec; lock.unlock()
            if on, let r = axFrame(aw), let spec = spec, r.minY > frame.minY + 4 || r.height < frame.height - 4 {
                // the menu bar stayed (the window is kept below it): the display grows by that much
                // (and by the title bar cut off the picture), so what is shown is exactly the
                // client's screen; back to its size when the window leaves fullscreen
                // short at the top (menu bar) and/or at the bottom (Dock)
                let missing = max(0, r.minY - frame.minY), extra = max(0, frame.height - r.height) + w.inset
                lock.lock(); if fsBase == nil { fsBase = spec }; lock.unlock()
                if let nid = ensureMirrored(width: spec.0, height: spec.1 + Int((extra * CGFloat(spec.2)).rounded()), scale: spec.2) {
                    usleep(300_000)
                    let b = CGDisplayBounds(nid)
                    let f2 = CGRect(x: b.minX, y: b.minY + missing, width: b.width, height: frame.height + w.inset)
                    NSRunningApplication(processIdentifier: w.pid)?.activate(options: [.activateIgnoringOtherApps])
                    for _ in 0..<8 {
                        setFrame(aw, f2)
                        usleep(250_000)
                        if let r2 = axFrame(aw), abs(r2.minY - f2.minY) < 4, abs(r2.height - f2.height) < 4 { break }
                    }
                    let got2 = axFrame(aw).map { "\(Int($0.width))x\(Int($0.height)) at \(Int($0.minX)),\(Int($0.minY))" } ?? "?"
                    log("window \(w.id) fullscreen: display grown by \(Int(extra)) points (top \(Int(missing))), window has \(got2)")
                }
            }
            return true
        }
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

    private func axFrame(_ aw: AXUIElement) -> CGRect? {
        var p = CGPoint.zero, sz = CGSize.zero
        guard let pv = axAttr(aw, kAXPositionAttribute as String), let sv = axAttr(aw, kAXSizeAttribute as String),
              AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &sz) else { return nil }
        return CGRect(origin: p, size: sz)
    }

    /// The Mac's menu bar and Dock hidden automatically (shown when the pointer reaches them) while
    /// a window fills the display, as the user had them otherwise.
    private var chromeSaved: (menu: Bool, dock: Bool)?
    /// The Mac Desktop is open: its menu bar and Dock are part of it, shown whatever a
    /// fullscreen app window wants
    private var desktopOpen = false

    /// The Mac Desktop opened: the menu bar and Dock as the user has them, at once.
    func desktopOpened() {
        desktopOpen = true
        if let s = chromeSaved { applyChrome(menu: s.menu, dock: s.dock) }
    }

    /// The Mac Desktop closed: hidden again at once while an app window is still fullscreen.
    func desktopClosed() {
        desktopOpen = false
        lock.lock(); let any = !saved.isEmpty; lock.unlock()
        if any && chromeSaved != nil { applyChrome(menu: true, dock: true) }
    }

    func setChromeHidden(_ hide: Bool) {
        let menuKey = "_HIHideMenuBar" as CFString, dockKey = "autohide" as CFString, dock = "com.apple.dock" as CFString
        if hide {
            guard chromeSaved == nil else { return }
            chromeSaved = ((CFPreferencesCopyAppValue(menuKey, kCFPreferencesAnyApplication) as? Bool) ?? false,
                           (CFPreferencesCopyAppValue(dockKey, dock) as? Bool) ?? false)
            if !desktopOpen { applyChrome(menu: true, dock: true) }
        } else if let s = chromeSaved {
            chromeSaved = nil
            if !desktopOpen { applyChrome(menu: s.menu, dock: s.dock) }
        }
    }

    private func applyChrome(menu: Bool, dock: Bool) {
        // the window server's own switch (what System Settings flips), live; the preference too
        if let sky = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY),
           let conn = dlsym(sky, "SLSMainConnectionID"), let set = dlsym(sky, "SLSSetMenuBarAutohideEnabled") {
            typealias ConnFn = @convention(c) () -> Int32
            typealias SetFn = @convention(c) (Int32, Bool) -> Int32
            let r = unsafeBitCast(set, to: SetFn.self)(unsafeBitCast(conn, to: ConnFn.self)(), menu)
            log("menu bar autohide \(menu) via the window server (\(r))")
        }
        // the Dock's own switch, live (its preference alone waits for the Dock to restart)
        if let f = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CoreDockSetAutoHideEnabled") {
            typealias DockFn = @convention(c) (DarwinBoolean) -> Void
            unsafeBitCast(f, to: DockFn.self)(DarwinBoolean(dock))
            log("Dock autohide \(dock) via the Dock")
        }
        CFPreferencesSetAppValue("_HIHideMenuBar" as CFString, menu as CFBoolean, kCFPreferencesAnyApplication)
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication)
        CFPreferencesSetAppValue("autohide" as CFString, dock as CFBoolean, "com.apple.dock" as CFString)
        CFPreferencesAppSynchronize("com.apple.dock" as CFString)
        let dnc = DistributedNotificationCenter.default()
        dnc.postNotificationName(NSNotification.Name("AppleInterfaceMenuBarHidingChangedNotification"), object: nil, userInfo: nil, deliverImmediately: true)
        dnc.postNotificationName(NSNotification.Name("com.apple.dock.prefchanged"), object: nil, userInfo: nil, deliverImmediately: true)
        usleep(400_000) // the menu bar slides away before the window takes its place
        log("menu bar \(menu ? "hidden" : "shown"), Dock \(dock ? "hidden" : "shown")")
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
