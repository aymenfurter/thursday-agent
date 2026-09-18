import AppKit
import ApplicationServices

/// Window lookup and highlight outlines.
enum WindowLookup {
    static func runningApp(named name: String) -> NSRunningApplication? {
        if name.lowercased() == "frontmost" {
            return NSWorkspace.shared.frontmostApplication
        }
        let apps = NSWorkspace.shared.runningApplications.filter { $0.activationPolicy == .regular }
        let needle = name.lowercased()
        if let exact = apps.first(where: { $0.localizedName?.lowercased() == needle || $0.bundleIdentifier?.lowercased() == needle }) {
            return exact
        }
        return apps.first(where: { ($0.localizedName?.lowercased().contains(needle) ?? false) || ($0.bundleIdentifier?.lowercased().contains(needle) ?? false) })
    }

    /// Frame of the app's front window in AppKit (bottom-left origin) coordinates.
    static func frame(app name: String, title: String?) -> NSRect? {
        return window(app: name, title: title)?.frame
    }

    /// Refresh only the selected window; no application or z-order scan on the render path.
    static func frame(window id: CGWindowID) -> NSRect? {
        guard id != kCGNullWindowID,
              let list = CGWindowListCopyWindowInfo(.optionIncludingWindow, id) as? [[String: Any]],
              let info = list.first,
              (info[kCGWindowNumber as String] as? NSNumber)?.uint32Value == id,
              info[kCGWindowIsOnscreen as String] as? Bool == true,
              let bounds = bounds(of: info) else { return nil }
        return convertFromCG(bounds)
    }

    private static func bounds(of info: [String: Any]) -> CGRect? {
        guard let b = info[kCGWindowBounds as String] as? [String: CGFloat],
              let x = b["X"], let y = b["Y"], let width = b["Width"], let height = b["Height"],
              width > 80, height > 60 else { return nil }
        return CGRect(x: x, y: y, width: width, height: height)
    }

    /// Frame plus window-server id of the app's front window, and whether one of
    /// our own windows already sits directly behind it in the z-order.
    static func window(app name: String, title: String?) -> (frame: NSRect, id: CGWindowID, tucked: Bool)? {
        guard let app = runningApp(named: name) else { return nil }
        let pid = app.processIdentifier
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        let candidates = list.filter { info in
            guard let owner = info[kCGWindowOwnerPID as String] as? Int32, owner == pid else { return false }
            guard let layer = info[kCGWindowLayer as String] as? Int, layer == 0 else { return false }
            guard bounds(of: info) != nil else { return false }
            if let t = title?.lowercased(), !t.isEmpty {
                let wt = (info[kCGWindowName as String] as? String)?.lowercased() ?? ""
                return wt.contains(t)
            }
            return true
        }
        guard let info = candidates.first, let cg = bounds(of: info) else { return nil }
        let id = (info[kCGWindowNumber as String] as? NSNumber)?.uint32Value ?? 0
        // `list` is front-to-back: is the next normal-level window after the target ours?
        var tucked = false
        if let idx = list.firstIndex(where: { ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == id }) {
            let me = getpid()
            for next in list[(idx + 1)...] {
                guard let layer = next[kCGWindowLayer as String] as? Int, layer == 0 else { continue }
                tucked = (next[kCGWindowOwnerPID as String] as? Int32) == me
                break
            }
        }
        return (convertFromCG(cg), CGWindowID(id), tucked)
    }

    /// CoreGraphics uses a top-left origin on the primary display; AppKit uses bottom-left.
    static func convertFromCG(_ r: CGRect) -> NSRect {
        let primaryHeight = NSScreen.screens.first?.frame.height ?? 0
        return NSRect(x: r.origin.x, y: primaryHeight - r.origin.y - r.height, width: r.width, height: r.height)
    }
}

final class OutlineView: NSView {
    let color = NSColor(calibratedRed: 0.30, green: 0.74, blue: 1.00, alpha: 1)
    var pulse: CGFloat = 1
    override var isOpaque: Bool { false }
    override func draw(_ dirtyRect: NSRect) {
        let inset: CGFloat = 6
        let rect = bounds.insetBy(dx: inset, dy: inset)
        for i in 0..<6 {
            let f = CGFloat(i)
            let path = NSBezierPath(roundedRect: rect.insetBy(dx: -f * 1.8, dy: -f * 1.8), xRadius: 12 + f, yRadius: 12 + f)
            path.lineWidth = 3
            color.withAlphaComponent((0.9 - f * 0.14) * pulse).setStroke()
            path.stroke()
        }
    }
}

final class HighlightWindow: NSWindow {
    let view = OutlineView()
    private var timer: Timer?
    private var phase: Double = 0

    init(frame: NSRect, seconds: Double, onDone: @escaping () -> Void) {
        let padded = frame.insetBy(dx: -14, dy: -14)
        super.init(contentRect: padded, styleMask: .borderless, backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        level = .screenSaver
        ignoresMouseEvents = true
        collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        isReleasedWhenClosed = false
        view.frame = NSRect(origin: .zero, size: padded.size)
        contentView = view
        orderFrontRegardless()
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) { [weak self] _ in
            guard let self else { return }
            self.phase += 1.0 / 30.0
            self.view.pulse = CGFloat(0.65 + 0.35 * (sin(self.phase * 4.0) * 0.5 + 0.5))
            self.view.needsDisplay = true
        }
        RunLoop.main.add(timer!, forMode: .common)
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [weak self] in
            self?.dismiss()
            onDone()
        }
    }

    func dismiss() {
        timer?.invalidate()
        timer = nil
        orderOut(nil)
    }
}

final class AnnotationController {
    private var highlights: [HighlightWindow] = []

    func highlight(app: String, title: String?, seconds: Double) {
        guard let frame = WindowLookup.frame(app: app, title: title) else {
            Outgoing.error("no window found for \(app)")
            return
        }
        var w: HighlightWindow!
        w = HighlightWindow(frame: frame, seconds: seconds) { [weak self] in
            self?.highlights.removeAll { $0 === w }
        }
        highlights.append(w)
        Outgoing.ok("highlight")
    }

    func clear() {
        highlights.forEach { $0.dismiss() }
        highlights.removeAll()
        Outgoing.ok("clear")
    }
}
