// One ScreenCaptureKit stream per remote window -> VideoToolbox H.264 -> Annex-B packets.
import Foundation
import ScreenCaptureKit
import VideoToolbox
import CoreMedia
import CoreVideo

private let startCode = Data([0, 0, 0, 1])

/// Convert VideoToolbox's length-prefixed (AVCC) sample into Annex-B, prepending SPS/PPS on keyframes.
func annexB(_ sb: CMSampleBuffer, keyframe: Bool) -> Data? {
    var out = Data()
    if keyframe, let fd = CMSampleBufferGetFormatDescription(sb) {
        var count = 0
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fd, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
                                                           parameterSetCountOut: &count, nalUnitHeaderLengthOut: nil)
        for i in 0..<count {
            var p: UnsafePointer<UInt8>?; var sz = 0
            if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fd, parameterSetIndex: i, parameterSetPointerOut: &p, parameterSetSizeOut: &sz,
                                                                  parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil) == noErr, let p = p {
                out.append(startCode); out.append(p, count: sz)
            }
        }
    }
    guard let bb = CMSampleBufferGetDataBuffer(sb) else { return nil }
    var len = 0; var ptr: UnsafeMutablePointer<Int8>?
    guard CMBlockBufferGetDataPointer(bb, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &len, dataPointerOut: &ptr) == kCMBlockBufferNoErr,
          let base = ptr else { return nil }
    let raw = UnsafeRawPointer(base)
    var off = 0
    while off + 4 <= len {
        let n = Int(UInt32(bigEndian: raw.loadUnaligned(fromByteOffset: off, as: UInt32.self)))
        off += 4
        if n <= 0 || off + n > len { break }
        out.append(startCode); out.append(Data(bytes: raw + off, count: n))
        off += n
    }
    return out
}

/// 4:2:0 pictures (capture and encoder) must have even sizes.
func even(_ v: Int) -> Int { max(2, v + (v & 1)) }

/// Pixels per point the client shows (its display scale, 1...3): windows are captured at that
/// density so the client draws them 1:1, as sharp as its own windows. Capped near 4K.
var captureScale: CGFloat = 1
/// frames per second (the viewer's settings; Moonlight's FPS choice)
var targetFPS: Int32 = 60
/// Pixels per point the display showing `rect` draws at (1 or 2: the window's real pixels).
func backingScale(of rect: CGRect) -> CGFloat {
    var ids = [CGDirectDisplayID](repeating: 0, count: 8), n: UInt32 = 0
    CGGetDisplaysWithRect(rect, 8, &ids, &n)
    var best: CGFloat = 0
    for d in ids.prefix(Int(n)) {
        if let m = CGDisplayCopyDisplayMode(d), m.width > 0 { best = max(best, CGFloat(m.pixelWidth) / CGFloat(m.width)) }
    }
    return best > 0 ? max(1, best.rounded()) : 1
}

/// `backing`: the window's real pixels per point. When more than 1 pixel per point is asked for
/// (Native, Ultra) the window is captured at exactly its real pixels, never resampled here to a
/// fraction like 1.25 (ScreenCaptureKit
/// scales bilinearly, which softens every glyph): the client resamples it once, sharply, to its
/// own density, or shows it 1:1 (Sunshine captures at the display's own pixels too).
func capturePixels(_ w: CGFloat, _ h: CGFloat, backing: CGFloat? = nil) -> (Int, Int) {
    var s = max(1, min(3, captureScale))
    if let b = backing, captureScale > 1 { s = b }
    let maxPixels: CGFloat = 3840 * 2400
    if w * h * s * s > maxPixels { s = max(1, (maxPixels / max(1, w * h)).squareRoot()) }
    return (even(Int((w * s).rounded())), even(Int((h * s).rounded())))
}

/// The client said its decoder takes H.264 High (set before windows start streaming).
var useHighProfile = false

/// The Mac's own pointer is drawn into the video, as Sunshine captures it (AVCaptureScreenInput
/// with capturesCursor): what the viewer shows is where the pointer really is and what shape it
/// has (I-beam, hand, resize arrows). The viewer hides its own pointer over the picture, as
/// Moonlight does. RM_NO_CURSOR=1: leave it out (the viewer then shows its own).
var showRemoteCursor = ProcessInfo.processInfo.environment["RM_NO_CURSOR"] == nil

final class WindowStream: NSObject, SCStreamOutput {
    let windowID: CGWindowID
    /// Points cut off the top (the Mac title bar).
    let inset: CGFloat
    private let onPacket: (VideoPacket) -> Void
    private var scStream: SCStream?
    private var session: VTCompressionSession?
    private var encW = 0, encH = 0
    private let lock = NSLock()
    private var t0: CFAbsoluteTime = 0
    private(set) var sent = 0
    private var loggedEncodeError = false
    /// capture and encoding run here (also the refinement re-encode)
    private let q = DispatchQueue(label: "rm.capture", qos: .userInteractive)
    private var lastPB: CVPixelBuffer?
    /// the window's width in points (pixels per point = frame width / this)
    private var pointsWide: CGFloat = 0

    /// Make the picture look like a window of the client's own:
    ///  - the corners outside a macOS 26 window's (large) rounding are transparent, which
    ///    becomes black in video: fill them with the window's own colour next to them;
    ///  - a window whose title bar is part of its content (toolbar windows) carries the Mac's
    ///    own window buttons and macOS's purple "being captured" pill top-left: the viewer
    ///    draws its own buttons, so that area takes the colour beside it.
    private func polish(_ pb: CVPixelBuffer, scale: CGFloat, hideButtons: Bool, fillTop: Bool, fillBottom: Bool) {
        guard CVPixelBufferGetPlaneCount(pb) == 2, CVPixelBufferLockBaseAddress(pb, []) == kCVReturnSuccess else { return }
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let yb = CVPixelBufferGetBaseAddressOfPlane(pb, 0), let cb = CVPixelBufferGetBaseAddressOfPlane(pb, 1) else { return }
        let Y = yb.assumingMemoryBound(to: UInt8.self), C = cb.assumingMemoryBound(to: UInt8.self)
        let w = CVPixelBufferGetWidthOfPlane(pb, 0), h = CVPixelBufferGetHeightOfPlane(pb, 0)
        let ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), cs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
        let r = min(Int(28 * scale), w / 4, h / 4)
        guard r > 2 else { return }
        func copyPixel(from sx: Int, _ sy: Int, to x: Int, _ y: Int) {
            Y[y * ys + x] = Y[sy * ys + sx]
            let co = (sy / 2) * cs + (sx / 2) * 2, ct = (y / 2) * cs + (x / 2) * 2
            C[ct] = C[co]; C[ct + 1] = C[co + 1]
        }
        func isBlack(_ x: Int, _ y: Int) -> Bool {
            let c = (y / 2) * cs + (x / 2) * 2
            return Y[y * ys + x] <= 24 && abs(Int(C[c]) - 128) < 12 && abs(Int(C[c + 1]) - 128) < 12
        }
        // corners: transparent (black) pixels outside a circle of radius r take the colour of
        // the pixel diagonally inside the rounding (not when the viewer has the window's shape:
        // it shows the corners as clear, and the edge as the Mac draws it)
        for (left, top) in [(true, true), (false, true), (true, false), (false, false)] where top ? fillTop : fillBottom {
            let sx = left ? r : w - 1 - r, sy = top ? r : h - 1 - r
            for dy in 0..<r {
                for dx in 0..<r {
                    let ex = r - dx, ey = r - dy
                    guard ex * ex + ey * ey > r * r else { continue }
                    let x = left ? dx : w - 1 - dx, y = top ? dy : h - 1 - dy
                    if isBlack(x, y) { copyPixel(from: sx, sy, to: x, y) }
                }
            }
        }
        // the Mac's own buttons (and the capture pill) top-left: each row takes the colour just
        // to the right of that area
        if hideButtons {
            let bw = min(Int(86 * scale), w / 3), bh = min(Int(40 * scale), h / 4)
            for y in 0..<bh {
                for x in 0..<bw { copyPixel(from: bw, y, to: x, y) }
            }
        }
    }
    /// next encoded frame is an IDR (client asked, or frames were dropped)
    private var forceKey = true
    /// the first picture was sent again (see the frame handler)
    private var repeated = false
    /// The window's three buttons as the Mac lays them out (window points), and whether it is the
    /// active window: drawn back over macOS's "being shared" capsule (exact windows)
    private var lights: [CGRect] = []
    private var lightsActive = false
    private var lastScale: CGFloat = 1

    func setLights(_ rects: [CGRect]) { lock.lock(); lights = rects.count == 3 ? rects : []; lock.unlock() }

    /// The window became the active one, or stopped being it: its buttons take their colour (or
    /// grey) at once, even when nothing else in it changes.
    func setLightsActive(_ active: Bool) {
        lock.lock(); let changed = lightsActive != active && !lights.isEmpty; lightsActive = active; lock.unlock()
        guard changed else { return }
        q.async { [weak self] in
            guard let self = self else { return }
            self.lock.lock(); let pb = self.lastPB; let s = self.lastScale; self.lock.unlock()
            guard let pb = pb else { return }
            self.drawLights(pb, scale: s)
            self.encode(pb, pts: CMClockGetTime(CMClockGetHostTimeClock()), ptsUs: agentClockUs(), key: true)
        }
    }

    /// macOS puts a "being shared" capsule where a captured window's buttons are (on the Mac's
    /// screen and in the capture alike). The window is shown on Windows as the Mac draws it, so
    /// its three buttons are drawn back where the Mac lays them out: the capsule's area takes the
    /// title bar's colour beside it, then red, yellow and green (grey when the window is not the
    /// active one), as macOS draws them.
    private func drawLights(_ pb: CVPixelBuffer, scale: CGFloat) {
        lock.lock(); let rects = lights; let active = lightsActive; lock.unlock()
        guard rects.count == 3, CVPixelBufferGetPlaneCount(pb) == 2, CVPixelBufferLockBaseAddress(pb, []) == kCVReturnSuccess else { return }
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        guard let yb = CVPixelBufferGetBaseAddressOfPlane(pb, 0), let cb = CVPixelBufferGetBaseAddressOfPlane(pb, 1) else { return }
        let Y = yb.assumingMemoryBound(to: UInt8.self), C = cb.assumingMemoryBound(to: UInt8.self)
        let w = CVPixelBufferGetWidthOfPlane(pb, 0), h = CVPixelBufferGetHeightOfPlane(pb, 0)
        let ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), cs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
        let s = Double(scale)
        let all = rects.dropFirst().reduce(rects[0]) { $0.union($1) }
        let px = { (v: CGFloat) -> Int in Int((Double(v) * s).rounded()) }
        // the capsule's area: each row takes the colour just left of it (or right, at the edge)
        let x0 = max(2, px(all.minX - 9)), x1 = min(w, px(all.maxX + 9))
        let y0 = max(0, px(all.minY - 6)), y1 = min(h, px(all.maxY + 6))
        guard x1 > x0, y1 > y0 else { return }
        let sx = x0 >= 3 ? x0 - 2 : min(w - 1, x1 + 1)
        for y in y0..<y1 {
            let src = Y[y * ys + sx], co = (y / 2) * cs + (sx / 2) * 2
            let (u, v) = (C[co], C[co + 1])
            for x in x0..<x1 {
                Y[y * ys + x] = src
                let ct = (y / 2) * cs + (x / 2) * 2
                C[ct] = u; C[ct + 1] = v
            }
        }
        // the buttons: video-range BT.709, a darker rim, edges smoothed
        let dark = UserDefaults.standard.string(forKey: "AppleInterfaceStyle") == "Dark"
        typealias RGB = (Double, Double, Double)
        let grey: (RGB, RGB) = dark ? ((77, 77, 82), (94, 94, 99)) : ((221, 221, 223), (204, 204, 208))
        let colours: [(RGB, RGB)] = active ? [((255, 95, 87), (226, 70, 63)), ((254, 188, 46), (225, 161, 22)), ((40, 200, 64), (20, 174, 44))] : [grey, grey, grey]
        func yuv(_ c: RGB) -> RGB {
            let (r, g, b) = (c.0 / 255, c.1 / 255, c.2 / 255)
            let l = 0.2126 * r + 0.7152 * g + 0.0722 * b
            return (16 + 219 * l, 128 + 224 * (b - l) / 1.8556, 128 + 224 * (r - l) / 1.5748)
        }
        func mix(_ a: UInt8, _ b: Double, _ k: Double) -> UInt8 { UInt8(max(0, min(255, (Double(a) * (1 - k) + b * k).rounded()))) }
        for (i, f) in rects.enumerated() {
            let cx = Double(f.midX) * s, cy = Double(f.midY) * s
            let rad = max(4, Double(min(f.width, f.height)) / 2 - 1) * s
            let rimW = max(0.6, 0.5 * s)
            let (fill, rim) = (yuv(colours[i].0), yuv(colours[i].1))
            let bx0 = max(0, Int(cx - rad) - 2), bx1 = min(w - 1, Int(cx + rad) + 2)
            let by0 = max(0, Int(cy - rad) - 2), by1 = min(h - 1, Int(cy + rad) + 2)
            guard bx1 > bx0, by1 > by0 else { continue }
            func shade(_ x: Double, _ y: Double) -> (cov: Double, col: RGB) {
                let d = ((x - cx) * (x - cx) + (y - cy) * (y - cy)).squareRoot()
                let cov = max(0, min(1, rad + 0.5 - d))
                let inner = max(0, min(1, rad - rimW + 0.5 - d))
                return (cov, (rim.0 + (fill.0 - rim.0) * inner, rim.1 + (fill.1 - rim.1) * inner, rim.2 + (fill.2 - rim.2) * inner))
            }
            for y in by0...by1 {
                for x in bx0...bx1 {
                    let (cov, col) = shade(Double(x) + 0.5, Double(y) + 0.5)
                    if cov > 0 { Y[y * ys + x] = mix(Y[y * ys + x], col.0, cov) }
                }
            }
            for y in stride(from: by0 & ~1, through: by1, by: 2) {
                for x in stride(from: bx0 & ~1, through: bx1, by: 2) {
                    let (cov, col) = shade(Double(x) + 1, Double(y) + 1)
                    guard cov > 0 else { continue }
                    let ci = (y / 2) * cs + (x / 2) * 2
                    C[ci] = mix(C[ci], col.1, cov); C[ci + 1] = mix(C[ci + 1], col.2, cov)
                }
            }
        }
    }
    private var bitrate = 20_000_000

    func requestKeyframe() {
        lock.lock(); forceKey = true; let pb = lastPB; lock.unlock()
        // a picture that does not change brings no next frame to make the keyframe of (a dialog,
        // the menu bar): the last one goes again as a keyframe, unless a new frame took it first
        // (a moving window's next frame comes within a few ms: it is the keyframe then, nothing more)
        guard let last = pb else { return }
        q.asyncAfter(deadline: .now() + 0.06) { [weak self] in
            guard let self = self else { return }
            self.lock.lock(); let still = self.forceKey; self.forceKey = false; self.lock.unlock()
            if still { self.encode(last, pts: CMClockGetTime(CMClockGetHostTimeClock()), ptsUs: agentClockUs(), key: true) }
        }
    }

    /// Sharp when still (what remote desktops call refinement): while the picture moves the
    /// bitrate keeps it fluid; once it has been still for a moment one keyframe with the whole
    /// budget re-draws it crisp. Small frames are "nothing changed".
    private var lastChange: CFAbsoluteTime = 0, refined = true
    private func noteFrameSize(_ bytes: Int, keyframe: Bool) {
        let now = CFAbsoluteTimeGetCurrent()
        lock.lock()
        if keyframe { refined = true; lock.unlock(); return }
        let moving = bytes > 2_000
        if moving { lastChange = now; refined = false }
        lock.unlock()
        // macOS sends no frames while nothing changes: re-encode the last picture as a keyframe
        // once the window has been still for a moment
        if moving {
            q.asyncAfter(deadline: .now() + 0.45) { [weak self] in
                guard let self = self else { return }
                self.lock.lock()
                let still = !self.refined && CFAbsoluteTimeGetCurrent() - self.lastChange >= 0.44
                if still { self.refined = true }
                let pb = self.lastPB
                self.lock.unlock()
                if still, let pb = pb { self.encode(pb, pts: CMClockGetTime(CMClockGetHostTimeClock()), ptsUs: agentClockUs(), key: true) }
            }
        }
    }

    func setBitrate(_ b: Int) {
        lock.lock(); bitrate = b; let s = session; lock.unlock()
        if let s = s { applyBitrate(s, b) }
    }

    private func applyBitrate(_ s: VTCompressionSession, _ b: Int) {
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: b as CFNumber)
        // hard cap per second: rate spikes are what fill the link
        // Sunshine sizes the rate buffer to one frame (rc_buffer_size = bitrate / fps) so no frame
        // is much bigger than the link carries in a frame time: a scroll does not turn into a
        // burst that queues for a quarter second. Here: at most 1.5x the bitrate over any 100 ms
        // (one keyframe still fits), and over a second
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_DataRateLimits, value: [NSNumber(value: b / 8 * 3 / 20), NSNumber(value: 0.1), NSNumber(value: b / 8 * 3 / 2), NSNumber(value: 1)] as CFArray)
    }

    /// Set for a whole-display stream (Mac Desktop); `windowID` is then the reserved desktop id.
    let display: CGDirectDisplayID?
    /// The display's picture size asked for by the client (its Mac Desktop scale, 1x or 2x);
    /// nil: the usual pixels per point.
    var pixels: (Int, Int)?
    /// A pop-up menu or popover: ScreenCaptureKit does not capture those as a window of their own
    /// (it gave the whole display), so its rectangle of the display is captured instead.
    var popup = false
    /// The window's own buttons stay in the picture (exact windows: the viewer draws none).
    var keepButtons = false
    /// Gets the alpha of the picture (its shape) once the stream runs: (width, height, alpha).
    var onShape: ((Int, Int, [UInt8]) -> Void)?
    /// For a region (the Dock): the apps whose windows make its shape (not the desktop picture).
    var shapeApps: Set<pid_t>?

    /// The shape of what this stream shows, measured once in the background.
    private func measureShape(_ filter: SCContentFilter, _ cfg: SCStreamConfiguration, source: CGRect?) {
        guard let done = onShape else { return }
        let (w, h) = (cfg.width, cfg.height)
        Task { if let a = await Shape.alpha(filter: filter, width: w, height: h, source: source) { done(w, h, a) } }
    }
    /// A region of a display with only some apps' windows in it (the Mac's Dock over the
    /// desktop picture: Fusion.swift): (display, region in screen points, those apps).
    var region: (CGDirectDisplayID, CGRect, Set<pid_t>)?
    private var config: SCStreamConfiguration?

    /// The Mac's pointer in the picture or not (the viewer shows its own instead), live.
    func setCursor(_ on: Bool) {
        guard let s = scStream, let c = config, c.showsCursor != on else { return }
        c.showsCursor = on
        s.updateConfiguration(c) { e in if let e = e { log("pointer setting not applied: \(e)") } }
    }

    init(windowID: CGWindowID, inset: CGFloat = 0, display: CGDirectDisplayID? = nil, onPacket: @escaping (VideoPacket) -> Void) {
        self.windowID = windowID; self.inset = inset; self.display = display; self.onPacket = onPacket
    }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        if let rg = region {
            let (did, r, pids) = rg
            guard let d = content.displays.first(where: { $0.displayID == did }) else { throw WireError(description: "display \(did) not shareable") }
            let cfg = SCStreamConfiguration()
            cfg.sourceRect = CGRect(x: r.minX - d.frame.minX, y: r.minY - d.frame.minY, width: r.width, height: r.height)
            (cfg.width, cfg.height) = capturePixels(r.width, r.height, backing: backingScale(of: r))
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: targetFPS)
            cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange; cfg.colorMatrix = kCVImageBufferYCbCrMatrix_ITU_R_709_2
            cfg.queueDepth = 6; cfg.showsCursor = showRemoteCursor; cfg.scalesToFit = true
            // only those apps' windows (the Dock and the desktop picture), or everything there (no
            // apps given: the menu bar's strip, which no window covers)
            let apps = content.applications.filter { pids.contains($0.processID) }
            let filter = pids.isEmpty ? SCContentFilter(display: d, excludingWindows: []) : SCContentFilter(display: d, including: apps, exceptingWindows: [])
            let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
            t0 = CFAbsoluteTimeGetCurrent()
            config = cfg
            try await s.startCapture()
            scStream = s
            if let own = shapeApps {
                measureShape(SCContentFilter(display: d, including: content.applications.filter { own.contains($0.processID) }, exceptingWindows: []), cfg, source: cfg.sourceRect)
            }
            return
        }
        if let did = display {
            guard let d = content.displays.first(where: { $0.displayID == did }) else { throw WireError(description: "display \(did) not shareable") }
            let cfg = SCStreamConfiguration()
            if let p = pixels, p.0 > 0, p.1 > 0 {
                let (pw, ph) = p
                // exactly the size asked for (capped near 4K, which the encoder takes)
                let k = min(1, (CGFloat(3840 * 2400) / CGFloat(pw * ph)).squareRoot())
                (cfg.width, cfg.height) = (even(Int((CGFloat(pw) * k).rounded())), even(Int((CGFloat(ph) * k).rounded())))
            } else {
                (cfg.width, cfg.height) = capturePixels(CGFloat(d.width), CGFloat(d.height))
            }
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: targetFPS)
            cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange; cfg.colorMatrix = kCVImageBufferYCbCrMatrix_ITU_R_709_2 // YUV straight to the encoder (no conversion), BT.709 as the viewer expects
            cfg.queueDepth = 6; cfg.showsCursor = showRemoteCursor; cfg.scalesToFit = true // the Mac's pointer is in the picture (as Sunshine); content fills the output at any density
            let filter = SCContentFilter(display: d, excludingWindows: [])
            // what the display really draws per point (a display left at 1x enlarged to 2x is blurry)
            var drawn = "?"
            if #available(macOS 14.0, *) { drawn = "\(filter.pointPixelScale)x" }
            log("display \(did) draws at \(drawn) (ScreenCaptureKit)")
            let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
            t0 = CFAbsoluteTimeGetCurrent()
            config = cfg
            try await s.startCapture()
            scStream = s
            let mode = CGDisplayCopyDisplayMode(did).map { "\($0.pixelWidth)x\($0.pixelHeight) px" } ?? "?"
            log("display \(did) captured at \(cfg.width)x\(cfg.height) (display draws \(d.width)x\(d.height) points, \(mode))")
            return
        }
        guard let w = content.windows.first(where: { $0.windowID == windowID }) else { throw WireError(description: "window \(windowID) not shareable") }
        if popup, let d = content.displays.first(where: { $0.frame.intersects(w.frame) && $0.frame.contains(CGPoint(x: w.frame.midX, y: w.frame.midY)) }) ?? content.displays.first(where: { $0.frame.intersects(w.frame) }) {
            let cfg = SCStreamConfiguration()
            // the popup's rectangle of its display, as it is seen: menus and popovers are
            // translucent (their material blurs what is behind them); drawn alone they lost their
            // background and their items
            cfg.sourceRect = CGRect(x: w.frame.minX - d.frame.minX, y: w.frame.minY - d.frame.minY, width: w.frame.width, height: w.frame.height)
            (cfg.width, cfg.height) = capturePixels(w.frame.width, w.frame.height, backing: backingScale(of: w.frame))
            pointsWide = w.frame.width
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: targetFPS)
            cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange; cfg.colorMatrix = kCVImageBufferYCbCrMatrix_ITU_R_709_2
            cfg.queueDepth = 6; cfg.showsCursor = showRemoteCursor; cfg.scalesToFit = true
            let s = SCStream(filter: SCContentFilter(display: d, excludingWindows: []), configuration: cfg, delegate: nil)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
            t0 = CFAbsoluteTimeGetCurrent()
            config = cfg
            try await s.startCapture()
            scStream = s
            // its outline: the popup window alone, at the same size
            measureShape(SCContentFilter(desktopIndependentWindow: w), cfg, source: nil)
            return
        }
        let cfg = SCStreamConfiguration()
        let cut = min(inset, max(0, w.frame.height - 2))
        if cut > 0 { cfg.sourceRect = CGRect(x: 0, y: cut, width: w.frame.width, height: w.frame.height - cut) }
        // 4:2:0 needs even sizes (an odd one gets no frames at all): round up a pixel
        (cfg.width, cfg.height) = capturePixels(w.frame.width, w.frame.height - cut, backing: backingScale(of: w.frame))
        pointsWide = w.frame.width
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: targetFPS)
        cfg.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange; cfg.colorMatrix = kCVImageBufferYCbCrMatrix_ITU_R_709_2 // YUV straight to the encoder (no conversion), BT.709 as the viewer expects
        cfg.queueDepth = 6; cfg.showsCursor = showRemoteCursor; cfg.scalesToFit = true // fill the output at any capture density (never a corner of it, never cropped)
        let filter = SCContentFilter(desktopIndependentWindow: w)
        let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
        try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
        t0 = CFAbsoluteTimeGetCurrent()
        config = cfg
        try await s.startCapture()
        scStream = s
        measureShape(filter, cfg, source: cut > 0 ? cfg.sourceRect : nil)
    }

    func stop() async {
        if let s = scStream { try? await s.stopCapture() }
        scStream = nil
        lock.lock(); let sess = session; session = nil; lastPB = nil; lock.unlock()
        if let s = sess { VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid); VTCompressionSessionInvalidate(s) }
    }

    private func makeSession(_ w: Int, _ h: Int) -> VTCompressionSession? {
        var s: VTCompressionSession?
        // Apple's low-latency rate control (what FaceTime uses) when available, else the regular one
        let lowLatency = [kVTVideoEncoderSpecification_EnableLowLatencyRateControl as String: true] as CFDictionary
        if VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
                                      encoderSpecification: lowLatency, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                      outputCallback: nil, refcon: nil, compressionSessionOut: &s) != noErr {
            s = nil
            guard VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
                                             encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                             outputCallback: nil, refcon: nil, compressionSessionOut: &s) == noErr else { return nil }
        }
        guard let s = s else { return nil }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: useHighProfile ? kVTProfileLevel_H264_High_AutoLevel : kVTProfileLevel_H264_Main_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: targetFPS as CFNumber)
        // signal BT.709 in the stream (the viewer's GPU colour conversion uses it)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ColorPrimaries, value: kCVImageBufferColorPrimaries_ITU_R_709_2)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_TransferFunction, value: kCVImageBufferTransferFunction_ITU_R_709_2)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_YCbCrMatrix, value: kCVImageBufferYCbCrMatrix_ITU_R_709_2)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxFrameDelayCount, value: 0 as CFNumber)
        // Sunshine's VideoToolbox settings (video.cpp: realtime, prio_speed): speed over quality
        if #available(macOS 13.0, *) {
            VTSessionSetProperty(s, key: kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, value: kCFBooleanTrue)
        }

        applyBitrate(s, bitrate)
        // keyframes on request (start, client resync, after drops); a long safety interval only
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, value: 10 as CFNumber)
        VTCompressionSessionPrepareToEncodeFrames(s)
        encW = w; encH = h
        return s
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, sb.isValid,
              let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let raw = atts.first?[.status] as? Int, raw == SCFrameStatus.complete.rawValue,
              let pb = CMSampleBufferGetImageBuffer(sb) else { return }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        lock.lock()
        if session == nil { session = makeSession(w, h) }
        let s = session; let ok = (w == encW && h == encH)
        let key = forceKey; forceKey = false
        lock.unlock()
        guard s != nil, ok else { return }   // size changed: the owner restarts the stream
        // capture time on the agent clock (host time, as pongs report it): the viewer turns it
        // into end-to-end latency
        let cap = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sb))
        let nowUs = agentClockUs()
        let capUs = cap.isFinite && cap > 0 ? UInt64(cap * 1_000_000) : nowUs
        let ptsUs = (capUs <= nowUs && nowUs - capUs < 1_000_000) ? capUs : nowUs
        // (not a popup: its first row is not window buttons, filling it hid the item there)
        if display == nil && region == nil && !popup && pointsWide > 0 {
            // (the top corners sit under the viewer's own title bar unless the window is exact)
            polish(pb, scale: CGFloat(w) / pointsWide, hideButtons: inset == 0 && !keepButtons, fillTop: onShape == nil || !keepButtons, fillBottom: onShape == nil)
            if keepButtons {
                lock.lock(); lastScale = CGFloat(w) / pointsWide; lock.unlock()
                drawLights(pb, scale: CGFloat(w) / pointsWide)
            }
        }
        lock.lock(); lastPB = pb; let first = !repeated; repeated = true; lock.unlock()
        encode(pb, pts: CMSampleBufferGetPresentationTimeStamp(sb), ptsUs: ptsUs, key: key)
        // a moment after the stream starts its picture goes once more as a keyframe: a viewer that
        // learned of this window after its first frame (that came first, over UDP) still gets a
        // picture of what never changes (the menu bar, the Dock, a still window)
        if first {
            q.asyncAfter(deadline: .now() + 0.6) { [weak self] in
                guard let self = self else { return }
                self.lock.lock(); let last = self.lastPB; self.lock.unlock()
                if let last = last { self.encode(last, pts: CMClockGetTime(CMClockGetHostTimeClock()), ptsUs: agentClockUs(), key: true) }
            }
        }
    }

    private func encode(_ pb: CVPixelBuffer, pts: CMTime, ptsUs: UInt64, key: Bool) {
        lock.lock(); let s = session; lock.unlock()
        guard let sess = s else { return }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        let wid = UInt64(windowID), ew = UInt16(w), eh = UInt16(h)
        VTCompressionSessionEncodeFrame(sess, imageBuffer: pb, presentationTimeStamp: pts,
                                        duration: .invalid,
                                        frameProperties: key ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue] as CFDictionary : nil,
                                        infoFlagsOut: nil) { [weak self] status, _, out in
            guard let self = self else { return }
            guard status == noErr, let out = out, CMSampleBufferDataIsReady(out) else {
                if status != noErr && !self.loggedEncodeError { self.loggedEncodeError = true; log("encoder error \(status) window=\(wid) \(ew)x\(eh)") }
                return
            }
            let a = CMSampleBufferGetSampleAttachmentsArray(out, createIfNecessary: false) as? [[CFString: Any]]
            let key = (a?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true
            guard let data = annexB(out, keyframe: key) else { return }
            self.noteFrameSize(data.count, keyframe: key)
            self.sent += 1
            self.onPacket(VideoPacket(windowID: wid, ptsMicros: ptsUs, keyframe: key, width: ew, height: eh, data: data))
        }
    }
}
