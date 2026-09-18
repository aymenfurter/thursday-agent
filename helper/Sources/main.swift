import AppKit

/// thursday-agent Helper: no Dock icon, no menu bar, no windows of its own except
/// the click-through overlays. Driven entirely over stdin.
final class Controller: NSObject, NSApplicationDelegate {
    private var glow: GlowController!
    private let annotations = AnnotationController()

    func applicationDidFinishLaunching(_ notification: Notification) {
        glow = GlowController()
        Outgoing.ready(accessibility: Permissions.accessibility, screen: Permissions.screenRecording)
        let thread = Thread { [weak self] in
            while let line = readLine(strippingNewline: true) {
                guard let msg = Incoming.parse(line) else { continue }
                DispatchQueue.main.async { self?.handle(msg) }
            }
            // stdin closed: the core is gone.
            DispatchQueue.main.async { NSApp.terminate(nil) }
        }
        thread.name = "stdin-reader"
        thread.start()
    }

    private func handle(_ msg: Incoming) {
        switch msg {
        case let .energy(input, output, state):
            glow.update(input: input, output: output, state: state)
        case let .highlight(app, title, seconds):
            annotations.highlight(app: app, title: title, seconds: seconds)
        case .clear:
            annotations.clear()
        case let .focus(app, title):
            glow.focus(app: app, title: title)
        case let .glance(app, title):
            glow.glance(app: app, title: title)
        case .quit:
            ScreenFeed.shared.stop()
            glow.hide()
            annotations.clear()
            NSApp.terminate(nil)
        }
    }
}

setvbuf(stdout, nil, _IOLBF, 0)
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let controller = Controller()
app.delegate = controller
app.run()
