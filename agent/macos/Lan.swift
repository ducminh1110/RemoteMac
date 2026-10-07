// Reachable on the local network without a relay (crates/rm-relay/src/lan.rs is the viewer's
// side): a broadcast "RMLAN?<session>" on UDP 7471 is answered with "RMLAN!<session> <tcp port>",
// and that TCP port takes the same join line a relay does; the token (hash of ID and password)
// is checked here. After "READY" the stream is the session, as one paired through a relay.
import Foundation

let lanPort: UInt16 = 7471

final class LanListener {
    private let session: String
    private let udp: Int32
    private let tcp: Int32
    let tcpPort: UInt16
    private var closed = false
    private var failures = 0
    private var lockedUntil = Date.distantPast

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
        let u = sock(SOCK_DGRAM), t = sock(SOCK_STREAM)
        func giveUp() { if u >= 0 { Darwin.close(u) }; if t >= 0 { Darwin.close(t) } }
        guard u >= 0, t >= 0, bindTo(u, lanPort) else {
            log("LAN: discovery port \(lanPort) unavailable; this Mac is reached through the relay only"); giveUp(); return nil
        }
        // the TCP port: 7471 as well when free, else any
        guard bindTo(t, lanPort) || bindTo(t, 0), listen(t, 4) == 0 else { giveUp(); return nil }
        var a = sockaddr_in(); var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(t, $0, &len) } }
        udp = u; tcp = t
        tcpPort = UInt16(bigEndian: a.sin_port)
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

    /// The next viewer that knows the password (others are turned away; after 5 wrong ones in a
    /// row, everyone for a minute). nil once closed.
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
            if Date() < lockedUntil {
                try? c.writeAll(Data("ERR locked, try again in a minute\n".utf8)); Darwin.close(fd); continue
            }
            guard j["session_id"] as? String == session, j["token"] as? String == token else {
                failures += 1
                if failures >= 5 { failures = 0; lockedUntil = Date().addingTimeInterval(60) }
                log("LAN: a viewer gave a wrong password")
                try? c.writeAll(Data("ERR wrong password\n".utf8)); Darwin.close(fd); continue
            }
            failures = 0
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
