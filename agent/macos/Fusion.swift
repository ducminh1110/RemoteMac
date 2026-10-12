// The Mac's wallpaper, as older versions' Desktop Fusion changed it: put back if it still is
// (the Mac's own was saved first, and is restored at the next start, on `--stop`, and when a
// session ends). Nothing changes it any more.
import Foundation
import AppKit

// ---- the wallpaper ----------------------------------------------------------------------------

enum Wallpaper {
    static var restoreFile: URL { supportDirectory.appendingPathComponent("wallpaper-restore.json") }

    private static func displayID(_ s: NSScreen) -> Int {
        (s.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.intValue ?? 0
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
