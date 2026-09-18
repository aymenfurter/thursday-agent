//! `thursday-agent doctor`: check live auth, Copilot login, MCP tools, helper and audio.

use anyhow::Result;

use crate::config::Config;
use crate::{audio, copilot, helper, logging};

pub async fn run(cfg: Config, no_helper: bool) -> Result<()> {
    println!("thursday-agent doctor\n");
    println!("Log file: {}", logging::log_path().display());

    let auth_kind = match cfg.live_provider {
        crate::config::Provider::Azure => "Azure CLI bearer token",
        crate::config::Provider::Openai => "OpenAI API key",
    };
    println!("     live provider: {} ({auth_kind})", cfg.live_provider);
    println!("     live endpoint: {}  model/deployment: {}", cfg.live_url, cfg.live_model);
    match cfg.live_auth.authorization_header().await {
        Ok(_) => println!("[ok] live credentials available (use `thursday-agent voice-check` to test the endpoint)"),
        Err(e) => println!("[!!] live auth: {e}"),
    }
    println!("     user: {}  workspace: {}", cfg.user_name, cfg.workspace.display());
    println!("     fast model: {} ({})  deep model: {} ({})", cfg.quick_model, cfg.fast_reasoning, cfg.deep_model, cfg.reasoning_effort);

    match copilot::start_client(&cfg).await {
        Ok(client) => match client.get_auth_status().await {
            Ok(st) if st.is_authenticated => {
                println!(
                    "[ok] copilot: signed in as {} ({})",
                    st.login.unwrap_or_default(),
                    st.auth_type.unwrap_or_default()
                );
                match client.list_models().await {
                    Ok(models) => {
                        let mut names: Vec<String> = models
                            .iter()
                            .filter_map(|m| serde_json::to_value(m).ok())
                            .map(|v| {
                                let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                                let vision = v
                                    .pointer("/capabilities/supports/vision")
                                    .and_then(|x| x.as_bool())
                                    .unwrap_or(false);
                                if vision { format!("{id} (vision)") } else { id }
                            })
                            .collect();
                        names.sort();
                        println!("     models: {}", names.join(", "));
                    }
                    Err(e) => println!("[..] could not list models: {e}"),
                }
            }
            Ok(st) => println!(
                "[!!] copilot: not signed in ({}). Run: copilot login",
                st.status_message.unwrap_or_default()
            ),
            Err(e) => println!("[!!] copilot auth status failed: {e}"),
        },
        Err(e) => println!("[!!] copilot runtime failed to start: {e:#}"),
    }

    let pb = &cfg.peekaboo_command;
    let out = tokio::process::Command::new(&pb[0])
        .args(&pb[1..])
        .args(["permissions", "--json"])
        .output()
        .await;
    match out {
        Ok(o) => {
            let txt = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&txt) {
                Ok(v) => {
                    let perms = v.pointer("/data/permissions").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                    for p in perms {
                        let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("?");
                        let granted = p.get("isGranted").and_then(|x| x.as_bool()).unwrap_or(false);
                        let how = p.get("grantInstructions").and_then(|x| x.as_str()).unwrap_or("");
                        println!("[{}] peekaboo {name}{}", if granted { "ok" } else { "!!" }, if granted { String::new() } else { format!(": {how}") });
                    }
                }
                Err(_) => println!("[!!] peekaboo permissions: unexpected output: {}", txt.trim()),
            }
        }
        Err(e) => println!("[!!] peekaboo not runnable ({}): {e}", pb.join(" ")),
    }

    if no_helper {
        println!("[..] helper skipped (--no-helper)");
    } else {
        match helper::find_binary() {
            Some(p) => match helper::spawn().await {
                Ok(h) => {
                    println!("[ok] helper: {}", p.display());
                    h.handle.highlight("frontmost", None, 3.0);
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    h.handle.quit();
                }
                Err(e) => println!("[!!] helper failed: {e:#}"),
            },
            None => println!("[!!] helper not built. Run: scripts/build-helper.sh"),
        }
    }

    match tokio::task::spawn_blocking(audio::start).await? {
        Ok(_a) => println!("[ok] audio: microphone and speakers opened"),
        Err(e) => println!("[!!] audio: {e:#}"),
    }

    println!("\nIf everything is [ok], run: thursday-agent");
    Ok(())
}
