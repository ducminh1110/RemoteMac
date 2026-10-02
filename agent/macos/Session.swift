// Connecting by ID + password: the Mac shows "ID session to connect" and a password; the
// Windows client types both. The relay pairs by the ID, both sides derive the session token from
// ID + password (identical to rm-protocol's `session::token`, same test vector).
import Foundation
import CryptoKit

let defaultRelay = "remotemac.mooo.com:7470"

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
