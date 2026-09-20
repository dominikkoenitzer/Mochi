//! Tracing to stderr and to a daily log file.
//!
//! Console output follows `RUST_LOG` and defaults to `info`. The file always
//! gets at least `debug`, because the interesting question after a bad tile is
//! always "which event fired just before".

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Directory under `%LOCALAPPDATA%` that holds the log files.
pub const LOG_DIR_NAME: &str = "mochi";
/// Base name of the log file. `tracing-appender` appends the date.
pub const LOG_FILE_NAME: &str = "mochi.log";

/// Keeps the background log writer alive. Drop it last, on the way out.
pub struct LogGuard {
    _worker: tracing_appender::non_blocking::WorkerGuard,
    path: PathBuf,
}

impl LogGuard {
    /// The directory the rolling log files are written to.
    pub fn directory(&self) -> &std::path::Path {
        &self.path
    }
}

/// `%LOCALAPPDATA%\mochi`, created if it is not there yet.
pub fn log_directory() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("LOCALAPPDATA is not set, so there is nowhere to write logs")?;
    let dir = base.join(LOG_DIR_NAME);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    Ok(dir)
}

/// Installs the global subscriber.
///
/// Returns a guard that must stay alive for as long as anything logs.
pub fn init() -> Result<LogGuard> {
    let directory = log_directory()?;
    // Capped. A plain daily roll keeps every file for ever: a week of ordinary
    // use is tens of megabytes across a growing pile of them, and the thirty
    // lines that matter after something goes wrong are a rounding error inside
    // it. A fortnight is long enough to explain a problem someone noticed a
    // few days late.
    let appender = tracing_appender::rolling::Builder::new()
        .filename_prefix(LOG_FILE_NAME)
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .max_log_files(14)
        .build(&directory)
        .with_context(|| format!("could not open a log file in {}", directory.display()))?;
    let (writer, worker) = tracing_appender::non_blocking(appender);

    let console_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_filter = EnvFilter::try_from_env("MOCHI_LOG").unwrap_or_else(|_| {
        EnvFilter::new("debug,notify=info,notify_debouncer_full=info,polling=info")
    });

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(console_filter),
        )
        .with(
            fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true)
                .with_filter(file_filter),
        )
        .try_init()
        .map_err(|e| anyhow::anyhow!("could not install the tracing subscriber: {e}"))?;

    Ok(LogGuard {
        _worker: worker,
        path: directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_directory_is_under_local_appdata() {
        let dir = log_directory().unwrap();
        assert!(dir.ends_with(LOG_DIR_NAME));
        assert!(dir.is_dir());
    }
}
