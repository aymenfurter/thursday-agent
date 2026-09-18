//! `thursday-agent probe "<a> ;; <b>"`: drive the real fast and deep workers with
//! typed prompts instead of the voice. The live console shows exactly what the
//! app would do, including everything that would be sent to the voice.
//! A prompt starting with `deep:` goes straight to the deep worker.

use anyhow::Result;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::copilot::event::next_event;
use crate::sessions::Sessions;
use crate::{helper, live, screen};

pub async fn run(cfg: Config, prompts: String, screenshot: bool, no_helper: bool) -> Result<()> {
    let helper_proc = helper::spawn_optional(no_helper).await;
    let helper = helper::handle_of(&helper_proc);
    // Nothing listens on the voice side: appends are only logged.
    let (live_handle, _voice_rx) = live::client::handle();
    let (sessions, mut events) = Sessions::start(&cfg, &helper, &live_handle).await?;
    let started = Instant::now();

    let (fast, deep) = (sessions.fast.clone(), sessions.deep.clone());
    tokio::spawn(async move { while let Some(ev) = next_event(&mut events.fast).await { fast.on_event(&ev) } });
    tokio::spawn(async move { while let Some(ev) = next_event(&mut events.deep).await { deep.on_event(&ev) } });

    // Each prompt goes out once both workers are done, the way a user follows up on a finished job.
    let mut conversation = String::new();
    for (n, prompt) in prompts.split(";;").map(str::trim).filter(|p| !p.is_empty()).enumerate() {
        conversation.push_str(&format!("User: {prompt}\n"));
        let id = format!("probe_{n}");
        if let Some(goal) = prompt.strip_prefix("deep:") {
            sessions.deep.start(goal.trim().to_string(), conversation.clone(), Some(id)).await?;
        } else {
            let shot = if screenshot { screen::capture(1024).await.ok() } else { None };
            sessions.fast.request(id, &conversation, shot).await;
        }
        while sessions.fast.is_busy() || sessions.deep.is_running() {
            anyhow::ensure!(started.elapsed() < Duration::from_secs(600), "probe timed out");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    println!("done in {:.1}s\n--- deep task report ---\n{}", started.elapsed().as_secs_f32(), sessions.deep.status_report());
    helper.quit();
    sessions.stop().await;
    Ok(())
}
