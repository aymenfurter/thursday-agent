//! Screenshots for the backend model. Uses the system `screencapture` tool so
//! no extra permissions beyond Screen Recording are needed.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use github_copilot_sdk::types::Attachment;
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Clone)]
pub struct Shot {
    pub jpeg_base64: String,
}

fn tmp_path() -> PathBuf {
    let dir = std::env::temp_dir().join("thursday");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("shot-{}.jpg", chrono::Utc::now().format("%Y%m%d-%H%M%S%3f")))
}

/// Capture the main display, downscale to `max_px` on the long edge, return JPEG.
pub async fn capture(max_px: u32) -> Result<Shot> {
    let path = tmp_path();
    let status = Command::new("screencapture")
        .args(["-x", "-t", "jpg"])
        .arg(&path)
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .context("running screencapture")?;
    if !status.success() {
        return Err(anyhow!("screencapture exited with {status}"));
    }
    let status = Command::new("sips")
        .args(["-Z", &max_px.to_string(), "-s", "formatOptions", "50"])
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .status()
        .await
        .context("running sips")?;
    if !status.success() {
        tracing::warn!("sips resize failed; sending full-size screenshot");
    }
    let bytes = tokio::fs::read(&path).await?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(Shot {
        jpeg_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

impl Shot {
    pub fn attachment(self) -> Attachment {
        Attachment::Blob {
            data: self.jpeg_base64,
            mime_type: "image/jpeg".into(),
            display_name: Some("screen.jpg".into()),
        }
    }
}

/// Best-effort cleanup of old screenshots.
pub fn sweep() {
    let dir = std::env::temp_dir().join("thursday");
    if let Ok(entries) = std::fs::read_dir(dir) {
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for e in entries.flatten() {
            if let Ok(meta) = e.metadata() {
                if meta.modified().map(|m| m < cutoff).unwrap_or(false) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}
