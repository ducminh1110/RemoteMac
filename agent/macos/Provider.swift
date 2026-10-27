// A provider's Taro (Taro Enterprise): the official taro, unmodified, with a provider/ folder
// next to it holding provider.json (crates/rm-brand: the same file the provider's Windows app
// and relay read). Its relay and key replace the ones built into this binary, and its product
// name replaces "Taro" in what the Mac says to the user (not in protocol strings, file paths or
// labels). A provider/ folder that is wrong stops the program: falling back to Taro's own relay
// would take the provider's customers to the wrong service.
import Foundation

struct ProviderInfo {
    let name: String
    let shortName: String
    let vendor: String
    let relayHost: String
    let relayPort: Int
    let relayKey: String
    /// updates.clients == "off": the provider ships new packages itself
    let updatesOff: Bool

    /// host:port (an IPv6 address in brackets), as rm-brand's Relay::address.
    var relayAddress: String {
        relayHost.contains(":") && !relayHost.hasPrefix("[") ? "[\(relayHost)]:\(relayPort)" : "\(relayHost):\(relayPort)"
    }
}

/// The real path of this program (symbolic links resolved), so provider/ is found next to the
/// file itself, wherever it was started from.
func realExecutablePath() -> String {
    var size = UInt32(4096)
    var buf = [CChar](repeating: 0, count: Int(size))
    if _NSGetExecutablePath(&buf, &size) == 0, let real = realpath(buf, nil) {
        defer { free(real) }
        return String(cString: real)
    }
    let path = Bundle.main.executablePath ?? CommandLine.arguments[0]
    return URL(fileURLWithPath: path).resolvingSymlinksInPath().path
}

/// Validation as rm-brand's (valid_host, valid_relay_key, the name's length).
func validRelayHost(_ s: String) -> Bool {
    var a4 = in_addr(), a6 = in6_addr()
    if inet_pton(AF_INET, s, &a4) == 1 || inet_pton(AF_INET6, s, &a6) == 1 { return true }
    let h = s.hasSuffix(".") ? String(s.dropLast()) : s
    guard h.count <= 253, h.contains(".") else { return false }
    return h.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { l in
        (1...63).contains(l.count) && l.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-") } && !l.hasPrefix("-") && !l.hasSuffix("-")
    }
}

func validRelayKey(_ s: String) -> Bool {
    (16...128).contains(s.count) && s.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }
}

/// What is wrong with a provider.json.
struct ProviderProblem: Error {
    let message: String
}

/// provider.json's fields, or what is wrong with it.
func parseProvider(_ data: Data) -> Result<ProviderInfo, ProviderProblem> {
    guard let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return .failure(ProviderProblem(message: "provider.json is not valid JSON")) }
    guard root["schema"] as? String == "taro.provider/v1" else { return .failure(ProviderProblem(message: "schema: must be taro.provider/v1")) }
    let product = root["product"] as? [String: Any] ?? [:]
    let relay = root["relay"] as? [String: Any] ?? [:]
    let updates = root["updates"] as? [String: Any] ?? [:]
    func plain(_ v: Any?, _ max: Int) -> String? {
        guard let s = v as? String, (1...max).contains(s.count), s.trimmingCharacters(in: .whitespaces) == s,
              !s.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return nil }
        return s
    }
    guard let name = plain(product["name"], 40) else { return .failure(ProviderProblem(message: "product.name: 1-40 characters, no line breaks")) }
    guard let short = plain(product["short_name"], 16) else { return .failure(ProviderProblem(message: "product.short_name: 1-16 characters")) }
    guard let vendor = plain(product["vendor"], 80) else { return .failure(ProviderProblem(message: "product.vendor: 1-80 characters, no line breaks")) }
    guard let host = relay["host"] as? String, validRelayHost(host) else { return .failure(ProviderProblem(message: "relay.host: a DNS name (relay.example.com) or an IP address")) }
    // a whole number 1-65535 (not 7470.5, not "7470", not true)
    guard let n = relay["port"] as? NSNumber, CFGetTypeID(n) == CFNumberGetTypeID(), n.doubleValue == Double(n.intValue),
          (1...65535).contains(n.intValue) else { return .failure(ProviderProblem(message: "relay.port: 1-65535")) }
    guard let key = relay["key"] as? String, validRelayKey(key) else { return .failure(ProviderProblem(message: "relay.key: 16-128 letters, digits, dashes or underscores")) }
    let clients = updates["clients"] as? String ?? "notify"
    guard clients == "notify" || clients == "off" else { return .failure(ProviderProblem(message: "updates.clients: notify or off")) }
    return .success(ProviderInfo(name: name, shortName: short, vendor: vendor, relayHost: host, relayPort: n.intValue, relayKey: key, updatesOff: clients == "off"))
}

/// The provider/ folder: next to this program; in Taro.app (the Enterprise edition,
/// EnterpriseApp.swift) also in the app's Resources, else next to the app. nil: none.
let providerDirectory: URL? = { () -> URL? in
    var dirs = [URL(fileURLWithPath: realExecutablePath()).deletingLastPathComponent().appendingPathComponent("provider", isDirectory: true)]
    let bundle = Bundle.main.bundleURL
    if bundle.pathExtension == "app" {
        if let res = Bundle.main.resourceURL { dirs.append(res.appendingPathComponent("provider", isDirectory: true)) }
        dirs.append(bundle.deletingLastPathComponent().appendingPathComponent("provider", isDirectory: true))
    }
    return dirs.first { d in
        var isDir: ObjCBool = false
        return FileManager.default.fileExists(atPath: d.path, isDirectory: &isDir) && isDir.boolValue
    }
}()

/// The provider next to this program: nil without a provider/ folder; the program ends with a
/// clear message when the folder is there but its provider.json is missing or wrong.
let provider: ProviderInfo? = { () -> ProviderInfo? in
    guard let dir = providerDirectory else { return nil }
    let file = dir.appendingPathComponent("provider.json")
    let why: String
    if let data = FileManager.default.contents(atPath: file.path) {
        switch parseProvider(data) {
        case .success(let p): return p
        case .failure(let e): why = e.message
        }
    } else {
        why = "provider.json is missing"
    }
    FileHandle.standardError.write(Data("taro: \(file.path): \(why)\nThis program cannot start: its provider settings are not valid. Download the package again from your provider, or ask them for help.\n".utf8))
    exit(4)
}()

/// The product's name in what the Mac says to the user: "Taro", or the provider's.
var productName: String { provider?.name ?? "Taro" }

/// What the user reads, with the provider's product name where it says Taro (the word alone).
func named(_ s: String) -> String {
    guard let p = provider else { return s }
    return s.replacingOccurrences(of: "(?<![/\\\\\\w])Taro(?![/\\\\\\w]|://)", with: NSRegularExpression.escapedTemplate(for: p.name), options: .regularExpression)
}

/// `taro --print-provider`: the product, the relay and whether a provider key is set (never the
/// key), as this program would use them.
func printProvider(relay: String?) {
    print("product: \(productName)")
    if let p = provider { print("vendor: \(p.vendor)") }
    print("relay: \(relay ?? "none (this network only)")")
    print("provider key: \(provider == nil ? "not set" : "set")")
    if let p = provider { print("client updates: \(p.updatesOff ? "off" : "notify")") }
}
