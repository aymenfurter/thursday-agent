//! File-only logging. Nothing is printed to the terminal while running,
//! because the whole point of the app is that there is nothing to look at.

use anyhow::Result;
use std::io::IsTerminal;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn log_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join("Library")
        .join("Logs")
        .join("thursday-agent.log")
}

/// Initialise tracing to a rolling file. Returns the guard that must be kept alive.
pub fn init(to_stderr: bool) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let path = log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("THURSDAY_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = fmt::layer().with_writer(writer).with_ansi(false).with_target(true);
    let registry = tracing_subscriber::registry().with(filter).with(file_layer);
    if to_stderr {
        registry.with(fmt::layer().with_writer(std::io::stderr).with_ansi(std::io::stderr().is_terminal())).init();
    } else {
        registry.init();
    }
    Ok(guard)
}
