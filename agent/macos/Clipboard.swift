// Bidirectional plain-text clipboard. The agent polls NSPasteboard's changeCount (there is no
// change notification API) and ignores the change it caused itself when applying the client's text.
import Foundation
import AppKit

final class ClipboardSync {
    private let pb = NSPasteboard.general
    private var lastCount: Int
    private var seq: UInt64 = 0
    private let queue = DispatchQueue(label: "rm.clipboard")
    private var timer: DispatchSourceTimer?
    var onLocalChange: ((UInt64, String) -> Void)?

    init() { lastCount = NSPasteboard.general.changeCount }

    func start() {
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now() + .milliseconds(300), repeating: .milliseconds(250))
        t.setEventHandler { [weak self] in self?.poll() }
        t.resume(); timer = t
    }

    private func poll() {
        let c = pb.changeCount
        guard c != lastCount else { return }
        lastCount = c
        guard let s = pb.string(forType: .string) else { return }
        seq += 1
        onLocalChange?(seq, s)
    }

    /// Apply text from the client; the resulting changeCount bump is not echoed back.
    func apply(_ text: String) {
        queue.sync {
            pb.clearContents()
            pb.setString(text, forType: .string)
            lastCount = pb.changeCount
        }
    }
}

/// Square RGBA (straight alpha) rendering of an application's Finder icon.
func appIconRGBA(path: String, size: Int) -> Data? {
    let img = NSWorkspace.shared.icon(forFile: path)
    var rect = NSRect(x: 0, y: 0, width: size, height: size)
    guard let cg = img.cgImage(forProposedRect: &rect, context: nil, hints: nil) else { return nil }
    var buf = [UInt8](repeating: 0, count: size * size * 4)
    guard let ctx = CGContext(data: &buf, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4,
                              space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
    ctx.draw(cg, in: CGRect(x: 0, y: 0, width: size, height: size))
    // un-premultiply so the wire format is straight alpha
    for i in stride(from: 0, to: buf.count, by: 4) {
        let a = Int(buf[i + 3])
        if a > 0 && a < 255 { for k in 0..<3 { buf[i + k] = UInt8(min(255, Int(buf[i + k]) * 255 / a)) } }
    }
    return Data(buf)
}
