//! One daemon per session.
//!
//! Two window managers fighting over the same windows is the fastest way to
//! lose a desktop, so startup takes a named mutex first. `Local\` scope, not
//! `Global\`: a global object needs a privilege a normal user account does not
//! have, and one manager per interactive session is exactly the right rule.

use anyhow::{Result, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

/// Name of the mutex that marks a running daemon.
pub const MUTEX_NAME: &str = r"Local\mochi-single-instance";

/// Holds the mutex for as long as the daemon runs.
pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    /// Takes the mutex, or fails if another daemon already has it.
    pub fn acquire() -> Result<Self> {
        Self::acquire_named(MUTEX_NAME)
    }

    /// Same as [`SingleInstance::acquire`] with an explicit name, for tests.
    pub fn acquire_named(name: &str) -> Result<Self> {
        let handle = unsafe { CreateMutexW(None, true, &HSTRING::from(name)) }?;
        // CreateMutexW succeeds either way; the last error says who won.
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already {
            let _ = unsafe { CloseHandle(handle) };
            bail!("another mochi is already running in this session");
        }
        Ok(Self { handle })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_acquire_is_refused_and_the_slot_frees_on_drop() {
        let name = format!(r"Local\mochi-test-{}", std::process::id());
        let first = SingleInstance::acquire_named(&name).expect("first should win");
        assert!(
            SingleInstance::acquire_named(&name).is_err(),
            "second must be refused"
        );
        drop(first);
        SingleInstance::acquire_named(&name).expect("the name should be free again");
    }
}
