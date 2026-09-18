//! The deep worker: one long-running Copilot session (the "deep" model) that
//! the fast session starts on demand for multi-step jobs. It tracks one task,
//! keeps a progress log, and keeps the voice informed with *quiet* context
//! (start, a milestone every few seconds, finish) so the voice can talk about
//! the work naturally. Only questions and the final outcome are spoken.

use anyhow::{Result, anyhow};
use github_copilot_sdk::session::Session;
use github_copilot_sdk::types::{DeliveryMode, MessageOptions, MessageSource, SessionEvent};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::console::{self, Tag};
use crate::copilot::event::EventData;
use crate::copilot::{prompt, summarize};
use crate::live::client::MAX_APPEND_CHARS;
use crate::narration::narrate_tool;
use crate::screen;
use crate::text::{base_name, clip, human_secs};
use crate::tools::ToolDeps;

const MAX_STEPS: usize = 14;
const MAX_NOTES: usize = 4;
/// Minimum gap between quiet progress notes to the voice.
const UPDATE_EVERY: Duration = Duration::from_secs(9);
/// Minimum gap between *spoken* remarks about what is being done right now.
const SPEAK_EVERY: Duration = Duration::from_secs(7);
const MAX_FILES: usize = 40;
/// Bytes of streamed tool input kept per call; longer than any file header.
const STREAM_TAIL: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub goal: String,
    pub started: Instant,
    pub ended: Option<Instant>,
    pub status: Status,
    /// The voice delegation that (last) drove this task, for updates and the final report.
    pub delegation_id: Option<String>,
    steps: VecDeque<(Instant, String)>,
    notes: VecDeque<String>,
    last_message: String,
    /// The worker already told the user something itself (`say_on_success` / `tell_user`).
    announced: Option<String>,
    /// Follow-ups sent but not yet picked up by the worker.
    pending_followups: usize,
    turn_idle: bool,
    /// The task's own first prompt has been seen as a `user.message`.
    first_prompt_seen: bool,
    last_update: Instant,
    steps_at_last_update: usize,
    total_steps: usize,
    /// Files the worker (or its helpers) created or edited, in order, base names.
    files_changed: Vec<String>,
    /// Raw live events the voice has not been asked to react to yet.
    unspoken: Vec<(Instant, String)>,
    /// Tool name per running tool call, to label its result.
    calls: HashMap<String, String>,
    /// Tail of the tool input the model is still writing, per tool call.
    streaming: HashMap<String, String>,
    /// Files already announced while their content was still streaming in.
    streamed_files: HashSet<String>,
    last_spoken: Instant,
    pub result: Option<String>,
}

impl Task {
    fn new(goal: String, delegation_id: Option<String>, files_changed: Vec<String>) -> Self {
        let now = Instant::now();
        Self {
            goal,
            started: now,
            ended: None,
            status: Status::Running,
            delegation_id,
            steps: VecDeque::new(),
            notes: VecDeque::new(),
            last_message: String::new(),
            announced: None,
            pending_followups: 0,
            turn_idle: false,
            first_prompt_seen: false,
            last_update: now,
            steps_at_last_update: 0,
            total_steps: 0,
            files_changed,
            unspoken: Vec::new(),
            calls: HashMap::new(),
            streaming: HashMap::new(),
            streamed_files: HashSet::new(),
            last_spoken: now,
            result: None,
        }
    }
}

pub struct DeepRunner {
    session: Arc<Session>,
    /// The deep session's own tool state (separate from the fast session's).
    deps: Arc<ToolDeps>,
    task: Mutex<Option<Task>>,
    /// The runtime itself is idle (`session.idle`). It is not while a background shell
    /// the worker started (a dev server) is alive, even though the worker is done; in
    /// that state queued prompts are never picked up, so they must be sent immediately.
    session_idle: AtomicBool,
    sending: tokio::sync::Mutex<()>,
}

impl DeepRunner {
    pub fn new(session: Arc<Session>, deps: Arc<ToolDeps>) -> Arc<Self> {
        Arc::new(Self { session, deps, task: Mutex::new(None), session_idle: AtomicBool::new(true), sending: tokio::sync::Mutex::new(()) })
    }

    /// Queue behind running work; but when the worker is done and only a background
    /// shell keeps the session busy, the queue never drains, so deliver immediately.
    fn delivery_mode(&self, worker_running: bool) -> DeliveryMode {
        if !worker_running && !self.session_idle.load(Ordering::Relaxed) { DeliveryMode::Immediate } else { DeliveryMode::Enqueue }
    }

    pub fn is_running(&self) -> bool {
        self.task.lock().as_ref().map(|t| t.status == Status::Running).unwrap_or(false)
    }

    /// Start a task, or, when one is already running, queue this as a follow-up to it.
    pub async fn start(&self, goal: String, context: String, delegation_id: Option<String>) -> Result<String> {
        let _sending = self.sending.lock().await;
        if self.is_running() {
            self.continue_inner(goal.clone(), delegation_id).await?;
            return Ok(format!("A longer task was already running, so this was added to it as a follow-up: {goal}"));
        }
        let mut opts = MessageOptions::new(prompt::deep_task_prompt(&goal, &context))
            // Enqueue on an idle session: there "immediate" is treated as steering and dropped.
            .with_mode(self.delivery_mode(false))
            .with_source(MessageSource::User);
        if let Ok(shot) = screen::capture(1600).await {
            opts = opts.with_attachments(vec![shot.attachment()]);
        }
        *self.task.lock() = Some(Task::new(goal.clone(), delegation_id.clone(), Vec::new()));
        *self.deps.current_delegation.lock() = delegation_id.clone();
        self.deps.spoke_early.store(false, Ordering::Relaxed);
        self.send(opts, false, None).await?;
        tracing::info!(%goal, "deep task started");
        console::log(Tag::Deep, format!("task started: {}", clip(&goal, 160)));
        console::status("deep", format!("running · {}", clip(&goal, 60)));
        self.deps.live.thinking(
            delegation_id.as_deref(),
            &format!("Background task started: {}. It runs on its own; you can keep talking. You will get quiet updates; use them if asked how it is going.", summarize::for_speech(&goal, 220)),
        );
        Ok(format!("Deep task started: {goal}"))
    }

    /// Send the user's answer or an addition into the task. Works while it is
    /// still running (queued behind the current step) and after it paused or finished.
    pub async fn continue_with(&self, message: String, delegation_id: Option<String>) -> Result<()> {
        let _sending = self.sending.lock().await;
        self.continue_inner(message, delegation_id).await
    }

    async fn continue_inner(&self, message: String, delegation_id: Option<String>) -> Result<()> {
        let (running, previous_delegation) = {
            let mut guard = self.task.lock();
            let t = guard.as_mut().ok_or_else(|| anyhow!("no longer task exists to continue"))?;
            let was_running = t.status == Status::Running;
            let previous_delegation = t.delegation_id.clone();
            if was_running {
                t.pending_followups += 1;
            } else {
                // A finished task picks up again: fresh clock and log, same goal and file list.
                *t = Task::new(t.goal.clone(), t.delegation_id.clone(), std::mem::take(&mut t.files_changed));
            }
            if delegation_id.is_some() {
                t.delegation_id = delegation_id.clone();
            }
            (was_running, previous_delegation)
        };
        if delegation_id.is_some() {
            *self.deps.current_delegation.lock() = delegation_id.clone();
        }
        let mode = self.delivery_mode(running);
        let mut opts = MessageOptions::new(prompt::deep_continue_prompt(&message))
            .with_mode(mode)
            .with_source(MessageSource::User);
        if let Ok(shot) = screen::capture(1600).await {
            opts = opts.with_attachments(vec![shot.attachment()]);
        }
        self.send(opts, running, previous_delegation).await?;
        tracing::info!(%message, running, ?mode, "deep task continued");
        console::log(Tag::Deep, format!("{}: {}", if running { "follow-up queued" } else { "continued with" }, clip(&message, 120)));
        console::status("deep", if running { "running · follow-up queued".to_string() } else { "running · continuing".to_string() });
        self.deps.live.thinking(
            delegation_id.as_deref(),
            &format!("Background task {}: {}.", if running { "got a follow-up that will be handled after its current step" } else { "continues" }, summarize::for_speech(&message, 200)),
        );
        Ok(())
    }

    async fn send(&self, opts: MessageOptions, followup: bool, previous_delegation: Option<String>) -> Result<()> {
        if let Err(error) = self.session.send(opts).await {
            let message = format!("deep session send failed: {error}");
            tracing::error!(%message);
            console::log(Tag::Err, &message);
            let mut guard = self.task.lock();
            if let Some(t) = guard.as_mut() {
                if followup {
                    t.delegation_id = previous_delegation.clone();
                    *self.deps.current_delegation.lock() = previous_delegation;
                    t.pending_followups = t.pending_followups.saturating_sub(1);
                    if t.status == Status::Running && t.turn_idle && t.pending_followups == 0 {
                        self.finish(t);
                    }
                } else {
                    t.status = Status::Failed;
                    t.ended = Some(Instant::now());
                    t.result = Some(message.clone());
                    *self.deps.current_delegation.lock() = None;
                    console::status("deep", "failed");
                }
            }
            return Err(anyhow!(message));
        }
        Ok(())
    }

    pub async fn cancel(&self) -> Result<bool> {
        let _sending = self.sending.lock().await;
        if !self.is_running() {
            return Ok(false);
        }
        self.session.abort().await.map_err(|e| anyhow!("abort failed: {e}"))?;
        if let Some(t) = self.task.lock().as_mut() {
            t.status = Status::Cancelled;
            t.ended = Some(Instant::now());
        }
        tracing::info!("deep task cancelled");
        console::log(Tag::Deep, "task cancelled");
        console::status("deep", "cancelled");
        self.deps.live.thinking(None, "Background task was cancelled. Nothing is running now.");
        Ok(true)
    }

    /// One line for the fast session's request header.
    pub fn brief_status(&self) -> String {
        let guard = self.task.lock();
        match guard.as_ref() {
            None => "No longer task has been started.".into(),
            Some(t) => {
                let elapsed = human_secs(t.ended.unwrap_or_else(Instant::now).duration_since(t.started).as_secs());
                let last = t.steps.back().map(|(_, s)| s.as_str()).unwrap_or("just started");
                match t.status {
                    Status::Running => format!("RUNNING for {elapsed}: \"{}\". Latest step: {last}. Additions or changes to it go through continue_deep_task.", clip(&t.goal, 140)),
                    Status::Done => format!("FINISHED after {elapsed}: \"{}\". Result: {}. New work on the same thing goes through continue_deep_task so the worker keeps its context.", clip(&t.goal, 120), clip(t.result.as_deref().unwrap_or("done"), 200)),
                    Status::Failed => format!("FAILED after {elapsed}: {}", clip(t.result.as_deref().unwrap_or(""), 200)),
                    Status::Cancelled => format!("CANCELLED after {elapsed}: \"{}\".", clip(&t.goal, 140)),
                }
            }
        }
    }

    /// Plain-text report for the fast session (and therefore the voice).
    pub fn status_report(&self) -> String {
        let guard = self.task.lock();
        let Some(t) = guard.as_ref() else {
            return "No longer task has been started in this session.".into();
        };
        let elapsed = t.ended.unwrap_or_else(Instant::now).duration_since(t.started).as_secs();
        let mut out = format!("Task: {}\nStatus: {:?}, {} elapsed, {} steps so far.\n", t.goal, t.status, human_secs(elapsed), t.total_steps);
        if t.pending_followups > 0 {
            out.push_str(&format!("Follow-ups waiting: {}\n", t.pending_followups));
        }
        if !t.files_changed.is_empty() {
            out.push_str(&format!("Files created or edited so far: {}\n", t.files_changed.join(", ")));
        }
        if !t.steps.is_empty() {
            out.push_str("Recent steps (oldest first):\n");
            for (at, step) in &t.steps {
                out.push_str(&format!("- {} ago: {}\n", human_secs(at.elapsed().as_secs()), step));
            }
        }
        if let Some(note) = t.notes.back() {
            out.push_str(&format!("Latest note from the worker: {note}\n"));
        }
        if let Some(said) = &t.announced {
            out.push_str(&format!("Already told the user: {said}\n"));
        }
        if let Some(r) = &t.result {
            out.push_str(&format!("Result: {r}\n"));
        }
        out
    }

    /// Feed an event from the deep session. Voice updates are sent from here.
    pub fn on_event(&self, ev: &SessionEvent) {
        // Helpers (sub-agents) do most of the file work. Their tool calls are progress;
        // their messages, prompts and idles are not the task's own and must not end it.
        if ev.agent_id.is_some() && !matches!(ev.event_type.as_str(), "tool.execution_start" | "assistant.intent" | "assistant.tool_call_delta") {
            return;
        }
        match ev.event_type.as_str() {
            "session.idle" => self.session_idle.store(true, Ordering::Relaxed),
            "user.message" | "assistant.turn_start" => self.session_idle.store(false, Ordering::Relaxed),
            _ => {}
        }
        let d = &ev.data;
        let mut guard = self.task.lock();
        let Some(t) = guard.as_mut() else { return };
        if t.status != Status::Running {
            return;
        }
        match ev.event_type.as_str() {
            // The worker picked up a prompt: the first is the task itself, later ones are follow-ups.
            "user.message" => {
                t.turn_idle = false;
                if t.first_prompt_seen {
                    t.pending_followups = t.pending_followups.saturating_sub(1);
                } else {
                    t.first_prompt_seen = true;
                }
            }
            "assistant.turn_start" => t.turn_idle = false,
            // The model is still writing a tool call (a patch can take half a minute):
            // name each file the moment its header appears in the stream.
            "assistant.tool_call_delta" => {
                let buf = t.streaming.entry(d.text("toolCallId")).or_default();
                buf.push_str(&d.text("inputDelta"));
                let found = streamed_files(buf);
                // Keep only a tail long enough to hold a header that is still incomplete.
                let cut = (buf.len().saturating_sub(STREAM_TAIL)..buf.len()).find(|i| buf.is_char_boundary(*i)).unwrap_or(0);
                buf.drain(..cut);
                for (verb, path) in found {
                    let file = base_name(&path).to_string();
                    if t.streamed_files.insert(file.clone()) {
                        let step = format!("{} {file}", if verb == "creating" { "writing" } else { verb });
                        console::log(Tag::Deep, format!("streaming: {step}"));
                        console::status("deep", format!("running {} · {step}", human_secs(t.started.elapsed().as_secs())));
                        push_bounded(&mut t.steps, (Instant::now(), step), MAX_STEPS);
                        self.forward(t, format!("file content is being written right now: {path}"), true);
                    }
                }
            }
            "tool.execution_start" => {
                t.streaming.remove(&d.text("toolCallId"));
                let name = d.text("toolName");
                let args = d.args();
                let step = narrate_tool(&name, args);
                tracing::info!(%step, "deep step");
                console::log(Tag::Deep, format!("tool {name} {}", clip(&args.to_string(), 120)));
                console::status("deep", format!("running {} · {}", human_secs(t.started.elapsed().as_secs()), clip(&step, 70)));
                let touched = files_touched(&name, args);
                // Already said while it streamed in: do not say it a second time.
                let already_said = !touched.is_empty() && touched.iter().all(|f| t.streamed_files.contains(f));
                for f in touched {
                    if !t.files_changed.contains(&f) && t.files_changed.len() < MAX_FILES {
                        t.files_changed.push(f);
                    }
                }
                t.calls.insert(d.text("toolCallId"), name.clone());
                self.forward(t, format!("tool call started: {name} {}", clip(&args.to_string(), if already_said { 100 } else { 260 })), !is_silent(&name) && !already_said);
                push_bounded(&mut t.steps, (Instant::now(), step), MAX_STEPS);
                t.total_steps += 1;
                self.maybe_update_voice(t);
            }
            "tool.execution_complete" => {
                if let Some(name) = t.calls.remove(&d.text("toolCallId")) {
                    let line = format!("tool call {}: {name} → {}", if d.error().is_none() { "finished" } else { "FAILED" }, clip(d.output(), 200));
                    // Results are awareness only; the next started step is what gets spoken about.
                    self.forward(t, line, false);
                }
                if self.deps.spoke_early.swap(false, Ordering::Relaxed) {
                    t.announced = Some(self.deps.last_said.lock().clone());
                }
            }
            "assistant.intent" => {
                let intent = d.text("intent");
                if !intent.trim().is_empty() {
                    push_bounded(&mut t.steps, (Instant::now(), intent), MAX_STEPS);
                    self.maybe_update_voice(t);
                }
            }
            "subagent.started" => {
                push_bounded(&mut t.steps, (Instant::now(), format!("handed a part to the {} helper", d.text("agentDisplayName"))), MAX_STEPS);
            }
            "assistant.message" => {
                let content = d.text("content");
                if !content.trim().is_empty() {
                    console::log(Tag::Deep, format!("says: {}", clip(&content, 160)));
                    push_bounded(&mut t.notes, summarize::for_speech(&content, 240), MAX_NOTES);
                    t.last_message = content;
                }
            }
            "session.task_complete" => {
                let summary = d.text("summary");
                if !summary.trim().is_empty() {
                    t.last_message = summary;
                }
            }
            // `assistant.idle` fires when the agent is done even if background shells
            // (a dev server it started) keep running; `session.idle` would wait for those.
            "assistant.idle" | "session.idle" => {
                t.turn_idle = true;
                if d.flag("aborted") {
                    t.status = Status::Cancelled;
                    t.ended = Some(Instant::now());
                    return;
                }
                if t.pending_followups > 0 {
                    // A follow-up is still waiting to be picked up; the task is not over.
                    return;
                }
                self.finish(t);
            }
            "session.error" if !ev.is_transient_error() => {
                let msg = summarize::for_speech(&d.text("message"), 200);
                t.status = Status::Failed;
                t.ended = Some(Instant::now());
                t.result = Some(format!("Failed: {msg}"));
                tracing::error!(%msg, "deep task failed");
                console::log(Tag::Err, format!("deep task failed: {msg}"));
                console::status("deep", "failed");
                self.deps.live.commentary(t.delegation_id.as_deref(), &format!("The longer task stopped with a problem: {msg}"));
                self.deps.live.thinking(None, "Background task failed. Nothing is running now.");
            }
            _ => {}
        }
    }

    fn finish(&self, t: &mut Task) {
        t.status = Status::Done;
        t.ended = Some(Instant::now());
        let took = human_secs(t.started.elapsed().as_secs());
        let written = if t.last_message.trim().is_empty() { None } else { Some(summarize::for_speech(&t.last_message, MAX_APPEND_CHARS - 80)) };
        let result = written.clone().or_else(|| t.announced.clone()).unwrap_or_default();
        t.result = Some(result.clone());
        tracing::info!(goal = %t.goal, "deep task done");
        console::log(Tag::Deep, format!("done in {took}"));
        console::status("deep", format!("done · {took}"));
        match (&written, &t.announced) {
            // The worker already told the user itself and wrote nothing more: stay quiet.
            (None, Some(_)) => {}
            _ => self.deps.live.commentary(t.delegation_id.as_deref(), format!("The longer task is finished. {result}").trim()),
        }
        let outcome = if result.is_empty() { "no report was written".to_string() } else { clip(&result, 300) };
        self.deps.live.thinking(None, &format!("Background task finished after {took}. Nothing is running now. Outcome: {outcome}"));
    }

    /// Quiet progress note for the voice, at most every `UPDATE_EVERY`, only when there is something new.
    fn maybe_update_voice(&self, t: &mut Task) {
        if t.last_update.elapsed() < UPDATE_EVERY || t.total_steps == t.steps_at_last_update {
            return;
        }
        t.last_update = Instant::now();
        t.steps_at_last_update = t.total_steps;
        let files = if t.files_changed.is_empty() {
            " No files written yet.".to_string()
        } else {
            let last: Vec<&str> = t.files_changed.iter().rev().take(8).rev().map(|f| f.as_str()).collect();
            format!(" Files created or edited so far ({}): {}.", t.files_changed.len(), last.join(", "))
        };
        self.deps.live.thinking(
            t.delegation_id.as_deref(),
            &format!(
                "Background task status: {} in, still running, {} steps so far.{files} Not finished; do not say it is done.",
                human_secs(t.started.elapsed().as_secs()),
                t.total_steps
            ),
        );
    }

    /// Forward one raw event from the work to the voice, as it happens. Every event goes
    /// in quietly (awareness); `speakable` ones are also batched into a spoken cue at most
    /// every `SPEAK_EVERY`. No wording is made up here: how to talk about it is the voice's
    /// own job (see "Live activity" in the voice prompt).
    fn forward(&self, t: &mut Task, event: String, speakable: bool) {
        self.deps.live.thinking(t.delegation_id.as_deref(), &format!("Live activity: {event}"));
        if !speakable {
            return;
        }
        t.unspoken.push((Instant::now(), event));
        if t.last_spoken.elapsed() < SPEAK_EVERY {
            return;
        }
        t.last_spoken = Instant::now();
        // Only what is still current: an event that waited too long is history, not "right now".
        let recent: Vec<String> = t.unspoken.drain(..).filter(|(at, _)| at.elapsed() < Duration::from_secs(10)).map(|(_, e)| e).collect::<Vec<_>>().into_iter().rev().take(3).rev().collect();
        self.deps.live.commentary(t.delegation_id.as_deref(), &format!("Live activity: {}", recent.join(" | ")));
    }
}

/// Steps that are never said aloud: the assistant's own talking and pointing, and hand-offs.
fn is_silent(tool: &str) -> bool {
    matches!(tool, "tell_user" | "highlight_window" | "clear_annotations" | "focus_window" | "task" | "report_intent")
}

/// Complete file headers in tool input that is still streaming in, as `(verb, path)`.
/// Input may be raw (`\n`) or JSON-escaped (`\\n`); a header without its end is not reported yet.
fn streamed_files(text: &str) -> Vec<(&'static str, String)> {
    const MARKERS: [(&str, &str); 3] = [("*** Add File: ", "creating"), ("*** Update File: ", "editing"), ("\"path\":\"", "writing")];
    let mut found = Vec::new();
    for (marker, verb) in MARKERS {
        for (i, _) in text.match_indices(marker) {
            let rest = &text[i + marker.len()..];
            let end = if marker.starts_with('"') { rest.find('"') } else { [rest.find('\n'), rest.find("\\n")].into_iter().flatten().min() };
            if let Some(path) = end.map(|e| rest[..e].trim()).filter(|p| !p.is_empty()) {
                found.push((verb, path.to_string()));
            }
        }
    }
    found
}

/// Base names of files a write-type tool call touches.
fn files_touched(tool: &str, args: &serde_json::Value) -> Vec<String> {
    let base = |p: &str| base_name(p).to_string();
    match tool {
        "create" | "create_file" | "write_file" | "edit" | "edit_file" => ["path", "file", "filePath"]
            .iter()
            .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
            .map(|p| vec![base(p)])
            .unwrap_or_default(),
        "apply_patch" | "patch" => crate::narration::patch_files(args).into_iter().map(|(_, p)| base(&p)).collect(),
        _ => Vec::new(),
    }
}


fn push_bounded<T>(q: &mut VecDeque<T>, item: T, max: usize) {
    q.push_back(item);
    while q.len() > max {
        q.pop_front();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::copilot::test_support::{SessionFixture, event};
    use crate::helper::HelperHandle;
    use serde_json::json;

    async fn failing_runner() -> (SessionFixture, Arc<DeepRunner>) {
        let fixture = SessionFixture::new(|_| Err("fixture delivery failure".into())).await;
        let (live, _commands) = crate::live::client::handle();
        let deps = ToolDeps::new(HelperHandle::disabled(), live, std::env::temp_dir());
        let runner = DeepRunner::new(fixture.session.clone(), deps);
        *runner.task.lock() = Some(Task::new("Write a file".into(), Some("request".into()), Vec::new()));
        (fixture, runner)
    }

    #[tokio::test]
    async fn failed_initial_or_resumed_send_does_not_leave_task_running() {
        let (_fixture, runner) = failing_runner().await;
        assert!(runner.send(MessageOptions::new("start"), false, None).await.is_err());
        assert!(!runner.is_running());
        let guard = runner.task.lock();
        let task = guard.as_ref().unwrap();
        assert_eq!(task.status, Status::Failed);
        assert!(task.ended.is_some());
        assert!(task.result.as_ref().unwrap().contains("fixture delivery failure"));
    }

    #[tokio::test]
    async fn failed_followup_preserves_work_and_allows_completion() {
        for idle_before_failure in [false, true] {
            let (_fixture, runner) = failing_runner().await;
            runner.on_event(&event("user.message", json!({})));
            runner.task.lock().as_mut().unwrap().pending_followups = 1;
            runner.on_event(&event("assistant.message", json!({"content": "File written."})));
            if idle_before_failure {
                runner.on_event(&event("assistant.idle", json!({})));
                assert!(runner.is_running());
            }
            assert!(runner.send(MessageOptions::new("follow-up"), true, Some("original".into())).await.is_err());
            assert_eq!(runner.task.lock().as_ref().unwrap().pending_followups, 0);
            assert_eq!(runner.task.lock().as_ref().unwrap().delegation_id.as_deref(), Some("original"));
            if !idle_before_failure {
                assert!(runner.is_running());
                runner.on_event(&event("assistant.idle", json!({})));
            }
            let guard = runner.task.lock();
            let task = guard.as_ref().unwrap();
            assert_eq!(task.status, Status::Done);
            assert_eq!(task.result.as_deref(), Some("File written."));
        }
    }

    #[test]
    fn touched_files() {
        let patch = serde_json::json!({"input": "*** Begin Patch\n*** Add File: todo-app/index.html\n+x\n*** Update File: todo-app/app.js\n@@\n*** End Patch"});
        assert_eq!(files_touched("apply_patch", &patch), vec!["index.html", "app.js"]);
        assert_eq!(files_touched("create", &serde_json::json!({"path": "/a/b/c.css"})), vec!["c.css"]);
        assert!(files_touched("view", &serde_json::json!({"path": "/a/b"})).is_empty());
        assert!(is_silent("tell_user") && !is_silent("view"));
        let f = streamed_files("*** Begin Patch\n*** Add File: todo/index.html\n+<!doc");
        assert_eq!(f, vec![("creating", "todo/index.html".to_string())]);
        let f = streamed_files("{\"input\":\"*** Begin Patch\\n*** Update File: a/app.js\\n@@");
        assert_eq!(f, vec![("editing", "a/app.js".to_string())]);
        assert!(streamed_files("*** Add File: todo/ind").is_empty());
    }

}
