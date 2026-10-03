// Video over UDP through the relay (same host and port as TCP), as crates/rm-protocol/src/udp.rs
// defines it: each frame is cut into equal shards, grouped in blocks of <= 64, each block gets
// Reed-Solomon parity (Fec.swift). Packets leave paced (a keyframe is not one burst that
// overflows a router queue). Video uses this path only while the client's reports arrive; when
// they stop, Sender goes back to TCP by itself.
//
// Direct path (as Moonlight talks straight to the host): both sides swap their addresses (LAN,
// public via STUN) and a secret over the authenticated TCP connection ("p2p_offer"), then punch
// to each other. When a punch carrying our secret arrives, that address is the direct path and
// video, pings and the client's input go there; the relay is only the meeting point and fallback.
import Foundation

/// The agent clock (microseconds): frame pts and pong answers. Same base as capture timestamps.
func agentClockUs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1000 }

struct UdpReport { var loss: Double; var lost: Int; var recovered: Int; var frames: Int; var rttMs: Int = 0 }

/// Numeric "ip:port" of a socket address (also the key addresses are compared by).
func addrKey(_ ss: sockaddr_storage) -> String? {
    var s = ss
    var host = [CChar](repeating: 0, count: Int(NI_MAXHOST)), serv = [CChar](repeating: 0, count: Int(NI_MAXSERV))
    let len = socklen_t(s.ss_family == sa_family_t(AF_INET6) ? MemoryLayout<sockaddr_in6>.size : MemoryLayout<sockaddr_in>.size)
    let r = withUnsafePointer(to: &s) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
        getnameinfo($0, len, &host, socklen_t(host.count), &serv, socklen_t(serv.count), NI_NUMERICHOST | NI_NUMERICSERV) } }
    guard r == 0 else { return nil }
    let h = String(cString: host), p = String(cString: serv)
    return h.contains(":") ? "[\(h)]:\(p)" : "\(h):\(p)"
}

/// Socket address for "host:port" (numeric only unless `resolve`), of the given family.
func sockAddr(_ hostPort: String, family: Int32, resolve: Bool = false) -> sockaddr_storage? {
    guard let idx = hostPort.lastIndex(of: ":") else { return nil }
    var host = String(hostPort[..<idx]); let port = String(hostPort[hostPort.index(after: idx)...])
    if host.hasPrefix("[") { host = String(host.dropFirst().dropLast()) }
    var hints = addrinfo(); hints.ai_family = family; hints.ai_socktype = SOCK_DGRAM
    if !resolve { hints.ai_flags = AI_NUMERICHOST }
    var res: UnsafeMutablePointer<addrinfo>?
    guard getaddrinfo(host, port, &hints, &res) == 0, let ai = res else { return nil }
    defer { freeaddrinfo(res) }
    var ss = sockaddr_storage()
    withUnsafeMutableBytes(of: &ss) { $0.copyMemory(from: UnsafeRawBufferPointer(start: ai.pointee.ai_addr, count: Int(ai.pointee.ai_addrlen))) }
    return ss
}

/// This Mac's LAN addresses (IPv4, up, not loopback).
func lanAddresses() -> [String] {
    var out: [String] = []
    var list: UnsafeMutablePointer<ifaddrs>?
    guard getifaddrs(&list) == 0 else { return out }
    defer { freeifaddrs(list) }
    var p = list
    while let i = p {
        let f = Int32(i.pointee.ifa_flags)
        if let a = i.pointee.ifa_addr, a.pointee.sa_family == sa_family_t(AF_INET), f & IFF_UP != 0, f & IFF_LOOPBACK == 0 {
            var ss = sockaddr_storage()
            withUnsafeMutableBytes(of: &ss) { $0.copyMemory(from: UnsafeRawBufferPointer(start: a, count: MemoryLayout<sockaddr_in>.size)) }
            if let k = addrKey(ss) { out.append(String(k.dropLast(2))) } // "ip:0" -> "ip"
        }
        p = i.pointee.ifa_next
    }
    return out
}

func isPrivate(_ key: String) -> Bool {
    key.hasPrefix("10.") || key.hasPrefix("192.168.") || key.hasPrefix("127.") || key.hasPrefix("169.254.") ||
        (key.hasPrefix("172.") && (Int(key.split(separator: ".")[1]) ?? 0) >= 16 && (Int(key.split(separator: ".")[1]) ?? 0) < 32)
}

func hexString(_ b: [UInt8]) -> String { b.map { String(format: "%02x", $0) }.joined() }
func unhex(_ s: String) -> [UInt8]? {
    let c = Array(s.utf8); guard c.count % 2 == 0 else { return nil }
    var out: [UInt8] = []
    for i in stride(from: 0, to: c.count, by: 2) { guard let v = UInt8(String(decoding: c[i..<i + 2], as: UTF8.self), radix: 16) else { return nil }; out.append(v) }
    return out
}

final class UdpLink {
    static let shard = 1200, maxBlock = 64, header = 38
    private let fd: Int32
    private let register: Data
    private let relay: sockaddr_storage
    private let relayKey: String
    private let family: Int32
    // direct path (guarded by cond)
    private var secret = [UInt8](repeating: 0, count: 16)
    private var peer: (secret: [UInt8], cands: [sockaddr_storage], since: CFAbsoluteTime)?
    private var direct: (key: String, addr: sockaddr_storage)?
    private var verified: Set<String> = []
    private var lastDirect: CFAbsoluteTime = 0
    private var inputLast: UInt32 = 0
    /// the client's input that came over UDP (JSON objects, in order, each once)
    var onInput: (([String: Any]) -> Void)?
    /// our addresses are known: send them to the client (p2p_offer)
    var onOffer: ((String, [String]) -> Void)?
    /// the path changed (direct address, or nil: through the relay)
    var onPath: ((String?) -> Void)?
    private let cond = NSCondition()
    private var queue: [(window: UInt64, key: Bool, packets: [Data], queued: CFAbsoluteTime)] = []
    private var seq: [UInt64: UInt32] = [:]
    private var lastReport: CFAbsoluteTime = 0
    private var registered = false
    private(set) var fecPct = 20
    /// bits per second the pacer lets out (a few times the video bitrate)
    var paceBitsPerSecond = 60_000_000
    var onReport: ((UdpReport) -> Void)?
    /// the video path came up or went away (the encoder should send a keyframe)
    var onAlive: ((Bool) -> Void)?
    var requestKeyframe: ((UInt64) -> Void)?
    private(set) var dropped = 0, sentFrames = 0
    private var wasAlive = false

    init?(hostPort: String, session: String, token: String, key: String?) {
        guard let idx = hostPort.lastIndex(of: ":") else { return nil }
        let host = String(hostPort[..<idx]), port = String(hostPort[hostPort.index(after: idx)...])
        var hints = addrinfo(); hints.ai_family = AF_UNSPEC; hints.ai_socktype = SOCK_DGRAM
        var res: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, port, &hints, &res) == 0, let ai = res else { return nil }
        defer { freeaddrinfo(res) }
        let s = socket(ai.pointee.ai_family, SOCK_DGRAM, IPPROTO_UDP)
        guard s >= 0 else { return nil }
        family = ai.pointee.ai_family
        var rs = sockaddr_storage()
        withUnsafeMutableBytes(of: &rs) { $0.copyMemory(from: UnsafeRawBufferPointer(start: ai.pointee.ai_addr, count: Int(ai.pointee.ai_addrlen))) }
        relay = rs
        guard let rk = addrKey(rs) else { close(s); return nil }
        relayKey = rk
        var big: Int32 = 4 << 20
        setsockopt(s, SOL_SOCKET, SO_SNDBUF, &big, socklen_t(MemoryLayout<Int32>.size))
        setsockopt(s, SOL_SOCKET, SO_RCVBUF, &big, socklen_t(MemoryLayout<Int32>.size))
        var tv = timeval(tv_sec: 0, tv_usec: 50_000)
        setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        fd = s
        var r = Data([0x52, 0x4D, 1, 0 /* agent */])
        for f in [session, token, key ?? ""] {
            let b = Array(f.utf8.prefix(255)); r.append(UInt8(b.count)); r.append(contentsOf: b)
        }
        register = r
        arc4random_buf(&secret, 16)
        let rt = Thread { [weak self] in self?.receive() }
        rt.name = "rm.udp.recv"; rt.qualityOfService = .userInteractive; rt.start()
        let st = Thread { [weak self] in self?.pace() }
        st.name = "rm.udp.send"; st.qualityOfService = .userInteractive; st.start()
    }

    /// The client's reports are coming in: video may go this way.
    var alive: Bool { cond.lock(); defer { cond.unlock() }; return CFAbsoluteTimeGetCurrent() - lastReport < 1.5 }

    private func sendTo(_ d: Data, _ to: sockaddr_storage) {
        var a = to
        let len = socklen_t(a.ss_family == sa_family_t(AF_INET6) ? MemoryLayout<sockaddr_in6>.size : MemoryLayout<sockaddr_in>.size)
        _ = d.withUnsafeBytes { b in withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(fd, b.baseAddress, d.count, 0, $0, len) } } }
    }
    /// Where UDP goes now: the direct path when there is one, else the relay.
    private var dest: sockaddr_storage { cond.lock(); defer { cond.unlock() }; return direct?.addr ?? relay }
    private func raw(_ d: Data) { sendTo(d, dest) }

    /// The client's offer arrived: punch to its candidates.
    func peerOffer(secret: String, candidates: [String]) {
        guard let sec = unhex(secret), sec.count == 16 else { return }
        let c = candidates.compactMap { sockAddr($0, family: family) }
        cond.lock(); peer = (sec, c, CFAbsoluteTimeGetCurrent()); cond.unlock()
        log("direct path: client offers \(candidates.joined(separator: ", "))")
    }

    private func punch(_ sec: [UInt8], ack: Bool) -> Data { Data([0x52, 0x4D, 20, ack ? 1 : 0] + sec) }

    private func receive() {
        var buf = [UInt8](repeating: 0, count: 2048)
        var lastRegister: CFAbsoluteTime = 0, lastPunch: CFAbsoluteTime = 0
        // our public address (STUN), then the offer to the client
        let p2p = ProcessInfo.processInfo.environment["RM_NO_P2P"] == nil && family == AF_INET
        var stunTx = [UInt8](repeating: 0, count: 12); arc4random_buf(&stunTx, 12)
        let stun = p2p ? ["stun.l.google.com:19302", "stun.cloudflare.com:3478"].compactMap { sockAddr($0, family: AF_INET, resolve: true) } : []
        let started = CFAbsoluteTimeGetCurrent()
        var stunSent = 0, offered = !p2p, publicAddr: String?
        // our UDP port (the socket gets one with its first datagram)
        func localPort() -> Int {
            var ss = sockaddr_storage(); var sl = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let r = withUnsafeMutablePointer(to: &ss) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &sl) } }
            guard r == 0, let k = addrKey(ss) else { return 0 }
            return Int(k.split(separator: ":").last ?? "") ?? 0
        }
        var port = 0
        while true {
            let now = CFAbsoluteTimeGetCurrent()
            if now - lastRegister >= (registered ? 2 : 0.3) { sendTo(register, relay); lastRegister = now }
            if port == 0 { port = localPort() }
            let alive = self.alive
            if alive != wasAlive { wasAlive = alive; onAlive?(alive) }
            if !offered {
                if stunSent < 3 && now - started >= 0.3 * Double(stunSent) {
                    let req = Data([0x00, 0x01, 0x00, 0x00, 0x21, 0x12, 0xA4, 0x42] + stunTx)
                    for s in stun { sendTo(req, s) }
                    stunSent += 1
                }
                if now - started >= 1.2 && port > 0 {
                    var cands = lanAddresses().map { "\($0):\(port)" }
                    if relayKey.hasPrefix("127.") { cands.append("127.0.0.1:\(port)") }
                    if let p = publicAddr, !cands.contains(p) { cands.append(p) }
                    offered = true
                    onOffer?(hexString(secret), cands)
                    log("direct path: offering \(cands.joined(separator: ", "))")
                }
            }
            // punch to the client until a direct path answers, then keep it open; lose it after 3 s of silence
            cond.lock()
            var lost = false
            if let pr = peer, now - lastPunch >= (direct == nil ? 0.1 : 1) {
                lastPunch = now
                let trying = direct != nil || now - pr.since < 20 || Int(now - pr.since) % 10 == 0
                if trying { for a in direct.map({ [$0.addr] }) ?? pr.cands { sendTo(punch(pr.secret, ack: false), a) } }
            }
            if direct != nil && now - lastDirect > 3 { direct = nil; verified = []; lost = true }
            cond.unlock()
            if lost { log("direct path lost; back through the relay"); onPath?(nil) }

            var ss = sockaddr_storage(); var sl = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let n = withUnsafeMutablePointer(to: &ss) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, &buf, buf.count, 0, $0, &sl) } }
            guard n >= 3, let from = addrKey(ss) else { continue }
            // STUN answer: our public address
            if !offered && n >= 20 && buf[0] == 1 && buf[1] == 1 && Array(buf[8..<20]) == stunTx {
                if let a = parseStun(Array(buf[0..<n])) { publicAddr = a }
                continue
            }
            guard buf[0] == 0x52, buf[1] == 0x4D else { continue }
            if buf[2] == 20 && n >= 20 {                    // punch / punch ack
                guard Array(buf[4..<20]) == secret else { continue }
                var newPath: String?
                cond.lock()
                if buf[3] == 0, let pr = peer { sendTo(punch(pr.secret, ack: true), ss) }
                verified.insert(from)
                // an ack proves both directions work: the first address that acks (a LAN one
                // over a public one) becomes the path
                if buf[3] == 1 && (direct == nil || (direct!.key != from && isPrivate(from) && !isPrivate(direct!.key))) { direct = (from, ss); newPath = from }
                lastDirect = now
                cond.unlock()
                if let p = newPath { log("direct path to the client: \(p)"); onPath?(p) }
                continue
            }
            cond.lock(); let known = from == relayKey || verified.contains(from); if known && from != relayKey { lastDirect = now }; cond.unlock()
            guard known else { continue }
            func be32(_ o: Int) -> UInt32 { var v: UInt32 = 0; for i in o..<(o + 4) { v = (v << 8) | UInt32(buf[i]) }; return v }
            func be64(_ o: Int) -> UInt64 { var v: UInt64 = 0; for i in o..<(o + 8) { v = (v << 8) | UInt64(buf[i]) }; return v }
            switch buf[2] {
            case 2 where n >= 4 && from == relayKey:   // relay status
                registered = buf[3] != 0xFF
            case 17 where n >= 24:                     // client report
                let expected = be32(4), received = be32(8), recovered = be32(12), lost = be32(16), frames = be32(20)
                cond.lock(); lastReport = CFAbsoluteTimeGetCurrent(); cond.unlock()
                let loss = expected == 0 ? 0 : 1 - Double(min(received, expected)) / Double(expected)
                // more parity on a lossy link (Moonlight-style adaptive FEC), 10..50 %
                if expected > 0 { fecPct = Int(max(10, min(50, 10 + loss * 300))) }
                let rtt = n >= 28 ? Int(be32(24)) : 0
                onReport?(UdpReport(loss: loss, lost: Int(lost), recovered: Int(recovered), frames: Int(frames), rttMs: rtt))
            case 18 where n >= 12:                     // ping -> pong with our clock
                var d = Data([0x52, 0x4D, 19, 0])
                var t = be64(4).bigEndian, nowUs = agentClockUs().bigEndian
                withUnsafeBytes(of: &t) { d.append(contentsOf: $0) }
                withUnsafeBytes(of: &nowUs) { d.append(contentsOf: $0) }
                sendTo(d, ss)
            case 22:                                   // input from the client (reliable, in order)
                var o = 4, msgs: [[String: Any]] = []
                cond.lock()
                while o + 6 <= n {
                    let sq = be32(o), l = Int(buf[o + 4]) << 8 | Int(buf[o + 5])
                    guard o + 6 + l <= n else { break }
                    if inputLast == 0 || Int32(bitPattern: sq &- inputLast) > 0 {
                        inputLast = sq
                        if let m = try? JSONSerialization.jsonObject(with: Data(buf[(o + 6)..<(o + 6 + l)])) as? [String: Any] { msgs.append(m) }
                    }
                    o += 6 + l
                }
                var ack = Data([0x52, 0x4D, 23, 0]); var be = inputLast.bigEndian
                cond.unlock()
                withUnsafeBytes(of: &be) { ack.append(contentsOf: $0) }
                sendTo(ack, ss)
                for m in msgs { onInput?(m) }
            default: break
            }
        }
    }

    /// Cut, protect and queue one frame. Under backlog the window's waiting frames are dropped
    /// and it restarts from a keyframe (never queue seconds of video).
    func sendVideo(_ p: VideoPacket) {
        let packets = packetize(p)
        var ask = false
        cond.lock()
        let pending = queue.filter { $0.window == p.windowID }.count
        if pending >= 2 && !p.keyframe {
            let before = queue.count
            queue.removeAll { $0.window == p.windowID }
            dropped += before - queue.count + 1
            ask = true
        } else {
            queue.append((p.windowID, p.keyframe, packets, CFAbsoluteTimeGetCurrent()))
            cond.signal()
        }
        cond.unlock()
        if ask { requestKeyframe?(p.windowID) }
    }

    /// Longest time a frame waited in the pacer since the last call (seconds).
    private var maxWait: Double = 0
    func takeMaxWait() -> Double { cond.lock(); defer { cond.unlock() }; let w = maxWait; maxWait = 0; return w }

    private func pace() {
        var tokens = 0.0, last = CFAbsoluteTimeGetCurrent()
        while true {
            cond.lock()
            while queue.isEmpty { cond.wait() }
            let item = queue.removeFirst()
            maxWait = max(maxWait, CFAbsoluteTimeGetCurrent() - item.queued)
            cond.unlock()
            let to = dest
            for d in item.packets {
                let rate = Double(paceBitsPerSecond) / 8, burst = 64.0 * 1024
                let now = CFAbsoluteTimeGetCurrent()
                tokens = min(burst, tokens + (now - last) * rate); last = now
                if tokens < Double(d.count) {
                    let wait = (Double(d.count) - tokens) / rate
                    usleep(UInt32(max(50, wait * 1_000_000)))
                    let t = CFAbsoluteTimeGetCurrent(); tokens = min(burst, tokens + (t - last) * rate); last = t
                }
                tokens -= Double(d.count)
                sendTo(d, to)
            }
            sentFrames += 1
        }
    }

    /// XOR-MAPPED-ADDRESS (IPv4) of a STUN binding response, as "ip:port".
    private func parseStun(_ p: [UInt8]) -> String? {
        let end = min(p.count, 20 + (Int(p[2]) << 8 | Int(p[3])))
        var o = 20
        while o + 4 <= end {
            let t = Int(p[o]) << 8 | Int(p[o + 1]), l = Int(p[o + 2]) << 8 | Int(p[o + 3])
            guard o + 4 + l <= p.count else { return nil }
            if (t == 0x20 || t == 0x01) && l >= 8 && p[o + 5] == 1 {
                let x = t == 0x20
                let port = (Int(p[o + 6]) << 8 | Int(p[o + 7])) ^ (x ? 0x2112 : 0)
                let cookie: [UInt8] = [0x21, 0x12, 0xA4, 0x42]
                let ip = (0..<4).map { String(p[o + 8 + $0] ^ (x ? cookie[$0] : 0)) }.joined(separator: ".")
                return "\(ip):\(port)"
            }
            o += 4 + (l + 3) / 4 * 4
        }
        return nil
    }

    /// Sunshine's packet format (crates/rm-gamestream, the Moonlight protocol): RTP +
    /// NV_VIDEO_PACKET + frame header, Reed-Solomon parity in FEC blocks; the RTP SSRC is the
    /// window. Each packet goes out behind an 8-byte tag ("RM", 24, 0, width, height BE) so
    /// relays and the client's demultiplexer know it.
    static let gsPacketSize: UInt32 = 1200, gsMinFec: UInt32 = 2
    private var packetizers: [UInt64: OpaquePointer] = [:]
    private var frameIndex: [UInt64: UInt32] = [:]

    func forgetWindow(_ id: UInt64) {
        cond.lock(); let p = packetizers.removeValue(forKey: id); frameIndex.removeValue(forKey: id); cond.unlock()
        if let p = p { rm_gs_packetizer_free(p) }
    }

    private func packetize(_ p: VideoPacket) -> [Data] {
        cond.lock()
        let pz: OpaquePointer
        if let x = packetizers[p.windowID] { pz = x } else {
            pz = rm_gs_packetizer_new(Self.gsPacketSize, UInt32(fecPct), Self.gsMinFec, UInt32(truncatingIfNeeded: p.windowID))!
            packetizers[p.windowID] = pz
        }
        let index = (frameIndex[p.windowID] ?? 0) &+ 1
        frameIndex[p.windowID] = index
        rm_gs_packetizer_set_fec(pz, UInt32(fecPct))
        // 90 kHz RTP clock from the capture time; latency in 1/10 ms (Sunshine's frame header)
        let ts = UInt32(truncatingIfNeeded: p.ptsMicros &* 9 / 100)
        let now = agentClockUs()
        let latency = UInt16(min(65535, now > p.ptsMicros ? (now - p.ptsMicros) / 100 : 0))
        var count = 0
        let bytes = [UInt8](p.data)
        let ptr = bytes.withUnsafeBufferPointer { rm_gs_packetize(pz, $0.baseAddress, $0.count, index, p.keyframe, ts, latency, &count) }
        cond.unlock()
        guard let base = ptr else { return [] }
        let size = Int(Self.gsPacketSize) + 16
        var out: [Data] = []
        out.reserveCapacity(count)
        for i in 0..<count {
            var d = Data([0x52, 0x4D, 24, 0, UInt8(p.width >> 8), UInt8(p.width & 0xff), UInt8(p.height >> 8), UInt8(p.height & 0xff)])
            d.append(base + i * size, count: size)
            out.append(d)
        }
        rm_gs_free(base, count * size)
        return out
    }
}
