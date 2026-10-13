// Updating this Taro from the viewer ("mac_update", feature "update"): runs taro-update.sh
// (next to this program) in its machine-readable mode and reports how far it is. The files are
// replaced while the viewer stays connected; the new program starts at the next connection (a
// session's end starts this program over, and that is the new file), so nothing is cut off.
import Foundation

enum MacUpdater {
    private static let lock = NSLock()
    private static var running = false

    static func run(install: Bool, send: @escaping ([String: Any]) -> Void) {
        lock.lock()
        if running { lock.unlock(); return }
        running = true
        lock.unlock()
        DispatchQueue.global().async {
            defer { lock.lock(); running = false; lock.unlock() }
            func status(_ state: String, latest: String = "", progress: Int = 0, detail: String = "") {
                send(["type": "mac_update_status", "state": state, "version": appVersion, "latest": latest, "progress": progress, "detail": detail])
            }
            let exe = URL(fileURLWithPath: Bundle.main.executablePath ?? CommandLine.arguments[0]).resolvingSymlinksInPath()
            let script = exe.deletingLastPathComponent().appendingPathComponent("taro-update.sh").path
            guard FileManager.default.fileExists(atPath: script) else {
                status("failed", detail: "taro-update.sh is not next to taro on the Mac")
                return
            }
            let p = Process()
            p.executableURL = URL(fileURLWithPath: "/bin/bash")
            p.arguments = [script, "--porcelain"] + (install ? [] : ["--check"])
            let out = Pipe(), err = Pipe()
            p.standardOutput = out
            p.standardError = err
            p.standardInput = FileHandle.nullDevice
            do { try p.run() } catch { status("failed", detail: "could not run taro-update.sh"); return }
            if install { status("installing", progress: 0, detail: "Starting") }
            var latest = "", done = "", last = -1, buf = Data()
            func line(_ l: String) {
                let w = l.split(separator: " ", maxSplits: 1).map(String.init)
                guard w.count == 2 else { return }
                switch w[0] {
                case "latest": latest = w[1]
                case "done": done = w[1]
                case "progress":
                    if let n = Int(w[1]), n != last, install { last = n; status("installing", latest: latest, progress: n, detail: "") }
                default: break
                }
            }
            while true {
                let d = out.fileHandleForReading.availableData
                if d.isEmpty { break }
                buf.append(d)
                while let nl = buf.firstIndex(of: 10) {
                    line(String(decoding: buf[buf.startIndex..<nl], as: UTF8.self))
                    buf.removeSubrange(buf.startIndex...nl)
                }
            }
            p.waitUntilExit()
            let why = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
                .split(separator: "\n").last.map { String($0).replacingOccurrences(of: "taro-update: ", with: "") } ?? "the update did not finish"
            if !install {
                switch p.terminationStatus {
                case 10: status("available", latest: latest)
                case 0: status("up_to_date", latest: latest)
                default: status("failed", detail: why)
                }
            } else if !done.isEmpty {
                status("done", latest: done, progress: 100)
            } else if p.terminationStatus == 0 {
                status("up_to_date", latest: latest)
            } else {
                status("failed", detail: why)
            }
        }
    }
}
