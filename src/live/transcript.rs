//! Rolling transcript of both speakers, grouped into turns. GPT-Live emits
//! timed fragments and no turn boundaries, so the grouping lives here.

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::User => "User",
            Role::Assistant => "You (voice)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Turn {
    pub role: Role,
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub last_update: Instant,
}

/// Fragments closer than this (session-timeline ms) join the previous turn.
const JOIN_GAP_MS: u64 = 1_500;
const MAX_TURNS: usize = 400;

#[derive(Debug, Default)]
pub struct Transcript {
    turns: Vec<Turn>,
    /// Index into `turns` where the most recent delegation prompt ended.
    mark: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, role: Role, delta: &str, start_ms: Option<u64>, end_ms: Option<u64>) {
        let start = start_ms.unwrap_or_else(|| self.turns.last().map(|t| t.end_ms).unwrap_or(0));
        let end = end_ms.unwrap_or(start);
        let joins = match self.turns.last() {
            Some(last) => last.role == role && start.saturating_sub(last.end_ms) <= JOIN_GAP_MS,
            None => false,
        };
        if joins {
            let last = self.turns.last_mut().unwrap();
            if !last.text.is_empty() && !last.text.ends_with(' ') && !delta.starts_with(' ') {
                last.text.push(' ');
            }
            last.text.push_str(delta);
            last.end_ms = last.end_ms.max(end);
            last.last_update = Instant::now();
        } else {
            self.turns.push(Turn {
                role,
                text: delta.trim_start().to_string(),
                start_ms: start,
                end_ms: end,
                last_update: Instant::now(),
            });
            if self.turns.len() > MAX_TURNS {
                let drop = self.turns.len() - MAX_TURNS;
                self.turns.drain(..drop);
                self.mark = self.mark.saturating_sub(drop);
            }
        }
    }

    #[cfg(test)]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    pub fn last_assistant_text(&self) -> Option<String> {
        self.turns.iter().rev().find(|t| t.role == Role::Assistant).map(|t| t.text.clone())
    }

    pub fn last_user_turn(&self) -> Option<&Turn> {
        self.turns.iter().rev().find(|t| t.role == Role::User)
    }

    /// Everything since the last mark, plus up to `context` earlier turns.
    pub fn render_for_delegation(&self, context: usize) -> String {
        let start = self.mark.saturating_sub(context);
        let mut out = String::new();
        for (i, t) in self.turns.iter().enumerate().skip(start) {
            if i == self.mark && self.mark > start {
                out.push_str("--- new request ---\n");
            }
            out.push_str(t.role.label());
            out.push_str(": ");
            out.push_str(t.text.trim());
            out.push('\n');
        }
        out
    }

    pub fn mark_delegated(&mut self) {
        self.mark = self.turns.len();
    }

    /// Compact summary for reconnects: last `n` turns, plain text.
    pub fn tail(&self, n: usize) -> String {
        let start = self.turns.len().saturating_sub(n);
        self.turns[start..]
            .iter()
            .map(|t| format!("{}: {}", t.role.label(), t.text.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// True when an utterance is clearly an instruction to do something on the
/// computer ("open Safari", "can you switch to the other tab, please").
/// Used as a safety net when the voice model answers without delegating.
pub fn is_command(text: &str) -> bool {
    if is_stop_phrase(text) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let cleaned: String = lower.chars().map(|c| if c.is_ascii_alphanumeric() || c == ' ' || c == '\'' { c } else { ' ' }).collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    const FILLERS: &[&str] = &["um", "uh", "hmm", "okay", "ok", "so", "now", "and", "then", "please", "hey", "thursday", "agent", "yeah", "yes", "alright", "well", "just", "also"];
    while let Some(w) = words.first() {
        if FILLERS.contains(w) { words.remove(0); } else { break; }
    }
    // "can you …", "could you …", "would you …", "i want you to …", "i need you to …"
    for lead in [&["can", "you"][..], &["could", "you"], &["would", "you"], &["will", "you"], &["i", "want", "you", "to"], &["i", "need", "you", "to"], &["i'd", "like", "you", "to"]] {
        if words.len() > lead.len() && words[..lead.len()] == *lead {
            words.drain(..lead.len());
            break;
        }
    }
    while let Some(w) = words.first() {
        if FILLERS.contains(w) { words.remove(0); } else { break; }
    }
    const VERBS: &[&str] = &[
        "open", "close", "quit", "switch", "go", "navigate", "click", "press", "scroll", "type", "write", "play", "pause",
        "search", "create", "make", "build", "change", "add", "remove", "delete", "show", "find", "run", "start", "stop",
        "move", "resize", "minimize", "maximize", "reload", "refresh", "bring", "put", "set", "turn", "launch", "focus",
        "select", "copy", "paste", "save", "rename", "fix", "update", "install", "check", "look", "read", "tell",
    ];
    match words.first() {
        Some(w) if words.len() >= 2 => VERBS.contains(w),
        _ => false,
    }
}

/// True when a completed user utterance is a bare stop command.
pub fn is_stop_phrase(text: &str) -> bool {
    let t = text
        .trim()
        .trim_end_matches(['.', '!', '?'])
        .to_ascii_lowercase();
    matches!(
        t.as_str(),
        "stop" | "stop it" | "cancel" | "cancel that" | "abort" | "never mind" | "nevermind"
            | "forget it" | "halt" | "stop that" | "stop working"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_fragments_into_turns() {
        let mut t = Transcript::new();
        t.push(Role::User, "open the", Some(0), Some(400));
        t.push(Role::User, " readme", Some(450), Some(800));
        t.push(Role::Assistant, "Sure", Some(1200), Some(1500));
        t.push(Role::User, "thanks", Some(9000), Some(9300));
        assert_eq!(t.turns().len(), 3);
        assert_eq!(t.turns()[0].text, "open the readme");
        assert_eq!(t.turns()[2].text, "thanks");
    }

    #[test]
    fn delegation_render_marks_new_request() {
        let mut t = Transcript::new();
        t.push(Role::User, "hello", Some(0), Some(100));
        t.mark_delegated();
        t.push(Role::User, "show me the tests", Some(5000), Some(5600));
        let r = t.render_for_delegation(5);
        assert!(r.contains("--- new request ---"));
        assert!(r.ends_with("User: show me the tests\n"));
    }

    #[test]
    fn recognises_commands() {
        assert!(is_command("Please open Safari for me"));
        assert!(is_command("Um, switch to Safari please"));
        assert!(is_command("Can you make the world in red"));
        assert!(is_command("Okay. Create a new hello world sample application and open it here"));
        assert!(is_command("Hey thursday-agent, open Safari"));
        assert!(is_command("Thursday agent, can you open Safari"));
        assert!(!is_command("Hello, how are you"));
        assert!(!is_command("This looks good"));
        assert!(!is_command("I don't see Safari"));
        assert!(!is_command("Stop"));
    }

    #[test]
    fn stop_phrases() {
        assert!(is_stop_phrase("Stop."));
        assert!(is_stop_phrase("never mind"));
        assert!(!is_stop_phrase("stop at the second paragraph"));
        for phrase in ["Stop.", "stop it", "stop that", "stop working", "cancel that", "never mind"] {
            assert!(!is_command(phrase), "{phrase} must not start new work");
        }
        assert!(is_command("stop the music"));
    }
}
