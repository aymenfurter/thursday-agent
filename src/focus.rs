//! Which window is the assistant working in? Inferred from tool calls so the
//! overlay can anchor to it without the model having to say so every time.

use serde_json::Value;

use crate::helper::HelperHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    /// The assistant is working in this app's window.
    Work { app: String, title: Option<String> },
    /// The assistant just looked at this window (flash, then stay there).
    Glance { app: String, title: Option<String> },
    /// Whatever window is in front right now.
    Frontmost,
}

/// Move the overlay to the window a tool call is about to touch.
pub fn apply(helper: &HelperHandle, tool: &str, args: &Value) {
    match infer(tool, args) {
        Some(Focus::Work { app, title }) => helper.focus(Some(&app), title.as_deref()),
        Some(Focus::Glance { app, title }) => helper.glance(Some(&app), title.as_deref()),
        Some(Focus::Frontmost) => helper.glance(Some("frontmost"), None),
        None => {}
    }
}

fn arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()))
}

/// Extract `tell application "X"` or `open -a "X"` / `open -a X` from a shell command.
pub fn app_in_command(cmd: &str) -> Option<String> {
    if let Some(i) = cmd.find("tell application \"") {
        let rest = &cmd[i + "tell application \"".len()..];
        if let Some(j) = rest.find('"') {
            let app = &rest[..j];
            if app != "System Events" {
                return Some(app.to_string());
            }
        }
    }
    if let Some(i) = cmd.find("open -a ") {
        let rest = cmd[i + 8..].trim_start();
        let app = if let Some(r) = rest.strip_prefix('"') {
            r.split('"').next().unwrap_or("")
        } else {
            rest.split_whitespace().next().unwrap_or("")
        };
        if !app.is_empty() {
            return Some(app.trim_matches('\'').to_string());
        }
    }
    None
}

pub fn infer(tool: &str, args: &Value) -> Option<Focus> {
    let n = tool.trim_start_matches("peekaboo-");
    let title = arg(args, &["window_title", "title"]).map(|s| s.to_string());
    match n {
        "see" | "image" | "inspect_ui" | "capture" => {
            let app = arg(args, &["app_target", "app", "app_name"])?;
            if app.eq_ignore_ascii_case("frontmost") || app.eq_ignore_ascii_case("screen") {
                return Some(Focus::Frontmost);
            }
            Some(Focus::Glance { app: app.to_string(), title })
        }
        "click" | "type" | "press" | "scroll" | "drag" | "menu" | "dialog" | "set_value" | "action" | "paste" => {
            arg(args, &["app", "app_target", "app_name"]).map(|a| Focus::Work { app: a.to_string(), title })
        }
        "app" | "window" => arg(args, &["name", "app", "app_target", "to", "bundle_id"]).map(|a| Focus::Work { app: a.to_string(), title }),
        "look_at_screen" => Some(Focus::Frontmost),
        "show_file" => Some(Focus::Frontmost),
        "highlight_window" => arg(args, &["app"]).map(|a| Focus::Glance { app: a.to_string(), title }),
        "focus_window" => arg(args, &["app"]).map(|a| Focus::Work { app: a.to_string(), title }),
        "run_command" | "bash" | "shell" => arg(args, &["command", "cmd"]).and_then(app_in_command).map(|a| Focus::Work { app: a, title: None }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_from_tools_and_commands() {
        assert_eq!(app_in_command("osascript -e 'tell application \"Safari\" to activate'"), Some("Safari".into()));
        assert_eq!(app_in_command("open -a \"Microsoft Edge\" https://x"), Some("Microsoft Edge".into()));
        assert_eq!(app_in_command("ls -la"), None);
        assert!(matches!(infer("peekaboo-see", &serde_json::json!({"app_target": "Safari"})), Some(Focus::Glance { .. })));
        assert!(matches!(infer("peekaboo-click", &serde_json::json!({"app": "Safari", "on": "elem_1"})), Some(Focus::Work { .. })));
        assert!(matches!(infer("run_command", &serde_json::json!({"command": "open -a Safari"})), Some(Focus::Work { .. })));
        assert!(matches!(infer("look_at_screen", &serde_json::json!({})), Some(Focus::Frontmost)));
    }
}
