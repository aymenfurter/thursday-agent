import Foundation

@main
enum ProtocolTests {
    static func main() {
        assert(Incoming.parse(#"{"t":"postit","text":"Old note","app":"Safari","seconds":25}"#) == nil,
               "The helper must not accept custom note commands.")

        guard case let .highlight(app, title, seconds)? = Incoming.parse(#"{"t":"highlight","app":"Safari","title":"Docs","seconds":3}"#) else {
            fatalError("Window highlight commands must still be accepted.")
        }
        assert(app == "Safari" && title == "Docs" && seconds == 3)

        guard case let .highlight(app, title, seconds)? = Incoming.parse(#"{"t":"highlight"}"#) else {
            fatalError("Default window highlight commands must still be accepted.")
        }
        assert(app == "frontmost" && title == nil && seconds == 8)

        guard case .clear? = Incoming.parse(#"{"t":"clear"}"#) else {
            fatalError("Clearing window highlights must still be supported.")
        }
        print("Helper protocol tests passed.")
    }
}
