// Probe: which ScreenCaptureKit filters make macOS put its window-sharing indicator (a grey
// capsule) where a window's traffic lights are. Captures the top left of TextEdit's window
// several ways and prints each as a picture in the log (SHARE<n>-PICTURE-BEGIN … -END).
import AppKit
import CoreImage
import CoreMedia
import Foundation
import ScreenCaptureKit

func picture(_ tag: String, _ img: CGImage?) {
    guard let img = img, let data = NSBitmapImageRep(cgImage: img).representation(using: .png, properties: [:]) else {
        print("\(tag): no picture")
        return
    }
    print("\(tag)-PICTURE-BEGIN \(img.width)x\(img.height)")
    let b = data.base64EncodedString()
    var i = b.startIndex
    while i < b.endIndex {
        let j = b.index(i, offsetBy: 76, limitedBy: b.endIndex) ?? b.endIndex
        print(b[i..<j])
        i = j
    }
    print("\(tag)-PICTURE-END")
}

final class Grab: NSObject, SCStreamOutput {
    var frame: CGImage?
    let got = DispatchSemaphore(value: 0)
    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, frame == nil, let pb = CMSampleBufferGetImageBuffer(sb) else { return }
        guard let att = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let st = att.first?[.status] as? Int, st == SCFrameStatus.complete.rawValue else { return }
        let ci = CIImage(cvPixelBuffer: pb)
        frame = CIContext().createCGImage(ci, from: ci.extent)
        got.signal()
    }
}

func config(_ rect: CGRect, _ scale: CGFloat) -> SCStreamConfiguration {
    let c = SCStreamConfiguration()
    c.sourceRect = rect
    c.width = Int(rect.width * scale)
    c.height = Int(rect.height * scale)
    c.showsCursor = false
    c.pixelFormat = kCVPixelFormatType_32BGRA
    return c
}

/// A stream of `filter` (left running), and its first whole frame.
func stream(_ filter: SCContentFilter, _ cfg: SCStreamConfiguration) async -> (SCStream?, CGImage?) {
    let g = Grab()
    let s = SCStream(filter: filter, configuration: cfg, delegate: nil)
    do {
        try s.addStreamOutput(g, type: .screen, sampleHandlerQueue: DispatchQueue(label: "probe"))
        try await s.startCapture()
    } catch {
        print("stream failed: \(error)")
        return (nil, nil)
    }
    // the indicator, if any, comes when the capture starts: give it time
    try? await Task.sleep(nanoseconds: 1_500_000_000)
    g.frame = nil
    _ = g.got.wait(timeout: .now() + 4)
    return (s, g.frame)
}

func shot(_ filter: SCContentFilter, _ cfg: SCStreamConfiguration) async -> CGImage? {
    do { return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: cfg) } catch {
        print("screenshot failed: \(error)")
        return nil
    }
}

func run() async {
    guard let content = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) else {
        print("no shareable content")
        return
    }
    let mine = content.windows.filter { $0.owningApplication?.bundleIdentifier == "com.apple.TextEdit" && $0.windowLayer == 0 && $0.frame.width > 200 }
    guard let w = mine.max(by: { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }),
          let app = w.owningApplication,
          let d = content.displays.first(where: { $0.frame.intersects(w.frame) }) ?? content.displays.first else {
        print("no TextEdit window")
        return
    }
    print("TextEdit window \(w.windowID) at \(w.frame) on display \(d.displayID) \(d.frame)")
    // the window's top left, in the window's and in the display's coordinates
    let local = CGRect(x: 0, y: 0, width: min(260, w.frame.width), height: 56)
    let onDisplay = local.offsetBy(dx: w.frame.minX - d.frame.minX, dy: w.frame.minY - d.frame.minY)
    let screen = SCContentFilter(display: d, excludingWindows: [])
    let scale = CGFloat(screen.pointPixelScale)

    picture("SHARE0", await shot(screen, config(onDisplay, scale)))
    // 1: the window alone (desktop independent), as MacBridge streams windows now
    let f1 = SCContentFilter(desktopIndependentWindow: w)
    let (s1, p1) = await stream(f1, config(local, scale))
    picture("SHARE1", p1)
    picture("SHARE1SCREEN", await shot(screen, config(onDisplay, scale)))
    try? await s1?.stopCapture()
    try? await Task.sleep(nanoseconds: 1_000_000_000)
    // 2: the display with only this window in it
    let f2 = SCContentFilter(display: d, including: [w])
    let (s2, p2) = await stream(f2, config(onDisplay, scale))
    picture("SHARE2", p2)
    picture("SHARE2SCREEN", await shot(screen, config(onDisplay, scale)))
    try? await s2?.stopCapture()
    try? await Task.sleep(nanoseconds: 1_000_000_000)
    // 3: the display with the app's windows in it
    let f3 = SCContentFilter(display: d, including: [app], exceptingWindows: [])
    let (s3, p3) = await stream(f3, config(onDisplay, scale))
    picture("SHARE3", p3)
    try? await s3?.stopCapture()
    // 4: a screenshot of the window alone (no stream)
    picture("SHARE4", await shot(f1, config(local, scale)))
}

let done = DispatchSemaphore(value: 0)
Task {
    await run()
    done.signal()
}
done.wait()
