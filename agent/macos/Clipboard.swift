// Bidirectional clipboard: text, and pictures (as .bmp files, which Windows takes as CF_DIB). The agent polls NSPasteboard's changeCount (there is no
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
    var onLocalImage: ((UInt64, Data) -> Void)?

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
        if let s = pb.string(forType: .string) {
            seq += 1
            onLocalChange?(seq, s)
        } else if let d = pb.data(forType: .png) ?? pb.data(forType: .tiff), let bmp = bmpFile(d), bmp.count <= 11 << 20 {
            seq += 1
            log("clipboard -> Windows: picture, \(bmp.count) bytes")
            onLocalImage?(seq, bmp)
        }
    }

    /// A picture from Windows (a .bmp file) on the Mac pasteboard.
    func applyImage(_ bmp: Data) {
        guard let img = NSImage(data: bmp) else { log("clipboard: picture from Windows not readable"); return }
        queue.sync {
            pb.clearContents()
            pb.writeObjects([img])
            lastCount = pb.changeCount
        }
        log("clipboard <- Windows: picture \(Int(img.size.width))x\(Int(img.size.height))")
    }

    /// Any picture as a 32-bit bottom-up .bmp (BITMAPINFOHEADER, BI_RGB), as every Windows
    /// program takes from the clipboard.
    private func bmpFile(_ data: Data) -> Data? {
        guard let rep = NSBitmapImageRep(data: data), let cg = rep.cgImage else { return nil }
        let (w, h) = (cg.width, cg.height)
        guard w > 0, h > 0, w * h <= 40_000_000 else { return nil }
        var px = [UInt8](repeating: 0, count: w * h * 4)
        // BGRA, premultiplied (Windows' CF_DIB has no alpha meaning; opaque pixels are exact)
        guard let ctx = CGContext(data: &px, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue) else { return nil }
        // bottom-up rows, as a DIB with a positive height stores them
        ctx.translateBy(x: 0, y: CGFloat(h)); ctx.scaleBy(x: 1, y: -1)
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
        var out = Data()
        func u32(_ v: Int) { var x = UInt32(v).littleEndian; out.append(Data(bytes: &x, count: 4)) }
        func u16(_ v: Int) { var x = UInt16(v).littleEndian; out.append(Data(bytes: &x, count: 2)) }
        out.append(contentsOf: [0x42, 0x4D]); u32(54 + px.count); u32(0); u32(54)
        u32(40); u32(w); u32(h); u16(1); u16(32); u32(0); u32(px.count); u32(2835); u32(2835); u32(0); u32(0)
        out.append(contentsOf: px)
        return out
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
