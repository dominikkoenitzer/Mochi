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

use crate::platform::{CloakUnsupported, Hwnd, Platform, ShowState};

/// One window that was off screen when the file was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The handle, as a plain number.
    pub hwnd: isize,
    /// The process that owns the window, to catch handle reuse.
    ///
    /// Defaulted, like every field below. A record written by a different
    /// build must not be thrown away wholesale over one field it does not
    /// recognise: the entries in it name windows that are off screen right
    /// now, and losing them means losing the windows.
    #[serde(default)]
    pub pid: u32,
    /// The window class, to catch handle reuse within one process.
    #[serde(default)]
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
    #[serde(default)]
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
    // Written and FLUSHED before the rename. `fs::write` only hands the bytes
    // to the cache: NTFS journals the rename's metadata, not the file's data,
    // so a power loss can leave a correctly named record full of zeros, which
    // does not parse, which means every window a previous session hid stays
    // hidden with nothing naming it. The claim above this function that "the
    // file on disk is always one whole record" is only true with this call.
    let written = std::fs::File::create(&temporary).and_then(|mut file| {
        use std::io::Write as _;
        file.write_all(&bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        tracing::error!(error = %e, "could not write the off screen record");
        let _ = std::fs::remove_file(&temporary);
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
    // Parsed entry by entry, not as one document. The whole file used to go
    // through `Vec<Entry>`, so a single malformed entry, or one field a
    // different build wrote, lost EVERY window named in it rather than the one.
    // The entries are independent facts about independent windows, so they are
    // recovered independently.
    if let Ok(raw) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) {
        let total = raw.len();
        let entries: Vec<Entry> = raw
            .into_iter()
            .filter_map(|value| match serde_json::from_value::<Entry>(value) {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::error!(error = %e, "an entry in the off screen record could not be read");
                    None
                }
            })
            .collect();
        if entries.len() < total {
            tracing::error!(
                kept = entries.len(),
                lost = total - entries.len(),
                path = %path.display(),
                "part of the off screen record could not be read;                  a window a previous session hid may still be off screen"
            );
        }
        return Some(entries);
    }

    match serde_json::from_slice::<Vec<Entry>>(&bytes) {
        Ok(entries) => Some(entries),
        Err(e) => {
            // Moved aside, not left in place. Leaving it was the old behaviour
            // and it only looked safe: this session carries on with an empty
            // record, and the very first window it hides rewrites the file, so
            // the damaged one survived for about as long as it took the user to
            // change workspace. Whatever it named was then off screen, out of
            // the model, out of the record and invisible to `restore-windows`.
            // Under a name of its own it survives, and the log says where.
            let aside = quarantine(path);
            tracing::error!(
                error = %e,
                path = %path.display(),
                kept = %aside.as_deref().unwrap_or(path).display(),
                "the off screen record is damaged; it has been moved aside rather than                  overwritten, because a window a previous session hid may still be off screen"
            );
            None
        }
    }
}

/// Moves an unreadable record out of the way under a name of its own.
///
/// Returns where it went, or `None` when it could not be moved, in which case
/// the caller has said what it knows and there is nothing further to do.
fn quarantine(path: &Path) -> Option<std::path::PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let aside = path.with_extension(format!("damaged-{stamp}.json"));
    std::fs::rename(path, &aside).ok().map(|()| aside)
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

/// What a recovery pass managed, and what it did not.
///
/// The leftovers matter as much as the successes: they are still off screen,
/// and the session starting now has to take them into its own record. A record
/// that starts empty rewrites the file on its first hide and erases the only
/// trace of them.
#[derive(Debug, Default)]
pub struct Recovered {
    /// Windows that were put back on screen.
    pub back: Vec<Hwnd>,
    /// Entries that are still owed to the user.
    pub unfinished: Vec<Entry>,
}

/// Puts back everything a previous session left off screen.
///
/// Returns what came back and what is still owed.
pub fn recover(platform: &dyn Platform, path: &Path) -> Recovered {
    let Some(entries) = load(path) else {
        // Unreadable, and `load` has already moved it aside under a name this
        // session will not write to, so the evidence outlives the next save.
        return Recovered::default();
    };
    if entries.is_empty() {
        let _ = std::fs::remove_file(path);
        return Recovered::default();
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
            // Two ways this is permanent rather than bad luck, and both
            // mean every start from here will fail the same way.
            //
            // `CloakUnsupported`: the shell has no application view for
            // the window, so the only cloak left to try is DWM's, and DWM
            // only cloaks windows of the calling process. The window was
            // put away through the shell by a session that could, and
            // nothing this process can call will undo it.
            //
            // `outranks_us`: the window belongs to an elevated process and
            // this session is not elevated, so Windows refuses on sight.
            Err(ref e)
                if e.downcast_ref::<CloakUnsupported>().is_some() || platform.outranks_us(hwnd) =>
            {
                // Refused today and at every start after: the previous session
                // hid this while it had the rights to and this one does not.
                // It happens when Mochi is started once from an administrator
                // terminal and once normally. Keeping the entry only
                // guarantees the same error on every start for as long as the
                // window lives, and buries the one window the user has to go
                // and rescue by hand. Say it once, plainly, and let go.
                tracing::warn!(
                    %hwnd,
                    title = %info.title,
                    "a previous session took this window off screen and this one cannot put it                      back, because the window outranks mochi now. Bring it back from the taskbar                      or with alt+tab. Mochi is letting go of it rather than trying again at                      every start"
                );
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
        // Either failure keeps the entry. Gating the alpha half on "this
        // window was only faded" meant a window that was both cloaked and
        // faded, uncloaked successfully and then failed to go opaque, was
        // forgotten while still translucent: at a low alpha that is a window
        // on screen, in Alt-Tab, focusable and impossible to see, with nothing
        // anywhere recording that Mochi did it.
        if !restored || !alpha_cleared {
            unfinished.push(Entry {
                // Narrowed to the half still owed, so a successful uncloak is
                // not attempted a second time on the next start.
                behaviour: if restored { None } else { entry.behaviour },
                faded: !alpha_cleared,
                ..entry
            });
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
    Recovered { back, unfinished }
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

    /// Removes the set-aside copies an earlier run of the same test left.
    fn clear_quarantine(path: &Path) {
        let Some(dir) = path.parent() else { return };
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&stem) && name.contains("damaged-") {
                let _ = std::fs::remove_file(entry.path());
            }
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
    fn one_unreadable_entry_does_not_lose_the_others() {
        // A record written by a different build, or with one entry mangled.
        // Parsed as a single document, the one bad entry lost every window the
        // file named; they are independent facts about independent windows and
        // are recovered independently. The middle one here carries a field of
        // the wrong type, which no default can rescue.
        let path = temp("partial");
        let json = r#"[
            {"hwnd": 1, "pid": 7, "class": "A", "behaviour": "Cloak", "faded": false},
            {"hwnd": "not a number", "pid": 7, "class": "B", "faded": false},
            {"hwnd": 3, "pid": 7, "class": "C", "behaviour": "Cloak", "faded": false}
        ]"#;
        std::fs::write(&path, json).expect("write");

        let entries = load(&path).expect("the readable entries survive");
        let hwnds: Vec<isize> = entries.iter().map(|e| e.hwnd).collect();
        assert_eq!(hwnds, vec![1, 3], "a bad entry took good ones with it");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_entry_from_another_build_still_names_its_window() {
        // Only `hwnd` is really required now. A build that adds a field, or one
        // that drops one, must still hand back the handle: that is the part
        // that gets the window back on screen.
        let path = temp("future");
        std::fs::write(
            &path,
            br#"[{"hwnd": 42, "behaviour": "Cloak", "something_new": true}]"#,
        )
        .expect("write");

        let entries = load(&path).expect("it parses");
        assert_eq!(entries.len(), 1, "the entry was thrown away");
        assert_eq!(entries[0].hwnd, 42);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_damaged_record_is_kept_rather_than_deleted() {
        // A half-written record is the shape a kill leaves behind. Deleting it
        // destroys the only evidence that windows are off screen, so a read
        // that fails has to be distinguishable from a file that is not there.
        let path = temp("broken");
        // `temp` clears the record itself; the quarantined copies of earlier
        // runs share its stem and would otherwise accumulate and be counted.
        clear_quarantine(&path);
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert_eq!(load(&path), None);

        let platform = crate::platform::new(true);
        assert!(recover(platform.as_ref(), &path).back.is_empty());

        // Kept, under a name of its own. It used to be left exactly where it
        // was, which only looked safe: this session carries on with an empty
        // record and its first hide rewrites that path, so the evidence lasted
        // only until the user next changed workspace. Moved aside, it outlives
        // the session that found it.
        let dir = path.parent().expect("a parent");
        let stem = path
            .file_stem()
            .expect("a stem")
            .to_string_lossy()
            .to_string();
        let kept: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(&stem) && n.contains("damaged-"))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "recover lost a record it could not read, which is the only thing",
        );
        for name in kept {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_record_is_not_an_error() {
        assert_eq!(load(&temp("missing")), Some(Vec::new()));
    }

    #[test]
    fn a_damaged_record_is_moved_aside_instead_of_being_overwritten() {
        // The precondition is real: a power loss between the write and the
        // flush leaves a correctly named file full of zeros. Left in place, the
        // very first window the new session hides rewrites it, and whatever it
        // named is off screen with nothing naming it. This is the one file that
        // knows where the user's windows went.
        let dir = std::env::temp_dir().join(format!("mochi-damaged-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("off-screen.json");
        std::fs::write(&path, b"not json at all, and not even close").expect("write");

        assert!(load(&path).is_none(), "damaged json must not parse");
        assert!(
            !path.exists(),
            "the damaged record was left where the next save would destroy it"
        );

        let kept: Vec<_> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("damaged-"))
            .collect();
        assert_eq!(kept.len(), 1, "the evidence was not kept: {kept:?}");

        // And a fresh save beside it cannot touch what was kept.
        save(&path, &[entry(1, HidingBehaviour::Cloak)]);
        assert!(kept[0].path().exists(), "the kept copy was overwritten");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(recover(platform.as_ref(), &path).back.is_empty());
        assert_eq!(
            load(&path).unwrap().len(),
            1,
            "an entry this run could not deal with was thrown away"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn what_this_run_could_not_put_back_is_handed_to_the_next_session() {
        // The handover. `recover` keeps what it could not finish, and the
        // session starting now has to take those entries into its own record:
        // a record that starts empty rewrites the file on its very first hide,
        // and these entries were the only thing that knew those windows are
        // off screen. They are not in the model and a cloaked window cannot be
        // enumerated, so erasing them loses the window for good.
        let path = temp("handover");
        save(&path, &[entry(4242, HidingBehaviour::Cloak)]);

        let platform = crate::platform::new(true);
        let recovered = recover(platform.as_ref(), &path);
        assert!(recovered.back.is_empty());
        assert_eq!(
            recovered.unfinished.len(),
            1,
            "the entry it could not deal with was not reported to the caller"
        );
        assert_eq!(recovered.unfinished[0].hwnd, 4242);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_window_that_uncloaked_but_stayed_translucent_is_still_owed() {
        // Both halves are undone separately, and either failure has to keep
        // the entry. Keeping it only when the window was faded *and nothing
        // else* meant a window that was cloaked and faded, uncloaked fine and
        // then refused to go opaque, was forgotten while still translucent —
        // at a low alpha that is a window on screen, in Alt-Tab, focusable and
        // impossible to see, with nothing recording that Mochi did it.
        let path = temp("still-faded");
        let mut e = entry(4243, HidingBehaviour::Cloak);
        e.faded = true;
        save(&path, &[e]);

        // The dry-run platform cannot read the window, so the uncloak is not
        // even attempted and the whole entry survives; that much the sibling
        // test already covers. What matters here is the shape of what is kept.
        let platform = crate::platform::new(true);
        let recovered = recover(platform.as_ref(), &path);
        assert_eq!(recovered.unfinished.len(), 1);
        assert!(
            recovered.unfinished[0].faded,
            "the alpha half of the entry was dropped"
        );

        let _ = std::fs::remove_file(&path);
    }
}
