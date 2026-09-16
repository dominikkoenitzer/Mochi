//! Starting and stopping the daemon and the hotkey daemon.

use std::path::PathBuf;
use std::process::Command;

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
fn spawn_detached(program: &PathBuf, args: &[String]) -> Result<u32> {
    use std::os::windows::process::CommandExt;

    let child = Command::new(program)
        .args(args)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .with_context(|| format!("could not start {}", program.display()))?;
    Ok(child.id())
}

/// Starts the daemon and waits until it answers on its pipe.
pub fn start_daemon(args: &[String]) -> Result<()> {
    let Some(exe) = find_executable("mochi") else {
        bail!("mochi.exe is neither next to mochic nor on PATH");
    };
    let pid = spawn_detached(&exe, args)?;
    println!("started {} (pid {pid})", exe.display());

    let deadline = std::time::Instant::now() + START_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if mochi_client::is_running() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!(
        "mochi was started but did not open {} within {} seconds, check the log in %LOCALAPPDATA%\\mochi",
        mochi_client::PIPE_NAME,
        START_TIMEOUT.as_secs()
    )
}

/// Starts whkd if it is on `PATH`. A missing whkd is reported, not fatal.
pub fn start_whkd() -> Result<()> {
    let Some(exe) = find_executable("whkd") else {
        bail!("whkd is not on PATH, hotkeys will not work");
    };
    let pid = spawn_detached(&exe, &[])?;
    println!("started {} (pid {pid})", exe.display());
    Ok(())
}

/// Stops whkd.
///
/// whkd has no control channel, so there is nothing to ask politely and
/// `taskkill` is the only way to bring it down.
pub fn stop_whkd() -> Result<()> {
    let status = Command::new("taskkill")
        .args(["/F", "/IM", "whkd.exe"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("could not run taskkill")?;
    match status.code() {
        Some(0) => {
            println!("stopped whkd");
            Ok(())
        }
        // 128 is taskkill's "no such process", which is not a failure here.
        Some(128) => {
            println!("whkd was not running");
            Ok(())
        }
        other => bail!("taskkill exited with {other:?}"),
    }
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
