// Taro.app, the Enterprise edition (enterprise/docs/architecture/FLEET.md): the same program as
// the taro command, opened as an app (from Finder, or at login). A setup window asks for Screen
// Recording and Accessibility, then for an enrollment code made in the console; enrolled, the
// app lives in the menu bar, keeps a heartbeat with the server and runs the session the server
// asks for: this program again (--foreground), as the app's own child, so macOS counts the app's
// permissions for it. The server's password reaches it through the environment (RM_PASSWORD),
// never the command line; a new epoch restarts it with the new password.
import Foundation
import AppKit
import ApplicationServices
import CoreGraphics
import ServiceManagement

/// Started as the app: in an app bundle with no arguments (old macOS adds -psn_… when Finder
/// opens it), or with --enterprise-app. Never for the copies the CLI starts of itself.
func isEnterpriseAppLaunch() -> Bool {
    let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("-psn_") && $0 != "--logs-enabled" }
    if args == ["--enterprise-app"] { return true }
    let e = ProcessInfo.processInfo.environment
    return args.isEmpty && Bundle.main.bundleURL.pathExtension == "app" && e["RM_DAEMON"] == nil && e["RM_WORKER"] == nil
}

/// The app's one controller (AppKit keeps its targets weakly).
private var enterpriseApp: EnterpriseController?
/// The session's pid, for the SIGTERM handler (which cannot reach the app's objects).
private var enterpriseChildPID: pid_t = 0

func runEnterpriseApp() -> Never {
    // a write to a server or relay that is gone returns an error; by default it would kill us
    signal(SIGPIPE, SIG_IGN)
    // ended by a signal (not by Quit): the session ends with the app
    signal(SIGTERM) { _ in
        if enterpriseChildPID > 0 { kill(enterpriseChildPID, SIGTERM) }
        _exit(0)
    }
    if logsEnabled {
        // opened from Finder, stderr goes nowhere: the log file, as the CLI's in the background
        let dir = (backgroundLogPath as NSString).deletingLastPathComponent
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        _ = freopen(backgroundLogPath, "a", stderr)
    }
    let app = NSApplication.shared
    let c = EnterpriseController()
    enterpriseApp = c
    DispatchQueue.main.async { c.start() }
    app.run()
    exit(0)
}

private var permissionsGranted: Bool { CGPreflightScreenCaptureAccess() && AXIsProcessTrusted() }

/// System Settings > Privacy & Security at one of its panes (Privacy_ScreenCapture, …).
private func openPrivacyPane(_ anchor: String) {
    if let u = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") { NSWorkspace.shared.open(u) }
}

// ---- the session -------------------------------------------------------------------------------

/// What the session runs with.
struct SessionPlan: Equatable {
    let password: String
    let epoch: Int
    /// host:port; nil: the provider's (or the built-in) relay
    let relay: String?
}

/// The session as a child process of the app; started again 2 s after it ends on its own.
/// Everything here runs on the main queue.
final class SessionSupervisor {
    private var child: Process?
    /// what the session should run with (nil: none)
    private var wanted: SessionPlan?
    /// what the running child runs with
    private var running: SessionPlan?
    /// the child is being ended on purpose; `afterEnd` runs once it has
    private var ending = false
    private var afterEnd: (() -> Void)?
    /// cancels a start that was planned (a later plan, or stop)
    private var generation = 0

    /// Run the session with `plan`: started, or restarted when it runs with something else (a new
    /// epoch: any viewer is disconnected). true: a session was (re)started.
    @discardableResult
    func apply(_ plan: SessionPlan) -> Bool {
        wanted = plan
        if child != nil, !ending, running == plan { return false }
        generation += 1
        endChild { [weak self] in self?.launch() }
        return true
    }

    /// No session any more.
    func stop() {
        wanted = nil
        generation += 1
        endChild {}
    }

    /// At quit: the child ends now (SIGTERM, SIGKILL after 3 s).
    func stopNow() {
        wanted = nil
        generation += 1
        guard let p = child, p.isRunning else { return }
        p.terminate()
        for _ in 0..<30 where p.isRunning { usleep(100_000) }
        if p.isRunning { kill(p.processIdentifier, SIGKILL) }
        enterpriseChildPID = 0
    }

    /// End the child (SIGTERM, then SIGKILL after 3 s), then `next`.
    private func endChild(then next: @escaping () -> Void) {
        guard let p = child else { next(); return }
        afterEnd = next
        if ending { return }
        ending = true
        if p.isRunning { p.terminate() }
        let pid = p.processIdentifier
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            guard let s = self, let c = s.child, c === p, p.isRunning else { return }
            log("enterprise: the session did not end within 3 s: killing it")
            kill(pid, SIGKILL)
        }
    }

    private func launch() {
        guard let plan = wanted, child == nil, let exe = Bundle.main.executablePath else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        var args = ["--foreground"]
        if let r = plan.relay { args += ["--relay", r] }
        if logsEnabled { args.append("--logs-enabled") }
        p.arguments = args
        var e = ProcessInfo.processInfo.environment
        for k in ["RM_ID", "RM_RESTARTED", "RM_DAEMON", "RM_WORKER", "RM_SESSION_TOKEN"] { e.removeValue(forKey: k) }
        // the password: in the environment only (the command line is visible to every user)
        e["RM_PASSWORD"] = plan.password
        e["RM_QUIET_BANNER"] = "1"
        p.environment = e
        p.standardInput = FileHandle.nullDevice
        // its banner shows the password: never kept anywhere
        p.standardOutput = FileHandle.nullDevice
        p.standardError = logsEnabled ? FileHandle.standardError : FileHandle.nullDevice
        p.terminationHandler = { [weak self] proc in DispatchQueue.main.async { self?.ended(proc) } }
        do {
            try p.run()
        } catch {
            log("enterprise: could not start the session (\(error)); trying again in 2 s")
            retryLater()
            return
        }
        child = p
        running = plan
        enterpriseChildPID = p.processIdentifier
        log("enterprise: session started (pid \(p.processIdentifier), epoch \(plan.epoch), relay \(plan.relay ?? "the provider's"))")
    }

    private func ended(_ p: Process) {
        guard p === child else { return }
        child = nil
        running = nil
        enterpriseChildPID = 0
        if ending {
            ending = false
            let next = afterEnd
            afterEnd = nil
            next?()
            return
        }
        log("enterprise: the session ended (status \(p.terminationStatus)): starting it again in 2 s")
        retryLater()
    }

    private func retryLater() {
        generation += 1
        let g = generation
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) { [weak self] in
            guard let s = self, s.generation == g, s.child == nil, !s.ending else { return }
            s.launch()
        }
    }
}

// ---- the app -----------------------------------------------------------------------------------

/// The app: enrollment, heartbeat, session, menu bar item and setup window. Main queue only.
final class EnterpriseController: NSObject {
    private var enrollment: Enrollment?
    private var appliedEpoch = 0
    private let session = SessionSupervisor()
    private var setup: SetupWindow?
    private var statusItem: NSStatusItem?
    private var stateItem: NSMenuItem?
    private var idItem: NSMenuItem?
    /// the heartbeat runs; a new generation cancels the one planned
    private var heartbeatOn = false
    private var beatGeneration = 0
    private var failures = 0
    /// the last heartbeat was answered
    private var connected = false
    /// the relay the server gave (nil: the provider's)
    private var lastRelay: String?
    private var activity: NSObjectProtocol?
    private var ticker: Timer?

    /// The server this Mac is enrolled on (the setup window shows it).
    var enrolledServer: String? { enrollment?.server }

    func start() {
        // never napped: the session must answer viewers at once (as the CLI's)
        activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiatedAllowingIdleSystemSleep, .latencyCritical], reason: "\(productName) keeps this Mac reachable")
        NotificationCenter.default.addObserver(self, selector: #selector(willTerminate(_:)), name: NSApplication.willTerminateNotification, object: nil)
        NSApp.mainMenu = makeMainMenu()
        enrollment = loadEnrollment()
        if let e = enrollment { appliedEpoch = loadAppliedEpoch(for: e) }
        let t = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(t, forMode: .common)
        ticker = t
        if enrollment != nil { showStatusItem() }
        if enrollment != nil && permissionsGranted {
            NSApp.setActivationPolicy(.accessory)
            beginHeartbeat()
        } else {
            showSetup(message: nil)
        }
        updateStatus()
    }

    /// Each second: the setup window's steps, the session once all is there, the menu.
    private func tick() {
        setup?.refresh()
        if enrollment != nil && permissionsGranted && !heartbeatOn { beginHeartbeat() }
        updateStatus()
    }

    // ---- heartbeat (FLEET.md: POST /api/v1/agent/heartbeat) -----------------------------------

    private func beginHeartbeat() {
        heartbeatOn = true
        failures = 0
        beatGeneration += 1
        beat(beatGeneration)
    }

    private func stopHeartbeat() {
        heartbeatOn = false
        beatGeneration += 1
    }

    private func beat(_ g: Int) {
        guard g == beatGeneration, let e = enrollment else { return }
        var body: [String: Any] = ["applied_epoch": appliedEpoch, "version": appVersion]
        if let id = currentTaroID() { body["taro_id"] = id }
        fleetRequest(server: e.server, path: "/api/v1/agent/heartbeat", body: body, token: e.token) { [weak self] answer in
            guard let s = self, g == s.beatGeneration else { return }
            s.heard(answer, g)
        }
    }

    private func heard(_ answer: FleetAnswer, _ g: Int) {
        guard let e = enrollment else { return }
        switch answer {
        case .ok(let m):
            guard let password = m["password"] as? String, password.count >= 4, let epoch = (m["epoch"] as? NSNumber)?.intValue else {
                retry(g, "the server's answer has no session password")
                return
            }
            let relay = (m["relay"] as? String)?.trimmingCharacters(in: .whitespaces)
            let interval = max(2, min(300, (m["heartbeat_seconds"] as? NSNumber)?.intValue ?? 10))
            failures = 0
            connected = true
            lastRelay = relay?.isEmpty == false ? relay : nil
            let restarted = session.apply(SessionPlan(password: password, epoch: epoch, relay: lastRelay))
            if appliedEpoch != epoch {
                appliedEpoch = epoch
                saveAppliedEpoch(epoch, for: e)
            }
            updateStatus()
            // a session (re)started: its epoch (and the relay's ID) told soon, not a whole interval later
            after(restarted ? 2 : interval, g)
        case .unauthorized:
            log("enterprise: the server no longer knows this Mac (401): it was removed")
            try? SMAppService.mainApp.unregister()
            removed(message: "This Mac was removed from the server. To use it again, enroll it with a new code from the console (Macs > Add a Mac).")
        case .refused(let status, let message):
            retry(g, "the server answered \(status): \(message)")
        case .unreachable(let why):
            retry(g, why)
        }
    }

    /// The heartbeat failed: the session goes on as it is; again after 2, 4, 8 … 60 s.
    private func retry(_ g: Int, _ why: String) {
        failures += 1
        connected = false
        let delay = min(60, 1 << min(failures, 6))
        log("enterprise: heartbeat failed (\(why)); again in \(delay) s")
        updateStatus()
        after(delay, g)
    }

    private func after(_ seconds: Int, _ g: Int) {
        DispatchQueue.main.asyncAfter(deadline: .now() + .seconds(seconds)) { [weak self] in self?.beat(g) }
    }

    /// The ID the relay gave this Mac (kept by the session in relay-ids.json, Session.swift), or
    /// this Mac's own when there is no relay at all.
    private func currentTaroID() -> String? {
        if let r = lastRelay ?? defaultRelay { return savedRelayID(r) }
        return persistentID()
    }

    // ---- enrollment ---------------------------------------------------------------------------

    /// The setup window enrolled this Mac (already kept in the Keychain).
    func enrolled(_ e: Enrollment) {
        enrollment = e
        appliedEpoch = 0
        connected = false
        do { try SMAppService.mainApp.register() } catch { log("enterprise: could not open at login (\(error))") }
        showStatusItem()
        if permissionsGranted { beginHeartbeat() }
        updateStatus()
    }

    /// Not enrolled any more (removed on the server, or here): no session, setup again.
    private func removed(message: String?) {
        stopHeartbeat()
        session.stop()
        forgetEnrollment()
        enrollment = nil
        appliedEpoch = 0
        connected = false
        lastRelay = nil
        if let item = statusItem { NSStatusBar.system.removeStatusItem(item) }
        statusItem = nil
        stateItem = nil
        idItem = nil
        showSetup(message: message)
    }

    // ---- windows and menus --------------------------------------------------------------------

    func showSetup(message: String?) {
        NSApp.setActivationPolicy(.regular)
        if setup == nil { setup = SetupWindow(controller: self) }
        setup?.show(message: message)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// The setup window was closed: to the menu bar, or (not enrolled) nothing left to do.
    func setupClosed() {
        if enrollment == nil {
            NSApp.terminate(nil)
            return
        }
        NSApp.setActivationPolicy(.accessory)
    }

    /// Screen Recording counts only for a process started after it was allowed: start the app
    /// again (a new copy once this one has quit).
    func relaunch() {
        session.stopNow()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = ["-c", "sleep 1; /usr/bin/open -n \"$0\"", Bundle.main.bundlePath]
        do { try p.run() } catch { log("enterprise: could not start again (\(error))"); return }
        NSApp.terminate(nil)
    }

    private func showStatusItem() {
        guard statusItem == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let b = item.button {
            if let img = NSImage(systemSymbolName: "display", accessibilityDescription: productName) {
                img.isTemplate = true
                b.image = img
            } else {
                b.title = productName
            }
            b.toolTip = productName
        }
        let menu = NSMenu()
        menu.autoenablesItems = false
        func info(_ title: String) -> NSMenuItem {
            let i = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            i.isEnabled = false
            menu.addItem(i)
            return i
        }
        func action(_ title: String, _ sel: Selector) {
            let i = NSMenuItem(title: title, action: sel, keyEquivalent: "")
            i.target = self
            menu.addItem(i)
        }
        _ = info(productName)
        stateItem = info("")
        idItem = info("")
        menu.addItem(NSMenuItem.separator())
        action("Open setup…", #selector(openSetup(_:)))
        action("Remove this Mac…", #selector(removeThisMac(_:)))
        menu.addItem(NSMenuItem.separator())
        action("Quit \(productName)", #selector(quit(_:)))
        item.menu = menu
        statusItem = item
    }

    private func updateStatus() {
        guard statusItem != nil else { return }
        let text: String
        if !permissionsGranted {
            text = "Permissions needed"
        } else if connected, let e = enrollment {
            text = "Connected to \(URL(string: e.server)?.host ?? e.server)"
        } else {
            text = "Waiting for the server"
        }
        if stateItem?.title != text { stateItem?.title = text }
        if let id = currentTaroID() {
            idItem?.title = "ID: \(displayID(id))"
            idItem?.isHidden = false
        } else {
            idItem?.isHidden = true
        }
    }

    private func makeMainMenu() -> NSMenu {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Close Window", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        appMenu.addItem(withTitle: "Quit \(productName)", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        main.addItem(appItem)
        // (the code is often pasted: the fields need Edit's shortcuts)
        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        main.addItem(editItem)
        return main
    }

    @objc private func openSetup(_ sender: Any?) { showSetup(message: nil) }

    @objc private func removeThisMac(_ sender: Any?) {
        NSApp.activate(ignoringOtherApps: true)
        let a = NSAlert()
        a.messageText = "Remove this Mac from the server?"
        a.informativeText = "\(productName) forgets its enrollment on this Mac and ends its session: nobody can connect to it through the server any more. An administrator removes it from the console's list of Macs. To use it again, enroll it with a new code."
        a.addButton(withTitle: "Remove")
        a.addButton(withTitle: "Cancel")
        guard a.runModal() == .alertFirstButtonReturn else { return }
        try? SMAppService.mainApp.unregister()
        removed(message: nil)
    }

    @objc private func quit(_ sender: Any?) { NSApp.terminate(nil) }

    @objc private func willTerminate(_ n: Notification) { session.stopNow() }
}

// ---- the setup window --------------------------------------------------------------------------

/// The three steps (Screen Recording, Accessibility, the server), with their status live.
final class SetupWindow: NSObject {
    private weak var controller: EnterpriseController?
    private let window: NSWindow
    private let notice: NSTextField
    private let screenStatus: NSTextField
    private let axStatus: NSTextField
    private let serverField: NSTextField
    private let codeField: NSTextField
    private let connectStatus: NSTextField
    private let enrolledStatus: NSTextField
    private let doneStatus: NSTextField
    private let screenButton: NSButton
    private let restartButton: NSButton
    private let axButton: NSButton
    private let connectButton: NSButton
    private let doneButton: NSButton
    /// step 3's fields (hidden once enrolled)
    private let form: NSStackView
    private var connecting = false

    init(controller: EnterpriseController) {
        self.controller = controller
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 560), styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
        notice = NSTextField(wrappingLabelWithString: "")
        screenStatus = NSTextField(labelWithString: "")
        axStatus = NSTextField(labelWithString: "")
        serverField = NSTextField(string: "")
        codeField = NSTextField(string: "")
        connectStatus = NSTextField(wrappingLabelWithString: "")
        enrolledStatus = NSTextField(wrappingLabelWithString: "")
        doneStatus = NSTextField(wrappingLabelWithString: "")
        screenButton = NSButton(title: "Allow Screen Recording…", target: nil, action: #selector(SetupWindow.allowScreen(_:)))
        restartButton = NSButton(title: "Restart \(productName)", target: nil, action: #selector(SetupWindow.restart(_:)))
        axButton = NSButton(title: "Allow Accessibility…", target: nil, action: #selector(SetupWindow.allowAccessibility(_:)))
        connectButton = NSButton(title: "Connect", target: nil, action: #selector(SetupWindow.connect(_:)))
        doneButton = NSButton(title: "Done", target: nil, action: #selector(SetupWindow.done(_:)))
        form = NSStackView()
        super.init()
        for b in [screenButton, restartButton, axButton, connectButton, doneButton] { b.target = self }
        build()
        NotificationCenter.default.addObserver(self, selector: #selector(closed(_:)), name: NSWindow.willCloseNotification, object: window)
    }

    private func wrapping(_ s: String, secondary: Bool = true) -> NSTextField {
        let l = NSTextField(wrappingLabelWithString: s)
        l.preferredMaxLayoutWidth = 500
        if secondary { l.textColor = .secondaryLabelColor }
        return l
    }

    private func title(_ s: String) -> NSTextField {
        let l = NSTextField(labelWithString: s)
        l.font = .systemFont(ofSize: 15, weight: .semibold)
        return l
    }

    private func row(_ views: [NSView]) -> NSStackView {
        let r = NSStackView(views: views)
        r.orientation = .horizontal
        r.spacing = 10
        return r
    }

    private func field(_ label: String, _ f: NSTextField) -> NSStackView {
        let l = NSTextField(labelWithString: label)
        l.widthAnchor.constraint(equalToConstant: 120).isActive = true
        f.widthAnchor.constraint(equalToConstant: 340).isActive = true
        return row([l, f])
    }

    private func build() {
        window.title = "\(productName) Setup"
        window.isReleasedWhenClosed = false
        let brand = providerColor()
        let header = NSTextField(labelWithString: productName)
        header.font = .systemFont(ofSize: 26, weight: .bold)
        header.textColor = brand ?? .labelColor
        let intro = wrapping("Three steps make this Mac available through your server: allow \(productName) to see and control the screen, then connect it with an enrollment code from the console (Macs > Add a Mac).")
        notice.preferredMaxLayoutWidth = 500
        notice.textColor = .systemRed
        notice.isHidden = true
        for l in [connectStatus, enrolledStatus, doneStatus] { l.preferredMaxLayoutWidth = 500 }

        serverField.placeholderString = "https://taro.example.com"
        serverField.stringValue = providerPortal() ?? ""
        codeField.placeholderString = "ABCD-EFGH-JKLM"
        connectButton.keyEquivalent = "\r"
        if let c = brand { connectButton.bezelColor = c }
        for v in [field("Server", serverField), field("Enrollment code", codeField), row([connectButton])] { form.addArrangedSubview(v) }
        form.orientation = .vertical
        form.alignment = .leading
        form.spacing = 8
        enrolledStatus.textColor = .systemGreen
        connectStatus.isHidden = true

        let screenText = wrapping("Lets \(productName) send this Mac's screen. macOS asks to restart \(productName) after it is allowed.")
        let axText = wrapping("Lets \(productName) use the keyboard and mouse for the person connected.")
        let serverText = wrapping("The server's address and a one-time code made in the console.")
        let screenRow = row([screenButton, restartButton]), axRow = row([axButton])
        let steps: [NSView] = [
            header, intro, notice,
            title("1. Screen Recording"), screenText, screenStatus, screenRow,
            title("2. Accessibility"), axText, axStatus, axRow,
            title("3. Connect to the server"), serverText, form, connectStatus, enrolledStatus,
            doneStatus, row([doneButton]),
        ]
        let stack = NSStackView(views: steps)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 24, left: 28, bottom: 24, right: 28)
        for v in [intro, notice, screenStatus, screenRow, axStatus, axRow, connectStatus, enrolledStatus] as [NSView] { stack.setCustomSpacing(18, after: v) }
        stack.setCustomSpacing(12, after: header)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            content.widthAnchor.constraint(equalToConstant: 560),
        ])
        window.contentView = content
        refresh()
    }

    /// Fit the window to what is shown (steps done hide their buttons).
    private func fit() {
        guard let content = window.contentView else { return }
        content.layoutSubtreeIfNeeded()
        let size = content.fittingSize
        guard abs(size.height - content.frame.height) > 0.5 else { return }
        // (the top edge stays where it is)
        var f = window.frame
        let wanted = window.frameRect(forContentRect: NSRect(origin: .zero, size: size))
        f.origin.y += f.height - wanted.height
        f.size = wanted.size
        window.setFrame(f, display: true)
    }

    func show(message: String?) {
        notice.stringValue = message ?? ""
        notice.isHidden = message == nil
        refresh()
        if !window.isVisible { window.center() }
        window.makeKeyAndOrderFront(nil)
    }

    /// The steps' status (each second while the app runs).
    func refresh() {
        let screen = CGPreflightScreenCaptureAccess(), ax = AXIsProcessTrusted()
        mark(screenStatus, screen, no: "Not allowed yet. Allow it in System Settings, then restart \(productName).")
        // (the buttons' row: hidden with them)
        screenButton.superview?.isHidden = screen
        mark(axStatus, ax, no: "Not allowed yet.")
        axButton.superview?.isHidden = ax
        let server = controller?.enrolledServer
        form.isHidden = server != nil
        enrolledStatus.isHidden = server == nil
        if let s = server {
            let text = "✓ This Mac is enrolled on \(s)."
            if enrolledStatus.stringValue != text { enrolledStatus.stringValue = text }
            if !connectStatus.isHidden { connectStatus.isHidden = true }
        }
        let all = screen && ax && server != nil
        let doneText = all ? "All set. \(productName) keeps running in the menu bar and opens at login." : ""
        if doneStatus.stringValue != doneText { doneStatus.stringValue = doneText }
        doneStatus.isHidden = !all
        doneButton.isEnabled = all
        fit()
    }

    private func mark(_ label: NSTextField, _ ok: Bool, no: String) {
        let text = ok ? "✓ Allowed" : no
        if label.stringValue != text { label.stringValue = text }
        label.textColor = ok ? .systemGreen : .secondaryLabelColor
    }

    private func say(_ s: String, error: Bool) {
        connectStatus.stringValue = s
        connectStatus.textColor = error ? .systemRed : .secondaryLabelColor
        connectStatus.isHidden = s.isEmpty
        fit()
    }

    @objc private func allowScreen(_ sender: Any?) {
        CGRequestScreenCaptureAccess()
        openPrivacyPane("Privacy_ScreenCapture")
    }

    @objc private func restart(_ sender: Any?) { controller?.relaunch() }

    @objc private func allowAccessibility(_ sender: Any?) {
        _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        openPrivacyPane("Privacy_Accessibility")
    }

    @objc private func connect(_ sender: Any?) {
        guard !connecting else { return }
        guard let server = normalizedServer(serverField.stringValue) else {
            say("Enter the server's address: https:// and its name, without a path (https://taro.example.com).", error: true)
            return
        }
        guard let code = normalizedEnrollmentCode(codeField.stringValue) else {
            say("The enrollment code has 12 letters and digits, like ABCD-EFGH-JKLM (console: Macs > Add a Mac).", error: true)
            return
        }
        serverField.stringValue = server
        codeField.stringValue = code
        connecting = true
        connectButton.isEnabled = false
        say("Connecting to \(server)…", error: false)
        var body = machineFacts()
        body["code"] = code
        fleetRequest(server: server, path: "/api/v1/agent/enroll", body: body, token: nil) { [weak self] answer in
            guard let s = self else { return }
            s.connecting = false
            s.connectButton.isEnabled = true
            switch answer {
            case .ok(let m):
                guard let id = m["agent_id"] as? String, !id.isEmpty, let token = m["token"] as? String, !token.isEmpty else {
                    s.say("The server's answer was not understood. Is this the right address?", error: true)
                    return
                }
                let e = Enrollment(server: server, agentID: id, token: token)
                guard saveEnrollment(e) else {
                    s.say("The Keychain did not keep the enrollment. Try again.", error: true)
                    return
                }
                s.say("", error: false)
                s.notice.isHidden = true
                s.codeField.stringValue = ""
                s.controller?.enrolled(e)
                s.refresh()
            case .unauthorized:
                s.say("The server refused this Mac.", error: true)
            case .refused(_, let message):
                s.say(message, error: true)
            case .unreachable(let why):
                s.say("Could not reach \(server): \(why)", error: true)
            }
        }
    }

    @objc private func done(_ sender: Any?) { window.close() }

    @objc private func closed(_ n: Notification) {
        // (after the window has gone: the controller may end the app)
        DispatchQueue.main.async { [weak self] in self?.controller?.setupClosed() }
    }
}
