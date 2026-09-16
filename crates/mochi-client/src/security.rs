//! A per-user security descriptor for Mochi's named pipes.
//!
//! `CreateNamedPipeW` with a null `lpSecurityAttributes` hands the pipe the
//! default security descriptor, and Microsoft documents that as: full control
//! for `LocalSystem`, administrators and the creator owner, plus **read access
//! for the `Everyone` group and the anonymous account**.
//!
//! Mochi is a per-user daemon. Another interactive session on the same machine
//! has no business reading the notification stream, occupying command pipe
//! instances, or making the daemon spawn a connection thread per handle it
//! opens. So every pipe is created with an explicit descriptor instead:
//!
//! ```text
//! D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;<the SID this process runs as>)
//! ```
//!
//! `D:P` is a protected DACL, so nothing is inherited on top of those three
//! entries. Only `SYSTEM`, the local administrators group and the account the
//! daemon runs as get access; every other account, including a second user
//! logged into the same machine, is denied by the absence of an entry.
//!
//! Administrators are on the list because they can take ownership of the object
//! anyway; leaving them out buys nothing and breaks tooling.

use std::io;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// An owned security descriptor plus the `SECURITY_ATTRIBUTES` that points at it.
///
/// Dropping it frees the descriptor, so it has to outlive the
/// `CreateNamedPipeW` call that uses it.
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

// The descriptor is a plain heap block that is only read by the kernel during
// the create call, and nothing in this type has interior mutability.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Builds a descriptor that grants access to this account only.
    ///
    /// See the module documentation for the exact DACL.
    pub fn current_user_only() -> io::Result<Self> {
        let sddl = format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{})",
            current_user_sid()?
        );
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the SDDL string is NUL terminated by HSTRING and outlives the
        // call; the descriptor is an out parameter that we own afterwards.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl.as_str()),
                SDDL_REVISION_1,
                &raw mut descriptor,
                None,
            )
        }
        .map_err(io::Error::other)?;

        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }

    /// The pointer to hand to `CreateNamedPipeW`.
    ///
    /// Valid for as long as `self` is alive and not moved out of.
    pub const fn attributes(&self) -> *const SECURITY_ATTRIBUTES {
        &raw const self.attributes
    }
}

impl std::fmt::Debug for PipeSecurity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PipeSecurity(current user only)")
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_invalid() {
            // SAFETY: ConvertStringSecurityDescriptorToSecurityDescriptorW
            // allocates with LocalAlloc and documents LocalFree as the way back.
            let _ = unsafe { LocalFree(Some(HLOCAL(self.descriptor.0))) };
        }
    }
}

/// The string form of the SID this process runs as, for example `S-1-5-21-...`.
fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo handle that never needs closing.
    let process = unsafe { GetCurrentProcess() };
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) }.map_err(io::Error::other)?;
    let token = OwnedToken(token);

    // First call sizes the buffer, second fills it.
    let mut needed = 0u32;
    let sized = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::other(match sized {
            Err(e) => format!("GetTokenInformation could not size TokenUser: {e}"),
            Ok(()) => "GetTokenInformation reported a zero sized TokenUser".to_owned(),
        }));
    }

    // Over-aligned so the TOKEN_USER prefix of the buffer is well aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(io::Error::other)?;

    // SAFETY: the call above filled the buffer with a TOKEN_USER followed by the
    // SID it points at, and the buffer outlives the read below.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut text) }.map_err(io::Error::other)?;
    // SAFETY: ConvertSidToStringSidW returned a NUL terminated LocalAlloc string.
    let sid = unsafe { text.to_string() }.map_err(io::Error::other);
    let _ = unsafe { LocalFree(Some(HLOCAL(text.as_ptr().cast()))) };
    sid
}

/// Closes an access token however the enclosing function returns.
struct OwnedToken(HANDLE);

impl Drop for OwnedToken {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcessToken and is not shared.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_user_sid_looks_like_a_sid() {
        let sid = current_user_sid().expect("this process has a token");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert!(sid.len() > 8, "{sid}");
    }

    #[test]
    fn a_descriptor_is_built_and_freed() {
        let security = PipeSecurity::current_user_only().expect("descriptor");
        assert!(!security.attributes().is_null());
        // SAFETY: the pointer comes straight from the value above.
        let attributes = unsafe { &*security.attributes() };
        assert_eq!(
            attributes.nLength as usize,
            size_of::<SECURITY_ATTRIBUTES>()
        );
        assert!(!attributes.lpSecurityDescriptor.is_null());
        assert!(!attributes.bInheritHandle.as_bool());
        drop(security);
    }
}
