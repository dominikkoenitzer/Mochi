//! One error type for the whole crate.
//!
//! Hand written rather than derived: the testbed keeps its dependency list down
//! to the four crates it cannot avoid, so that it still builds when the rest of
//! the workspace does not.

/// The result type used throughout the testbed.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while driving test windows.
#[derive(Debug)]
pub enum Error {
    /// A Win32 call failed. `call` is the function name, `code` the `HRESULT`.
    Win32 {
        /// Name of the Win32 function that failed.
        call: &'static str,
        /// The `HRESULT` as returned by the windows crate.
        code: i32,
        /// The system message for that code.
        message: String,
    },
    /// A file or pipe operation failed.
    Io(std::io::Error),
    /// A control message could not be encoded or decoded.
    Json(serde_json::Error),
    /// Something did not happen inside the timeout.
    Timeout(String),
    /// A window, monitor or running host was not found.
    NotFound(String),
    /// Anything else, with a sentence explaining it.
    Other(String),
}

impl Error {
    /// Convenience constructor for the `Other` variant.
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }

    /// Convenience constructor for the `NotFound` variant.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    /// Convenience constructor for the `Timeout` variant.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Win32 {
                call,
                code,
                message,
            } => write!(f, "{call} failed: {message} (0x{code:08x})"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Json(e) => write!(f, "json: {e}"),
            Self::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Self::NotFound(what) => write!(f, "not found: {what}"),
            Self::Other(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

#[cfg(windows)]
impl Error {
    /// Wraps a windows crate error together with the call that produced it.
    pub fn win32(call: &'static str, source: &windows::core::Error) -> Self {
        Self::Win32 {
            call,
            code: source.code().0,
            message: source.message(),
        }
    }

    /// The last thread error, for the Win32 calls that only return a `BOOL`.
    pub fn last(call: &'static str) -> Self {
        Self::win32(call, &windows::core::Error::from_thread())
    }
}

/// `HRESULT_FROM_WIN32`, which the windows crate does not expose as a function.
/// Needed to compare a `windows::core::Error` against a plain `ERROR_*` code.
#[cfg(windows)]
pub(crate) const fn hresult_from_win32(code: u32) -> i32 {
    ((code & 0x0000_ffff) | 0x8007_0000) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_like_sentences() {
        assert_eq!(
            Error::timeout("the rect to settle").to_string(),
            "timed out waiting for the rect to settle"
        );
        assert_eq!(
            Error::not_found("hwnd 0x1234").to_string(),
            "not found: hwnd 0x1234"
        );
        assert_eq!(
            Error::other("no host is running").to_string(),
            "no host is running"
        );
    }

    #[test]
    #[cfg(windows)]
    fn win32_codes_turn_into_the_hresults_the_api_returns() {
        // ERROR_PIPE_BUSY and ERROR_CLASS_ALREADY_EXISTS.
        assert_eq!(hresult_from_win32(231) as u32, 0x8007_00e7);
        assert_eq!(hresult_from_win32(1410) as u32, 0x8007_0582);
    }

    #[test]
    fn a_win32_failure_shows_the_call_and_the_code() {
        let e = Error::Win32 {
            call: "CreateWindowExW",
            code: 0x8007_0005_u32 as i32,
            message: "Access is denied.".into(),
        };
        assert_eq!(
            e.to_string(),
            "CreateWindowExW failed: Access is denied. (0x80070005)"
        );
    }
}
