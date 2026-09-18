//! Typed access to the JSON payload of Copilot session events.

use github_copilot_sdk::subscription::EventSubscription;
use github_copilot_sdk::types::SessionEvent;
use serde_json::Value;

pub trait EventData {
    fn text(&self, key: &str) -> String;
    fn int(&self, key: &str) -> i64;
    fn flag(&self, key: &str) -> bool;
    /// Arguments of a tool call (`Null` when absent).
    fn args(&self) -> &Value;
    /// Error of a finished tool call, if it failed.
    fn error(&self) -> Option<String>;
    /// Text output of a finished tool call.
    fn output(&self) -> &str;
}

impl EventData for Value {
    fn text(&self, key: &str) -> String {
        self.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
    }
    fn int(&self, key: &str) -> i64 {
        self.get(key).and_then(Value::as_i64).unwrap_or(0)
    }
    fn flag(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    fn args(&self) -> &Value {
        self.get("arguments").unwrap_or(&Value::Null)
    }
    fn error(&self) -> Option<String> {
        self.get("error").filter(|e| !e.is_null()).map(Value::to_string)
    }
    fn output(&self) -> &str {
        self.pointer("/result/content").and_then(Value::as_str).unwrap_or_default()
    }
}

/// Next event from a subscription, skipping lag notices; `None` when it ends.
pub async fn next_event(events: &mut EventSubscription) -> Option<SessionEvent> {
    loop {
        match events.recv().await {
            Ok(ev) => return Some(ev),
            Err(e) if e.kind().to_string().to_ascii_lowercase().contains("lag") => tracing::warn!("event subscription lagged: {e}"),
            Err(e) => {
                tracing::warn!("event subscription ended: {e}");
                return None;
            }
        }
    }
}
