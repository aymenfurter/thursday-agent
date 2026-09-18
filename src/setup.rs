//! `thursday-agent setup`: select a live provider and store preferences, never Azure tokens.

use anyhow::Result;

use crate::auth::LiveAuth;
use crate::config::{self, LiveSelection, Overrides, Provider};

pub fn run(cli: &Overrides) -> Result<()> {
    println!("thursday-agent setup. Values are stored in {} (mode 600).", config::config_path().display());
    println!("Leave a field empty to keep the current value.\n");
    let mut file = config::load_file()?;

    let (current_provider, _) = config::live_target(cli, &file, config::env);
    let provider = match ask(&format!("Voice provider: azure or openai [{current_provider}]"))?.as_str() {
        "" => current_provider,
        "azure" => Provider::Azure,
        "openai" => Provider::Openai,
        _ => anyhow::bail!("Voice provider must be azure or openai"),
    };
    let mut overrides = Overrides { provider: Some(provider), ..cli.clone() };
    let (_, current_endpoint) = config::live_target(&overrides, &file, config::env);
    let endpoint = ask_or_keep("Live endpoint", current_endpoint.as_deref().unwrap_or(""))?;
    config::live_url(provider, &endpoint)?;
    overrides.endpoint = Some(endpoint.clone());
    let live = config::resolve_live(&overrides, &file, config::env)?;
    let model_label = if provider == Provider::Azure { "Azure deployment name" } else { "OpenAI live model" };
    let model = ask_or_keep(model_label, &live.model)?;
    let (subscription, tenant) = match live.auth {
        LiveAuth::AzureCli { subscription, tenant } => {
            println!("Azure uses your Azure CLI login. No Azure API key is needed or saved.");
            let subscription = ask_scope("Azure subscription", subscription)?;
            let tenant = ask_scope("Azure tenant", tenant)?;
            (subscription, tenant)
        }
        LiveAuth::OpenAi { .. } => {
            let key = rpassword::prompt_password("OpenAI API key (sk-..., empty to keep): ")?;
            if !key.trim().is_empty() {
                file.openai_api_key = Some(key.trim().to_string());
            }
            (None, None)
        }
    };
    file.live_selection = Some(LiveSelection {
        provider, model, endpoint: Some(endpoint), subscription, tenant,
    });
    config::resolve_live(&Overrides::default(), &file, |_| None)?;
    let name = ask("Your first name (for greetings)")?;
    if !name.is_empty() {
        file.user_name = Some(name);
    }
    let ws = ask("Default workspace folder (optional)")?;
    if !ws.is_empty() {
        file.workspace = Some(crate::text::resolve_path(&std::env::current_dir()?, &ws));
    }
    let quick = ask("Fast Copilot model (optional, default gpt-5.6-luna)")?;
    if !quick.is_empty() {
        file.quick_model = Some(quick);
    }
    let deep = ask("Deep Copilot model (optional, default gpt-6-astra)")?;
    if !deep.is_empty() {
        file.deep_model = Some(deep);
    }
    let path = config::save_file(&file)?;
    println!("\nSaved {}.", path.display());
    if provider == Provider::Azure {
        println!("Next: make sure `az login` is done for the selected tenant and subscription.");
    }
    println!("Run `thursday-agent voice-check` to test the live endpoint, then `copilot login` and `thursday-agent doctor`.");
    Ok(())
}

fn ask_or_keep(label: &str, current: &str) -> Result<String> {
    let value = ask(&format!("{label} [{current}]"))?;
    Ok(if value.is_empty() { current.to_string() } else { value })
}

fn ask_scope(label: &str, current: Option<String>) -> Result<Option<String>> {
    let value = ask(&format!("{label} [{}; '-' to use the Azure CLI account]", current.as_deref().unwrap_or("Azure CLI account")))?;
    Ok(match value.as_str() {
        "" => current,
        "-" => None,
        _ => Some(value),
    })
}

fn ask(label: &str) -> Result<String> {
    use std::io::Write;
    print!("{label}: ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
