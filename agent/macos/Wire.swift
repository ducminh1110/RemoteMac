// Wire format shared with crates/rm-protocol:
//   u32 BE length | u8 channel | payload     (length counts channel byte + payload)
// Control/metadata/input payloads are JSON; the Video channel is binary (see VideoPacket).
import Foundation
import CryptoKit

enum Chan: UInt8 { case input = 0, control = 1, windowMetadata = 2, video = 3, clipboard = 4, files = 5, telemetry = 6, audio = 7 }

func channel(forType t: String) -> Chan {
    switch t {
    case "mouse_move", "mouse_button", "scroll", "key", "text_input": return .input
    case "window_created", "window_destroyed", "window_moved", "window_title_changed": return .windowMetadata
    case "ping", "pong": return .telemetry
    case "clipboard_set", "clipboard_image": return .clipboard // pictures are larger than a control frame may be
    default: return .control
    }
}

struct WireError: Error, CustomStringConvertible { let description: String }

final class Conn {
    let fd: Int32
    private let writeLock = NSLock()
    /// set once the end-to-end handshake is done: from then on everything is encrypted
    var cipher: StreamCipher?
    /// decrypted bytes not read yet
    private var rbuf = Data()
    init(fd: Int32) { self.fd = fd }

    static func connect(hostPort: String) throws -> Conn {
        guard let idx = hostPort.lastIndex(of: ":"), let port = Int(hostPort[hostPort.index(after: idx)...]) else {
            throw WireError(description: "bad relay address \(hostPort)")
        }
        let host = String(hostPort[..<idx])
        var hints = addrinfo(); hints.ai_family = AF_UNSPEC; hints.ai_socktype = SOCK_STREAM
        var res: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, String(port), &hints, &res) == 0, let first = res else { throw WireError(description: "resolve \(host) failed") }
        defer { freeaddrinfo(res) }
        var p: UnsafeMutablePointer<addrinfo>? = first
        while let ai = p {
            let fd = socket(ai.pointee.ai_family, ai.pointee.ai_socktype, ai.pointee.ai_protocol)
            if fd >= 0 {
                if Darwin.connect(fd, ai.pointee.ai_addr, ai.pointee.ai_addrlen) == 0 {
                    var one: Int32 = 1
                    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))
                    // keep at most ~128 KB unsent in the kernel: writes then block early, so the
                    // sender sees a slow link at once instead of filling seconds of buffers
                    var lowat: Int32 = 128 * 1024
                    setsockopt(fd, IPPROTO_TCP, 0x201 /* TCP_NOTSENT_LOWAT */, &lowat, socklen_t(MemoryLayout<Int32>.size))
                    return Conn(fd: fd)
                }
                close(fd)
            }
            p = ai.pointee.ai_next
        }
        throw WireError(description: "connect \(hostPort) failed")
    }

    /// Write all of `data`: as encrypted records once the handshake is done.
    func writeAll(_ data: Data) throws {
        writeLock.lock(); defer { writeLock.unlock() }
        guard let c = cipher else { try rawWrite(data); return }
        var off = 0
        repeat {
            let end = min(off + 65536, data.count)
            let box = try ChaChaPoly.seal(data.subdata(in: (data.startIndex + off)..<(data.startIndex + end)), using: c.tx, nonce: secureNonce(c.txCounter))
            c.txCounter += 1
            let n = box.ciphertext.count + box.tag.count
            var rec = Data([UInt8(n >> 24), UInt8((n >> 16) & 0xff), UInt8((n >> 8) & 0xff), UInt8(n & 0xff)])
            rec.append(box.ciphertext); rec.append(box.tag)
            try rawWrite(rec)
            off = end
        } while off < data.count
    }

    private func rawWrite(_ data: Data) throws {
        try data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            var off = 0
            while off < raw.count {
                let n = Darwin.write(fd, raw.baseAddress! + off, raw.count - off)
                if n <= 0 { throw WireError(description: "write failed errno=\(errno)") }
                off += n
            }
        }
    }

    /// Reads exactly n bytes (decrypted once the handshake is done); nil on clean EOF before the
    /// first byte.
    func readExact(_ n: Int) throws -> Data? {
        guard let c = cipher else { return try rawReadExact(n) }
        while rbuf.count < n {
            guard let head = try rawReadExact(4) else {
                if rbuf.isEmpty { return nil }
                throw WireError(description: "eof mid-frame")
            }
            let len = Int(head.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) })
            guard len >= 16, len <= 65536 + 16, let ct = try rawReadExact(len) else { throw WireError(description: "bad encrypted record") }
            let box = try ChaChaPoly.SealedBox(nonce: secureNonce(c.rxCounter), ciphertext: Data(ct.prefix(len - 16)), tag: Data(ct.suffix(16)))
            guard let pt = try? ChaChaPoly.open(box, using: c.rx) else { throw WireError(description: "encrypted record failed authentication") }
            c.rxCounter += 1
            rbuf.append(pt)
        }
        let out = Data(rbuf.prefix(n))
        rbuf = Data(rbuf.dropFirst(n))
        return out
    }

    private func rawReadExact(_ n: Int) throws -> Data? {
        var buf = Data(count: n)
        var off = 0
        try buf.withUnsafeMutableBytes { (raw: UnsafeMutableRawBufferPointer) in
            while off < n {
                let r = Darwin.read(fd, raw.baseAddress! + off, n - off)
                if r == 0 { if off == 0 { return } else { throw WireError(description: "eof mid-frame") } }
                if r < 0 { if errno == EINTR { continue }; throw WireError(description: "read failed errno=\(errno)") }
                off += r
            }
        }
        return off == n ? buf : nil
    }

    func readLine(maxLen: Int = 64) throws -> String {
        var bytes = [UInt8](); var b = [UInt8](repeating: 0, count: 1)
        while bytes.last != 10 {
            if bytes.count > maxLen { throw WireError(description: "line too long") }
            let r = Darwin.read(fd, &b, 1)
            if r <= 0 { throw WireError(description: "relay closed") }
            bytes.append(b[0])
        }
        return String(decoding: bytes, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func frame(_ ch: Chan, _ payload: Data) -> Data {
        var d = Data(); var len = UInt32(payload.count + 1).bigEndian
        withUnsafeBytes(of: &len) { d.append(contentsOf: $0) }
        d.append(ch.rawValue); d.append(payload)
        return d
    }

    func send(_ msg: [String: Any]) throws {
        let t = msg["type"] as? String ?? ""
        let json = try JSONSerialization.data(withJSONObject: msg)
        try writeAll(frame(channel(forType: t), json))
    }

    func sendVideo(_ v: VideoPacket) throws { try writeAll(frame(.video, v.payload())) }

    /// Next frame: (channel, payload) or nil on EOF.
    func readFrame() throws -> (Chan, Data)? {
        guard let head = try readExact(4) else { return nil }
        let len = Int(head.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) })
        if len == 0 || len > 16 << 20 { throw WireError(description: "bad frame length \(len)") }
        guard let body = try readExact(len), let ch = Chan(rawValue: body[0]) else { throw WireError(description: "bad channel") }
        return (ch, body.dropFirst())
    }
}

struct VideoPacket {
    var windowID: UInt64, ptsMicros: UInt64, keyframe: Bool, width: UInt16, height: UInt16, data: Data
    func payload() -> Data {
        var d = Data()
        func be<T: FixedWidthInteger>(_ v: T) { var x = v.bigEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }
        be(windowID); be(ptsMicros); d.append(keyframe ? 1 : 0); d.append(1 /* H.264 */); be(width); be(height)
        d.append(data)
        return d
    }
}

func num(_ v: Any?) -> Double { (v as? NSNumber)?.doubleValue ?? 0 }
func int(_ v: Any?) -> Int { (v as? NSNumber)?.intValue ?? 0 }

/// Admission key of a relay on a public address: the environment, else the one built in.
func relayKey() -> String? {
    ProcessInfo.processInfo.environment["RM_RELAY_KEY"].flatMap({ $0.isEmpty ? nil : $0 }) ?? (builtinRelayKey.isEmpty ? nil : builtinRelayKey)
}

func joinRelay(_ conn: Conn, session: String) throws {
    // the relay sees a token made from the session only (the password is proved end to end);
    // the owner secret: a relay that handed out this ID lets only its owner wait under it
    var join: [String: Any] = ["session_id": session, "role": "agent", "token": relayToken(session), "owner": ownerSecret()]
    if let key = relayKey() { join["key"] = key }
    let line = try JSONSerialization.data(withJSONObject: join)
    try conn.writeAll(line + Data([10]))
    let reply = try conn.readLine()
    if reply != "READY" { throw WireError(description: "relay refused: \(reply)") }
}

/// Everything the agent sends goes through here, on one writer thread (the approach of
/// Sunshine/Moonlight, done for one TCP stream):
///  - control and input replies first, video after them;
///  - video never queues up: when a window has frames waiting, its pending frames are dropped
///    and it waits for a fresh IDR (requested from its encoder), so what is shown stays current;
///  - the bitrate follows the link: queueing delay or drops lower it, a clear link raises it.
final class Sender {
    private let conn: Conn
    private let cond = NSCondition()
    private var control: [Data] = []
    /// big, unhurried replies (app icons): after control, taking turns with video
    private var bulk: [Data] = []
    private var bulkTurn = false
    /// sound on the stream (UDP not alive): right after control, at most 100 ms of it queued
    private var audio: [Data] = []
    private var video: [(data: Data, window: UInt64, key: Bool, queued: CFAbsoluteTime)] = []
    private var waitingForKey: Set<UInt64> = []
    private var maxDelay: Double = 0
    private var congested = false
    private var lastAdjust = CFAbsoluteTimeGetCurrent(), lastDecrease = CFAbsoluteTimeGetCurrent()
    private(set) var dropped = 0
    private(set) var bitrate: Int
    /// lowest recent round trip (ms): the path without queues
    private var rttFloor: Double = 0
    private var rttFloorAt: CFAbsoluteTime = 0
    let minBitrate = 1_000_000, maxBitrate = 80_000_000
    /// highest bitrate the adaptation may reach: the user's setting, or maxBitrate (Auto)
    private(set) var ceiling = 80_000_000

    /// The user's bitrate (settings): start there and never go above it; nil: Auto.
    func setCeiling(_ c: Int?) {
        cond.lock()
        ceiling = c.map { max(minBitrate, min(maxBitrate, $0)) } ?? maxBitrate
        if c != nil { bitrate = ceiling } else { bitrate = min(bitrate, ceiling) }
        let b = bitrate
        cond.unlock()
        onBitrate?(b)
    }
    /// Ask a window's encoder for an IDR frame.
    var requestKeyframe: ((UInt64) -> Void)?
    var onBitrate: ((Int) -> Void)?

    init(conn: Conn, bitrate: Int = 20_000_000) {
        self.conn = conn; self.bitrate = bitrate
        let t = Thread { [weak self] in self?.run() }
        t.name = "rm.sender"; t.qualityOfService = .userInteractive
        t.start()
    }

    func send(_ msg: [String: Any]) throws {
        let json = try JSONSerialization.data(withJSONObject: msg)
        let d = conn.frame(channel(forType: msg["type"] as? String ?? ""), json)
        cond.lock(); control.append(d); cond.signal(); cond.unlock()
    }

    /// Low priority: never ahead of input replies, window events or a new window's video.
    func sendBulk(_ msg: [String: Any]) throws {
        let json = try JSONSerialization.data(withJSONObject: msg)
        let d = conn.frame(channel(forType: msg["type"] as? String ?? ""), json)
        cond.lock(); bulk.append(d); cond.signal(); cond.unlock()
    }

    /// UDP video path; used while it is alive, TCP otherwise.
    var udp: UdpLink?

    /// A packet of sound (its payload, Audio.swift): UDP while alive, else the Audio channel.
    func sendAudio(_ payload: Data) {
        if let u = udp, u.alive { u.sendAudio(payload); return }
        let d = conn.frame(.audio, payload)
        cond.lock()
        audio.append(d)
        if audio.count > 20 { audio.removeFirst(audio.count - 20) } // late sound is useless: the newest wins
        cond.signal()
        cond.unlock()
    }

    func sendVideo(_ p: VideoPacket) {
        if let u = udp, u.alive { u.sendVideo(p); return }
        var ask: UInt64?
        cond.lock()
        if waitingForKey.contains(p.windowID) && !p.keyframe {
            dropped += 1                       // decoder state is gone until the IDR: skip
        } else {
            if p.keyframe { waitingForKey.remove(p.windowID) }
            let pending = video.filter { $0.window == p.windowID }.count
            if pending >= 2 && !p.keyframe {
                // backlog: drop what is waiting for this window, start again from an IDR
                let before = video.count
                video.removeAll { $0.window == p.windowID }
                dropped += before - video.count + 1
                waitingForKey.insert(p.windowID)
                congested = true
                ask = p.windowID
            } else {
                video.append((conn.frame(.video, p.payload()), p.windowID, p.keyframe, CFAbsoluteTimeGetCurrent()))
                cond.signal()
            }
        }
        cond.unlock()
        if let w = ask { requestKeyframe?(w) }
    }

    private func run() {
        while true {
            cond.lock()
            while control.isEmpty && video.isEmpty && bulk.isEmpty && audio.isEmpty { cond.wait() }
            let item: (Data, CFAbsoluteTime?)
            if !control.isEmpty { item = (control.removeFirst(), nil) }
            else if !audio.isEmpty { item = (audio.removeFirst(), nil) }
            else if !bulk.isEmpty && (video.isEmpty || bulkTurn) { item = (bulk.removeFirst(), nil); bulkTurn = false }
            else { let v = video.removeFirst(); item = (v.data, v.queued); bulkTurn = true }
            cond.unlock()
            do { try conn.writeAll(item.0) } catch { log("send failed: \(error)"); return }
            if let q = item.1 { note(delay: CFAbsoluteTimeGetCurrent() - q) }
        }
    }

    /// UDP: the client's report (every 200 ms) and how long frames waited in the pacer drive the
    /// bitrate: loss after FEC or a growing queue cut it, a clean link lets it grow.
    /// The UDP path changed (direct <-> relay): its round trip is a new floor.
    func pathChanged() { cond.lock(); rttFloor = 0; cond.unlock() }

    func udpReport(_ r: UdpReport, wait: Double) {
        var change: Int?
        cond.lock()
        let now = CFAbsoluteTimeGetCurrent()
        // delay-based (as Google's congestion control): the round trip's floor is the path
        // itself; anything above it is queues filling (router, relay, Wi-Fi). React to that
        // before packets are lost, so video never runs seconds behind the hand
        var queued = false
        if r.rttMs > 0 {
            let rtt = Double(r.rttMs)
            if rttFloor == 0 || rtt < rttFloor { rttFloor = rtt; rttFloorAt = now }
            else if now - rttFloorAt > 10 { rttFloor = rttFloor * 0.9 + rtt * 0.1; rttFloorAt = now } // the path may have changed
            queued = rtt > rttFloor + max(40, rttFloor * 0.25)
        }
        // random loss is FEC's job (its share rises with the loss); real congestion - frames
        // lost despite FEC, a growing queue on the path or here, a very lossy link - costs bitrate
        if r.lost > 0 || r.loss > 0.15 || wait > 0.05 || queued {
            if now - lastDecrease > 0.3 { bitrate = max(minBitrate, Int(Double(bitrate) * 0.75)); lastDecrease = now; change = bitrate }
        } else if r.loss < 0.05 && wait < 0.02 && now - lastDecrease > 2 && now - lastAdjust > 0.4 && bitrate < ceiling {
            bitrate = min(ceiling, Int(Double(bitrate) * 1.06)); lastAdjust = now; change = bitrate
        }
        cond.unlock()
        if let b = change {
            udp?.paceBitsPerSecond = max(20_000_000, b * 3)
            onBitrate?(b)
        }
    }

    /// Additive-increase / multiplicative-decrease on the measured queueing delay.
    private func note(delay: Double) {
        var change: Int?
        cond.lock()
        maxDelay = max(maxDelay, delay)
        let now = CFAbsoluteTimeGetCurrent()
        if now - lastAdjust >= 0.5 {
            if congested || maxDelay > 0.12 {
                bitrate = max(minBitrate, Int(Double(bitrate) * 0.7)); lastDecrease = now; change = bitrate
            } else if maxDelay < 0.03 && now - lastDecrease > 3 && bitrate < ceiling {
                bitrate = min(ceiling, Int(Double(bitrate) * 1.12)); change = bitrate
            }
            maxDelay = 0; congested = false; lastAdjust = now
        }
        cond.unlock()
        if let b = change { onBitrate?(b) }
    }
}
