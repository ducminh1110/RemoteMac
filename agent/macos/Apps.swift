// Server-side application allowlist. The client names an id; we never run client-supplied strings.
import Foundation
import AppKit

struct AppDescriptor { let id: String, name: String, executable: String, maxArgs: Int }

final class AppManager {
    let apps: [AppDescriptor]
    private var running: [String: Process] = [:]
    /// Apps that were already running and that we present rather than spawn (Finder): id -> pid.
    private var adopted: [String: pid_t] = [:]
    private let lock = NSLock()

    init() {
        let cwd = FileManager.default.currentDirectoryPath
        let testapp = ProcessInfo.processInfo.environment["RM_TESTAPP"] ?? cwd + "/out/rm-testapp"
        apps = [
            AppDescriptor(id: "testapp", name: "RM Test App", executable: testapp, maxArgs: 0),
            AppDescriptor(id: "textedit", name: "TextEdit", executable: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit", maxArgs: 0),
            AppDescriptor(id: "xcode", name: "Xcode", executable: "/Applications/Xcode.app/Contents/MacOS/Xcode", maxArgs: 4),
            AppDescriptor(id: "simulator", name: "Simulator", executable: "/Applications/Xcode.app/Contents/Developer/Applications/Simulator.app/Contents/MacOS/Simulator", maxArgs: 0),
            AppDescriptor(id: "finder", name: "Finder", executable: "/System/Library/CoreServices/Finder.app/Contents/MacOS/Finder", maxArgs: 0),
        ]
    }

    func list() -> [[String: Any]] {
        // the whole Mac first, then its apps
        [["id": desktopAppID, "name": "Mac Desktop", "available": true]]
            + apps.map { ["id": $0.id, "name": $0.name, "available": FileManager.default.isExecutableFile(atPath: $0.executable)] }
    }

    func descriptor(_ id: String) -> AppDescriptor? { apps.first { $0.id == id } }

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
        guard let d = apps.first(where: { $0.id == id }) else { return (nil, ("launch_rejected", "unknown application '\(id)'")) }
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
