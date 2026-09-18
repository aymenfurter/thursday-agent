//! Turn a Markdown-ish assistant answer into something a voice can say.

use crate::live::client::clamp;

pub fn for_speech(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_fence = false;
    let mut fence_seen = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if !in_fence && !fence_seen {
                out.push_str("(the code is on your screen) ");
                fence_seen = true;
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut l = trimmed.trim_start_matches('#').trim().to_string();
        for marker in ["- ", "* ", "+ ", "> "] {
            if let Some(rest) = l.strip_prefix(marker) {
                l = rest.to_string();
            }
        }
        // "1. item" -> "item"
        if let Some(idx) = l.find(". ") {
            if idx <= 3 && l[..idx].chars().all(|c| c.is_ascii_digit()) {
                l = l[idx + 2..].to_string();
            }
        }
        if l.is_empty() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            continue;
        }
        out.push_str(&strip_inline(&l));
        if !out.ends_with(['.', '!', '?', ':', ',']) {
            out.push('.');
        }
        out.push(' ');
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    clamp(&collapsed, max_chars)
}

fn strip_inline(s: &str) -> String {
    let mut r = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '*' | '_' if !r.ends_with(['\\']) => continue,
            '[' => {
                // [text](url) -> text
                let mut text = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    text.push(ch);
                }
                if closed && chars.peek() == Some(&'(') {
                    for ch in chars.by_ref() {
                        if ch == ')' {
                            break;
                        }
                    }
                }
                r.push_str(&text);
            }
            _ => r.push(c),
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markdown() {
        let md = "## Result\n\nI **opened** the file `README.md`.\n\n- first\n- second\n\n```rust\nfn x() {}\n```\nSee [docs](https://x.y).";
        let s = for_speech(md, 500);
        assert_eq!(
            s,
            "Result. I opened the file README.md. first. second. (the code is on your screen) See docs."
        );
    }

    #[test]
    fn clamps() {
        let long = "Sentence one is here. ".repeat(200);
        assert!(for_speech(&long, 100).chars().count() <= 101);
    }
}
