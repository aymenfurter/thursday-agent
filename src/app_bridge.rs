//! Authenticated loopback connection to the canvas-owning Copilot App session.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Bridge {
    url: String,
    token: String,
}

fn validate_bridge(bridge: &Bridge) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&bridge.url).context("invalid Copilot App bridge URL")?;
    ensure!(
        url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
            && url.port().is_some() && url.username().is_empty() && url.password().is_none()
            && url.query().is_none() && url.fragment().is_none() && url.path() == "/",
        "Copilot App bridge must be an explicit loopback HTTP address"
    );
    ensure!(!bridge.token.is_empty(), "Copilot App bridge token is missing");
    Ok(url)
}

async fn response_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body: Value = response.json().await.context("invalid Copilot App bridge response")?;
    ensure!(status.is_success(), "Copilot App bridge: {}",
        body.get("error").and_then(Value::as_str).unwrap_or("request failed"));
    Ok(body)
}

pub async fn ask(prompt: &str) -> Result<String> {
    ensure!(!prompt.trim().is_empty() && prompt.len() <= 16_000, "Prompt must contain 1 to 16000 bytes");
    let path = std::env::var("THURSDAY_APP_BRIDGE")
        .context("Open the thursday-agent canvas and start thursday-agent there to connect to Copilot App")?;
    let bytes = tokio::fs::read(path).await.context("reading Copilot App bridge descriptor")?;
    let bridge: Bridge = serde_json::from_slice(&bytes).context("invalid Copilot App bridge descriptor")?;
    ask_connected(prompt, &bridge).await
}

async fn ask_connected(prompt: &str, bridge: &Bridge) -> Result<String> {
    let url = validate_bridge(bridge)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let request = client.post(url.join("prompt")?)
        .bearer_auth(&bridge.token)
        .json(&json!({"prompt": prompt}))
        .send().await.context("sending prompt to Copilot App; is its canvas still open?")?;
    let mut result = response_json(request).await?;
    let id = result.get("id").and_then(Value::as_str)
        .context("Copilot App returned no request ID")?.to_string();
    ensure!(id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-'), "Invalid request ID");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match result.get("status").and_then(Value::as_str) {
            Some("completed") => return Ok(result.get("response").and_then(Value::as_str)
                .context("Copilot App returned no response")?.to_string()),
            Some("failed") => anyhow::bail!("{}", result.get("error").and_then(Value::as_str)
                .unwrap_or("Copilot App request failed")),
            Some("queued" | "running") => {}
            _ => anyhow::bail!("Copilot App returned an invalid request status"),
        }
        ensure!(Instant::now() < deadline,
            "Copilot App request {id} is still queued or running. Do not resend it; inspect the canvas for its result.");
        tokio::time::sleep(Duration::from_millis(750)).await;
        result = response_json(client.get(url.join(&format!("requests/{id}"))?)
            .bearer_auth(&bridge.token).send().await.context("reading Copilot App response")?).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_accepts_only_loopback_without_credentials_or_redirects() {
        for url in ["https://example.com/", "http://localhost:1234/", "http://127.0.0.1/",
            "http://user@127.0.0.1:1234/", "http://127.0.0.1:1234/?secret=1"] {
            assert!(validate_bridge(&Bridge { url: url.into(), token: "test".into() }).is_err());
        }
        assert!(validate_bridge(&Bridge {
            url: "http://127.0.0.1:1234/".into(), token: "test".into(),
        }).is_ok());
    }

    #[tokio::test]
    async fn bridge_posts_prompt_then_returns_only_its_completed_answer() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (route, body) in [
                ("POST /prompt ", r#"{"id":"abc","status":"queued"}"#),
                ("GET /requests/abc ", r#"{"id":"abc","status":"completed","response":"One session is running."}"#),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let size = socket.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&chunk[..size]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") { break; }
                }
                let header_end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap() + 4;
                let header_text = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                let content_length = header_text.lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map(|value| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
                while request.len() < header_end + content_length {
                    let size = socket.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&chunk[..size]);
                }
                if route.starts_with("POST") {
                    let body: Value = serde_json::from_slice(&request[header_end..]).unwrap();
                    assert_eq!(body["prompt"], "List sessions");
                }
                let headers = String::from_utf8(request).unwrap();
                assert!(headers.starts_with(route));
                assert!(headers.to_ascii_lowercase().contains("authorization: bearer test"));
                socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                ).as_bytes()).await.unwrap();
            }
        });
        let answer = tokio::time::timeout(Duration::from_secs(5), ask_connected("List sessions", &Bridge {
            url: format!("http://{address}/"), token: "test".into(),
        })).await.unwrap().unwrap();
        assert_eq!(answer, "One session is running.");
        server.await.unwrap();
    }
}
