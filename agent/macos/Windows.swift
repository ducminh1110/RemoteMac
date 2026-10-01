// Polls the window server and turns changes into WINDOW_* events for windows owned by launched apps.
import Foundation
import CoreGraphics

struct WinInfo: Equatable { var id: CGWindowID, pid: pid_t, title: String, rect: CGRect }

func listWindows(pids: Set<pid_t>) -> [WinInfo] {
    let all = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    var out: [WinInfo] = []
    for w in all {
        guard let pid = w[kCGWindowOwnerPID as String] as? Int32, pids.contains(pid),
              (w[kCGWindowLayer as String] as? Int) == 0, let n = w[kCGWindowNumber as String] as? UInt32,
              let d = w[kCGWindowBounds as String] as? NSDictionary, let r = CGRect(dictionaryRepresentation: d as CFDictionary),
              r.width >= 64, r.height >= 64 else { continue }
        out.append(WinInfo(id: CGWindowID(n), pid: pid, title: w[kCGWindowName as String] as? String ?? "", rect: r))
    }
    return out
}

func rectJSON(_ r: CGRect) -> [String: Any] { ["x": Int(r.minX), "y": Int(r.minY), "w": Int(r.width), "h": Int(r.height)] }

final class WindowTracker {
    private var known: [CGWindowID: WinInfo] = [:]
    private let queue = DispatchQueue(label: "rm.windows")
    private var timer: DispatchSourceTimer?
    let apps: AppManager
    var onCreated: ((WinInfo, String) -> Void)?
    var onDestroyed: ((CGWindowID) -> Void)?
    var onMoved: ((WinInfo) -> Void)?       // position/size changed
    var onTitle: ((WinInfo) -> Void)?
    var onAppExited: ((String, Int32) -> Void)?

    init(apps: AppManager) { self.apps = apps }

    func current(_ id: CGWindowID) -> WinInfo? { queue.sync { known[id] } }

    func start() {
        let t = DispatchSource.makeTimerSource(queue: queue)
        t.schedule(deadline: .now(), repeating: .milliseconds(100))
        t.setEventHandler { [weak self] in self?.tick() }
        t.resume(); timer = t
    }

    private func tick() {
        let now = listWindows(pids: Set(apps.pids))
        let nowByID = Dictionary(uniqueKeysWithValues: now.map { ($0.id, $0) })
        for (id, old) in known where nowByID[id] == nil { known.removeValue(forKey: id); _ = old; onDestroyed?(id) }
        for w in now {
            if let old = known[w.id] {
                if old.rect != w.rect { known[w.id] = w; onMoved?(w) }
                if old.title != w.title { known[w.id] = w; onTitle?(w) }
            } else {
                known[w.id] = w
                onCreated?(w, apps.appID(forPid: w.pid) ?? "unknown")
            }
        }
        for (id, code) in apps.reapExited() { onAppExited?(id, code) }
    }
}
