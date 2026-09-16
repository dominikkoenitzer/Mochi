//! Surviving a crash with every window still reachable.
//!
//! Taking a window off screen is the one thing Mochi does that outlives the
//! process: a cloaked or hidden window stays that way when the daemon is
//! killed outright, and the user has no way to bring it back. So the record of
//! what is off screen is mirrored to a small file, and the next start puts
//! those windows back before it does anything else.
//!
//! Handles are reused by Windows, so an entry alone is not trusted: the window
//! has to still exist and still be the same process and class.

use std::path::{Path, PathBuf};

use mochi_core::model::HidingBehaviour;
use serde::{Deserialize, Serialize};

use crate::platform::{Hwnd, Platform, ShowState};

/// One window that was off screen when the file was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The handle, as a plain number.
    pub hwnd: isize,
    /// The process that owns the window, to catch handle reuse.
    pub pid: u32,
    /// The window class, to catch handle reuse within one process.
    pub class: String,
    /// How the window was taken off screen.
    pub behaviour: HidingBehaviour,
    /// Whether Mochi also made the window translucent.
    pub faded: bool,
}

/// Where the record lives by default, next to the log files.
pub fn default_path() -> PathBuf {
    crate::logging::log_directory().map_or_else(
        |_| std::env::temp_dir().join("mochi-off-screen.json"),
        |dir| dir.join("off-screen.json"),
    )
}

/// Writes the record, replacing whatever was there.
///
/// Called on every change to the hidden set, so it stays small and cheap: a
/// handful of entries, written whole.
pub fn save(path: &Path, entries: &[Entry]) {
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_vec_pretty(entries) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(path, bytes) {
                tracing::debug!(error = %e, "could not write the off screen record");
            }
        }
        Err(e) => tracing::debug!(error = %e, "could not serialise the off screen record"),
    }
}

/// Reads the record. A missing or unreadable file simply means nothing to do.
pub fn load(path: &Path) -> Vec<Entry> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    match serde_json::from_slice::<Vec<Entry>>(&bytes) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(error = %e, "the off screen record is unreadable, ignoring it");
            Vec::new()
        }
    }
}

/// Puts back everything a previous session left off screen, then forgets it.
///
/// Returns the handles it brought back.
pub fn recover(platform: &dyn Platform, path: &Path) -> Vec<Hwnd> {
    let entries = load(path);
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return Vec::new();
    }
    let mut back = Vec::new();
    for entry in entries {
        let hwnd = Hwnd(entry.hwnd);
        let Ok(info) = platform.window_info(hwnd) else {
            continue;
        };
        if info.pid != entry.pid || info.class != entry.class {
            tracing::debug!(%hwnd, "the handle belongs to another window now, leaving it alone");
            continue;
        }
        tracing::warn!(
            %hwnd,
            title = %info.title,
            "putting back a window a previous session left off screen"
        );
        let result = match entry.behaviour {
            HidingBehaviour::Cloak => platform.set_cloaked(hwnd, false),
            HidingBehaviour::Minimize => platform.show(hwnd, ShowState::Restore),
            HidingBehaviour::Hide => platform.show(hwnd, ShowState::ShowNoActivate),
        };
        match result {
            Ok(()) => back.push(hwnd),
            Err(e) => tracing::error!(%hwnd, error = %e, "could not put the window back"),
        }
        if entry.faded
            && let Err(e) = platform.set_transparency(hwnd, None)
        {
            tracing::debug!(%hwnd, error = %e, "could not clear the alpha");
        }
    }
    let _ = std::fs::remove_file(path);
    back
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(hwnd: isize, behaviour: HidingBehaviour) -> Entry {
        Entry {
            hwnd,
            pid: 42,
            class: "Test".to_owned(),
            behaviour,
            faded: false,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("mochi-recover-{name}.json"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn a_record_round_trips() {
        let path = temp("round-trip");
        let entries = vec![
            entry(1, HidingBehaviour::Cloak),
            entry(2, HidingBehaviour::Hide),
        ];
        save(&path, &entries);
        assert_eq!(load(&path), entries);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_nothing_removes_the_file() {
        let path = temp("empty");
        save(&path, &[entry(1, HidingBehaviour::Cloak)]);
        assert!(path.exists());
        save(&path, &[]);
        assert!(!path.exists());
    }

    #[test]
    fn a_broken_record_is_ignored() {
        let path = temp("broken");
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_record_is_not_an_error() {
        assert!(load(&temp("missing")).is_empty());
    }
}
