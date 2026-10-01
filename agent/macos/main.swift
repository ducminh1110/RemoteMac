// remote-agent-mac: terminal-launched macOS agent. Speaks the rm-protocol over a relay connection.
//   RM_SESSION_TOKEN=... remote-agent-mac --relay HOST:PORT --session ID
import Foundation
import AppKit
import ApplicationServices
import VideoToolbox

func log(_ s: String) { FileHandle.standardError.write(Data("[agent] \(s)\n".utf8)) }
func fail(_ s: String) -> Never { log(s); exit(1) }

var relayAddr: String?, sessionID: String?
var argv = CommandLine.arguments.dropFirst().makeIterator()
while let a = argv.next() {
    switch a {
    case "--relay": relayAddr = argv.next()
    case "--session": sessionID = argv.next()
    default: fail("usage: RM_SESSION_TOKEN=.. remote-agent-mac --relay HOST:PORT --session ID")
    }
}
guard let relayAddr = relayAddr, let sessionID = sessionID, let token = ProcessInfo.processInfo.environment["RM_SESSION_TOKEN"] else {
    fail("usage: RM_SESSION_TOKEN=.. remote-agent-mac --relay HOST:PORT --session ID")
}

// ---- capability probe (runtime, never assumed) ------------------------------------------------
func cap(_ ok: Bool, _ yes: String, _ no: String) -> [String: Any] {
    ok ? ["state": "available", "detail": yes] : ["state": "unavailable", "reason": no]
}
func probeCapabilities() -> [String: Any] {
    let gui = CGSessionCopyCurrentDictionary() != nil
    let screen = CGPreflightScreenCaptureAccess()
    let ax = AXIsProcessTrusted()
    var s: VTCompressionSession?
    let hw = VTCompressionSessionCreate(allocator: nil, width: 1280, height: 720, codecType: kCMVideoCodecType_H264,
        encoderSpecification: [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true] as CFDictionary,
        imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s) == noErr
    if let s = s { VTCompressionSessionInvalidate(s) }
    return ["type": "capability_report",
            "gui_session": cap(gui, "WindowServer reachable", "no GUI session"),
            "capture": cap(screen, "Screen Recording granted", "CGPreflightScreenCaptureAccess=false"),
            "input": cap(ax, "Accessibility granted (event injection allowed)", "AXIsProcessTrusted=false"),
            "accessibility": cap(ax, "trusted", "AXIsProcessTrusted=false"),
            "hardware_encode": cap(hw, "hardware H.264", "software H.264 only")]
}

// ---- connect + handshake ------------------------------------------------------------------------
let conn: Conn
do { conn = try Conn.connect(hostPort: relayAddr); try joinRelay(conn, session: sessionID, token: token) } catch { fail("\(error)") }
log("relay joined, session=\(sessionID) (token not logged)")

func readJSON() throws -> [String: Any]? {
    guard let (_, payload) = try conn.readFrame() else { return nil }
    return try JSONSerialization.jsonObject(with: payload) as? [String: Any]
}

do {
    guard let hello = try readJSON(), hello["type"] as? String == "client_hello" else { fail("expected client_hello") }
    let cmin = int(hello["min_version"]), cmax = int(hello["max_version"])
    guard cmin <= 1 && cmax >= 1 else {
        try conn.send(["type": "error", "code": "version_mismatch", "message": "agent speaks protocol 1, client \(cmin)...\(cmax)"]); exit(1)
    }
    try conn.send(["type": "server_hello", "min_version": 1, "max_version": 1, "codecs": ["h264"], "features": ["control", "video"],
                   "max_surface": [3840, 2160], "agent": "remote-agent-mac 0.2 \(ProcessInfo.processInfo.operatingSystemVersionString)"])
    try conn.send(probeCapabilities())
} catch { fail("handshake: \(error)") }
log("handshake complete")

// ---- runtime -------------------------------------------------------------------------------------
let apps = AppManager()
let tracker = WindowTracker(apps: apps)
let injector = InputInjector(tracker: tracker)
let streamsLock = NSLock()
var streams: [CGWindowID: WindowStream] = [:]
var lastSize: [CGWindowID: CGSize] = [:]

func axAttr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
/// The AX window of `pid` whose frame matches the window-server rect (apps may own several windows).
func axWindowFor(pid: pid_t, id: CGWindowID, rect: CGRect) -> AXUIElement? {
    let wins = axAttr(AXUIElementCreateApplication(pid), kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    for w in wins {
        var p = CGPoint.zero, s = CGSize.zero
        if let pv = axAttr(w, kAXPositionAttribute as String), let sv = axAttr(w, kAXSizeAttribute as String),
           AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &s),
           abs(p.x - rect.minX) < 3, abs(p.y - rect.minY) < 3, abs(s.width - rect.width) < 3, abs(s.height - rect.height) < 3 { return w }
    }
    return wins.first
}

func send(_ m: [String: Any]) { do { try conn.send(m) } catch { log("send failed: \(error)") } }

func startStream(_ id: CGWindowID) {
    let ws = WindowStream(windowID: id) { pkt in do { try conn.sendVideo(pkt) } catch { log("video send failed: \(error)") } }
    streamsLock.lock(); streams[id] = ws; streamsLock.unlock()
    Task { do { try await ws.start(); log("stream started window=\(id)") } catch { log("stream start failed window=\(id): \(error)")
        send(["type": "capability_unavailable", "capability": "capture", "reason": "\(error)"]) } }
}
func stopStream(_ id: CGWindowID) {
    streamsLock.lock(); let ws = streams.removeValue(forKey: id); streamsLock.unlock()
    if let ws = ws { Task { await ws.stop(); log("stream stopped window=\(id) packets=\(ws.sent)") } }
}

tracker.onCreated = { w, appID in
    log("window created id=\(w.id) app=\(appID) \(Int(w.rect.width))x\(Int(w.rect.height))")
    lastSize[w.id] = w.rect.size
    send(["type": "window_created", "window_id": Int(w.id), "application_id": appID, "title": w.title, "bounds": rectJSON(w.rect), "parent_id": NSNull()])
    startStream(w.id)
}
tracker.onDestroyed = { id in
    log("window destroyed id=\(id)")
    lastSize.removeValue(forKey: id)
    stopStream(id)
    send(["type": "window_destroyed", "window_id": Int(id)])
}
tracker.onMoved = { w in
    send(["type": "window_moved", "window_id": Int(w.id), "bounds": rectJSON(w.rect)])
    if lastSize[w.id] != w.rect.size {            // size changed: the encoder is bound to a size, so restart the stream
        lastSize[w.id] = w.rect.size
        stopStream(w.id); startStream(w.id)
    }
}
tracker.onTitle = { w in send(["type": "window_title_changed", "window_id": Int(w.id), "title": w.title]) }
tracker.onAppExited = { id, code in
    log("app exited on its own id=\(id) code=\(code)")
    send(["type": "app_exited", "application_id": id, "code": Int(code)])
}
tracker.start()

let inputTypes: Set<String> = ["mouse_move", "mouse_button", "scroll", "key", "text_input"]

func handle(_ m: [String: Any]) {
    let type = m["type"] as? String ?? ""
    switch type {
    case "list_apps":
        send(["type": "apps", "apps": apps.list()])
    case "app_launch":
        let id = m["application_id"] as? String ?? ""
        let r = apps.launch(id: id, args: m["arguments"] as? [String] ?? [])
        if let pid = r.pid { log("launched \(id) pid=\(pid)"); send(["type": "app_launched", "application_id": id, "pid": Int(pid)]) }
        else if let e = r.err { send(["type": "error", "code": e.0, "message": e.1]) }
    case "app_terminate":
        let id = m["application_id"] as? String ?? ""
        if apps.terminate(id: id) { send(["type": "app_exited", "application_id": id, "code": NSNull()]) }
        else { send(["type": "error", "code": "not_running", "message": id]) }
    case "window_close", "window_resize_request":
        let wid = CGWindowID(int(m["window_id"]))
        guard let w = tracker.current(wid), let aw = axWindowFor(pid: w.pid, id: wid, rect: w.rect) else {
            send(["type": "error", "code": "no_such_window", "message": "\(wid)"]); break
        }
        if type == "window_close" {
            if let cb = axAttr(aw, kAXCloseButtonAttribute as String) { AXUIElementPerformAction(cb as! AXUIElement, kAXPressAction as CFString) }
        } else {
            var size = CGSize(width: num(m["width"]), height: num(m["height"]))
            if let v = AXValueCreate(.cgSize, &size) { AXUIElementSetAttributeValue(aw, kAXSizeAttribute as CFString, v) }
        }
    case "ping":
        send(["type": "pong", "nonce": m["nonce"] ?? 0])
    case _ where inputTypes.contains(type):
        if let err = injector.handle(m) { send(["type": "error", "code": "input_failed", "message": err]) }
    default:
        send(["type": "error", "code": "unexpected", "message": "message '\(type)' not valid for agent"])
    }
}

let reader = Thread {
    do {
        while let (ch, payload) = try conn.readFrame() {
            guard ch != .video, let m = try? JSONSerialization.jsonObject(with: payload) as? [String: Any] else { continue }
            handle(m)
        }
        log("client disconnected")
    } catch { log("read loop ended: \(error)") }
    apps.terminateAll()
    exit(0)
}
reader.stackSize = 4 << 20
reader.start()
dispatchMain()
