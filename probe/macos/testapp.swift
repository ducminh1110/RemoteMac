// Minimal Cocoa app used as a deterministic probe target: one titled window with
// a text view that is first responder. Launched by executable path from a shell,
// exactly like the agent will launch real applications.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.regular)

// Minimal menu so standard key equivalents (Cmd+A) have something to dispatch to, like a real app.
let mainMenu = NSMenu()
let appItem = NSMenuItem(); mainMenu.addItem(appItem); appItem.submenu = NSMenu()
let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: ""); mainMenu.addItem(editItem)
let editMenu = NSMenu(title: "Edit"); editItem.submenu = editMenu
editMenu.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
editMenu.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
editMenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
app.mainMenu = mainMenu

let win = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 480, height: 320),
                   styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
win.title = "RM Test App"
let content = win.contentView!.bounds
let stripH: CGFloat = 24
let scroll = NSScrollView(frame: NSRect(x: 0, y: stripH, width: content.width, height: content.height - stripH))
scroll.autoresizingMask = [.width, .height]
scroll.hasVerticalScroller = true
let tv = NSTextView(frame: scroll.bounds)
tv.autoresizingMask = [.width, .height]
tv.font = NSFont.systemFont(ofSize: 22)
scroll.documentView = tv
win.contentView!.addSubview(scroll)
// 60 Hz animated strip: gives ScreenCaptureKit a steady stream of damaged frames.
let ticker = NSView(frame: NSRect(x: 0, y: 0, width: content.width, height: stripH))
ticker.autoresizingMask = [.width, .maxYMargin]
ticker.wantsLayer = true
win.contentView!.addSubview(ticker)
var hue: CGFloat = 0
Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { _ in
    hue = (hue + 0.01).truncatingRemainder(dividingBy: 1)
    ticker.layer?.backgroundColor = NSColor(hue: hue, saturation: 1, brightness: 1, alpha: 1).cgColor
}
final class TitleMirror: NSObject, NSTextViewDelegate {
    func textDidChange(_ notification: Notification) { win.title = "RM Test App [\(tv.string.count) chars]" }
}
let mirror = TitleMirror()
tv.delegate = mirror
win.title = "RM Test App [0 chars]"
win.makeKeyAndOrderFront(nil)
win.makeFirstResponder(tv)
app.activate(ignoringOtherApps: true)
app.run()
