//! Starting and stopping the daemon and the hotkey daemon.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

/// `DETACHED_PROCESS`: the child gets no console and survives this one.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl-C in this console does not reach the child.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// How long `start` waits for the daemon to open its pipe.
const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Finds an executable next to `mochic` first, then on `PATH`.
///
/// The sibling lookup matters during development: a freshly built `mochic`
/// must start the `mochi` it was built with, not an older one on `PATH`.
pub fn find_executable(stem: &str) -> Option<PathBuf> {
    let file = format!("{stem}.exe");

    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
    {
        let sibling = dir.join(&file);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|candidate| candidate.is_file())
}

/// Spawns a process detached from this console.
///
/// The handle is returned rather than just the pid, because `start_daemon` has
/// to be able to ask whether this daemon is still alive.
fn spawn_detached(program: &PathBuf, args: &[String]) -> Result<Child> {
    use std::os::windows::process::CommandExt;

    Command::new(program)
        .args(args)
        // `DETACHED_PROCESS` denies the child a console but not the handles it
        // inherits, so without this the daemon would write its whole tracing
        // output down this terminal's stdout and stderr for the rest of its
        // life, and `mochic start > log.txt` would never finish because the
        // daemon still holds the write end of that file.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .with_context(|| format!("could not start {}", program.display()))
}

/// The outside world `start` works with, so the tests can stand in for it
/// without a single `mochi` being started.
pub trait Launch {
    /// True when a daemon answers on the control pipe.
    fn daemon_is_running(&self) -> bool;
    /// Starts the daemon detached and waits until it answers on its pipe.
    fn daemon(&self, args: &[String]) -> Result<()>;
    /// Runs `mochi` on this console and returns once it has exited.
    fn attached(&self, args: &[String]) -> Result<ExitStatus>;
}

/// The real processes and the real pipe.
pub struct System;

impl Launch for System {
    fn daemon_is_running(&self) -> bool {
        mochi_client::is_running()
    }

    fn daemon(&self, args: &[String]) -> Result<()> {
        start_daemon(args)
    }

    fn attached(&self, args: &[String]) -> Result<ExitStatus> {
        let exe = mochi_executable()?;
        // Same console, same output, same process group: the user reads the
        // dry run where they typed it, and Ctrl-C or closing the window reaches
        // it as well as this process.
        let mut child = Command::new(&exe)
            .args(args)
            .spawn()
            .with_context(|| format!("could not start {}", exe.display()))?;
        let pid = child.id();
        child
            .wait()
            .with_context(|| format!("could not wait for mochi (pid {pid})"))
    }
}

/// `mochic start`: the daemon, or with `dry_run` an observer on this console.
///
/// A dry run is never started detached. It opens no control pipe, so a
/// detached one could not be reached by anything: `start_daemon` waited for a
/// pipe that never came, reported a failure, and left the observer running
/// with no console, no pipe and no keys until somebody found it in Task
/// Manager. Run on this console it prints what it would do right here, Ctrl-C
/// ends it, and `start` returns only after it has exited. It also does not
/// look for a running daemon first, because a dry run is safe beside one.
///
/// # Errors
///
/// When `mochi` cannot be started, or a dry run ends with a failure.
pub fn start(launch: &impl Launch, args: &[String], dry_run: bool) -> Result<()> {
    if dry_run {
        let mut args = args.to_vec();
        args.insert(0, "--dry-run".to_owned());
        let status = launch.attached(&args)?;
        if !status.success() {
            bail!("the dry run exited with {}", exit_code(status));
        }
        return Ok(());
    }
    if launch.daemon_is_running() {
        println!("mochi is already running");
        return Ok(());
    }
    launch.daemon(args)
}

fn mochi_executable() -> Result<PathBuf> {
    find_executable("mochi").context("mochi.exe is neither next to mochic nor on PATH")
}

fn exit_code(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "an unknown code".to_owned(),
        |code| format!("code {code}"),
    )
}

/// Starts the daemon and waits until it answers on its pipe.
///
/// Success is this daemon answering, not any daemon: two `mochic start` at once
/// both spawn, the single instance mutex refuses the loser, and the loser would
/// otherwise see the winner's pipe and report a start that never happened.
fn start_daemon(args: &[String]) -> Result<()> {
    let exe = mochi_executable()?;
    let mut child = spawn_detached(&exe, args)?;
    let pid = child.id();

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    loop {
        // Asked before the pipe, so a daemon that was refused is reported as
        // the exit it is instead of costing the whole timeout, and instead of
        // being covered up by whichever daemon does hold the pipe.
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("could not check on mochi (pid {pid})"))?
        {
            let code = exit_code(status);
            if mochi_client::is_running() {
                bail!(
                    "mochi (pid {pid}) exited with {code}; another mochi already holds {}, so nothing was started here",
                    mochi_client::PIPE_NAME
                );
            }
            // `mochic check` before the log, on purpose. The daemon is started
            // detached with no stderr, so whatever reason it gave for giving up
            // went to a dated log file and never to this terminal. Far and away
            // the most common reason is a configuration file it could not read,
            // and `check` finds exactly that, offline, naming the file, the line
            // and the column.
            bail!(
                "mochi (pid {pid}) exited with {code} without opening {}.
Run `mochic check` to test your configuration, or read the log in %LOCALAPPDATA%\\mochi",
                mochi_client::PIPE_NAME
            );
        }
        if mochi_client::is_running() {
            // Printed only now: a line on stdout is what a script reads to
            // decide the start worked.
            println!("started {} (pid {pid})", exe.display());
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "mochi (pid {pid}) is running but did not open {} within {} seconds.
Run `mochic check` to test your configuration, or read the log in %LOCALAPPDATA%\\mochi",
        mochi_client::PIPE_NAME,
        START_TIMEOUT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_executable_is_not_found() {
        assert!(find_executable("mochi-definitely-not-installed").is_none());
    }

    #[test]
    fn the_running_test_binary_is_found_next_to_itself() {
        let current = std::env::current_exe().unwrap();
        let stem = current.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(find_executable(&stem), Some(current));
    }

    /// Stands in for the processes and the pipe, and writes down every call.
    struct Fake {
        running: bool,
        exit: u32,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(running: bool, exit: u32) -> Self {
            Self {
                running,
                exit,
                calls: std::cell::RefCell::default(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl Launch for Fake {
        fn daemon_is_running(&self) -> bool {
            self.calls.borrow_mut().push("pipe".to_owned());
            self.running
        }

        fn daemon(&self, args: &[String]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("detached {}", args.join(" ")));
            Ok(())
        }

        fn attached(&self, args: &[String]) -> Result<ExitStatus> {
            use std::os::windows::process::ExitStatusExt;
            self.calls
                .borrow_mut()
                .push(format!("attached {}", args.join(" ")));
            Ok(ExitStatus::from_raw(self.exit))
        }
    }

    fn args() -> Vec<String> {
        vec!["--config".to_owned(), r"D:\mochi.json".to_owned()]
    }

    #[test]
    fn a_dry_run_runs_on_this_console_and_is_waited_for() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), true).expect("the dry run should succeed");
        assert_eq!(
            fake.calls(),
            [r"attached --dry-run --config D:\mochi.json"],
            "a dry run started detached opens no pipe and is left running where nothing can reach it"
        );
    }

    #[test]
    fn a_dry_run_runs_beside_a_running_daemon() {
        let fake = Fake::new(true, 0);
        start(&fake, &args(), true).expect("the dry run should succeed");
        assert_eq!(
            fake.calls(),
            [r"attached --dry-run --config D:\mochi.json"],
            "a dry run is safe next to a daemon and must not be turned away by one"
        );
    }

    #[test]
    fn a_dry_run_that_fails_is_reported() {
        let fake = Fake::new(false, 2);
        let error = start(&fake, &args(), true).expect_err("an exit code 2 is not a success");
        assert!(error.to_string().contains("code 2"), "{error}");
        assert_eq!(fake.calls().len(), 1);
    }

    #[test]
    fn a_start_goes_detached_when_no_daemon_runs() {
        let fake = Fake::new(false, 0);
        start(&fake, &args(), false).expect("the start should succeed");
        assert_eq!(fake.calls(), ["pipe", r"detached --config D:\mochi.json"]);
    }

    #[test]
    fn a_start_next_to_a_running_daemon_starts_nothing() {
        let fake = Fake::new(true, 0);
        start(&fake, &args(), false).expect("already running is not an error");
        assert_eq!(fake.calls(), ["pipe"]);
    }
}
