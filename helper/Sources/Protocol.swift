import Foundation

/// Messages from the Rust core (one JSON object per line on stdin).
enum Incoming {
    case energy(input: Double, output: Double, state: String)
    case highlight(app: String, title: String?, seconds: Double)
    case clear
    case focus(app: String?, title: String?)
    case glance(app: String?, title: String?)
    case quit

    static func parse(_ line: String) -> Incoming? {
        guard let data = line.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let t = obj["t"] as? String else { return nil }
        switch t {
        case "energy":
            return .energy(
                input: (obj["in"] as? Double) ?? 0,
                output: (obj["out"] as? Double) ?? 0,
                state: (obj["state"] as? String) ?? "idle")
        case "highlight":
            return .highlight(
                app: (obj["app"] as? String) ?? "frontmost",
                title: obj["title"] as? String,
                seconds: (obj["seconds"] as? Double) ?? 8)
        case "clear": return .clear
        case "focus": return .focus(app: obj["app"] as? String, title: obj["title"] as? String)
        case "glance": return .glance(app: obj["app"] as? String, title: obj["title"] as? String)
        case "quit": return .quit
        default: return nil
        }
    }
}

enum Outgoing {
    static func send(_ obj: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: obj),
              var s = String(data: data, encoding: .utf8) else { return }
        s.append("\n")
        FileHandle.standardOutput.write(s.data(using: .utf8)!)
    }
    static func ready(accessibility: Bool, screen: Bool) {
        send(["t": "ready", "permissions": ["accessibility": accessibility, "screen": screen]])
    }
    static func error(_ msg: String) { send(["t": "error", "msg": msg]) }
    static func ok(_ what: String) { send(["t": "ok", "what": what]) }
}
