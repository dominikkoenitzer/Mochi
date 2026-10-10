//! The install and the uninstall, step by step, in the order scripts\install.ps1
//! takes them.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::plan::{self, Layout, Mode, Options, RunAction};
use crate::win;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DOWNLOAD: &str = "https://get-mochi.vercel.app/download/windows";
const WAY_OUT: &str = "Pause (or alt + f12) turns tiling off and on, alt + shift + e stops Mochi and puts every window back.";
const RUN_BACKUP: &str = "RunBackup_Mochi";

struct Payload {
    mochi: &'static [u8],
    mochic: &'static [u8],
}

#[cfg(payload)]
fn payload() -> Option<Payload> {
    Some(Payload {
        mochi: include_bytes!(env!("MOCHI_SETUP_MOCHI")),
        mochic: include_bytes!(env!("MOCHI_SETUP_MOCHIC")),
    })
}

#[cfg(not(payload))]
fn payload() -> Option<Payload> {
    None
}

pub fn run() -> i32 {
    let options = match plan::parse_args(std::env::args_os().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            win::tell("Mochi Setup", &message, true);
            return 2;
        }
    };
    let Some(layout) = layout() else {
        return fail(
            options,
            "Mochi could not be set up",
            "Windows did not say where this user's programs go.",
        );
    };
    match options.mode {
        Mode::Install => install(options, &layout),
        Mode::Uninstall => uninstall(options, &layout),
    }
}

fn layout() -> Option<Layout> {
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let local = set("LOCALAPPDATA")?;
    let profile = set("USERPROFILE")?;
    let programs = win::start_menu_programs().or_else(|| {
        set("APPDATA").map(|roaming| roaming.join(r"Microsoft\Windows\Start Menu\Programs"))
    })?;
    Some(Layout::new(&local, &programs, &profile))
}

fn fail(options: Options, instruction: &str, content: &str) -> i32 {
    if !options.quiet {
        win::tell(instruction, content, true);
    }
    1
}

fn expand(text: &str) -> String {
    plan::expand_vars(text, &|name| std::env::var(name).ok())
}

fn install(options: Options, layout: &Layout) -> i32 {
    let Some(payload) = payload() else {
        return fail(
            options,
            "This Mochi Setup has no Mochi inside",
            &format!(
                "It was built without the Mochi program files. Download Mochi-Setup.exe from {DOWNLOAD}"
            ),
        );
    };
    if !options.quiet && !win::ask("Install Mochi?", WAY_OUT, "Install") {
        return 0;
    }

    // A running mochi.exe cannot be replaced, and asking it to stop puts
    // every window it was hiding back first.
    if let Err(message) = stop_mochi(layout, Some(&payload)) {
        return fail(options, "Mochi is still running", &message);
    }
    if let Err(message) = put_binaries(&layout.bin, &payload) {
        return fail(options, "Mochi could not be installed", &message);
    }

    let mut problems = Vec::new();
    let mut note = |result: Result<(), String>| {
        if let Err(message) = result {
            problems.push(message);
        }
    };
    let current = std::env::current_exe().ok();
    note(keep_setup_copy(layout, current.as_deref()));
    note(add_to_path(&layout.bin));
    note(quickstart(layout));
    note(win::write_text(
        plan::STATE_KEY,
        "InstallRoot",
        &layout.root.display().to_string(),
        false,
    ));
    note(register_autostart(layout));
    note(win::create_shortcut(
        &layout.shortcut,
        &layout.mochic(),
        &layout.mochi(),
    ));
    let size = (payload.mochi.len() + payload.mochic.len()) as u64
        + current
            .as_deref()
            .and_then(|exe| std::fs::metadata(exe).ok())
            .map_or(0, |meta| meta.len());
    note(write_uninstall_entry(layout, size));
    let started = start_mochi(layout);

    if options.quiet {
        return i32::from(started.is_err() || !problems.is_empty());
    }
    match started {
        Ok(()) if problems.is_empty() => {
            win::tell(
                "Mochi is running",
                "It starts with Windows from now on. alt + shift + e puts every window back.",
                false,
            );
            0
        }
        Ok(()) => {
            win::tell(
                "Mochi is running, with a few things left undone",
                &problems.join("\n"),
                true,
            );
            1
        }
        Err(message) => {
            problems.push(message);
            win::tell(
                "Mochi is installed but did not start",
                &problems.join("\n"),
                true,
            );
            1
        }
    }
}

fn uninstall(options: Options, layout: &Layout) -> i32 {
    if !options.quiet
        && !win::ask(
            "Remove Mochi?",
            "Every window goes back to normal. Your mochi.json and hotkeys stay where they are.",
            "Remove",
        )
    {
        return 0;
    }

    let mut problems = Vec::new();
    let mut note = |result: Result<(), String>| {
        if let Err(message) = result {
            problems.push(message);
        }
    };
    // Before the stop: the login start and the watchdog would bring back a
    // Mochi whose files are about to go.
    note(unregister_autostart());
    remove_watchdog();
    if let Err(message) = stop_mochi(layout, payload().as_ref()) {
        return fail(options, "Mochi is still running", &message);
    }
    note(remove_file(&layout.shortcut));
    note(remove_from_path(&layout.bin));
    note(win::delete_key(plan::UNINSTALL_KEY));
    note(win::delete_value(plan::STATE_KEY, "InstallRoot"));
    if win::key_is_empty(plan::STATE_KEY) {
        note(win::delete_key(plan::STATE_KEY));
    }

    let current = std::env::current_exe().unwrap_or_default();
    let removal = plan::removal(layout, &current);
    for file in &removal.files {
        note(remove_file(file));
    }
    for dir in &removal.dirs {
        // Only an empty folder goes: anything the user put there stays.
        let _ = std::fs::remove_dir(dir);
    }
    if let Some(file) = &removal.after_exit
        && let Some(args) = plan::cleanup_after_exit(file, &layout.root)
    {
        let _ = win::spawn_cmd(&args);
    }

    if options.quiet {
        return i32::from(!problems.is_empty());
    }
    if problems.is_empty() {
        win::tell(
            "Mochi is removed",
            "Your mochi.json and hotkeys are still in your user folder.",
            false,
        );
        0
    } else {
        win::tell(
            "Mochi is removed, with a few things left behind",
            &problems.join("\n"),
            true,
        );
        1
    }
}

/// Asks a running Mochi to stop and waits until it has. The installed mochic
/// speaks the running daemon's protocol best; the one inside this setup is
/// the fallback when there is none.
fn stop_mochi(layout: &Layout, payload: Option<&Payload>) -> Result<(), String> {
    if !win::mochi_running() {
        return Ok(());
    }
    let mut temp = None;
    let mochic = if layout.mochic().is_file() {
        layout.mochic()
    } else if let Some(payload) = payload {
        let dir = std::env::temp_dir().join(format!("mochi-setup-{}", std::process::id()));
        let path = dir.join("mochic.exe");
        if std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, payload.mochic))
            .is_ok()
        {
            temp = Some(dir);
        }
        path
    } else {
        PathBuf::new()
    };
    let _ = win::run_hidden(&mochic, &["stop"]);
    let stopped = win::wait_for(Duration::from_secs(10), || !win::mochi_running());
    if let Some(dir) = temp {
        let _ = std::fs::remove_dir_all(dir);
    }
    if stopped {
        Ok(())
    } else {
        Err("Press alt + shift + e to stop it, then open Mochi Setup again.".into())
    }
}

/// Both files are written next to their places first and only then moved
/// over the old ones, so a full disk never leaves a new mochi.exe beside an
/// old mochic.exe.
fn put_binaries(bin: &Path, payload: &Payload) -> Result<(), String> {
    std::fs::create_dir_all(bin).map_err(|e| format!("creating {}: {e}", bin.display()))?;
    let files = [("mochi.exe", payload.mochi), ("mochic.exe", payload.mochic)];
    let staged = |name: &str| bin.join(format!("{name}.new"));
    let clean_up = || {
        for (name, _) in files {
            let _ = std::fs::remove_file(staged(name));
        }
    };
    for (name, bytes) in files {
        if let Err(e) = std::fs::write(staged(name), bytes) {
            clean_up();
            return Err(format!("writing {}: {e}", bin.join(name).display()));
        }
    }
    for (name, _) in files {
        if let Err(e) = std::fs::rename(staged(name), bin.join(name)) {
            clean_up();
            return Err(format!("replacing {}: {e}", bin.join(name).display()));
        }
    }
    Ok(())
}

/// A copy of this setup in the install folder is what Apps & features runs
/// to remove Mochi later.
fn keep_setup_copy(layout: &Layout, current: Option<&Path>) -> Result<(), String> {
    let Some(current) = current else {
        return Err(
            "Setup could not find its own file, so Apps & features cannot remove Mochi.".into(),
        );
    };
    if plan::same_path(current, &layout.setup_copy) {
        return Ok(());
    }
    std::fs::copy(current, &layout.setup_copy)
        .map(drop)
        .map_err(|e| {
            format!(
                "copying the uninstaller to {}: {e}",
                layout.setup_copy.display()
            )
        })
}

fn add_to_path(dir: &Path) -> Result<(), String> {
    // A user without a PATH value gets one of the kind Windows itself writes.
    let (raw, expandable) = win::read_text(plan::ENV_KEY, "Path")?.unwrap_or((String::new(), true));
    if let Some(updated) = plan::path_with(&raw, &dir.display().to_string(), &expand) {
        win::write_text(plan::ENV_KEY, "Path", &updated, expandable)?;
        win::announce_environment_change();
    }
    Ok(())
}

fn remove_from_path(dir: &Path) -> Result<(), String> {
    let Some((raw, expandable)) = win::read_text(plan::ENV_KEY, "Path")? else {
        return Ok(());
    };
    if let Some(updated) = plan::path_without(&raw, &dir.display().to_string(), &expand) {
        win::write_text(plan::ENV_KEY, "Path", &updated, expandable)?;
        win::announce_environment_change();
    }
    Ok(())
}

fn quickstart(layout: &Layout) -> Result<(), String> {
    if !plan::needs_quickstart(
        layout.config.exists(),
        layout.hotkeys.exists(),
        layout.legacy_hotkeys.exists(),
    ) {
        return Ok(());
    }
    match win::run_hidden(&layout.mochic(), &["quickstart"]) {
        Ok((0, _)) => Ok(()),
        Ok((code, said)) => Err(format!(
            "writing the default settings failed ({code}): {said}"
        )),
        Err(e) => Err(format!("writing the default settings failed: {e}")),
    }
}

fn register_autostart(layout: &Layout) -> Result<(), String> {
    let wanted = plan::autostart_command(&layout.mochic());
    let current = win::read_text(plan::RUN_KEY, plan::RUN_VALUE)?.map(|(value, _)| value);
    match plan::run_action(current.as_deref(), &wanted) {
        RunAction::Write => win::write_text(plan::RUN_KEY, plan::RUN_VALUE, &wanted, false),
        RunAction::AlreadyThere => Ok(()),
        RunAction::LeaveAlone => {
            Err("Another program starts at login under the name Mochi, so Mochi does not start with Windows.".into())
        }
    }
}

/// Removes the Run value when it runs mochic, and puts back the value
/// scripts\autostart.ps1 -Enable saved when it replaced one.
fn unregister_autostart() -> Result<(), String> {
    let Some((current, _)) = win::read_text(plan::RUN_KEY, plan::RUN_VALUE)? else {
        return Ok(());
    };
    if !plan::runs_mochic(&current) {
        return Ok(());
    }
    win::delete_value(plan::RUN_KEY, plan::RUN_VALUE)?;
    if let Some((saved, _)) = win::read_text(plan::STATE_KEY, RUN_BACKUP)? {
        let kind_name = format!("{RUN_BACKUP}_Kind");
        let expandable = win::read_text(plan::STATE_KEY, &kind_name)?
            .is_some_and(|(kind, _)| kind == "ExpandString");
        win::write_text(plan::RUN_KEY, plan::RUN_VALUE, &saved, expandable)?;
        win::delete_value(plan::STATE_KEY, RUN_BACKUP)?;
        win::delete_value(plan::STATE_KEY, &kind_name)?;
    }
    Ok(())
}

/// The task scripts\autostart.ps1 -Watchdog registers. Left behind, it would
/// try to start a deleted mochic every five minutes.
fn remove_watchdog() {
    let schtasks = win::system32().join("schtasks.exe");
    if matches!(
        win::run_hidden(&schtasks, &["/query", "/tn", plan::WATCHDOG_TASK]),
        Ok((0, _))
    ) {
        let _ = win::run_hidden(&schtasks, &["/delete", "/tn", plan::WATCHDOG_TASK, "/f"]);
    }
}

fn write_uninstall_entry(layout: &Layout, size: u64) -> Result<(), String> {
    for (name, value) in plan::uninstall_entry(layout, VERSION, size) {
        win::write_value(plan::UNINSTALL_KEY, name, &value)?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("removing {}: {e}", path.display())),
    }
}

fn start_mochi(layout: &Layout) -> Result<(), String> {
    if win::is_elevated() {
        // Mochi must not inherit administrator rights, and Explorer opens the
        // Start menu entry at the user's normal ones.
        win::open_with_explorer(&layout.shortcut).map_err(|e| format!("starting Mochi: {e}"))?;
        return if win::wait_for(Duration::from_secs(10), win::mochi_running) {
            Ok(())
        } else {
            Err("Mochi did not come up within 10 seconds. Open Mochi from the Start menu.".into())
        };
    }
    match win::run_hidden(&layout.mochic(), &["start"]) {
        Ok((0, _)) => Ok(()),
        Ok((_, said)) => Err(said),
        Err(e) => Err(format!("starting Mochi: {e}")),
    }
}
