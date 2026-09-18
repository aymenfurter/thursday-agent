//! thursday-agent: a GUI-less, voice-driven Copilot for macOS.

mod app;
mod app_bridge;
mod audio;
mod auth;
mod config;
mod console;
mod copilot;
mod deep;
mod doctor;
mod fast;
mod focus;
mod helper;
mod live;
mod logging;
mod narration;
mod orchestrator;
mod probe;
mod screen;
mod sessions;
mod setup;
mod text;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "thursday-agent", version, about = "A voice-only Copilot for your Mac. Start it, say hello.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// Also print logs to stderr (the default is a log file only).
    #[arg(long, global = true)]
    verbose: bool,
    /// Working directory the assistant operates in (default: current directory or config).
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    /// Fast model for quick commands and checks (default: gpt-5.6-luna).
    #[arg(long, global = true)]
    quick_model: Option<String>,
    /// Deep model for long multi-step tasks (default: gpt-6-astra).
    #[arg(long, global = true)]
    deep_model: Option<String>,
    /// Reasoning effort of the deep model: none, minimal, low, medium, high, xhigh, max (default: low).
    #[arg(long, global = true)]
    reasoning: Option<String>,
    /// Reasoning effort of the fast model: none, low, medium, high (default: none, the snappiest).
    #[arg(long, global = true)]
    fast_reasoning: Option<String>,
    /// Voice provider: azure (Azure CLI auth) or openai (API key).
    #[arg(long, global = true, value_enum)]
    provider: Option<config::Provider>,
    /// Resource root or full secure live WebSocket URL.
    #[arg(long, global = true)]
    endpoint: Option<String>,
    /// Voice model, or Azure deployment name (default: gpt-live-1).
    #[arg(long, global = true)]
    live_model: Option<String>,
    /// Azure subscription used by `az account get-access-token`.
    #[arg(long, global = true)]
    azure_subscription: Option<String>,
    /// Azure tenant to select, or verify when a subscription is provided.
    #[arg(long, global = true)]
    azure_tenant: Option<String>,
    /// Run without the native overlay helper (no glow or highlights).
    #[arg(long, global = true)]
    no_helper: bool,
    /// Do not print the live debug console to the terminal.
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Start listening (default).
    Run,
    /// Select Azure CLI or OpenAI API-key auth and store preferences.
    Setup,
    /// Check live authentication, Copilot login, permissions, audio and the helper.
    Doctor,
    /// Test a real live session, without audio or workers (55-second limit).
    VoiceCheck,
    /// Drive the real Copilot workers with typed prompts ("a ;; b"); no voice needed.
    #[command(hide = true)]
    Probe {
        prompts: String,
        /// Attach a screenshot like a real delegation would.
        #[arg(long)]
        screenshot: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Cmd::Run);
    let overrides = config::Overrides {
        workspace: cli.workspace,
        quick_model: cli.quick_model,
        deep_model: cli.deep_model,
        reasoning: cli.reasoning,
        fast_reasoning: cli.fast_reasoning,
        provider: cli.provider,
        endpoint: cli.endpoint,
        live_model: cli.live_model,
        azure_subscription: cli.azure_subscription,
        azure_tenant: cli.azure_tenant,
    };
    if matches!(command, Cmd::Setup) {
        return setup::run(&overrides);
    }
    let cfg = config::resolve(&overrides)?;
    let _log_guard = logging::init(cli.verbose)?;
    let checking_voice = matches!(command, Cmd::VoiceCheck);
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async move {
        console::init(!cli.quiet && !matches!(command, Cmd::Doctor | Cmd::VoiceCheck));
        match command {
            Cmd::Run => app::run(cfg, cli.no_helper).await,
            Cmd::Doctor => doctor::run(cfg, cli.no_helper).await,
            Cmd::VoiceCheck => {
                live::client::voice_check(live::client::ConnectParams {
                    url: cfg.live_url.clone(),
                    auth: cfg.live_auth,
                    instructions: "Connection check only. Do not speak or delegate.".into(),
                    voice: cfg.voice,
                    model: cfg.live_model.clone(),
                }).await?;
                println!("[ok] {} session.started: {} (model/deployment: {})", cfg.live_provider, cfg.live_url, cfg.live_model);
                Ok(())
            }
            Cmd::Probe { prompts, screenshot } => probe::run(cfg, prompts, screenshot, cli.no_helper).await,
            Cmd::Setup => unreachable!("handled above"),
        }
    });
    if checking_voice {
        // A cancelled OS DNS lookup must not extend the diagnostic's deadline.
        runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_uses_the_product_name() {
        use clap::CommandFactory;
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("Usage: thursday-agent"));
        assert_eq!(env!("CARGO_PKG_NAME"), "thursday-agent");
    }

    #[test]
    fn live_flags_are_typed_and_global() {
        let cli = Cli::try_parse_from([
            "thursday-agent", "voice-check", "--provider", "azure", "--endpoint", "https://resource.azure.com",
            "--live-model", "deployment", "--azure-subscription", "subscription", "--azure-tenant", "tenant",
        ]).unwrap();
        assert!(matches!(cli.command, Some(Cmd::VoiceCheck)));
        assert_eq!(cli.provider, Some(config::Provider::Azure));
        assert_eq!(cli.live_model.as_deref(), Some("deployment"));
        assert_eq!(cli.azure_subscription.as_deref(), Some("subscription"));
        assert_eq!(cli.azure_tenant.as_deref(), Some("tenant"));
        assert!(Cli::try_parse_from(["thursday-agent", "--provider", "other", "voice-check"]).is_err());
        assert_eq!(Cli::try_parse_from(["thursday-agent", "--provider", "openai", "voice-check"]).unwrap().provider, Some(config::Provider::Openai));
    }
}
