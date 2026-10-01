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

final class WindowStream: NSObject, SCStreamOutput {
    let windowID: CGWindowID
    private let onPacket: (VideoPacket) -> Void
    private var scStream: SCStream?
    private var session: VTCompressionSession?
    private var encW = 0, encH = 0
    private let lock = NSLock()
    private var t0: CFAbsoluteTime = 0
    private(set) var sent = 0

    init(windowID: CGWindowID, onPacket: @escaping (VideoPacket) -> Void) { self.windowID = windowID; self.onPacket = onPacket }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let w = content.windows.first(where: { $0.windowID == windowID }) else { throw WireError(description: "window \(windowID) not shareable") }
        let cfg = SCStreamConfiguration()
        cfg.width = max(2, Int(w.frame.width)); cfg.height = max(2, Int(w.frame.height))
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        cfg.pixelFormat = kCVPixelFormatType_32BGRA
        cfg.queueDepth = 6; cfg.showsCursor = false
        let s = SCStream(filter: SCContentFilter(desktopIndependentWindow: w), configuration: cfg, delegate: nil)
        try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: DispatchQueue(label: "rm.capture.\(windowID)"))
        t0 = CFAbsoluteTimeGetCurrent()
        try await s.startCapture()
        scStream = s
    }

    func stop() async {
        if let s = scStream { try? await s.stopCapture() }
        scStream = nil
        lock.lock(); let sess = session; session = nil; lock.unlock()
        if let s = sess { VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid); VTCompressionSessionInvalidate(s) }
    }

    private func makeSession(_ w: Int, _ h: Int) -> VTCompressionSession? {
        var s: VTCompressionSession?
        guard VTCompressionSessionCreate(allocator: nil, width: Int32(w), height: Int32(h), codecType: kCMVideoCodecType_H264,
                                         encoderSpecification: nil, imageBufferAttributes: nil, compressedDataAllocator: nil,
                                         outputCallback: nil, refcon: nil, compressionSessionOut: &s) == noErr, let s = s else { return nil }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_Main_AutoLevel)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: 8_000_000 as CFNumber)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, value: 1 as CFNumber)
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
        lock.unlock()
        guard let sess = s, ok else { return }   // size changed: the owner restarts the stream
        let ptsUs = UInt64(max(0, (CFAbsoluteTimeGetCurrent() - t0) * 1_000_000))
        let wid = UInt64(windowID), ew = UInt16(w), eh = UInt16(h)
        VTCompressionSessionEncodeFrame(sess, imageBuffer: pb, presentationTimeStamp: CMSampleBufferGetPresentationTimeStamp(sb),
                                        duration: .invalid, frameProperties: nil, infoFlagsOut: nil) { [weak self] status, _, out in
            guard let self = self, status == noErr, let out = out, CMSampleBufferDataIsReady(out) else { return }
            let a = CMSampleBufferGetSampleAttachmentsArray(out, createIfNecessary: false) as? [[CFString: Any]]
            let key = (a?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) != true
            guard let data = annexB(out, keyframe: key) else { return }
            self.sent += 1
            self.onPacket(VideoPacket(windowID: wid, ptsMicros: ptsUs, keyframe: key, width: ew, height: eh, data: data))
        }
    }
}
