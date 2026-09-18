//! Glue between the voice (GPT-Live) and the two workers: it keeps the
//! transcript, turns every delegation into a request for the fast worker
//! (with a screenshot taken while the user was still talking), feeds session
//! events to the workers, drives the overlay's energy, and watches for "stop"
//! and for commands the voice forgot to delegate.

use base64::Engine;
use github_copilot_sdk::subscription::EventSubscription;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::audio::{self, AudioHandles};
use crate::console::{self, Tag};
use crate::copilot::event::{EventData, next_event};
use crate::deep::DeepRunner;
use crate::fast::FastRunner;
use crate::focus;
use crate::helper::{GlowState, HelperHandle};
use crate::live::transcript::{Role, Transcript, is_command, is_stop_phrase};
use crate::live::{LiveEvent, LiveHandle, LiveIncoming};
use crate::screen;
use crate::sessions::Events;
use crate::text::clip;

const SCREENSHOT_MAX_PX: u32 = 1024;
/// How long the voice model gets to delegate a spoken command before we start it ourselves.
const FALLBACK_AFTER: Duration = Duration::from_millis(3200);
const FALLBACK_WINDOW: Duration = Duration::from_secs(12);
/// A screenshot taken while the user was still talking is reused if it is at most this old.
const PRESHOT_MAX_AGE: Duration = Duration::from_millis(2500);
const CONTEXT_TURNS: usize = 12;

pub struct Orchestrator {
    live: LiveHandle,
    helper: HelperHandle,
    fast: Arc<FastRunner>,
    deep: Arc<DeepRunner>,
    user_name: String,
    transcript: Mutex<Transcript>,
    last_audio_out: Mutex<Instant>,
    audio_deltas: AtomicU64,
    /// Screenshot taken as soon as the user starts speaking, so the request
    /// does not have to wait for one.
    preshot: Mutex<Option<(Instant, screen::Shot)>>,
    preshot_busy: AtomicBool,
    /// Start time (ms) of the newest user turn that was handed to the fast worker / treated as "stop".
    handled_turn: Mutex<Option<u64>>,
    handled_stop_turn: Mutex<Option<u64>>,
    shutdown: mpsc::Sender<String>,
}

impl Orchestrator {
    pub fn new(live: LiveHandle, helper: HelperHandle, fast: Arc<FastRunner>, deep: Arc<DeepRunner>, user_name: String, shutdown: mpsc::Sender<String>) -> Arc<Self> {
        Arc::new(Self {
            live,
            helper,
            fast,
            deep,
            user_name,
            transcript: Mutex::new(Transcript::new()),
            last_audio_out: Mutex::new(Instant::now() - Duration::from_secs(10)),
            audio_deltas: AtomicU64::new(0),
            preshot: Mutex::new(None),
            preshot_busy: AtomicBool::new(false),
            handled_turn: Mutex::new(None),
            handled_stop_turn: Mutex::new(None),
            shutdown,
        })
    }

    /// Spawn every long-running loop. Returns immediately.
    pub fn run(
        self: &Arc<Self>,
        live_rx: mpsc::Receiver<LiveIncoming>,
        events: Events,
        audio: AudioHandles,
    ) {
        let AudioHandles { mic_rx, in_level, playback, shutdown: audio_guard } = audio;
        self.spawn_mic_pump(mic_rx);
        self.spawn_live_loop(live_rx, playback.clone());
        self.spawn_quick_loop(events.fast);
        self.spawn_deep_loop(events.deep);
        self.spawn_glow_ticker(playback, in_level, audio_guard);
        self.spawn_stop_watcher();
    }

    // ---------------------------------------------------------------- loops

    fn spawn_mic_pump(&self, mut mic_rx: mpsc::Receiver<Vec<i16>>) {
        let live = self.live.clone();
        tokio::spawn(async move {
            while let Some(chunk) = mic_rx.recv().await {
                live.input_audio(base64::engine::general_purpose::STANDARD.encode(audio::i16_to_le_bytes(&chunk)));
            }
            tracing::warn!("mic channel closed");
        });
    }

    fn spawn_live_loop(self: &Arc<Self>, mut live_rx: mpsc::Receiver<LiveIncoming>, playback: Arc<audio::Playback>) {
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = live_rx.recv().await {
                this.on_live(msg, &playback).await;
            }
            tracing::warn!("live channel closed");
        });
    }

    fn spawn_quick_loop(self: &Arc<Self>, mut events: EventSubscription) {
        let fast = self.fast.clone();
        tokio::spawn(async move {
            while let Some(ev) = next_event(&mut events).await {
                fast.on_event(&ev);
            }
        });
    }

    fn spawn_deep_loop(self: &Arc<Self>, mut events: EventSubscription) {
        let this = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = next_event(&mut events).await {
                if ev.event_type == "tool.execution_start" {
                    focus::apply(&this.helper, &ev.data.text("toolName"), ev.data.args());
                }
                this.deep.on_event(&ev);
            }
        });
    }

    fn spawn_glow_ticker(
        self: &Arc<Self>,
        playback: Arc<audio::Playback>,
        in_level: Arc<audio::Meter>,
        audio_guard: std::sync::mpsc::Sender<()>,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            let _keep_audio_alive = audio_guard;
            let mut tick = tokio::time::interval(Duration::from_millis(40));
            loop {
                tick.tick().await;
                let out = playback.out_level.get();
                let inp = in_level.get();
                let speaking = out > 0.004
                    || this.last_audio_out.lock().elapsed() < Duration::from_millis(250)
                    || playback.queued_ms() > 0;
                let state = if speaking {
                    GlowState::Speaking
                } else if inp > 0.015 {
                    GlowState::Listening
                } else if this.fast.is_busy() || this.deep.is_running() {
                    GlowState::Working
                } else {
                    GlowState::Idle
                };
                this.helper.energy((inp * 6.0).min(1.0), (out * 5.0).min(1.0), state);
            }
        });
    }

    fn spawn_stop_watcher(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(400));
            loop {
                tick.tick().await;
                this.check_stop().await;
                this.check_missed_command().await;
            }
        });
    }

    // ---------------------------------------------------------------- voice

    async fn on_live(self: &Arc<Self>, msg: LiveIncoming, playback: &Arc<audio::Playback>) {
        match msg {
            LiveIncoming::Connected { attempt } => self.on_connected(attempt),
            LiveIncoming::Disconnected { fatal, reason } => {
                tracing::warn!(fatal, %reason, "live disconnected");
                if fatal {
                    let _ = self.shutdown.send(format!("voice connection ended: {reason}")).await;
                }
            }
            LiveIncoming::Event(ev) => match ev {
                LiveEvent::OutputAudioDelta { delta, .. } => match base64::engine::general_purpose::STANDARD.decode(delta.as_bytes()) {
                    Ok(bytes) => {
                        let n = self.audio_deltas.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 100 == 1 {
                            tracing::info!(deltas = n, queued_ms = playback.queued_ms(), "audio from voice");
                        }
                        playback.push(&audio::le_bytes_to_i16(&bytes));
                        *self.last_audio_out.lock() = Instant::now();
                    }
                    Err(e) => tracing::warn!("bad audio delta: {e}"),
                },
                LiveEvent::InputTranscriptDelta { delta, start_ms, end_ms } => {
                    tracing::info!(at = ?start_ms, "user: {}", delta.trim());
                    console::stream(Tag::User, &delta);
                    self.transcript.lock().push(Role::User, &delta, start_ms, end_ms);
                    self.prepare_screenshot();
                }
                LiveEvent::OutputTranscriptDelta { delta, start_ms, end_ms } => {
                    tracing::info!(at = ?start_ms, "voice: {}", delta.trim());
                    console::stream(Tag::Voice, &delta);
                    self.transcript.lock().push(Role::Assistant, &delta, start_ms, end_ms);
                    if let Some(t) = self.transcript.lock().last_assistant_text() {
                        console::status("recv", t);
                    }
                }
                LiveEvent::DelegationCreated { delegation, .. } => {
                    tracing::info!(id = %delegation.id, "delegation created");
                    console::log(Tag::Deleg, format!("voice asked for work ({})", delegation.id));
                    self.on_delegation(delegation.id).await;
                }
                LiveEvent::UsageUpdated { usage } => tracing::info!(%usage, "usage"),
                LiveEvent::Error { error, .. } => tracing::error!(%error, "live error"),
                _ => {}
            },
        }
    }

    /// Take a screenshot in the background while the user is still talking.
    fn prepare_screenshot(self: &Arc<Self>) {
        let fresh = self.preshot.lock().as_ref().map(|(t, _)| t.elapsed() < Duration::from_millis(1500)).unwrap_or(false);
        if fresh || self.preshot_busy.swap(true, Ordering::Relaxed) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            if let Ok(shot) = screen::capture(SCREENSHOT_MAX_PX).await {
                *this.preshot.lock() = Some((Instant::now(), shot));
            }
            this.preshot_busy.store(false, Ordering::Relaxed);
        });
    }

    /// The pre-taken screenshot if it is recent enough, otherwise a fresh one.
    async fn screenshot_for_request(&self) -> Option<screen::Shot> {
        if let Some((taken, shot)) = self.preshot.lock().take() {
            if taken.elapsed() <= PRESHOT_MAX_AGE {
                return Some(shot);
            }
        }
        screen::capture(SCREENSHOT_MAX_PX).await.inspect_err(|e| tracing::warn!("screenshot failed: {e:#}")).ok()
    }

    fn on_connected(&self, attempt: u32) {
        if attempt == 1 {
            self.live.instructions(None, &crate::live::prompt::greeting_instruction(&self.user_name));
        } else {
            let tail = self.transcript.lock().tail(8);
            self.live.thinking(None, &format!("The connection was briefly lost and is back. Recent conversation:\n{tail}"));
            self.live.instructions(None, "Say in a few words that you are back, then continue.");
        }
    }

    /// Safety net: the user clearly asked for something to be done, but the
    /// voice model answered without delegating (it sometimes just claims
    /// "opening it now"). Start the work ourselves.
    async fn check_missed_command(&self) {
        if self.fast.is_busy() {
            return;
        }
        let (start_ms, text) = {
            let t = self.transcript.lock();
            match t.last_user_turn() {
                Some(turn) if (FALLBACK_AFTER..=FALLBACK_WINDOW).contains(&turn.last_update.elapsed()) => (turn.start_ms, turn.text.clone()),
                _ => return,
            }
        };
        if *self.handled_turn.lock() == Some(start_ms) || !is_command(&text) {
            return;
        }
        console::log(Tag::Deleg, format!("voice did not delegate \"{}\"; starting it ourselves", clip(&text, 80)));
        self.live.thinking(None, "You answered the last request without delegating it, so nothing happened yet. The work has now been started for you. Do not claim it is done until the result arrives.");
        self.on_delegation(self.fast.next_local_id()).await;
    }

    /// Hand the recent conversation and a screenshot to the fast worker.
    async fn on_delegation(&self, id: String) {
        let conversation = {
            let mut t = self.transcript.lock();
            *self.handled_turn.lock() = t.last_user_turn().map(|turn| turn.start_ms);
            let rendered = t.render_for_delegation(CONTEXT_TURNS);
            t.mark_delegated();
            rendered
        };
        let shot = self.screenshot_for_request().await;
        self.fast.request(id, &conversation, shot).await;
    }

    // ---------------------------------------------------------------- stop

    async fn check_stop(&self) {
        let deep_running = self.deep.is_running();
        if !self.fast.is_busy() && !deep_running {
            return;
        }
        let start_ms = {
            let t = self.transcript.lock();
            match t.last_user_turn() {
                Some(turn) if turn.last_update.elapsed() > Duration::from_millis(700) && is_stop_phrase(&turn.text) => turn.start_ms,
                _ => return,
            }
        };
        if self.handled_stop_turn.lock().replace(start_ms) == Some(start_ms) {
            return;
        }
        *self.handled_turn.lock() = Some(start_ms);
        tracing::info!("stop phrase detected");
        self.fast.cancel().await;
        if deep_running {
            let _ = self.deep.cancel().await;
        }
        self.helper.clear();
        self.live.thinking(None, "The work was cancelled as the user asked. Nothing further is running.");
    }
}
