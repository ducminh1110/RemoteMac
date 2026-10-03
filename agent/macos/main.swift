// remotemac (remote-agent-mac): terminal-launched macOS agent. Speaks the rm-protocol over a relay.
//   ./remotemac --password SECRET            -> shows "ID session to connect" + password
//   ./remotemac                              -> same, with a random password
//   RM_SESSION_TOKEN=... ./remotemac --relay HOST:PORT --session NAME   (scripts, CI)
import Foundation
import AppKit
import ApplicationServices
import VideoToolbox

func log(_ s: String) { FileHandle.standardError.write(Data("[agent] \(s)\n".utf8)) }
func fail(_ s: String) -> Never { log(s); exit(1) }

let usage = "usage: remotemac [--password SECRET] [--id 123456789] [--relay HOST:PORT]\n       RM_SESSION_TOKEN=.. remotemac --relay HOST:PORT --session NAME"
var relayArg: String?, sessionArg: String?, passwordArg: String?, idArg: String?
var argv = CommandLine.arguments.dropFirst().makeIterator()
while let a = argv.next() {
    switch a {
    case "--relay": relayArg = argv.next()
    case "--session": sessionArg = argv.next()
    case "--password": passwordArg = argv.next()
    case "--id": idArg = argv.next()?.filter(\.isNumber)
    case "-h", "--help": print(usage); exit(0)
    default: fail(usage)
    }
}
let env = ProcessInfo.processInfo.environment
let relayAddr = relayArg ?? env["RM_RELAY"] ?? defaultRelay
let sessionID: String, token: String
if let s = sessionArg {
    guard let t = env["RM_SESSION_TOKEN"] else { fail(usage) }
    sessionID = s; token = t
} else {
    // ID + password mode; a restart for the next client keeps both (RM_PASSWORD, RM_ID)
    let id = idArg ?? env["RM_ID"] ?? persistentID()
    guard id.count == 9 else { fail("the ID must be 9 digits") }
    let password = passwordArg ?? env["RM_PASSWORD"] ?? randomPassword()
    guard password.count >= 4 else { fail("the password must have at least 4 characters") }
    setenv("RM_ID", id, 1); setenv("RM_PASSWORD", password, 1)
    sessionID = relaySession(id: id); token = sessionToken(id: id, password: password)
    if env["RM_QUIET_BANNER"] == nil {
        print("")
        print("  RemoteMac is ready — connect from Windows with:")
        print("    ID session to connect: \(displayID(id))")
        print("    Password: \(password)")
        print("  (relay \(relayAddr); Ctrl+C to stop)")
        print("")
        fflush(stdout)
        setenv("RM_QUIET_BANNER", "1", 1) // printed once, not again after each session
    }
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
do { conn = try Conn.connect(hostPort: relayAddr) } catch {
    log("relay \(relayAddr) not reachable (\(error)); retrying in 5 s"); restartForNextClient(after: 5)
}
log("relay joined, session=\(sessionID) (token not logged); waiting for a client")
do { try joinRelay(conn, session: sessionID, token: token) } catch {
    // nobody came within the relay's wait (or the relay refused): wait again
    log("\(error); waiting again"); restartForNextClient(after: 2)
}
log("client connected")

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
let sender = Sender(conn: conn)
log("fec self-test \(fecSelfTest() ? "ok" : "FAILED")")

// ---- runtime -------------------------------------------------------------------------------------
let apps = AppManager()
let tracker = WindowTracker(apps: apps)
let desktop = DesktopSession()
let injector = InputInjector(tracker: tracker, desktop: desktop)
let streamsLock = NSLock()
var streams: [CGWindowID: WindowStream] = [:]
var lastSize: [CGWindowID: CGSize] = [:]

func axAttr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
}
/// The AX window of `pid` whose frame matches the window-server rect (apps may own several windows).
@_silgen_name("_AXUIElementGetWindow")
func _AXUIElementGetWindow(_ element: AXUIElement, _ id: UnsafeMutablePointer<CGWindowID>) -> AXError

/// The AX window that *is* window `id` (by its window-server id), else the one with its frame.
func axWindowFor(pid: pid_t, id: CGWindowID, rect: CGRect) -> AXUIElement? {
    let wins = axAttr(AXUIElementCreateApplication(pid), kAXWindowsAttribute as String) as? [AXUIElement] ?? []
    for w in wins {
        var wid: CGWindowID = 0
        if _AXUIElementGetWindow(w, &wid) == .success && wid == id { return w }
    }
    for w in wins {
        var p = CGPoint.zero, s = CGSize.zero
        if let pv = axAttr(w, kAXPositionAttribute as String), let sv = axAttr(w, kAXSizeAttribute as String),
           AXValueGetValue(pv as! AXValue, .cgPoint, &p), AXValueGetValue(sv as! AXValue, .cgSize, &s),
           abs(p.x - rect.minX) < 3, abs(p.y - rect.minY) < 3, abs(s.width - rect.width) < 3, abs(s.height - rect.height) < 3 { return w }
    }
    return nil // never guess: acting on another window is worse than reporting an error
}

func send(_ m: [String: Any]) { do { try sender.send(m) } catch { log("send failed: \(error)") } }
sender.requestKeyframe = { wid in streamsLock.lock(); let ws = streams[CGWindowID(wid)]; streamsLock.unlock(); ws?.requestKeyframe() }
sender.onBitrate = { b in
    log("bitrate -> \(b / 1000) kbit/s (dropped frames so far: \(sender.dropped + (sender.udp?.dropped ?? 0)))")
    streamsLock.lock(); let all = Array(streams.values); streamsLock.unlock()
    for ws in all { ws.setBitrate(b) }
}
// video over UDP + FEC beside the TCP connection (RM_NO_UDP=1: TCP only)
if env["RM_NO_UDP"] == nil, let u = UdpLink(hostPort: relayAddr, session: sessionID, token: token, key: relayKey()) {
    sender.udp = u
    u.requestKeyframe = { wid in sender.requestKeyframe?(wid) }
    u.onAlive = { up in
        log("UDP video \(up ? "on" : "off (TCP)")")
        // the client resynchronises on a keyframe after a transport change
        streamsLock.lock(); let all = Array(streams.values); streamsLock.unlock()
        for ws in all { ws.requestKeyframe() }
    }
    var reports = 0
    u.onReport = { r in
        sender.udpReport(r, wait: u.takeMaxWait())
        reports += 1
        if reports % 25 == 0 {
            log(String(format: "UDP report: loss %.1f%% recovered %d lost %d fec %d%% frames sent %d", r.loss * 100, r.recovered, r.lost, u.fecPct, u.sentFrames))
        }
    }
}

let menuQueue = DispatchQueue(label: "rm.menus")
let iconQueue = DispatchQueue(label: "rm.icons", qos: .utility)
/// Read (off the main path: big apps take a moment) and send an app's menu bar.
func sendMenuBar(_ id: String) {
    menuQueue.async {
        guard let pid = apps.pidFor(id) else { return }
        send(["type": "menu_bar", "application_id": id, "menus": readMenuBar(pid: pid)])
    }
}

func startStream(_ id: CGWindowID, inset: CGFloat) {
    let ws = WindowStream(windowID: id, inset: inset) { pkt in sender.sendVideo(pkt) }
    ws.setBitrate(sender.bitrate)
    streamsLock.lock(); streams[id] = ws; streamsLock.unlock()
    Task { do { try await ws.start(); log("stream started window=\(id)") } catch { log("stream start failed window=\(id): \(error)")
        send(["type": "capability_unavailable", "capability": "capture", "reason": "\(error)"]) } }
}
func stopStream(_ id: CGWindowID) {
    streamsLock.lock(); let ws = streams.removeValue(forKey: id); streamsLock.unlock()
    if let ws = ws { Task { await ws.stop(); log("stream stopped window=\(id) packets=\(ws.sent)") } }
}

tracker.onCreated = { w in
    log("window created id=\(w.id) app=\(w.appID) role=\(w.role.rawValue) parent=\(w.parent.map { String($0) } ?? "-") \(Int(w.rect.width))x\(Int(w.rect.height)) titleBarCut=\(Int(w.inset))")
    lastSize[w.id] = w.rect.size
    send(["type": "window_created", "window_id": Int(w.id), "application_id": w.appID, "title": w.title, "bounds": rectJSON(w.content),
          "parent_id": w.parent.map { Int($0) as Any } ?? NSNull(), "role": w.role.rawValue])
    startStream(w.id, inset: w.inset)
    if w.role == .window { DispatchQueue.global().asyncAfter(deadline: .now() + 0.6) { sendMenuBar(w.appID) } }
}
tracker.onDestroyed = { id in
    log("window destroyed id=\(id)")
    lastSize.removeValue(forKey: id)
    stopStream(id)
    send(["type": "window_destroyed", "window_id": Int(id)])
}
tracker.onMoved = { w in
    send(["type": "window_moved", "window_id": Int(w.id), "bounds": rectJSON(w.content)])
    if lastSize[w.id] != w.rect.size {            // size changed: the encoder is bound to a size, so restart the stream
        lastSize[w.id] = w.rect.size
        stopStream(w.id); startStream(w.id, inset: w.inset)
    }
}
tracker.onTitle = { w in send(["type": "window_title_changed", "window_id": Int(w.id), "title": w.title]) }
tracker.onAppExited = { id, code in
    log("app exited on its own id=\(id) code=\(code)")
    send(["type": "app_exited", "application_id": id, "code": Int(code)])
}
tracker.start()

let clipboard = ClipboardSync()
clipboard.onLocalChange = { seq, text in send(["type": "clipboard_set", "seq": Int(seq), "text": text]) }
clipboard.start()

let uploads = UploadStore(send: send)
let displays = DisplayManager()

func keyTo(_ pid: pid_t, _ code: CGKeyCode, _ flags: CGEventFlags = []) {
    for down in [true, false] {
        guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: code, keyDown: down) else { continue }
        e.flags = flags
        e.postToPid(pid)
        usleep(20_000)
    }
}

/// Make a file panel open `path`: "Go to folder" (Cmd+Shift+G), type the full path, confirm twice.
let inputTypes: Set<String> = ["mouse_move", "mouse_button", "scroll", "key", "text_input"]

func handle(_ m: [String: Any]) {
    let type = m["type"] as? String ?? ""
    switch type {
    case "list_apps":
        send(["type": "apps", "apps": apps.list()])
    case "app_launch" where (m["application_id"] as? String) == desktopAppID:
        // Mac Desktop: the main display as one window
        guard !desktop.isActive else { break }
        send(["type": "app_launched", "application_id": desktopAppID, "pid": 0])
        // "fit=W,H,S": the client's screen (pixels, Mac scale). Like BetterDummy: a virtual display
        // of exactly that size, the Mac's screen mirrored onto it, and that display streamed
        var fitted: CGDirectDisplayID?
        if let fit = (m["arguments"] as? [String])?.first(where: { $0.hasPrefix("fit=") }) {
            let v = fit.dropFirst(4).split(separator: ",").compactMap { Int($0) }
            if v.count == 3 {
                let st = displays.configure(width: v[0], height: v[1], scale: v[2], forDesktop: true)
                if st["available"] as? Bool == true, displays.mirrorDesktop(onto: displays.displayID) {
                    usleep(500_000) // the window server settles the new layout
                    fitted = displays.displayID
                } else {
                    log("desktop: no fitted display (\(st["reason"] ?? "mirroring refused")); streaming the Mac's own screen")
                    displays.unmirrorDesktop()
                }
            }
        }
        send(desktop.start(display: fitted))
        let ws = WindowStream(windowID: desktopWindowID, display: desktop.displayID) { pkt in sender.sendVideo(pkt) }
        ws.setBitrate(sender.bitrate)
        streamsLock.lock(); streams[desktopWindowID] = ws; streamsLock.unlock()
        Task { do { try await ws.start(); log("desktop stream started") } catch { log("desktop stream failed: \(error)")
            send(["type": "capability_unavailable", "capability": "capture", "reason": "\(error)"]) } }
    case "app_terminate" where (m["application_id"] as? String) == desktopAppID,
         "window_close" where CGWindowID(int(m["window_id"])) == desktopWindowID:
        guard desktop.isActive else { break }
        desktop.stop()
        displays.unmirrorDesktop()
        stopStream(desktopWindowID)
        send(["type": "window_destroyed", "window_id": Int(desktopWindowID)])
        send(["type": "app_exited", "application_id": desktopAppID, "code": NSNull()])
    case "window_focus" where CGWindowID(int(m["window_id"])) == desktopWindowID,
         "window_fullscreen" where CGWindowID(int(m["window_id"])) == desktopWindowID,
         "window_resize_request" where CGWindowID(int(m["window_id"])) == desktopWindowID:
        break // the desktop is the display itself: nothing to raise or resize
    case "get_menu_bar" where (m["application_id"] as? String) == desktopAppID:
        send(["type": "menu_bar", "application_id": desktopAppID, "menus": [Any]()]) // the Mac's own menu bar is in the picture
    case "get_app_icon" where (m["application_id"] as? String) == desktopAppID:
        iconQueue.async {
            if let rgba = appIconRGBA(path: "/System/Library/CoreServices/Finder.app", size: 64) {
                try? sender.sendBulk(["type": "app_icon", "application_id": desktopAppID, "size": 64, "rgba_base64": rgba.base64EncodedString()])
            }
        }
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
            var size = CGSize(width: num(m["width"]), height: num(m["height"]) + w.inset) // the viewer asks for the picture size
            if let v = AXValueCreate(.cgSize, &size) { AXUIElementSetAttributeValue(aw, kAXSizeAttribute as CFString, v) }
        }
    case "window_focus":
        let wid = CGWindowID(int(m["window_id"]))
        if let w = tracker.current(wid) {
            sendMenuBar(w.appID)
            NSRunningApplication(processIdentifier: w.pid)?.activate(options: [.activateIgnoringOtherApps])
            if let aw = axWindowFor(pid: w.pid, id: wid, rect: w.rect) { AXUIElementPerformAction(aw, kAXRaiseAction as CFString) }
        }
    case "get_app_icon":
        // drawn off the reader thread and sent at low priority: a viewer asks for all the
        // icons at once, and a launch must not wait behind them
        let id = m["application_id"] as? String ?? ""
        guard let d = apps.descriptor(id) else { send(["type": "error", "code": "unknown_app", "message": id]); break }
        iconQueue.async {
            // the bundle (…/Foo.app) carries the real icon; a bare executable gets the generic one
            let bundle = d.executable.components(separatedBy: "/Contents/MacOS/").first ?? d.executable
            if let rgba = appIconRGBA(path: bundle, size: 64) {
                try? sender.sendBulk(["type": "app_icon", "application_id": id, "size": 64, "rgba_base64": rgba.base64EncodedString()])
            }
        }
    case "file_upload_begin", "file_upload_chunk", "file_upload_end":
        uploads.handle(m)
    case "panel_choose_file":
        let wid = CGWindowID(int(m["window_id"]))
        guard let w = tracker.current(wid), w.role == .open_panel, let path = m["remote_path"] as? String,
              path.hasPrefix(uploads.dir.path + "/"), FileManager.default.fileExists(atPath: path) else {
            send(["type": "error", "code": "panel_choose_failed", "message": "\(wid)"]); break
        }
        DispatchQueue.global().async { chooseInPanel(pid: w.pid, rect: w.rect, path: path) }
    case "panel_cancel":
        if let w = tracker.current(CGWindowID(int(m["window_id"]))), w.role == .open_panel || w.role == .save_panel { keyTo(w.pid, 53) }
    case "display_configure":
        let (w, h, sc) = (int(m["width"]), int(m["height"]), max(1, int(m["scale"])))
        DispatchQueue.global().async { send(displays.configure(width: w, height: h, scale: sc)) }
    case "window_fullscreen":
        let wid = CGWindowID(int(m["window_id"]))
        guard let w = tracker.current(wid) else { send(["type": "error", "code": "no_such_window", "message": "\(wid)"]); break }
        let on = m["on"] as? Bool ?? true
        DispatchQueue.global().async {
            if !displays.fullscreen(w, on: on) { send(["type": "error", "code": "fullscreen_failed", "message": "\(wid)"]) }
        }
    case "get_menu_bar":
        sendMenuBar(m["application_id"] as? String ?? "")
    case "menu_invoke":
        let id = m["application_id"] as? String ?? ""
        let path = (m["path"] as? [Any] ?? []).map { int($0) }
        guard let pid = apps.pidFor(id) else { send(["type": "error", "code": "not_running", "message": id]); break }
        NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateIgnoringOtherApps])
        if !invokeMenu(pid: pid, path: path) { send(["type": "error", "code": "menu_invoke_failed", "message": "\(id) \(path)"]) }
        // menus often change after a command (enabled items, window list)
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.8) { sendMenuBar(id) }
    case "clipboard_set":
        clipboard.apply(m["text"] as? String ?? "")
    case "request_keyframe":
        let wid = CGWindowID(int(m["window_id"]))
        streamsLock.lock(); let ws = streams[wid]; streamsLock.unlock()
        ws?.requestKeyframe()
    case "video_decoder":
        // the client decodes with Windows' own decoder: High profile (better quality per bit)
        useHighProfile = m["high_profile"] as? Bool ?? false
        let sc = num(m["scale"])
        if sc >= 1 { captureScale = CGFloat(min(3, sc)) }
        log("client decoder: high_profile=\(useHighProfile) hardware=\(m["hardware"] as? Bool ?? false) scale=\(captureScale)")
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
    displays.unmirrorDesktop()
    // ready for the next connection (same ID and password)
    if sessionArg == nil { restartForNextClient() }
    exit(0)
}
reader.stackSize = 4 << 20
reader.start()
dispatchMain()
