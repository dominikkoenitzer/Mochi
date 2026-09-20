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

/// Environment variable that overrides the hotkey file path.
pub const HOTKEYS_ENV: &str = "MOCHI_HOTKEYS";

/// Where the hotkey file lives when nothing says otherwise, relative to the
/// user profile.
pub const HOTKEYS_PATH: [&str; 3] = [".config", "mochi", "hotkeys"];

/// The other file name [`resolve_hotkeys_path`] accepts, for a setup that
/// arrived from a standalone hotkey daemon and still uses its file name.
pub const HOTKEYS_LEGACY_FILE_NAME: &str = "whkdrc";

/// Editors save by writing a temporary file and renaming it, which produces a
/// burst of events. The watcher waits for this long of quiet before it reports
/// one change, so a burst costs one reload and the last write in it still wins.
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

/// Both names the hotkey file is accepted under, in the directory it lives in.
///
/// Empty when the path was given explicitly: a `--hotkeys` argument names one
/// file and nothing else counts.
pub fn hotkey_candidates(explicit: Option<&Path>) -> Result<Vec<PathBuf>> {
    if explicit.is_some() || std::env::var_os(HOTKEYS_ENV).is_some_and(|v| !v.is_empty()) {
        return Ok(Vec::new());
    }
    let directory = HOTKEYS_PATH
        .iter()
        .take(HOTKEYS_PATH.len() - 1)
        .fold(user_profile()?, |path, part| path.join(part));
    Ok(vec![
        directory.join(HOTKEYS_PATH[HOTKEYS_PATH.len() - 1]),
        directory.join(HOTKEYS_LEGACY_FILE_NAME),
    ])
}

/// Resolves the hotkey file path.
///
/// Precedence: the `--hotkeys` argument, then `MOCHI_HOTKEYS`, then
/// `%USERPROFILE%\.config\mochi\hotkeys`, then the same directory's `whkdrc`.
/// The last one is there so a desktop that came from a standalone hotkey daemon
/// keeps working the day Mochi takes the job over: the file is read where it
/// already lies, under the name it already has.
///
/// When neither file exists the first path is returned anyway, so the watcher
/// has something to wait on and the file starts working the moment it appears.
pub fn resolve_hotkeys_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(from_env) = std::env::var_os(HOTKEYS_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(from_env));
    }

    let directory = HOTKEYS_PATH
        .iter()
        .take(HOTKEYS_PATH.len() - 1)
        .fold(user_profile()?, |path, part| path.join(part));
    let preferred = directory.join(HOTKEYS_PATH[HOTKEYS_PATH.len() - 1]);
    if preferred.exists() {
        return Ok(preferred);
    }

    let legacy = directory.join(HOTKEYS_LEGACY_FILE_NAME);
    if legacy.exists() {
        return Ok(legacy);
    }
    Ok(preferred)
}

/// Reads and parses the hotkey file.
///
/// A missing file is not an error and not a warning: hotkeys are optional, and
/// plenty of setups drive Mochi from a separate hotkey daemon or from scripts.
/// A file that is there but has broken lines is loaded anyway, with every bad
/// line reported: one typo must not cost the user all fifty of their working
/// bindings. The errors come back so the daemon can log them and `mochic
/// hotkeys` can show them.
pub fn load_hotkeys(path: &Path) -> (mochi_hotkey::Bindings, Vec<String>) {
    if !path.exists() {
        tracing::info!(path = %path.display(), "no hotkey file, Mochi binds no keys");
        return (mochi_hotkey::Bindings::default(), Vec::new());
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            let message = format!("could not read {}: {e}", path.display());
            tracing::error!(message);
            return (mochi_hotkey::Bindings::default(), vec![message]);
        }
    };

    let (bindings, errors) = mochi_hotkey::Bindings::parse_lossy(&text);
    for error in &errors {
        tracing::error!(path = %path.display(), "{error}");
    }
    tracing::info!(
        path = %path.display(),
        bindings = bindings.len(),
        broken = errors.len(),
        "loaded the hotkeys"
    );
    (
        bindings,
        errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
    )
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
    write_if_absent(path, DEFAULT_CONFIG)
}

/// Writes the shipped hotkey file to `path` unless something is already there.
///
/// Returns true when a file was written.
pub fn write_default_hotkeys(path: &Path) -> Result<bool> {
    write_if_absent(path, mochi_hotkey::DEFAULT)
}

/// Creates the parent directory and writes `contents`, never clobbering.
fn write_if_absent(path: &Path, contents: &str) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(path, contents)
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
            // PowerShell's prefix is case insensitive, so `$ENV:` has to
            // work too. It did not, and the whole path came back unexpanded:
            // the application rule file was simply not found, and that is only
            // a warning, so 363 rules went missing without an error.
            let (tail, marker) = if tail
                .get(..4)
                .is_some_and(|head| head.eq_ignore_ascii_case("env:"))
            {
                (&tail[4..], 5)
            } else {
                (tail, 1)
            };
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

/// Watches a file and turns changes into [`Event::ConfigChanged`].
///
/// The *directory* is watched, not the file: a rename-into-place save would
/// otherwise take the watch with it and the second save would go unnoticed.
///
/// Between the watcher and the window manager sits a thread that collects the
/// burst one save produces and reports it once, after a quarter second of quiet.
/// Counting from the *first* event of a burst instead, and dropping the rest,
/// loses a save that lands just after one: the events that would have reported
/// it were already spent, and nothing comes along later to make up for it.
pub struct ConfigWatcher {
    /// Dropped before the thread is joined: the closure inside it holds the
    /// other sender, and the thread ends when every sender is gone.
    watcher: Option<RecommendedWatcher>,
    dirty: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl ConfigWatcher {
    /// Starts watching. A missing file is fine, the watch fires when it appears.
    pub fn start(path: PathBuf, tx: EventSender) -> Result<Self> {
        Self::start_watching(path, Vec::new(), tx)
    }

    /// Starts watching a file that may legitimately change its name.
    ///
    /// The hotkey file is read under either of two names, and renaming it from
    /// one to the other is a thing a user does exactly once: the day they stop
    /// carrying a file that was written for something else. Watching only the
    /// name that happened to exist at startup means that rename is the last
    /// event this watcher ever reports.
    pub fn start_any(path: PathBuf, others: Vec<PathBuf>, tx: EventSender) -> Result<Self> {
        Self::start_watching(path, others, tx)
    }

    fn start_watching(path: PathBuf, others: Vec<PathBuf>, tx: EventSender) -> Result<Self> {
        let directory = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut watched = vec![path.clone()];
        watched.extend(others);

        let (dirty_tx, dirty_rx) = std::sync::mpsc::channel::<()>();
        let reported = path.clone();
        let thread = std::thread::Builder::new()
            .name("mochi-file-watch".into())
            .spawn(move || settle(&dirty_rx, &tx, &reported))
            .context("could not spawn the file watcher thread")?;

        let notify_dirty = dirty_tx.clone();
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
                if !event
                    .paths
                    .iter()
                    .any(|p| watched.iter().any(|w| same_file(p, w)))
                {
                    return;
                }
                let _ = notify_dirty.send(());
            })
            .context("could not create a file watcher")?;

        watcher
            .watch(&directory, RecursiveMode::NonRecursive)
            .with_context(|| format!("could not watch {}", directory.display()))?;

        tracing::debug!(path = %path.display(), "watching the configuration");
        Ok(Self {
            watcher: Some(watcher),
            dirty: Some(dirty_tx),
            thread: Some(thread),
            path,
        })
    }

    /// The path being watched.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // The thread waits on the channel, so dropping every sender is what
        // tells it to stop. The watcher closure holds one, this struct the
        // other, and both have to go before the join can return.
        self.watcher = None;
        self.dirty = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the file watcher thread panicked");
        }
    }
}

/// Reports one change per burst, once the writes have stopped.
fn settle(dirty: &std::sync::mpsc::Receiver<()>, tx: &EventSender, path: &Path) {
    while dirty.recv().is_ok() {
        // Wait out the rest of the burst. A save that is still in progress
        // keeps extending the quiet period rather than starting a second one.
        while dirty.recv_timeout(DEBOUNCE).is_ok() {}
        if tx.send(Event::ConfigChanged(path.to_path_buf())).is_err() {
            return;
        }
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
    fn a_variable_name_that_is_not_ascii_does_not_panic() {
        // `$` then three ASCII bytes then a multi-byte character put the
        // fourth byte inside that character, and slicing there panics — at
        // daemon start and again on every config reload.
        for raw in ["$MY_ÖRDNER/x", "$abcä", "$USR😀", "$Zürich", "$"] {
            let _ = expand_env(raw);
        }
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
            // PowerShell does not care about the case of its own prefix, and
            // an unexpanded path just means the rule file is not found, which
            // is only a warning.
            "$ENV:USERPROFILE/applications.json",
            "$eNv:USERPROFILE/applications.json",
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

    #[test]
    fn two_saves_in_quick_succession_do_not_lose_the_second() {
        // The bug this covers: a debounce that counts from the first event of a
        // burst and drops the rest will drop a real save that lands just after
        // one, and nothing comes along later to report it. The desktop is then
        // running the file before last, with no sign that anything went wrong.
        let dir = std::env::temp_dir().join(format!("mochi-burst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mochi.json");
        std::fs::write(&path, "{}").unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = ConfigWatcher::start(path.clone(), tx).unwrap();

        std::fs::write(&path, "{\"first\":true}").unwrap();
        std::thread::sleep(DEBOUNCE / 5);
        std::fs::write(&path, "{\"second\":true}").unwrap();

        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the burst should be reported");
        // Whatever the watcher reported, the file it points at has to be the
        // one that was written last.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"second\":true}");

        // And a save after the burst is still noticed, rather than the thread
        // having gone away with it.
        std::fs::write(&path, "{\"third\":true}").unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("a later save should be reported too");

        drop(watcher);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_hotkey_path_wins() {
        let explicit = PathBuf::from(r"D:\somewhere\keys");
        assert_eq!(resolve_hotkeys_path(Some(&explicit)).unwrap(), explicit);
    }

    #[test]
    fn the_default_hotkey_path_sits_under_the_user_profile() {
        if std::env::var_os(HOTKEYS_ENV).is_some() {
            return;
        }
        let path = resolve_hotkeys_path(None).unwrap();
        let directory = path.parent().unwrap();
        assert!(path.starts_with(user_profile().unwrap()));
        assert_eq!(directory.file_name().unwrap(), "mochi");
        // Either the preferred name or the one a migrated setup still uses.
        let name = path.file_name().unwrap();
        assert!(
            name == HOTKEYS_PATH[2] || name == HOTKEYS_LEGACY_FILE_NAME,
            "unexpected hotkey file name: {name:?}"
        );
    }

    #[test]
    fn the_default_hotkeys_parse_and_bind_every_command_group() {
        let (bindings, errors) = mochi_hotkey::Bindings::parse_lossy(mochi_hotkey::DEFAULT);
        assert!(
            errors.is_empty(),
            "the shipped hotkeys do not parse: {errors:?}"
        );

        let bound = |text: &str| {
            bindings
                .get(text.parse().expect("the test spells its triggers right"))
                .map(|binding| binding.action.clone())
        };
        assert_eq!(
            bound("alt + shift + g"),
            Some(mochi_hotkey::Action::Command(
                mochi_client::Command::ToggleGameMode
            ))
        );
        assert_eq!(
            bound("alt + 3"),
            Some(mochi_hotkey::Action::Command(
                mochi_client::Command::FocusWorkspace { index: 2 }
            ))
        );
        assert!(bound("alt + shift + 9").is_some());
    }

    #[test]
    fn a_hotkey_file_with_one_bad_line_still_binds_the_rest() {
        let dir = std::env::temp_dir().join(format!("mochi-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hotkeys");
        std::fs::write(&path, "alt + h : focus left\nalt + nosuchkey : retile\n").unwrap();

        let (bindings, errors) = load_hotkeys(&path);
        assert_eq!(bindings.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("nosuchkey"),
            "unhelpful error: {errors:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_hotkey_file_is_not_an_error() {
        let (bindings, errors) = load_hotkeys(Path::new(r"D:\no\such\hotkeys"));
        assert!(bindings.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn the_shipped_hotkeys_are_written_once_and_never_clobbered() {
        let dir = std::env::temp_dir().join(format!("mochi-keys-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(".config").join("mochi").join("hotkeys");

        assert!(write_default_hotkeys(&path).unwrap());
        std::fs::write(&path, "alt + h : retile\n").unwrap();
        assert!(!write_default_hotkeys(&path).unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alt + h : retile\n"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
