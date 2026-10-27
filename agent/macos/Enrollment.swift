// Taro Enterprise: this Mac enrolled on the provider's server (enterprise/docs/architecture/FLEET.md).
// The app (EnterpriseApp.swift) enrolls the Mac with a code made in the console, then keeps a
// heartbeat; the server answers with the session password, its epoch and the relay. The token
// is kept in the Keychain (never logged, never in a file); enterprise.json in the support
// directory holds what is not secret (the server, the agent's id, the epoch applied).
import Foundation
import Security
import AppKit

/// Where this Mac is enrolled.
struct Enrollment {
    /// the server's origin: https://taro.example.com (no path)
    let server: String
    let agentID: String
    /// `tag_<id>.<secret>`: only in the Keychain and the Authorization header
    let token: String
}

// ---- the provider's settings the app reads (besides those Provider.swift checks) ------------

/// provider.json as JSON (nil without a provider folder).
private func providerJSON() -> [String: Any]? {
    guard let dir = providerDirectory, let d = FileManager.default.contents(atPath: dir.appendingPathComponent("provider.json").path) else { return nil }
    return (try? JSONSerialization.jsonObject(with: d)) as? [String: Any]
}

/// account.portal: the server the setup window offers (the user may change it).
func providerPortal() -> String? {
    (providerJSON()?["account"] as? [String: Any])?["portal"] as? String
}

/// branding.primary_color (#RRGGBB) as a colour, nil when there is none.
func providerColor() -> NSColor? {
    guard let s = (providerJSON()?["branding"] as? [String: Any])?["primary_color"] as? String else { return nil }
    let hex = s.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "#", with: "")
    guard hex.count == 6, let v = UInt32(hex, radix: 16) else { return nil }
    return NSColor(srgbRed: CGFloat((v >> 16) & 0xff) / 255, green: CGFloat((v >> 8) & 0xff) / 255, blue: CGFloat(v & 0xff) / 255, alpha: 1)
}

// ---- what is typed ---------------------------------------------------------------------------

/// The server's origin from what was typed ("taro.example.com", "https://taro.example.com/"):
/// https only (http for this computer, for development), no path, query or user. nil: not one.
func normalizedServer(_ typed: String) -> String? {
    var s = typed.trimmingCharacters(in: .whitespacesAndNewlines)
    while s.hasSuffix("/") { s.removeLast() }
    if !s.contains("://") { s = "https://" + s }
    guard let c = URLComponents(string: s), let scheme = c.scheme?.lowercased(), let host = c.host?.lowercased(), !host.isEmpty,
          c.path.isEmpty, c.query == nil, c.fragment == nil, c.user == nil, c.password == nil else { return nil }
    let local = ["localhost", "127.0.0.1", "::1", "[::1]"].contains(host)
    guard scheme == "https" || (scheme == "http" && local) else { return nil }
    let h = host.contains(":") && !host.hasPrefix("[") ? "[\(host)]" : host
    return "\(scheme)://\(h)\(c.port.map { ":\($0)" } ?? "")"
}

/// An enrollment code as the console shows it (ABCD-EFGH-JKLM), from what was typed (any case,
/// with or without dashes or spaces). nil: not 12 letters and digits.
func normalizedEnrollmentCode(_ typed: String) -> String? {
    let t = typed.trimmingCharacters(in: .whitespacesAndNewlines)
    let chars = t.uppercased().filter { $0.isASCII && ($0.isLetter || $0.isNumber) }
    guard chars.count == 12, t.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == " ") }) else { return nil }
    let a = Array(chars)
    return "\(String(a[0..<4]))-\(String(a[4..<8]))-\(String(a[8..<12]))"
}

// ---- the Mac, as the console lists it ----------------------------------------------------------

/// A sysctl string ("hw.model", "machdep.cpu.brand_string"); empty when there is none.
func sysctlString(_ name: String) -> String {
    var size = 0
    guard sysctlbyname(name, nil, &size, nil, 0) == 0, size > 0 else { return "" }
    var buf = [CChar](repeating: 0, count: size + 1)
    guard sysctlbyname(name, &buf, &size, nil, 0) == 0 else { return "" }
    return String(cString: buf).trimmingCharacters(in: .whitespacesAndNewlines)
}

/// The enroll call's facts about this Mac (FLEET.md: name, model, chip, cpu_cores, memory_gb,
/// storage_gb, macos, version).
func machineFacts() -> [String: Any] {
    let pi = ProcessInfo.processInfo
    var name = Host.current().localizedName ?? ""
    if name.isEmpty { name = pi.hostName }
    let memory = Int((Double(pi.physicalMemory) / 1_073_741_824).rounded())
    // (disks are sold in decimal gigabytes: a "512 GB" Mac says 512 here)
    let capacity = (try? URL(fileURLWithPath: "/").resourceValues(forKeys: [.volumeTotalCapacityKey]))?.volumeTotalCapacity ?? 0
    let storage = Int((Double(capacity) / 1_000_000_000).rounded())
    return ["name": name, "model": sysctlString("hw.model"), "chip": sysctlString("machdep.cpu.brand_string"),
            "cpu_cores": pi.processorCount, "memory_gb": memory, "storage_gb": storage,
            "macos": pi.operatingSystemVersionString, "version": appVersion]
}

// ---- kept on this Mac ----------------------------------------------------------------------------

private let keychainService = "com.taro.enterprise"
private let keychainAccount = "enrollment"
private var keychainQuery: [String: Any] {
    [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: keychainService, kSecAttrAccount as String: keychainAccount]
}
private var enterpriseFile: URL { supportDirectory.appendingPathComponent("enterprise.json") }

/// The enrollment in the Keychain (server, agent id and token), if this Mac is enrolled.
func loadEnrollment() -> Enrollment? {
    var q = keychainQuery
    q[kSecReturnData as String] = true
    q[kSecMatchLimit as String] = kSecMatchLimitOne
    var out: CFTypeRef?
    guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let d = out as? Data,
          let m = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any],
          let server = m["server"] as? String, let id = m["agent_id"] as? String, let token = m["token"] as? String,
          !server.isEmpty, !id.isEmpty, !token.isEmpty else { return nil }
    return Enrollment(server: server, agentID: id, token: token)
}

/// Keep the enrollment: the Keychain (with the token) and enterprise.json (without it).
func saveEnrollment(_ e: Enrollment) -> Bool {
    guard let d = try? JSONSerialization.data(withJSONObject: ["server": e.server, "agent_id": e.agentID, "token": e.token]) else { return false }
    SecItemDelete(keychainQuery as CFDictionary)
    var add = keychainQuery
    add[kSecValueData as String] = d
    add[kSecAttrLabel as String] = "\(productName) Enterprise enrollment"
    add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
    let rc = SecItemAdd(add as CFDictionary, nil)
    guard rc == errSecSuccess else { log("enterprise: the Keychain did not keep the enrollment (\(rc))"); return false }
    saveAppliedEpoch(0, for: e)
    return true
}

/// Forget the enrollment here (the server keeps its record until an administrator removes it).
func forgetEnrollment() {
    SecItemDelete(keychainQuery as CFDictionary)
    try? FileManager.default.removeItem(at: enterpriseFile)
}

/// The epoch whose password the session runs with (enterprise.json; 0 when none was applied).
func loadAppliedEpoch(for e: Enrollment) -> Int {
    guard let d = try? Data(contentsOf: enterpriseFile), let m = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any],
          m["agent_id"] as? String == e.agentID, let n = m["applied_epoch"] as? NSNumber else { return 0 }
    return n.intValue
}

func saveAppliedEpoch(_ epoch: Int, for e: Enrollment) {
    let m: [String: Any] = ["server": e.server, "agent_id": e.agentID, "applied_epoch": epoch]
    try? FileManager.default.createDirectory(at: supportDirectory, withIntermediateDirectories: true)
    if let d = try? JSONSerialization.data(withJSONObject: m, options: [.prettyPrinted, .sortedKeys]) { try? d.write(to: enterpriseFile, options: .atomic) }
}

// ---- the server's API ----------------------------------------------------------------------------

/// How a call to the server went.
enum FleetAnswer {
    case ok([String: Any])
    /// 401: the token is not (or no longer) valid: the Mac was removed
    case unauthorized
    /// the server said no (its message, for the user)
    case refused(status: Int, message: String)
    /// no answer (no network, the server down)
    case unreachable(String)
}

/// POST `path` on `server` with a JSON body (and the agent's token). `done` runs on the main queue.
func fleetRequest(server: String, path: String, body: [String: Any], token: String?, done: @escaping (FleetAnswer) -> Void) {
    func finish(_ a: FleetAnswer) { DispatchQueue.main.async { done(a) } }
    guard let url = URL(string: server + path), let payload = try? JSONSerialization.data(withJSONObject: body) else {
        finish(.refused(status: 0, message: "The server's address is not valid.")); return
    }
    var req = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 20)
    req.httpMethod = "POST"
    req.httpBody = payload
    req.setValue("application/json", forHTTPHeaderField: "Content-Type")
    req.setValue("application/json", forHTTPHeaderField: "Accept")
    req.setValue("Taro-Mac/\(appVersion)", forHTTPHeaderField: "User-Agent")
    if let t = token { req.setValue("Bearer \(t)", forHTTPHeaderField: "Authorization") }
    URLSession.shared.dataTask(with: req) { data, response, error in
        if let error = error { finish(.unreachable(error.localizedDescription)); return }
        guard let http = response as? HTTPURLResponse else { finish(.unreachable("no answer from the server")); return }
        var obj: [String: Any]?
        if let d = data, !d.isEmpty { obj = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any] }
        if (200..<300).contains(http.statusCode) { finish(.ok(obj ?? [:])); return }
        if http.statusCode == 401 { finish(.unauthorized); return }
        let message = ((obj?["error"] as? [String: Any])?["message"] as? String) ?? "The server answered \(http.statusCode)."
        finish(.refused(status: http.statusCode, message: message))
    }.resume()
}
