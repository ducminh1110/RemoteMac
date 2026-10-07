// Files the user picked on Windows, written into ~/Downloads/RemoteMac Uploads. The client names the
// file; we never let it choose a directory: names are sanitised exactly like rm_protocol::sanitize_upload_name.
// They are only there for the session: when it ends (and at start, after a crash) the folder is
// removed, so nothing piles up on the Mac's disk.
import Foundation

let maxUpload: UInt64 = 2 << 30

func sanitizeUploadName(_ name: String) -> String {
    let base = name.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
    let cleaned = String(base.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) && $0 != ":" })
    var out = cleaned.trimmingCharacters(in: .whitespaces)
    while out.hasPrefix(".") { out.removeFirst() }
    out = String(out.trimmingCharacters(in: .whitespaces).prefix(200))
    return out.isEmpty || out == "." || out == ".." ? "upload" : out
}

final class UploadStore {
    let dir: URL
    private struct Active { let handle: FileHandle; let url: URL; let size: UInt64; var received: UInt64 }
    private var active: [UInt64: Active] = [:]
    private let send: ([String: Any]) -> Void

    init(send: @escaping ([String: Any]) -> Void) {
        self.send = send
        dir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Downloads/RemoteMac Uploads", isDirectory: true)
    }

    /// Remove everything uploaded (the session is over): open transfers are dropped too.
    func cleanup() {
        for a in active.values { try? a.handle.close() }
        active.removeAll()
        let fm = FileManager.default
        if let items = try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.fileSizeKey]) {
            let bytes = items.reduce(0) { $0 + ((try? $1.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0) }
            try? fm.removeItem(at: dir)
            if !items.isEmpty { log("uploads cleaned: \(items.count) file(s), \(bytes / 1024) KB freed") }
        }
    }

    private func fail(_ id: UInt64, _ reason: String) {
        if let a = active.removeValue(forKey: id) { try? a.handle.close(); try? FileManager.default.removeItem(at: a.url) }
        send(["type": "file_upload_failed", "transfer_id": Int(id), "reason": reason])
    }

    /// A name inside `dir` that does not exist yet: "a.txt", "a (2).txt", ...
    private func unique(_ name: String) -> URL {
        let ext = (name as NSString).pathExtension, stem = (name as NSString).deletingPathExtension
        var url = dir.appendingPathComponent(name), n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent(ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)"); n += 1
        }
        return url
    }

    func handle(_ m: [String: Any]) {
        let id = UInt64(int(m["transfer_id"]))
        switch m["type"] as? String ?? "" {
        case "file_upload_begin":
            let size = UInt64(max(0, int(m["size"])))
            guard size <= maxUpload else { return fail(id, "file too large") }
            guard active[id] == nil else { return fail(id, "duplicate transfer id") }
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let url = unique(sanitizeUploadName(m["name"] as? String ?? ""))
                guard FileManager.default.createFile(atPath: url.path, contents: nil) else { return fail(id, "cannot create file") }
                active[id] = Active(handle: try FileHandle(forWritingTo: url), url: url, size: size, received: 0)
            } catch { fail(id, "\(error)") }
        case "file_upload_chunk":
            guard var a = active[id] else { return fail(id, "unknown transfer") }
            guard let b64 = m["data_base64"] as? String, let data = Data(base64Encoded: b64) else { return fail(id, "bad chunk") }
            guard UInt64(max(0, int(m["offset"]))) == a.received, a.received + UInt64(data.count) <= a.size else { return fail(id, "out-of-order or oversized chunk") }
            do { try a.handle.write(contentsOf: data) } catch { return fail(id, "write failed") }
            a.received += UInt64(data.count); active[id] = a
        case "file_upload_end":
            guard let a = active[id] else { return fail(id, "unknown transfer") }
            guard a.received == a.size else { return fail(id, "incomplete (\(a.received)/\(a.size))") }
            try? a.handle.close(); active.removeValue(forKey: id)
            log("upload complete \(a.url.lastPathComponent) \(a.size) bytes")
            send(["type": "file_uploaded", "transfer_id": Int(id), "remote_path": a.url.path])
        default: break
        }
    }
}
