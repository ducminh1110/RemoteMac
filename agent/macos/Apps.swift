// Server-side application allowlist. The client names an id; we never run client-supplied strings.
import Foundation
import AppKit

struct AppDescriptor { let id: String, name: String, executable: String, maxArgs: Int; var bundleID: String? = nil }

/// Where the Mac's applications live (one level, plus the Utilities folders).
let appFolders: [String] = ["/Applications", "/Applications/Utilities", "/System/Applications", "/System/Applications/Utilities",
                            NSHomeDirectory() + "/Applications"]

/// Short ids for apps the tests and older clients know by name; everything else is its bundle id.
let knownIDs: [String: String] = ["com.apple.TextEdit": "textedit", "com.apple.dt.Xcode": "xcode", "com.apple.iphonesimulator": "simulator", "com.apple.finder": "finder"]

/// Every .app bundle in the application folders: name, executable, bundle id from Info.plist.
func scanApplications() -> [AppDescriptor] {
    var out: [AppDescriptor] = [], seen = Set<String>()
    let fm = FileManager.default
    func add(_ path: String) {
        guard let b = Bundle(path: path), let exe = b.executablePath, fm.isExecutableFile(atPath: exe) else { return }
        let bid = b.bundleIdentifier ?? path
        guard !seen.contains(bid) else { return }
        seen.insert(bid)
        let info = b.infoDictionary ?? [:]
        let name = (b.localizedInfoDictionary?["CFBundleDisplayName"] as? String) ?? (info["CFBundleDisplayName"] as? String)
            ?? (info["CFBundleName"] as? String) ?? ((path as NSString).lastPathComponent as NSString).deletingPathExtension
        out.append(AppDescriptor(id: knownIDs[bid] ?? bid.lowercased(), name: name, executable: exe, maxArgs: bid == "com.apple.dt.Xcode" ? 4 : 0, bundleID: bid))
    }
    for dir in appFolders {
        guard let items = try? fm.contentsOfDirectory(atPath: dir) else { continue }
        for item in items.sorted() where item.hasSuffix(".app") { add(dir + "/" + item) }
    }
    // apps that are not in a folder but always there
    add("/System/Library/CoreServices/Finder.app")
    add("/Applications/Xcode.app/Contents/Developer/Applications/Simulator.app")
    return out.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
}

/// System UI and helpers that are never shown as an app of their own (Finder is a companion).
let neverAdopted: Set<String> = [
    "com.apple.finder", "com.apple.dock", "com.apple.controlcenter", "com.apple.notificationcenterui", "com.apple.Spotlight",
    "com.apple.loginwindow", "com.apple.systemuiserver", "com.apple.WindowManager", "com.apple.SecurityAgent", "com.apple.UserNotificationCenter",
    "com.apple.screencaptureui", "com.apple.ScreenContinuity",
]

/// An app we started: through LaunchServices (as the Dock and Finder open apps), or for a bare
/// executable (the test app) as a child process.
enum Launched {
    case app(NSRunningApplication)
    case process(Process)
    var processIdentifier: pid_t { switch self { case .app(let a): return a.processIdentifier; case .process(let p): return p.processIdentifier } }
    var isRunning: Bool { switch self { case .app(let a): return !a.isTerminated; case .process(let p): return p.isRunning } }
    var terminationStatus: Int32 { switch self { case .app: return 0; case .process(let p): return p.terminationStatus } }
}

final class AppManager {
    private var apps: [AppDescriptor]
    private var running: [String: Launched] = [:]
    /// Apps that were already running and that we present rather than spawn (Finder): id -> pid.
    private var adopted: [String: pid_t] = [:]
    private let lock = NSLock()
    private let testapp: AppDescriptor
    /// Apps opened from the session that are not in the application folders (kept across rescans).
    private var extras: [AppDescriptor] = []

    init() {
        let cwd = FileManager.default.currentDirectoryPath
        let path = ProcessInfo.processInfo.environment["RM_TESTAPP"] ?? cwd + "/out/rm-testapp"
        testapp = AppDescriptor(id: "testapp", name: "RM Test App", executable: path, maxArgs: 0)
        apps = []
        rescan()
    }

    /// Read the application folders again (apps installed or removed since).
    func rescan() {
        var list = scanApplications()
        if FileManager.default.isExecutableFile(atPath: testapp.executable) { list.insert(testapp, at: 0) }
        lock.lock(); apps = list + extras.filter { e in !list.contains { $0.id == e.id } }; lock.unlock()
    }

    func list() -> [[String: Any]] {
        rescan()
        lock.lock(); let a = apps; lock.unlock()
        // the whole Mac first, then its apps (only what is installed)
        return [["id": desktopAppID, "name": "Mac Desktop", "available": true]] + a.map { ["id": $0.id, "name": $0.name, "available": true] }
    }

    func descriptor(_ id: String) -> AppDescriptor? { lock.lock(); defer { lock.unlock() }; return apps.first { $0.id == id } }

    func appID(forPid pid: pid_t) -> String? {
        lock.lock(); defer { lock.unlock() }
        return running.first(where: { $0.value.processIdentifier == pid })?.key ?? adopted.first(where: { $0.value == pid })?.key
    }
    /// pid of a running (or adopted) app id.
    func pidFor(_ id: String) -> pid_t? {
        lock.lock(); defer { lock.unlock() }
        return running[id].map { $0.processIdentifier } ?? adopted[id]
    }
    var pids: [pid_t] { lock.lock(); defer { lock.unlock() }; return running.values.map { $0.processIdentifier } + Array(adopted.values) }

    /// Returns pid or an (code, message) error.
    func launch(id: String, args: [String]) -> (pid: pid_t?, err: (String, String)?) {
        guard let d = descriptor(id) else { return (nil, ("launch_rejected", "unknown application '\(id)'")) }
        if args.count > d.maxArgs { return (nil, ("launch_rejected", "too many arguments (max \(d.maxArgs))")) }
        if args.contains(where: { $0.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) } || $0.utf8.count > 4096 }) {
            return (nil, ("launch_rejected", "argument contains control characters"))
        }
        guard FileManager.default.isExecutableFile(atPath: d.executable) else { return (nil, ("app_not_installed", id)) }
        if id == "finder" {
            // Finder is always running; "launching" it means opening a new Finder window.
            guard let f = NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.finder").first else { return (nil, ("launch_failed", "Finder not running")) }
            NSWorkspace.shared.open(URL(fileURLWithPath: NSHomeDirectory(), isDirectory: true))
            lock.lock(); adopted[id] = f.processIdentifier; lock.unlock()
            return (f.processIdentifier, nil)
        }
        lock.lock(); defer { lock.unlock() }
        if let p = running[id], p.isRunning { return (nil, ("already_running", id)) }
        // already open on the Mac (started there, not by us): show that one, never a second copy
        if let bid = d.bundleID, let r = NSRunningApplication.runningApplications(withBundleIdentifier: bid).first(where: { !$0.isTerminated }) {
            adopted[id] = r.processIdentifier
            // as a click on its Dock icon: shown, its minimized windows back, and an app running
            // with no window at all asked to open one (the reopen event openApplication sends)
            r.unhide()
            let axApp = AXUIElementCreateApplication(r.processIdentifier)
            var v: CFTypeRef?
            if AXUIElementCopyAttributeValue(axApp, kAXWindowsAttribute as CFString, &v) == .success, let wins = v as? [AXUIElement] {
                for w in wins { AXUIElementSetAttributeValue(w, kAXMinimizedAttribute as CFString, kCFBooleanFalse) }
            }
            if let bundle = bundlePath(d.executable) {
                let cfg = NSWorkspace.OpenConfiguration()
                cfg.activates = true
                cfg.addsToRecentItems = false
                NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: bundle), configuration: cfg) { _, _ in }
            } else {
                r.activate(options: [.activateIgnoringOtherApps])
            }
            return (r.processIdentifier, nil)
        }
        // an app bundle opens through LaunchServices, as from the Dock: run as a child of this
        // process (its environment, its Terminal as the "responsible" app, no app registration)
        // sandboxed apps and apps with helper services quit unexpectedly
        if let bundle = bundlePath(d.executable) {
            let cfg = NSWorkspace.OpenConfiguration()
            cfg.arguments = args
            cfg.activates = true
            cfg.addsToRecentItems = false
            cfg.createsNewApplicationInstance = false
            let done = DispatchSemaphore(value: 0)
            var launched: NSRunningApplication?, failure: Error?
            NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: bundle), configuration: cfg) { app, err in
                launched = app; failure = err; done.signal()
            }
            // (the app list stays readable meanwhile: the window tracker asks it many times a second)
            lock.unlock()
            let waited = done.wait(timeout: .now() + 20)
            lock.lock()
            if waited == .timedOut { return (nil, ("launch_failed", "\(id) did not start in time")) }
            guard let app = launched else { return (nil, ("launch_failed", "\(failure.map { "\($0)" } ?? "unknown error")")) }
            running[id] = .app(app)
            return (app.processIdentifier, nil)
        }
        let p = Process(); p.executableURL = URL(fileURLWithPath: d.executable); p.arguments = args
        do { try p.run() } catch { return (nil, ("launch_failed", "\(error)")) }
        running[id] = .process(p)
        return (p.processIdentifier, nil)
    }

    func terminate(id: String) -> Bool {
        lock.lock(); let p = running.removeValue(forKey: id); let a = adopted.removeValue(forKey: id); lock.unlock()
        if a != nil { return true }   // never kill an app we did not start
        switch p {
        case .process(let proc)?:
            // a process that ignores the request is stopped after a while (waiting for it to
            // exit on its own could wait for ever, and the Mac would never take the next viewer)
            let pid = proc.processIdentifier
            proc.terminate()
            for _ in 0..<30 where proc.isRunning && kill(pid, 0) == 0 { usleep(100_000) }
            if proc.isRunning { kill(pid, SIGKILL) }
        case .app(let app)?:
            // asked to quit, as Cmd+Q; forced after a while (gone is checked with the process
            // itself too: `isTerminated` is only kept up to date by a run loop)
            let pid = app.processIdentifier
            app.terminate()
            for _ in 0..<30 where !app.isTerminated && kill(pid, 0) == 0 { usleep(100_000) }
            if !app.isTerminated && kill(pid, 0) == 0 { app.forceTerminate() }
        case nil:
            return false
        }
        return true
    }

    /// Apps that exited on their own (crash / quit): returns their ids and forgets them.
    func reapExited() -> [(String, Int32)] {
        lock.lock(); defer { lock.unlock() }
        var out: [(String, Int32)] = []
        for (id, p) in running where !p.isRunning { out.append((id, p.terminationStatus)); running.removeValue(forKey: id) }
        return out
    }

    /// An app the user opened from the session (a document double-clicked in Finder, a link
    /// opened from an app): it is shown on Windows like one launched from there. Only regular
    /// apps (with a Dock icon); never the system's own UI. Returns its id, nil when it is not one
    /// to show (or already is).
    func adoptOpened(pid: pid_t) -> String? {
        guard pid != getpid(), let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.activationPolicy == .regular, let bid = app.bundleIdentifier, !neverAdopted.contains(bid) else { return nil }
        lock.lock(); defer { lock.unlock() }
        var d = apps.first { $0.bundleID == bid }
        if d == nil, let exe = app.executableURL?.path {
            let nd = AppDescriptor(id: knownIDs[bid] ?? bid.lowercased(), name: app.localizedName ?? bid, executable: exe, maxArgs: 0, bundleID: bid)
            extras.append(nd); apps.append(nd); d = nd
        }
        guard let desc = d else { return nil }
        if let r = running[desc.id], r.isRunning { return nil }
        if let p = adopted[desc.id], p == pid { return nil }
        adopted[desc.id] = pid
        return desc.id
    }

    /// Started by us (not an app that was already open on the Mac).
    func launchedByUs(_ id: String) -> Bool { lock.lock(); defer { lock.unlock() }; return running[id] != nil }

    /// The app is gone: forget it.
    func forget(_ id: String) { lock.lock(); running.removeValue(forKey: id); adopted.removeValue(forKey: id); lock.unlock() }

    /// The .app bundle of a listed app.
    func bundle(of id: String) -> String? { descriptor(id).flatMap { bundlePath($0.executable) } }

    /// The .app bundle an executable belongs to (…/X.app/Contents/MacOS/X), if any.
    private func bundlePath(_ exe: String) -> String? {
        guard let r = exe.range(of: ".app/Contents/MacOS/", options: .backwards) else { return nil }
        return String(exe[..<r.lowerBound]) + ".app"
    }

    /// All at once: each may take a few seconds to quit.
    func terminateAll() {
        lock.lock(); let ids = Array(running.keys); lock.unlock()
        DispatchQueue.concurrentPerform(iterations: ids.count) { i in _ = terminate(id: ids[i]) }
    }
}
