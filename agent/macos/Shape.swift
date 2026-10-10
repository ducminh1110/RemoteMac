// Window shapes for the viewer ("window_mask", rm-protocol's mask.rs).
//
// Windows are streamed as video, which has no transparency: the corners outside a window's
// rounding, and the outline of a menu or of the Dock, came out black on Windows. One screenshot of
// the same thing (same filter, same size, same part of it) in BGRA says which pixels are the
// window's: its alpha is sent once per stream, and the viewer shows the picture through it.
import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreVideo

enum Shape {
    /// The alpha of what `filter` shows, `width`x`height` pixels of `source` (or all of it), as
    /// the stream with the same settings captures it; nil when it could not be taken.
    static func alpha(filter: SCContentFilter, width: Int, height: Int, source: CGRect?) async -> [UInt8]? {
        let cfg = SCStreamConfiguration()
        cfg.width = width; cfg.height = height
        if let r = source { cfg.sourceRect = r }
        cfg.pixelFormat = kCVPixelFormatType_32BGRA
        cfg.showsCursor = false
        cfg.scalesToFit = true
        cfg.ignoreShadowsSingleWindow = true
        let sb: CMSampleBuffer
        do { sb = try await SCScreenshotManager.captureSampleBuffer(contentFilter: filter, configuration: cfg) } catch {
            log("shape: no screenshot (\(error.localizedDescription))")
            return nil
        }
        guard let pb = CMSampleBufferGetImageBuffer(sb) else { return nil }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        guard w == width, h == height, CVPixelBufferGetPixelFormatType(pb) == kCVPixelFormatType_32BGRA,
              CVPixelBufferLockBaseAddress(pb, .readOnly) == kCVReturnSuccess else { return nil }
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pb) else { return nil }
        let stride = CVPixelBufferGetBytesPerRow(pb)
        let px = base.assumingMemoryBound(to: UInt8.self)
        var a = [UInt8](repeating: 0, count: w * h)
        for y in 0..<h {
            let row = y * stride, o = y * w
            for x in 0..<w { a[o + x] = px[row + x * 4 + 3] }
        }
        return a
    }

    /// rm-protocol's mask runs: (u16 LE length, u8 alpha) pairs, row by row.
    static func runs(_ a: [UInt8]) -> Data {
        var out = Data()
        var i = 0
        while i < a.count {
            let v = a[i]
            var n = 1
            while i + n < a.count && a[i + n] == v && n < 65535 { n += 1 }
            out.append(UInt8(n & 0xff)); out.append(UInt8(n >> 8)); out.append(v)
            i += n
        }
        return out
    }

    /// The "window_mask" message for window `id`: nil when the screenshot shows nothing (all
    /// clear: the window was not on the screen); an empty `rle` when it is all opaque.
    static func message(_ id: CGWindowID, width: Int, height: Int, alpha a: [UInt8]) -> [String: Any]? {
        guard a.contains(where: { $0 > 0 }) else { return nil }
        let rle = a.allSatisfy({ $0 == 255 }) ? "" : runs(a).base64EncodedString()
        return ["type": "window_mask", "window_id": Int(id), "width": width, "height": height, "rle": rle]
    }
}
