//! `thursday-agent run`: voice, workers, overlay and audio, until Ctrl-C.

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::console::{self, Tag};
use crate::sessions::Sessions;
use crate::{audio, helper, live, logging, orchestrator::Orchestrator, screen};

pub async fn run(cfg: Config, no_helper: bool) -> Result<()> {
    cfg.live_auth.validate()?;
    screen::sweep();
    tracing::info!(workspace = %cfg.workspace.display(), live_url = %cfg.live_url, "starting");
    console::log(Tag::Sys, format!("workspace {} · fast {} ({}) · deep {} ({})", cfg.workspace.display(), cfg.quick_model, cfg.fast_reasoning, cfg.deep_model, cfg.reasoning_effort));

    let helper_proc = helper::spawn_optional(no_helper).await;
    let helper = helper::handle_of(&helper_proc);
    let audio = tokio::task::spawn_blocking(audio::start).await??;
    let (live_handle, live_cmd_rx) = live::client::handle();

    // Copilot before the voice: fail fast if not signed in.
    let (sessions, events) = Sessions::start(&cfg, &helper, &live_handle).await?;
    sessions.warm_up().await;
    console::status("fast", "idle");
    console::status("deep", "none");
    console::status("voice", "connecting");

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<String>(4);
    let (live_in_tx, live_in_rx) = mpsc::channel::<live::LiveIncoming>(256);
    let orch = Orchestrator::new(live_handle.clone(), helper.clone(), sessions.fast.clone(), sessions.deep.clone(), cfg.user_name.clone(), shutdown_tx.clone());
    orch.run(live_in_rx, events, audio);

    let params = live::client::ConnectParams {
        url: cfg.live_url.clone(),
        auth: cfg.live_auth.clone(),
        instructions: live::prompt::instructions(&cfg.user_name),
        voice: cfg.voice.clone(),
        model: cfg.live_model.clone(),
    };
    let live_task = tokio::spawn(async move {
        if let Err(e) = live::connect_forever(params, live_cmd_rx, live_in_tx).await {
            let _ = shutdown_tx.send(format!("{e:#}")).await;
        }
    });

    eprintln!("thursday-agent is listening. Press Ctrl-C to stop. Logs: {}", logging::log_path().display());
    if console::is_enabled() {
        eprintln!("Live console: VOICE = what the voice model says, USER = what it heard, DELEG = it asked for work, SEND = what we tell it, FAST/DEEP = Copilot sessions. --quiet to hide.");
    }
    let result = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl-c");
            signal.context("waiting for Ctrl-C")
        }
        Some(reason) = shutdown_rx.recv() => {
            tracing::error!(%reason, "shutting down");
            eprintln!("Stopping: {reason}");
            Err(anyhow::anyhow!(reason))
        }
    };

    live_handle.close();
    helper.clear();
    helper.quit();
    sessions.stop().await;
    live_task.abort();
    result
}
