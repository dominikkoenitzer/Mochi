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
    /// How the window was taken off screen, or `None` for a window Mochi
    /// only faded: that one is still on screen and needs its alpha cleared,
    /// not showing.
    ///
    /// Defaulted so a record written before this field could be missing still
    /// parses. An entry with no behaviour in an old file cannot happen, so the
    /// default costs nothing there.
    #[serde(default)]
    pub behaviour: Option<HidingBehaviour>,
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
///
/// Written to a temporary file and renamed into place, because this file is
/// the only thing that knows where the user's windows went. A plain write
/// truncates first, so a `taskkill` or a power loss during one of them leaves a
/// half record on disk, and half a record is worth nothing: it does not parse,
/// and the windows it should have named stay invisible. `ReplaceFile` semantics
/// on NTFS make the rename atomic, so the file on disk is always one whole
/// record, either the old one or the new one.
pub fn save(path: &Path, entries: &[Entry]) {
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let bytes = match serde_json::to_vec_pretty(entries) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!(error = %e, "could not serialise the off screen record");
            return;
        }
    };

    let temporary = path.with_extension("json.new");
    if let Err(e) = std::fs::write(&temporary, bytes) {
        tracing::error!(error = %e, "could not write the off screen record");
        return;
    }
    if let Err(e) = std::fs::rename(&temporary, path) {
        tracing::error!(error = %e, "could not replace the off screen record");
        let _ = std::fs::remove_file(&temporary);
    }
}

/// Reads the record.
///
/// `Some(entries)` for a file that parsed, including an empty one. `None` when
/// there is a file and it could not be read, which is not the same thing as
/// "nothing to do": it means windows may be off screen and this file was the
/// only way to know which. The caller must not delete a record it could not
/// read, because deleting it destroys the only evidence left.
pub fn load(path: &Path) -> Option<Vec<Entry>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(e) => {
            tracing::error!(error = %e, path = %path.display(), "the off screen record could not be read");
            return None;
        }
    };
    match serde_json::from_slice::<Vec<Entry>>(&bytes) {
        Ok(entries) => Some(entries),
        Err(e) => {
            tracing::error!(
                error = %e,
                path = %path.display(),
                "the off screen record is damaged and is being kept, not deleted:                  a window a previous session hid may still be off screen"
            );
            None
        }
    }
}

/// Whether a recorded entry still names the window its handle points at.
///
/// Windows reuses handle values, so a record from a dead session can name a
/// window that belongs to somebody else now; restoring that one would show a
/// window the user had deliberately minimised, or worse.
///
/// An entry with no identity at all is the exception. It was written before its
/// window could be identified, and comparing it can only ever fail, so matching
/// it strictly would strand precisely the window the record exists for. A
/// window that is off screen with no name attached is put back.
fn still_the_same_window(entry: &Entry, info: &crate::platform::WindowInfo) -> bool {
    let anonymous = entry.pid == 0 && entry.class.is_empty();
    anonymous || (info.pid == entry.pid && info.class == entry.class)
}

/// Puts back everything a previous session left off screen, then forgets it.
///
/// Returns the handles it brought back.
pub fn recover(platform: &dyn Platform, path: &Path) -> Vec<Hwnd> {
    let Some(entries) = load(path) else {
        // Unreadable. It stays on disk: the next save overwrites it, and until
        // then it is the only sign that something may be hidden.
        return Vec::new();
    };
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return Vec::new();
    }
    let mut back = Vec::new();
    let mut unfinished = Vec::new();
    for entry in entries {
        let hwnd = Hwnd(entry.hwnd);
        let Ok(info) = platform.window_info(hwnd) else {
            // The window may simply be gone, or this may be a transient
            // failure. Either way the entry is kept: dropping it here is how a
            // window gets stranded, and an entry for a window that no longer
            // exists costs one failed call on the next start.
            unfinished.push(entry);
            continue;
        };
        if !still_the_same_window(&entry, &info) {
            tracing::debug!(%hwnd, "the handle belongs to another window now, leaving it alone");
            continue;
        }
        if entry.behaviour.is_some() {
            tracing::warn!(
                %hwnd,
                title = %info.title,
                "putting back a window a previous session left off screen"
            );
        } else {
            tracing::warn!(
                %hwnd,
                title = %info.title,
                "clearing the alpha a previous session left on a window"
            );
        }
        let result = match entry.behaviour {
            Some(HidingBehaviour::Cloak) => platform.set_cloaked(hwnd, false),
            Some(HidingBehaviour::Minimize) => platform.show(hwnd, ShowState::Restore),
            Some(HidingBehaviour::Hide) => platform.show(hwnd, ShowState::ShowNoActivate),
            // On screen the whole time, only translucent. Clearing the alpha
            // below is the whole of it.
            None => Ok(()),
        };
        let restored = match result {
            Ok(()) => {
                if entry.behaviour.is_some() {
                    back.push(hwnd);
                }
                true
            }
            Err(e) => {
                tracing::error!(%hwnd, error = %e, "could not put the window back");
                false
            }
        };
        let mut alpha_cleared = true;
        if entry.faded
            && let Err(e) = platform.set_transparency(hwnd, None)
        {
            tracing::debug!(%hwnd, error = %e, "could not clear the alpha");
            alpha_cleared = false;
        }
        // A window that was only faded has nothing else to it, so a failed
        // alpha clear is the whole failure and the entry has to survive.
        if !restored || (entry.behaviour.is_none() && !alpha_cleared) {
            unfinished.push(entry);
        }
    }

    // Only what was genuinely dealt with is forgotten. A window this run could
    // not put back is still owed to the user on the next one.
    save(path, &unfinished);
    if !unfinished.is_empty() {
        tracing::warn!(
            count = unfinished.len(),
            "some windows could not be put back yet, keeping them in the record"
        );
    }
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
            behaviour: Some(behaviour),
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
        assert_eq!(load(&path), Some(entries));
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
    fn a_damaged_record_is_kept_rather_than_deleted() {
        // A half-written record is the shape a kill leaves behind. Deleting it
        // destroys the only evidence that windows are off screen, so a read
        // that fails has to be distinguishable from a file that is not there.
        let path = temp("broken");
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert_eq!(load(&path), None);

        let platform = crate::platform::new(true);
        assert!(recover(platform.as_ref(), &path).is_empty());
        assert!(
            path.exists(),
            "recover deleted a record it could not read, which is the only \
             thing that knew where the windows went"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_record_is_not_an_error() {
        assert_eq!(load(&temp("missing")), Some(Vec::new()));
    }

    #[test]
    fn the_record_is_replaced_whole_and_never_left_half_written() {
        // The temporary the write goes through must not survive, and the file
        // that is there has to be a complete record at every moment.
        let path = temp("atomic");
        save(&path, &[entry(1, HidingBehaviour::Cloak)]);
        save(
            &path,
            &[
                entry(2, HidingBehaviour::Hide),
                entry(3, HidingBehaviour::Cloak),
            ],
        );

        assert_eq!(load(&path).unwrap().len(), 2);
        assert!(
            !path.with_extension("json.new").exists(),
            "the temporary file was left behind"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_window_the_record_could_not_name_is_still_put_back() {
        // An entry written before its identity was known carries pid 0 and no
        // class. Matching that strictly can only ever fail, so it would strand
        // exactly the window the record exists for.
        let live = crate::platform::WindowInfo {
            pid: 4242,
            class: "Chrome_WidgetWin_1".to_owned(),
            ..crate::platform::WindowInfo::placeholder(Hwnd(1))
        };

        let known = Entry {
            hwnd: 1,
            pid: 4242,
            class: "Chrome_WidgetWin_1".to_owned(),
            behaviour: Some(HidingBehaviour::Cloak),
            faded: false,
        };
        assert!(still_the_same_window(&known, &live));

        let anonymous = Entry {
            pid: 0,
            class: String::new(),
            ..known.clone()
        };
        assert!(
            still_the_same_window(&anonymous, &live),
            "a window with no name recorded was skipped, and nothing else would have put it back"
        );

        let stranger = Entry {
            pid: 99,
            class: "Notepad".to_owned(),
            ..known.clone()
        };
        assert!(
            !still_the_same_window(&stranger, &live),
            "the handle was reused by another window and Mochi touched it anyway"
        );
    }

    #[test]
    fn a_window_that_could_not_be_inspected_stays_in_the_record() {
        // A handle that cannot be read may be a window that is simply gone, or
        // a transient failure. Dropping the entry is how a window gets
        // stranded, so it is kept for the next start to try again.
        let path = temp("kept");
        save(&path, &[entry(4242, HidingBehaviour::Cloak)]);

        let platform = crate::platform::new(true);
        assert!(recover(platform.as_ref(), &path).is_empty());
        assert_eq!(
            load(&path).unwrap().len(),
            1,
            "an entry this run could not deal with was thrown away"
        );
        let _ = std::fs::remove_file(&path);
    }
}
