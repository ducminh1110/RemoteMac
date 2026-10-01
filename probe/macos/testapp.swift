// Minimal Cocoa app used as a deterministic probe target: one titled window with
// a text view that is first responder. Launched by executable path from a shell,
// exactly like the agent will launch real applications.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.regular)

let win = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 480, height: 320),
                   styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
win.title = "RM Test App"
let scroll = NSScrollView(frame: win.contentView!.bounds)
scroll.autoresizingMask = [.width, .height]
scroll.hasVerticalScroller = true
let tv = NSTextView(frame: scroll.bounds)
tv.autoresizingMask = [.width, .height]
tv.font = NSFont.systemFont(ofSize: 22)
scroll.documentView = tv
win.contentView!.addSubview(scroll)
win.makeKeyAndOrderFront(nil)
win.makeFirstResponder(tv)
app.activate(ignoringOtherApps: true)
app.run()
