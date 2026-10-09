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
    private func polish(_ pb: CVPixelBuffer, scale: CGFloat, hideButtons: Bool) {
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
        // the pixel diagonally inside the rounding
        for (left, top) in [(true, true), (false, true), (true, false), (false, false)] {
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
    private var bitrate = 20_000_000

    func requestKeyframe() { lock.lock(); forceKey = true; lock.unlock() }

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
            let apps = content.applications.filter { pids.contains($0.processID) }
            let s = SCStream(filter: SCContentFilter(display: d, including: apps, exceptingWindows: []), configuration: cfg, delegate: nil)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
            t0 = CFAbsoluteTimeGetCurrent()
            config = cfg
            try await s.startCapture()
            scStream = s
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
        let s = SCStream(filter: SCContentFilter(desktopIndependentWindow: w), configuration: cfg, delegate: nil)
        try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
        t0 = CFAbsoluteTimeGetCurrent()
        config = cfg
        try await s.startCapture()
        scStream = s
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
            polish(pb, scale: CGFloat(w) / pointsWide, hideButtons: inset == 0)
        }
        lock.lock(); lastPB = pb; lock.unlock()
        encode(pb, pts: CMSampleBufferGetPresentationTimeStamp(sb), ptsUs: ptsUs, key: key)
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
