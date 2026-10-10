//! What the setup decides, apart from the calls that carry it out, so the
//! tests can check every decision without touching the machine they run on.
//!
//! The rules match scripts\install.ps1 on purpose: an install made by either
//! one is upgraded or removed cleanly by the other.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "Mochi";
pub const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Mochi";
pub const STATE_KEY: &str = r"Software\Mochi";
pub const ENV_KEY: &str = "Environment";
pub const WATCHDOG_TASK: &str = "MochiWatchdog";
pub const SETUP_NAME: &str = "Mochi-Setup.exe";
pub const BINARIES: [&str; 2] = ["mochi.exe", "mochic.exe"];

/// Every place the setup reads or writes, worked out from the folders
/// Windows names for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub root: PathBuf,
    pub bin: PathBuf,
    pub setup_copy: PathBuf,
    pub shortcut: PathBuf,
    pub config: PathBuf,
    pub hotkeys: PathBuf,
    pub legacy_hotkeys: PathBuf,
}

impl Layout {
    pub fn new(local_app_data: &Path, start_menu_programs: &Path, profile: &Path) -> Self {
        let root = local_app_data.join("Programs").join("Mochi");
        let hotkey_dir = profile.join(".config").join("mochi");
        Self {
            bin: root.join("bin"),
            setup_copy: root.join(SETUP_NAME),
            shortcut: start_menu_programs.join("Mochi.lnk"),
            config: profile.join("mochi.json"),
            hotkeys: hotkey_dir.join("hotkeys"),
            legacy_hotkeys: hotkey_dir.join("whkdrc"),
            root,
        }
    }

    pub fn mochi(&self) -> PathBuf {
        self.bin.join("mochi.exe")
    }

    pub fn mochic(&self) -> PathBuf {
        self.bin.join("mochic.exe")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    pub quiet: bool,
}

pub const USAGE: &str = "Mochi-Setup.exe [--uninstall] [--quiet]";

/// The arguments after the program name. Slashes are accepted too, as
/// Windows tools usually do.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut options = Options {
        mode: Mode::Install,
        quiet: false,
    };
    for arg in args {
        let text = arg.to_string_lossy().to_ascii_lowercase();
        match text.trim_start_matches(['-', '/']) {
            "uninstall" | "u" => options.mode = Mode::Uninstall,
            "quiet" | "q" | "s" | "silent" => options.quiet = true,
            _ => {
                return Err(format!(
                    "Unknown option: {}\n\nUse: {USAGE}",
                    arg.to_string_lossy()
                ));
            }
        }
    }
    Ok(options)
}

/// Whether the arguments ask for no dialogs, even when one of them is
/// unknown: a scripted install must not stop at a dialog nobody sees.
pub fn asks_for_quiet(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().any(|arg| {
        let text = arg.to_string_lossy().to_ascii_lowercase();
        matches!(
            text.trim_start_matches(['-', '/']),
            "quiet" | "q" | "s" | "silent"
        )
    })
}

/// The bin folder of an install scripts\install.ps1 recorded in a folder
/// other than this one, or None when the record names this one or nothing.
pub fn other_bin(recorded: Option<&str>, layout: &Layout) -> Option<PathBuf> {
    let root = PathBuf::from(recorded.map(str::trim).filter(|text| !text.is_empty())?);
    (!same_path(&root, &layout.root)).then(|| root.join("bin"))
}

/// Expands %NAME% the way Windows does for a REG_EXPAND_SZ value: a name
/// with no variable behind it stays as it was written.
pub fn expand_vars(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after
            .find('%')
            .filter(|&end| end > 0)
            .and_then(|end| lookup(&after[..end]).map(|value| (end, value)));
        match value {
            Some((end, value)) => {
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The form a PATH entry is compared in: trimmed, unquoted, variables
/// expanded, no trailing backslash, case folded. The raw text is what gets
/// written back; this is only for deciding whether an entry is ours.
pub fn comparable(entry: &str, expand: &dyn Fn(&str) -> String) -> String {
    let text = entry.trim().trim_matches('"');
    if text.is_empty() {
        return String::new();
    }
    expand(text).trim_end_matches('\\').to_lowercase()
}

/// The user PATH with `dir` appended, or None when it is already on it.
///
/// Only the one entry is added and the rest of the value is left exactly as
/// it was, empty fields included: an empty field means the current
/// directory, and dropping it would change what the PATH does.
pub fn path_with(raw: &str, dir: &str, expand: &dyn Fn(&str) -> String) -> Option<String> {
    let wanted = comparable(dir, expand);
    if !raw.is_empty()
        && raw
            .split(';')
            .any(|entry| comparable(entry, expand) == wanted)
    {
        return None;
    }
    Some(if raw.is_empty() {
        dir.to_owned()
    } else if raw.ends_with(';') {
        format!("{raw}{dir}")
    } else {
        format!("{raw};{dir}")
    })
}

/// The user PATH without any entry that names `dir`, or None when there was
/// none to remove.
pub fn path_without(raw: &str, dir: &str, expand: &dyn Fn(&str) -> String) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let wanted = comparable(dir, expand);
    let entries: Vec<&str> = raw.split(';').collect();
    let kept: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|entry| comparable(entry, expand) != wanted)
        .collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

/// The Run value scripts\autostart.ps1 -Enable writes, word for word, so that
/// script reports it as registered and its -Disable removes it.
pub fn autostart_command(mochic: &Path) -> String {
    let quoted = mochic.display().to_string().replace('\'', "''");
    format!(
        "powershell.exe -NoProfile -WindowStyle Hidden -Command \"Start-Process -FilePath '{quoted}' -ArgumentList 'start' -WindowStyle Hidden\""
    )
}

/// Whether a Run value starts mochic, which is what makes it ours: the word
/// mochic on its own, with or without .exe.
pub fn runs_mochic(value: &str) -> bool {
    let lower = value.to_lowercase();
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    lower.match_indices("mochic").any(|(at, found)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + found.len()..].chars().next();
        !word(before) && !word(after)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAction {
    Write,
    AlreadyThere,
    /// A value under the same name that does not run mochic belongs to
    /// another program and is not ours to replace.
    LeaveAlone,
}

pub fn run_action(current: Option<&str>, wanted: &str) -> RunAction {
    match current {
        None => RunAction::Write,
        Some(value) if value == wanted => RunAction::AlreadyThere,
        Some(value) if runs_mochic(value) => RunAction::Write,
        Some(_) => RunAction::LeaveAlone,
    }
}

/// `mochic quickstart` writes whichever of the two files is missing and
/// never overwrites one, so it only has to run when one of them is.
pub fn needs_quickstart(config: bool, hotkeys: bool, legacy_hotkeys: bool) -> bool {
    !config || !(hotkeys || legacy_hotkeys)
}

/// Windows paths compare without regard to case or slash direction.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.display()
            .to_string()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    Text(String),
    Number(u32),
}

/// The values of the Apps & features entry.
pub fn uninstall_entry(
    layout: &Layout,
    version: &str,
    size_bytes: u64,
) -> Vec<(&'static str, RegValue)> {
    let setup = layout.setup_copy.display();
    vec![
        ("DisplayName", RegValue::Text("Mochi".into())),
        ("Publisher", RegValue::Text("Mochi".into())),
        ("DisplayVersion", RegValue::Text(version.into())),
        (
            "DisplayIcon",
            RegValue::Text(format!("{},0", layout.mochi().display())),
        ),
        (
            "InstallLocation",
            RegValue::Text(layout.root.display().to_string()),
        ),
        (
            "UninstallString",
            RegValue::Text(format!("\"{setup}\" --uninstall")),
        ),
        (
            "QuietUninstallString",
            RegValue::Text(format!("\"{setup}\" --uninstall --quiet")),
        ),
        ("NoModify", RegValue::Number(1)),
        ("NoRepair", RegValue::Number(1)),
        (
            "EstimatedSize",
            RegValue::Number(u32::try_from(size_bytes.div_ceil(1024)).unwrap_or(u32::MAX)),
        ),
    ]
}

/// What an uninstall deletes, in order. Folders go only when they are empty
/// by then, so nothing the user put there is lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub files: Vec<PathBuf>,
    pub dirs: Vec<PathBuf>,
    /// Set when the setup runs from the install folder: a running exe cannot
    /// delete itself, so this file and the folder go a moment after it exits.
    pub after_exit: Option<PathBuf>,
}

pub fn removal(layout: &Layout, current_exe: &Path) -> Removal {
    let mut files: Vec<PathBuf> = BINARIES.iter().map(|name| layout.bin.join(name)).collect();
    let running_copy = same_path(current_exe, &layout.setup_copy);
    let mut dirs = vec![layout.bin.clone()];
    if running_copy {
        return Removal {
            files,
            dirs,
            after_exit: Some(layout.setup_copy.clone()),
        };
    }
    files.push(layout.setup_copy.clone());
    dirs.push(layout.root.clone());
    Removal {
        files,
        dirs,
        after_exit: None,
    }
}

/// The arguments for a hidden cmd.exe that waits for this process to exit,
/// deletes `file` and then removes `dir` if it is empty. None for a path cmd
/// would read as something else; the file then stays behind, harmlessly.
pub fn cleanup_after_exit(file: &Path, dir: &Path) -> Option<String> {
    let file = file.display().to_string();
    let dir = dir.display().to_string();
    if [&file, &dir]
        .iter()
        .any(|p| p.contains(['"', '%']) || p.is_empty())
    {
        return None;
    }
    // ping is the one wait cmd offers that needs no console input.
    Some(format!(
        "/d /c ping -n 3 127.0.0.1 >nul & del /f /q \"{file}\" & rmdir \"{dir}\""
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(text: &str) -> String {
        text.to_owned()
    }

    fn local(text: &str) -> String {
        text.replace("%LOCALAPPDATA%", r"C:\Users\a\AppData\Local")
    }

    const BIN: &str = r"C:\Users\a\AppData\Local\Programs\Mochi\bin";

    fn layout() -> Layout {
        Layout::new(
            Path::new(r"C:\Users\a\AppData\Local"),
            Path::new(r"C:\Users\a\AppData\Roaming\Microsoft\Windows\Start Menu\Programs"),
            Path::new(r"C:\Users\a"),
        )
    }

    #[test]
    fn layout_matches_the_install_script() {
        let l = layout();
        assert_eq!(l.bin, Path::new(BIN));
        assert_eq!(l.mochic(), Path::new(BIN).join("mochic.exe"));
        assert_eq!(
            l.setup_copy,
            Path::new(r"C:\Users\a\AppData\Local\Programs\Mochi\Mochi-Setup.exe")
        );
        assert!(l.shortcut.ends_with(r"Start Menu\Programs\Mochi.lnk"));
        assert_eq!(l.config, Path::new(r"C:\Users\a\mochi.json"));
        assert_eq!(l.hotkeys, Path::new(r"C:\Users\a\.config\mochi\hotkeys"));
        assert_eq!(
            l.legacy_hotkeys,
            Path::new(r"C:\Users\a\.config\mochi\whkdrc")
        );
    }

    #[test]
    fn arguments() {
        let parse = |args: &[&str]| parse_args(args.iter().map(OsString::from));
        assert_eq!(
            parse(&[]),
            Ok(Options {
                mode: Mode::Install,
                quiet: false
            })
        );
        assert_eq!(
            parse(&["--uninstall", "--quiet"]),
            Ok(Options {
                mode: Mode::Uninstall,
                quiet: true
            })
        );
        assert_eq!(
            parse(&["/S"]),
            Ok(Options {
                mode: Mode::Install,
                quiet: true
            })
        );
        assert!(parse(&["--frobnicate"]).is_err());
    }

    #[test]
    fn quiet_is_seen_even_beside_a_typo() {
        let quiet = |args: &[&str]| asks_for_quiet(args.iter().map(OsString::from));
        assert!(quiet(&["--quiet", "--frobnicate"]));
        assert!(quiet(&["/S"]));
        assert!(!quiet(&["--frobnicate"]));
    }

    #[test]
    fn another_recorded_install_is_found_only_elsewhere() {
        let l = layout();
        assert_eq!(other_bin(None, &l), None);
        assert_eq!(other_bin(Some("  "), &l), None);
        let same = l.root.display().to_string().to_lowercase() + "\\";
        assert_eq!(other_bin(Some(&same), &l), None);
        assert_eq!(
            other_bin(Some(r"D:\Apps\Mochi"), &l),
            Some(PathBuf::from(r"D:\Apps\Mochi\bin"))
        );
    }

    #[test]
    fn variables_expand_and_unknown_ones_stay() {
        let lookup = |name: &str| (name == "HOME").then(|| r"C:\Users\a".to_owned());
        assert_eq!(expand_vars(r"%HOME%\bin", &lookup), r"C:\Users\a\bin");
        assert_eq!(
            expand_vars(r"%NOPE%\bin;%HOME%", &lookup),
            r"%NOPE%\bin;C:\Users\a"
        );
        assert_eq!(expand_vars("100%", &lookup), "100%");
        assert_eq!(expand_vars("%%HOME%", &lookup), r"%C:\Users\a");
    }

    #[test]
    fn path_is_added_to_an_empty_value() {
        assert_eq!(path_with("", BIN, &none).as_deref(), Some(BIN));
    }

    #[test]
    fn path_is_appended_and_the_rest_left_as_it_was() {
        let raw = r"%USERPROFILE%\bin;;C:\Tools";
        assert_eq!(path_with(raw, BIN, &none), Some(format!("{raw};{BIN}")));
        assert_eq!(
            path_with("C:\\Tools;", BIN, &none),
            Some(format!("C:\\Tools;{BIN}"))
        );
    }

    #[test]
    fn path_already_present_in_any_spelling_is_not_added_twice() {
        for spelling in [
            BIN.to_owned(),
            format!("{BIN}\\"),
            format!("\"{BIN}\""),
            BIN.to_uppercase(),
            r"%LOCALAPPDATA%\Programs\Mochi\bin".to_owned(),
        ] {
            let raw = format!(r"C:\Tools;{spelling}");
            assert_eq!(path_with(&raw, BIN, &local), None, "{spelling}");
        }
    }

    #[test]
    fn path_removal_takes_every_spelling_and_keeps_the_rest() {
        let raw = format!(r"C:\Tools;;%LOCALAPPDATA%\Programs\Mochi\bin;D:\x;{BIN}\");
        assert_eq!(
            path_without(&raw, BIN, &local).as_deref(),
            Some(r"C:\Tools;;D:\x")
        );
        assert_eq!(path_without(r"C:\Tools;D:\x", BIN, &local), None);
        assert_eq!(path_without("", BIN, &local), None);
        assert_eq!(path_without(BIN, BIN, &local).as_deref(), Some(""));
    }

    #[test]
    fn autostart_command_is_the_one_the_scripts_write() {
        let command = autostart_command(Path::new(r"C:\Users\o'neil\bin\mochic.exe"));
        assert_eq!(
            command,
            r#"powershell.exe -NoProfile -WindowStyle Hidden -Command "Start-Process -FilePath 'C:\Users\o''neil\bin\mochic.exe' -ArgumentList 'start' -WindowStyle Hidden""#
        );
        assert!(runs_mochic(&command));
    }

    #[test]
    fn only_a_word_mochic_counts_as_ours() {
        assert!(runs_mochic(r"C:\bin\MOCHIC.EXE start"));
        assert!(runs_mochic("mochic start"));
        assert!(!runs_mochic(r"C:\bin\mochicat.exe"));
        assert!(!runs_mochic(r"C:\bin\mochi.exe"));
        assert!(!runs_mochic("my_mochic"));
        assert!(!runs_mochic(""));
    }

    #[test]
    fn run_value_decisions() {
        let wanted = autostart_command(Path::new(r"C:\b\mochic.exe"));
        assert_eq!(run_action(None, &wanted), RunAction::Write);
        assert_eq!(run_action(Some(&wanted), &wanted), RunAction::AlreadyThere);
        assert_eq!(
            run_action(Some(r"D:\old\mochic.exe start"), &wanted),
            RunAction::Write
        );
        assert_eq!(
            run_action(Some(r"C:\Other\app.exe"), &wanted),
            RunAction::LeaveAlone
        );
    }

    #[test]
    fn quickstart_runs_only_when_a_file_is_missing() {
        assert!(!needs_quickstart(true, true, false));
        assert!(!needs_quickstart(true, false, true));
        assert!(needs_quickstart(false, true, false));
        assert!(needs_quickstart(true, false, false));
    }

    #[test]
    fn paths_compare_like_windows_does() {
        assert!(same_path(Path::new(r"C:\A\b.exe"), Path::new("c:/a/B.EXE")));
        assert!(!same_path(
            Path::new(r"C:\A\b.exe"),
            Path::new(r"C:\A\c.exe")
        ));
    }

    #[test]
    fn uninstall_entry_points_at_the_copy_in_the_install_folder() {
        let entry = uninstall_entry(&layout(), "0.1.21", 5_000_000);
        let get = |name: &str| {
            entry
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("DisplayName"), Some(RegValue::Text("Mochi".into())));
        assert_eq!(get("Publisher"), Some(RegValue::Text("Mochi".into())));
        assert_eq!(get("DisplayVersion"), Some(RegValue::Text("0.1.21".into())));
        assert_eq!(
            get("UninstallString"),
            Some(RegValue::Text(
                r#""C:\Users\a\AppData\Local\Programs\Mochi\Mochi-Setup.exe" --uninstall"#.into()
            ))
        );
        assert_eq!(get("NoModify"), Some(RegValue::Number(1)));
        assert_eq!(get("NoRepair"), Some(RegValue::Number(1)));
        assert_eq!(get("EstimatedSize"), Some(RegValue::Number(4883)));
    }

    #[test]
    fn uninstall_from_a_download_removes_everything_now() {
        let l = layout();
        let plan = removal(&l, Path::new(r"C:\Users\a\Downloads\Mochi-Setup.exe"));
        assert_eq!(
            plan.files,
            vec![l.mochi(), l.mochic(), l.setup_copy.clone()]
        );
        assert_eq!(plan.dirs, vec![l.bin.clone(), l.root.clone()]);
        assert_eq!(plan.after_exit, None);
    }

    #[test]
    fn uninstall_from_the_installed_copy_leaves_itself_for_after_exit() {
        let l = layout();
        let running = PathBuf::from(l.setup_copy.display().to_string().to_uppercase());
        let plan = removal(&l, &running);
        assert_eq!(plan.files, vec![l.mochi(), l.mochic()]);
        assert_eq!(plan.dirs, vec![l.bin.clone()]);
        assert_eq!(plan.after_exit, Some(l.setup_copy.clone()));
    }

    #[test]
    fn cleanup_command_quotes_paths_and_refuses_what_cmd_would_rewrite() {
        let l = layout();
        let args = cleanup_after_exit(&l.setup_copy, &l.root).unwrap();
        assert!(
            args.contains(r#"del /f /q "C:\Users\a\AppData\Local\Programs\Mochi\Mochi-Setup.exe""#)
        );
        assert!(args.ends_with(r#"rmdir "C:\Users\a\AppData\Local\Programs\Mochi""#));
        assert!(
            !args.contains("/s"),
            "the folder must only go when it is empty"
        );
        assert_eq!(
            cleanup_after_exit(Path::new(r"C:\100%\x.exe"), &l.root),
            None
        );
    }
}
