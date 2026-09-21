//! A dry run must be safe to start while a real daemon is managing the desktop.
//!
//! `mochi --dry-run` exists to show a user what Mochi would make of their
//! machine, and its own help text promises it is safe to run next to another
//! window manager. It used to be refused by the single-instance lock, so the
//! one diagnostic a new user has was unavailable exactly when they wanted it:
//! while something was running.
//!
//! Three things had to be true together, and this proves the first directly.
//! It takes no single-instance lock, it takes no control pipe (the name is
//! shared, so a `mochic` command would reach the observer instead of the
//! daemon) and it binds no keys (they would be taken from the daemon that is
//! actually managing the desktop).

#![cfg(windows)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the observer is given to get past the lock and read the desktop.
const SETTLE: Duration = Duration::from_secs(10);

#[test]
fn a_dry_run_starts_while_the_single_instance_lock_is_held() {
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_mochi"));
    if !binary.exists() {
        eprintln!("skipping: the mochi binary was not built");
        return;
    }
    if !mochi_testbed::monitors().is_ok_and(|m| !m.is_empty()) {
        eprintln!("skipping: no interactive desktop in this window station");
        return;
    }

    // Stand in for a daemon that is already managing the desktop. Held for the
    // whole test, so the observer has to get past it rather than around it.
    let _held = mochi::single_instance::SingleInstance::acquire().ok();

    // A config path that does not exist, so the test never reads the config of
    // whoever is running it and the defaults are exercised instead.
    let config = std::env::temp_dir().join(format!("mochi-dry-run-{}.json", std::process::id()));
    assert!(!config.exists(), "the test must not read a real config");

    let mut child = Command::new(&binary)
        .arg("--dry-run")
        .arg("--config")
        .arg(&config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the observer should start");

    // It runs until stopped, so give it long enough to have failed if it were
    // going to, then take it down and read what it said.
    let deadline = Instant::now() + SETTLE;
    let mut exited_early = None;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            exited_early = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if exited_early.is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();

    let mut output = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut output);
    }
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut output);
    }

    assert!(
        !output.contains("another mochi is already running"),
        "the dry run was refused by the single-instance lock:\n{output}"
    );
    if let Some(status) = exited_early {
        panic!("the dry run exited early with {status}:\n{output}");
    }
    assert!(
        output.contains("not taking the single-instance lock"),
        "the dry run did not say it was skipping the lock:\n{output}"
    );
    assert!(
        output.contains("no control pipe"),
        "a dry run must not listen on the shared control pipe:\n{output}"
    );
    assert!(
        output.contains("binds no keys"),
        "a dry run must not take the hotkeys from the running daemon:\n{output}"
    );
    assert!(
        output.contains("no window will be moved"),
        "the dry run did not announce that it writes nothing:\n{output}"
    );
}
