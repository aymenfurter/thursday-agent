//! Permission policy for the Copilot session. There is no screen to click
//! "allow" on, so the policy is: anything read-only or inside the workspace is
//! fine, destructive shell commands and writes elsewhere are rejected with a
//! hint that makes the voice ask the user first.

use async_trait::async_trait;
use github_copilot_sdk::handler::{PermissionHandler, PermissionResult};
use github_copilot_sdk::types::{PermissionRequestData, PermissionRequestKind, RequestId, SessionId};
use std::path::{Path, PathBuf};

pub struct Policy {
    workspace: PathBuf,
}

impl Policy {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace: workspace.canonicalize().unwrap_or(workspace),
        }
    }
}

const DENY_SHELL: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "rm -rf $home",
    "sudo ",
    "diskutil erase",
    "mkfs",
    "dd if=",
    "shutdown",
    "reboot",
    "launchctl unload",
    "killall ",
    "git push --force",
    "git push -f",
    "git reset --hard",
    ":(){",
    "> /dev/",
    "chmod -r 777 /",
];

pub fn shell_is_dangerous(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    DENY_SHELL.iter().any(|p| c.contains(p))
}

pub fn path_in_workspace(workspace: &Path, file: &str) -> bool {
    let p = Path::new(file);
    if p.is_relative() {
        return !file.starts_with("..");
    }
    let canon = p
        .canonicalize()
        .or_else(|_| p.parent().map(|d| d.canonicalize().map(|c| c.join(p.file_name().unwrap_or_default()))).unwrap_or_else(|| Ok(p.to_path_buf())))
        .unwrap_or_else(|_| p.to_path_buf());
    canon.starts_with(workspace)
}

#[async_trait]
impl PermissionHandler for Policy {
    async fn handle(
        &self,
        _session_id: SessionId,
        _request_id: RequestId,
        data: PermissionRequestData,
    ) -> PermissionResult {
        if data.managed_approval_required == Some(true) {
            return PermissionResult::no_result();
        }
        let extra = &data.extra;
        let str_field = |k: &str| extra.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());
        let decision = match data.kind {
            Some(PermissionRequestKind::Read)
            | Some(PermissionRequestKind::Url)
            | Some(PermissionRequestKind::Mcp)
            | Some(PermissionRequestKind::CustomTool)
            | Some(PermissionRequestKind::Memory)
            | Some(PermissionRequestKind::Hook) => PermissionResult::approve_once(),
            Some(PermissionRequestKind::Write) => {
                let file = str_field("fileName").or_else(|| str_field("path")).unwrap_or_default();
                if file.is_empty() || path_in_workspace(&self.workspace, &file) {
                    PermissionResult::approve_once()
                } else {
                    PermissionResult::reject(Some(format!(
                        "Writing to {file} is outside the workspace {}. Use tell_user to ask the user out loud first, and only retry after they explicitly confirm.",
                        self.workspace.display()
                    )))
                }
            }
            Some(PermissionRequestKind::Shell) => {
                let cmd = str_field("fullCommandText")
                    .or_else(|| str_field("command"))
                    .unwrap_or_default();
                if shell_is_dangerous(&cmd) {
                    PermissionResult::reject(Some(
                        "That command looks destructive. Ask the user out loud (tell_user) and only proceed after they explicitly confirm.".to_string(),
                    ))
                } else {
                    PermissionResult::approve_once()
                }
            }
            _ => PermissionResult::reject(Some(
                "This kind of action needs the user's spoken confirmation. Ask them via tell_user first.".to_string(),
            )),
        };
        tracing::info!(kind = ?data.kind, decision = ?matches!(decision, PermissionResult::Decision { .. }), "permission");
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_shell_detection() {
        assert!(shell_is_dangerous("sudo rm -rf /tmp/x"));
        assert!(shell_is_dangerous("git push --force origin main"));
        assert!(!shell_is_dangerous("cargo test"));
        assert!(!shell_is_dangerous("rm -rf target"));
    }

    #[test]
    fn workspace_paths() {
        let ws = std::env::temp_dir().canonicalize().unwrap();
        assert!(path_in_workspace(&ws, "src/main.rs"));
        assert!(!path_in_workspace(&ws, "../etc/passwd"));
        assert!(path_in_workspace(&ws, ws.join("new-file.txt").to_str().unwrap()));
        assert!(!path_in_workspace(&ws, "/etc/hosts"));
    }
}
