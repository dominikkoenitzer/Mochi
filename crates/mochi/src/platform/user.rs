//! The desktop user's ordinary rights, for a Mochi that runs as administrator.
//!
//! Running elevated is what lets Mochi move windows that belong to elevated
//! programs, but the daemon still lives in the user's profile. It writes its
//! log and its off-screen record under `%LOCALAPPDATA%`, and a hotkey line
//! starts whatever program the user's hotkey file names. Both are the user's
//! to change. A link planted in the profile could steer an administrator's
//! write at a file only administrators may change, and a line in the hotkey
//! file would start its program as administrator.
//!
//! So both happen with the rights of the desktop shell instead. Files are
//! written while the thread impersonates the shell's user, which makes a
//! planted link lead to a refusal like any other, and programs are started
//! with the shell as their parent, which hands them the shell's token. When
//! the shell runs elevated itself, as it does with UAC turned off, or when
//! Mochi does not run elevated, there is no boundary to keep and both run as
//! they always did.

use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, EqualSid, GetTokenInformation, RevertToSelf, SecurityImpersonation,
    TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_USER, TokenImpersonation, TokenUser,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcess, OpenProcessToken, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROCESS_CREATE_PROCESS,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOEXW, SetThreadToken,
    UpdateProcThreadAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
use windows::core::{PCWSTR, PWSTR};

use super::win32::{process_is_elevated, we_are_elevated};

/// What [`as_user`] found out so far.
#[derive(Clone, Copy)]
enum Known {
    /// Nothing yet, or no shell the last time it looked.
    Nothing,
    /// Mochi is not elevated, or the shell is elevated too: writes run as
    /// they are.
    NotNeeded,
    /// The shell user's token, duplicated for impersonation. Kept for the
    /// life of the daemon: Explorer can restart, and the rights it stood for
    /// do not change when it does.
    Token(isize),
}

static KNOWN: Mutex<Known> = Mutex::new(Known::Nothing);

/// Whether the "no unelevated shell" warning has been written yet.
static WARNED: Mutex<bool> = Mutex::new(false);

/// What a program started on Mochi's behalf runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rights {
    /// Mochi's own: it is not elevated, or the shell is elevated as well.
    Own,
    /// The desktop shell's, through the shell process with this id.
    Shell(u32),
    /// Mochi is elevated and there is no desktop shell to borrow lesser
    /// rights from, so nothing should be started at all.
    None,
}

/// The process behind the desktop shell's window, or `None` without a shell.
fn shell_pid() -> Option<u32> {
    // SAFETY: both calls tolerate a missing shell; the pid is a local.
    let pid = unsafe {
        let shell = GetShellWindow();
        if shell.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(shell, Some(&raw mut pid));
        pid
    };
    (pid != 0).then_some(pid)
}

/// The rights a program started now would get, worked out from what is
/// running at this moment.
pub fn launch_rights() -> Rights {
    decide(
        we_are_elevated(),
        shell_pid().map(|pid| (pid, process_is_elevated(pid))),
    )
}

/// [`launch_rights`] without the system calls: `shell` is the shell's process
/// id and whether it runs elevated, `None` for no shell at all.
fn decide(elevated: bool, shell: Option<(u32, Option<bool>)>) -> Rights {
    if !elevated {
        return Rights::Own;
    }
    match shell {
        Some((_, Some(true))) => Rights::Own,
        Some((pid, _)) => Rights::Shell(pid),
        None => Rights::None,
    }
}

/// Runs `f` on this thread with the desktop user's ordinary rights when Mochi
/// is elevated and the shell is not, and simply runs it otherwise.
///
/// For file writes into the user's profile. Should no unelevated shell be
/// there to take the rights from, `f` runs with Mochi's own and that is
/// logged once: a missing log file or a lost off-screen record would cost
/// more than the write.
pub fn as_user<T>(f: impl FnOnce() -> T) -> T {
    let Some(token) = user_token() else {
        return f();
    };

    /// Goes back to the process's own rights, also when `f` panics.
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // SAFETY: undoes the SetThreadToken below on this same thread.
            let _ = unsafe { RevertToSelf() };
        }
    }

    // SAFETY: the token is a live impersonation token owned by TOKEN.
    if let Err(e) = unsafe { SetThreadToken(None, Some(HANDLE(token as *mut c_void))) } {
        tracing::warn!(error = %e, "could not take the desktop user's rights for a write");
        return f();
    }
    let _revert = Revert;
    f()
}

/// The impersonation token [`as_user`] uses, taken from the shell the first
/// time there is a boundary to keep.
fn user_token() -> Option<isize> {
    if !we_are_elevated() {
        return None;
    }
    let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    match *known {
        Known::Token(token) => return Some(token),
        Known::NotNeeded => return None,
        Known::Nothing => {}
    }
    match launch_rights() {
        Rights::Own => {
            *known = Known::NotNeeded;
            None
        }
        Rights::Shell(pid) => match duplicate_shell_token(pid) {
            Ok(token) => {
                *known = Known::Token(token);
                tracing::info!(
                    shell = pid,
                    "running as administrator: files in the profile are written with the desktop user's rights"
                );
                Some(token)
            }
            Err(e) => {
                warn_once(&format!(
                    "the desktop shell's rights could not be taken: {e}"
                ));
                None
            }
        },
        Rights::None => {
            warn_once("there is no desktop shell to take the user's rights from");
            None
        }
    }
}

fn warn_once(why: &str) {
    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if !*warned {
        *warned = true;
        tracing::warn!(
            why,
            "running as administrator: files in the profile are written with administrator rights"
        );
    }
}

/// An impersonation copy of the shell's token, refused when the shell runs
/// as another account than Mochi does.
fn duplicate_shell_token(pid: u32) -> windows::core::Result<isize> {
    // SAFETY: every handle opened here is closed on every path.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
        let mut shell = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_DUPLICATE | TOKEN_QUERY, &raw mut shell);
        let _ = CloseHandle(process);
        opened?;
        let mut own = HANDLE::default();
        let same = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut own).is_ok() && {
            let same = same_user(shell, own);
            let _ = CloseHandle(own);
            same
        };
        if !same {
            let _ = CloseHandle(shell);
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_ACCESSDENIED,
                "the desktop belongs to another account than Mochi",
            ));
        }
        let mut copy = HANDLE::default();
        let duplicated = DuplicateTokenEx(
            shell,
            TOKEN_IMPERSONATE | TOKEN_QUERY,
            None,
            SecurityImpersonation,
            TokenImpersonation,
            &raw mut copy,
        );
        let _ = CloseHandle(shell);
        duplicated?;
        Ok(copy.0 as isize)
    }
}

/// Whether two tokens belong to the same account.
fn same_user(a: HANDLE, b: HANDLE) -> bool {
    let (Some(first), Some(second)) = (token_user(a), token_user(b)) else {
        return false;
    };
    // SAFETY: both buffers start with a TOKEN_USER whose SID points into the
    // same buffer, and both outlive the comparison.
    unsafe {
        let first = &*first.as_ptr().cast::<TOKEN_USER>();
        let second = &*second.as_ptr().cast::<TOKEN_USER>();
        EqualSid(first.User.Sid, second.User.Sid).is_ok()
    }
}

/// The `TOKEN_USER` of a token, in a buffer aligned for it.
fn token_user(token: HANDLE) -> Option<Vec<u64>> {
    let mut needed = 0u32;
    // SAFETY: the first call only asks for the size.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer holds at least `needed` bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            needed,
            &raw mut needed,
        )
    }
    .ok()?;
    Some(buffer)
}

/// Starts `command_line` with the shell process `shell` as its parent, so it
/// runs with the shell's token, with no console window and nothing waited for.
///
/// Returns the new process's id. The program is found the way
/// `CreateProcessW` finds the first word of a command line.
pub fn spawn_as_shell(shell: u32, command_line: &str) -> std::io::Result<u32> {
    let handle = spawn_with_parent(shell, command_line)?;
    // SAFETY: the handle was just returned by CreateProcessW and is ours.
    let pid = unsafe { windows::Win32::System::Threading::GetProcessId(handle) };
    let _ = unsafe { CloseHandle(handle) };
    Ok(pid)
}

/// [`spawn_as_shell`], handing back the process handle instead of its id.
fn spawn_with_parent(parent_pid: u32, command_line: &str) -> std::io::Result<HANDLE> {
    // SAFETY: the parent handle, the attribute list and the process handles
    // are released on every path; the list buffer outlives CreateProcessW.
    unsafe {
        let parent = OpenProcess(PROCESS_CREATE_PROCESS, false, parent_pid)
            .map_err(std::io::Error::other)?;
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &raw mut size);
        let mut buffer = vec![0u64; size.div_ceil(8)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast::<c_void>());
        let result = (|| {
            InitializeProcThreadAttributeList(Some(list), 1, None, &raw mut size)
                .map_err(std::io::Error::other)?;
            let updated = UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
                Some((&raw const parent).cast::<c_void>()),
                size_of::<HANDLE>(),
                None,
                None,
            );
            let created = updated.and_then(|()| {
                let mut info = STARTUPINFOEXW::default();
                info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                info.lpAttributeList = list;
                let mut wide: Vec<u16> = command_line.encode_utf16().chain(Some(0)).collect();
                let mut process = PROCESS_INFORMATION::default();
                CreateProcessW(
                    PCWSTR::null(),
                    Some(PWSTR(wide.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
                    None,
                    PCWSTR::null(),
                    (&raw const info.StartupInfo).cast(),
                    &raw mut process,
                )
                .map(|()| {
                    let _ = CloseHandle(process.hThread);
                    process.hProcess
                })
            });
            DeleteProcThreadAttributeList(list);
            created.map_err(std::io::Error::other)
        })();
        let _ = CloseHandle(parent);
        result
    }
}

/// One argument quoted the way the C runtime splits a command line back up.
pub fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return argument.to_owned();
    }
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use windows::Win32::Security::{TOKEN_ELEVATION, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    use super::*;

    /// Whether a token says it is elevated.
    fn elevated(token: HANDLE) -> bool {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        // SAFETY: the buffer is a TOKEN_ELEVATION of the size passed.
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some((&raw mut elevation).cast::<c_void>()),
                size_of::<TOKEN_ELEVATION>() as u32,
                &raw mut returned,
            )
        }
        .expect("a token says whether it is elevated");
        elevation.TokenIsElevated != 0
    }

    /// The rights this thread acts with right now.
    fn thread_is_elevated() -> bool {
        let mut token = HANDLE::default();
        // SAFETY: the token is closed below; without a thread token the
        // thread acts with the process's.
        unsafe {
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, true, &raw mut token).is_err() {
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token)
                    .expect("the process token opens");
            }
            let answer = elevated(token);
            let _ = CloseHandle(token);
            answer
        }
    }

    /// What a write or a program started on Mochi's behalf should run as.
    fn expected() -> bool {
        match launch_rights() {
            Rights::Own | Rights::None => we_are_elevated(),
            Rights::Shell(_) => false,
        }
    }

    #[test]
    fn only_an_elevated_mochi_beside_an_unelevated_shell_borrows_the_shells_rights() {
        assert_eq!(decide(false, Some((7, Some(false)))), Rights::Own);
        assert_eq!(decide(false, None), Rights::Own);
        assert_eq!(decide(true, Some((7, Some(false)))), Rights::Shell(7));
        // UAC turned off: the shell is as elevated as Mochi, nothing to keep.
        assert_eq!(decide(true, Some((7, Some(true)))), Rights::Own);
        // A shell that will not say is treated as the lesser one.
        assert_eq!(decide(true, Some((7, None))), Rights::Shell(7));
        assert_eq!(decide(true, None), Rights::None);
    }

    #[test]
    fn a_write_runs_with_the_rights_of_the_desktop() {
        let inside = as_user(thread_is_elevated);
        assert_eq!(inside, expected(), "{:?}", launch_rights());
        assert_eq!(
            thread_is_elevated(),
            we_are_elevated(),
            "the thread did not go back to its own rights"
        );
    }

    #[test]
    fn a_program_started_through_the_shell_gets_the_shells_rights() {
        let Rights::Shell(shell) = launch_rights() else {
            // Not elevated, or no boundary on this machine: the path that
            // matters is the one below, and it only exists with a boundary.
            return;
        };
        let child = spawn_with_parent(shell, "cmd.exe /C exit 0").expect("cmd starts");
        let mut token = HANDLE::default();
        // SAFETY: the handles are ours and closed below.
        unsafe {
            OpenProcessToken(child, TOKEN_QUERY, &raw mut token).expect("the child's token opens");
            assert!(!elevated(token), "the child inherited administrator rights");
            let _ = CloseHandle(token);
            let _ = CloseHandle(child);
        }
    }

    #[test]
    fn arguments_are_quoted_the_way_the_runtime_splits_them() {
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument(""), r#""""#);
        assert_eq!(quote_argument("a b"), r#""a b""#);
        assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_argument(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_argument(r#"x\"y"#), r#""x\\\"y""#);
        assert_eq!(quote_argument(r"no\space"), r"no\space");
    }
}
