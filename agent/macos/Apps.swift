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

final class AppManager {
    private var apps: [AppDescriptor]
    private var running: [String: Process] = [:]
    /// Apps that were already running and that we present rather than spawn (Finder): id -> pid.
    private var adopted: [String: pid_t] = [:]
    private let lock = NSLock()
    private let testapp: AppDescriptor

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
        lock.lock(); apps = list; lock.unlock()
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
            r.activate(options: [])
            return (r.processIdentifier, nil)
        }
        let p = Process(); p.executableURL = URL(fileURLWithPath: d.executable); p.arguments = args
        do { try p.run() } catch { return (nil, ("launch_failed", "\(error)")) }
        running[id] = p
        return (p.processIdentifier, nil)
    }

    func terminate(id: String) -> Bool {
        lock.lock(); let p = running.removeValue(forKey: id); let a = adopted.removeValue(forKey: id); lock.unlock()
        if a != nil { return true }   // never kill an app we did not start
        guard let proc = p else { return false }
        proc.terminate(); proc.waitUntilExit()
        return true
    }

    /// Apps that exited on their own (crash / quit): returns their ids and forgets them.
    func reapExited() -> [(String, Int32)] {
        lock.lock(); defer { lock.unlock() }
        var out: [(String, Int32)] = []
        for (id, p) in running where !p.isRunning { out.append((id, p.terminationStatus)); running.removeValue(forKey: id) }
        return out
    }

    func terminateAll() { for id in Array(running.keys) { _ = terminate(id: id) } }
}
