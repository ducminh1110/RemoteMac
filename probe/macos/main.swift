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
import AppKit

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

func pixels(_ img: CGImage) -> [UInt8] {
    let w = img.width, h = img.height
    var buf = [UInt8](repeating: 0, count: max(1, w * h * 4))
    guard w > 0, h > 0, let ctx = CGContext(data: &buf, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                            space: CGColorSpaceCreateDeviceRGB(),
                                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return buf }
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: w, height: h))
    return buf
}

/// Number of pixels that differ between two same-sized images (-1 if sizes differ).
func diffCount(_ a: CGImage, _ b: CGImage) -> Int {
    guard a.width == b.width, a.height == b.height else { return -1 }
    let pa = pixels(a), pb = pixels(b)
    var n = 0, i = 0
    while i + 3 < pa.count {
        if pa[i] != pb[i] || pa[i+1] != pb[i+1] || pa[i+2] != pb[i+2] { n += 1 }
        i += 4
    }
    return n
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

var notes: [String] = []
func note(_ s: String) {
    notes.append(s)
    FileHandle.standardError.write(Data("[NOTE] \(s)\n".utf8))
}

func axAttr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
func axStr(_ el: AXUIElement, _ name: String) -> String? { axAttr(el, name) as? String }

/// Text of the app's focused UI element, read through the Accessibility API.
func axFocused(_ pid: pid_t) -> (role: String, value: String)? {
    let app = AXUIElementCreateApplication(pid)
    guard let f = axAttr(app, kAXFocusedUIElementAttribute as String) else { return nil }
    let el = f as! AXUIElement
    return (axStr(el, kAXRoleAttribute as String) ?? "?", axStr(el, kAXValueAttribute as String) ?? "")
}

/// Human-readable inventory of what the app currently shows (CG windows + AX windows).
func describeApp(_ pid: pid_t, _ label: String) {
    let all = (CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? [])
        .filter { ($0[kCGWindowOwnerPID as String] as? Int32) == pid }
    for w in all {
        var r = "?"
        if let d = w[kCGWindowBounds as String] as? NSDictionary, let rect = CGRect(dictionaryRepresentation: d as CFDictionary) {
            r = "\(Int(rect.width))x\(Int(rect.height))@\(Int(rect.minX)),\(Int(rect.minY))"
        }
        note("\(label) CG window id=\(w[kCGWindowNumber as String] ?? "?") layer=\(w[kCGWindowLayer as String] ?? "?") onscreen=\(w[kCGWindowIsOnscreen as String] ?? "no") name=\"\(w[kCGWindowName as String] ?? "")\" \(r)")
    }
    let app = AXUIElementCreateApplication(pid)
    let wins = axAttr(app, kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    note("\(label) AX windows=\(wins.count)")
    for w in wins {
        note("\(label)   AX title=\"\(axStr(w, kAXTitleAttribute as String) ?? "")\" role=\(axStr(w, kAXRoleAttribute as String) ?? "?") subrole=\(axStr(w, kAXSubroleAttribute as String) ?? "?")")
    }
    if let f = axFocused(pid) { note("\(label) AX focused role=\(f.role) valueLen=\(f.value.count)") } else { note("\(label) AX focused element: none") }
}

func press(_ code: CGKeyCode, _ flags: CGEventFlags = [], pid: pid_t?) {
    let src = CGEventSource(stateID: .hidSystemState)
    for down in [true, false] {
        guard let e = CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: down) else { continue }
        e.flags = flags
        if let pid = pid { e.postToPid(pid) } else { e.post(tap: .cghidEventTap) }
        usleep(25_000)
    }
}

/// Type "hik hi" and decide whether it visibly reached the app. Two independent observations:
/// pixels changed beyond the no-input noise floor, or the focused AX text value changed.
func tryTyping(_ method: String, pid: pid_t, windowID: CGWindowID, viaHID: Bool) async -> (Bool, String) {
    let (a, _) = await captureWindow(windowID)
    Thread.sleep(forTimeInterval: 0.8)
    let (b, _) = await captureWindow(windowID)
    let noise = (a != nil && b != nil) ? diffCount(a!, b!) : -1
    let before = axFocused(pid)
    for k: CGKeyCode in [4, 34, 40, 49, 4, 34] { press(k, pid: viaHID ? nil : pid) }
    Thread.sleep(forTimeInterval: 1.5)
    let (c, d) = await captureWindow(windowID)
    let after = axFocused(pid)
    let px = (b != nil && c != nil) ? diffCount(b!, c!) : -1
    let pixelOK = px > max(50, noise * 3)
    let axOK = after != nil && (before?.value ?? "") != after!.value
    let detail = "method=\(method) pixelsChanged=\(px) noise=\(noise) pixelOK=\(pixelOK) axRole=\(after?.role ?? "none") axBeforeLen=\(before?.value.count ?? -1) axAfter=\"\(String((after?.value ?? "").suffix(24)))\" axChanged=\(axOK)" + (c == nil ? " recapture=\(d)" : "")
    return (pixelOK || axOK, detail)
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
    func area(_ w: [String: Any]) -> CGFloat {
        guard let d = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: d as CFDictionary) else { return 0 }
        return r.width >= 64 && r.height >= 64 ? r.width * r.height : 0 // ignore 0x0 / tiny placeholder windows
    }
    func largestWindow() -> [String: Any]? {
        windows(ofPid: pid).filter { area($0) > 0 }.sorted { area($0) > area($1) }.first
    }
    for _ in 0..<40 { if largestWindow() != nil { break }; Thread.sleep(forTimeInterval: 0.5) }
    Thread.sleep(forTimeInterval: 2.0) // let the app finish its launch UI
    describeApp(pid, "after-launch")

    // Make sure there is an untitled document: dismiss any Open panel, then Cmd+N.
    press(53, pid: pid)                       // Escape
    Thread.sleep(forTimeInterval: 1.0)
    press(45, .maskCommand, pid: pid)         // Cmd+N
    Thread.sleep(forTimeInterval: 2.0)
    describeApp(pid, "after-cmd-n")

    if let first = largestWindow(), let n = first[kCGWindowNumber as String] as? UInt32 {
        winID = CGWindowID(n)
        record("G3", "launch GUI app by executable path + enumerate its windows", true,
               "pid=\(pid) id=\(n) name=\"\(first[kCGWindowName as String] ?? "")\" bounds=\(first[kCGWindowBounds as String] ?? "?")")
    } else {
        record("G3", "launch GUI app by executable path + enumerate its windows", false, "no window >=64x64 for pid \(pid)")
    }

    if let id = winID {
        // ---- G4: per-window capture ----------------------------------------
        let (img, d) = await captureWindow(id)
        if let img = img {
            let s = imageStats(img)
            capOK = s.distinct > 8
            capDetail = "\(d) distinctPixels=\(s.distinct) (blank/black frames fail this)"
        } else { capOK = false; capDetail = d }
        record("G4", "capture ONE window via ScreenCaptureKit (not whole screen)", capOK, capDetail)

        // ---- G5: input injection, two delivery routes ------------------------
        let (okA, detA) = await tryTyping("postToPid", pid: pid, windowID: id, viaHID: false)
        record("G5a", "keyboard via CGEvent.postToPid (info)", okA, detA)
        NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
        Thread.sleep(forTimeInterval: 1.0)
        let (okB, detB) = await tryTyping("activate+cghidEventTap", pid: pid, windowID: id, viaHID: true)
        record("G5b", "keyboard via activate + HID event tap (info)", okB, detB)
        inputOK = okA || okB
        inputDetail = "postToPid=\(okA) hidTap=\(okB)"
        record("G5", "inject keyboard input into the app and observe effect (either route)", inputOK, inputDetail)
        describeApp(pid, "after-typing")
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
let relayURL = ProcessInfo.processInfo.environment["RM_PROBE_URL"] ?? "https://github.com/robots.txt"
let (code, body) = sh("/usr/bin/curl", ["-sS", "-m", "10", "-o", "/dev/null", "-w", "%{http_code}", relayURL])
// Any HTTP status (even 403/429 rate limiting) proves outbound HTTPS works; 000 = no connection.
let httpCode = String(body.suffix(3))
record("G7", "outbound HTTPS from runner", code == 0 && httpCode != "000", "\(relayURL) -> HTTP \(httpCode) (curl exit \(code))")

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
    "notes": notes,
]
let data = try JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys])
print(String(data: data, encoding: .utf8)!)
