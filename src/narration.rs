//! Turns Copilot tool calls into short first-person phrases. Used for the
//! progress log that the fast session reads when the user asks how a long
//! task is going.

use serde_json::Value;

use crate::text::{base_name, clip};

/// "reading README.md", "running `cargo test`", "clicking Save".
pub fn narrate_tool(name: &str, args: &Value) -> String {
    let first = |keys: &[&str]| -> Option<String> {
        keys.iter().find_map(|k| match args.get(*k) {
            Some(Value::String(t)) if !t.trim().is_empty() => Some(t.clone()),
            Some(v) if !v.is_null() && !v.is_string() => Some(v.to_string()),
            _ => None,
        })
    };
    let short = |t: String, n: usize| clip(&t, n);
    let base = |t: String| base_name(&t).to_string();
    let with = |verb: &str, keys: &[&str], n: usize, fallback: &str| -> String {
        match first(keys) {
            Some(v) => format!("{verb} {}", short(v, n)),
            None => fallback.to_string(),
        }
    };
    // Shell calls usually carry a human-written description ("Run integration
    // suite and check local app"): that is the best progress line there is.
    if let Some(Value::String(desc)) = args.get("description") {
        let desc = desc.trim();
        if desc.len() > 6 && !matches!(name, "task") {
            return short(desc.to_string(), 90);
        }
    }
    let n = name.trim_start_matches("peekaboo-");
    match n {
        "view" | "read_file" | "cat" => first(&["path", "file", "filePath"])
            .map(|p| {
                let name = base(p);
                // No extension: a folder.
                if name.contains('.') { format!("reading {name}") } else { format!("looking through the {name} folder") }
            })
            .unwrap_or_else(|| "reading a file".into()),
        "apply_patch" | "patch" => {
            let files = patch_files(args);
            if files.is_empty() {
                "editing files".into()
            } else {
                let parts: Vec<String> = files.iter().take(3).map(|(verb, p)| format!("{verb} {}", base(p.clone()))).collect();
                let more = if files.len() > 3 { format!(" and {} more", files.len() - 3) } else { String::new() };
                format!("{}{more}", parts.join(", "))
            }
        }
        "create" | "create_file" | "write_file" => first(&["path", "file", "filePath"])
            .map(|p| format!("creating {}", base(p)))
            .unwrap_or_else(|| "creating a file".into()),
        "edit" | "edit_file" => first(&["path", "file", "filePath"])
            .map(|p| format!("editing {}", base(p)))
            .unwrap_or_else(|| "editing a file".into()),
        "bash" | "shell" | "run_in_terminal" | "run_command" => first(&["command", "cmd"])
            .map(|c| format!("running `{}`", short(c, 48)))
            .unwrap_or_else(|| "running a command".into()),
        "grep" | "glob" | "search" | "find" => with("searching for", &["pattern", "query", "glob"], 40, "searching the workspace"),
        "web_fetch" | "fetch" | "web_search" => first(&["url", "query"])
            .map(|u| format!("looking up {}", short(u.replace("https://", "").replace("http://", ""), 40)))
            .unwrap_or_else(|| "looking something up on the web".into()),
        "task" => with("handing off:", &["description", "prompt"], 50, "handing part of the work to a helper"),
        "see" | "look_at_screen" | "image" | "capture" | "inspect_ui" => "taking a look at the screen".into(),
        "verify_state" => "checking the result on screen".into(),
        "browser" => "checking the browser".into(),
        "focus_window" => with("moving to", &["app"], 30, "moving to another window"),
        "click" | "action" | "set_value" => with("clicking", &["target", "element", "on", "id", "label"], 30, "clicking"),
        "type" | "paste" => with("typing", &["text", "content"], 30, "typing"),
        "press" => with("pressing", &["keys", "key", "chord"], 20, "pressing a key"),
        "scroll" | "drag" => "scrolling".into(),
        "app" | "window" | "space" | "dock" => with("switching to", &["name", "app", "to", "bundleId"], 30, "switching windows"),
        "menu" | "dialog" => "using a menu".into(),
        "show_file" => first(&["path"])
            .map(|p| format!("opening {} on the screen", base(p)))
            .unwrap_or_else(|| "opening a file on the screen".into()),
        "highlight_window" => "pointing at it on the screen".into(),
        "clear_annotations" => "clearing the window highlights".into(),
        "tell_user" => "telling the user something".into(),
        "start_deep_task" => "starting a longer task".into(),
        "deep_task_status" => "checking on the longer task".into(),
        "continue_deep_task" => "passing the answer to the longer task".into(),
        "cancel_deep_task" => "cancelling the longer task".into(),
        other => format!("using {}", other.replace('_', " ")),
    }
}

/// `(verb, path)` for every file named in an `apply_patch` call ("*** Add File: x").
pub fn patch_files(args: &Value) -> Vec<(&'static str, String)> {
    let text = match args {
        Value::String(t) => t.as_str(),
        other => ["input", "patch", "diff", "content"].iter().find_map(|k| other.get(*k).and_then(|v| v.as_str())).unwrap_or(""),
    };
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            [("*** Add File:", "creating"), ("*** Update File:", "editing"), ("*** Delete File:", "deleting")]
                .iter()
                .find_map(|(prefix, verb)| l.strip_prefix(prefix).map(|p| (*verb, p.trim().to_string())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_narration_describes_window_highlights() {
        assert_eq!(narrate_tool("highlight_window", &serde_json::json!({})), "pointing at it on the screen");
        assert_eq!(narrate_tool("clear_annotations", &serde_json::json!({})), "clearing the window highlights");
    }

    #[test]
    fn narration_uses_arguments() {
        assert_eq!(narrate_tool("view", &serde_json::json!({"path": "/x/y/README.md"})), "reading README.md");
        assert_eq!(narrate_tool("view", &serde_json::json!({"path": "/x/thursday"})), "looking through the thursday folder");
        assert_eq!(narrate_tool("bash", &serde_json::json!({"command": "cargo test --all"})), "running `cargo test --all`");
        assert_eq!(narrate_tool("bash", &serde_json::json!({"command": "ls", "description": "Inspect workspace structure"})), "Inspect workspace structure");
        assert_eq!(narrate_tool("create", &serde_json::json!({"path": "/x/index.html"})), "creating index.html");
        assert_eq!(
            narrate_tool("apply_patch", &serde_json::json!("*** Begin Patch\n*** Add File: a/app.js\n+1\n*** Update File: a/style.css\n")),
            "creating app.js, editing style.css"
        );
        assert_eq!(narrate_tool("peekaboo-click", &serde_json::json!({"target": "B3"})), "clicking B3");
        assert_eq!(narrate_tool("some_new_tool", &serde_json::json!({})), "using some new tool");
    }
}
