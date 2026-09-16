//! Where the configuration lives and how Mochi notices that it changed.
//!
//! Parsing is deliberately not here. `mochi-core` owns the schema; this module
//! only resolves the path, writes the quickstart stub and watches the file.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::events::{Event, EventSender};

/// File name Mochi looks for in the user profile.
pub const CONFIG_FILE_NAME: &str = "mochi.json";

/// Environment variable that overrides the configuration path.
pub const CONFIG_ENV: &str = "MOCHI_CONFIG";

/// Editors save by writing a temporary file and renaming it, which produces a
/// burst of events. Anything inside this window after the first one is dropped.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// The stub `mochic quickstart` writes.
///
/// Intentionally minimal: once `mochi-core` owns the schema, the real defaults
/// and the `$schema` link belong here.
pub const DEFAULT_CONFIG: &str = r#"{
  "window_hiding_behaviour": "Cloak",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "monitors": []
}
"#;

/// Resolves the configuration path.
///
/// Precedence: the `--config` argument, then `MOCHI_CONFIG`, then
/// `%USERPROFILE%\mochi.json`.
pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(from_env) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(from_env));
    }
    Ok(user_profile()?.join(CONFIG_FILE_NAME))
}

/// The user profile directory, the home of `mochi.json`.
pub fn user_profile() -> Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("USERPROFILE is not set, so the configuration path cannot be resolved")
}

/// Writes [`DEFAULT_CONFIG`] to `path` unless something is already there.
///
/// Returns true when a file was written.
pub fn write_default(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(path, DEFAULT_CONFIG)
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(true)
}

/// Watches the configuration file and turns changes into [`Event::ConfigChanged`].
///
/// The *directory* is watched, not the file: a rename-into-place save would
/// otherwise take the watch with it and the second save would go unnoticed.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    path: PathBuf,
}

impl ConfigWatcher {
    /// Starts watching. A missing file is fine, the watch fires when it appears.
    pub fn start(path: PathBuf, tx: EventSender) -> Result<Self> {
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let watched = path.clone();
        let mut last = std::time::Instant::now() - DEBOUNCE;

        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else {
                    return;
                };
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                if !event.paths.iter().any(|p| same_file(p, &watched)) {
                    return;
                }
                if last.elapsed() < DEBOUNCE {
                    return;
                }
                last = std::time::Instant::now();
                let _ = tx.send(Event::ConfigChanged(watched.clone()));
            })
            .context("could not create a file watcher")?;

        watcher
            .watch(&directory, RecursiveMode::NonRecursive)
            .with_context(|| format!("could not watch {}", directory.display()))?;

        tracing::debug!(path = %path.display(), "watching the configuration");
        Ok(Self {
            _watcher: watcher,
            path,
        })
    }

    /// The path being watched.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Compares two paths by file name and, when possible, by canonical form.
///
/// `notify` reports paths as the platform hands them over, which on Windows may
/// differ from what the user typed in case and in short-name form.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_wins() {
        let explicit = PathBuf::from(r"D:\somewhere\else.json");
        assert_eq!(resolve_path(Some(&explicit)).unwrap(), explicit);
    }

    #[test]
    fn the_default_is_mochi_json_in_the_user_profile() {
        // Only meaningful when the environment variable is not set, which is
        // the normal case for a developer machine.
        if std::env::var_os(CONFIG_ENV).is_some() {
            return;
        }
        let path = resolve_path(None).unwrap();
        assert_eq!(path.file_name().unwrap(), CONFIG_FILE_NAME);
        assert_eq!(path.parent().unwrap(), user_profile().unwrap());
    }

    #[test]
    fn the_stub_is_valid_json() {
        let value: serde_json::Value = serde_json::from_str(DEFAULT_CONFIG).unwrap();
        assert!(value.is_object());
        assert!(value.get("monitors").is_some());
    }

    #[test]
    fn write_default_never_clobbers() {
        let dir = std::env::temp_dir().join(format!("mochi-cfg-{}", std::process::id()));
        let path = dir.join("mochi.json");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(write_default(&path).unwrap(), "first write should happen");
        std::fs::write(&path, "{\"mine\":true}").unwrap();
        assert!(
            !write_default(&path).unwrap(),
            "second write must be skipped"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"mine\":true}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_matching_ignores_case_and_directory() {
        assert!(same_file(
            Path::new(r"C:\Users\x\MOCHI.JSON"),
            Path::new(r"C:\Users\x\mochi.json")
        ));
        assert!(!same_file(
            Path::new(r"C:\Users\x\other.json"),
            Path::new(r"C:\Users\x\mochi.json")
        ));
    }

    #[test]
    fn a_change_on_disk_reaches_the_channel() {
        let dir = std::env::temp_dir().join(format!("mochi-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mochi.json");
        std::fs::write(&path, "{}").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = ConfigWatcher::start(path.clone(), tx).unwrap();
        assert_eq!(watcher.path(), path);

        std::fs::write(&path, "{\"changed\":true}").unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("a write should be noticed");
        match event {
            Event::ConfigChanged(p) => assert_eq!(p, path),
            other => panic!("unexpected event: {other:?}"),
        }

        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
