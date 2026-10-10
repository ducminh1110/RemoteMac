// End-to-end encryption with the viewer, byte for byte as crates/rm-protocol/src/secure.rs
// (see there for the full description): after READY, a password-authenticated key exchange on
// P-256 (x-coordinates only, CryptoKit's ECDH), then ChaCha20-Poly1305 on the stream (records)
// and on the UDP datagrams. The relay only ever carries ciphertext.
import Foundation
import CryptoKit

enum SecureError: Error, CustomStringConvertible {
    case wrongPassword, locked, failed(String)
    var description: String {
        switch self {
        case .wrongPassword: return "wrong password"
        case .locked: return "locked: too many wrong passwords"
        case .failed(let s): return "secure handshake: \(s)"
        }
    }
}

/// What the relay (and our local network port) is shown to pair: made from the session name
/// only, it tells nothing about the password.
func relayToken(_ session: String) -> String {
    let d = SHA256.hash(data: Data("remotemac/v2/relay:\(session)".utf8))
    return String(d.map { String(format: "%02x", $0) }.joined().prefix(48))
}

/// The curve point both sides make from the session and its secret.
func secureGenerator(session: String, secret: String) -> P256.KeyAgreement.PublicKey {
    for ctr in 0...255 {
        var h = SHA256()
        h.update(data: Data("remotemac/v2/G".utf8))
        for f in [Data(session.utf8), Data(secret.utf8)] {
            h.update(data: Data([UInt8(f.count >> 8), UInt8(f.count & 0xff)]))
            h.update(data: f)
        }
        h.update(data: Data([UInt8(ctr)]))
        if let p = try? P256.KeyAgreement.PublicKey(compressedRepresentation: Data([2]) + Data(h.finalize())) { return p }
    }
    fatalError("about half of all x-coordinates are on the curve")
}

/// x-coordinate of k·p.
private func xOnly(_ k: P256.KeyAgreement.PrivateKey, _ p: P256.KeyAgreement.PublicKey) throws -> Data {
    try k.sharedSecretFromKeyAgreement(with: p).withUnsafeBytes { Data($0) }
}

private func lift(_ x: Data) throws -> P256.KeyAgreement.PublicKey {
    do { return try P256.KeyAgreement.PublicKey(compressedRepresentation: Data([2]) + x) } catch { throw SecureError.failed("the viewer sent a bad point") }
}

struct SessionKeys {
    let streamTx: SymmetricKey, streamRx: SymmetricKey, udpTx: SymmetricKey, udpRx: SymmetricKey
}

/// The Mac's side of the handshake on a fresh connection. `locked`: refuse at once.
func agentHandshake(_ c: Conn, session: String, secret: String, locked: Bool) throws -> SessionKeys {
    var tv = timeval(tv_sec: 10, tv_usec: 0)
    setsockopt(c.fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
    defer {
        var none = timeval(tv_sec: 0, tv_usec: 0)
        setsockopt(c.fd, SOL_SOCKET, SO_RCVTIMEO, &none, socklen_t(MemoryLayout<timeval>.size))
    }
    if locked {
        try c.writeAll(Data("RMKL".utf8) + Data(count: 32))
        throw SecureError.locked
    }
    let g = secureGenerator(session: session, secret: secret)
    let y = P256.KeyAgreement.PrivateKey()
    let ym = try xOnly(y, g)
    try c.writeAll(Data("RMK2".utf8) + ym)
    guard let reply = try c.readExact(64) else { throw SecureError.failed("the viewer left") }
    let yc = Data(reply.prefix(32)), tc = Data(reply.suffix(32))
    let k = try xOnly(y, try lift(yc))
    let okm = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: k), salt: Data(session.utf8),
                                     info: Data("remotemac/v2 keys".utf8) + ym + yc, outputByteCount: 160)
    let b = okm.withUnsafeBytes { Data($0) }
    let part = { (i: Int) in SymmetricKey(data: Data(b[(i * 32)..<(i * 32 + 32)])) }
    let kc = part(0)
    guard HMAC<SHA256>.isValidAuthenticationCode(tc, authenticating: Data("client".utf8) + ym + yc, using: kc) else {
        try? c.writeAll(Data([0]))
        throw SecureError.wrongPassword
    }
    let ta = Data(HMAC<SHA256>.authenticationCode(for: Data("agent".utf8) + ym + yc, using: kc))
    try c.writeAll(Data([1]) + ta)
    return SessionKeys(streamTx: part(1), streamRx: part(2), udpTx: part(3), udpRx: part(4))
}

func secureNonce(_ counter: UInt64) -> ChaChaPoly.Nonce {
    var b = [UInt8](repeating: 0, count: 12)
    for i in 0..<8 { b[4 + i] = UInt8((counter >> (56 - 8 * UInt64(i))) & 0xff) }
    return try! ChaChaPoly.Nonce(data: b)
}

/// The cipher states of the stream (one per direction).
final class StreamCipher {
    let tx: SymmetricKey, rx: SymmetricKey
    var txCounter: UInt64 = 0, rxCounter: UInt64 = 0
    init(_ k: SessionKeys) { tx = k.streamTx; rx = k.streamRx }
}

/// Encryption of the UDP datagrams of a session (types 16 and up, but the hole punch).
final class DatagramCipher {
    private let tx: SymmetricKey, rx: SymmetricKey
    private let lock = NSLock()
    private var seq: UInt64 = 0
    init(_ k: SessionKeys) { tx = k.udpTx; rx = k.udpRx }

    static func sealed(_ t: UInt8) -> Bool { t >= 16 && t != 20 }

    func seal(_ d: Data) -> Data {
        let b = [UInt8](d)
        guard b.count >= 3, b[0] == 0x52, b[1] == 0x4D, DatagramCipher.sealed(b[2]) else { return d }
        lock.lock(); let s = seq; seq += 1; lock.unlock()
        let head = Data(b[0..<3])
        guard let box = try? ChaChaPoly.seal(Data(b[3...]), using: tx, nonce: secureNonce(s), authenticating: head) else { return d }
        var o = head
        for i in 0..<8 { o.append(UInt8((s >> (56 - 8 * UInt64(i))) & 0xff)) }
        o.append(box.ciphertext); o.append(box.tag)
        return o
    }

    /// nil: forged or damaged. Types that are not encrypted pass as they are.
    func open(_ b: [UInt8]) -> [UInt8]? {
        guard b.count >= 3, b[0] == 0x52, b[1] == 0x4D, DatagramCipher.sealed(b[2]) else { return b }
        guard b.count >= 11 + 16 else { return nil }
        var s: UInt64 = 0
        for i in 3..<11 { s = (s << 8) | UInt64(b[i]) }
        guard let box = try? ChaChaPoly.SealedBox(nonce: secureNonce(s), ciphertext: Data(b[11..<(b.count - 16)]), tag: Data(b[(b.count - 16)...])),
              let pt = try? ChaChaPoly.open(box, using: rx, authenticating: Data(b[0..<3])) else { return nil }
        return Array(b[0..<3]) + [UInt8](pt)
    }
}

/// The values shared with crates/rm-protocol/src/secure.rs (its `shared_vectors` test).
func secureSelfTest() -> Bool {
    let g = secureGenerator(session: "rm-123456789", secret: "3a6365467c85f122da38bf3b7192b081049bbf94ace2a9e0")
    let hex = g.compressedRepresentation.map { String(format: "%02x", $0) }.joined()
    return hex == "0257182b0a31970df563ea4f030bab772b0b852eccaf68d3b651d737058a8b05f8"
        && relayToken("rm-123456789") == "9ad6e704b2c652cfafac52f6da98942bdb9fbb428b2894fc"
        && directToken(password: "s3cret") == "5ac160a7467369b58d0ac10dc876745f4d9163fc3fdb4815"
}
