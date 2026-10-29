//! The error type every fallible operation in this crate returns.

use crate::model::WindowId;

/// Everything that can go wrong in pure window-management logic.
///
/// The variants are cheap to clone and compare so they can travel over the IPC
/// wire without losing information.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// There are no monitors, or the focused index points past the end.
    #[error("no monitor is focused")]
    NoFocusedMonitor,

    /// The focused monitor has no workspaces.
    #[error("monitor {0} has no focused workspace")]
    NoFocusedWorkspace(usize),

    /// An operation needed a focused window and there was none.
    #[error("no window is focused")]
    NoFocusedWindow,

    /// An operation needed a focused container and there was none.
    #[error("no container is focused")]
    NoFocusedContainer,

    /// The monitor index does not exist.
    #[error("monitor {0} does not exist")]
    MonitorNotFound(usize),

    /// The workspace index does not exist on that monitor.
    #[error("workspace {workspace} does not exist on monitor {monitor}")]
    WorkspaceNotFound {
        /// The monitor that was searched.
        monitor: usize,
        /// The workspace index that was asked for.
        workspace: usize,
    },

    /// No workspace anywhere carries that name.
    #[error("no workspace is named {0:?}")]
    WorkspaceNameNotFound(String),

    /// The window is not managed by any workspace.
    #[error("window {0} is not managed")]
    WindowNotFound(WindowId),

    /// Workspaces are created on demand, but only up to [`crate::MAX_WORKSPACES`].
    #[error("workspace index {0} is out of range, at most {max} workspaces are supported", max = crate::MAX_WORKSPACES)]
    WorkspaceIndexOutOfRange(usize),

    /// The operation only makes sense with more than one of something.
    #[error("{0}")]
    NothingToDo(&'static str),

    /// A string could not be parsed into one of the small enums.
    #[error("{value:?} is not a valid {kind}")]
    Parse {
        /// The kind of value that was expected, for example `direction`.
        kind: &'static str,
        /// The string that could not be parsed.
        value: String,
    },

    /// A rule carried a pattern that is not a valid regular expression.
    #[error("invalid regular expression {pattern:?}: {message}")]
    InvalidRegex {
        /// The pattern as written in the config.
        pattern: String,
        /// The compiler's complaint.
        message: String,
    },

    /// A rule carried an empty identifier, which names no window and would be
    /// a wildcard for the substring strategies.
    #[error("a {kind} rule has an empty id, which would match every window")]
    EmptyRuleId {
        /// The piece of window metadata the rule looks at.
        kind: crate::rules::ApplicationIdentifier,
    },

    /// A `scratchpads` entry had no name, so no command could ever reach it.
    #[error("a scratchpad has an empty name, so no command can reach it")]
    EmptyScratchpadName,

    /// JSON could not be turned into a config or a rule set.
    #[error("invalid json: {0}")]
    Json(String),
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(plain_json_error(&error.to_string()))
    }
}

/// A serde message with the Rust words in it said the way a config file says
/// them: `i32` is a whole number, a sequence is a list, a map is an object.
#[must_use]
pub fn plain_json_error(message: &str) -> String {
    const PHRASES: [(&str, &str); 9] = [
        (
            "EOF while parsing a value",
            "the file ends where a value should be",
        ),
        (
            "EOF while parsing an object",
            "the file ends before the closing }",
        ),
        (
            "EOF while parsing a list",
            "the file ends before the closing ]",
        ),
        (
            "EOF while parsing a string",
            "the file ends inside a string",
        ),
        ("expected ident", "expected true, false or null"),
        ("invalid type: map", "invalid type: an object"),
        ("invalid type: sequence", "invalid type: a list"),
        ("expected a sequence", "expected a list"),
        ("expected a map", "expected an object"),
    ];
    let mut out = message.replace("floating point `", "decimal number `");
    for (from, to) in PHRASES {
        out = out.replace(from, to);
    }
    let mut plain = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(at) = rest.find("expected ") {
        let (head, tail) = rest.split_at(at + "expected ".len());
        plain.push_str(head);
        let word_end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        let word = &tail[..word_end];
        rest = &tail[word_end..];
        match word {
            "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => plain.push_str("a whole number"),
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => {
                plain.push_str("a whole number of 0 or more");
            }
            "f32" | "f64" => plain.push_str("a number"),
            "struct" => {
                plain.push_str("an object");
                // The Rust name of the struct follows, which the file never spells.
                let name_end = rest
                    .trim_start()
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .map_or(rest.len(), |end| end + rest.len() - rest.trim_start().len());
                rest = &rest[name_end..];
            }
            _ => plain.push_str(word),
        }
    }
    plain.push_str(rest);
    plain
}

/// A [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
