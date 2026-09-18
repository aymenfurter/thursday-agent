import Foundation

@main
enum ScreenFeedLifecycleTests {
    static func main() {
        var feed = ScreenFeedLifecycle<String>()
        assert(!feed.isActive)
        let first = feed.beginStart()!
        assert(feed.isActive, "Startup itself must be stoppable before any display has registered.")
        assert(feed.beginStart() == nil, "Concurrent start requests must share one startup.")
        assert(feed.stop().isEmpty)
        assert(!feed.isActive && !feed.isCurrent(first))
        assert(!feed.register("late display", for: 1, generation: first),
               "A completed asynchronous lookup must not restart capture after stop.")

        let second = feed.beginStart()!
        assert(second != first)
        assert(!feed.finishStart(first), "A stale completion must not clear a newer startup.")
        assert(feed.isStarting)
        assert(feed.register("display 1", for: 1, generation: second))
        assert(feed.register("display 2", for: 2, generation: second))
        assert(Set(feed.stop()) == Set(["display 1", "display 2"]),
               "Stopping or failing startup must return every capture for teardown, including pending ones.")
        assert(feed.captures.isEmpty && !feed.isActive)
        assert(!feed.isCurrent(second),
               "A stream whose startCapture finishes after stop must be stopped again.")
        assert(!feed.register("stale display", for: 1, generation: second))
        assert(feed.stop().isEmpty, "Repeated stop must not return captures from an older generation.")

        let third = feed.beginStart()!
        assert(feed.register("replacement display", for: 3, generation: third))
        assert(feed.finishStart(third))
        assert(!feed.isStarting && feed.isActive)
        assert(feed.beginStart() == nil)
        assert(!feed.isCurrent(second), "Old stream errors must not tear down replacement streams.")
        assert(!feed.register("late registration", for: 4, generation: third))
        assert(feed.captures.count == 1)
        assert(feed.stop() == ["replacement display"])

        let empty = feed.beginStart()!
        assert(feed.finishStart(empty))
        assert(!feed.isActive && feed.beginStart() != nil,
               "A lookup with no displays must allow a later retry.")
        print("Screen feed lifecycle tests passed.")
    }
}
