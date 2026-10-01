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
import CoreMedia
import CoreVideo

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
        let kids = axAttr(w, kAXChildrenAttribute as String) as? [AXUIElement] ?? []
        for k in kids.prefix(25) {
            note("\(label)     child role=\(axStr(k, kAXRoleAttribute as String) ?? "?") title=\"\(axStr(k, kAXTitleAttribute as String) ?? "")\" desc=\"\(axStr(k, kAXDescriptionAttribute as String) ?? "")\" value=\"\(String((axStr(k, kAXValueAttribute as String) ?? "").prefix(40)))\"")
        }
    }
    if let f = axFocused(pid) { note("\(label) AX focused role=\(f.role) valueLen=\(f.value.count)") } else { note("\(label) AX focused element: none") }
}

/// Press the first dialog button whose title is in `titles` (AXPress). Returns whether one was pressed.
func axPress(_ pid: pid_t, titles: [String]) -> Bool {
    let app = AXUIElementCreateApplication(pid)
    let wins = axAttr(app, kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    for w in wins {
        let kids = axAttr(w, kAXChildrenAttribute as String) as? [AXUIElement] ?? []
        for k in kids where axStr(k, kAXRoleAttribute as String) == (kAXButtonRole as String)
            && titles.contains(axStr(k, kAXTitleAttribute as String) ?? "") {
            return AXUIElementPerformAction(k, kAXPressAction as CFString) == .success
        }
    }
    return false
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

// ---- exercise a GUI app launched by executable path -----------------------
struct Outcome { var cap: Bool?; var capDetail: String; var input: Bool?; var inputDetail: String }

/// Launch `path` from this shell, find its window, capture it, type into it.
/// Gate ids are `<prefix>3/4/5/5a/5b`. Prefix "G" = required gates, "T" = informational.
func exercise(prefix p: String, label: String, path: String, args: [String], dismissFirstRun: Bool) async -> Outcome {
    var out = Outcome(cap: nil, capDetail: "not attempted", input: nil, inputDetail: "not attempted")
    guard FileManager.default.isExecutableFile(atPath: path) else {
        record("\(p)3", "\(label): launch by executable path", false, "not executable: \(path)")
        return out
    }
    let proc = Process(); proc.executableURL = URL(fileURLWithPath: path); proc.arguments = args
    do { try proc.run() } catch { record("\(p)3", "\(label): launch by executable path", false, "\(error)"); return out }
    defer { proc.terminate() }
    let pid = proc.processIdentifier

    func area(_ w: [String: Any]) -> CGFloat {
        guard let d = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: d as CFDictionary) else { return 0 }
        return r.width >= 64 && r.height >= 64 ? r.width * r.height : 0
    }
    // Prefer windows that are actually on screen: apps also own large invisible backing windows.
    func isOnscreen(_ w: [String: Any]) -> Bool { (w[kCGWindowIsOnscreen as String] as? Bool) == true }
    func largest() -> [String: Any]? {
        let all = windows(ofPid: pid).filter { area($0) > 0 }
        let on = all.filter(isOnscreen)
        return (on.isEmpty ? all : on).sorted { area($0) > area($1) }.first
    }
    for _ in 0..<40 { if let w = largest(), isOnscreen(w) { break }; Thread.sleep(forTimeInterval: 0.5) }
    Thread.sleep(forTimeInterval: 2.0)
    describeApp(pid, "\(label)/launch")

    if dismissFirstRun {
        let pressed = axPress(pid, titles: ["OK", "Cancel", "Close"])         // dismiss an alert if one is up
        note("\(label) dismissed alert via AXPress: \(pressed)")
        Thread.sleep(forTimeInterval: 1.0)
        press(53, pid: pid); Thread.sleep(forTimeInterval: 1.0)              // Escape (closes an Open panel)
        press(45, .maskCommand, pid: pid); Thread.sleep(forTimeInterval: 2.0) // Cmd+N
        describeApp(pid, "\(label)/after-esc-cmdn")
    }

    guard let first = largest(), let n = first[kCGWindowNumber as String] as? UInt32 else {
        record("\(p)3", "\(label): launch by executable path + window", false, "no window >=64x64 for pid \(pid)")
        return out
    }
    let id = CGWindowID(n)
    record("\(p)3", "\(label): launch by executable path + window", true,
           "pid=\(pid) id=\(n) name=\"\(first[kCGWindowName as String] ?? "")\" bounds=\(first[kCGWindowBounds as String] ?? "?")")

    let (img, d) = await captureWindow(id)
    if let img = img {
        let st = imageStats(img)
        out.cap = st.distinct > 8
        out.capDetail = "\(d) distinctPixels=\(st.distinct)"
    } else { out.cap = false; out.capDetail = d }
    record("\(p)4", "\(label): capture ONE window via ScreenCaptureKit", out.cap, out.capDetail)

    let (okA, detA) = await tryTyping("postToPid", pid: pid, windowID: id, viaHID: false)
    record("\(p)5a", "\(label): keyboard via CGEvent.postToPid", okA, detA)
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
    Thread.sleep(forTimeInterval: 1.0)
    let (okB, detB) = await tryTyping("activate+cghidEventTap", pid: pid, windowID: id, viaHID: true)
    record("\(p)5b", "\(label): keyboard via activate + HID event tap", okB, detB)
    out.input = okA || okB
    out.inputDetail = "postToPid=\(okA) hidTap=\(okB)"
    record("\(p)5", "\(label): keyboard input has a visible effect (either route)", out.input, out.inputDetail)
    describeApp(pid, "\(label)/after-typing")
    return out
}

let cwd = FileManager.default.currentDirectoryPath
let testApp = ProcessInfo.processInfo.environment["RM_TESTAPP"] ?? cwd + "/out/rm-testapp"
let primary = await exercise(prefix: "G", label: "testapp", path: testApp, args: [], dismissFirstRun: false)
let capOK = primary.cap, capDetail = primary.capDetail
let inputOK = primary.input, inputDetail = primary.inputDetail

// Real-world target, informational: TextEdit on a fresh runner may sit behind a first-run dialog.
// (No file argument: passing a temp-file path made TextEdit raise "document could not be opened".)
_ = await exercise(prefix: "T", label: "textedit", path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit", args: [], dismissFirstRun: true)


// =====================================================================
// M1 gates: continuous capture, encode+decode, mouse/unicode, resize/close
// =====================================================================
func rectOf(_ w: [String: Any]) -> CGRect {
    guard let d = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: d as CFDictionary) else { return .zero }
    return r
}
func onscreenWindow(_ pid: pid_t) -> [String: Any]? {
    windows(ofPid: pid).filter { rectOf($0).width >= 64 && rectOf($0).height >= 64 && ($0[kCGWindowIsOnscreen as String] as? Bool) == true }
        .sorted { rectOf($0).width * rectOf($0).height > rectOf($1).width * rectOf($1).height }.first
}
func axWindow(_ pid: pid_t) -> AXUIElement? {
    (axAttr(AXUIElementCreateApplication(pid), kAXWindowsAttribute as String) as? [AXUIElement])?.first
}
func percentile(_ v: [Double], _ p: Double) -> Double {
    guard !v.isEmpty else { return 0 }
    let s = v.sorted(); return s[min(s.count - 1, Int(Double(s.count - 1) * p))]
}

/// Receives ScreenCaptureKit frames, feeds each into a VideoToolbox H.264 encoder.
final class Recorder: NSObject, SCStreamOutput {
    let lock = NSLock()
    var frames = 0, dropped = 0, encBytes = 0, keyframes = 0, encErrors = 0
    var arrival: [Double] = [], encLatencyMs: [Double] = []
    var encoded: [CMSampleBuffer] = []
    var session: VTCompressionSession?
    var encW = 0, encH = 0

    func setup(_ w: Int, _ h: Int) {
        var s: VTCompressionSession?
        let st = VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
                                            encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                            outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard st == noErr, let s = s else { encErrors += 1; return }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Main_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: 8_000_000 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, value: 2 as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
        session = s; encW = w; encH = h
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sb.isValid,
              let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = atts.first?[.status] as? Int, raw == SCFrameStatus.complete.rawValue,
              let pb = CMSampleBufferGetImageBuffer(sb) else { return }
        lock.lock(); frames += 1; arrival.append(CFAbsoluteTimeGetCurrent()); lock.unlock()
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        if session == nil { setup(w, h) }
        guard let s = session, w == encW, h == encH else { lock.lock(); dropped += 1; lock.unlock(); return }
        let t0 = CFAbsoluteTimeGetCurrent()
        let st = VTCompressionSessionEncodeFrame(s, imageBuffer: pb, presentationTimeStamp: CMSampleBufferGetPresentationTimeStamp(sb),
                                                 duration: .invalid, frameProperties: nil, infoFlagsOut: nil) { [self] status, _, out in
            lock.lock(); defer { lock.unlock() }
            guard status == noErr, let out = out, CMSampleBufferDataIsReady(out) else { encErrors += 1; return }
            encLatencyMs.append((CFAbsoluteTimeGetCurrent() - t0) * 1000)
            encBytes += CMSampleBufferGetTotalSampleSize(out)
            let a = CMSampleBufferGetSampleAttachmentsArray(out, createIfNecessary: false) as? [[CFString: Any]]
            if (a?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true { keyframes += 1 }
            encoded.append(out)
        }
        if st != noErr { lock.lock(); encErrors += 1; lock.unlock() }
    }
    func finish() { if let s = session { VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid); VTCompressionSessionInvalidate(s) } }
}

func decodeAll(_ samples: [CMSampleBuffer]) -> (ok: Int, w: Int, h: Int, err: String) {
    guard let first = samples.first, let fd = CMSampleBufferGetFormatDescription(first) else { return (0, 0, 0, "no samples") }
    var dsess: VTDecompressionSession?
    let attrs: CFDictionary = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA] as CFDictionary
    let st = VTDecompressionSessionCreate(allocator: nil, formatDescription: fd, decoderSpecification: nil, imageBufferAttributes: attrs,
                                          outputCallback: nil, decompressionSessionOut: &dsess)
    guard st == noErr, let s = dsess else { return (0, 0, 0, "decoder create status \(st)") }
    let lock = NSLock(); var ok = 0, dw = 0, dh = 0
    for sb in samples {
        VTDecompressionSessionDecodeFrame(s, sampleBuffer: sb, flags: [], infoFlagsOut: nil) { status, _, img, _, _ in
            guard status == noErr, let img = img else { return }
            lock.lock(); ok += 1; dw = CVPixelBufferGetWidth(img); dh = CVPixelBufferGetHeight(img); lock.unlock()
        }
    }
    VTDecompressionSessionWaitForAsynchronousFrames(s)
    VTDecompressionSessionInvalidate(s)
    return (ok, dw, dh, "")
}

func launchTestApp() -> (Process, pid_t)? {
    let proc = Process(); proc.executableURL = URL(fileURLWithPath: testApp)
    do { try proc.run() } catch { return nil }
    return (proc, proc.processIdentifier)
}

func postMouse(_ type: CGEventType, _ p: CGPoint, pid: pid_t?) {
    guard let e = CGEvent(mouseEventSource: CGEventSource(stateID: .hidSystemState), mouseType: type, mouseCursorPosition: p, mouseButton: .left) else { return }
    e.setIntegerValueField(.mouseEventClickState, value: 1)
    if let pid = pid { e.postToPid(pid) } else { e.post(tap: .cghidEventTap) }
}
func click(_ p: CGPoint, pid: pid_t?) {
    postMouse(.mouseMoved, p, pid: pid); usleep(30_000)
    postMouse(.leftMouseDown, p, pid: pid); usleep(40_000)
    postMouse(.leftMouseUp, p, pid: pid); usleep(40_000)
}
func typeUnicode(_ s: String, pid: pid_t?) {
    for ch in s {
        let units = Array(String(ch).utf16)
        for down in [true, false] {
            guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 0, keyDown: down) else { continue }
            e.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
            if let pid = pid { e.postToPid(pid) } else { e.post(tap: .cghidEventTap) }
            usleep(20_000)
        }
    }
}

func runM1() async {
    // ---- M1a + M1b: continuous capture while the app animates, encode every frame, decode them back
    guard let (proc, pid) = launchTestApp() else { for id in ["M1a", "M1b", "M1c", "M1d"] { record(id, "m1", false, "testapp launch failed") }; return }
    defer { proc.terminate() }
    for _ in 0..<40 { if onscreenWindow(pid) != nil { break }; Thread.sleep(forTimeInterval: 0.5) }
    Thread.sleep(forTimeInterval: 1.5)
    guard let win = onscreenWindow(pid), let wn = win[kCGWindowNumber as String] as? UInt32 else {
        for id in ["M1a", "M1b", "M1c", "M1d"] { record(id, "m1", false, "no window") }; return
    }
    NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
    let seconds = 5.0
    let rec = Recorder()
    var streamErr = ""
    do {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let w = content.windows.first(where: { $0.windowID == CGWindowID(wn) }) else { throw NSError(domain: "rm", code: 1, userInfo: [NSLocalizedDescriptionKey: "window not shareable"]) }
        let cfg = SCStreamConfiguration()
        cfg.width = Int(w.frame.width); cfg.height = Int(w.frame.height)
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        cfg.pixelFormat = kCVPixelFormatType_32BGRA
        cfg.queueDepth = 6; cfg.showsCursor = false
        let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: w), configuration: cfg, delegate: nil)
        try stream.addStreamOutput(rec, type: .screen, sampleHandlerQueue: DispatchQueue(label: "rm.frames"))
        try await stream.startCapture()
        try await Task.sleep(nanoseconds: UInt64(seconds * 1e9))
        try await stream.stopCapture()
    } catch { streamErr = "\(error)" }
    rec.finish()
    rec.lock.lock()
    let gaps = zip(rec.arrival.dropFirst(), rec.arrival).map { ($0 - $1) * 1000 }
    let fps = Double(rec.frames) / seconds
    let a = "frames=\(rec.frames) fps=\(String(format: "%.1f", fps)) gapP50=\(String(format: "%.1f", percentile(gaps, 0.5)))ms gapP95=\(String(format: "%.1f", percentile(gaps, 0.95)))ms gapMax=\(String(format: "%.0f", gaps.max() ?? 0))ms size=\(rec.encW)x\(rec.encH)"
    let encodedCopy = rec.encoded
    let b1 = "encoded=\(encodedCopy.count) keyframes=\(rec.keyframes) errors=\(rec.encErrors) dropped=\(rec.dropped) bitrate=\(String(format: "%.2f", Double(rec.encBytes) * 8 / seconds / 1e6))Mbps encLatP50=\(String(format: "%.1f", percentile(rec.encLatencyMs, 0.5)))ms encLatP95=\(String(format: "%.1f", percentile(rec.encLatencyMs, 0.95)))ms"
    rec.lock.unlock()
    record("M1a", "continuous SCStream of one window (>=20 fps while animating)", streamErr.isEmpty && fps >= 20, streamErr.isEmpty ? a : streamErr)
    let dec = decodeAll(encodedCopy)
    record("M1b", "H.264 encode every captured frame, decode them back", !encodedCopy.isEmpty && dec.ok >= encodedCopy.count * 9 / 10 && dec.w == rec.encW && dec.h == rec.encH,
           b1 + " decoded=\(dec.ok) decodedSize=\(dec.w)x\(dec.h) \(dec.err)")

    // ---- M1c: unicode text + mouse click that moves the caret
    var routeUsed = "none", c_ok = false, c_detail = ""
    for (name, target) in [("postToPid", Optional(pid)), ("hidTap", Optional<pid_t>.none)] {
        guard let w = onscreenWindow(pid) else { break }
        let r = rectOf(w)
        // reset the field through Cmd+A, Delete so each route starts clean
        press(0, .maskCommand, pid: target); usleep(100_000); press(51, pid: target); Thread.sleep(forTimeInterval: 0.3)
        let text = "alpha beta \u{e9}\u{4e2d}"
        typeUnicode(text, pid: target); Thread.sleep(forTimeInterval: 0.6)
        let typed = axFocused(pid)?.value ?? "nil"
        click(CGPoint(x: r.minX + 3, y: r.minY + 40), pid: target); Thread.sleep(forTimeInterval: 0.4)
        typeUnicode("X", pid: target); Thread.sleep(forTimeInterval: 0.6)
        let after = axFocused(pid)?.value ?? "nil"
        let unicodeOK = typed == text
        let caretMoved = after.contains("X") && !after.hasSuffix("X")
        c_detail += "[\(name) typed=\"\(typed)\" unicodeOK=\(unicodeOK) afterClick=\"\(after)\" caretMoved=\(caretMoved)] "
        if unicodeOK && caretMoved { c_ok = true; routeUsed = name; break }
        NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps]); Thread.sleep(forTimeInterval: 0.5)
    }
    record("M1c", "unicode text input + mouse click (caret moved)", c_ok, "route=\(routeUsed) " + c_detail)

    // ---- M1d: resize, move, close through the window server / AX
    guard let aw = axWindow(pid) else { record("M1d", "resize/move/close", false, "no AX window"); return }
    var size = CGSize(width: 640, height: 440), pos = CGPoint(x: 120, y: 120)
    let rs = AXUIElementSetAttributeValue(aw, kAXSizeAttribute as CFString, AXValueCreate(.cgSize, &size)!)
    let ps = AXUIElementSetAttributeValue(aw, kAXPositionAttribute as CFString, AXValueCreate(.cgPoint, &pos)!)
    var moved = CGRect.zero
    for _ in 0..<20 { Thread.sleep(forTimeInterval: 0.2); if let w = onscreenWindow(pid) { moved = rectOf(w); if abs(moved.width - 640) < 4 && abs(moved.height - 440) < 4 { break } } }
    let (img, _) = await captureWindow(CGWindowID(wn))
    let resizedOK = abs(moved.width - 640) < 4 && abs(moved.height - 440) < 4
    let movedOK = abs(moved.minX - 120) < 4 && abs(moved.minY - 120) < 4
    let captureFollows = img != nil && abs(img!.width - 640) <= 4
    var closed = false, closeStatus = "no close button"
    if let cb = axAttr(aw, kAXCloseButtonAttribute as String) {
        closeStatus = "\(AXUIElementPerformAction(cb as! AXUIElement, kAXPressAction as CFString).rawValue)"
        for _ in 0..<20 { Thread.sleep(forTimeInterval: 0.2); if onscreenWindow(pid) == nil { closed = true; break } }
    }
    record("M1d", "resize + move + close window via Accessibility", resizedOK && movedOK && captureFollows && closed,
           "setSize=\(rs.rawValue) setPos=\(ps.rawValue) now=\(Int(moved.width))x\(Int(moved.height))@\(Int(moved.minX)),\(Int(moved.minY)) captureFollows=\(captureFollows) closePress=\(closeStatus) closed=\(closed)")
}
await runM1()

var hwOK: Bool? = nil, hwDetail = ""

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
