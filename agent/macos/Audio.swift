// The Mac's sound for the viewer. ScreenCaptureKit captures what the session's apps play
// (every app while the Mac Desktop is open), at 48 kHz stereo; it leaves as 16-bit PCM in 5 ms
// packets (crates/rm-protocol/src/audio.rs): over UDP while that path is alive, otherwise on the
// Audio channel (7) of the encrypted stream. Nothing is captured until the viewer asks
// ("audio_control"), and MacBridge's own sound is never captured.
import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreAudio

/// 5 ms at 48 kHz.
let audioPacketFrames = 240

final class AudioCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private let q = DispatchQueue(label: "rm.audio", qos: .userInteractive)
    private let lock = NSLock()
    private var stream: SCStream?
    /// samples (interleaved stereo) not yet sent, and the capture time of the first one
    private var pending: [Int16] = []
    private var pendingPts: UInt64 = 0
    private var seq: UInt32 = 0
    /// packets of digital silence in a row: after a second of it nothing is sent until sound
    /// comes back (an idle session costs no bandwidth)
    private var silentRun = 0
    private var loggedFormat = false
    /// what is captured: every app, or these processes only
    private var everything = false
    private var pids: Set<pid_t> = []
    /// the viewer asked for sound (capture restarts on its own while this holds)
    private var wanted = false
    private(set) var packetsSent = 0
    /// a packet's payload (crates/rm-protocol audio.rs)
    private let onPacket: (Data) -> Void
    /// "playing", "stopped", "unavailable" (+ why)
    var onStatus: ((String, String?) -> Void)?

    init(onPacket: @escaping (Data) -> Void) { self.onPacket = onPacket }

    /// The viewer wants sound (or not any more).
    func setWanted(_ on: Bool, everything e: Bool, pids p: Set<pid_t>) {
        lock.lock(); wanted = on; everything = e; pids = p; lock.unlock()
        restart()
    }

    /// The session's apps changed, or the Mac Desktop opened or closed.
    func update(everything e: Bool, pids p: Set<pid_t>) {
        lock.lock()
        let changed = e != everything || p != pids
        everything = e; pids = p
        let on = wanted
        lock.unlock()
        if on && changed { restart() }
    }

    /// One (re)start at a time; a request made meanwhile runs once the current one is done.
    private var busy = false, again = false
    private func restart() {
        lock.lock()
        if busy { again = true; lock.unlock(); return }
        busy = true
        lock.unlock()
        Task {
            while true {
                await stopStream()
                lock.lock(); let on = wanted; lock.unlock()
                if on { await startStream() } else { onStatus?("stopped", nil); log("sound: stopped") }
                lock.lock()
                let more = again
                again = false
                if !more { busy = false }
                lock.unlock()
                if !more { break }
            }
        }
    }

    private func stopStream() async {
        lock.lock(); let s = stream; stream = nil; lock.unlock()
        if let s = s { try? await s.stopCapture() }
        q.sync { pending.removeAll(); silentRun = 0 }
    }

    private func startStream() async {
        lock.lock(); let (all, set, on) = (everything, pids, wanted); lock.unlock()
        guard on else { return }
        if !CGPreflightScreenCaptureAccess() {
            onStatus?("unavailable", "Screen Recording is not allowed for MacBridge (System Settings > Privacy & Security > Screen Recording)")
            return
        }
        do {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            guard let display = content.displays.first else { onStatus?("unavailable", "no display to capture sound with"); return }
            let filter: SCContentFilter
            if all {
                filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])
            } else {
                let mine = content.applications.filter { set.contains($0.processID) }
                // no app open from the viewer yet: nothing to hear (and nothing sent)
                guard !mine.isEmpty else { log("sound: no app of the session to listen to yet"); onStatus?("playing", nil); return }
                filter = SCContentFilter(display: display, including: mine, exceptingWindows: [])
            }
            let cfg = SCStreamConfiguration()
            cfg.capturesAudio = true
            cfg.sampleRate = 48_000
            cfg.channelCount = 2
            cfg.excludesCurrentProcessAudio = true
            // the picture side of a sound-only stream: as small and as rare as it can be
            cfg.width = 2; cfg.height = 2
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: 1)
            cfg.queueDepth = 3
            let s = SCStream(filter: filter, configuration: cfg, delegate: self)
            try s.addStreamOutput(self, type: .audio, sampleHandlerQueue: q)
            // (pictures ignored; without an output for them ScreenCaptureKit logs each one dropped)
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: q)
            try await s.startCapture()
            lock.lock()
            let still = wanted
            if still { stream = s }
            lock.unlock()
            if !still { try? await s.stopCapture(); return }
            log("sound: capturing \(all ? "every app" : "\(set.count) app(s) of the session")")
            onStatus?("playing", nil)
        } catch {
            log("sound: capture failed: \(error)")
            onStatus?("unavailable", "\(error.localizedDescription)")
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        log("sound: capture stopped: \(error)")
        lock.lock(); let on = wanted; if self.stream === stream { self.stream = nil }; lock.unlock()
        guard on else { return }
        onStatus?("unavailable", "\(error.localizedDescription) (trying again)")
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) { [weak self] in self?.restart() }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio, sb.isValid, let fd = CMSampleBufferGetFormatDescription(sb),
              let asbdPtr = CMAudioFormatDescriptionGetStreamBasicDescription(fd) else { return }
        let asbd = asbdPtr.pointee
        let frames = CMSampleBufferGetNumSamples(sb)
        guard frames > 0, asbd.mChannelsPerFrame > 0 else { return }
        var needed = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sb, bufferListSizeNeededOut: &needed, bufferListOut: nil, bufferListSize: 0,
                                                                blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: nil)
        guard needed > 0 else { return }
        let raw = UnsafeMutableRawPointer.allocate(byteCount: needed, alignment: 16)
        defer { raw.deallocate() }
        let abl = raw.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        guard CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sb, bufferListSizeNeededOut: nil, bufferListOut: abl, bufferListSize: needed,
                                                                      blockBufferAllocator: nil, blockBufferMemoryAllocator: nil,
                                                                      flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, blockBufferOut: &block) == noErr else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(abl)
        let isFloat = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
        let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        let ch = Int(asbd.mChannelsPerFrame)
        let bits = Int(asbd.mBitsPerChannel)
        if !loggedFormat {
            loggedFormat = true
            log("sound: \(Int(asbd.mSampleRate)) Hz, \(ch) channel(s), \(bits)-bit \(isFloat ? "float" : "integer") \(planar ? "planar" : "interleaved")")
        }
        guard (isFloat && bits == 32) || (!isFloat && bits == 16) else { return }
        /// sample `i` of channel `c` (a mono source plays on both sides) as 16-bit
        func sample(_ c: Int, _ i: Int) -> Int16 {
            let src = min(c, ch - 1)
            let (buf, idx) = planar ? (src, i) : (0, i * ch + src)
            guard buf < buffers.count, let d = buffers[buf].mData else { return 0 }
            if isFloat {
                let v = d.assumingMemoryBound(to: Float32.self)[idx]
                return Int16(max(-1, min(1, v)) * 32767)
            }
            return d.assumingMemoryBound(to: Int16.self)[idx]
        }
        let cap = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sb))
        let ptsUs = cap.isFinite && cap > 0 ? UInt64(cap * 1_000_000) : agentClockUs()
        if pending.isEmpty { pendingPts = ptsUs }
        // ScreenCaptureKit was asked for 48 kHz; another rate is resampled (linear)
        let rate = asbd.mSampleRate > 0 ? asbd.mSampleRate : 48_000
        if abs(rate - 48_000) < 1 {
            pending.reserveCapacity(pending.count + frames * 2)
            for i in 0..<frames { pending.append(sample(0, i)); pending.append(sample(1, i)) }
        } else {
            let outFrames = Int(Double(frames) * 48_000 / rate)
            for o in 0..<outFrames {
                let pos = Double(o) * rate / 48_000, i = min(frames - 1, Int(pos)), j = min(frames - 1, i + 1), f = pos - Double(i)
                for c in 0..<2 { pending.append(Int16(Double(sample(c, i)) * (1 - f) + Double(sample(c, j)) * f)) }
            }
        }
        let per = audioPacketFrames * 2
        var offset = 0
        while pending.count - offset >= per {
            let chunk = pending[offset..<(offset + per)]
            let pts = pendingPts + UInt64(offset / 2) * 1_000_000 / 48_000
            offset += per
            if chunk.allSatisfy({ $0 == 0 }) {
                silentRun += 1
                if silentRun > 200 { continue } // a second of silence: nothing more until sound
            } else {
                silentRun = 0
            }
            var d = Data(capacity: 16 + per * 2)
            func be<T: FixedWidthInteger>(_ v: T) { var x = v.bigEndian; withUnsafeBytes(of: &x) { d.append(contentsOf: $0) } }
            be(seq); be(pts); d.append(1 /* s16le */); d.append(2); be(UInt16(audioPacketFrames))
            chunk.withUnsafeBufferPointer { p in
                for s in p { var le = s.littleEndian; withUnsafeBytes(of: &le) { d.append(contentsOf: $0) } }
            }
            seq &+= 1
            packetsSent += 1
            onPacket(d)
        }
        if offset > 0 {
            pending.removeFirst(offset)
            pendingPts += UInt64(offset / 2) * 1_000_000 / 48_000
        }
    }
}
