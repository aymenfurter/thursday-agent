import AppKit
import CoreMedia
import CoreVideo
import Metal
import ScreenCaptureKit

/// Live, low-resolution capture of every display, as Metal textures. The
/// shaders sample it at the focus window's edge and extend those colours
/// outward. Our own overlay windows are excluded so the glow never feeds back.
final class ScreenFeed: NSObject, SCStreamDelegate {
    static let shared = ScreenFeed()

    private struct Capture {
        let stream: SCStream
        let sink: Sink
    }
    private var lifecycle = ScreenFeedLifecycle<Capture>()
    private var latest: [CGDirectDisplayID: (texture: MTLTexture, backing: CVMetalTexture)] = [:]
    private var cache: CVMetalTextureCache?
    private let lock = NSLock()
    private let queue = DispatchQueue(label: "thursday-agent.screenfeed", qos: .userInteractive)

    /// Frames per second and downscale factor: colours only, so small is fine.
    private let fps: Int32 = 15
    private let divisor = 3

    var isRunning: Bool { lock.withLock { lifecycle.isActive } }

    func texture(for display: CGDirectDisplayID) -> (texture: MTLTexture, backing: CVMetalTexture)? {
        lock.lock(); defer { lock.unlock() }
        return latest[display]
    }

    func start() {
        guard let generation = lock.withLock({ lifecycle.beginStart() }) else { return }
        guard CGPreflightScreenCaptureAccess() else {
            Outgoing.error("screen feed: Screen Recording permission is missing; falling back to palette colours")
            _ = stop(generation: generation)
            return
        }
        lock.withLock {
            if cache == nil {
                CVMetalTextureCacheCreate(kCFAllocatorDefault, nil, EffectLibrary.shared.device, nil, &cache)
            }
        }
        Task {
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
                let mine = content.applications.filter { $0.processID == getpid() }
                for display in content.displays {
                    guard self.isCurrent(generation) else { return }
                    let filter = SCContentFilter(display: display, excludingApplications: mine, exceptingWindows: [])
                    let cfg = SCStreamConfiguration()
                    cfg.width = max(320, display.width / self.divisor)
                    cfg.height = max(200, display.height / self.divisor)
                    cfg.minimumFrameInterval = CMTime(value: 1, timescale: self.fps)
                    cfg.pixelFormat = kCVPixelFormatType_32BGRA
                    cfg.showsCursor = false
                    cfg.queueDepth = 3
                    let stream = SCStream(filter: filter, configuration: cfg, delegate: self)
                    let sink = Sink(display: display.displayID, feed: self)
                    try stream.addStreamOutput(sink, type: .screen, sampleHandlerQueue: self.queue)
                    let capture = Capture(stream: stream, sink: sink)
                    guard self.register(capture, for: display.displayID, generation: generation) else { return }
                    do {
                        try await stream.startCapture()
                    } catch {
                        try? await stream.stopCapture()
                        throw error
                    }
                    // stopCapture may have run before startCapture finished.
                    guard self.isCurrent(generation) else {
                        try? await stream.stopCapture()
                        return
                    }
                }
                if self.lock.withLock({ self.lifecycle.finishStart(generation) }) {
                    Outgoing.ok("screen feed started (\(content.displays.count) display(s))")
                }
            } catch {
                if self.stop(generation: generation) {
                    Outgoing.error("screen feed: \(error.localizedDescription)")
                }
            }
        }
    }

    private func register(_ capture: Capture, for display: CGDirectDisplayID, generation: UUID) -> Bool {
        lock.withLock { lifecycle.register(capture, for: display, generation: generation) }
    }

    private func isCurrent(_ generation: UUID) -> Bool {
        lock.withLock { lifecycle.isCurrent(generation) }
    }

    func stop() {
        _ = stop(generation: nil)
    }

    @discardableResult
    private func stop(generation: UUID?) -> Bool {
        let running: [Capture]? = lock.withLock {
            if let generation, !lifecycle.isCurrent(generation) { return nil }
            latest.removeAll()
            return lifecycle.stop()
        }
        guard let running else { return false }
        for capture in running { capture.stream.stopCapture { _ in } }
        return true
    }

    fileprivate func receive(_ buffer: CMSampleBuffer, display: CGDirectDisplayID, stream: SCStream) {
        let activeCache: CVMetalTextureCache? = lock.withLock {
            guard lifecycle.captures[display]?.stream === stream else { return nil }
            return cache
        }
        guard let activeCache, let pixels = CMSampleBufferGetImageBuffer(buffer) else { return }
        var cvTex: CVMetalTexture?
        let w = CVPixelBufferGetWidth(pixels), h = CVPixelBufferGetHeight(pixels)
        CVMetalTextureCacheCreateTextureFromImage(kCFAllocatorDefault, activeCache, pixels, nil, .bgra8Unorm, w, h, 0, &cvTex)
        guard let cvTex, let tex = CVMetalTextureGetTexture(cvTex) else { return }
        lock.withLock {
            guard lifecycle.captures[display]?.stream === stream else { return }
            latest[display] = (tex, cvTex)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        let generation: UUID? = lock.withLock {
            guard lifecycle.captures.values.contains(where: { $0.stream === stream }) else { return nil }
            return lifecycle.generation
        }
        if let generation, stop(generation: generation) {
            Outgoing.error("screen feed stopped: \(error.localizedDescription)")
        }
    }

    /// One output object per display, because the callback does not say which display a frame is from.
    fileprivate final class Sink: NSObject, SCStreamOutput {
        let display: CGDirectDisplayID
        weak var feed: ScreenFeed?
        init(display: CGDirectDisplayID, feed: ScreenFeed) { self.display = display; self.feed = feed }
        func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
            guard type == .screen, sampleBuffer.isValid else { return }
            feed?.receive(sampleBuffer, display: display, stream: stream)
        }
    }
}
