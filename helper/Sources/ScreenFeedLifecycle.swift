import CoreGraphics
import Foundation

/// Mutated under ScreenFeed's lock. A generation identifies one asynchronous startup.
struct ScreenFeedLifecycle<Capture> {
    private(set) var generation: UUID?
    private(set) var isStarting = false
    private(set) var captures: [CGDirectDisplayID: Capture] = [:]

    var isActive: Bool { isStarting || !captures.isEmpty }

    mutating func beginStart() -> UUID? {
        guard !isActive else { return nil }
        let token = UUID()
        generation = token
        isStarting = true
        return token
    }

    func isCurrent(_ token: UUID) -> Bool { generation == token }

    mutating func register(_ capture: Capture, for display: CGDirectDisplayID, generation token: UUID) -> Bool {
        guard isCurrent(token), isStarting else { return false }
        captures[display] = capture
        return true
    }

    @discardableResult
    mutating func finishStart(_ token: UUID) -> Bool {
        guard isCurrent(token) else { return false }
        isStarting = false
        return true
    }

    mutating func stop() -> [Capture] {
        let running = Array(captures.values)
        generation = nil
        isStarting = false
        captures.removeAll()
        return running
    }
}
