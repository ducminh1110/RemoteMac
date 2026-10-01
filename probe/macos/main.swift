// Feasibility-gate probe. Runs on a macOS GitHub Actions runner, started from a
// shell (NOT an .app). Emits one JSON document on stdout:
//   { "report": <CapabilityReport shape from rm-protocol>, "gates": [ {id,name,status,detail} ] }
// status: pass | fail | inconclusive. It never fakes a pass.
//
// NOTE: written without access to a Mac; the first real run on a runner is the
// test of this file itself. Fix compile errors there, then trust the results.

import Foundation
import CoreGraphics
import ApplicationServices
import VideoToolbox
import ScreenCaptureKit

struct Gate { let id: String; let name: String; var status: String; var detail: String }
var gates: [Gate] = []
func record(_ id: String, _ name: String, _ ok: Bool?, _ detail: String) {
    gates.append(Gate(id: id, name: name, status: ok == nil ? "inconclusive" : (ok! ? "pass" : "fail"), detail: detail))
    FileHandle.standardError.write(Data("[\(ok == nil ? "????" : (ok! ? "PASS" : "FAIL"))] \(id) \(name): \(detail)\n".utf8))
}

func sh(_ path: String, _ args: [String]) -> (Int32, String) {
    let p = Process(); p.executableURL = URL(fileURLWithPath: path); p.arguments = args
    let out = Pipe(); p.standardOutput = out; p.standardError = out
    do { try p.run() } catch { return (-1, "\(error)") }
    p.waitUntilExit()
    return (p.terminationStatus, String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? "")
}

/// Count distinct 32-bit pixel values (sampled) and whether any pixel is non-zero.
func imageStats(_ img: CGImage) -> (distinct: Int, hash: UInt64) {
    let w = img.width, h = img.height
    guard w > 0, h > 0 else { return (0, 0) }
    var buf = [UInt8](repeating: 0, count: w * h * 4)
    let cs = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(data: &buf, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: cs,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return (0, 0) }
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    var seen = Set<UInt32>(); var hash: UInt64 = 1469598103934665603
    var i = 0
    while i + 3 < buf.count {
        let px = UInt32(buf[i]) | UInt32(buf[i+1]) << 8 | UInt32(buf[i+2]) << 16 | UInt32(buf[i+3]) << 24
        if seen.count < 4096 { seen.insert(px) }
        hash = (hash ^ UInt64(px)) &* 1099511628211
        i += 4 * 7 // sample
    }
    return (seen.count, hash)
}

func windows(ofPid pid: pid_t) -> [[String: Any]] {
    let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []
    return list.filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }
}

func captureWindow(_ windowID: CGWindowID) async -> (CGImage?, String) {
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let w = content.windows.first(where: { $0.windowID == windowID }) else { return (nil, "window not in SCShareableContent") }
        let filter = SCContentFilter(desktopIndependentWindow: w)
        let cfg = SCStreamConfiguration()
        cfg.width = max(1, Int(w.frame.width)); cfg.height = max(1, Int(w.frame.height))
        let img = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: cfg)
        return (img, "ok \(img.width)x\(img.height)")
    } catch { return (nil, "\(error)") }
}

// ---- G0: environment ------------------------------------------------------
let osv = ProcessInfo.processInfo.operatingSystemVersionString
let arch = sh("/usr/bin/uname", ["-m"]).1.trimmingCharacters(in: .whitespacesAndNewlines)
record("G0", "environment", true, "macOS \(osv) \(arch) user=\(NSUserName()) tty=\(isatty(0) != 0)")

// ---- G1: GUI session ------------------------------------------------------
let sessionDict = CGSessionCopyCurrentDictionary() as? [String: Any]
let display = CGMainDisplayID()
let bounds = CGDisplayBounds(display)
let guiOK = sessionDict != nil && bounds.width > 0
record("G1", "GUI (WindowServer) session reachable from shell", guiOK,
       "session=\(sessionDict != nil) onConsole=\(String(describing: sessionDict?["kCGSSessionOnConsoleKey"])) display=\(Int(bounds.width))x\(Int(bounds.height))")

// ---- G2: permissions (TCC) ------------------------------------------------
let screenOK = CGPreflightScreenCaptureAccess()
let axOK = AXIsProcessTrusted()
record("G2a", "Screen Recording permission (preflight)", screenOK, "CGPreflightScreenCaptureAccess=\(screenOK)")
record("G2b", "Accessibility permission", axOK, "AXIsProcessTrusted=\(axOK)")

// ---- launch target as a plain executable ---------------------------------
let target = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
let docPath = NSTemporaryDirectory() + "rm-probe.txt"
FileManager.default.createFile(atPath: docPath, contents: Data("probe\n".utf8))
let proc = Process(); proc.executableURL = URL(fileURLWithPath: target); proc.arguments = [docPath]
var launched = false
do { try proc.run(); launched = true } catch { record("G3", "launch GUI app by executable path", false, "\(error)") }

var winID: CGWindowID? = nil
var capOK: Bool? = nil, capDetail = "not attempted"
var inputOK: Bool? = nil, inputDetail = "not attempted"
var hwOK: Bool? = nil, hwDetail = ""

if launched {
    let pid = proc.processIdentifier
    var found: [[String: Any]] = []
    for _ in 0..<40 { found = windows(ofPid: pid); if !found.isEmpty { break }; Thread.sleep(forTimeInterval: 0.5) }
    if let first = found.first, let n = first[kCGWindowNumber as String] as? UInt32 {
        winID = CGWindowID(n)
        record("G3", "launch GUI app by executable path + enumerate its windows", true,
               "pid=\(pid) windows=\(found.count) first=\(first[kCGWindowBounds as String] ?? "?")")
    } else {
        record("G3", "launch GUI app by executable path + enumerate its windows", false, "no layer-0 window for pid \(pid) after 20s")
    }

    // ---- G4: per-window capture ------------------------------------------
    if let id = winID {
        let (img, d) = await captureWindow(id)
        if let img = img {
            let s = imageStats(img)
            capOK = s.distinct > 8
            capDetail = "\(d) distinctPixels=\(s.distinct) (blank/black frames fail this)"
        } else { capOK = false; capDetail = d }
        record("G4", "capture ONE window via ScreenCaptureKit (not whole screen)", capOK, capDetail)

        // ---- G5: input injection + visible effect -------------------------
        let before = img.map { imageStats($0).hash }
        let src = CGEventSource(stateID: .hidSystemState)
        for ch: UInt16 in [4, 34, 40] { // h, i, k  (ANSI virtual keys)
            CGEvent(keyboardEventSource: src, virtualKey: ch, keyDown: true)?.postToPid(pid)
            CGEvent(keyboardEventSource: src, virtualKey: ch, keyDown: false)?.postToPid(pid)
        }
        Thread.sleep(forTimeInterval: 1.5)
        let (img2, d2) = await captureWindow(id)
        if let a = before, let i2 = img2 {
            let changed = a != imageStats(i2).hash
            inputOK = changed
            inputDetail = changed ? "window pixels changed after postToPid key events" : "no pixel change after key events (no permission, or not focused)"
        } else { inputOK = nil; inputDetail = "re-capture failed: \(d2)" }
        record("G5", "inject keyboard input into the app and observe effect in captured window", inputOK, inputDetail)
    } else {
        record("G4", "capture ONE window via ScreenCaptureKit", false, "skipped: no window")
        record("G5", "inject input and observe effect", false, "skipped: no window")
    }
    proc.terminate()
}

// ---- G6: encoder ----------------------------------------------------------
func tryEncoder(requireHW: Bool) -> OSStatus {
    var session: VTCompressionSession?
    let spec: CFDictionary = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: requireHW] as CFDictionary
    let st = VTCompressionSessionCreate(allocator: nil, width: 1280, height: 720, codecType: kCMVideoCodecType_H264,
                                        encoderSpecification: spec, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                        outputCallback: nil, refcon: nil, compressionSessionOut: &session)
    if let s = session { VTCompressionSessionInvalidate(s) }
    return st
}
let hw = tryEncoder(requireHW: true), sw = tryEncoder(requireHW: false)
hwOK = hw == noErr
hwDetail = hw == noErr ? "hardware H.264 encoder available" : (sw == noErr ? "only software H.264 (hw status \(hw))" : "no H.264 encoder (sw status \(sw))")
record("G6", "H.264 encoder (hardware preferred; software acceptable on VM)", sw == noErr, hwDetail)

// ---- G7: outbound network -------------------------------------------------
let relayURL = ProcessInfo.processInfo.environment["RM_PROBE_URL"] ?? "https://api.github.com/zen"
let (code, body) = sh("/usr/bin/curl", ["-sS", "-m", "10", "-o", "/dev/null", "-w", "%{http_code}", relayURL])
record("G7", "outbound HTTPS from runner", code == 0 && body.hasPrefix("2"), "\(relayURL) -> \(body.prefix(80))")

// ---- emit -----------------------------------------------------------------
func cap(_ ok: Bool?, _ yes: String, _ no: String) -> [String: String] {
    guard let ok = ok else { return ["state": "unknown", "reason": no] }
    return ok ? ["state": "available", "detail": yes] : ["state": "unavailable", "reason": no]
}
let report: [String: Any] = [
    "gui_session": cap(guiOK, "WindowServer reachable", "no GUI session"),
    "capture": cap(capOK, capDetail, capDetail),
    "input": cap(inputOK, inputDetail, inputDetail),
    "accessibility": cap(axOK, "trusted", "AXIsProcessTrusted=false"),
    "hardware_encode": cap(hwOK, hwDetail, hwDetail),
]
let out: [String: Any] = [
    "report": report,
    "gates": gates.map { ["id": $0.id, "name": $0.name, "status": $0.status, "detail": $0.detail] },
]
let data = try JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys])
print(String(data: data, encoding: .utf8)!)
