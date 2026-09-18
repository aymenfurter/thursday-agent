//! Live debug console printed to the terminal: what the voice model says and
//! what we send it, which tools the Copilot sessions call, and a one-line
//! status of every session. Disabled with `--quiet`.

use parking_lot::Mutex;

use crate::text::clip;
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// The realtime (voice) model spoke.
    Voice,
    /// The user was heard.
    User,
    /// A delegation from the voice model.
    Deleg,
    /// Something we sent to the voice model.
    Send,
    /// The fast Copilot session.
    Fast,
    /// The deep Copilot session.
    Deep,
    /// Connection and lifecycle.
    Sys,
    Err,
}

impl Tag {
    fn label(self) -> (&'static str, &'static str) {
        match self {
            Tag::Voice => ("VOICE", "\x1b[36m"),
            Tag::User => ("USER ", "\x1b[32m"),
            Tag::Deleg => ("DELEG", "\x1b[35m"),
            Tag::Send => ("SEND ", "\x1b[33m"),
            Tag::Fast => ("FAST ", "\x1b[34m"),
            Tag::Deep => ("DEEP ", "\x1b[95m"),
            Tag::Sys => ("SYS  ", "\x1b[90m"),
            Tag::Err => ("ERR  ", "\x1b[31m"),
        }
    }
}

#[derive(Default)]
struct Status {
    voice: String,
    fast: String,
    deep: String,
    last_sent: String,
    last_recv: String,
}

struct Stream {
    tag: Tag,
    buf: String,
    last: Instant,
}

struct Inner {
    status: Status,
    streams: Vec<Stream>,
}

pub struct Console {
    enabled: bool,
    color: bool,
    inner: Mutex<Inner>,
    start: Instant,
}

static CONSOLE: OnceLock<Console> = OnceLock::new();

pub fn init(enabled: bool) {
    let c = Console {
        enabled,
        color: std::io::stdout().is_terminal(),
        inner: Mutex::new(Inner { status: Status::default(), streams: Vec::new() }),
        start: Instant::now(),
    };
    let _ = CONSOLE.set(c);
    if enabled {
        tokio::spawn(async {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tick.tick().await;
                get().flush_streams(Duration::from_millis(700));
            }
        });
    }
}

pub fn is_enabled() -> bool {
    get().enabled
}

fn get() -> &'static Console {
    CONSOLE.get_or_init(|| Console {
        enabled: false,
        color: false,
        inner: Mutex::new(Inner { status: Status::default(), streams: Vec::new() }),
        start: Instant::now(),
    })
}

const RESET: &str = "\x1b[0m";

impl Console {
    fn paint(&self, text: String, color: &str) -> String {
        if self.color { format!("{color}{text}{RESET}") } else { text }
    }

    fn stamp(&self) -> String {
        let e = self.start.elapsed();
        format!("{:>4}.{:03}s", e.as_secs(), e.subsec_millis())
    }

    fn print(&self, tag: Tag, text: &str) {
        let (label, color) = tag.label();
        let prefix = self.paint(format!("{} {label}", self.stamp()), color);
        println!("{prefix} {}", clip(text, 220));
    }

    fn flush_streams(&self, older_than: Duration) {
        let mut done = Vec::new();
        {
            let mut g = self.inner.lock();
            g.streams.retain(|s| {
                if s.last.elapsed() >= older_than {
                    done.push((s.tag, s.buf.clone()));
                    false
                } else {
                    true
                }
            });
        }
        for (tag, text) in done {
            self.print(tag, text.trim());
        }
    }
}

/// One line, printed immediately.
pub fn log(tag: Tag, text: impl AsRef<str>) {
    let c = get();
    if !c.enabled {
        return;
    }
    c.flush_streams(Duration::ZERO);
    c.print(tag, text.as_ref());
}

/// Fragmented text (transcripts) is buffered and printed as one line once it pauses.
pub fn stream(tag: Tag, delta: &str) {
    let c = get();
    if !c.enabled {
        return;
    }
    let mut g = c.inner.lock();
    match g.streams.iter_mut().find(|s| s.tag == tag) {
        Some(s) => {
            if !s.buf.ends_with(' ') && !delta.starts_with(' ') && !s.buf.is_empty() {
                s.buf.push(' ');
            }
            s.buf.push_str(delta.trim_end());
            s.last = Instant::now();
        }
        None => g.streams.push(Stream { tag, buf: delta.trim().to_string(), last: Instant::now() }),
    }
}

/// Update one field of the status line. The line is reprinted only when a
/// session state (voice/fast/deep) actually changes; "sent"/"recv" are kept
/// for the next print.
pub fn status(field: &str, value: impl Into<String>) {
    let c = get();
    if !c.enabled {
        return;
    }
    let line = {
        let mut g = c.inner.lock();
        let v = value.into();
        let changed = match field {
            "voice" => { let ch = g.status.voice != v; g.status.voice = v; ch }
            "fast" => { let ch = g.status.fast != v; g.status.fast = v; ch }
            "deep" => { let ch = g.status.deep != v; g.status.deep = v; ch }
            "sent" => { g.status.last_sent = v; false }
            "recv" => { g.status.last_recv = v; false }
            _ => false,
        };
        if !changed {
            return;
        }
        let s = &g.status;
        format!(
            "{} ── voice: {} │ fast: {} │ deep: {}{}{}",
            c.stamp(),
            s.voice,
            s.fast,
            s.deep,
            if s.last_sent.is_empty() { String::new() } else { format!("\n            ↑ last sent to voice: {}", clip(&s.last_sent, 120)) },
            if s.last_recv.is_empty() { String::new() } else { format!("\n            ↓ last heard from voice: {}", clip(&s.last_recv, 120)) },
        )
    };
    println!("{}", c.paint(line, "\x1b[90m"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(color: bool) -> Console {
        Console {
            enabled: true, color, start: Instant::now(),
            inner: Mutex::new(Inner { status: Status::default(), streams: Vec::new() }),
        }
    }

    #[test]
    fn captured_console_labels_and_status_have_no_terminal_codes() {
        let c = console(false);
        for tag in [Tag::Voice, Tag::User, Tag::Deleg, Tag::Send, Tag::Fast, Tag::Deep, Tag::Sys, Tag::Err] {
            let (label, color) = tag.label();
            assert_eq!(c.paint(label.into(), color), label);
        }
        let status = "5.942s ── voice: connected │ fast: idle │ deep: none";
        assert_eq!(c.paint(status.into(), "\x1b[90m"), status);
    }

    #[test]
    fn terminal_console_keeps_its_color_and_reset() {
        assert_eq!(console(true).paint("SYS".into(), "\x1b[90m"), "\x1b[90mSYS\x1b[0m");
    }
}
