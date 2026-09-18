//! The fast worker: the always-open Copilot session that answers every voice
//! delegation. It never queues: a new request aborts the running one and
//! replaces it, so the voice always works on the latest thing the user said.
//! Its final message is spoken, unless a tool already spoke (`say_on_success`).

use github_copilot_sdk::session::Session;
use github_copilot_sdk::types::{DeliveryMode, MessageOptions, MessageSource, SessionEvent};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::console::{self, Tag};
use crate::copilot::event::EventData;
use crate::copilot::{prompt, summarize};
use crate::deep::DeepRunner;
use crate::focus;
use crate::live::client::MAX_APPEND_CHARS;
use crate::narration::narrate_tool;
use crate::screen::Shot;
use crate::text::clip;
use crate::tools::ToolDeps;

/// Prefix of request ids we made up ourselves (the voice model never issued them).
pub const LOCAL_ID: &str = "local_";

/// The delegation id to use when talking to the voice model: none for our own ids.
fn voice_ref(id: &str) -> Option<&str> {
    if id.starts_with(LOCAL_ID) { None } else { Some(id) }
}

/// The one request the session is working on, with its timing breakdown.
struct Request {
    delegation_id: String,
    started: Instant,
    /// Set when a newer request replaced this one; its idle must stay silent.
    superseded: bool,
    /// A tool already spoke the confirmation; the final model turn was skipped.
    answered_early: bool,
    last_message: String,
    /// When the model last got control (request sent or last tool finished).
    model_since: Option<Instant>,
    tools: HashMap<String, (String, Instant)>,
    model_ms: u128,
    tool_ms: u128,
    turns: u32,
}

impl Request {
    /// Milliseconds the model has had control since it last got it; adds them to the total.
    fn model_elapsed(&mut self) -> u128 {
        let ms = self.model_since.take().map(|i| i.elapsed().as_millis()).unwrap_or(0);
        self.model_ms += ms;
        ms
    }
}

pub struct FastRunner {
    session: Arc<Session>,
    deps: Arc<ToolDeps>,
    deep: Arc<DeepRunner>,
    current: Mutex<Option<Request>>,
    requests: AtomicU64,
    session_idle: AtomicBool,
    /// Signalled when the idle of an aborted request arrives.
    abort_settled: tokio::sync::Notify,
}

impl FastRunner {
    pub fn new(session: Arc<Session>, deps: Arc<ToolDeps>, deep: Arc<DeepRunner>) -> Arc<Self> {
        Arc::new(Self { session, deps, deep, current: Mutex::new(None), requests: AtomicU64::new(0), session_idle: AtomicBool::new(true), abort_settled: tokio::sync::Notify::new() })
    }

    pub fn is_busy(&self) -> bool {
        self.current.lock().is_some()
    }

    pub fn next_local_id(&self) -> String {
        format!("{LOCAL_ID}{}", self.requests.load(Ordering::Relaxed) + 1)
    }

    /// Send the conversation to the session, replacing whatever it is doing.
    pub async fn request(&self, id: String, conversation: &str, shot: Option<Shot>) {
        *self.deps.recent_conversation.lock() = conversation.to_string();
        self.replace_running().await;

        let live = &self.deps.live;
        let n = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
        self.deps.spoke_early.store(false, Ordering::Relaxed);
        *self.deps.current_delegation.lock() = voice_ref(&id).map(str::to_string);
        *self.current.lock() = Some(Request {
            delegation_id: id.clone(),
            started: Instant::now(),
            superseded: false,
            answered_early: false,
            last_message: String::new(),
            model_since: Some(Instant::now()),
            tools: HashMap::new(),
            model_ms: 0,
            tool_ms: 0,
            turns: 0,
        });
        console::status("fast", format!("busy · request #{n}"));
        live.thinking(voice_ref(&id), "Started on this. Nothing is finished yet; do not describe results until they arrive.");

        let mode = if self.session_idle.load(Ordering::Relaxed) { DeliveryMode::Enqueue } else { DeliveryMode::Immediate };
        let mut opts = MessageOptions::new(format!("{}\n\nLonger task status: {}\n\n{}", prompt::delegation_header(), self.deep.brief_status(), conversation.trim()))
            .with_mode(mode)
            .with_source(MessageSource::User);
        console::log(Tag::Fast, format!("request #{n} sent ({})", if shot.is_some() { "with screenshot" } else { "no screenshot" }));
        if let Some(shot) = shot {
            opts = opts.with_attachments(vec![shot.attachment()]);
        }
        if let Err(e) = self.session.send(opts).await {
            tracing::error!("fast session send failed: {e}");
            live.commentary(voice_ref(&id), "I could not start on that; something is wrong on my side. Please try again in a moment.");
            self.finish();
        }
    }

    /// The newest request wins: abort the running one and wait until it has settled.
    async fn replace_running(&self) {
        let Some(old) = self.current.lock().as_mut().map(|r| {
            r.superseded = true;
            r.delegation_id.clone()
        }) else {
            return;
        };
        console::log(Tag::Fast, format!("new request replaces the running one ({old})"));
        match self.session.abort().await {
            // Wait for the aborted turn's idle so it cannot be mistaken for the new request's.
            Ok(()) => drop(tokio::time::timeout(Duration::from_secs(4), self.abort_settled.notified()).await),
            Err(e) => tracing::warn!("abort before resend failed: {e}"),
        }
        self.deps.live.thinking(voice_ref(&old), "That request was replaced by the user's newer one; do not report on it.");
    }

    /// Stop the running request, if any, without a spoken result.
    pub async fn cancel(&self) {
        if self.is_busy() {
            if let Err(e) = self.session.abort().await {
                tracing::warn!("abort failed: {e}");
            }
        }
        self.finish();
    }

    fn finish(&self) {
        *self.current.lock() = None;
        *self.deps.current_delegation.lock() = None;
    }

    pub fn on_event(&self, ev: &SessionEvent) {
        if ev.agent_id.is_some() {
            return;
        }
        match ev.event_type.as_str() {
            "session.idle" => self.session_idle.store(true, Ordering::Relaxed),
            "user.message" | "assistant.turn_start" => self.session_idle.store(false, Ordering::Relaxed),
            _ => {}
        }
        let d = &ev.data;
        let mut guard = self.current.lock();
        match ev.event_type.as_str() {
            "tool.execution_start" => {
                let Some(r) = guard.as_mut() else { return };
                let name = d.text("toolName");
                console::log(Tag::Fast, format!("model {} ms → tool {name} {}", r.model_elapsed(), clip(&d.args().to_string(), 140)));
                console::status("fast", format!("busy · {}", narrate_tool(&name, d.args())));
                focus::apply(&self.deps.helper, &name, d.args());
                r.tools.insert(d.text("toolCallId"), (name, Instant::now()));
            }
            "tool.execution_complete" => {
                let Some(r) = guard.as_mut() else { return };
                if self.deps.spoke_early.swap(false, Ordering::Relaxed) {
                    // The tool already told the user; skip the model's closing turn.
                    r.answered_early = true;
                    console::log(Tag::Fast, "spoke early → skipping the final model turn");
                    let session = self.session.clone();
                    tokio::spawn(async move {
                        if let Err(e) = session.abort().await {
                            tracing::warn!("abort after early answer failed: {e}");
                        }
                    });
                }
                if let Some((name, started)) = r.tools.remove(&d.text("toolCallId")) {
                    let ms = started.elapsed().as_millis();
                    r.tool_ms += ms;
                    r.model_since = Some(Instant::now());
                    match d.error() {
                        Some(e) => console::log(Tag::Err, format!("tool {name} failed after {ms} ms: {}", clip(&e, 120))),
                        None => console::log(Tag::Fast, format!("tool {name} done in {ms} ms")),
                    }
                }

            }
            "assistant.turn_start" => {
                if let Some(r) = guard.as_mut() {
                    r.turns += 1;
                }
            }
            "assistant.message" => {
                let content = d.text("content");
                if let Some(r) = guard.as_mut().filter(|_| !content.trim().is_empty()) {
                    console::log(Tag::Fast, format!("model {} ms → says: {}", r.model_elapsed(), clip(&content, 160)));
                    r.last_message = content;
                }
            }
            "session.usage_info" if d.int("currentTokens") > 0 => console::log(
                Tag::Sys,
                format!(
                    "fast context: {}k of {}k tokens ({} system, {} tool schemas), {} messages",
                    d.int("currentTokens") / 1000,
                    d.int("tokenLimit") / 1000,
                    d.int("systemTokens"),
                    d.int("toolDefinitionsTokens"),
                    d.int("messagesLength")
                ),
            ),
            "assistant.usage" if d.int("inputTokens") > 0 => console::log(
                Tag::Sys,
                format!("model call: {} in ({} cached) / {} out, {} ms", d.int("inputTokens"), d.int("cacheReadTokens"), d.int("outputTokens"), d.int("duration")),
            ),
            // `assistant.idle`: the agent is done, even if a background shell it started keeps running.
            "assistant.idle" | "session.idle" => {
                drop(guard);
                self.deliver(d.flag("aborted"));
            }
            "session.error" if !ev.is_transient_error() => {
                let msg = d.text("message");
                tracing::error!(%msg, "fast session error");
                let id = guard.as_ref().map(|r| r.delegation_id.clone());
                drop(guard);
                self.deps.live.commentary(id.as_deref().and_then(voice_ref), &format!("Something went wrong while working on that: {}.", summarize::for_speech(&msg, 200)));
                self.finish();
            }
            "session.warning" => tracing::warn!(%d, "fast session warning"),
            _ => {}
        }
    }

    /// The request just completed: speak its final message.
    fn deliver(&self, aborted: bool) {
        let mut guard = self.current.lock();
        let Some(r) = guard.as_ref() else { return };
        let took = r.started.elapsed().as_secs_f32();
        if r.superseded {
            // The idle belongs to the aborted request; the replacement is already on its way.
            console::log(Tag::Fast, format!("replaced request stopped after {took:.1}s"));
            if aborted {
                self.abort_settled.notify_one();
            }
            return;
        }
        let r = guard.take().expect("checked above");
        drop(guard);
        *self.deps.current_delegation.lock() = None;

        let how = if r.answered_early { " (answered by the tool)" } else if aborted { " (aborted)" } else { "" };
        console::log(Tag::Fast, format!("done in {took:.1}s{how} · model {:.1}s in {} turns · tools {:.1}s", r.model_ms as f32 / 1000.0, r.turns, r.tool_ms as f32 / 1000.0));
        console::status("fast", format!("idle · {} requests · last took {took:.1}s", self.requests.load(Ordering::Relaxed)));

        let (live, id) = (&self.deps.live, voice_ref(&r.delegation_id));
        if r.answered_early {
            // Already spoken.
        } else if aborted {
            live.thinking(id, "The work was stopped before it finished.");
        } else if r.last_message.trim().is_empty() {
            live.commentary(id, "That finished, but there was nothing to report back.");
        } else {
            live.commentary(id, &summarize::for_speech(&r.last_message, MAX_APPEND_CHARS));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copilot::test_support::{SessionFixture, event};
    use crate::helper::HelperHandle;
    use serde_json::json;

    #[tokio::test]
    async fn new_requests_reach_a_worker_with_a_background_shell() {
        let modes = Arc::new(Mutex::new(Vec::new()));
        let sent = modes.clone();
        let fixture = SessionFixture::new(move |params| {
            sent.lock().push(params["mode"].clone());
            Ok(json!({"messageId": "sent"}))
        }).await;
        let (live, _commands) = crate::live::client::handle();
        let deps = ToolDeps::new(HelperHandle::disabled(), live, std::env::temp_dir());
        let deep = DeepRunner::new(fixture.session.clone(), deps.clone());
        let fast = FastRunner::new(fixture.session.clone(), deps, deep);

        fast.request("first".into(), "Start a server", None).await;
        fast.on_event(&event("user.message", json!({})));
        fast.on_event(&event("assistant.message", json!({"content": "Server started."})));
        fast.on_event(&event("assistant.idle", json!({})));
        assert!(!fast.is_busy());
        fast.request("second".into(), "Next request", None).await;
        fast.on_event(&event("session.idle", json!({})));
        fast.request("third".into(), "Idle request", None).await;

        assert_eq!(*modes.lock(), vec![json!("enqueue"), json!("immediate"), json!("enqueue")]);
    }
}
