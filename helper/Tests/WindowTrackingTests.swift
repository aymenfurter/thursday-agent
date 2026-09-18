import CoreGraphics
import Foundation

@main
enum WindowTrackingTests {
    static func main() {
        let initial = CGRect(x: 100, y: 200, width: 800, height: 600)
        let other = CGRect(x: -900, y: -100, width: 500, height: 400)

        for fps in [60.0, 120.0] {
            var tracking = WindowTracking()
            assert(tracking.update(window: 1, target: initial, at: 0) == initial)
            for index in 1...600 {
                let time = Double(index) / fps
                let delta = CGFloat(sin(time * 12) * 1_500)
                let target = CGRect(x: delta, y: -delta, width: 800 + CGFloat(index % 100), height: 600)
                assert(tracking.update(window: 1, target: target, at: time) == target,
                       "Same-window movement and resizing must not trail the latest frame.")
            }
        }

        var tracking = WindowTracking()
        _ = tracking.update(window: 1, target: initial, at: 0)
        let switched = tracking.update(window: 2, target: other, at: 1.0 / 60)
        assert(switched != initial && switched != other, "A focus change must still glide.")
        assert(switched.minX < initial.minX && switched.minX > other.minX)
        assert(tracking.update(window: 2, target: other, at: 1.0 / 60) == switched,
               "Drawing a second display at the same time must not advance the transition.")
        for index in 2...60 {
            _ = tracking.update(window: 2, target: other, at: Double(index) / 60)
        }
        assert(tracking.frame == other, "A focus transition must settle on the exact target.")
        let back = tracking.update(window: 1, target: initial, at: 61.0 / 60)
        assert(back != initial)
        let dragged = initial.offsetBy(dx: 900, dy: -800)
        assert(tracking.update(window: 1, target: dragged, at: 62.0 / 60) == dragged,
               "Dragging during a focus transition must attach the glow immediately.")
        tracking.reset()
        assert(tracking.frame == nil)
        assert(tracking.update(window: 3, target: other, at: 3) == other,
               "A new target after hiding must not glide from the old window.")

        var at60Hz = WindowTracking()
        var at120Hz = WindowTracking()
        _ = at60Hz.update(window: 1, target: initial, at: 0)
        _ = at120Hz.update(window: 1, target: initial, at: 0)
        for index in 1...12 {
            _ = at120Hz.update(window: 2, target: other, at: Double(index) / 120)
            if index % 2 == 0 {
                _ = at60Hz.update(window: 2, target: other, at: Double(index) / 120)
            }
        }
        assert(abs(at60Hz.frame!.minX - at120Hz.frame!.minX) < 0.0001,
               "Focus transition speed must not depend on the draw frequency.")

        compareWithPreviousTracking()
        print("Window tracking tests passed.")
    }

    static func compareWithPreviousTracking() {
        var tracking = WindowTracking()
        var oldDrawn: CGFloat = 0
        var oldError: CGFloat = 0
        var newError: CGFloat = 0
        let velocity: CGFloat = 1_200
        for index in 0..<300 {
            let time = Double(index) / 60
            let position = velocity * time
            let oldSample = velocity * (floor(time / 0.12) * 0.12)
            oldDrawn += (oldSample - oldDrawn) * 0.25
            let target = CGRect(x: position, y: 0, width: 800, height: 600)
            let drawn = tracking.update(window: 1, target: target, at: time)
            if index >= 60 {
                oldError += abs(position - oldDrawn)
                newError += abs(position - drawn.minX)
            }
        }
        assert(oldError / 240 > 100, "The baseline must reproduce the reported trailing effect.")
        assert(newError == 0, "Tracking must add no position error to the latest rendered sample.")
        print(String(format: "Modeled 1,200 pt/s movement: mean error %.1f pt before, %.1f pt after.",
                     Double(oldError / 240), Double(newError / 240)))
    }
}
