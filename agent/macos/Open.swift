// Opening a document from the viewer ("open_file"): a file the user dropped on a Mac app's
// window, or onto the launcher, after it was uploaded. Scoped: only documents in the session's
// upload folder or the user's own folders, never anything that runs (apps, installers,
// scripts, executables), and only with an app MacBridge lists. No shell is involved.
import Foundation
import AppKit
import UniformTypeIdentifiers

/// Extensions that run something or change the system when opened (as `RUNS_CODE` in
/// crates/rm-protocol, which the viewer checks before uploading).
private let deniedExtensions: Set<String> = [
    "exe", "msi", "bat", "cmd", "ps1", "vbs", "lnk", "scr", "com",
    "app", "pkg", "mpkg", "dmg", "command", "tool", "sh", "zsh", "bash", "csh", "ksh", "fish", "terminal", "workflow", "action",
    "scpt", "scptd", "applescript", "jar", "py", "rb", "pl", "php", "js", "jxa", "webloc", "inetloc", "fileloc", "url",
    "prefpane", "kext", "mobileconfig", "saver", "plugin", "bundle", "osax", "qlgenerator", "xpc", "systemextension", "appex",
]

/// Why `path` may not be opened, or nil when it may. `uploads`: the session's upload folder.
func openRejection(_ path: String, uploads: URL) -> String? {
    guard !path.isEmpty, path.utf8.count <= 4096, path.hasPrefix("/") else { return "not an absolute path" }
    guard !path.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return "control characters in the path" }
    let url = URL(fileURLWithPath: path).standardizedFileURL.resolvingSymlinksInPath()
    let real = url.path
    let home = URL(fileURLWithPath: NSHomeDirectory()).resolvingSymlinksInPath().path
    let up = uploads.resolvingSymlinksInPath().path
    let inUploads = real.hasPrefix(up + "/")
    let inHome = real.hasPrefix(home + "/") && !real.hasPrefix(home + "/Library/") && !real.hasPrefix(home + "/.")
    guard inUploads || inHome else { return "only files in your home folder can be opened" }
    var dir: ObjCBool = false
    guard FileManager.default.fileExists(atPath: real, isDirectory: &dir) else { return "no such file" }
    guard !dir.boolValue else { return "folders and packages are not opened this way" }
    let ext = url.pathExtension.lowercased()
    guard !deniedExtensions.contains(ext) else { return "files of type .\(ext) can run code and are not opened" }
    if let t = UTType(filenameExtension: ext), t.conforms(to: .executable) || t.conforms(to: .script) || t.conforms(to: .package) || t.conforms(to: .application) {
        return "this kind of file can run code and is not opened"
    }
    guard !FileManager.default.isExecutableFile(atPath: real) else { return "executable files are not opened" }
    return nil
}

/// Open `path` with the app `appBundle` (an app MacBridge lists), or with its default app.
func openDocument(_ path: String, appBundle: String?, done: @escaping (String?) -> Void) {
    let file = URL(fileURLWithPath: path)
    let cfg = NSWorkspace.OpenConfiguration()
    cfg.activates = true
    cfg.addsToRecentItems = true
    if let b = appBundle {
        NSWorkspace.shared.open([file], withApplicationAt: URL(fileURLWithPath: b), configuration: cfg) { _, err in done(err.map { "\($0.localizedDescription)" }) }
    } else {
        guard let app = NSWorkspace.shared.urlForApplication(toOpen: file) else { done("no app on the Mac opens this kind of file"); return }
        if app.pathExtension != "app" { done("no app on the Mac opens this kind of file"); return }
        NSWorkspace.shared.open([file], withApplicationAt: app, configuration: cfg) { _, err in done(err.map { "\($0.localizedDescription)" }) }
    }
}
