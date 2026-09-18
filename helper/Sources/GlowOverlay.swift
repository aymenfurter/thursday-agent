import AppKit
import MetalKit

/// Must match `struct U` in Shaders.metal.
struct Uniforms {
    var res: SIMD2<Float>
    var rectOrigin: SIMD2<Float>
    var rectSize: SIMD2<Float>
    var time: Float
    var level: Float
    var vis: Float
    var glance: Float
    var work: Float
    var listen: Float
    var scale: Float
    var hasTex: Float = 0
    var talkMode: Float = 0
    var levelFast: Float = 0
    var voicePhase: Float = 0
    var speak: Float = 0
    var beat: Float = 0
    var pad2: Float = 0
}

/// Compiles the shader library once and hands out pipelines per effect.
final class EffectLibrary {
    static let shared = EffectLibrary()
    let device: MTLDevice
    let queue: MTLCommandQueue
    private let library: MTLLibrary?
    private var pipelines: [Int: MTLRenderPipelineState] = [:]

    private init() {
        device = MTLCreateSystemDefaultDevice()!
        queue = device.makeCommandQueue()!
        var lib: MTLLibrary? = nil
        if let url = Bundle.main.url(forResource: "Shaders", withExtension: "metal")
            ?? URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
                .appendingPathComponent("../Resources/Shaders.metal") as URL?,
           let src = try? String(contentsOf: url, encoding: .utf8) {
            do { lib = try device.makeLibrary(source: src, options: nil) }
            catch { Outgoing.error("shader compile failed: \(error)") }
        } else {
            Outgoing.error("Shaders.metal not found")
        }
        library = lib
    }

    func pipeline(for effect: Int) -> MTLRenderPipelineState? {
        if let p = pipelines[effect] { return p }
        guard let lib = library, let v = lib.makeFunction(name: "vmain"), let f = lib.makeFunction(name: "fx\(effect)") else { return nil }
        let desc = MTLRenderPipelineDescriptor()
        desc.vertexFunction = v
        desc.fragmentFunction = f
        let att = desc.colorAttachments[0]!
        att.pixelFormat = .bgra8Unorm
        att.isBlendingEnabled = true
        att.rgbBlendOperation = .add
        att.alphaBlendOperation = .add
        att.sourceRGBBlendFactor = .one
        att.sourceAlphaBlendFactor = .one
        att.destinationRGBBlendFactor = .oneMinusSourceAlpha
        att.destinationAlphaBlendFactor = .oneMinusSourceAlpha
        guard let p = try? device.makeRenderPipelineState(descriptor: desc) else {
            Outgoing.error("pipeline failed for fx\(effect)")
            return nil
        }
        pipelines[effect] = p
        return p
    }
}

/// Transparent Metal view that renders the current effect every frame.
final class GlowView: MTKView, MTKViewDelegate {
    var effect: Int = 0
    var beforeDraw: (() -> Void)?
    var uniforms = Uniforms(res: .zero, rectOrigin: .zero, rectSize: .zero, time: 0, level: 0, vis: 0, glance: 0, work: 0, listen: 0, scale: 2)
    /// Display this view covers, to pick the matching screen texture.
    var displayID: CGDirectDisplayID = CGMainDisplayID()
    private lazy var blankTexture: MTLTexture = {
        let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 1, height: 1, mipmapped: false)
        d.usage = [.shaderRead]
        let t = EffectLibrary.shared.device.makeTexture(descriptor: d)!
        var px: [UInt8] = [255, 255, 255, 255]
        t.replace(region: MTLRegionMake2D(0, 0, 1, 1), mipmapLevel: 0, withBytes: &px, bytesPerRow: 4)
        return t
    }()

    init(frame: NSRect) {
        super.init(frame: frame, device: EffectLibrary.shared.device)
        delegate = self
        colorPixelFormat = .bgra8Unorm
        clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 0)
        framebufferOnly = true
        preferredFramesPerSecond = 60
        isPaused = false
        enableSetNeedsDisplay = false
        wantsLayer = true
        layer?.isOpaque = false
        (layer as? CAMetalLayer)?.isOpaque = false
    }

    required init(coder: NSCoder) { fatalError() }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let drawable = currentDrawable, let rpd = currentRenderPassDescriptor,
              let cb = EffectLibrary.shared.queue.makeCommandBuffer(),
              let enc = cb.makeRenderCommandEncoder(descriptor: rpd) else { return }
        beforeDraw?()
        if uniforms.vis > 0.004, let pipe = EffectLibrary.shared.pipeline(for: effect) {
            var u = uniforms
            u.res = SIMD2<Float>(Float(bounds.width), Float(bounds.height))
            u.scale = Float(window?.backingScaleFactor ?? 2)
            let live = ScreenFeed.shared.texture(for: displayID)
            u.hasTex = live == nil ? 0 : 1
            enc.setRenderPipelineState(pipe)
            enc.setFragmentBytes(&u, length: MemoryLayout<Uniforms>.stride, index: 0)
            enc.setFragmentTexture(live?.texture ?? blankTexture, index: 0)
            if let live {
                // Core Video's backing must outlive GPU use, not just encoding.
                cb.addCompletedHandler { _ in withExtendedLifetime(live) {} }
            }
            enc.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 3)
        }
        enc.endEncoding()
        cb.present(drawable)
        cb.commit()
    }
}

final class GlowWindow: NSWindow {
    let glowView: GlowView
    let screenFrame: NSRect

    init(screen: NSScreen) {
        screenFrame = screen.frame
        glowView = GlowView(frame: NSRect(origin: .zero, size: screen.frame.size))
        if let n = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber {
            glowView.displayID = CGDirectDisplayID(n.uint32Value)
        }
        super.init(contentRect: screen.frame, styleMask: .borderless, backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        // Normal level: the window is ordered directly behind the focus window,
        // whose real rounded corners then cut the light exactly.
        level = .normal
        ignoresMouseEvents = true
        hidesOnDeactivate = false
        collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        isReleasedWhenClosed = false
        contentView = glowView
        setFrame(screen.frame, display: true)
        // Not ordered in yet: it appears when it is first tucked behind a window.
    }

    /// Place this overlay immediately behind the given window of another app.
    func tuck(behind id: CGWindowID) {
        order(.below, relativeTo: Int(id))
    }
}

/// Owns one GlowWindow per screen and drives the animation state. The effect
/// is anchored to the "focus window": the app window the assistant is
/// currently working in.
final class GlowController {
    private var windows: [GlowWindow] = []
    private var timer: Timer?
    private var lookupTimer: Timer?
    private var level: CGFloat = 0
    private var vis: CGFloat = 0
    private var listen: CGFloat = 0
    private var work: CGFloat = 0
    private var glance: CGFloat = 0
    private var state = "idle"
    private var output: Double = 0
    private var input: Double = 0
    private var lastEnergyAt = Date()
    private var hiddenSince: Date? = Date()
    private let start = Date()
    private var levelFast: CGFloat = 0
    private var speakEnv: CGFloat = 0
    private var voicePhase: Double = 0
    /// App (and optional title) the assistant is working in. By default it is
    /// "at home" in whatever window is frontmost; a tool-driven focus overrides
    /// that for a while and then falls back.
    private var focusApp: String? = "frontmost"
    private var focusTitle: String?
    private var explicitFocusAt = Date.distantPast
    private let explicitFocusTTL: TimeInterval = 25
    /// Target rect in global AppKit coordinates.
    private var targetRect: NSRect?
    private var targetWindow: CGWindowID = 0
    private var tracking = WindowTracking()
    private var feedWantedSince: Date?
    private var lastOnset = Date.distantPast
    private var prevFast: CGFloat = 0
    init() {
        rebuild()
        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.rebuild() }
        timer = Timer.scheduledTimer(withTimeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(timer!, forMode: .common)
        lookupTimer = Timer.scheduledTimer(withTimeInterval: 0.12, repeats: true) { [weak self] _ in self?.lookupFocus() }
        RunLoop.main.add(lookupTimer!, forMode: .common)
    }

    /// The assistant is now working in this window (no flash).
    func focus(app: String?, title: String?) {
        focusApp = app ?? "frontmost"
        focusTitle = title
        explicitFocusAt = Date()
        lookupFocus()
    }

    /// The assistant just looked at this window: move there with a flash.
    func glance(app: String?, title: String?) {
        if let app { focusApp = app; focusTitle = title; explicitFocusAt = Date() }
        lookupFocus()
        glance = 1
    }

    private func lookupFocus() {
        // A tool-driven focus lapses back to the frontmost window once things go quiet.
        if focusApp != "frontmost", state != "working", state != "speaking",
           Date().timeIntervalSince(explicitFocusAt) > explicitFocusTTL {
            focusApp = "frontmost"; focusTitle = nil
        }
        guard let app = focusApp, let w = WindowLookup.window(app: app, title: focusTitle) else {
            targetRect = nil
            targetWindow = 0
            return
        }
        targetRect = w.frame
        targetWindow = w.id
        if feedWantedSince == nil { feedWantedSince = Date() }
        ScreenFeed.shared.start()
        // Re-order only when the overlay is not already right behind the focus
        // window; re-ordering on every tick makes the window server flicker.
        if w.id != 0, !w.tucked || windows.contains(where: { !$0.isVisible }) {
            windows.forEach { $0.tuck(behind: w.id) }
        }
    }

    /// True once live colours are available, or after a short wait when they never will be
    /// (no Screen Recording permission). Drawing before that would flash a fallback tint.
    private var coloursReady: Bool {
        if windows.contains(where: { ScreenFeed.shared.texture(for: $0.glowView.displayID) != nil }) { return true }
        guard let since = feedWantedSince else { return false }
        return Date().timeIntervalSince(since) > 1.5
    }

    func rebuild() {
        ScreenFeed.shared.stop()
        feedWantedSince = nil
        windows.forEach { $0.orderOut(nil) }
        windows = NSScreen.screens.map { GlowWindow(screen: $0) }
        for window in windows {
            window.glowView.beforeDraw = { [weak self, weak window] in
                guard let self, let window else { return }
                self.updateGeometry(for: window)
            }
        }
    }

    private func updateGeometry(for window: GlowWindow) {
        if targetWindow != 0 {
            if let frame = WindowLookup.frame(window: targetWindow) {
                targetRect = frame
                _ = tracking.update(window: targetWindow, target: frame, at: ProcessInfo.processInfo.systemUptime)
            } else {
                targetRect = nil
                targetWindow = 0
            }
        }
        if let frame = tracking.frame {
            let view = window.glowView
            // Global AppKit coordinates -> this display's bottom-left origin.
            view.uniforms.rectOrigin = SIMD2<Float>(
                Float(frame.minX - window.screenFrame.minX), Float(frame.minY - window.screenFrame.minY)
            )
            view.uniforms.rectSize = SIMD2<Float>(Float(frame.width), Float(frame.height))
        }
    }

    func update(input: Double, output: Double, state: String) {
        self.input = input
        self.output = output
        self.state = state
        lastEnergyAt = Date()
    }

    private func tick() {
        if Date().timeIntervalSince(lastEnergyAt) > 2.0 { state = "idle"; output = 0; input = 0 }
        let now = Date().timeIntervalSince(start)
        let speaking = state == "speaking"
        let rawLevel: CGFloat = speaking ? CGFloat(min(output, 1.0)) : 0
        level += (rawLevel - level) * (rawLevel > level ? 0.25 : 0.07)
        levelFast += (rawLevel - levelFast) * (rawLevel > levelFast ? 0.6 : 0.18)
        let speakTarget: CGFloat = speaking ? 1 : 0
        speakEnv += (speakTarget - speakEnv) * (speakTarget > speakEnv ? 0.10 : 0.035)
        // The animation clock only runs while it is actually speaking.
        voicePhase += (1.0 / 60.0) * (0.4 + 2.6 * Double(level)) * Double(speakEnv)
        // Heartbeat: a lub-dub envelope fired by each real syllable onset.
        if speaking, levelFast > 0.30, levelFast > prevFast + 0.04, Date().timeIntervalSince(lastOnset) > 0.26 {
            lastOnset = Date()
        }
        prevFast = levelFast
        let sinceOnset = Date().timeIntervalSince(lastOnset)
        let lub = exp(-sinceOnset * 9.0)
        let dub = 0.6 * exp(-pow((sinceOnset - 0.19) / 0.06, 2.0))
        let beat = CGFloat(min(1.0, lub + dub)) * speakEnv
        let listenTarget: CGFloat = state == "listening" ? CGFloat(min(input * 1.5, 1.0)) : 0
        listen += (listenTarget - listen) * 0.15
        let workTarget: CGFloat = state == "working" ? 1 : 0
        work += (workTarget - work) * 0.08
        glance = max(0, glance - CGFloat(1.0 / 60.0) / 1.4)

        // Presence: only while there is a window to anchor to.
        let visTarget: CGFloat = (targetRect != nil && coloursReady) ? 1 : 0
        vis += (visTarget - vis) * (visTarget > vis ? 0.12 : 0.06)
        if vis < 0.004 { vis = 0 }

        if targetRect == nil, vis == 0 {
            tracking.reset()
        }

        let visible = vis > 0.004
        // The colour effects need the live screen; capture only while something is shown.
        if visible {
            hiddenSince = nil
        } else {
            if hiddenSince == nil { hiddenSince = Date() }
            if let h = hiddenSince, Date().timeIntervalSince(h) > 8, targetRect == nil {
                if ScreenFeed.shared.isRunning { ScreenFeed.shared.stop() }
                feedWantedSince = nil
                windows.filter { $0.isVisible }.forEach { $0.orderOut(nil) }
            }
        }
        for w in windows {
            let v = w.glowView
            v.uniforms.time = Float(now)
            v.uniforms.level = Float(level)
            v.uniforms.vis = Float(vis)
            v.uniforms.glance = Float(glance)
            v.uniforms.work = Float(work)
            v.uniforms.listen = Float(listen)
            v.uniforms.levelFast = Float(levelFast)
            v.uniforms.voicePhase = Float(voicePhase.truncatingRemainder(dividingBy: 10_000))
            v.uniforms.speak = Float(speakEnv)
            v.uniforms.beat = Float(beat)
            if visible { v.isPaused = false } else if !v.isPaused { v.draw(); v.isPaused = true }
        }
    }

    func hide() {
        windows.forEach { $0.orderOut(nil) }
    }
}
