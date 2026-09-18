//! Copilot sessions. Two roles share one CLI runtime:
//! - `Quick`: the fast model, always in front. Handles short commands and
//!   quick looks, and starts, checks or cancels the deep worker.
//! - `Deep`: the strong model for long multi-step jobs, started on demand.

pub mod event;
pub mod permissions;
pub mod prompt;
pub mod summarize;

use anyhow::{Context, Result};
use github_copilot_sdk::session::Session;
use github_copilot_sdk::subscription::EventSubscription;
use github_copilot_sdk::types::{
    CustomAgentConfig, InfiniteSessionConfig, McpServerConfig, McpStdioServerConfig,
    SystemMessageConfig,
};
use github_copilot_sdk::{Client, ClientOptions, SessionConfig, Tool};
use indexmap::IndexMap;
use std::sync::Arc;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Quick,
    Deep,
}

pub struct SessionHandle {
    pub session: Arc<Session>,
    pub events: EventSubscription,
}

pub async fn start_client(cfg: &Config) -> Result<Client> {
    let mut opts = ClientOptions::default();
    opts.working_directory = cfg.workspace.clone();
    Client::start(opts).await.context("starting Copilot CLI runtime")
}

/// The fast session only gets the Peekaboo tools it needs; fewer schemas in
/// the prompt means a quicker first token.
const FAST_PEEKABOO_TOOLS: &[&str] = &[
    "see", "click", "type", "press", "scroll", "app", "window", "menu", "dialog",
];

/// Built-in Copilot tools the fast session keeps (everything else is dropped from its prompt).
const FAST_BUILTIN_TOOLS: &[&str] = &["bash", "view", "edit", "create", "grep", "glob"];

/// Peekaboo gives both sessions eyes and hands on the Mac.
fn peekaboo_server(cfg: &Config, role: Role) -> McpServerConfig {
    let mut cmd = cfg.peekaboo_command.clone();
    let command = cmd.remove(0);
    // --no-remote: capture and input stay in this process (which holds the
    // permissions); otherwise Peekaboo may pick a "bridge host" owned by
    // another app and refuse to capture the screen.
    cmd.extend(["mcp".to_string(), "--allow-foreground".to_string(), "--no-remote".to_string()]);
    let tools = match role {
        Role::Quick => FAST_PEEKABOO_TOOLS.iter().map(|s| s.to_string()).collect(),
        Role::Deep => vec!["*".to_string()],
    };
    McpServerConfig::Stdio(McpStdioServerConfig {
        tools: Some(tools),
        timeout: Some(120_000),
        command,
        args: cmd,
        env: Default::default(),
        working_directory: None,
    })
}

pub const PEEKABOO_TOOLS: &[&str] = &[
    "peekaboo-see", "peekaboo-click", "peekaboo-type", "peekaboo-press", "peekaboo-scroll",
    "peekaboo-drag", "peekaboo-app", "peekaboo-window", "peekaboo-menu", "peekaboo-dialog",
    "peekaboo-dock", "peekaboo-space", "peekaboo-set_value", "peekaboo-action",
    "peekaboo-verify_state", "peekaboo-inspect_ui", "peekaboo-paste", "peekaboo-clipboard",
];

pub const OVERLAY_TOOLS: &[&str] = &[
    "show_file", "highlight_window", "clear_annotations", "look_at_screen", "tell_user", "run_command", "focus_window",
];

fn deep_agents() -> Vec<CustomAgentConfig> {
    let mut researcher = CustomAgentConfig::new("researcher", prompt::researcher());
    researcher.display_name = Some("Researcher".into());
    researcher.description = Some("Read-only exploration: find files, read code and docs, search the web, summarise.".into());
    researcher.tools = Some(["view", "grep", "glob", "web_fetch", "bash"].iter().map(|s| s.to_string()).collect());

    let mut coder = CustomAgentConfig::new("coder", prompt::coder());
    coder.display_name = Some("Coder".into());
    coder.description = Some("Makes focused code and file changes, runs commands and tests.".into());

    let mut operator = CustomAgentConfig::new("operator", prompt::operator());
    operator.display_name = Some("Operator".into());
    operator.description = Some("Controls Mac apps and browser windows with Peekaboo.".into());
    operator.tools = Some(PEEKABOO_TOOLS.iter().chain(OVERLAY_TOOLS.iter()).map(|s| s.to_string()).collect());

    vec![researcher, coder, operator]
}

/// MCP servers from the user's global Copilot config (`~/.copilot/mcp-config.json`)
/// plus Copilot's built-in GitHub server. They are disabled for thursday-agent's
/// sessions: their tool schemas would be re-read on every model turn.
fn is_foreign_mcp_server(name: &str) -> bool {
    name != "peekaboo"
}

fn foreign_mcp_servers() -> Vec<String> {
    let mut names = vec!["github-mcp-server".to_string(), "github".to_string()];
    let path = dirs::home_dir().unwrap_or_default().join(".copilot").join("mcp-config.json");
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(obj) = v.get("mcpServers").and_then(|m| m.as_object()) {
                names.extend(obj.keys().filter(|k| is_foreign_mcp_server(k)).cloned());
            }
        }
    }
    names
}

pub async fn start_session(cfg: &Config, client: &Client, role: Role, tools: Vec<Tool>) -> Result<SessionHandle> {
    let mut mcp = IndexMap::new();
    mcp.insert("peekaboo".to_string(), peekaboo_server(cfg, role));
    let disabled = foreign_mcp_servers();

    let (name, model, reasoning, system) = match role {
        Role::Quick => ("thursday-agent-quick", &cfg.quick_model, &cfg.fast_reasoning, prompt::quick_system(&cfg.user_name, &cfg.workspace)),
        Role::Deep => ("thursday-agent-deep", &cfg.deep_model, &cfg.reasoning_effort, prompt::deep_system(&cfg.user_name, &cfg.workspace)),
    };

    let mut config = SessionConfig::default()
        .with_client_name(name)
        .with_model(model.clone())
        .with_reasoning_effort(reasoning.clone())
        .with_streaming(true)
        .with_working_directory(cfg.workspace.clone())
        .with_tools(tools)
        .with_mcp_servers(mcp)
        .with_disabled_mcp_servers(disabled.clone())
        .with_github_mcp_tool_config(github_copilot_sdk::types::GitHubMcpToolConfig::default())
        .with_infinite_sessions(InfiniteSessionConfig::new().with_enabled(true))
        .with_permission_handler(Arc::new(permissions::Policy::new(cfg.workspace.clone())))
        .with_event_buffer_capacity(8192);

    config = match role {
        // The fast session must stay fast: a compact system prompt instead of
        // Copilot's default one, only the built-in tools it needs, no skills,
        // no repo instruction discovery, no sub-agents. This is the difference
        // between ~28k and a few k tokens of prefill on every turn.
        Role::Quick => config
            .with_system_message(SystemMessageConfig::new().with_mode("replace").with_content(system))
            .with_available_tools(FAST_BUILTIN_TOOLS.iter().map(|s| format!("builtin:{s}")).chain(["custom:*".to_string(), "mcp:*".to_string()]))
            .with_enable_skills(false)
            .with_skip_custom_instructions(true)
            .with_enable_config_discovery(false),
        Role::Deep => config
            .with_system_message(SystemMessageConfig::new().with_mode("append").with_content(system))
            .with_custom_agents(deep_agents())
            // Helpers write most files; their streamed tool input lets us name a file while it is being written.
            .with_include_sub_agent_streaming_events(true),
    };

    let prepared = client.prepare_session(config).context("preparing Copilot session")?;
    let events = prepared.subscribe();
    let session = prepared.start().await.with_context(|| format!("starting {name} session"))?;
    tracing::info!(role = ?role, %model, %reasoning, session = %session.id(), disabled_mcp = ?disabled, "copilot session ready");
    Ok(SessionHandle { session: Arc::new(session), events })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    pub struct SessionFixture {
        pub session: Arc<Session>,
        client: Client,
        server: tokio::task::JoinHandle<()>,
    }

    impl SessionFixture {
        pub async fn new(send: impl Fn(Value) -> std::result::Result<Value, String> + Send + 'static) -> Self {
            let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
            let (reader, writer) = tokio::io::split(client_stream);
            let client = Client::from_streams(reader, writer, std::env::temp_dir()).unwrap();
            let server = tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(server_stream);
                let mut reader = BufReader::new(reader);
                loop {
                    let mut length = None;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).await.unwrap() == 0 {
                            return;
                        }
                        if line == "\r\n" { break; }
                        if let Some(value) = line.strip_prefix("Content-Length: ") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                    let mut body = vec![0; length.unwrap()];
                    reader.read_exact(&mut body).await.unwrap();
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    if request.get("id").is_none() { continue; }
                    let result = match request["method"].as_str().unwrap() {
                        "session.create" => Ok(json!({"sessionId": request["params"]["sessionId"]})),
                        "session.send" => send(request["params"].clone()),
                        _ => Ok(json!({})),
                    };
                    let mut response = json!({"jsonrpc": "2.0", "id": request["id"]});
                    match result {
                        Ok(result) => response["result"] = result,
                        Err(message) => response["error"] = json!({"code": -32000, "message": message}),
                    }
                    let body = response.to_string();
                    writer.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let session = client.create_session(SessionConfig::default()).await.unwrap();
            Self { session: Arc::new(session), client, server }
        }
    }

    impl Drop for SessionFixture {
        fn drop(&mut self) {
            self.client.force_stop();
            self.server.abort();
        }
    }

    pub fn event(kind: &str, data: Value) -> github_copilot_sdk::types::SessionEvent {
        serde_json::from_value(json!({
            "id": "fixture-event", "timestamp": "2026-01-01T00:00:00Z",
            "type": kind, "data": data,
        })).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn operator_uses_only_peekaboo_and_overlay_tools() {
        let agents = deep_agents();
        let operator = agents.iter().find(|agent| agent.name == "operator").unwrap();
        let tools = operator.tools.as_ref().unwrap();
        let expected: Vec<String> = PEEKABOO_TOOLS.iter().chain(OVERLAY_TOOLS.iter()).map(|s| s.to_string()).collect();
        assert_eq!(tools, &expected);
        assert!(tools.contains(&"peekaboo-see".to_string()));
        assert!(tools.contains(&"peekaboo-click".to_string()));
        assert!(tools.contains(&"peekaboo-type".to_string()));
    }

    #[test]
    fn workers_and_operator_use_peekaboo() {
        for prompt in [
            prompt::quick_system("User", Path::new("/tmp")),
            prompt::deep_system("User", Path::new("/tmp")),
            prompt::operator(),
        ] {
            assert!(prompt.contains("peekaboo-see"));
            assert!(prompt.contains("browser"));
        }
    }

    #[test]
    fn annotations_use_native_stickies_in_all_worker_prompts() {
        for prompt in [
            prompt::quick_system("User", Path::new("/tmp")),
            prompt::deep_system("User", Path::new("/tmp")),
            prompt::operator(),
        ] {
            assert!(prompt.contains("native macOS Stickies app through Peekaboo"));
            assert!(prompt.contains("without covering it"));
            assert!(prompt.contains("Do not change or close the user's existing notes"));
            assert!(prompt.contains("remain until closed"));
            assert!(prompt.contains("`clear_annotations` only removes thursday-agent's window highlights"));
            assert!(!prompt.contains("add_note"));
        }
        let agents = deep_agents();
        let tools = agents.iter().find(|agent| agent.name == "operator").unwrap().tools.as_ref().unwrap();
        assert!(!tools.iter().any(|name| name == "add_note"));
        assert!(tools.iter().any(|name| name == "highlight_window"));
        assert!(tools.iter().any(|name| name == "clear_annotations"));
    }

    #[test]
    fn only_peekaboo_is_kept_from_global_mcp_configuration() {
        assert!(!is_foreign_mcp_server("peekaboo"));
        for name in ["github", "external-tools", "another-server"] {
            assert!(is_foreign_mcp_server(name));
        }
    }
}
