//! The two Copilot sessions and their workers, wired the same way for the
//! real app and for `probe`.

use anyhow::{Context, Result};
use github_copilot_sdk::Client;
use github_copilot_sdk::subscription::EventSubscription;
use github_copilot_sdk::types::MessageOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::console::{self, Tag};
use crate::copilot::{self, Role};
use crate::deep::DeepRunner;
use crate::fast::FastRunner;
use crate::helper::HelperHandle;
use crate::live::LiveHandle;
use crate::tools::{self, ToolDeps};

/// Event streams of the two sessions, consumed by whoever drives them.
pub struct Events {
    pub fast: EventSubscription,
    pub deep: EventSubscription,
}

pub struct Sessions {
    client: Client,
    pub fast: Arc<FastRunner>,
    pub deep: Arc<DeepRunner>,
    sessions: [Arc<github_copilot_sdk::session::Session>; 2],
}

impl Sessions {
    /// Deep first, then the fast one that controls it. Each session gets its own
    /// tool state so "already spoken" flags never cross over.
    pub async fn start(cfg: &Config, helper: &HelperHandle, live: &LiveHandle) -> Result<(Self, Events)> {
        let client = copilot::start_client(cfg).await?;
        if !client.get_auth_status().await.context("copilot auth status")?.is_authenticated {
            anyhow::bail!("Copilot is not signed in. Run: copilot login");
        }
        let deps = || ToolDeps::new(helper.clone(), live.clone(), cfg.workspace.clone());
        let started = async {
            let deep_deps = deps();
            let deep = copilot::start_session(cfg, &client, Role::Deep, tools::overlay_tools(deep_deps.clone())).await?;
            let deep_runner = DeepRunner::new(deep.session.clone(), deep_deps);

            let fast_deps = deps();
            let mut fast_tools = tools::overlay_tools(fast_deps.clone());
            fast_tools.extend(tools::deep_task_tools(fast_deps.clone(), deep_runner.clone()));
            let fast = copilot::start_session(cfg, &client, Role::Quick, fast_tools).await?;
            anyhow::Ok((deep, deep_runner, fast_deps, fast))
        }.await;
        let (deep, deep_runner, fast_deps, fast) = match started {
            Ok(workers) => workers,
            Err(error) => {
                let _ = client.stop().await;
                return Err(error);
            }
        };
        console::log(Tag::Sys, format!("sessions ready: fast {} ({}), deep {} ({}); both stay open for the whole run", fast.session.id(), cfg.quick_model, deep.session.id(), cfg.deep_model));

        let sessions = Self {
            client,
            fast: FastRunner::new(fast.session.clone(), fast_deps, deep_runner.clone()),
            deep: deep_runner,
            sessions: [fast.session, deep.session],
        };
        Ok((sessions, Events { fast: fast.events, deep: deep.events }))
    }

    /// One throwaway turn so the first real request does not pay the cold start.
    pub async fn warm_up(&self) {
        let t0 = Instant::now();
        let warm = MessageOptions::new("Warm-up. Reply with the single word: ready").with_wait_timeout(Duration::from_secs(30));
        match self.sessions[0].send_and_wait(warm).await {
            Ok(_) => console::log(Tag::Sys, format!("fast session warmed up in {:.1}s", t0.elapsed().as_secs_f32())),
            Err(e) => console::log(Tag::Sys, format!("fast session warm-up skipped: {e}")),
        }
    }

    pub async fn stop(self) {
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            for s in &self.sessions {
                let _ = s.disconnect().await;
            }
            let _ = self.client.stop().await;
        })
        .await;
    }
}
