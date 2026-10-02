// Video over UDP through the relay (same host and port as TCP), as crates/rm-protocol/src/udp.rs
// defines it: each frame is cut into equal shards, grouped in blocks of <= 64, each block gets
// Reed-Solomon parity (Fec.swift). Packets leave paced (a keyframe is not one burst that
// overflows a router queue). Video uses this path only while the client's reports arrive; when
// they stop, Sender goes back to TCP by itself.
import Foundation

/// The agent clock (microseconds): frame pts and pong answers. Same base as capture timestamps.
func agentClockUs() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1000 }

struct UdpReport { var loss: Double; var lost: Int; var recovered: Int; var frames: Int }

final class UdpLink {
    static let shard = 1200, maxBlock = 64, header = 38
    private let fd: Int32
    private let register: Data
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
        guard s >= 0, Darwin.connect(s, ai.pointee.ai_addr, ai.pointee.ai_addrlen) == 0 else { if s >= 0 { close(s) }; return nil }
        var big: Int32 = 4 << 20
        setsockopt(s, SOL_SOCKET, SO_SNDBUF, &big, socklen_t(MemoryLayout<Int32>.size))
        var tv = timeval(tv_sec: 0, tv_usec: 50_000)
        setsockopt(s, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        fd = s
        var r = Data([0x52, 0x4D, 1, 0 /* agent */])
        for f in [session, token, key ?? ""] {
            let b = Array(f.utf8.prefix(255)); r.append(UInt8(b.count)); r.append(contentsOf: b)
        }
        register = r
        let rt = Thread { [weak self] in self?.receive() }
        rt.name = "rm.udp.recv"; rt.qualityOfService = .userInteractive; rt.start()
        let st = Thread { [weak self] in self?.pace() }
        st.name = "rm.udp.send"; st.qualityOfService = .userInteractive; st.start()
    }

    /// The client's reports are coming in: video may go this way.
    var alive: Bool { cond.lock(); defer { cond.unlock() }; return CFAbsoluteTimeGetCurrent() - lastReport < 1.5 }

    private func raw(_ d: Data) { _ = d.withUnsafeBytes { Darwin.send(fd, $0.baseAddress, d.count, 0) } }

    private func receive() {
        var buf = [UInt8](repeating: 0, count: 2048)
        var lastRegister: CFAbsoluteTime = 0
        while true {
            let now = CFAbsoluteTimeGetCurrent()
            if now - lastRegister >= (registered ? 2 : 0.3) { raw(register); lastRegister = now }
            let alive = self.alive
            if alive != wasAlive { wasAlive = alive; onAlive?(alive) }
            let n = Darwin.recv(fd, &buf, buf.count, 0)
            guard n >= 3, buf[0] == 0x52, buf[1] == 0x4D else { continue }
            func be32(_ o: Int) -> UInt32 { buf[o..<o + 4].reduce(0) { $0 << 8 | UInt32($1) } }
            func be64(_ o: Int) -> UInt64 { buf[o..<o + 8].reduce(0) { $0 << 8 | UInt64($1) } }
            switch buf[2] {
            case 2 where n >= 4:                       // relay status
                registered = buf[3] != 0xFF
            case 17 where n >= 24:                     // client report
                let expected = be32(4), received = be32(8), recovered = be32(12), lost = be32(16), frames = be32(20)
                cond.lock(); lastReport = CFAbsoluteTimeGetCurrent(); cond.unlock()
                let loss = expected == 0 ? 0 : 1 - Double(min(received, expected)) / Double(expected)
                // more parity on a lossy link (Moonlight-style adaptive FEC), 10..50 %
                if expected > 0 { fecPct = Int(max(10, min(50, 10 + loss * 300))) }
                onReport?(UdpReport(loss: loss, lost: Int(lost), recovered: Int(recovered), frames: Int(frames)))
            case 18 where n >= 12:                     // ping -> pong with our clock
                var d = Data([0x52, 0x4D, 19, 0])
                var t = be64(4).bigEndian, nowUs = agentClockUs().bigEndian
                withUnsafeBytes(of: &t) { d.append(contentsOf: $0) }
                withUnsafeBytes(of: &nowUs) { d.append(contentsOf: $0) }
                raw(d)
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
                raw(d)
            }
            sentFrames += 1
        }
    }

    private func packetize(_ p: VideoPacket) -> [Data] {
        cond.lock(); let s = (seq[p.windowID] ?? 0) &+ 1; seq[p.windowID] = s; cond.unlock()
        let bytes = [UInt8](p.data), len = bytes.count
        let n = max(1, (len + Self.shard - 1) / Self.shard)
        let size = max(1, (len + n - 1) / n)
        let blocks = (n + Self.maxBlock - 1) / Self.maxBlock
        var out: [Data] = []
        var head = Data()
        func be<T: FixedWidthInteger>(_ v: T) { var x = v.bigEndian; withUnsafeBytes(of: &x) { head.append(contentsOf: $0) } }
        head.append(contentsOf: [0x52, 0x4D, 16, p.keyframe ? 1 : 0])
        be(p.windowID); be(s); be(p.ptsMicros); be(p.width); be(p.height); be(UInt32(len))
        for b in 0..<blocks {
            let first = b * Self.maxBlock, k = min(Self.maxBlock, n - first)
            let m = fecPct == 0 ? 0 : max(1, min(255 - k, (k * fecPct + 99) / 100))
            let shards: [[UInt8]] = (0..<k).map { j in
                let start = min(len, (first + j) * size), end = min(len, start + size)
                var sh = Array(bytes[start..<end]); if sh.count < size { sh += [UInt8](repeating: 0, count: size - sh.count) }
                return sh
            }
            let parity = fecEncode(shards, m: m)
            for (i, sh) in (shards + parity).enumerated() {
                var d = head
                d.append(contentsOf: [UInt8(b), UInt8(blocks), UInt8(i), UInt8(k), UInt8(m), 0])
                d.append(contentsOf: sh)
                out.append(d)
            }
        }
        return out
    }
}
