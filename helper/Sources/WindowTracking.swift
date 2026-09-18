import CoreGraphics
import Foundation

struct WindowTracking {
    private(set) var frame: CGRect?
    private var windowID: CGWindowID?
    private var previousTarget: CGRect?
    private var lastUpdate: TimeInterval?

    mutating func update(window: CGWindowID, target: CGRect, at time: TimeInterval) -> CGRect {
        let moved = windowID == window && previousTarget != target
        var next = target
        // Ease focus changes, but never ease movement or resizing of the same window.
        if let current = frame, !moved {
            let elapsed = max(0, time - (lastUpdate ?? time))
            let k = CGFloat(1 - pow(0.75, elapsed * 60))
            next = CGRect(
                x: current.minX + (target.minX - current.minX) * k,
                y: current.minY + (target.minY - current.minY) * k,
                width: current.width + (target.width - current.width) * k,
                height: current.height + (target.height - current.height) * k
            )
            let error = max(abs(next.minX - target.minX), abs(next.minY - target.minY),
                            abs(next.width - target.width), abs(next.height - target.height))
            if error < 0.25 { next = target }
        }
        windowID = window
        previousTarget = target
        lastUpdate = time
        frame = next
        return next
    }

    mutating func reset() {
        self = WindowTracking()
    }
}
