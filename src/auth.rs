//! Live credentials. Azure CLI access tokens are acquired per connection and never persisted.

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio_tungstenite::tungstenite::http::HeaderValue;

const AZURE_TOKEN_TIMEOUT: Duration = Duration::from_secs(25);
const AZURE_RESOURCE: &str = "https://cognitiveservices.azure.com/";

#[derive(Clone)]
pub enum LiveAuth {
    OpenAi { api_key: String },
    AzureCli { subscription: Option<String>, tenant: Option<String> },
}

impl std::fmt::Debug for LiveAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenAi { api_key } => f.debug_struct("OpenAi").field("configured", &!api_key.is_empty()).finish(),
            Self::AzureCli { subscription, tenant } => f.debug_struct("AzureCli")
                .field("subscription", subscription).field("tenant", tenant).finish(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("No OpenAI API key configured. Set OPENAI_API_KEY or run: thursday-agent setup")]
    MissingKey,
    #[error("Invalid live credential: expected a nonempty bearer token without whitespace or control characters")]
    InvalidCredential,
    #[error("Azure CLI authentication could not run. Install Azure CLI and run: az login")]
    CliUnavailable,
    #[error("Azure CLI authentication failed. Run az login and check the configured subscription and tenant")]
    CliFailed,
    #[error("Azure CLI authentication timed out; the token subprocess was terminated")]
    CliTimeout,
    #[error("Azure CLI returned an invalid access-token response")]
    InvalidTokenResponse,
    #[error("Azure CLI token tenant does not match the configured tenant")]
    TenantMismatch,
}

impl LiveAuth {
    pub fn validate(&self) -> Result<(), AuthError> {
        if let Self::OpenAi { api_key } = self {
            if api_key.is_empty() {
                return Err(AuthError::MissingKey);
            }
            bearer_header(api_key)?;
        }
        Ok(())
    }

    pub async fn authorization_header(&self) -> Result<HeaderValue, AuthError> {
        self.validate()?;
        match self {
            Self::OpenAi { api_key } => bearer_header(api_key),
            Self::AzureCli { subscription, tenant } => {
                azure_header(
                    azure_command(subscription.as_deref(), tenant.as_deref()),
                    AZURE_TOKEN_TIMEOUT,
                    tenant.as_deref(),
                ).await
            }
        }
    }
}

fn bearer_header(token: &str) -> Result<HeaderValue, AuthError> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b)) {
        return Err(AuthError::InvalidCredential);
    }
    let mut header = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| AuthError::InvalidCredential)?;
    header.set_sensitive(true);
    Ok(header)
}

fn azure_command(subscription: Option<&str>, tenant: Option<&str>) -> Command {
    let mut command = Command::new("az");
    command.args(["account", "get-access-token", "--resource", AZURE_RESOURCE, "--output", "json", "--only-show-errors"]);
    if let Some(subscription) = subscription {
        command.args(["--subscription", subscription]);
    } else if let Some(tenant) = tenant {
        command.args(["--tenant", tenant]);
    }
    command
}

async fn azure_header(mut command: Command, timeout: Duration, expected_tenant: Option<&str>) -> Result<HeaderValue, AuthError> {
    // Dropping wait_with_output on timeout/cancellation kills the child. Neither stream is logged.
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let child = command.spawn().map_err(|_| AuthError::CliUnavailable)?;
    azure_child_header(child, timeout, expected_tenant).await
}

async fn azure_child_header(child: tokio::process::Child, timeout: Duration, expected_tenant: Option<&str>) -> Result<HeaderValue, AuthError> {
    let output = tokio::time::timeout(timeout, child.wait_with_output()).await
        .map_err(|_| AuthError::CliTimeout)?
        .map_err(|_| AuthError::CliFailed)?;
    if !output.status.success() {
        return Err(AuthError::CliFailed);
    }
    parse_azure_header(&output.stdout, expected_tenant)
}

fn parse_azure_header(bytes: &[u8], expected_tenant: Option<&str>) -> Result<HeaderValue, AuthError> {
    #[derive(serde::Deserialize)]
    struct TokenResponse {
        #[serde(rename = "accessToken")]
        access_token: String,
        #[serde(default)]
        tenant: Option<String>,
    }
    let token: TokenResponse = serde_json::from_slice(bytes).map_err(|_| AuthError::InvalidTokenResponse)?;
    if let Some(expected) = expected_tenant {
        let actual = token.tenant.as_deref().filter(|t| !t.is_empty()).ok_or(AuthError::InvalidTokenResponse)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(AuthError::TenantMismatch);
        }
    }
    bearer_header(&token.access_token).map_err(|_| AuthError::InvalidTokenResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_command_scopes_resource_with_mutually_exclusive_selectors() {
        let command = azure_command(Some("subscription"), Some("tenant"));
        let args: Vec<_> = command.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, ["account", "get-access-token", "--resource", AZURE_RESOURCE, "--output", "json", "--only-show-errors", "--subscription", "subscription"]);
        let subscription_only = azure_command(Some("subscription"), None);
        assert_eq!(subscription_only.as_std().get_args().collect::<Vec<_>>(), command.as_std().get_args().collect::<Vec<_>>());
        let tenant_only = azure_command(None, Some("tenant"));
        let args: Vec<_> = tenant_only.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, ["account", "get-access-token", "--resource", AZURE_RESOURCE, "--output", "json", "--only-show-errors", "--tenant", "tenant"]);
        let unscoped = azure_command(None, None);
        assert!(!unscoped.as_std().get_args().any(|a| a == "--subscription" || a == "--tenant"));
    }

    #[test]
    fn malformed_tokens_never_appear_in_errors() {
        for response in [
            r#"secret raw output"#, r#"{}"#, r#"{"accessToken":null}"#, r#"{"accessToken":42}"#,
            r#"{"accessToken":""}"#, r#"{"accessToken":"secret\r\nheader"}"#,
            r#"{"accessToken":"secret token"}"#, r#"{"accessToken":"ésecret"}"#,
        ] {
            let error = parse_azure_header(response.as_bytes(), None).unwrap_err();
            assert!(matches!(error, AuthError::InvalidTokenResponse));
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
    }

    #[tokio::test]
    async fn both_providers_build_sensitive_bearer_headers() {
        let auth = LiveAuth::OpenAi { api_key: "sk-local-fixture".into() };
        let openai = auth.authorization_header().await.unwrap();
        assert_eq!(openai.to_str().unwrap(), "Bearer sk-local-fixture");
        assert!(openai.is_sensitive());
        assert!(!format!("{auth:?}").contains("sk-local-fixture"));
        let azure = parse_azure_header(br#"{"accessToken":"local.azure.token","expires_on":9999999999}"#, None).unwrap();
        assert_eq!(azure.to_str().unwrap(), "Bearer local.azure.token");
        assert!(azure.is_sensitive());
    }

    #[tokio::test]
    async fn missing_and_invalid_openai_keys_fail_without_panicking() {
        assert!(matches!(LiveAuth::OpenAi { api_key: String::new() }.authorization_header().await, Err(AuthError::MissingKey)));
        assert!(matches!(LiveAuth::OpenAi { api_key: "secret\nkey".into() }.authorization_header().await, Err(AuthError::InvalidCredential)));
    }

    #[cfg(unix)]
    fn fixture(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_success_failure_and_malformed_output() {
        let header = azure_header(fixture("printf '%s' '{\"accessToken\":\"fixture-token\"}'"), Duration::from_secs(2), None).await.unwrap();
        assert_eq!(header.to_str().unwrap(), "Bearer fixture-token");
        for (script, expected) in [
            ("printf 'secret stdout'; printf 'secret stderr' >&2; exit 1", "failed"),
            ("printf 'secret malformed response'", "invalid access-token"),
        ] {
            let error = azure_header(fixture(script), Duration::from_secs(2), None).await.unwrap_err().to_string();
            assert!(error.contains(expected));
            assert!(!error.contains("secret"));
        }
        assert!(matches!(azure_header(Command::new("/nonexistent/thursday-az-fixture"), Duration::from_secs(2), None).await, Err(AuthError::CliUnavailable)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn token_subprocess_times_out_and_is_killed() {
        let child = fixture("exec sleep 30").stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
        let pid = child.id().unwrap().to_string();
        let error = azure_child_header(child, Duration::from_millis(100), None).await.unwrap_err();
        assert!(matches!(error, AuthError::CliTimeout));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !Command::new("/bin/kill").args(["-0", &pid]).stderr(Stdio::null()).status().await.unwrap().success() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("timed-out process must be killed and reaped");
    }

    #[test]
    fn configured_tenant_must_match_token_response() {
        let response = br#"{"accessToken":"secret-token","tenant":"tenant-a"}"#;
        assert!(parse_azure_header(response, Some("tenant-a")).is_ok());
        assert!(parse_azure_header(response, Some("TENANT-A")).is_ok());
        assert!(parse_azure_header(response, None).is_ok());
        let error = parse_azure_header(response, Some("other-tenant")).unwrap_err();
        assert!(matches!(error, AuthError::TenantMismatch));
        assert!(!format!("{error:?} {error}").contains("secret"));
        for response in [
            r#"{"accessToken":"secret-token"}"#,
            r#"{"accessToken":"secret-token","tenant":null}"#,
            r#"{"accessToken":"secret-token","tenant":""}"#,
            r#"{"accessToken":"secret-token","tenant":42}"#,
        ] {
            let error = parse_azure_header(response.as_bytes(), Some("expected-tenant")).unwrap_err();
            assert!(matches!(error, AuthError::InvalidTokenResponse));
            assert!(!format!("{error:?} {error}").contains("secret"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_response_tenant_is_verified_before_using_token() {
        let script = "printf '%s' '{\"accessToken\":\"secret-token\",\"tenant\":\"expected-tenant\"}'";
        assert!(azure_header(fixture(script), Duration::from_secs(2), Some("expected-tenant")).await.is_ok());
        let error = azure_header(fixture(script), Duration::from_secs(2), Some("other-tenant")).await.unwrap_err();
        assert!(matches!(error, AuthError::TenantMismatch));
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
}
