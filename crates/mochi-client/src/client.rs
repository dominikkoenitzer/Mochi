//! A tiny blocking client for the daemon's command pipe.

use std::io::BufReader;

use windows::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

use crate::protocol;
use crate::{Command, Response};

/// Full path of the pipe the daemon listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\mochi";

/// Prefix every Mochi pipe shares. Subscriber pipes are `PIPE_PREFIX` + name.
pub const PIPE_PREFIX: &str = r"\\.\pipe\";

/// The daemon's own pipe name, without the prefix.
///
/// A subscriber may not claim it: writing the notification stream into the
/// command channel makes the daemon talk to itself.
pub const PIPE_SUFFIX: &str = "mochi";

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
///
/// The quality of service is pinned to `SECURITY_IDENTIFICATION`. Without
/// `SECURITY_SQOS_PRESENT` Windows hands the pipe server a full impersonation
/// token, so a process that squatted the pipe name before the daemon started
/// could call `ImpersonateNamedPipeClient` and act as the user for as long as
/// the connection lasts. `FILE_FLAG_FIRST_PIPE_INSTANCE` proves the name was
/// free for the daemon; it does nothing for whoever connects.
fn connect(pipe: &str) -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    let sqos = (SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION).0;
    let deadline = std::time::Instant::now() + BUSY_RETRY;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(sqos)
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

    /// A pipe server that tries to impersonate whoever connects to it, and
    /// reports the impersonation level it managed to reach.
    ///
    /// This is what a process that squatted `\\.\pipe\mochi` before the daemon
    /// started would do: `FILE_FLAG_FIRST_PIPE_INSTANCE` protects the daemon,
    /// not the client, so the only thing standing between the squatter and the
    /// user's token is the quality of service the client asks for.
    #[test]
    fn a_pipe_server_cannot_impersonate_the_client() {
        use std::io::{Read, Write};
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
        use windows::Win32::Security::{
            GetTokenInformation, RevertToSelf, SECURITY_IMPERSONATION_LEVEL,
            SecurityIdentification, TOKEN_QUERY, TokenImpersonationLevel,
        };
        use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
        use windows::Win32::System::Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE,
            PIPE_TYPE_BYTE, PIPE_WAIT,
        };
        use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};
        use windows::core::HSTRING;

        let pipe = format!(r"\\.\pipe\mochi-test-sqos-{}", std::process::id());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (level_tx, level_rx) = std::sync::mpsc::channel::<i32>();

        let squatter = {
            let pipe = pipe.clone();
            std::thread::spawn(move || {
                let handle = unsafe {
                    CreateNamedPipeW(
                        &HSTRING::from(pipe.as_str()),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        1,
                        4096,
                        4096,
                        0,
                        None,
                    )
                };
                assert!(handle != INVALID_HANDLE_VALUE, "could not create {pipe}");
                let mut server = unsafe { std::fs::File::from_raw_handle(handle.0) };
                ready_tx.send(()).unwrap();

                unsafe { ConnectNamedPipe(HANDLE(handle.0), None) }.ok();
                // A local client can be impersonated straight after the
                // connect, but reading first is what a real squatter would do.
                let mut byte = [0u8; 1];
                let _ = server.read(&mut byte);

                let mut level = -1i32;
                if unsafe { ImpersonateNamedPipeClient(HANDLE(handle.0)) }.is_ok() {
                    let mut token = HANDLE::default();
                    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &mut token) }
                        .is_ok()
                    {
                        let mut got = SECURITY_IMPERSONATION_LEVEL::default();
                        let mut len = 0u32;
                        if unsafe {
                            GetTokenInformation(
                                token,
                                TokenImpersonationLevel,
                                Some((&raw mut got).cast()),
                                std::mem::size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                                &mut len,
                            )
                        }
                        .is_ok()
                        {
                            level = got.0;
                        }
                        let _ = unsafe { windows::Win32::Foundation::CloseHandle(token) };
                    }
                    let _ = unsafe { RevertToSelf() };
                }
                let _ = level_tx.send(level);
            })
        };

        ready_rx.recv().unwrap();
        let mut client = connect(&pipe).expect("the squatted pipe should accept a connection");
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();

        let level = level_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the squatter never reported an impersonation level");
        drop(client);
        squatter.join().unwrap();

        assert!(
            level <= SecurityIdentification.0,
            "a pipe server reached impersonation level {level}; the client must ask for \
             SECURITY_IDENTIFICATION so a squatted pipe can only read who we are, never act as us"
        );
    }

    #[test]
    fn missing_pipe_reports_not_running() {
        let err =
            send_to(r"\\.\pipe\mochi-test-definitely-not-there", &Command::State).unwrap_err();
        assert!(matches!(err, Error::NotRunning), "got {err:?}");
    }
}
