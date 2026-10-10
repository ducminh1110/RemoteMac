// macbridge (remote-agent-mac): the Mac side of MacBridge, started from Terminal.
//   ./macbridge --password SECRET     -> shows the ID and the password, then runs in the background
//   ./macbridge                       -> same, with a random password
//   ./macbridge --stop                -> stops the one running in the background
//   RM_SESSION_TOKEN=... ./macbridge --relay HOST:PORT --session NAME   (scripts, CI)
// Nothing is logged unless --logs-enabled is given.
import Foundation
import AppKit
import ApplicationServices
import VideoToolbox

/// Logs only when asked for (--logs-enabled, or RM_LOGS=1): to stderr, or in the background to
/// ~/Library/Logs/MacBridge/macbridge.log.
let logsEnabled = CommandLine.arguments.contains("--logs-enabled") || ProcessInfo.processInfo.environment["RM_LOGS"] == "1"
func log(_ s: String) { if logsEnabled { FileHandle.standardError.write(Data("[agent] \(s)\n".utf8)) } }
/// Errors that stop the app are always shown.
func fail(_ s: String) -> Never { FileHandle.standardError.write(Data("macbridge: \(s)\n".utf8)); exit(1) }

let usage = """
usage: macbridge [--password SECRET] [--id 123456789] [--relay HOST:PORT] [--port N] [--foreground] [--logs-enabled]
       macbridge --stop
       RM_SESSION_TOKEN=.. macbridge --relay HOST:PORT --session NAME
  --relay HOST:PORT  reachable from anywhere through this relay (it also gives this Mac its ID)
  --port N           the TCP port viewers on this network, or typing this Mac's address, join on (7471)
  --foreground       stay in the terminal instead of going to the background
  --logs-enabled     write a log (stderr; in the background ~/Library/Logs/MacBridge/macbridge.log)
  --stop             stop the MacBridge running in the background
  --check-permissions  say which macOS permissions are missing (exit 3 when one is)
  --version          show the version
"""
var relayArg: String?, sessionArg: String?, passwordArg: String?, idArg: String?, foreground = false
var argv = CommandLine.arguments.dropFirst().makeIterator()
while let a = argv.next() {
    switch a {
    case "--relay": relayArg = argv.next()
    case "--session": sessionArg = argv.next()
    case "--password": passwordArg = argv.next()
    case "--id": idArg = argv.next()?.filter(\.isNumber)
    case "--port":
        guard let p = argv.next().flatMap({ UInt16($0) }), p > 0 else { fail("--port takes a TCP port number (1-65535)") }
        directPort = p; setenv("RM_PORT", "\(p)", 1) // kept for the restart for the next client
    case "--foreground": foreground = true
    case "--logs-enabled": break
    case "--stop": exit(stopBackground() ? 0 : 1)
    case "-h", "--help": print(usage); exit(0)
    case "--version": print("macbridge \(appVersion)"); exit(0)
    case "--check-permissions": exit(checkPermissions())
    default: fail(usage)
    }
}
let env = ProcessInfo.processInfo.environment
// started in the background (below): our own session, so closing the terminal does not end us
if env["RM_DAEMON"] != nil { becomeDaemon() }
/// From a terminal (not a script) with the ID and password: show them, then go to the background.
let goBackground = !foreground && env["RM_DAEMON"] == nil && sessionArg == nil && isatty(STDOUT_FILENO) == 1
if goBackground, let pid = runningInBackground() {
    print("MacBridge is already running in the background (pid \(pid)). Stop it with: \(CommandLine.arguments[0]) --stop")
    exit(1)
}
// a session that ended without a word (a crash, power lost) may have left the PC's wallpaper
// (not while another MacBridge runs: it may be showing it)
if runningInBackground() == nil { Wallpaper.restore() }
// no relay: reachable from this network only (the viewer finds the Mac by its ID there)
let relayAddr: String? = [relayArg, env["RM_RELAY"], defaultRelay].compactMap { $0?.trimmingCharacters(in: .whitespaces) }.first { !$0.isEmpty }
let sessionID: String, token: String
/// the secret of a viewer that typed this Mac's address (the password alone; none in the legacy
/// session mode)
var directSecret: String?
if let s = sessionArg {
    guard let t = env["RM_SESSION_TOKEN"] else { fail(usage) }
    sessionID = s; token = t
} else {
    // ID + password mode; a restart for the next client keeps both (RM_PASSWORD, RM_ID)
    // the ID: given, else from the relay (it keeps IDs unique there), else this Mac's own
    // (with no relay it is only reached on this network, where the viewer looks for it by ID)
    let id: String
    if let given = idArg ?? env["RM_ID"] {
        id = given
    } else if let r = relayAddr, let i = claimID(relay: r) {
        id = i; log("ID \(i) from the relay \(r)")
    } else if let r = relayAddr, let i = savedRelayID(r) {
        id = i; log("ID \(i): the one the relay \(r) gave before (it did not answer now)")
    } else {
        id = persistentID(); log("ID \(id): this Mac's own\(relayAddr == nil ? " (no relay: reached on this network only)" : "")")
    }
    guard id.count == 9 else { fail("the ID must be 9 digits") }
    let password = passwordArg ?? env["RM_PASSWORD"] ?? randomPassword()
    guard password.count >= 4 else { fail("the password must have at least 4 characters") }
    setenv("RM_ID", id, 1); setenv("RM_PASSWORD", password, 1)
    sessionID = relaySession(id: id); token = sessionToken(id: id, password: password)
    directSecret = directToken(password: password)
    if env["RM_QUIET_BANNER"] == nil {
        print("")
        print("  MacBridge is ready — connect from Windows with:")
        print("    ID session to connect: \(displayID(id))")
        print("    Password: \(password)")
        if let r = relayAddr {
            print("    Reachable on this network directly, and from anywhere through the relay \(r)")
        } else {
            print("    Reachable on this network only (from anywhere: start with --relay HOST:PORT)")
        }
        if let ip = lanAddresses().first {
            print("    Or type this Mac's address in the viewer: \(ip)\(directPort == lanPort ? "" : ":\(directPort)") (By Address: only the password, no ID)")
        }
        print("    Connections are end-to-end encrypted.")
        for w in permissionWarnings() { print("  ! \(w)") }
        print("")
        setenv("RM_QUIET_BANNER", "1", 1) // printed once, not again after each session
        if goBackground {
            if let pid = startInBackground() {
                print("  Running in the background (pid \(pid)). Stop it with: \(CommandLine.arguments[0]) --stop")
                if logsEnabled { print("  Log: \(backgroundLogPath)") }
                print("")
                exit(0)
            }
            print("  (could not go to the background: running here; Ctrl+C to stop)")
        } else {
            print("  Ctrl+C to stop.")
        }
        print("")
        fflush(stdout)
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
/// The first viewer to arrive, on this network (straight to our port) or through the relay;
/// whichever comes first, the other way is closed.
final class ClientRace {
    private let lock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private var winner: Conn?
    /// the winner came straight over the local network
    private(set) var local = false
    /// the session it joined (ours, or the direct one)
    private(set) var joined = sessionID
    /// a relay connection still waiting for its client
    private var pending: Conn?
    static let lan = "on this network"
    var taken: Bool { lock.lock(); defer { lock.unlock() }; return winner != nil }
    func waiting(_ c: Conn?) { lock.lock(); pending = c; lock.unlock() }
    /// `c` is the connection taken, unless another came first (then it is closed).
    func offer(_ c: Conn, _ how: String, session: String = sessionID) {
        lock.lock(); defer { lock.unlock() }
        guard winner == nil else { close(c.fd); return }
        winner = c; local = how == ClientRace.lan; joined = session
        log("client connected (\(session == directSession ? "straight to this Mac's address" : how))"); done.signal()
    }
    func wait() -> Conn {
        done.wait()
        lock.lock(); defer { lock.unlock() }
        // a relay join still waiting is dropped (under the lock: its fd is still open)
        if let p = pending, p !== winner { Darwin.shutdown(p.fd, SHUT_RDWR) }
        return winner!
    }
}

func waitForClient() -> (Conn, local: Bool, session: String) {
    let lan = env["RM_NO_LAN"] == nil && sessionArg == nil ? LanListener(session: sessionID) : nil
    if let l = lan { log("on this network at port \(l.tcpPort) (found by the viewer through UDP \(lanPort))") }
    guard lan != nil || relayAddr != nil else { fail("no relay given and the local network port is unavailable: start with --relay HOST:PORT") }
    let race = ClientRace()
    if let relay = relayAddr {
        let lanToo = lan != nil
        Thread {
            var announced = false
            while !race.taken {
                let c: Conn
                do { c = try Conn.connect(hostPort: relay) } catch {
                    // without the LAN as well, nothing can come: start over as before
                    if !lanToo { log("relay \(relay) not reachable (\(error)); retrying in 5 s"); restartForNextClient(after: 5) }
                    if !announced { log("relay \(relay) not reachable (\(error)); still reachable on this network, retrying") }
                    announced = true; sleep(5); continue
                }
                if race.taken { close(c.fd); return }
                race.waiting(c)
                if !announced { log("relay \(relay) joined, session=\(sessionID) (token not logged); waiting for a client") }
                announced = true
                do { try joinRelay(c, session: sessionID) } catch {
                    race.waiting(nil); close(c.fd)
                    if race.taken { return }
                    // nobody came within the relay's wait (or the relay refused): wait again
                    if !lanToo { log("\(error); waiting again"); restartForNextClient(after: 2) }
                    log("relay: \(error); waiting again"); sleep(2); continue
                }
                race.waiting(nil)
                race.offer(c, "through the relay")
                return
            }
        }.start()
    } else {
        log("waiting for a client on this network (no relay)")
    }
    if let l = lan {
        Thread { if let got = l.accept() { race.offer(got.0, ClientRace.lan, session: got.1) } }.start()
    }
    let c = race.wait()
    lan?.close() // the LAN port closes once a client is in
    return (c, race.local, race.joined)
}
let (conn, cameLocally, joinedSession) = waitForClient()

// ---- end-to-end encryption: the viewer proves the password, both agree on the keys ---------------
/// Wrong passwords in a row (kept across the restart for the next client): five lock it for a minute.
let failState = (env["RM_PAKE_FAILS"] ?? "0:0").split(separator: ":").compactMap { Double($0) }
let failCount = Int(failState.first ?? 0), lockedUntil = failState.count > 1 ? failState[1] : 0
let sessionKeys: SessionKeys
do {
    // by ID: the ID's secret; straight to this Mac's address: the password's alone
    guard let secret = joinedSession == directSession ? directSecret : token else { throw SecureError.failed("no password for a direct connection (legacy session mode)") }
    sessionKeys = try agentHandshake(conn, session: joinedSession, secret: secret, locked: Date().timeIntervalSince1970 < lockedUntil)
    setenv("RM_PAKE_FAILS", "0:0", 1)
} catch SecureError.wrongPassword {
    let n = failCount + 1
    log("a viewer gave a wrong password (\(n) in a row)")
    setenv("RM_PAKE_FAILS", n >= 5 ? "0:\(Date().timeIntervalSince1970 + 60)" : "\(n):0", 1)
    restartForNextClient(after: 1)
} catch {
    log("\(error); waiting again")
    restartForNextClient(after: 1)
}
conn.cipher = StreamCipher(sessionKeys)
log("end-to-end encrypted (ChaCha20-Poly1305)")

func readJSON() throws -> [String: Any]? {
    guard let (_, payload) = try conn.readFrame() else { return nil }
    return try JSONSerialization.jsonObject(with: payload) as? [String: Any]
}

/// What the viewer can do (its hello's features): new messages are only sent to one that has them.
var viewerFeatures = Set<String>()
/// The viewer shows windows as the Mac draws them, title bar and buttons included ("window_style").
var exactWindows = false

do {
    guard let hello = try readJSON(), hello["type"] as? String == "client_hello" else { fail("expected client_hello") }
    viewerFeatures = Set(hello["features"] as? [String] ?? [])
    let cmin = int(hello["min_version"]), cmax = int(hello["max_version"])
    guard cmin <= 1 && cmax >= 1 else {
        try conn.send(["type": "error", "code": "version_mismatch", "message": "agent speaks protocol 1, client \(cmin)...\(cmax)"]); exit(1)
    }
    try conn.send(["type": "server_hello", "min_version": 1, "max_version": 1, "codecs": ["h264"], "features": ["control", "video", "audio", "open_file", "fusion", "mask", "exact", "menubar"],
                   "max_surface": [3840, 2160], "agent": "macbridge \(appVersion) \(ProcessInfo.processInfo.operatingSystemVersionString)"])
    try conn.send(probeCapabilities())
} catch { fail("handshake: \(error)") }
log("handshake complete")
let sender = Sender(conn: conn)
log("fec self-test \(fecSelfTest() ? "ok" : "FAILED")")
log("secure self-test \(secureSelfTest() ? "ok" : "FAILED")")

// ---- runtime -------------------------------------------------------------------------------------
let apps = AppManager()
let tracker = WindowTracker(apps: apps)
let desktop = DesktopSession()
let injector = InputInjector(tracker: tracker, desktop: desktop)
injector.dockRect = { dockMirror.rect }
injector.menuBarRect = { menuBarMirror.rect }
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
    dockMirror.setBitrate(b)
    menuBarMirror.setBitrate(b)
}
// video over UDP + FEC beside the TCP connection (RM_NO_UDP=1: TCP only)
// (with the client on this network, or no relay, a port that ignores it stands in for the
// relay: the direct path comes from the offer)
if env["RM_NO_UDP"] == nil, let u = UdpLink(hostPort: cameLocally ? "127.0.0.1:9" : relayAddr ?? "127.0.0.1:9", session: joinedSession, token: relayToken(joinedSession), key: relayKey(), cipher: DatagramCipher(sessionKeys), quickOffer: cameLocally) {
    sender.udp = u
    u.requestKeyframe = { wid in sender.requestKeyframe?(wid) }
    u.onAlive = { up in
        log("UDP video \(up ? "on" : "off (TCP)")")
        // the client resynchronises on a keyframe after a transport change
        streamsLock.lock(); let all = Array(streams.values); streamsLock.unlock()
        for ws in all { ws.requestKeyframe() }
    }
    // direct path to the client (as Moonlight connects straight to the host)
    u.onOffer = { secret, cands in send(["type": "p2p_offer", "secret": secret, "candidates": cands]) }
    u.onPath = { path in
        log(path.map { "video and input now go straight to the client (\($0))" } ?? "video goes through the relay")
        sender.pathChanged()
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
let inputQueue = DispatchQueue(label: "rm.input", qos: .userInteractive)
/// Read (off the main path: big apps take a moment) and send an app's menu bar.
func sendMenuBar(_ id: String) {
    menuQueue.async {
        guard let pid = apps.pidFor(id) else { return }
        send(["type": "menu_bar", "application_id": id, "menus": readMenuBar(pid: pid)])
    }
}

/// `popup`: a pop-up menu or popover (cut out of its display). Callers pass what they know of
/// the window: the tracker's callbacks run on its queue, where asking it again would deadlock.
func startStream(_ id: CGWindowID, inset: CGFloat, popup: Bool = false) {
    let ws = WindowStream(windowID: id, inset: inset) { pkt in sender.sendVideo(pkt) }
    ws.popup = popup
    ws.keepButtons = exactWindows
    // the shape: all of it for exact windows and popups; under the viewer's own title bar
    // (its frame) the top corners are filled, so they are not cut out
    let framed = !exactWindows && !popup
    if viewerFeatures.contains("mask") { ws.onShape = { w, h, a in sendShape(id, w, h, framed ? Shape.fillingTop(a, width: w, height: h) : a) } }
    ws.setBitrate(sender.bitrate)
    streamsLock.lock(); streams[id] = ws; streamsLock.unlock()
    Task { do { try await ws.start(); log("stream started window=\(id)") } catch { log("stream start failed window=\(id): \(error)")
        send(["type": "capability_unavailable", "capability": "capture", "reason": "\(error)"]) } }
}
/// The shape of window `id`'s picture, for the viewer ("window_mask"), while its stream is the
/// one measured (a newer stream of another size sends its own).
func sendShape(_ id: CGWindowID, _ w: Int, _ h: Int, _ a: [UInt8]) {
    guard let m = Shape.message(id, width: w, height: h, alpha: a) else { return }
    let clear = a.reduce(0) { $0 + ($1 < 128 ? 1 : 0) }
    log("window \(id) shape: \(w)x\(h) px, \(clear) clear (\((m["rle"] as? String)?.count ?? 0) bytes)")
    send(m)
}
/// The title bar of exact window `id` as it is now ("window_chrome"), for the viewer to move it by.
func sendChrome(_ id: CGWindowID) {
    guard let w = tracker.current(id), let m = windowChrome(id: id, pid: w.pid, rect: w.rect) else { return }
    log("window \(id) title bar: \(m["title_height"] ?? 0) points, \((m["controls"] as? [Any])?.count ?? 0) control(s) in it")
    send(m)
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
    startStream(w.id, inset: w.inset, popup: w.role == .popup)
    if w.role == .popup {
        // a menu is still being drawn (its items, the highlight) when it is first captured, and the
        // capture only sends a new picture when something changes: capture it afresh once it is up
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.35) {
            streamsLock.lock(); let open = streams[w.id] != nil; streamsLock.unlock()
            if open { stopStream(w.id); startStream(w.id, inset: 0, popup: true) }
        }
    }
    if w.role == .window { DispatchQueue.global().asyncAfter(deadline: .now() + 0.6) { sendMenuBar(w.appID) } }
    if exactWindows && w.role != .popup { DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) { sendChrome(w.id) } }
}
tracker.onDestroyed = { id in
    log("window destroyed id=\(id)")
    displays.windowGone(id)
    lastSize.removeValue(forKey: id)
    stopStream(id)
    sender.udp?.forgetWindow(UInt64(id))
    send(["type": "window_destroyed", "window_id": Int(id)])
}
tracker.onMoved = { w in
    send(["type": "window_moved", "window_id": Int(w.id), "bounds": rectJSON(w.content)])
    if lastSize[w.id] != w.rect.size {            // size changed: the encoder is bound to a size, so restart the stream
        lastSize[w.id] = w.rect.size
        stopStream(w.id); startStream(w.id, inset: w.inset, popup: w.role == .popup)
        // the toolbar's items move with the width
        if exactWindows && w.role != .popup { DispatchQueue.global().asyncAfter(deadline: .now() + 0.4) { sendChrome(w.id) } }
    }
}
tracker.onTitle = { w in send(["type": "window_title_changed", "window_id": Int(w.id), "title": w.title]) }
tracker.onAdopted = { id, pid in
    send(["type": "app_launched", "application_id": id, "pid": Int(pid)])
}
tracker.onAppExited = { id, code in
    log("app exited on its own id=\(id) code=\(code)")
    send(["type": "app_exited", "application_id": id, "code": Int(code)])
}
tracker.start()

let clipboard = ClipboardSync()
clipboard.onLocalChange = { seq, text in send(["type": "clipboard_set", "seq": Int(seq), "text": text]) }
clipboard.onLocalImage = { seq, bmp in send(["type": "clipboard_image", "seq": Int(seq), "bmp_base64": bmp.base64EncodedString()]) }
clipboard.start()

// the Mac's sound, once the viewer asks for it: the session's apps, or every app while the Mac
// Desktop is open
let audioCap = AudioCapture { payload in sender.sendAudio(payload) }
audioCap.onStatus = { state, why in
    var m: [String: Any] = ["type": "audio_status", "state": state]
    if let w = why { m["reason"] = w }
    send(m)
}

// Desktop Fusion: the Mac's own Dock streamed to Windows, over the PC's wallpaper
let dockMirror = DockMirror()
dockMirror.onPacket = { pkt in sender.sendVideo(pkt) }
dockMirror.onStatus = { m in send(m) }
dockMirror.onShown = { d in tracker.setDock(d) }

// exact windows' menus: the Mac's own menu bar streamed to Windows
let menuBarMirror = MenuBarMirror()
menuBarMirror.onPacket = { pkt in sender.sendVideo(pkt) }
menuBarMirror.onStatus = { m in send(m) }
menuBarMirror.onShown = { r in tracker.setMenuBar(r) }

let uploads = UploadStore(send: send)
uploads.cleanup() // leftovers of a session that ended without cleaning (crash, power loss)
let displays = DisplayManager()
/// the Mac's screens mirror our HiDPI display for the whole session (not only the Mac Desktop)
var keepMirror = false
/// the layout the app windows get ("W,H,S" from the client): the Mac Desktop may lay out its
/// own (the laptop's exact scale) while it is open; this one comes back when it closes
var appScreen: [Int]?
/// what the client last asked for (its "screen")
var appScreenArg: String?

/// App windows on a HiDPI virtual display (Mac screen mirrored onto it), from the client's
/// "W,H,S". The client asks for its screen's size in points at 2x ("Ultra" sharpness): every
/// app renders twice the pixels per point and the client scales the picture down to its own
/// density (supersampling), sharper than drawing at the laptop's own scale. nil: none.
func applyAppScreen(_ screen: String?) {
    let v = screen.map { $0.split(separator: ",").compactMap { Int($0) } }
    // "W,H,1,1": pixel for pixel, the client's own pixels at 1x (as Sunshine streams a screen)
    let exact = v.map { $0.count >= 4 && $0[3] == 1 } ?? false
    guard let v = v, v.count >= 3, v[2] >= 2 || exact, (NSScreen.main?.backingScaleFactor ?? 1) < 2 || exact, env["RM_NO_HIDPI"] == nil else {
        appScreen = nil
        if keepMirror && !desktop.isActive { keepMirror = false; DispatchQueue.global().async { displays.unmirrorDesktop() } }
        return
    }
    appScreen = v
    keepMirror = true
    // the Mac Desktop keeps its own layout while open; this one is made when it closes
    guard !desktop.isActive else { return }
    DispatchQueue.global().async {
        if displays.ensureMirrored(width: v[0], height: v[1], scale: v[2]) != nil {
            log("desktop for apps: \(v[0] / v[2])x\(v[1] / v[2]) points at \(v[2])x")
        } else {
            keepMirror = false
            log("HiDPI desktop unavailable (mirroring refused); windows stay 1x")
        }
    }
}

func keyTo(_ pid: pid_t, _ code: CGKeyCode, _ flags: CGEventFlags = []) {
    for down in [true, false] {
        guard let e = CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: code, keyDown: down) else { continue }
        e.flags = flags
        e.postToPid(pid)
        usleep(20_000)
    }
}

// ---- Mac Desktop over full GameStream (crates/rm-gamestream: a Sunshine-style host session) ----
// The client's Moonlight core reaches it through the tunnel: RTSP as "gs_tunnel" messages on
// this connection, the UDP flows as "RM" 25 datagrams on the UDP path.
var gsTunnel: OpaquePointer?
/// The viewer gets GameStream's pictures: the desktop goes only that way. Until then it also goes
/// the usual way, so it shows at once (as an app window does) while Moonlight connects.
var gsReady = false
var gsPoint = CGPoint.zero
let gsOut: rm_gs_out = { _, kind, id, data, len in
    let bytes = data.map { Data(bytes: $0, count: len) } ?? Data()
    if kind < 10 {
        sender.udp?.sendTunnel(UInt8(kind), bytes)
    } else if kind == 10 {
        send(["type": "gs_tunnel", "id": Int(id), "op": "data", "data_base64": bytes.base64EncodedString()])
    } else {
        send(["type": "gs_tunnel", "id": Int(id), "op": "close"])
    }
}

func gsStart(keyHex: String) -> Bool {
    guard let key = unhex(keyHex), key.count == 16 else { return false }
    var ports = [UInt16](repeating: 0, count: 4)
    guard let t = rm_gs_desktop_start(key, 20, gsOut, nil, &ports) else { return false }
    gsTunnel = t
    log("Mac Desktop: GameStream host session (rtsp \(ports[0]), video \(ports[1]), audio \(ports[2]), control \(ports[3])) behind the tunnel")
    sender.udp?.onTunnel = { flow, d in
        guard let t = gsTunnel else { return }
        d.withUnsafeBytes { rm_gs_desktop_udp_in(t, flow, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), d.count) }
    }
    // events: input from Moonlight's input stream, IDR requests
    Thread {
        var ev = RmGsEvent()
        while let t = gsTunnel {
            var any = false
            while rm_gs_desktop_poll(t, &ev) { any = true; gsEvent(ev) }
            if !any { usleep(1000) }
        }
    }.start()
    return true
}

func gsStop() {
    guard let t = gsTunnel else { return }
    gsTunnel = nil
    gsReady = false
    sender.udp?.onTunnel = nil
    rm_gs_desktop_stop(t)
}

/// One GameStream event, as the agent's own input messages for the desktop window.
func gsEvent(_ e: RmGsEvent) {
    let b = desktop.bounds
    var m: [String: Any] = ["window_id": Int(desktopWindowID)]
    switch e.kind {
    case 1: log("Mac Desktop GameStream: client asks \(e.a)x\(e.b) at \(e.c) fps, \(e.d) kbit/s"); return
    case 2:
        streamsLock.lock(); let ws = streams[desktopWindowID]; streamsLock.unlock()
        ws?.requestKeyframe(); return
    case 3:
        log("Mac Desktop GameStream: client left; the desktop goes the usual way")
        gsReady = false
        streamsLock.lock(); let ws = streams[desktopWindowID]; streamsLock.unlock()
        ws?.requestKeyframe(); return
    case 10:
        if [0x10, 0x11, 0x12, 0x14, 0x5B, 0x5C].contains(e.a) || (0xA0...0xA5).contains(e.a) { return } // modifiers ride on keys
        guard let n = rm_gs_vk_name(UInt16(e.a)) else { return }
        var mods: [String] = []
        if e.c & 0x01 != 0 { mods.append("shift") }
        if e.c & 0x02 != 0 { mods.append("control") }
        if e.c & 0x04 != 0 { mods.append("option") }
        if e.c & 0x08 != 0 { mods.append("command") }
        m["type"] = "key"; m["physical_key"] = String(cString: n); m["modifiers"] = mods; m["down"] = e.b != 0
    case 11:
        gsPoint = CGPoint(x: max(0, min(b.width, gsPoint.x + CGFloat(e.a))), y: max(0, min(b.height, gsPoint.y + CGFloat(e.b))))
        m["type"] = "mouse_move"; m["x"] = Double(gsPoint.x); m["y"] = Double(gsPoint.y)
    case 12:
        guard e.c > 0, e.d > 0 else { return }
        gsPoint = CGPoint(x: CGFloat(e.a) * b.width / CGFloat(e.c), y: CGFloat(e.b) * b.height / CGFloat(e.d))
        m["type"] = "mouse_move"; m["x"] = Double(gsPoint.x); m["y"] = Double(gsPoint.y)
    case 13:
        m["type"] = "mouse_button"; m["button"] = e.a == 3 ? "right" : e.a == 2 ? "middle" : "left"; m["down"] = e.b != 0
        m["path"] = "gs" // the viewer sends each press and release over its own input path too
        m["x"] = Double(gsPoint.x); m["y"] = Double(gsPoint.y)
    case 14: m["type"] = "scroll"; m["dx"] = 0.0; m["dy"] = Double(e.a) * 40 / 120
    case 15: m["type"] = "scroll"; m["dx"] = Double(e.a) * 40 / 120; m["dy"] = 0.0
    case 16:
        var t = e.text
        let s = withUnsafeBytes(of: &t) { String(cString: $0.bindMemory(to: CChar.self).baseAddress!) }
        m["type"] = "text_input"; m["text"] = s
    default: return
    }
    inputQueue.async { handle(m) }
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
        displays.desktopOpened() // its menu bar and Dock, even while an app window is fullscreen
        // "fit=W,H,S": the client's screen (pixels, Mac scale). Like BetterDummy: a virtual display
        // of exactly that size, the Mac's screen mirrored onto it, and that display streamed
        var fitted: CGDirectDisplayID?, fittedPixels: (Int, Int)?
        if let fit = (m["arguments"] as? [String])?.first(where: { $0.hasPrefix("fit=") }) {
            let v = fit.dropFirst(4).split(separator: ",").compactMap { Int($0) }
            if v.count == 3 {
                // the HiDPI display made at connect is reused when it is this size
                if let id = displays.ensureMirrored(width: v[0], height: v[1], scale: v[2]) {
                    usleep(300_000) // the window server settles the new layout
                    fitted = id
                    fittedPixels = (v[0], v[1]) // streamed at the scale the client chose (1x or 2x)
                } else {
                    log("desktop: no fitted display (mirroring refused); streaming the Mac's own screen")
                }
            }
        }
        inputQueue.async { injector.resetDesktopClicks() }
        send(desktop.start(display: fitted))
        audioCap.update(everything: true, pids: Set(apps.pids)) // the whole Mac is heard
        // full GameStream mode: the client's Moonlight core gets the desktop through a host session
        if let gs = (m["arguments"] as? [String])?.first(where: { $0.hasPrefix("gamestream=") }) {
            if !gsStart(keyHex: String(gs.dropFirst(11))) { log("Mac Desktop: GameStream host session failed; streaming the usual way") }
        }
        let ws = WindowStream(windowID: desktopWindowID, display: desktop.displayID) { pkt in
            if let t = gsTunnel {
                let age = agentClockUs() > pkt.ptsMicros ? agentClockUs() - pkt.ptsMicros : 0
                _ = pkt.data.withUnsafeBytes { rm_gs_desktop_frame(t, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), pkt.data.count, pkt.keyframe, age) }
            }
            if gsTunnel == nil || !gsReady { sender.sendVideo(pkt) }
        }
        ws.pixels = fittedPixels
        ws.setBitrate(sender.bitrate)
        streamsLock.lock(); streams[desktopWindowID] = ws; streamsLock.unlock()
        Task { do { try await ws.start(); log("desktop stream started") } catch { log("desktop stream failed: \(error)")
            send(["type": "capability_unavailable", "capability": "capture", "reason": "\(error)"]) } }
    case "app_terminate" where (m["application_id"] as? String) == desktopAppID,
         "window_close" where CGWindowID(int(m["window_id"])) == desktopWindowID:
        guard desktop.isActive else { break }
        desktop.stop()
        audioCap.update(everything: false, pids: Set(apps.pids))
        displays.desktopClosed() // a fullscreen app window gets the whole display again
        gsStop()
        // the app windows get their own layout back (HiDPI, Ultra sharpness), or the Mac's own
        if keepMirror, let v = appScreen {
            DispatchQueue.global().async {
                if displays.ensureMirrored(width: v[0], height: v[1], scale: v[2]) == nil { keepMirror = false; log("HiDPI desktop for apps not restored") }
            }
        } else {
            keepMirror = false
            displays.unmirrorDesktop()
        }
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
        if let pid = r.pid {
            log("launched \(id) pid=\(pid)\(apps.launchedByUs(id) ? "" : " (already open on the Mac)")")
            if !apps.launchedByUs(id) { tracker.adopt(pid: pid) }
            send(["type": "app_launched", "application_id": id, "pid": Int(pid)])
        }
        else if let e = r.err { send(["type": "error", "code": e.0, "message": e.1]) }
    case "app_terminate":
        // quit as Cmd+Q does (the app may ask to save; its sheet shows in the viewer), also apps
        // that were already open on the Mac; a process we started that ignores it and has no
        // window left is stopped
        let id = m["application_id"] as? String ?? ""
        guard let pid = apps.pidFor(id) else { send(["type": "error", "code": "not_running", "message": id]); break }
        if id == "finder" { apps.forget(id); send(["type": "app_exited", "application_id": id, "code": NSNull()]); break } // Finder never quits
        DispatchQueue.global().async {
            let app = NSRunningApplication(processIdentifier: pid)
            if let a = app { a.terminate() } else { _ = apps.terminate(id: id) }
            var gone = false
            for _ in 0..<30 {
                gone = app?.isTerminated ?? (kill(pid, 0) != 0)
                if gone { break }
                usleep(100_000)
            }
            // still running: unless it is asking something (a dialog is up), stop a process we started
            if !gone && !tracker.hasDialog(pid: pid) && apps.launchedByUs(id) { _ = apps.terminate(id: id); gone = true }
            if gone {
                apps.forget(id)
                log("quit \(id)")
                send(["type": "app_exited", "application_id": id, "code": NSNull()])
            } else {
                log("\(id) did not quit (it is asking something, or was open before)")
            }
        }
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
        DispatchQueue.global().async {
            // the session's HiDPI display (the Mac's screen mirrored onto it) is that display:
            // replacing it would leave the Mac's screen mirroring nothing
            if keepMirror, let st = displays.status() { send(st) } else { send(displays.configure(width: w, height: h, scale: sc)) }
        }
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
        let t = m["text"] as? String ?? ""
        log("clipboard <- Windows: \(t.count) characters")
        clipboard.apply(t)
    case "clipboard_image":
        if let d = Data(base64Encoded: m["bmp_base64"] as? String ?? "") { clipboard.applyImage(d) }
    case "request_keyframe" where CGWindowID(int(m["window_id"])) == dockWindowID:
        dockMirror.requestKeyframe()
    case "request_keyframe" where CGWindowID(int(m["window_id"])) == menuBarWindowID:
        menuBarMirror.requestKeyframe()
    case "request_keyframe":
        let wid = CGWindowID(int(m["window_id"]))
        streamsLock.lock(); let ws = streams[wid]; streamsLock.unlock()
        ws?.requestKeyframe()
    case "video_decoder":
        // the client decodes with Windows' own decoder: High profile (better quality per bit)
        useHighProfile = m["high_profile"] as? Bool ?? false
        let sc = num(m["scale"])
        if sc >= 1 { captureScale = CGFloat(min(3, sc)) }
        // a 1x Mac renders windows at 1x: blurry on a HiDPI PC. Like BetterDummy, lay the desktop
        // out on a HiDPI virtual display (Mac screen mirrored onto it): every app renders at 2x
        appScreenArg = m["screen"] as? String ?? ""
        applyAppScreen(appScreenArg)
        log("client decoder: high_profile=\(useHighProfile) hardware=\(m["hardware"] as? Bool ?? false) scale=\(captureScale)")
    case "stream_settings":
        // the viewer's settings (as Moonlight's): frame rate, bitrate (nil: Auto), sharpness
        let fps = Int32(max(10, min(144, int(m["fps"]))))
        let sc = num(m["scale"])
        // the app windows' layout ("" none): Ultra sharpness lays them out at 2x
        let screen = m["screen"] as? String
        let relayout = screen != nil && screen != appScreenArg
        if relayout { appScreenArg = screen; applyAppScreen(screen) }
        let changed = fps != targetFPS || (sc > 0 && CGFloat(sc) != captureScale) || relayout
        targetFPS = fps
        if sc > 0 { captureScale = CGFloat(max(0.5, min(3, sc))) }
        // the viewer shows its own pointer: the Mac's is left out of the picture (live)
        if let on = m["mac_cursor"] as? Bool, on != showRemoteCursor {
            showRemoteCursor = on
            streamsLock.lock(); let all = Array(streams.values); streamsLock.unlock()
            for ws in all { ws.setCursor(on) }
        }
        let kbps = int(m["bitrate_kbps"])
        sender.setCeiling(kbps > 0 ? kbps * 1000 : nil)
        log("settings: \(fps) fps, bitrate \(kbps > 0 ? "\(kbps) kbit/s" : "auto"), \(captureScale) px per point")
        if changed {
            // capture size and rate are fixed per stream: restart the windows' streams
            // (after a new layout, once the apps have redrawn at their new density)
            DispatchQueue.global().asyncAfter(deadline: .now() + (relayout ? 2 : 0)) {
                streamsLock.lock(); let ids = streams.keys.filter { $0 != desktopWindowID }; streamsLock.unlock()
                for id in ids {
                    guard let w = tracker.current(id) else { continue }
                    stopStream(id); startStream(id, inset: w.inset, popup: w.role == .popup)
                }
            }
        }
    case "window_style":
        exactWindows = m["exact"] as? Bool ?? false
        log("windows shown \(exactWindows ? "as the Mac draws them (title bar and buttons)" : "with the viewer's own title bar")")
    case "menu_bar_stream":
        let on = m["enabled"] as? Bool ?? false
        log("Mac menu bar on Windows: \(on ? "asked for" : "no longer wanted")")
        if on { menuBarMirror.start() } else { menuBarMirror.stop() }
    case "dock_stream":
        let on = m["enabled"] as? Bool ?? false
        log("Mac Dock on Windows: \(on ? "asked for" : "no longer wanted")")
        if on { dockMirror.start() } else { dockMirror.stop() }
    case "set_wallpaper":
        let path = m["path"] as? String
        if let p = path, let why = Wallpaper.rejection(p, uploads: uploads.dir) {
            send(["type": "wallpaper_status", "applied": false, "reason": why]); break
        }
        let (style, color) = (m["style"] as? String ?? "fill", m["color"] as? String ?? "#000000")
        DispatchQueue.global().async {
            if let why = Wallpaper.apply(path: path, style: style, color: color) {
                send(["type": "wallpaper_status", "applied": false, "reason": why])
            } else {
                send(["type": "wallpaper_status", "applied": true])
            }
        }
    case "restore_wallpaper":
        DispatchQueue.global().async {
            Wallpaper.restore()
            send(["type": "wallpaper_status", "applied": false])
        }
    case "audio_control":
        let on = m["enabled"] as? Bool ?? false
        log("sound \(on ? "asked for" : "no longer wanted") by the viewer")
        audioCap.setWanted(on, everything: desktop.isActive, pids: Set(apps.pids))
    case "ping":
        viewerSendsHeartbeats = true
        send(["type": "pong", "nonce": m["nonce"] ?? 0])
    case "gs_tunnel":
        guard let t = gsTunnel else { break }
        let id = UInt32(int(m["id"]))
        switch m["op"] as? String ?? "" {
        case "ready":
            // the viewer shows GameStream's pictures now: the usual stream of the desktop stops
            if !gsReady { gsReady = true; log("Mac Desktop: GameStream carries the desktop now") }
        case "open": rm_gs_desktop_tcp_open(t, id)
        case "data":
            let d = Data(base64Encoded: m["data_base64"] as? String ?? "") ?? Data()
            d.withUnsafeBytes { rm_gs_desktop_tcp_data(t, id, $0.baseAddress?.assumingMemoryBound(to: UInt8.self), d.count) }
        default: rm_gs_desktop_tcp_close(t, id)
        }
    case "p2p_offer":
        sender.udp?.peerOffer(secret: m["secret"] as? String ?? "", candidates: m["candidates"] as? [String] ?? [])
    case "open_file":
        // a document from the viewer (dropped on an app's window, or on the launcher)
        let path = m["path"] as? String ?? ""
        let appID = m["application_id"] as? String
        if let why = openRejection(path, uploads: uploads.dir) { send(["type": "error", "code": "open_rejected", "message": why]); break }
        var bundle: String?
        if let id = appID, id != desktopAppID {
            guard let b = apps.bundle(of: id) else { send(["type": "error", "code": "open_rejected", "message": "unknown application '\(id)'"]); break }
            bundle = b
        }
        tracker.noteInput() // the app that opens it is shown on Windows
        log("opening a document\(appID.map { " with \($0)" } ?? "")")
        openDocument(path, appBundle: bundle) { err in
            if let e = err { send(["type": "error", "code": "open_failed", "message": e]) }
        }
    case _ where inputTypes.contains(type):
        // a click or key in an app window: an app it opens is shown on Windows too
        if CGWindowID(int(m["window_id"])) != desktopWindowID && (type == "key" || type == "text_input" || (type == "mouse_button" && m["down"] as? Bool == true)) {
            tracker.noteInput()
        }
        if let err = injector.handle(m) { send(["type": "error", "code": "input_failed", "message": err]) }
    default:
        send(["type": "error", "code": "unexpected", "message": "message '\(type)' not valid for agent"])
    }
}

// input runs on its own queue (inputQueue), in arrival order, whether it came over TCP or straight over UDP
sender.udp?.onInput = { m in inputQueue.async { handle(m) } }

// ---- a viewer gone without a word (network lost) is noticed, and the Mac waits for the next ----
/// The viewer pings every 2 s (older viewers do not: then nothing is assumed).
var viewerSendsHeartbeats = false
/// When the viewer was last heard over the connection.
var heardOverTCP = CFAbsoluteTimeGetCurrent()
/// The session ended because the viewer went silent, not because it closed.
var connectionLost = false
Thread {
    var ticks = 0
    while true {
        sleep(1)
        // the sound follows the session's apps (one launched, one quit)
        ticks += 1
        if ticks % 2 == 0 { audioCap.update(everything: desktop.isActive, pids: Set(apps.pids)) }
        if ticks % 2 == 1 { dockMirror.refresh() } // the Dock grew, moved, or restarted
        if ticks % 3 == 0 { menuBarMirror.refresh() } // the main display or the bar's height changed
        let heard = max(heardOverTCP, sender.udp?.lastHeard ?? 0)
        let silent = CFAbsoluteTimeGetCurrent() - heard
        if viewerSendsHeartbeats && silent > 10 {
            log("nothing from the viewer for \(Int(silent)) s: the connection is gone; waiting for the next one")
            connectionLost = true
            Darwin.shutdown(conn.fd, SHUT_RDWR) // the read loop ends, and with it the session
            return
        }
    }
}.start()

let reader = Thread {
    do {
        while let (ch, payload) = try conn.readFrame() {
            heardOverTCP = CFAbsoluteTimeGetCurrent()
            guard ch != .video, let m = try? JSONSerialization.jsonObject(with: payload) as? [String: Any] else { continue }
            // other messages wait for the input before them (typing, then closing the window)
            if inputTypes.contains(m["type"] as? String ?? "") { inputQueue.async { handle(m) } } else { inputQueue.sync {}; handle(m) }
        }
        log("client disconnected")
    } catch { log("read loop ended: \(error)") }
    // the viewer closed: its apps close with it; the connection was lost: they stay open, and the
    // viewer finds them again when it connects back
    if connectionLost { log("the apps stay open for the viewer to come back to") } else { apps.terminateAll() }
    displays.setChromeHidden(false) // the menu bar and Dock as the user had them
    Wallpaper.restore()              // and the wallpaper
    displays.unmirrorDesktop()
    uploads.cleanup()   // the session's uploaded files go with it
    // ready for the next connection (same ID and password)
    if sessionArg == nil { restartForNextClient() }
    exit(0)
}
reader.stackSize = 4 << 20
reader.start()
dispatchMain()
