// Desktop Fusion: the Mac's own Dock on Windows, over the PC's own wallpaper.
//
// The Dock is streamed as one more window (dockWindowID): the region of the screen it takes,
// captured with only the Dock and the desktop picture in it (no app window behind it ever
// shows). With the Mac's wallpaper set to the PC's (the viewer sends it), that strip reads as
// the Dock floating over the PC's desktop. Clicks go to the Dock at that spot; its menus and
// stacks are shown over it as popups; apps it opens are shown on Windows (WindowTracker's
// adoption after the viewer's input).
//
// The wallpaper is changed only while a viewer asks for it, and always put back: the Mac's own
// is saved first (once), and restored when the session ends, on `--stop`, or at the next start
// after a crash. A Dock that hides itself is left alone: MacBridge never changes that setting.
import Foundation
import AppKit
import ApplicationServices
import UniformTypeIdentifiers
import ImageIO

/// The reserved window id the Dock is streamed as (next to the Mac Desktop's).
let dockWindowID: CGWindowID = 0x7FFF_0002

private func dockPref<T>(_ key: String) -> T? {
    CFPreferencesCopyAppValue(key as CFString, "com.apple.dock" as CFString) as? T
}

private func fAX(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}

final class DockMirror {
    private let lock = NSLock()
    private var region: CGRect?
    private var edge = "bottom"
    private var pid: pid_t = 0
    private var wanted = false
    private var stream: WindowStream?
    /// a status to send to the viewer ("dock_status")
    var onStatus: (([String: Any]) -> Void)?
    var onPacket: ((VideoPacket) -> Void)?
    /// the Dock is shown on Windows: (its pid, the region) for the window tracker, or nil
    var onShown: (((pid_t, CGRect)?) -> Void)?

    /// The region of the screen streamed as the Dock (screen points), while it is.
    var rect: CGRect? { lock.lock(); defer { lock.unlock() }; return wanted ? region : nil }

    /// Where the Dock is now: (region, edge, pid), or why it cannot be shown.
    func locate() -> Result<(CGRect, String, pid_t), WireError> {
        CFPreferencesAppSynchronize("com.apple.dock" as CFString)
        if dockPref("autohide") ?? false {
            return .failure(WireError(description: "The Mac's Dock hides itself. To see it on Windows, turn off \"Automatically hide and show the Dock\" in System Settings > Desktop & Dock on the Mac."))
        }
        guard let app = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.dock").first else {
            return .failure(WireError(description: "the Dock is not running on the Mac"))
        }
        let ax = AXUIElementCreateApplication(app.processIdentifier)
        let kids = fAX(ax, kAXChildrenAttribute as String) as? [AXUIElement] ?? []
        guard let list = kids.first(where: { (fAX($0, kAXRoleAttribute as String) as? String) == (kAXListRole as String) }) else {
            return .failure(WireError(description: "the Dock could not be read (Accessibility)"))
        }
        var p = CGPoint.zero, s = CGSize.zero
        guard let pv = fAX(list, kAXPositionAttribute as String), let sv = fAX(list, kAXSizeAttribute as String),
              AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &s), s.width > 8, s.height > 8 else {
            return .failure(WireError(description: "the Dock's place on the screen is unknown"))
        }
        let orientation: String = dockPref("orientation") ?? "bottom"
        // room for the icons growing under the pointer (magnification), on the inner side
        let grow: Bool = dockPref("magnification") ?? false
        let large: Double = dockPref("largesize") ?? 0, tile: Double = dockPref("tilesize") ?? 48
        let head = grow ? CGFloat(max(0, large - tile)) : 0
        var r = CGRect(origin: p, size: s).insetBy(dx: -6, dy: -6)
        switch orientation {
        case "left": r.size.width += head
        case "right": r.origin.x -= head; r.size.width += head
        default: r.origin.y -= head; r.size.height += head
        }
        // within its screen, on whole points
        var ids = [CGDirectDisplayID](repeating: 0, count: 4), n: UInt32 = 0
        CGGetDisplaysWithPoint(CGPoint(x: r.midX, y: r.midY), 4, &ids, &n)
        if n > 0 { r = r.intersection(CGDisplayBounds(ids[0])) }
        r = r.integral
        // even sizes: the video encoder pads an odd one with a row or column of nothing, which
        // showed as a green line along the Dock (taken from the margin, on the inner side)
        if Int(r.width) % 2 == 1 { if orientation == "right" { r.origin.x += 1 }; r.size.width -= 1 }
        if Int(r.height) % 2 == 1 { if orientation != "left" && orientation != "right" { r.origin.y += 1 }; r.size.height -= 1 }
        guard r.width > 8, r.height > 8 else { return .failure(WireError(description: "the Dock is off the screen")) }
        return .success((r, orientation, app.processIdentifier))
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

    /// Follow the Dock (apps added make it wider, the Dock restarting): called every few seconds.
    func refresh(force: Bool = false) {
        lock.lock(); let on = wanted; let old = region; let oldPid = pid; lock.unlock()
        guard on else { return }
        switch locate() {
        case .failure(let e):
            let had = old != nil || force
            lock.lock(); let s = stream; stream = nil; region = nil; lock.unlock()
            if let s = s { Task { await s.stop() } }
            onShown?(nil)
            if had {
                log("Mac Dock not shown: \(e.description)")
                onStatus?(["type": "dock_status", "available": false, "window_id": Int(dockWindowID), "bounds": rectJSON(.zero), "edge": "", "reason": e.description])
            }
        case .success(let (r, edge, dockPid)):
            guard force || r != old || dockPid != oldPid else { return }
            lock.lock(); region = r; self.edge = edge; pid = dockPid; let s = stream; lock.unlock()
            if let s = s { Task { await s.stop() } }
            // only the Dock and the desktop picture: no app window behind the Dock shows
            let level = Int(CGWindowLevelForKey(.desktopWindow))
            let all = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
            var apps: Set<pid_t> = [dockPid]
            for w in all where (w[kCGWindowLayer as String] as? Int) == level { if let p = w[kCGWindowOwnerPID as String] as? Int32 { apps.insert(p) } }
            var ids = [CGDirectDisplayID](repeating: 0, count: 4), n: UInt32 = 0
            CGGetDisplaysWithPoint(CGPoint(x: r.midX, y: r.midY), 4, &ids, &n)
            let ws = WindowStream(windowID: dockWindowID) { [weak self] pkt in self?.onPacket?(pkt) }
            ws.region = (n > 0 ? ids[0] : CGMainDisplayID(), r, apps)
            // its outline: the Dock's own windows without the desktop picture (the rest of the
            // strip is clear on Windows)
            if viewerFeatures.contains("mask") {
                ws.shapeApps = [dockPid]
                ws.onShape = { w, h, a in sendShape(dockWindowID, w, h, a) }
            }
            lock.lock(); stream = ws; lock.unlock()
            onShown?((dockPid, r))
            onStatus?(["type": "dock_status", "available": true, "window_id": Int(dockWindowID), "bounds": rectJSON(r), "edge": edge])
            log("Mac Dock on Windows: \(Int(r.width))x\(Int(r.height)) at \(Int(r.minX)),\(Int(r.minY)) (\(edge))")
            Task { do { try await ws.start() } catch { log("Mac Dock stream failed: \(error)") } }
        }
    }

    func requestKeyframe() { lock.lock(); let s = stream; lock.unlock(); s?.requestKeyframe() }
    func setBitrate(_ b: Int) { lock.lock(); let s = stream; lock.unlock(); s?.setBitrate(b) }
}

// ---- the wallpaper ----------------------------------------------------------------------------

enum Wallpaper {
    static var restoreFile: URL { supportDirectory.appendingPathComponent("wallpaper-restore.json") }

    private static func displayID(_ s: NSScreen) -> Int {
        (s.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.intValue ?? 0
    }

    private static func color(_ hex: String) -> NSColor? {
        let h = hex.trimmingCharacters(in: CharacterSet(charactersIn: "# "))
        guard h.count == 6, let v = UInt32(h, radix: 16) else { return nil }
        return NSColor(srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255, blue: CGFloat(v & 0xff) / 255, alpha: 1)
    }

    /// Whether this file may be used: an image uploaded this session.
    static func rejection(_ path: String, uploads: URL) -> String? {
        let real = URL(fileURLWithPath: path).standardizedFileURL.resolvingSymlinksInPath()
        guard real.path.hasPrefix(uploads.resolvingSymlinksInPath().path + "/") else { return "only an image sent from Windows is used" }
        guard let t = UTType(filenameExtension: real.pathExtension.lowercased()), t.conforms(to: .image) else { return "not a picture" }
        let size = (try? FileManager.default.attributesOfItem(atPath: real.path)[.size] as? NSNumber)?.intValue ?? 0
        guard size > 0, size < 64 << 20 else { return "the picture is empty or too large" }
        return nil
    }

    /// Use `path` (or the plain colour) as the wallpaper of every screen; the Mac's own is
    /// saved first. Returns why it failed.
    static func apply(path: String?, style: String, color hex: String) -> String? {
        let fm = FileManager.default
        try? fm.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
        // the Mac's own wallpaper, kept once (a second change must not save ours as its own)
        if !fm.fileExists(atPath: restoreFile.path) {
            var saved: [[String: Any]] = []
            for s in NSScreen.screens {
                guard let url = NSWorkspace.shared.desktopImageURL(for: s) else { continue }
                let o = NSWorkspace.shared.desktopImageOptions(for: s) ?? [:]
                var opts: [String: Any] = [:]
                if let v = o[.imageScaling] as? NSNumber { opts["scaling"] = v.intValue }
                if let v = o[.allowClipping] as? NSNumber { opts["clipping"] = v.boolValue }
                if let c = (o[.fillColor] as? NSColor)?.usingColorSpace(.sRGB) { opts["fill"] = [c.redComponent, c.greenComponent, c.blueComponent] }
                saved.append(["display": displayID(s), "url": url.absoluteString, "options": opts])
            }
            guard !saved.isEmpty, let data = try? JSONSerialization.data(withJSONObject: ["screens": saved]) else {
                return "the Mac's own wallpaper could not be read, so it is left as it is"
            }
            do { try data.write(to: restoreFile, options: .atomic) } catch { return "the Mac's own wallpaper could not be saved, so it is left as it is" }
        }
        let fill = color(hex) ?? .black
        // our picture under a new name each time (the system keeps pictures by name)
        let stamp = Int(Date().timeIntervalSince1970 * 1000)
        for f in (try? fm.contentsOfDirectory(atPath: supportDirectory.path)) ?? [] where f.hasPrefix("fusion-wallpaper-") {
            try? fm.removeItem(at: supportDirectory.appendingPathComponent(f))
        }
        let url: URL
        if let p = path {
            let ext = URL(fileURLWithPath: p).pathExtension.lowercased()
            url = supportDirectory.appendingPathComponent("fusion-wallpaper-\(stamp).\(ext)")
            do { try fm.copyItem(at: URL(fileURLWithPath: p), to: url) } catch { return "the picture could not be copied: \(error.localizedDescription)" }
        } else {
            // a plain colour: a small picture of it, filling the screen
            url = supportDirectory.appendingPathComponent("fusion-wallpaper-\(stamp).png")
            guard let ctx = CGContext(data: nil, width: 64, height: 64, bitsPerComponent: 8, bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return "no picture of the colour" }
            ctx.setFillColor(fill.cgColor); ctx.fill(CGRect(x: 0, y: 0, width: 64, height: 64))
            guard let img = ctx.makeImage(), let dst = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else { return "no picture of the colour" }
            CGImageDestinationAddImage(dst, img, nil)
            guard CGImageDestinationFinalize(dst) else { return "no picture of the colour" }
        }
        var opts: [NSWorkspace.DesktopImageOptionKey: Any] = [.fillColor: fill]
        switch style {
        case "fit": opts[.imageScaling] = NSImageScaling.scaleProportionallyUpOrDown.rawValue; opts[.allowClipping] = false
        case "stretch": opts[.imageScaling] = NSImageScaling.scaleAxesIndependently.rawValue; opts[.allowClipping] = true
        case "center": opts[.imageScaling] = NSImageScaling.scaleNone.rawValue; opts[.allowClipping] = false
        default: opts[.imageScaling] = NSImageScaling.scaleProportionallyUpOrDown.rawValue; opts[.allowClipping] = true // fill, span, tile
        }
        for s in NSScreen.screens {
            do { try NSWorkspace.shared.setDesktopImageURL(url, for: s, options: opts) } catch {
                restore()
                return "the wallpaper could not be set: \(error.localizedDescription)"
            }
        }
        log("wallpaper: the PC's, on \(NSScreen.screens.count) screen(s) (the Mac's own is kept to put back)")
        return nil
    }

    /// Put the Mac's own wallpaper back (nothing to do when it was never changed).
    static func restore() {
        let fm = FileManager.default
        guard let data = try? Data(contentsOf: restoreFile),
              let j = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let screens = j["screens"] as? [[String: Any]], !screens.isEmpty else { return }
        for s in NSScreen.screens {
            let e = screens.first { ($0["display"] as? Int) == displayID(s) } ?? screens[0]
            guard let us = e["url"] as? String, let url = URL(string: us) else { continue }
            let o = e["options"] as? [String: Any] ?? [:]
            var opts: [NSWorkspace.DesktopImageOptionKey: Any] = [:]
            if let v = o["scaling"] as? Int { opts[.imageScaling] = v }
            if let v = o["clipping"] as? Bool { opts[.allowClipping] = v }
            if let c = o["fill"] as? [Double], c.count == 3 { opts[.fillColor] = NSColor(srgbRed: CGFloat(c[0]), green: CGFloat(c[1]), blue: CGFloat(c[2]), alpha: 1) }
            do { try NSWorkspace.shared.setDesktopImageURL(url, for: s, options: opts) } catch { log("wallpaper: could not put back \(us): \(error)") }
        }
        try? fm.removeItem(at: restoreFile)
        for f in (try? fm.contentsOfDirectory(atPath: supportDirectory.path)) ?? [] where f.hasPrefix("fusion-wallpaper-") {
            try? fm.removeItem(at: supportDirectory.appendingPathComponent(f))
        }
        log("wallpaper: the Mac's own is back")
    }

    /// The PC's wallpaper is on the Mac now (a restore is pending).
    static var applied: Bool { FileManager.default.fileExists(atPath: restoreFile.path) }
}
