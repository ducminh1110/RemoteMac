// Connecting by ID + password: the Mac shows "ID session to connect" and a password; the
// Windows client types both. The relay pairs by the ID, both sides derive the session token from
// ID + password (identical to rm-protocol's `session::token`, same test vector).
import Foundation
import CryptoKit

/// MacBridge's version (the same as the Windows app's, Cargo.toml).
let appVersion = "1.0.2"

/// The relay used when none is given: built in by release builds, none from source.
let defaultRelay: String? = builtinRelay.trimmingCharacters(in: .whitespaces).isEmpty ? nil : builtinRelay

func sessionToken(id: String, password: String) -> String {
    let digest = SHA256.hash(data: Data("remotemac/v1:\(id):\(password)".utf8))
    return String(digest.map { String(format: "%02x", $0) }.joined().prefix(48))
}

func relaySession(id: String) -> String { "rm-\(id)" }

func displayID(_ id: String) -> String {
    stride(from: 0, to: id.count, by: 3).map { i -> String in
        let s = id.index(id.startIndex, offsetBy: i), e = id.index(s, offsetBy: min(3, id.count - i))
        return String(id[s..<e])
    }.joined(separator: " ")
}

/// This Mac's ID: kept across runs (like a phone number), 9 digits.
func persistentID() -> String {
    let dir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RemoteMac", isDirectory: true)
    let file = dir.appendingPathComponent("id")
    if let s = try? String(contentsOf: file, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines),
       s.count == 9, s.allSatisfy(\.isNumber) { return s }
    let id = String(format: "%03d%03d%03d", Int.random(in: 100...999), Int.random(in: 0...999), Int.random(in: 0...999))
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    try? id.write(to: file, atomically: true, encoding: .utf8)
    return id
}

private var supportDir: URL {
    FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/RemoteMac", isDirectory: true)
}

/// This Mac's owner secret: random, kept here, shown to nobody. A relay gives an ID to the first
/// owner asking for it and lets only that owner wait under it.
func ownerSecret() -> String {
    let file = supportDir.appendingPathComponent("owner")
    if let s = try? String(contentsOf: file, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines),
       s.count >= 32, s.allSatisfy(\.isHexDigit) { return s }
    var b = [UInt8](repeating: 0, count: 24); arc4random_buf(&b, b.count)
    let s = b.map { String(format: "%02x", $0) }.joined()
    try? FileManager.default.createDirectory(at: supportDir, withIntermediateDirectories: true)
    try? s.write(to: file, atomically: true, encoding: .utf8)
    chmod(file.path, 0o600)
    return s
}

/// The ID each relay gave this Mac (relay address -> ID), kept so it stays the same.
private var relayIDsFile: URL { supportDir.appendingPathComponent("relay-ids.json") }
func savedRelayID(_ relay: String) -> String? {
    guard let d = try? Data(contentsOf: relayIDsFile), let m = try? JSONSerialization.jsonObject(with: d) as? [String: String] else { return nil }
    return m[relay]
}
func saveRelayID(_ relay: String, _ id: String) {
    var m = (try? Data(contentsOf: relayIDsFile)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: String] } ?? [:]
    m[relay] = id
    try? FileManager.default.createDirectory(at: supportDir, withIntermediateDirectories: true)
    if let d = try? JSONSerialization.data(withJSONObject: m) { try? d.write(to: relayIDsFile, options: .atomic) }
}

/// Ask `relay` for this Mac's ID (the one it gave before, when it still can). nil: the relay
/// is unreachable, or too old to hand out IDs.
func claimID(relay: String) -> String? {
    for attempt in 0..<3 {
        if attempt > 0 { sleep(2) }
        guard let c = try? Conn.connect(hostPort: relay) else { continue }
        defer { close(c.fd) }
        var tv = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(c.fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var ask: [String: Any] = ["claim_id": ownerSecret()]
        if let w = savedRelayID(relay) { ask["want"] = w }
        if let k = relayKey() { ask["key"] = k }
        guard let line = try? JSONSerialization.data(withJSONObject: ask), (try? c.writeAll(line + Data([10]))) != nil,
              let reply = try? c.readLine() else { continue }
        let id = String(reply.dropFirst(3))
        if reply.hasPrefix("ID "), id.count == 9, id.allSatisfy(\.isNumber) {
            saveRelayID(relay, id)
            return id
        }
        log("relay \(relay) gave no ID (\(reply))")
        return nil
    }
    log("relay \(relay) not reachable for an ID")
    return nil
}

func randomPassword() -> String {
    let alphabet = Array("abcdefghjkmnpqrstuvwxyz23456789")
    return String((0..<8).map { _ in alphabet.randomElement()! })
}

/// After a session ends (or the relay gave up waiting), start over as a fresh process waiting for
/// the next client, with the same ID and password.
func restartForNextClient(after seconds: UInt32 = 1) -> Never {
    sleep(seconds)
    // the binary itself (argv[0] may be a bare name found through PATH)
    let path = Bundle.main.executablePath ?? CommandLine.arguments[0]
    var args = CommandLine.arguments.map { strdup($0) }
    args.append(nil)
    execv(path, &args)
    fail("restart failed: errno \(errno)")
}
