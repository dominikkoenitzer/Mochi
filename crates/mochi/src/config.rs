//! Where the configuration lives, how it is read and how Mochi notices that it
//! changed.
//!
//! The schema itself belongs to `mochi-core`: this module resolves the path,
//! reads the two files ([`Loaded`]), writes the quickstart stub and watches for
//! changes. Applying a [`mochi_core::config::Config`] to the model is
//! [`crate::wm::WindowManager::reload_config`]'s job.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mochi_core::config::Config;
use mochi_core::rules::{RuleSets, load_app_specific_configuration};
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

/// Everything one configuration load produced.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// The parsed `mochi.json`. [`Config::default`] when there is no file.
    pub config: Config,
    /// The rules from `app_specific_configuration_path`, empty when there is none.
    pub app_rules: RuleSets,
    /// The community rule file that was read, for the log and for `state`.
    pub app_path: Option<PathBuf>,
    /// `false` when no configuration file existed and the defaults were used.
    pub present: bool,
}

/// Reads `mochi.json` and, if it names one, the community rule file next to it.
///
/// A missing `mochi.json` is not an error: Mochi has working defaults, and
/// refusing to start because a file is absent would be the wrong trade for a
/// window manager. A file that *is* there but does not parse is an error, so a
/// typo is loud instead of silently reverting the desktop to the defaults.
///
/// # Errors
///
/// When the file exists and cannot be read or parsed. A broken community rule
/// file is only logged, because it is not Mochi's file.
pub fn load(path: &Path) -> Result<Loaded> {
    if !path.exists() {
        tracing::info!(path = %path.display(), "no configuration file, using the defaults");
        return Ok(Loaded::default());
    }

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let config =
        Config::from_json(&text).with_context(|| format!("could not parse {}", path.display()))?;

    let mut loaded = Loaded {
        app_rules: RuleSets::new(),
        app_path: None,
        present: true,
        config,
    };

    if let Some(raw) = loaded.config.app_specific_configuration_path.clone() {
        let app_path = PathBuf::from(expand_env(&raw.to_string_lossy()));
        match std::fs::read_to_string(&app_path) {
            Ok(text) => match load_app_specific_configuration(&text) {
                Ok(rules) => {
                    tracing::info!(
                        path = %app_path.display(),
                        rules = rules.len(),
                        "loaded the application rules"
                    );
                    loaded.app_rules = rules;
                }
                Err(e) => tracing::warn!(
                    path = %app_path.display(),
                    error = %e,
                    "the application rules could not be parsed, ignoring them"
                ),
            },
            Err(e) => tracing::warn!(
                path = %app_path.display(),
                error = %e,
                "the application rules could not be read, ignoring them"
            ),
        }
        loaded.app_path = Some(app_path);
    }

    Ok(loaded)
}

/// Expands the three environment variable spellings a migrated config can carry.
///
/// `$Env:USERPROFILE` is what a config written next to a PowerShell setup uses,
/// `%USERPROFILE%` is what the rest of Windows uses, and `$USERPROFILE` turns up
/// in files that travelled through a shell script. An unset variable is left as
/// it was, so the failure shows up as a path in the log rather than as a
/// mysteriously empty one.
#[must_use]
pub fn expand_env(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;

    while let Some(start) = rest.find(['$', '%']) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];

        let (name, consumed) = if let Some(tail) = rest.strip_prefix('%') {
            match tail.find('%') {
                // `%NAME%`: the marker, the name and the closing marker.
                Some(end) => (&tail[..end], end + 2),
                None => {
                    out.push('%');
                    rest = tail;
                    continue;
                }
            }
        } else {
            let tail = &rest[1..];
            let (tail, marker) = tail
                .strip_prefix("Env:")
                .or_else(|| tail.strip_prefix("env:"))
                .map_or((tail, 1), |t| (t, 5));
            let len = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(tail.len());
            (&tail[..len], marker + len)
        };

        match std::env::var_os(name).filter(|v| !v.is_empty()) {
            Some(value) if !name.is_empty() => out.push_str(&value.to_string_lossy()),
            _ => out.push_str(&rest[..consumed]),
        }
        rest = &rest[consumed..];
    }

    out.push_str(rest);
    out
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
    fn a_missing_file_loads_the_defaults_instead_of_failing() {
        let loaded = load(Path::new(r"C:\nowhere\at\all\mochi.json")).unwrap();
        assert!(!loaded.present);
        assert!(loaded.app_rules.is_empty());
        assert_eq!(loaded.config, Config::default());
    }

    #[test]
    fn the_real_config_of_this_machine_loads_with_its_rules() {
        let dir = std::env::temp_dir().join(format!("mochi-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let apps = dir.join("applications.json");
        std::fs::write(
            &apps,
            r#"{ "$schema": "x", "Zoom": { "ignore": [{ "kind": "Exe", "id": "Zoom.exe" }] } }"#,
        )
        .unwrap();

        let path = dir.join("mochi.json");
        std::fs::write(
            &path,
            format!(
                r#"{{
                  "app_specific_configuration_path": "{}",
                  "window_hiding_behaviour": "Cloak",
                  "cross_monitor_move_behaviour": "Insert",
                  "default_workspace_padding": 14,
                  "default_container_padding": 10,
                  "monitors": [{{ "workspaces": [{{ "name": "1", "layout": "BSP" }}] }}]
                }}"#,
                apps.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();

        let loaded = load(&path).unwrap();
        assert!(loaded.present);
        assert_eq!(loaded.config.default_workspace_padding, Some(14));
        assert_eq!(loaded.config.default_container_padding, Some(10));
        assert_eq!(loaded.app_path.as_deref(), Some(apps.as_path()));
        assert_eq!(loaded.app_rules.ignore_rules.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_an_error_rather_than_a_silent_reset() {
        let dir = std::env::temp_dir().join(format!("mochi-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mochi.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_environment_variable_spelling_expands() {
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        if profile.is_empty() {
            return;
        }
        for raw in [
            "$Env:USERPROFILE/applications.json",
            "$env:USERPROFILE/applications.json",
            "%USERPROFILE%/applications.json",
            "$USERPROFILE/applications.json",
        ] {
            assert_eq!(
                expand_env(raw),
                format!("{profile}/applications.json"),
                "for {raw}"
            );
        }
    }

    #[test]
    fn an_unset_variable_and_a_lone_marker_are_left_alone() {
        assert_eq!(
            expand_env("$Env:MOCHI_NOT_SET_ANYWHERE/x"),
            "$Env:MOCHI_NOT_SET_ANYWHERE/x"
        );
        assert_eq!(
            expand_env("%MOCHI_NOT_SET_ANYWHERE%"),
            "%MOCHI_NOT_SET_ANYWHERE%"
        );
        assert_eq!(expand_env("100% done"), "100% done");
        assert_eq!(
            expand_env(r"C:\Users\x\mochi.json"),
            r"C:\Users\x\mochi.json"
        );
        assert_eq!(expand_env(""), "");
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
