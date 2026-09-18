//! Custom tools exposed to the Copilot sessions.
//! - overlay tools: show, highlight, look, speak (both sessions)
//! - deep-task tools: start, status, cancel (fast session only)

use github_copilot_sdk::tool::{JsonSchema, define_tool};
use github_copilot_sdk::types::{ToolBinaryResult, ToolResultExpanded};
use github_copilot_sdk::{Tool, ToolResult};
use parking_lot::Mutex;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::deep::DeepRunner;
use crate::helper::HelperHandle;
use crate::live::LiveHandle;
use crate::screen;
use crate::text::resolve_path;

/// Shared handles the tools need.
pub struct ToolDeps {
    pub helper: HelperHandle,
    pub live: LiveHandle,
    pub workspace: PathBuf,
    /// Delegation the fast session is currently answering (set by the orchestrator).
    pub current_delegation: Mutex<Option<String>>,
    /// Recent conversation, refreshed by the orchestrator, handed to deep tasks as context.
    pub recent_conversation: Mutex<String>,
    /// Set by a tool that already spoke its confirmation (`say_on_success`);
    /// the orchestrator then skips the model's final turn.
    pub spoke_early: AtomicBool,
    /// The sentence most recently spoken through a tool.
    pub last_said: Mutex<String>,
}

impl ToolDeps {
    pub fn new(helper: HelperHandle, live: LiveHandle, workspace: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            helper,
            live,
            workspace,
            current_delegation: Mutex::new(None),
            recent_conversation: Mutex::new(String::new()),
            spoke_early: AtomicBool::new(false),
            last_said: Mutex::new(String::new()),
        })
    }

    fn say(&self, text: &str) {
        self.live.commentary(self.current_delegation.lock().as_deref(), text);
    }

    /// Speak a tool's `say_on_success` right away and flag the request as answered.
    fn say_now(&self, text: &Option<String>) {
        if let Some(text) = text.as_deref().filter(|t| !t.trim().is_empty()) {
            self.say(text);
            *self.last_said.lock() = text.to_string();
            self.spoke_early.store(true, Ordering::Relaxed);
        }
    }
}

type Outcome = Result<ToolResult, github_copilot_sdk::Error>;

fn text(s: impl Into<String>) -> Outcome {
    Ok(ToolResult::Text(s.into()))
}

struct CommandGroup(Option<libc::pid_t>);

impl Drop for CommandGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // This process group contains only the shell and children started for this call.
            if unsafe { libc::kill(-pid, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    tracing::error!(pid, %error, "could not stop command process group");
                }
            }
        }
    }
}

async fn run_shell(command: &str, workspace: &Path, timeout: Duration) -> anyhow::Result<Output> {
    let child = tokio::process::Command::new("/bin/zsh")
        .arg("-lc").arg(command).current_dir(workspace)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .process_group(0).kill_on_drop(true).spawn()?;
    let mut group = CommandGroup(Some(child.id().expect("new child has a PID") as libc::pid_t));
    let output = tokio::time::timeout(timeout, child.wait_with_output()).await
        .map_err(|_| anyhow::anyhow!("Timed out after {} s: {command}", timeout.as_secs()))??;
    group.0 = None;
    Ok(output)
}

/// A tool whose handler gets the shared context `C` and its typed parameters.
/// All our tools are harmless or do their own checks, so none asks for permission.
fn tool<C, P, F, Fut>(name: &str, description: &str, ctx: &C, handler: F) -> Tool
where
    C: Clone + Send + Sync + 'static,
    P: JsonSchema + serde::de::DeserializeOwned + Send + 'static,
    F: Fn(C, P) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Outcome> + Send + 'static,
{
    let ctx = ctx.clone();
    define_tool(name, description, move |_inv, p: P| handler(ctx.clone(), p)).with_skip_permission(true)
}

#[derive(Deserialize, JsonSchema)]
struct ShowFileParams {
    /// Path of the file or folder to open, absolute or relative to the workspace.
    path: String,
    /// Application to open it with (e.g. "Visual Studio Code", "Preview"). Default app when omitted.
    app: Option<String>,
    /// Highlight the window after opening (default true).
    highlight: Option<bool>,
    /// Spoken confirmation to say the moment this succeeds (one short sentence). Then you are done; no final message needed.
    say_on_success: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct RunCommandParams {
    /// Shell command (zsh). Use for `open`, `osascript` and other one-shot commands.
    command: String,
    /// Spoken confirmation to say the moment the command exits 0 (one short sentence, e.g. "New tab is open."). Then you are done; no final message needed.
    say_on_success: Option<String>,
    /// Timeout in seconds (default 20).
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
struct HighlightParams {
    /// Application name, bundle id, or "frontmost".
    app: String,
    /// Optional window title (substring) to pick a specific window.
    title: Option<String>,
    /// How long the outline stays visible, in seconds (default 8).
    seconds: Option<f64>,
    /// Spoken confirmation to say right away (one short sentence). Then you are done.
    say_on_success: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct NoParams {}

#[derive(Deserialize, JsonSchema)]
struct FocusParams {
    /// Application name or bundle id whose window you are working in, or "frontmost".
    app: String,
    /// Optional window title (substring) to pick a specific window.
    title: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct TellParams {
    /// One or two short sentences the voice should say right now.
    text: String,
}

#[derive(Deserialize, JsonSchema)]
struct AppPromptParams {
    /// Request for the Copilot App agent, such as "list all sessions" or
    /// "get the last activity of session X". Include the user's full request.
    prompt: String,
}

#[derive(Deserialize, JsonSchema)]
struct ContinueDeepParams {
    /// The user's answer or follow-up, in their words.
    message: String,
}

#[derive(Deserialize, JsonSchema)]
struct StartDeepParams {
    /// Precise, self-contained description of the whole job, with every detail the user gave.
    goal: String,
}

pub fn overlay_tools(deps: Arc<ToolDeps>) -> Vec<Tool> {
    vec![
        define_tool(
            "ask_copilot_app",
            "Send a prompt to the connected Copilot App session and return its answer. Use it to list app sessions, get the last activity of a named session, or communicate with a running session when the user asks. The app agent uses its own tools and permissions. Do not call this recursively from the receiving app session.",
            |_inv, p: AppPromptParams| async move {
                match crate::app_bridge::ask(&p.prompt).await {
                    Ok(answer) => text(answer),
                    Err(error) => Ok(ToolResult::Expanded(ToolResultExpanded::new(
                        format!("Copilot App request failed: {error:#}"), "failure",
                    ))),
                }
            },
        ),
        tool(
            "show_file",
            "Open a file or folder on the user's screen with its default (or a named) application, bring it to front and highlight the window. Use this to show results instead of describing them.",
            &deps,
            |d: Arc<ToolDeps>, p: ShowFileParams| async move {
                let path = resolve_path(&d.workspace, &p.path);
                if !path.exists() {
                    return text(format!("Cannot open: {} does not exist", path.display()));
                }
                let mut cmd = tokio::process::Command::new("open");
                if let Some(app) = &p.app {
                    cmd.arg("-a").arg(app);
                }
                if !cmd.arg(&path).status().await.map(|s| s.success()).unwrap_or(false) {
                    return text(format!("open failed for {}", path.display()));
                }
                if p.highlight.unwrap_or(true) && d.helper.is_enabled() {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    d.helper.highlight("frontmost", None, 6.0);
                }
                d.say_now(&p.say_on_success);
                text(format!("Opened {} on screen and highlighted its window.", path.display()))
            },
        ),
        tool(
            "highlight_window",
            "Draw a glowing outline around an application window on the user's screen for a few seconds so they know where to look.",
            &deps,
            |d: Arc<ToolDeps>, p: HighlightParams| async move {
                if !d.helper.is_enabled() {
                    return text("Overlay not available; describe the location in words instead.");
                }
                d.helper.highlight(&p.app, p.title.as_deref(), p.seconds.unwrap_or(8.0));
                d.say_now(&p.say_on_success);
                text(format!("Highlighted the {} window.", p.app))
            },
        ),
        tool("clear_annotations", "Remove all thursday-agent window highlights. Does not remove notes in macOS Stickies.", &deps, |d: Arc<ToolDeps>, _: NoParams| async move {
            d.helper.clear();
            text("Cleared thursday-agent's window highlights.")
        }),
        tool(
            "look_at_screen",
            "Take a fresh screenshot of the user's whole screen and return it as an image. For app or browser GUI clicks, use peekaboo-see to get element ids.",
            &(),
            |_, _: NoParams| async move {
                match screen::capture(1600).await {
                    Ok(shot) => Ok(ToolResult::Expanded(ToolResultExpanded::new("Screenshot of the user's screen attached.", "success").with_binary_results(vec![
                        ToolBinaryResult { data: shot.jpeg_base64, mime_type: "image/jpeg".into(), r#type: "image".into(), description: Some("Current screen".into()) },
                    ]))),
                    Err(e) => text(format!("Screenshot failed: {e:#}")),
                }
            },
        ),
        tool(
            "tell_user",
            "Say one or two short sentences to the user right now, out loud. Use it for a needed decision or a single question. Do not use it for the final summary.",
            &deps,
            |d: Arc<ToolDeps>, p: TellParams| async move {
                d.say(&p.text);
                text("Said to the user.")
            },
        ),
        tool(
            "run_command",
            "Run one shell command (zsh) and return its output. Use it for `open`, `osascript` and other one-shot commands. If you already know what to say when it succeeds, pass `say_on_success`: it is spoken immediately and you do not need a final message.",
            &deps,
            |d: Arc<ToolDeps>, p: RunCommandParams| async move {
                let timeout = Duration::from_secs(p.timeout_seconds.unwrap_or(20).clamp(1, 120));
                let started = std::time::Instant::now();
                let out = match run_shell(&p.command, &d.workspace, timeout).await {
                    Err(e) => return Ok(ToolResult::Expanded(ToolResultExpanded::new(format!("Could not run: {e:#}"), "failure"))),
                    Ok(out) => out,
                };
                if out.status.success() {
                    d.say_now(&p.say_on_success);
                }
                let stream = |label: &str, bytes: &[u8], max: usize| {
                    let s = String::from_utf8_lossy(bytes);
                    if s.trim().is_empty() { String::new() } else { format!("{label}: {}\n", s.trim().chars().take(max).collect::<String>()) }
                };
                let result = format!("exit {} in {} ms\n{}{}", out.status.code().unwrap_or(-1), started.elapsed().as_millis(), stream("stdout", &out.stdout, 4000), stream("stderr", &out.stderr, 2000));
                if out.status.success() {
                    text(result)
                } else {
                    Ok(ToolResult::Expanded(ToolResultExpanded::new(result, "failure")))
                }
            },
        ),
        tool(
            "focus_window",
            "Tell the user which window you are working in: the on-screen effect moves to that window. Call it when you start working in an app that you did not open or look at with another tool.",
            &deps,
            |d: Arc<ToolDeps>, p: FocusParams| async move {
                d.helper.focus(Some(&p.app), p.title.as_deref());
                text(format!("Focus moved to {}.", p.app))
            },
        ),
    ]
}

pub fn deep_task_tools(deps: Arc<ToolDeps>, deep: Arc<DeepRunner>) -> Vec<Tool> {
    type Ctx = (Arc<ToolDeps>, Arc<DeepRunner>);
    let ctx: Ctx = (deps, deep);
    vec![
        tool(
            "start_deep_task",
            "Hand a long, multi-step job to the deep worker and return immediately. Use for anything beyond a few quick steps: multi-page web flows, purchases, forms, research, larger code changes. Give a precise, self-contained goal with every detail the user mentioned.",
            &ctx,
            |(d, deep): Ctx, p: StartDeepParams| async move {
                let (delegation, context) = (d.current_delegation.lock().clone(), d.recent_conversation.lock().clone());
                match deep.start(p.goal, context, delegation).await {
                    Ok(what) => text(format!("{what}. It runs in the background; the user will hear the result when it finishes. Tell them it is underway in one sentence.")),
                    Err(e) => text(format!("Could not start: {e:#}")),
                }
            },
        ),
        tool(
            "deep_task_status",
            "Read the progress log of the longer task: goal, status, elapsed time, recent steps and the latest note. Read-only; it does not interrupt the work.",
            &ctx,
            |(_, deep): Ctx, _: NoParams| async move { text(deep.status_report()) },
        ),
        tool(
            "continue_deep_task",
            "Pass the user's answer, addition or change into the longer task, so the worker handles it with its full context. Works while the task is still running (it is queued behind the current step) and after it finished. Do not redo the work yourself.",
            &ctx,
            |(d, deep): Ctx, p: ContinueDeepParams| async move {
                let delegation = d.current_delegation.lock().clone();
                match deep.continue_with(p.message, delegation).await {
                    Ok(()) => text("The longer task continues in the background; the user will hear the result when it finishes. Tell them so in one sentence."),
                    Err(e) => text(format!("Could not continue: {e:#}")),
                }
            },
        ),
        tool("cancel_deep_task", "Stop the longer task that is currently running.", &ctx, |(_, deep): Ctx, _: NoParams| async move {
            match deep.cancel().await {
                Ok(true) => text("The longer task was cancelled."),
                Ok(false) => text("No longer task is running."),
                Err(e) => text(format!("Cancel failed: {e:#}")),
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shell_preserves_output_and_exit_status() {
        let output = run_shell("printf output; printf error >&2; exit 7", &std::env::temp_dir(), Duration::from_secs(5)).await.unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"output");
        assert_eq!(output.stderr, b"error");
    }

    #[tokio::test]
    async fn failed_command_returns_failure_without_speaking_success() {
        let (live, mut commands) = crate::live::client::handle();
        let deps = ToolDeps::new(HelperHandle::disabled(), live, std::env::temp_dir());
        let tools = overlay_tools(deps.clone());
        let tool = tools.iter().find(|tool| tool.name == "run_command").unwrap();
        let mut invocation = github_copilot_sdk::types::ToolInvocation::default();
        invocation.arguments = serde_json::json!({"command": "exit 7", "say_on_success": "Done."});
        let result = tool.handler().unwrap().call(invocation).await.unwrap();
        let ToolResult::Expanded(result) = result else { panic!("expected explicit failure"); };
        assert_eq!(result.result_type, "failure");
        assert!(result.text_result_for_llm.starts_with("exit 7"));
        assert!(!deps.spoke_early.load(Ordering::Relaxed));
        assert!(commands.try_recv().is_err());
    }

    #[tokio::test]
    async fn shell_timeout_and_cancellation_stop_its_children() {
        for cancel in [false, true] {
            let dir = std::env::temp_dir().join(format!("thursday-command-{}-{cancel}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("child-pid");
            let workspace = dir.clone();
            let timeout = if cancel { Duration::from_secs(30) } else { Duration::from_secs(1) };
            let task = tokio::spawn(async move {
                run_shell("sleep 30 & printf '%s' \"$!\" > child-pid; wait", &workspace, timeout).await
            });
            let pid = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(raw) = tokio::fs::read_to_string(&path).await {
                        if let Ok(pid) = raw.parse::<libc::pid_t>() { break pid; }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            if cancel {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert!(task.await.unwrap().unwrap_err().to_string().contains("Timed out"));
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if unsafe { libc::kill(pid, 0) } != 0 {
                        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("the command's child must not survive");
            std::fs::remove_file(path).unwrap();
            std::fs::remove_dir(dir).unwrap();
        }
    }

    #[test]
    fn annotations_keep_highlights_without_a_custom_note_tool() {
        let (live, _commands) = crate::live::client::handle();
        let deps = ToolDeps::new(HelperHandle::disabled(), live, PathBuf::from("/tmp"));
        let tools = overlay_tools(deps);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(!names.contains(&"add_note"));
        assert!(names.contains(&"highlight_window"));
        assert!(names.contains(&"clear_annotations"));
        let clear = tools.iter().find(|tool| tool.name == "clear_annotations").unwrap();
        assert!(clear.description.contains("Does not remove notes in macOS Stickies"));
    }
}
