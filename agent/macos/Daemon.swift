// Running in the background: `./macbridge --password X` shows the ID and the password, then
// leaves the terminal (a copy of itself carries on in its own session, so closing the terminal
// window does not end it); `./macbridge --stop` ends it. Scripts (stdout not a terminal) and
// --foreground stay in the foreground.
import Foundation
import ApplicationServices
import CoreGraphics

private var supportDirectory: URL {
    FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RemoteMac", isDirectory: true)
}
private var pidFile: URL { supportDirectory.appendingPathComponent("macbridge.pid") }

let backgroundLogPath = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Logs/MacBridge/macbridge.log").path

/// The pid of the MacBridge running in the background, if one is.
func runningInBackground() -> pid_t? {
    guard let s = try? String(contentsOf: pidFile, encoding: .utf8), let pid = pid_t(s.trimmingCharacters(in: .whitespacesAndNewlines)),
          pid > 0, pid != getpid(), kill(pid, 0) == 0 else { return nil }
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
    return p.processIdentifier
}

/// In the background copy: own session (no terminal hang-up reaches us), and the pid file.
func becomeDaemon() {
    _ = setsid() // fails harmlessly after a restart for the next client (already the leader)
    signal(SIGHUP, SIG_IGN)
    try? FileManager.default.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
    try? "\(getpid())\n".write(to: pidFile, atomically: true, encoding: .utf8)
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
    print("MacBridge stopped (pid \(pid)).")
    return true
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
