//! The control channel between two invocations of `mochi-testwin`.
//!
//! The first invocation, `spawn`, becomes the *host*: it owns the windows and
//! their message loops and stays in the foreground of its console. A second
//! invocation needs a way to reach it, and there are two mechanisms here on
//! purpose:
//!
//! * A **named pipe**, `\\.\pipe\mochi-testbed`, carries the commands that only
//!   the host can carry out: adding windows to the running process and shutting
//!   the whole host down. One request is one line of JSON, one response is one
//!   line of JSON, and the connection is closed afterwards.
//! * A **session file**, `%TEMP%\mochi-testbed\session.json`, records the pid
//!   and the pipe name so a second invocation can tell a running host from a
//!   dead one without blocking on a connect. A file left behind by a crashed
//!   host is detected by a ping and removed.
//!
//! Everything else, listing windows and moving, resizing, renaming, focusing,
//! minimizing or closing a single window, goes straight through Win32 on the
//! window handle. Those are cross-process calls by nature, they are exactly the
//! calls a window manager makes, and routing them through the pipe would only
//! test the pipe. That also means those subcommands work against windows the
//! current shell never spawned.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE,
    FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows::core::PCWSTR;

use crate::TEST_WINDOW_CLASS;
use crate::error::{Error, Result, hresult_from_win32};
use crate::event::now_ms;
use crate::info::TestWindowInfo;
use crate::wide::to_wide;

/// The pipe a running host listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\mochi-testbed";

/// How long the client waits for a busy pipe, in milliseconds.
const CONNECT_TIMEOUT_MS: u32 = 2_000;

/// Buffer size of both directions of the pipe.
const PIPE_BUFFER: u32 = 64 * 1024;

/// What one invocation asks a running host to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Is anybody there? Used to tell a live host from a stale session file.
    Ping,
    /// List the test windows the host can see.
    List,
    /// Add more windows to the running host.
    Spawn {
        /// Everything the new batch should look like.
        #[serde(flatten)]
        options: SpawnRequest,
    },
    /// Close every window and let the host process exit.
    CloseAll,
}

/// The spawn options as they travel over the pipe.
///
/// A mirror of [`crate::SpawnOptions`] rather than the type itself: everything
/// here has to survive JSON, and every field added later defaults, so an older
/// client still talks to a newer host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// How many windows.
    pub count: u32,
    /// Which monitor, in `EnumDisplayMonitors` order.
    pub monitor: usize,
    /// Title prefix for the new windows.
    pub title_prefix: String,
    /// Exact top left corner, or `None` to stagger the batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<(i32, i32)>,
    /// Exact size in physical pixels, or `None` for the DPI scaled default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<(i32, i32)>,
    /// Minimum size the windows defend on `WM_GETMINMAXINFO`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<(i32, i32)>,
    /// Create owned popups.
    #[serde(default)]
    pub owned: bool,
    /// Create the windows without a title.
    #[serde(default)]
    pub no_title: bool,
}

impl From<&crate::SpawnOptions> for SpawnRequest {
    fn from(options: &crate::SpawnOptions) -> Self {
        Self {
            count: options.count,
            monitor: options.monitor,
            title_prefix: options.title_prefix.clone(),
            position: options.position,
            size: options.size,
            min_size: options.min_size,
            owned: options.owned,
            no_title: options.no_title,
        }
    }
}

impl SpawnRequest {
    /// The options a host spawns from. A host always emits events.
    #[must_use]
    pub fn into_options(self) -> crate::SpawnOptions {
        crate::SpawnOptions {
            count: self.count,
            monitor: self.monitor,
            title_prefix: self.title_prefix,
            emit_events: true,
            size: self.size,
            position: self.position,
            min_size: self.min_size,
            owned: self.owned,
            no_title: self.no_title,
        }
    }
}

/// What the host answers. Always one line of JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// True when the request was carried out.
    pub ok: bool,
    /// Why it was not, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The host process id.
    #[serde(default)]
    pub pid: u32,
    /// The windows the request produced or found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<TestWindowInfo>,
}

impl Response {
    /// A successful, empty answer.
    #[must_use]
    pub fn ok() -> Self {
        Self {
            ok: true,
            pid: std::process::id(),
            ..Self::default()
        }
    }

    /// A failure with a reason.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(reason.into()),
            pid: std::process::id(),
            ..Self::default()
        }
    }

    /// Attaches a window list to a successful answer.
    #[must_use]
    pub fn with_windows(mut self, windows: Vec<TestWindowInfo>) -> Self {
        self.windows = windows;
        self
    }
}

/// What the session file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Process id of the host.
    pub pid: u32,
    /// The pipe it listens on.
    pub pipe: String,
    /// The window class it spawns, so an unrelated tool can check the contract.
    pub class: String,
    /// When the host started, in milliseconds since the Unix epoch.
    pub started_ms: u64,
}

impl Session {
    /// The session of the current process.
    #[must_use]
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            pipe: PIPE_NAME.to_string(),
            class: TEST_WINDOW_CLASS.to_string(),
            started_ms: now_ms(),
        }
    }
}

/// `%TEMP%\mochi-testbed\session.json`.
///
/// Under `%TEMP%` rather than anywhere near the repository: scratch files must
/// not land in the user's project or download folders.
#[must_use]
pub fn session_path() -> PathBuf {
    std::env::temp_dir()
        .join("mochi-testbed")
        .join("session.json")
}

/// Writes the session file, creating the directory if needed.
///
/// # Errors
/// When the file cannot be written.
pub fn write_session(session: &Session) -> Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(session)?)?;
    Ok(())
}

/// Reads the session file, if there is one that parses.
#[must_use]
pub fn read_session() -> Option<Session> {
    let text = std::fs::read_to_string(session_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Removes the session file. Missing is success.
pub fn remove_session() {
    let _ = std::fs::remove_file(session_path());
}

/// The session of a host that actually answers, or `None`.
///
/// A session file whose host is gone is removed on the way out, so a crashed
/// host never blocks the next run.
#[must_use]
pub fn live_host() -> Option<Session> {
    let session = read_session()?;
    match send(&Request::Ping) {
        Ok(response) if response.ok => Some(session),
        _ => {
            remove_session();
            None
        }
    }
}

/// Sends one request to the running host and waits for its answer.
///
/// # Errors
/// [`Error::NotFound`] when no host is listening, or an IO error from the pipe.
pub fn send(request: &Request) -> Result<Response> {
    let pipe = OwnedHandle(open_client()?);
    write_line(pipe.0, &serde_json::to_string(request)?)?;
    let line = read_line(pipe.0)?;
    if line.trim().is_empty() {
        return Err(Error::other("the host closed the pipe without answering"));
    }
    Ok(serde_json::from_str(&line)?)
}

/// Starts the control server on a background thread.
///
/// The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` before
/// this returns, so a second host on the same desktop fails here instead of
/// quietly serving half the requests. The thread itself is never joined: it
/// blocks in `ConnectNamedPipe`, and the process exit that follows
/// `close --all` takes it down with everything else.
///
/// # Errors
/// When the pipe cannot be created, which normally means another host owns it.
pub fn serve<H>(handler: H) -> Result<()>
where
    H: Fn(Request) -> Response + Send + 'static,
{
    let first = create_instance(true)?;
    std::thread::Builder::new()
        .name("mochi-testbed-control".to_string())
        .spawn(move || {
            let mut instance = first;
            loop {
                serve_one(instance.0, &handler);
                match create_instance(false) {
                    Ok(next) => instance = next,
                    Err(_) => return,
                }
            }
        })
        .map_err(Error::Io)?;
    Ok(())
}

fn serve_one<H: Fn(Request) -> Response>(pipe: HANDLE, handler: &H) {
    let connected = unsafe { ConnectNamedPipe(pipe, None) };
    if let Err(e) = connected {
        // ERROR_PIPE_CONNECTED means the client beat us to it, which is fine.
        if e.code().0 != hresult_from_win32(ERROR_PIPE_CONNECTED.0) {
            return;
        }
    }

    let response = match read_line(pipe) {
        Ok(line) => match serde_json::from_str::<Request>(line.trim()) {
            Ok(request) => handler(request),
            Err(e) => Response::failed(format!("cannot parse the request: {e}")),
        },
        Err(e) => Response::failed(e.to_string()),
    };

    if let Ok(text) = serde_json::to_string(&response) {
        let _ = write_line(pipe, &text);
    }
    unsafe {
        let _ = FlushFileBuffers(pipe);
        let _ = DisconnectNamedPipe(pipe);
    }
}

fn create_instance(first: bool) -> Result<OwnedHandle> {
    let name = to_wide(PIPE_NAME);
    let mode = if first {
        // Refuses to start when another host already owns the name.
        PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_DUPLEX
    };
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(Error::last("CreateNamedPipeW"));
    }
    Ok(OwnedHandle(handle))
}

fn open_client() -> Result<HANDLE> {
    let name = to_wide(PIPE_NAME);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(u64::from(CONNECT_TIMEOUT_MS));
    loop {
        let opened = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match opened {
            Ok(handle) => return Ok(handle),
            Err(e) if e.code().0 == hresult_from_win32(ERROR_FILE_NOT_FOUND.0) => {
                return Err(Error::not_found(
                    "a running mochi-testwin host; start one with `mochi-testwin spawn`",
                ));
            }
            Err(e) if e.code().0 == hresult_from_win32(ERROR_PIPE_BUSY.0) => {
                if std::time::Instant::now() >= deadline {
                    return Err(Error::timeout("the host control pipe to free up"));
                }
                unsafe {
                    let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), 100);
                }
            }
            Err(e) => return Err(Error::win32("CreateFileW", &e)),
        }
    }
}

fn write_line(pipe: HANDLE, text: &str) -> Result<()> {
    let mut payload = text.as_bytes().to_vec();
    payload.push(b'\n');
    let mut written = 0;
    unsafe { WriteFile(pipe, Some(&payload), Some(&mut written), None) }
        .map_err(|e| Error::win32("WriteFile", &e))
}

fn read_line(pipe: HANDLE) -> Result<String> {
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let mut read = 0;
        unsafe { ReadFile(pipe, Some(&mut chunk), Some(&mut read), None) }
            .map_err(|e| Error::win32("ReadFile", &e))?;
        if read == 0 {
            break;
        }
        collected.extend_from_slice(&chunk[..read as usize]);
        if collected.contains(&b'\n') {
            break;
        }
        if collected.len() > 4 * PIPE_BUFFER as usize {
            return Err(Error::other("control message is implausibly long"));
        }
    }
    Ok(String::from_utf8_lossy(&collected).trim_end().to_string())
}

/// A handle that closes itself, so no error path leaks a pipe instance.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// The server thread hands the raw handle to blocking Win32 calls; it is only
// ever touched by that one thread.
unsafe impl Send for OwnedHandle {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_tagged_objects() {
        assert_eq!(
            serde_json::to_string(&Request::Ping).unwrap(),
            r#"{"cmd":"ping"}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::CloseAll).unwrap(),
            r#"{"cmd":"close_all"}"#
        );
        let spawn = Request::Spawn {
            options: SpawnRequest {
                count: 2,
                monitor: 1,
                title_prefix: "MochiTest".into(),
                ..SpawnRequest::default()
            },
        };
        let json = serde_json::to_string(&spawn).unwrap();
        assert!(
            json.starts_with(r#"{"cmd":"spawn","count":2,"monitor":1"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), spawn);

        // A client that knows none of the placement fields still parses.
        let older = r#"{"cmd":"spawn","count":1,"monitor":0,"title_prefix":"MochiTest"}"#;
        assert!(matches!(
            serde_json::from_str::<Request>(older).unwrap(),
            Request::Spawn { .. }
        ));
    }

    #[test]
    fn a_failure_carries_the_reason() {
        let response = Response::failed("no such monitor");
        assert!(!response.ok);
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("no such monitor"), "{json}");
        assert!(!json.contains("windows"), "{json}");
    }

    #[test]
    fn the_session_file_lives_under_temp() {
        let path = session_path();
        assert!(path.starts_with(std::env::temp_dir()), "{path:?}");
        assert!(path.ends_with("session.json"));
    }

    #[test]
    fn the_session_describes_the_contract() {
        let session = Session::current();
        assert_eq!(session.class, TEST_WINDOW_CLASS);
        assert_eq!(session.pipe, PIPE_NAME);
        assert_eq!(session.pid, std::process::id());
    }

    #[test]
    fn talking_to_a_host_that_is_not_there_says_so() {
        // No host is expected during a unit test run; if one happens to be up,
        // the call succeeds instead, which is equally fine.
        match send(&Request::Ping) {
            Ok(response) => assert!(response.ok || response.error.is_some()),
            Err(e) => {
                let message = e.to_string();
                assert!(
                    message.contains("host") || message.contains("pipe"),
                    "{message}"
                );
            }
        }
    }
}
