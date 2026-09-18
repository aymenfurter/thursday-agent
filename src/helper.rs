//! Bridge to the native Swift helper (glow overlay and window outlines).
//! Protocol: newline-delimited JSON on the helper's stdin; replies on stdout.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowState {
    Idle,
    Listening,
    Speaking,
    Working,
}

impl GlowState {
    fn as_str(self) -> &'static str {
        match self {
            GlowState::Idle => "idle",
            GlowState::Listening => "listening",
            GlowState::Speaking => "speaking",
            GlowState::Working => "working",
        }
    }
}

#[derive(Clone)]
pub struct HelperHandle {
    tx: Option<mpsc::UnboundedSender<Value>>,
}

impl HelperHandle {
    pub fn disabled() -> Self {
        Self { tx: None }
    }
    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }
    fn send(&self, v: Value) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(v);
        }
    }
    pub fn energy(&self, input: f32, output: f32, state: GlowState) {
        self.send(json!({"t":"energy","in":input,"out":output,"state":state.as_str()}));
    }
    pub fn highlight(&self, app: &str, title: Option<&str>, seconds: f64) {
        self.send(json!({"t":"highlight","app":app,"title":title,"seconds":seconds}));
    }
    pub fn clear(&self) {
        self.send(json!({"t":"clear"}));
    }
    /// Anchor the effect to this app's window (the assistant works here now).
    pub fn focus(&self, app: Option<&str>, title: Option<&str>) {
        self.send(json!({"t":"focus","app":app,"title":title}));
    }
    /// Flash and move to this window (the assistant just looked at it).
    pub fn glance(&self, app: Option<&str>, title: Option<&str>) {
        self.send(json!({"t":"glance","app":app,"title":title}));
    }
    pub fn quit(&self) {
        self.send(json!({"t":"quit"}));
    }
}

/// Locate the helper binary: `$THURSDAY_HELPER`, then next to the executable,
/// then the source tree build output.
pub fn find_binary() -> Option<PathBuf> {
    let rel = "thursday-agent Helper.app/Contents/MacOS/thursday-agent-helper";
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("THURSDAY_HELPER") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(rel));
            candidates.push(dir.join("../../helper/build").join(rel));
            candidates.push(dir.join("../../../helper/build").join(rel));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("helper/build").join(rel));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("helper/build").join(rel));
    candidates.into_iter().find(|p| p.is_file())
}

pub struct Helper {
    pub handle: HelperHandle,
    _child: Child,
}

/// The helper unless disabled or unavailable; the app works without it.
pub async fn spawn_optional(disabled: bool) -> Option<Helper> {
    if disabled {
        return None;
    }
    spawn().await.inspect_err(|e| eprintln!("Overlay helper unavailable ({e:#}); continuing without it.")).ok()
}

pub fn handle_of(helper: &Option<Helper>) -> HelperHandle {
    helper.as_ref().map(|h| h.handle.clone()).unwrap_or_else(HelperHandle::disabled)
}

pub async fn spawn() -> Result<Helper> {
    let bin = find_binary().context("helper binary not found; run scripts/build-helper.sh")?;
    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    let mut stdin = child.stdin.take().context("helper stdin")?;
    let stdout = child.stdout.take().context("helper stdout")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            let mut line = v.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() {
                tracing::warn!("helper stdin closed");
                break;
            }
        }
    });

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Value>();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut ready_tx = Some(ready_tx);
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<Value>(&line) {
                Ok(v) => {
                    let t = v.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    if t == "ready" {
                        if let Some(tx) = ready_tx.take() {
                            let _ = tx.send(v.clone());
                        }
                    } else if t == "error" {
                        tracing::warn!(msg = %v, "helper error");
                    } else {
                        tracing::debug!(msg = %v, "helper");
                    }
                }
                Err(_) => tracing::debug!(%line, "helper (raw)"),
            }
        }
        tracing::info!("helper stdout closed");
    });

    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), ready_rx)
        .await
        .context("helper did not report ready within 10s")?
        .context("helper exited before ready")?;
    tracing::info!(%ready, "helper ready");

    Ok(Helper {
        handle: HelperHandle { tx: Some(tx) },
        _child: child,
    })
}
