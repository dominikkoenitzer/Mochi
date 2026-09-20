//! Starting and stopping the daemon and the hotkey daemon.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

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

/// Starts the daemon and waits until it answers on its pipe.
///
/// Success is this daemon answering, not any daemon: two `mochic start` at once
/// both spawn, the single instance mutex refuses the loser, and the loser would
/// otherwise see the winner's pipe and report a start that never happened.
pub fn start_daemon(args: &[String]) -> Result<()> {
    let Some(exe) = find_executable("mochi") else {
        bail!("mochi.exe is neither next to mochic nor on PATH");
    };
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
            let code = status.code().map_or_else(
                || "an unknown code".to_owned(),
                |code| format!("code {code}"),
            );
            if mochi_client::is_running() {
                bail!(
                    "mochi (pid {pid}) exited with {code}; another mochi already holds {}, so nothing was started here",
                    mochi_client::PIPE_NAME
                );
            }
            bail!(
                "mochi (pid {pid}) exited with {code} without opening {}, check the log in %LOCALAPPDATA%\\mochi",
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
        "mochi (pid {pid}) is running but did not open {} within {} seconds, check the log in %LOCALAPPDATA%\\mochi",
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
}
