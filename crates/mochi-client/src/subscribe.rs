//! Subscriber side of the notification protocol.
//!
//! The subscriber owns the pipe. It creates `\\.\pipe\<name>`, tells the daemon
//! about it with [`crate::Command::SubscribePipe`], and the daemon connects as a
//! client and writes one JSON notification per line. That way a bar can be
//! started before or after the daemon and neither has to guess.

use std::fs::File;
use std::io::BufReader;
use std::os::windows::io::{AsRawHandle, FromRawHandle};

use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::core::HSTRING;

use crate::client::{Error, PIPE_PREFIX, Result};
use crate::security::PipeSecurity;
use crate::{Command, Notification, protocol};

/// Buffer size handed to `CreateNamedPipeW` for a subscriber pipe.
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// A live subscription. Iterate it to receive [`Notification`]s.
///
/// Dropping it closes the pipe; the daemon notices on its next write and
/// forgets the subscriber. Call [`Subscription::unsubscribe`] to do it politely.
pub struct Subscription {
    name: String,
    reader: BufReader<File>,
    connected: bool,
}

impl Subscription {
    /// The pipe name without the `\\.\pipe\` prefix.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tells the daemon to stop writing to this pipe. Errors are returned, not logged.
    pub fn unsubscribe(&self) -> Result<()> {
        crate::send(&Command::UnsubscribePipe {
            name: self.name.clone(),
        })
        .map(|_| ())
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.reader.get_ref().as_raw_handle())
    }

    /// Blocks until the daemon is connected. Cheap no-op once connected.
    fn ensure_connected(&mut self) -> std::io::Result<()> {
        if self.connected {
            return Ok(());
        }
        // ERROR_PIPE_CONNECTED means the peer beat us to it, which is a success.
        match unsafe { ConnectNamedPipe(self.handle(), None) } {
            Ok(()) => {}
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
            Err(e) => return Err(std::io::Error::other(e)),
        }
        self.connected = true;
        Ok(())
    }

    /// Reads the next notification, blocking until one arrives.
    ///
    /// Returns `Ok(None)` only on an unrecoverable pipe error. A daemon restart
    /// is handled transparently: the pipe is recycled and we wait again.
    pub fn next_notification(&mut self) -> std::io::Result<Option<Notification>> {
        loop {
            self.ensure_connected()?;
            match protocol::read_message::<_, Notification>(&mut self.reader) {
                Ok(Some(n)) => return Ok(Some(n)),
                Ok(None) => {
                    // The daemon went away. Recycle the pipe and wait for the next one.
                    self.connected = false;
                    unsafe { DisconnectNamedPipe(self.handle()) }.map_err(std::io::Error::other)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    // One bad line should not kill a bar. Skip it.
                    continue;
                }
                Err(e) if e.raw_os_error() == Some(109) => {
                    // ERROR_BROKEN_PIPE, same handling as a clean EOF.
                    self.connected = false;
                    let _ = unsafe { DisconnectNamedPipe(self.handle()) };
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Iterator for Subscription {
    type Item = std::io::Result<Notification>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_notification().transpose()
    }
}

/// Creates `\\.\pipe\<name>` and registers it with the daemon.
///
/// Fails with [`Error::NotRunning`] if the daemon is not up, after the pipe has
/// already been created; the pipe is dropped in that case.
pub fn subscribe(name: &str) -> Result<Subscription> {
    let subscription = create_pipe(name)?;
    crate::send(&Command::SubscribePipe {
        name: name.to_owned(),
    })?;
    Ok(subscription)
}

/// Creates the subscriber pipe without telling the daemon about it.
///
/// Useful when a bar wants to be ready before the daemon starts: create the
/// pipe, then send [`Command::SubscribePipe`] whenever the daemon appears.
pub fn create_pipe(name: &str) -> Result<Subscription> {
    validate_pipe_name(name)?;
    let path = format!("{PIPE_PREFIX}{name}");
    // A notification stream says which windows the user has open, so the pipe
    // carries an explicit descriptor rather than the default one, which grants
    // read access to Everyone. `FILE_FLAG_FIRST_PIPE_INSTANCE` makes a second
    // creator of the same name fail instead of quietly sharing it.
    let security = PipeSecurity::current_user_only().map_err(Error::Io)?;
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(path.as_str()),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(security.attributes()),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    // The File takes ownership and closes the handle on drop.
    let file = unsafe { File::from_raw_handle(handle.0) };
    Ok(Subscription {
        name: name.to_owned(),
        reader: BufReader::new(file),
        connected: false,
    })
}

/// Rejects pipe names that would escape the `\\.\pipe\` namespace.
///
/// The daemon calls this on the name a client sends it, not just the subscriber
/// on its own name. `\\.\pipe\` is a device namespace, so a name containing a
/// separator turns into a path to something that is not a pipe at all, and the
/// daemon would write its notification stream straight into it.
pub fn validate_pipe_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 200
        || name
            .chars()
            .any(|c| c == '\\' || c == '/' || c == ':' || c.is_control());
    if bad {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("`{name}` is not a usable pipe name"),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_separators_are_rejected() {
        for bad in [
            "",
            ".",
            "..",
            "a\\b",
            "a/b",
            "a:b",
            "a\nb",
            // The reason this matters: `\\.\pipe\` + this is a writable file.
            r"..\..\C:\Users\someone\notes.txt",
            r"\\server\pipe\elsewhere",
        ] {
            assert!(
                validate_pipe_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(validate_pipe_name("mochi-bar").is_ok());
        assert!(validate_pipe_name(&"n".repeat(200)).is_ok());
        assert!(validate_pipe_name(&"n".repeat(201)).is_err());
    }

    #[test]
    fn a_pipe_can_be_created_and_dropped() {
        let name = format!("mochi-test-{}", std::process::id());
        let sub = create_pipe(&name).expect("create");
        assert_eq!(sub.name(), name);
        drop(sub);
        // Creating it again proves the handle was really closed.
        create_pipe(&name).expect("recreate");
    }
}
