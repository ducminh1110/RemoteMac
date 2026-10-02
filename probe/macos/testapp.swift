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

// File > Open… (a real NSOpenPanel) and Window > About (a separate panel window), to exercise the
// agent's handling of dialogs and file panels the way real apps produce them.
final class Actions: NSObject {
    var about: NSPanel?
    @objc func openDocument(_ sender: Any?) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        panel.begin { resp in
            guard resp == .OK, let url = panel.url else { win.title = "RM Test App [open cancelled]"; return }
            let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)??.intValue ?? -1
            win.title = "RM Test App [opened \(url.lastPathComponent) \(size) bytes]"
        }
    }
    @objc func showAbout(_ sender: Any?) {
        let p = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 300, height: 160), styleMask: [.titled, .closable], backing: .buffered, defer: false)
        p.title = "About RM Test App"
        p.isReleasedWhenClosed = false
        let label = NSTextField(labelWithString: "RM Test App — remote window test")
        label.frame = NSRect(x: 20, y: 70, width: 260, height: 20)
        p.contentView?.addSubview(label)
        p.center()
        p.makeKeyAndOrderFront(nil)
        about = p
    }
}
let actions = Actions()
let fileItem = NSMenuItem(title: "File", action: nil, keyEquivalent: ""); mainMenu.insertItem(fileItem, at: 1)
let fileMenu = NSMenu(title: "File"); fileItem.submenu = fileMenu
let openItem = fileMenu.addItem(withTitle: "Open…", action: #selector(Actions.openDocument(_:)), keyEquivalent: "o"); openItem.target = actions
let aboutItem = fileMenu.addItem(withTitle: "About", action: #selector(Actions.showAbout(_:)), keyEquivalent: "i"); aboutItem.target = actions

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
