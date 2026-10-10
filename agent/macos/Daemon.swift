// Running in the background: `./macbridge --password X` shows the ID and the password, then
// leaves the terminal (a copy of itself carries on in its own session, so closing the terminal
// window does not end it); `./macbridge --stop` ends it. Scripts (stdout not a terminal) and
// --foreground stay in the foreground.
import Foundation
import ApplicationServices
import CoreGraphics

var supportDirectory: URL {
    FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RemoteMac", isDirectory: true)
}
private var pidFile: URL { supportDirectory.appendingPathComponent("macbridge.pid") }

let backgroundLogPath = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/MacBridge/macbridge.log").path

/// The pid of the MacBridge running in the background, if one is.
func runningInBackground() -> pid_t? {
    guard let s = try? String(contentsOf: pidFile, encoding: .utf8), let pid = pid_t(s.trimmingCharacters(in: .whitespacesAndNewlines)),
          pid > 0, pid != getpid(), pid != getppid(), kill(pid, 0) == 0 else { return nil }
    return pid
}

/// Start a copy of this program in the background (same arguments; ID and password through the
/// environment). Its output goes to the log when logs are on, else nowhere.
func startInBackground() -> pid_t? {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: Bundle.main.executablePath ?? CommandLine.arguments[0])
    p.arguments = Array(CommandLine.arguments.dropFirst())
    var e = ProcessInfo.processInfo.environment
    e["RM_DAEMON"] = "1"
    e["RM_QUIET_BANNER"] = "1"
    p.environment = e
    p.standardInput = FileHandle.nullDevice
    if logsEnabled {
        let dir = (backgroundLogPath as NSString).deletingLastPathComponent
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        if !FileManager.default.fileExists(atPath: backgroundLogPath) { FileManager.default.createFile(atPath: backgroundLogPath, contents: nil) }
        let h = FileHandle(forWritingAtPath: backgroundLogPath)
        h?.seekToEndOfFile()
        p.standardOutput = h ?? FileHandle.nullDevice
        p.standardError = h ?? FileHandle.nullDevice
    } else {
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
    }
    do { try p.run() } catch { return nil }
    // on its own (its own session, its pid file) before this one returns and the terminal may
    // close: a hang-up before then would end it
    let mine = "\(p.processIdentifier)"
    for _ in 0..<60 where (try? String(contentsOf: pidFile, encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) != mine { usleep(50_000) }
    return p.processIdentifier
}

/// In the background copy: own session (no terminal hang-up reaches us), and the pid file.
func becomeDaemon() {
    _ = setsid() // fails harmlessly after a restart for the next client (already the leader)
    signal(SIGHUP, SIG_IGN)
    try? FileManager.default.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
    try? "\(getpid())\n".write(to: pidFile, atomically: true, encoding: .utf8)
}

/// The worker the background copy watches over (for its signal handler).
private var supervisedWorker: pid_t = 0

/// The background copy watches over the MacBridge that does the work: a worker that dies (a
/// crash) is started again at once, so the Mac never stays unreachable. `--stop` ends both.
func superviseWorker() -> Never {
    signal(SIGTERM) { _ in
        if supervisedWorker > 0 { kill(supervisedWorker, SIGTERM) }
        _exit(0)
    }
    let path = Bundle.main.executablePath ?? CommandLine.arguments[0]
    var ends: [Date] = []
    while true {
        var env = ProcessInfo.processInfo.environment
        env["RM_WORKER"] = "1"
        let argv: [UnsafeMutablePointer<CChar>?] = CommandLine.arguments.map { strdup($0) } + [nil]
        let envp: [UnsafeMutablePointer<CChar>?] = env.map { strdup("\($0.key)=\($0.value)") } + [nil]
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, path, nil, nil, argv, envp)
        for p in argv + envp { free(p) }
        if rc != 0 { log("could not start the worker (\(rc)); trying again"); sleep(5); continue }
        supervisedWorker = pid
        log("worker started (pid \(pid))")
        var status: Int32 = 0
        while waitpid(pid, &status, 0) == -1 && errno == EINTR {}
        supervisedWorker = 0
        let signalled = status & 0x7f
        log("the worker ended (\(signalled != 0 ? "signal \(signalled)" : "exit \((status >> 8) & 0xff)")): starting it again")
        // a session it was serving may have left the PC's wallpaper on the Mac
        Wallpaper.restore()
        // one that keeps ending right away is started more slowly
        ends = ends.filter { $0.timeIntervalSinceNow > -60 } + [Date()]
        sleep(ends.count > 4 ? 15 : 1)
    }
}

/// `--stop`: end the MacBridge running in the background.
func stopBackground() -> Bool {
    guard let pid = runningInBackground() else {
        print("MacBridge is not running in the background.")
        return false
    }
    kill(pid, SIGTERM)
    for _ in 0..<30 where kill(pid, 0) == 0 { usleep(100_000) }
    if kill(pid, 0) == 0 { kill(pid, SIGKILL) }
    try? FileManager.default.removeItem(at: pidFile)
    Wallpaper.restore() // the Mac's own wallpaper, if a session had changed it
    print("MacBridge stopped (pid \(pid)).")
    return true
}

/// `--check-permissions`: what macOS allows this program (through the app it runs in), without
/// asking for anything. 0 when all is there, 3 when something is missing.
func checkPermissions() -> Int32 {
    let screen = CGPreflightScreenCaptureAccess()
    let ax = AXIsProcessTrusted()
    let gui = CGSessionCopyCurrentDictionary() != nil
    func line(_ ok: Bool, _ what: String, _ why: String) -> String { "  \(ok ? "ok     " : "MISSING") \(what)\(ok ? "" : ": \(why)")" }
    print("MacBridge permissions:")
    print(line(gui, "logged-in desktop session", "run MacBridge from a logged-in user's desktop (not over SSH alone)"))
    print(line(screen, "Screen Recording", "System Settings > Privacy & Security > Screen Recording"))
    print(line(ax, "Accessibility", "System Settings > Privacy & Security > Accessibility"))
    return gui && screen && ax ? 0 : 3
}

/// What macOS still has to allow (asked for here, so the system's prompt opens now).
func permissionWarnings() -> [String] {
    var w: [String] = []
    if !CGPreflightScreenCaptureAccess() {
        CGRequestScreenCaptureAccess()
        w.append("Allow Screen Recording for this terminal app (System Settings > Privacy & Security > Screen Recording), then run this again.")
    }
    if !AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary) {
        w.append("Allow Accessibility for this terminal app (System Settings > Privacy & Security > Accessibility), then run this again.")
    }
    return w
}
