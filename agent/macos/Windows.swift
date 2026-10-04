// Polls the window server and turns changes into WINDOW_* events. Besides the windows of launched
// apps it follows the windows an app makes appear elsewhere, so none of them get "lost":
//  - dialogs, sheets and panels of the app itself (classified via the Accessibility subrole),
//  - file Open/Save panels drawn by the out-of-process panel service (sandboxed apps use it),
//  - companion apps the launched app starts (Simulator for Xcode) and Finder windows it reveals.
import Foundation
import CoreGraphics
import AppKit
import ApplicationServices

enum Role: String { case window, dialog, open_panel, save_panel, popup }

/// Pop-up menus (a pop-up button's list, a context menu) are at this window level.
private let popUpMenuLayer = 101
/// Modal panels (an alert run modally): dialogs, not popups.
private let modalPanelLayer = 8

struct WinInfo: Equatable {
    var id: CGWindowID, pid: pid_t, title: String, rect: CGRect
    var appID: String = "unknown", role: Role = .window, parent: CGWindowID? = nil
    /// Height of the Mac title bar cut off the stream (the viewer draws its own title bar).
    var inset: CGFloat = 0
    /// What is streamed: the window without its title bar.
    var content: CGRect { CGRect(x: rect.minX, y: rect.minY + inset, width: rect.width, height: max(2, rect.height - inset)) }
}

func rectJSON(_ r: CGRect) -> [String: Any] { ["x": Int(r.minX), "y": Int(r.minY), "w": Int(r.width), "h": Int(r.height)] }

/// Processes whose windows are shown on behalf of whichever launched app is in front.
private let panelServiceBundles: Set<String> = [
    "com.apple.appkit.xpc.openAndSavePanelService",
    "com.apple.quicklook.QuickLookUIService",
]
/// Companion apps: bundle id -> registered application id they are presented as.
private let companionBundles: [String: String] = [
    "com.apple.iphonesimulator": "simulator",
    "com.apple.finder": "finder",
]

private func wAX(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
private func wAXString(_ el: AXUIElement, _ name: String) -> String? { wAX(el, name) as? String }

/// The AX window of `pid` whose frame matches `rect` exactly (no guessing).
func axWindowMatching(pid: pid_t, rect: CGRect) -> AXUIElement? {
    let wins = wAX(AXUIElementCreateApplication(pid), kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    for w in wins {
        var p = CGPoint.zero, s = CGSize.zero
        if let pv = wAX(w, kAXPositionAttribute as String), let sv = wAX(w, kAXSizeAttribute as String),
           AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &s),
           abs(p.x - rect.minX) < 3, abs(p.y - rect.minY) < 3, abs(s.width - rect.width) < 3, abs(s.height - rect.height) < 3 { return w }
    }
    return nil
}

/// Titles of buttons inside an AX element (bounded search: panels nest their buttons in groups).
private func buttonTitles(_ root: AXUIElement) -> Set<String> {
    var out = Set<String>(), stack = [(root, 0)], visited = 0
    while let (el, depth) = stack.popLast(), visited < 400 {
        visited += 1
        if wAXString(el, kAXRoleAttribute as String) == (kAXButtonRole as String), let t = wAXString(el, kAXTitleAttribute as String), !t.isEmpty { out.insert(t) }
        if depth < 7, let kids = wAX(el, kAXChildrenAttribute as String) as? [AXUIElement] { for k in kids { stack.append((k, depth + 1)) } }
    }
    return out
}

/// Window / dialog / open panel / save panel, from the AX subrole and the window's buttons.
/// File panels of non-sandboxed apps are drawn in-process and report AXStandardWindow, so every
/// window after an app's first one is checked for the panel's buttons, whatever its subrole.
func classify(pid: pid_t, rect: CGRect, fromPanelService: Bool, isFirstWindow: Bool) -> Role {
    guard let w = axWindowMatching(pid: pid, rect: rect) else { return fromPanelService ? .dialog : .window }
    let standard = wAXString(w, kAXSubroleAttribute as String) == (kAXStandardWindowSubrole as String)
    if !isFirstWindow || fromPanelService || !standard {
        let buttons = buttonTitles(w)
        if buttons.contains("Cancel") {
            if buttons.contains("Open") || buttons.contains("Choose") { return .open_panel }
            if buttons.contains("Save") { return .save_panel }
        }
    }
    return standard && !fromPanelService ? .window : .dialog
}

/// A popover, pop-up list or completion window over the app's window (Xcode's search options,
/// its code completion): not an Accessibility window of the app at all, or one with neither a
/// title bar nor a dialog/sheet role. Shown by the client over its parent, not as a window.
func isPopup(pid: pid_t, rect: CGRect) -> Bool {
    guard let w = axWindowMatching(pid: pid, rect: rect) else { return true }
    let role = wAXString(w, kAXRoleAttribute as String) ?? "", sub = wAXString(w, kAXSubroleAttribute as String) ?? ""
    if role == "AXPopover" || role == (kAXMenuRole as String) { return true }
    if role == (kAXSheetRole as String) || [kAXStandardWindowSubrole, kAXDialogSubrole, kAXSystemDialogSubrole].map({ $0 as String }).contains(sub) { return false }
    return wAX(w, kAXCloseButtonAttribute as String) == nil
}

/// Height of a plain title bar (traffic lights + title, nothing else in it), else 0. Windows whose
/// toolbar shares the title bar (Xcode, Finder) or whose content runs under it keep it.
func titleBarInset(pid: pid_t, rect: CGRect) -> CGFloat {
    guard let w = axWindowMatching(pid: pid, rect: rect),
          wAXString(w, kAXSubroleAttribute as String) == (kAXStandardWindowSubrole as String) else { return 0 }
    let kids = wAX(w, kAXChildrenAttribute as String) as? [AXUIElement] ?? []
    func top(_ el: AXUIElement) -> CGFloat? {
        var p = CGPoint.zero
        guard let pv = wAX(el, kAXPositionAttribute as String), AXValueGetValue(pv as! AXValue, .cgPoint, &p) else { return nil }
        return p.y - rect.minY
    }
    // a toolbar that shares the title bar (unified, starts at the window's top) keeps it
    let unified = kids.contains { wAXString($0, kAXRoleAttribute as String) == (kAXToolbarRole as String) && (top($0) ?? 99) < 8 }
    if unified {
        log("title bar kept (unified toolbar) pid=\(pid)")
        return 0
    }
    guard let cb = wAX(w, kAXCloseButtonAttribute as String) else {
        log("title bar kept (no close button) pid=\(pid) kids=\(kids.map { (wAXString($0, kAXRoleAttribute as String) ?? "?") + "@" + String(Int(top($0) ?? -1)) })")
        return 0
    }
    var p = CGPoint.zero, s = CGSize.zero
    guard let pv = wAX(cb as! AXUIElement, kAXPositionAttribute as String), let sv = wAX(cb as! AXUIElement, kAXSizeAttribute as String),
          AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &s) else { return 0 }
    let bar = ((p.y - rect.minY) * 2 + s.height).rounded()
    return (20...40).contains(bar) && bar < rect.height / 2 ? bar : 0
}

final class WindowTracker {
    private var known: [CGWindowID: WinInfo] = [:]
    /// Windows seen but not yet reported: dialogs need a moment before their AX tree is complete.
    private var pending: [CGWindowID: Int] = [:]
    /// Windows that already existed when the agent started (the user's desktop): never streamed.
    private var preexisting: Set<CGWindowID> = []
    private var started = false
    private var ticks = 0
    private var servicePids: Set<pid_t> = []
    private var companions: [pid_t: String] = [:]
    private let queue = DispatchQueue(label: "rm.windows")
    private var timer: DispatchSourceTimer?
    let apps: AppManager
    var onCreated: ((WinInfo) -> Void)?
    var onDestroyed: ((CGWindowID) -> Void)?
    var onMoved: ((WinInfo) -> Void)?       // position/size changed
    var onTitle: ((WinInfo) -> Void)?
    var onAppExited: ((String, Int32) -> Void)?

    init(apps: AppManager) { self.apps = apps }

    func current(_ id: CGWindowID) -> WinInfo? { queue.sync { known[id] } }
    /// Whether the app shows a dialog or panel (a "save changes?" sheet, for one).
    func hasDialog(pid: pid_t) -> Bool { queue.sync { known.values.contains { $0.pid == pid && $0.role != .window && $0.role != .popup } } }

    func start() {
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now(), repeating: .milliseconds(100))
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume(); timer = t
    }

    private func refreshHelpers(anyLaunched: Bool) {
        servicePids = []; companions = [:]
        guard anyLaunched else { return }
        for app in NSWorkspace.shared.runningApplications {
            guard let bid = app.bundleIdentifier else { continue }
            if panelServiceBundles.contains(bid) { servicePids.insert(app.processIdentifier) }
            if let id = companionBundles[bid], apps.descriptor(id) != nil { companions[app.processIdentifier] = id }
        }
    }

    /// The launched app the user is working in (for panels drawn by the shared panel service).
    private func frontLaunchedApp() -> String? {
        if let f = NSWorkspace.shared.frontmostApplication, let id = apps.appID(forPid: f.processIdentifier) { return id }
        return apps.pids.compactMap { apps.appID(forPid: $0) }.first
    }

    /// Windows on screen: (id, pid, title, bounds, layer). Normal windows, modal panels, floating
    /// panels, and pop-up menus (which are smaller: a one-item list).
    private func onscreen() -> [(CGWindowID, pid_t, String, CGRect, Int)] {
        let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
        return all.compactMap { w in
            guard let pid = w[kCGWindowOwnerPID as String] as? Int32, let layer = w[kCGWindowLayer as String] as? Int, (0...popUpMenuLayer).contains(layer),
                  let n = w[kCGWindowNumber as String] as? UInt32,
                  let d = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: d as CFDictionary) else { return nil }
            let least: CGFloat = layer == 0 ? 64 : 20
            guard r.width >= least, r.height >= least else { return nil }
            return (CGWindowID(n), pid, w[kCGWindowName as String] as? String ?? "", r, layer)
        }
    }

    /// Parent for a dialog: the app's window containing its centre, else its largest window.
    private func parentFor(appID: String, rect: CGRect) -> CGWindowID? {
        let mains = known.values.filter { $0.appID == appID && $0.role == .window }
        let centre = CGPoint(x: rect.midX, y: rect.minY + 10)
        return (mains.first { $0.rect.contains(centre) } ?? mains.max { $0.rect.width * $0.rect.height < $1.rect.width * $1.rect.height })?.id
    }

    private func tick() {
        ticks += 1
        let launched = Set(apps.pids)
        if ticks % 5 == 1 { refreshHelpers(anyLaunched: !launched.isEmpty) }
        let windows = onscreen()
        if !started { preexisting = Set(windows.map { $0.0 }); started = true }

        var seen = Set<CGWindowID>()
        for (id, pid, title, rect, layer) in windows where !preexisting.contains(id) {
            let fromService = servicePids.contains(pid)
            guard launched.contains(pid) || fromService || companions[pid] != nil else { continue }
            seen.insert(id)
            if var old = known[id] {
                if old.rect != rect { old.rect = rect; known[id] = old; onMoved?(old) }
                if old.title != title { old.title = title; known[id] = old; onTitle?(old) }
                continue
            }
            // give new windows ~300 ms so their AX tree (buttons, subrole) is in place
            let age = (pending[id] ?? 0) + 1
            pending[id] = age
            // (a pop-up menu is drawn at once: shown without the wait)
            if age < (layer == popUpMenuLayer ? 1 : 3) { continue }
            pending.removeValue(forKey: id)
            guard let appID = apps.appID(forPid: pid) ?? companions[pid] ?? (fromService ? frontLaunchedApp() : nil) else { continue }
            let first = !known.values.contains { $0.appID == appID && $0.role == .window }
            // a menu, popover or completion list over a window the app already shows is a popup
            let overMain = known.values.contains { $0.appID == appID && $0.role == .window && $0.rect.intersects(rect.insetBy(dx: -40, dy: -40)) }
            let popup = companions[pid] == nil && !fromService && overMain && layer != modalPanelLayer
                && (layer == popUpMenuLayer || (layer != 0 || !first) && isPopup(pid: pid, rect: rect))
            let role = companions[pid] != nil ? Role.window : popup ? Role.popup : classify(pid: pid, rect: rect, fromPanelService: fromService, isFirstWindow: first)
            var w = WinInfo(id: id, pid: pid, title: title, rect: rect, appID: appID, role: role)
            if role != .window { w.parent = parentFor(appID: appID, rect: rect) }
            if role == .window { w.inset = titleBarInset(pid: pid, rect: rect) }
            known[id] = w
            onCreated?(w)
        }
        for id in Array(pending.keys) where !seen.contains(id) { pending.removeValue(forKey: id) }
        // a window that left the on-screen list but still exists (another display, another Space)
        // is not gone; it is only reported destroyed once the window server forgets it
        // (a popup that left the screen is closed: menus keep their window for the next time)
        for id in known.keys where !seen.contains(id) && known[id]?.role != .popup {
            if (CGWindowListCopyWindowInfo([.optionIncludingWindow], id) as? [[String: Any]])?.isEmpty == false { seen.insert(id) }
        }
        // children are reported gone before their parents
        let gone = known.keys.filter { !seen.contains($0) }.sorted { (known[$0]?.parent != nil ? 0 : 1) < (known[$1]?.parent != nil ? 0 : 1) }
        for id in gone { known.removeValue(forKey: id); onDestroyed?(id) }
        for (id, code) in apps.reapExited() { onAppExited?(id, code) }
    }
}
