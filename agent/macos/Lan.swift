// Reachable on the local network without a relay (crates/rm-relay/src/lan.rs is the viewer's
// side): a broadcast "RMLAN?<session>" on UDP 7471 is answered with "RMLAN!<session> <tcp port>",
// and that TCP port takes the same join line a relay does. After "READY" the end-to-end
// handshake proves the password, as through a relay. The same TCP port takes a viewer that
// typed this Mac's address (IPv4 or IPv6, any network that reaches it): `--port` fixes it.
import Foundation

let lanPort: UInt16 = 7471
/// The TCP port viewers join on (`--port`, RM_PORT; 7471 by default, any free one if taken).
var directPort: UInt16 = UInt16(ProcessInfo.processInfo.environment["RM_PORT"] ?? "") ?? lanPort

final class LanListener {
    private let session: String
    private let udp: Int32
    private let tcp: Int32
    let tcpPort: UInt16
    private var closed = false

    init?(session: String) {
        self.session = session
        func sock(_ type: Int32) -> Int32 {
            let fd = socket(AF_INET, type, 0)
            guard fd >= 0 else { return -1 }
            var one: Int32 = 1
            setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
            setsockopt(fd, SOL_SOCKET, SO_REUSEPORT, &one, socklen_t(MemoryLayout<Int32>.size))
            _ = fcntl(fd, F_SETFD, FD_CLOEXEC) // not inherited by the restart for the next client
            return fd
        }
        func bindTo(_ fd: Int32, _ port: UInt16) -> Bool {
            var a = sockaddr_in()
            a.sin_family = sa_family_t(AF_INET); a.sin_port = port.bigEndian; a.sin_addr.s_addr = INADDR_ANY
            return withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } } == 0
        }
        /// TCP on IPv6 and IPv4 at once (a viewer may type either address); IPv4 only when the
        /// Mac has no IPv6
        func bindDual(_ fd: Int32, _ port: UInt16) -> Bool {
            var off: Int32 = 0
            setsockopt(fd, IPPROTO_IPV6, IPV6_V6ONLY, &off, socklen_t(MemoryLayout<Int32>.size))
            var a = sockaddr_in6()
            a.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
            a.sin6_family = sa_family_t(AF_INET6); a.sin6_port = port.bigEndian; a.sin6_addr = in6addr_any
            return withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in6>.size)) } } == 0
        }
        let u = sock(SOCK_DGRAM)
        var t: Int32 = -1
        var dual = false
        let t6 = socket(AF_INET6, SOCK_STREAM, 0)
        if t6 >= 0 {
            var one: Int32 = 1
            setsockopt(t6, SOL_SOCKET, SO_REUSEADDR, &one, socklen_t(MemoryLayout<Int32>.size))
            _ = fcntl(t6, F_SETFD, FD_CLOEXEC)
            if bindDual(t6, directPort) || bindDual(t6, 0) { t = t6; dual = true } else { Darwin.close(t6) }
        }
        if t < 0 { t = sock(SOCK_STREAM); if t >= 0 && !(bindTo(t, directPort) || bindTo(t, 0)) { Darwin.close(t); t = -1 } }
        let tt = t
        func giveUp() { if u >= 0 { Darwin.close(u) }; if tt >= 0 { Darwin.close(tt) } }
        guard u >= 0, tt >= 0, bindTo(u, lanPort) else {
            log("LAN: discovery port \(lanPort) unavailable; this Mac is reached through the relay only"); giveUp(); return nil
        }
        guard listen(tt, 4) == 0 else { giveUp(); return nil }
        var ss = sockaddr_storage(); var len = socklen_t(MemoryLayout<sockaddr_storage>.size)
        _ = withUnsafeMutablePointer(to: &ss) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(tt, $0, &len) } }
        udp = u; tcp = tt
        tcpPort = UInt16(Int(addrKey(ss)?.split(separator: ":").last ?? "") ?? 0)
        log("LAN: TCP port \(tcpPort) (\(dual ? "IPv4 and IPv6" : "IPv4"))")
        Thread { [self] in answerQueries() }.start()
    }

    /// Answer "where is <session>?" with our TCP port.
    private func answerQueries() {
        var buf = [UInt8](repeating: 0, count: 256)
        let reply = Array("RMLAN!\(session) \(tcpPort)".utf8)
        while !closed {
            var from = sockaddr_storage(); var flen = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let n = withUnsafeMutablePointer(to: &from) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(udp, &buf, buf.count, 0, $0, &flen) } }
            if n <= 0 { if closed { return }; continue }
            guard String(decoding: buf[0..<n], as: UTF8.self) == "RMLAN?\(session)" else { continue }
            _ = withUnsafePointer(to: &from) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { sendto(udp, reply, reply.count, 0, $0, flen) } }
        }
    }

    /// The next viewer asking for this session. nil once closed.
    func accept(token: String) -> Conn? {
        while !closed {
            var from = sockaddr_storage(); var flen = socklen_t(MemoryLayout<sockaddr_storage>.size)
            let fd = withUnsafeMutablePointer(to: &from) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.accept(tcp, $0, &flen) } }
            if fd < 0 { if closed { return nil }; continue }
            var tv = timeval(tv_sec: 10, tv_usec: 0)
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
            _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
            let c = Conn(fd: fd)
            guard let line = try? c.readLine(maxLen: 512),
                  let j = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] else { Darwin.close(fd); continue }
            // the password is proved in the end-to-end handshake that follows (Secure.swift)
            guard j["session_id"] as? String == session, j["token"] as? String == token else {
                try? c.writeAll(Data("ERR no such session\n".utf8)); Darwin.close(fd); continue
            }
            tv = timeval(tv_sec: 0, tv_usec: 0)
            setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
            var one: Int32 = 1
            setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, socklen_t(MemoryLayout<Int32>.size))
            do { try c.writeAll(Data("READY\n".utf8)) } catch { Darwin.close(fd); continue }
            return c
        }
        return nil
    }

    func close() {
        closed = true
        Darwin.shutdown(tcp, SHUT_RDWR); Darwin.close(tcp)
        Darwin.shutdown(udp, SHUT_RDWR); Darwin.close(udp)
    }
}
