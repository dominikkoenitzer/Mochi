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

    /// JSON could not be turned into a config or a rule set.
    #[error("invalid json: {0}")]
    Json(String),
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

/// A [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
