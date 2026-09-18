//! Configuration: explicit CLI options, saved live selection, then environment defaults.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio_tungstenite::tungstenite::http::Uri;

use crate::auth::LiveAuth;

pub const DEFAULT_VOICE: &str = "marin";
/// Fast model: quick commands, quick looks at the screen, and progress check-ins.
pub const DEFAULT_QUICK_MODEL: &str = "gpt-5.6-luna";
/// Deep model: long, multi-step tasks started on demand by the fast one.
pub const DEFAULT_DEEP_MODEL: &str = "gpt-6-astra";
pub const DEFAULT_REASONING: &str = "low";
/// Luna answers noticeably faster without reasoning; it accepts none/low/medium/high.
pub const DEFAULT_FAST_REASONING: &str = "none";
pub const LIVE_MODEL: &str = "gpt-live-1";
pub const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Azure,
    Openai,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Azure => "azure",
            Self::Openai => "openai",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSelection {
    pub provider: Provider,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_endpoint: Option<String>,
    /// Retained for old config files; Azure authentication now uses Azure CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_selection: Option<LiveSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Command used to launch the Peekaboo MCP server. Defaults to npx.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peekaboo_command: Option<Vec<String>>,
}

/// Values given on the command line; they win over environment and file.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub workspace: Option<PathBuf>,
    pub quick_model: Option<String>,
    pub deep_model: Option<String>,
    pub reasoning: Option<String>,
    pub fast_reasoning: Option<String>,
    pub provider: Option<Provider>,
    pub endpoint: Option<String>,
    pub live_model: Option<String>,
    pub azure_subscription: Option<String>,
    pub azure_tenant: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub provider: Provider,
    pub url: String,
    pub model: String,
    pub auth: LiveAuth,
}

/// Fully resolved runtime configuration. Credentials are never included in Debug output.
#[derive(Debug, Clone)]
pub struct Config {
    pub live_provider: Provider,
    pub live_auth: LiveAuth,
    pub live_url: String,
    pub live_model: String,
    pub user_name: String,
    pub workspace: PathBuf,
    pub quick_model: String,
    pub deep_model: String,
    /// Reasoning effort of the deep model.
    pub reasoning_effort: String,
    /// Reasoning effort of the fast model (lower is snappier).
    pub fast_reasoning: String,
    pub voice: String,
    pub peekaboo_command: Vec<String>,
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("THURSDAY_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
        .join("thursday")
        .join("config.json")
}

pub fn load_file() -> Result<FileConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(FileConfig::default());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save_file(cfg: &FileConfig) -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(cfg)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

fn account_first_name() -> Option<String> {
    let out = std::process::Command::new("id").arg("-F").output().ok()?;
    let full = String::from_utf8_lossy(&out.stdout).trim().to_string();
    full.split_whitespace().next().filter(|w| w.len() > 1).map(str::to_string)
}

pub(crate) fn env(key: &str) -> Option<String> {
    nonempty(std::env::var(key).ok())
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn setting(value: String, name: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) || value.starts_with('-') {
        bail!("{name} must be nonempty and must not contain control characters or start with '-'");
    }
    Ok(value.to_string())
}

/// Setup can ask for a missing endpoint before resolving the rest of the live configuration.
pub fn live_target(
    cli: &Overrides,
    file: &FileConfig,
    environment: impl Fn(&str) -> Option<String>,
) -> (Provider, Option<String>) {
    let azure_endpoint = nonempty(environment("AZURE_OPENAI_ENDPOINT"))
        .or_else(|| nonempty(file.azure_endpoint.clone()));
    let provider = cli.provider
        .or_else(|| file.live_selection.as_ref().map(|s| s.provider))
        .unwrap_or_else(|| {
            if azure_endpoint.is_some() { Provider::Azure } else { Provider::Openai }
        });
    // Switching provider explicitly must not reuse another provider's saved endpoint or deployment.
    let selection = file.live_selection.as_ref().filter(|s| s.provider == provider);
    let endpoint = cli.endpoint.clone().or_else(|| match selection {
        Some(s) => s.endpoint.clone(),
        None if provider == Provider::Azure => azure_endpoint,
        None => None,
    }).or_else(|| (provider == Provider::Openai).then(|| DEFAULT_OPENAI_ENDPOINT.to_string()));
    (provider, endpoint)
}

/// Pure live resolver: tests provide their own environment instead of mutating process globals.
pub fn resolve_live(
    cli: &Overrides,
    file: &FileConfig,
    environment: impl Fn(&str) -> Option<String>,
) -> Result<LiveConfig> {
    let get = |name| nonempty(environment(name));
    let openai_key = get("OPENAI_API_KEY").or_else(|| nonempty(file.openai_api_key.clone()));
    let (provider, endpoint) = live_target(cli, file, &environment);
    let endpoint = endpoint.context("No Azure endpoint configured. Select a resource in the canvas, run thursday-agent setup, or set --endpoint / AZURE_OPENAI_ENDPOINT")?;
    let selection = file.live_selection.as_ref().filter(|s| s.provider == provider);
    let model = cli.live_model.clone()
        .or_else(|| selection.map(|s| s.model.clone()))
        .or_else(|| get("THURSDAY_LIVE_MODEL"))
        .or_else(|| (provider == Provider::Azure).then(|| get("AZURE_OPENAI_DEPLOYMENT")).flatten())
        .unwrap_or_else(|| LIVE_MODEL.to_string());
    let auth = match provider {
        Provider::Openai => LiveAuth::OpenAi { api_key: openai_key.unwrap_or_default() },
        Provider::Azure => {
            let scope = |cli: &Option<String>, selected: Option<String>, var| {
                cli.clone().or_else(|| {
                    if selection.is_some() { selected } else { get(var) }
                })
            };
            let subscription = scope(
                &cli.azure_subscription, selection.and_then(|s| s.subscription.clone()),
                "AZURE_OPENAI_SUBSCRIPTION",
            ).map(|v| setting(v, "Azure subscription")).transpose()?;
            let tenant = scope(
                &cli.azure_tenant, selection.and_then(|s| s.tenant.clone()),
                "AZURE_OPENAI_TENANT",
            ).map(|v| setting(v, "Azure tenant")).transpose()?;
            LiveAuth::AzureCli { subscription, tenant }
        }
    };
    Ok(LiveConfig {
        provider,
        url: live_url(provider, &endpoint)?,
        model: setting(model, "Live model/deployment")?,
        auth,
    })
}

/// Credentials stay optional here so Copilot-only commands work without an OpenAI key.
pub fn resolve(cli: &Overrides) -> Result<Config> {
    let file = load_file()?;
    let live = resolve_live(cli, &file, env)?;
    let pick = |cli: &Option<String>, var: &str, file: Option<String>, default: &str| {
        cli.clone().or_else(|| env(var)).or(file).unwrap_or_else(|| default.to_string())
    };
    Ok(Config {
        live_provider: live.provider,
        live_auth: live.auth,
        live_url: live.url,
        live_model: live.model,
        user_name: env("THURSDAY_USER").or(file.user_name).or_else(account_first_name).or_else(|| env("USER")).unwrap_or_else(|| "there".into()),
        workspace: cli.workspace.clone()
            .or_else(|| env("THURSDAY_WORKSPACE").map(PathBuf::from))
            .or(file.workspace)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        quick_model: pick(&cli.quick_model, "THURSDAY_QUICK_MODEL", file.quick_model, DEFAULT_QUICK_MODEL),
        deep_model: pick(&cli.deep_model, "THURSDAY_DEEP_MODEL", file.deep_model, DEFAULT_DEEP_MODEL),
        reasoning_effort: pick(&cli.reasoning, "THURSDAY_REASONING", None, DEFAULT_REASONING),
        fast_reasoning: pick(&cli.fast_reasoning, "THURSDAY_FAST_REASONING", None, DEFAULT_FAST_REASONING),
        voice: pick(&None, "THURSDAY_VOICE", file.voice, DEFAULT_VOICE),
        peekaboo_command: peekaboo_command(file.peekaboo_command)?,
    })
}

fn peekaboo_command(command: Option<Vec<String>>) -> Result<Vec<String>> {
    let command = command.unwrap_or_else(|| ["npx", "-y", "@steipete/peekaboo"].map(String::from).to_vec());
    if command.first().is_none_or(|program| program.trim().is_empty()) {
        bail!("peekaboo_command must contain a nonempty executable followed by optional arguments");
    }
    Ok(command)
}

/// Accept resource roots or full live URLs, but never insecure URLs or embedded credentials.
pub fn live_url(provider: Provider, endpoint: &str) -> Result<String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() || endpoint.chars().any(char::is_whitespace) || endpoint.contains(['#', '?', '@', '\\']) {
        bail!("Live endpoint must be a secure resource URL without credentials, query, or fragment");
    }
    let normalized = if endpoint.contains("://") { endpoint.to_string() } else { format!("https://{endpoint}") };
    let uri: Uri = normalized.parse().map_err(|_| anyhow::anyhow!("Invalid live endpoint URL"))?;
    if !matches!(uri.scheme_str(), Some("https" | "wss")) || uri.host().is_none_or(str::is_empty) {
        bail!("Live endpoint must use https:// or wss:// and include a host");
    }
    let authority = uri.authority().context("Live endpoint is missing a host")?;
    if !authority.as_str().ends_with(']') {
        if let Some((_, port)) = authority.as_str().rsplit_once(':') {
            if port.parse::<u16>().is_err() {
                bail!("Invalid live endpoint port");
            }
        }
    }
    let path = match provider {
        Provider::Azure => "/openai/v1/live/sessions",
        Provider::Openai => "/v1/live/sessions",
    };
    if !matches!(uri.path(), "" | "/") && uri.path().trim_end_matches('/') != path {
        bail!("Live endpoint must be a resource root or end in {path}");
    }
    Ok(format!("wss://{authority}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_peekaboo_commands_fail_configuration_instead_of_panicking() {
        for command in [vec![], vec!["".into()], vec![" ".into(), "mcp".into()]] {
            assert!(peekaboo_command(Some(command)).is_err());
        }
        assert_eq!(peekaboo_command(None).unwrap(), ["npx", "-y", "@steipete/peekaboo"]);
        let custom = vec!["/path with spaces/peekaboo".into(), "--flag".into()];
        assert_eq!(peekaboo_command(Some(custom.clone())).unwrap(), custom);
    }

    fn resolve_with(cli: &Overrides, file: &FileConfig, vars: &[(&str, &str)]) -> LiveConfig {
        resolve_live(cli, file, |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string())).unwrap()
    }

    fn selected(provider: Provider) -> FileConfig {
        FileConfig {
            live_selection: Some(LiveSelection {
                provider, model: "saved-deployment".into(),
                endpoint: (provider == Provider::Azure).then(|| "https://saved.azure.com".into()),
                subscription: None, tenant: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn azure_urls_and_endpoint_validation() {
        for endpoint in ["https://my-res.openai.azure.com/", "my-res.openai.azure.com/openai/v1/live/sessions", "wss://my-res.openai.azure.com/openai/v1/live/sessions"] {
            assert_eq!(live_url(Provider::Azure, endpoint).unwrap(), "wss://my-res.openai.azure.com/openai/v1/live/sessions");
        }
        for endpoint in ["", "http://host", "ws://host", "https://", "https://user:secret@host", "https://host?key=secret", "https://host/#fragment", "https://host/other", "https://host:70000", "https://host:bad", "https://host:", "https://bad host", "https://host\\evil"] {
            assert!(live_url(Provider::Azure, endpoint).is_err(), "{endpoint}");
        }
        assert_eq!(live_url(Provider::Openai, DEFAULT_OPENAI_ENDPOINT).unwrap(), "wss://api.openai.com/v1/live/sessions");
        assert_eq!(live_url(Provider::Azure, "https://host:8443").unwrap(), "wss://host:8443/openai/v1/live/sessions");
        assert_eq!(live_url(Provider::Azure, "https://[::1]").unwrap(), "wss://[::1]/openai/v1/live/sessions");
    }

    #[test]
    fn azure_endpoint_wins_over_openai_key_without_azure_key() {
        let live = resolve_with(&Overrides::default(), &FileConfig::default(), &[("AZURE_OPENAI_ENDPOINT", "https://env.azure.com"), ("OPENAI_API_KEY", "key")]);
        assert_eq!(live.provider, Provider::Azure);
        assert!(matches!(live.auth, LiveAuth::AzureCli { .. }));
        assert_eq!(live.url, "wss://env.azure.com/openai/v1/live/sessions");
    }

    #[test]
    fn explicit_openai_wins_over_azure_environment_and_selection() {
        let live = resolve_with(&Overrides { provider: Some(Provider::Openai), ..Default::default() }, &selected(Provider::Azure), &[
            ("AZURE_OPENAI_ENDPOINT", "https://env.azure.com"), ("AZURE_OPENAI_DEPLOYMENT", "azure-only"),
            ("OPENAI_API_KEY", "openai-key"),
        ]);
        assert_eq!(live.provider, Provider::Openai);
        assert_eq!(live.url, "wss://api.openai.com/v1/live/sessions");
        assert_eq!(live.model, LIVE_MODEL);
        assert!(matches!(live.auth, LiveAuth::OpenAi { api_key } if api_key == "openai-key"));
    }

    #[test]
    fn saved_selection_wins_over_environment_including_missing_optional_fields() {
        for provider in [Provider::Azure, Provider::Openai] {
            let live = resolve_with(&Overrides::default(), &selected(provider), &[
                ("AZURE_OPENAI_ENDPOINT", "https://env.azure.com"), ("THURSDAY_LIVE_MODEL", "env-model"),
                ("AZURE_OPENAI_DEPLOYMENT", "env-deployment"), ("AZURE_OPENAI_SUBSCRIPTION", "env-sub"),
                ("AZURE_OPENAI_TENANT", "env-tenant"), ("OPENAI_API_KEY", "key"),
            ]);
            assert_eq!(live.provider, provider);
            assert_eq!(live.model, "saved-deployment");
            assert!(!live.url.contains("env.azure.com"));
            if let LiveAuth::AzureCli { subscription, tenant } = live.auth {
                assert!(subscription.is_none());
                assert!(tenant.is_none());
            }
        }
    }

    #[test]
    fn cli_overrides_all_saved_live_values() {
        let mut file = selected(Provider::Azure);
        let selection = file.live_selection.as_mut().unwrap();
        selection.endpoint = Some("https://saved.azure.com".into());
        selection.subscription = Some("saved-sub".into());
        selection.tenant = Some("saved-tenant".into());
        let cli = Overrides {
            endpoint: Some("https://cli.azure.com".into()), live_model: Some("cli-deployment".into()),
            azure_subscription: Some("cli-sub".into()), azure_tenant: Some("cli-tenant".into()),
            ..Default::default()
        };
        let live = resolve_with(&cli, &file, &[]);
        assert_eq!(live.url, "wss://cli.azure.com/openai/v1/live/sessions");
        assert_eq!(live.model, "cli-deployment");
        assert!(matches!(live.auth, LiveAuth::AzureCli { subscription: Some(s), tenant: Some(t) } if s == "cli-sub" && t == "cli-tenant"));
    }

    #[test]
    fn unconfigured_live_has_no_azure_resource_or_credentials() {
        let default = resolve_with(&Overrides::default(), &FileConfig::default(), &[]);
        assert_eq!(default.provider, Provider::Openai);
        assert_eq!(default.model, LIVE_MODEL);
        assert_eq!(default.url, "wss://api.openai.com/v1/live/sessions");
        assert!(matches!(default.auth, LiveAuth::OpenAi { api_key } if api_key.is_empty()));
    }

    #[test]
    fn azure_requires_an_explicit_endpoint_and_has_no_default_account_scope() {
        let cli = Overrides { provider: Some(Provider::Azure), ..Default::default() };
        for key in [None, Some("openai-key")] {
            let error = resolve_live(&cli, &FileConfig::default(), |name| {
                (name == "OPENAI_API_KEY").then(|| key.map(str::to_string)).flatten()
            }).unwrap_err();
            assert!(error.to_string().contains("No Azure endpoint configured"));
        }
        let live = resolve_with(&Overrides {
            endpoint: Some("https://resource.azure.com".into()), ..cli
        }, &FileConfig::default(), &[]);
        assert!(matches!(live.auth, LiveAuth::AzureCli { subscription: None, tenant: None }));
    }

    #[test]
    fn missing_saved_azure_endpoint_does_not_use_environment_or_openai() {
        let mut file = selected(Provider::Azure);
        file.live_selection.as_mut().unwrap().endpoint = None;
        let error = resolve_live(&Overrides::default(), &file, |name| match name {
            "AZURE_OPENAI_ENDPOINT" => Some("https://environment.azure.com".into()),
            "OPENAI_API_KEY" => Some("openai-key".into()),
            _ => None,
        }).unwrap_err();
        assert!(error.to_string().contains("No Azure endpoint configured"));
    }

    #[test]
    fn setup_can_select_azure_before_an_endpoint_is_configured() {
        let mut cli = Overrides { provider: Some(Provider::Azure), ..Default::default() };
        let file = selected(Provider::Openai);
        assert_eq!(live_target(&cli, &file, |_| None), (Provider::Azure, None));
        cli.endpoint = Some("https://resource.azure.com".into());
        let live = resolve_with(&cli, &file, &[]);
        assert_eq!(live.provider, Provider::Azure);
        assert_eq!(live.model, LIVE_MODEL);
        assert_eq!(live.url, "wss://resource.azure.com/openai/v1/live/sessions");
    }

    #[test]
    fn azure_environment_model_selection() {
        let cli = Overrides {
            provider: Some(Provider::Azure), endpoint: Some("https://resource.azure.com".into()),
            ..Default::default()
        };
        for (vars, model) in [
            (vec![("AZURE_OPENAI_DEPLOYMENT", "deployment")], "deployment"),
            (vec![("AZURE_OPENAI_DEPLOYMENT", "deployment"), ("THURSDAY_LIVE_MODEL", "live")], "live"),
        ] {
            assert_eq!(resolve_with(&cli, &FileConfig::default(), &vars).model, model);
        }
    }

    #[test]
    fn old_config_is_preserved_and_openai_still_works() {
        let file: FileConfig = serde_json::from_str(r#"{"openai_api_key":"old-key","azure_api_key":"legacy-key","user_name":"Ada","voice":"marin"}"#).unwrap();
        let live = resolve_with(&Overrides::default(), &file, &[]);
        assert_eq!(live.provider, Provider::Openai);
        assert_eq!(live.model, LIVE_MODEL);
        let saved = serde_json::to_value(file).unwrap();
        assert_eq!(saved["azure_api_key"], "legacy-key");
        assert_eq!(saved["user_name"], "Ada");
        assert!(saved.get("live_selection").is_none());
    }

    #[test]
    fn rejects_unknown_provider_or_empty_saved_model() {
        assert!(serde_json::from_str::<FileConfig>(r#"{"live_selection":{"provider":"other","model":"gpt-live-1"}}"#).is_err());
        let mut file = selected(Provider::Azure);
        file.live_selection.as_mut().unwrap().model.clear();
        assert!(resolve_live(&Overrides::default(), &file, |_| None).is_err());
    }

    #[test]
    fn saved_azure_values_override_environment_and_legacy_values() {
        let mut file = selected(Provider::Azure);
        file.azure_endpoint = Some("https://legacy.azure.com".into());
        let selection = file.live_selection.as_mut().unwrap();
        selection.endpoint = Some("https://saved.azure.com".into());
        selection.subscription = Some("saved-sub".into());
        selection.tenant = Some("saved-tenant".into());
        let live = resolve_with(&Overrides::default(), &file, &[
            ("AZURE_OPENAI_ENDPOINT", "https://env.azure.com"), ("OPENAI_API_KEY", "key"),
            ("THURSDAY_LIVE_MODEL", "env-model"), ("AZURE_OPENAI_DEPLOYMENT", "env-deployment"),
            ("AZURE_OPENAI_SUBSCRIPTION", "env-sub"), ("AZURE_OPENAI_TENANT", "env-tenant"),
        ]);
        assert_eq!(live.provider, Provider::Azure);
        assert_eq!(live.url, "wss://saved.azure.com/openai/v1/live/sessions");
        assert_eq!(live.model, "saved-deployment");
        assert!(matches!(live.auth, LiveAuth::AzureCli { subscription: Some(s), tenant: Some(t) } if s == "saved-sub" && t == "saved-tenant"));
    }

    #[test]
    fn environment_scopes_and_openai_model_are_used_without_a_selection() {
        let live = resolve_with(&Overrides::default(), &FileConfig::default(), &[
            ("AZURE_OPENAI_ENDPOINT", "https://resource.azure.com"),
            ("AZURE_OPENAI_SUBSCRIPTION", "env-sub"), ("AZURE_OPENAI_TENANT", "env-tenant"),
        ]);
        assert!(matches!(live.auth, LiveAuth::AzureCli { subscription: Some(s), tenant: Some(t) } if s == "env-sub" && t == "env-tenant"));
        let live = resolve_with(&Overrides::default(), &FileConfig::default(), &[
            ("OPENAI_API_KEY", "key"), ("THURSDAY_LIVE_MODEL", "openai-model"),
        ]);
        assert_eq!(live.provider, Provider::Openai);
        assert_eq!(live.model, "openai-model");
    }

    #[test]
    fn invalid_selected_azure_never_falls_back_to_openai() {
        let mut file = selected(Provider::Azure);
        file.live_selection.as_mut().unwrap().endpoint = Some("http://insecure.azure.com".into());
        file.openai_api_key = Some("valid-openai-key".into());
        assert!(resolve_live(&Overrides::default(), &file, |_| None).is_err());
    }
}
