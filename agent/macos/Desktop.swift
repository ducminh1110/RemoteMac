// "Mac Desktop": the whole screen as one remote window, for controlling the Mac itself rather
// than single apps. Offered as a pseudo application ("desktop") so clients list, launch and close
// it like any app; its window id is reserved (no real window has it).
import Foundation
import CoreGraphics

let desktopAppID = "desktop"
let desktopWindowID: CGWindowID = 0x7FFF_0001

final class DesktopSession {
    private(set) var active = false
    private(set) var displayID: CGDirectDisplayID = CGMainDisplayID()
    private let lock = NSLock()

    var isActive: Bool { lock.lock(); defer { lock.unlock() }; return active }
    /// The streamed display, global coordinates (input maps onto it).
    var bounds: CGRect { CGDisplayBounds(displayID) }

    /// Starts on the main display (the Mac's own screen). Returns the window_created message.
    func start() -> [String: Any] {
        lock.lock(); active = true; displayID = CGMainDisplayID(); lock.unlock()
        let b = bounds
        log("desktop session: display \(displayID) \(Int(b.width))x\(Int(b.height))")
        return ["type": "window_created", "window_id": Int(desktopWindowID), "application_id": desktopAppID, "title": "Mac Desktop",
                "bounds": rectJSON(b), "parent_id": NSNull(), "role": "window"]
    }

    func stop() {
        lock.lock(); active = false; lock.unlock()
    }
}
