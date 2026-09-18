//! GPT-Live WebSocket client with automatic reconnect. One persistent command
//! channel feeds whichever socket is currently alive.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, Request};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::auth::{AuthError, LiveAuth};
use super::events::LiveEvent;

/// Approximate upper bound for one append (spec: 500 tokens). ~3.5 chars/token,
/// kept conservative.
pub const MAX_APPEND_CHARS: usize = 1_400;

#[derive(Clone)]
pub struct LiveHandle {
    tx: mpsc::UnboundedSender<Value>,
    counter: Arc<AtomicU64>,
}

impl LiveHandle {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub fn send_raw(&self, v: Value) {
        if self.tx.send(v).is_err() {
            tracing::warn!("live command dropped: connection task gone");
        }
    }

    pub fn input_audio(&self, base64_pcm: String) {
        self.send_raw(json!({
            "type": "session.input_audio.append",
            "event_id": self.next_id("audio"),
            "audio": base64_pcm,
        }));
    }

    fn append(&self, kind: &str, delegation_id: Option<&str>, content: &str) {
        let content = clamp(content, MAX_APPEND_CHARS);
        tracing::info!(kind, delegation = ?delegation_id, %content, "append");
        crate::console::log(crate::console::Tag::Send, format!("{kind}{}: {content}", delegation_id.map(|d| format!(" [{}]", &d[..d.len().min(10)])).unwrap_or_default()));
        crate::console::status("sent", format!("{kind}: {content}"));
        self.send_raw(json!({
            "type": format!("session.{kind}.append"),
            "event_id": self.next_id(kind),
            "delegation_id": delegation_id,
            "content": content,
        }));
    }

    /// Quiet context the model may use but does not speak on arrival.
    pub fn thinking(&self, delegation_id: Option<&str>, content: &str) {
        self.append("thinking", delegation_id, content);
    }

    /// Something the model should say aloud (it may paraphrase).
    pub fn commentary(&self, delegation_id: Option<&str>, content: &str) {
        self.append("commentary", delegation_id, content);
    }

    /// Trusted behavioural instruction.
    pub fn instructions(&self, delegation_id: Option<&str>, content: &str) {
        self.append("instructions", delegation_id, content);
    }

    pub fn close(&self) {
        self.send_raw(json!({ "type": "session.close", "event_id": self.next_id("close") }));
    }
}

/// Clamp text to `max` chars at a sentence or word boundary.
pub fn clamp(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let boundary = cut
        .rfind(['.', '!', '?', '\n'])
        .filter(|i| *i > max / 2)
        .map(|i| i + 1)
        .or_else(|| cut.rfind(' '))
        .unwrap_or(cut.len());
    let mut s = cut[..boundary].trim_end().to_string();
    if !s.ends_with(['.', '!', '?']) {
        s.push('…');
    }
    s
}

pub enum LiveIncoming {
    Event(LiveEvent),
    /// A fresh socket is up and `session.started` was received.
    Connected { attempt: u32 },
    /// The socket died; a reconnect will follow unless `fatal`.
    Disconnected { fatal: bool, reason: String },
}

pub struct ConnectParams {
    pub url: String,
    pub auth: LiveAuth,
    pub instructions: String,
    pub voice: String,
    pub model: String,
}

pub fn handle() -> (LiveHandle, mpsc::UnboundedReceiver<Value>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        LiveHandle {
            tx,
            counter: Arc::new(AtomicU64::new(1)),
        },
        rx,
    )
}

/// Run the connection loop until the command channel closes or a fatal error.
pub async fn connect_forever(
    params: ConnectParams,
    mut commands: mpsc::UnboundedReceiver<Value>,
    incoming: mpsc::Sender<LiveIncoming>,
) -> Result<()> {
    let mut attempt: u32 = 0;
    let mut backoff = Duration::from_secs(1);
    loop {
        attempt += 1;
        match run_once(&params, &mut commands, &incoming, attempt).await {
            Ok(Ended::CommandsClosed) => return Ok(()),
            Ok(Ended::Closed(reason)) => {
                let fatal = reason.contains("duration limit");
                let _ = incoming
                    .send(LiveIncoming::Disconnected { fatal, reason: reason.clone() })
                    .await;
                if fatal {
                    return Ok(());
                }
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                let auth_failed = e.downcast_ref::<AuthError>().is_some();
                let reason = format!("{e:#}");
                tracing::warn!(attempt, %reason, "live connection failed");
                crate::console::status("voice", format!("disconnected: {}", crate::text::clip(&reason, 60)));
                let fatal = auth_failed || attempt >= 8 || is_auth_error(&reason);
                let _ = incoming
                    .send(LiveIncoming::Disconnected { fatal, reason: reason.clone() })
                    .await;
                if fatal {
                    anyhow::bail!("GPT-Live connection failed: {reason}");
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(20));
            }
        }
    }
}

fn is_auth_error(reason: &str) -> bool {
    reason.contains("401") || reason.contains("403") || reason.to_ascii_lowercase().contains("unauthorized")
}

enum Ended {
    CommandsClosed,
    Closed(String),
}

type LiveSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn websocket_request(url: &str, authorization: HeaderValue) -> Result<Request<()>> {
    let mut request = url.into_client_request().context("building websocket request")?;
    request.headers_mut().insert("Authorization", authorization);
    request.headers_mut().insert("User-Agent", HeaderValue::from_static("thursday-agent/0.1"));
    Ok(request)
}

fn session_start(params: &ConnectParams) -> Value {
    json!({
        "type": "session.start",
        "event_id": "event_start",
        "session": {
            "model": params.model,
            "instructions": params.instructions,
            "audio": {
                "format": { "type": "audio/pcm", "rate": 24000 },
                "output": { "voice": params.voice }
            },
            "delegation": { "type": "client" }
        }
    })
}

async fn open_session(params: &ConnectParams) -> Result<LiveSocket> {
    // Obtain a new Azure token on every connection, including each reconnect.
    let authorization = params.auth.authorization_header().await?;
    let request = websocket_request(&params.url, authorization)?;
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(request))
        .await.context("websocket connection timed out")?
        .context("websocket connect")?;
    ws.send(Message::Text(session_start(params).to_string().into())).await?;
    Ok(ws)
}

/// A real session handshake with no microphone, workers, tools, or native helper.
pub async fn voice_check(params: ConnectParams) -> Result<()> {
    voice_check_with_timeout(params, Duration::from_secs(55)).await
}

async fn voice_check_with_timeout(params: ConnectParams, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, async {
        let mut ws = open_session(&params).await?;
        while let Some(message) = ws.next().await {
            match message.context("receiving live session handshake")? {
                Message::Text(text) => match LiveEvent::parse(&text).context("invalid live session event")? {
                    LiveEvent::SessionStarted => {
                        ws.send(Message::Text(json!({
                            "type": "session.close", "event_id": "event_check_close",
                        }).to_string().into())).await?;
                        ws.close(None).await?;
                        return Ok(());
                    }
                    LiveEvent::Error { error } => anyhow::bail!("session.start rejected: {error}"),
                    LiveEvent::SessionClosed { .. } => anyhow::bail!("session closed before session.started"),
                    _ => {}
                },
                Message::Ping(payload) => ws.send(Message::Pong(payload)).await?,
                Message::Close(_) => anyhow::bail!("socket closed before session.started"),
                _ => {}
            }
        }
        anyhow::bail!("socket closed before session.started")
    }).await.context("voice-check timed out waiting for session.started/close")?
}

async fn run_once(
    params: &ConnectParams,
    commands: &mut mpsc::UnboundedReceiver<Value>,
    incoming: &mpsc::Sender<LiveIncoming>,
    attempt: u32,
) -> Result<Ended> {
    let ws = open_session(params).await?;
    let (mut sink, mut stream) = ws.split();
    let startup_timeout = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(startup_timeout);
    let mut started = false;
    loop {
        tokio::select! {
            _ = &mut startup_timeout, if !started => {
                anyhow::bail!("timed out waiting for session.started");
            }
            cmd = commands.recv() => {
                match cmd {
                    Some(v) => {
                        if let Err(e) = sink.send(Message::Text(v.to_string().into())).await {
                            anyhow::bail!("send failed: {e}");
                        }
                    }
                    None => {
                        let _ = sink.send(Message::Close(None)).await;
                        return Ok(Ended::CommandsClosed);
                    }
                }
            }
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => anyhow::bail!("receive failed: {e}"),
                    None => {
                        if started {
                            return Ok(Ended::Closed("socket closed".into()));
                        }
                        anyhow::bail!("socket closed before session.started");
                    }
                };
                match msg {
                    Message::Text(txt) => {
                        let ev = match LiveEvent::parse(&txt) {
                            Ok(ev) => ev,
                            Err(e) => {
                                tracing::debug!(error = %e, raw = %crate::text::clip(&txt, 300), "unparsed live event");
                                continue;
                            }
                        };
                        match &ev {
                            LiveEvent::SessionStarted => {
                                started = true;
                                tracing::info!(attempt, "live session started");
                                crate::console::status("voice", format!("connected (attempt {attempt})"));
                                let _ = incoming.send(LiveIncoming::Connected { attempt }).await;
                            }
                            LiveEvent::SessionClosed { reason, usage } => {
                                tracing::info!(?reason, ?usage, "live session closed");
                                let r = reason.clone().unwrap_or_else(|| "closed".into());
                                let _ = incoming.send(LiveIncoming::Event(ev.clone())).await;
                                return Ok(Ended::Closed(r));
                            }
                            LiveEvent::Error { error, .. } => {
                                tracing::error!(%error, "live error event");
                                crate::console::log(crate::console::Tag::Err, format!("voice: {error}"));
                                if !started {
                                    anyhow::bail!("session.start rejected: {error}");
                                }
                            }
                            LiveEvent::OutputAudioDelta { .. } => {}
                            LiveEvent::Other => tracing::debug!(raw = %crate::text::clip(&txt, 240), "live event (unhandled type)"),
                            other => tracing::debug!(?other, "live event"),
                        }
                        if incoming.send(LiveIncoming::Event(ev)).await.is_err() {
                            return Ok(Ended::CommandsClosed);
                        }
                    }
                    Message::Close(frame) => {
                        if !started {
                            anyhow::bail!("socket closed before session.started");
                        }
                        let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                        return Ok(Ended::Closed(format!("close frame: {reason}")));
                    }
                    Message::Ping(p) => { let _ = sink.send(Message::Pong(p)).await; }
                    _ => {}
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_short_text() {
        assert_eq!(clamp("hello", 10), "hello");
    }

    #[test]
    fn clamp_cuts_at_sentence() {
        let long = "First sentence here. Second sentence is longer. Third one goes over the limit for sure.";
        let c = clamp(long, 50);
        assert_eq!(c, "First sentence here. Second sentence is longer.");
    }

    #[test]
    fn clamp_adds_ellipsis_on_word_cut() {
        let c = clamp("alpha beta gamma delta epsilon", 12);
        assert_eq!(c, "alpha beta…");
    }

    fn params(url: String) -> ConnectParams {
        ConnectParams {
            url, auth: LiveAuth::OpenAi { api_key: "fixture-key".into() },
            instructions: "Connection check only.".into(), voice: "marin".into(),
            model: "custom-deployment".into(),
        }
    }

    #[tokio::test]
    async fn voice_check_sends_bearer_and_deployment_then_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/openai/v1/live/sessions", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(socket, |request: &Request<()>, response| {
                assert_eq!(request.headers()["Authorization"], "Bearer fixture-key");
                assert_eq!(request.headers()["User-Agent"], "thursday-agent/0.1");
                assert!(!request.headers().contains_key("api-key"));
                assert_eq!(request.uri().path(), "/openai/v1/live/sessions");
                Ok(response)
            }).await.unwrap();
            let start: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(start["type"], "session.start");
            assert_eq!(start["session"]["model"], "custom-deployment");
            assert_eq!(start["session"]["audio"]["format"]["rate"], 24000);
            ws.send(Message::Ping(vec![1].into())).await.unwrap();
            assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Pong(_)));
            ws.send(Message::Text(r#"{"type":"session.started"}"#.into())).await.unwrap();
            let close: Value = serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(close["type"], "session.close");
            assert!(matches!(ws.next().await.unwrap().unwrap(), Message::Close(_)));
        });
        voice_check_with_timeout(params(url), Duration::from_secs(3)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn voice_check_rejects_server_errors_or_early_close() {
        for message in [
            Message::Text(r#"{"type":"error","error":{"code":"deployment_not_found"}}"#.into()),
            Message::Text(r#"{"type":"session.closed"}"#.into()),
            Message::Close(None),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/v1/live/sessions", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
                ws.next().await.unwrap().unwrap();
                ws.send(message).await.unwrap();
            });
            assert!(voice_check_with_timeout(params(url), Duration::from_secs(3)).await.is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn voice_check_times_out_when_session_never_starts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/live/sessions", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.next().await.unwrap().unwrap();
            while ws.next().await.is_some_and(|message| message.is_ok()) {}
        });
        let error = voice_check_with_timeout(params(url), Duration::from_millis(150)).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
        tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invalid_auth_is_fatal_without_reconnect() {
        let mut params = params("ws://127.0.0.1:1".into());
        params.auth = LiveAuth::OpenAi { api_key: String::new() };
        let (_handle, commands) = handle();
        let (tx, mut incoming) = mpsc::channel(1);
        let result = tokio::time::timeout(Duration::from_secs(1), connect_forever(params, commands, tx)).await.unwrap();
        assert!(result.is_err());
        assert!(matches!(incoming.recv().await, Some(LiveIncoming::Disconnected { fatal: true, .. })));
    }
}
