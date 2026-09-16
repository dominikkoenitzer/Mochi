//! A tiny blocking client for the daemon's command pipe.

use std::io::BufReader;

use crate::protocol;
use crate::{Command, Response};

/// Full path of the pipe the daemon listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\mochi";

/// Prefix every Mochi pipe shares. Subscriber pipes are `PIPE_PREFIX` + name.
pub const PIPE_PREFIX: &str = r"\\.\pipe\";

/// Everything that can go wrong while talking to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No pipe at [`PIPE_NAME`]: the daemon is not running.
    #[error("mochi is not running (no pipe at {PIPE_NAME})")]
    NotRunning,
    /// The daemon accepted the connection but never answered.
    #[error("mochi accepted the connection but closed it without answering")]
    NoResponse,
    /// The daemon answered with [`Response::Error`].
    #[error("{0}")]
    Daemon(String),
    /// Anything the operating system reported.
    #[error("pipe I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// `Result` alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// How long to keep retrying while the pipe exists but every instance is busy.
const BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(2000);

/// Sends one command to the daemon on [`PIPE_NAME`] and waits for the answer.
pub fn send(cmd: &Command) -> Result<Response> {
    send_to(PIPE_NAME, cmd)
}

/// Same as [`send`], against an explicit pipe path. Useful in tests.
pub fn send_to(pipe: &str, cmd: &Command) -> Result<Response> {
    let mut stream = connect(pipe)?;
    protocol::write_message(&mut stream, cmd)?;
    let mut reader = BufReader::new(stream);
    match protocol::read_message::<_, Response>(&mut reader)? {
        None => Err(Error::NoResponse),
        Some(Response::Error { message }) => Err(Error::Daemon(message)),
        Some(other) => Ok(other),
    }
}

/// True when the daemon's pipe exists and accepts connections.
pub fn is_running() -> bool {
    match connect(PIPE_NAME) {
        Ok(_) => true,
        Err(Error::NotRunning) => false,
        // Busy or permission denied still means something is listening.
        Err(_) => true,
    }
}

/// Opens a duplex connection to a named pipe, retrying while all instances are busy.
fn connect(pipe: &str) -> Result<std::fs::File> {
    let deadline = std::time::Instant::now() + BUSY_RETRY;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
        {
            Ok(file) => return Ok(file),
            Err(e) => {
                // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND: nobody is listening.
                if matches!(e.raw_os_error(), Some(2) | Some(3)) {
                    return Err(Error::NotRunning);
                }
                // ERROR_PIPE_BUSY: every instance is taken, try again shortly.
                if e.raw_os_error() == Some(231) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                return Err(Error::Io(e));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_the_real_win32_spelling() {
        assert_eq!(PIPE_NAME, "\\\\.\\pipe\\mochi");
        assert!(PIPE_NAME.starts_with(PIPE_PREFIX));
    }

    #[test]
    fn missing_pipe_reports_not_running() {
        let err =
            send_to(r"\\.\pipe\mochi-test-definitely-not-there", &Command::State).unwrap_err();
        assert!(matches!(err, Error::NotRunning), "got {err:?}");
    }
}
