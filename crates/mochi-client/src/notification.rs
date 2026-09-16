//! Events the daemon pushes to subscribers.
//!
//! One JSON object per line on the subscriber pipe, same framing as commands.

use serde::{Deserialize, Serialize};

/// A window as seen by a subscriber. Deliberately tiny: bars want a title and
/// an exe, nothing more. The full picture is in [`Notification::state`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowRef {
    /// Raw `HWND` value as a signed integer, so it survives JSON.
    pub hwnd: i64,
    /// Window title at the time of the event.
    pub title: String,
    /// File name of the owning process, for example `firefox.exe`.
    pub exe: String,
}

impl WindowRef {
    /// Builds a reference from its three parts.
    pub fn new(hwnd: i64, title: impl Into<String>, exe: impl Into<String>) -> Self {
        Self {
            hwnd,
            title: title.into(),
            exe: exe.into(),
        }
    }
}

/// Why a session changed. Mirrors the `WM_WTSSESSION_CHANGE` codes Mochi cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionChangeKind {
    /// The session was locked.
    Lock,
    /// The session was unlocked.
    Unlock,
    /// The console session was connected to this session.
    ConsoleConnect,
    /// The console session was disconnected from this session.
    ConsoleDisconnect,
    /// The machine is resuming from sleep or hibernation.
    Resume,
}

/// The thing that happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum NotificationEvent {
    /// The foreground window changed.
    FocusChange {
        /// The window that took focus, if it could still be read.
        window: Option<WindowRef>,
    },
    /// A different workspace became visible on a monitor.
    WorkspaceChange {
        /// Zero-based monitor index.
        monitor: usize,
        /// Zero-based workspace index.
        workspace: usize,
        /// Workspace name, if it has one.
        name: Option<String>,
    },
    /// The layout of a workspace changed.
    LayoutChange {
        /// Zero-based monitor index.
        monitor: usize,
        /// Zero-based workspace index.
        workspace: usize,
        /// The new layout, kebab-case.
        layout: String,
    },
    /// Mochi started managing a window.
    Manage {
        /// The window that is now managed.
        window: WindowRef,
    },
    /// Mochi stopped managing a window: it closed, was cloaked away or a rule hit.
    Unmanage {
        /// The window that is no longer managed.
        window: WindowRef,
    },
    /// The monitor set, a resolution, a work area or a DPI changed.
    MonitorsChanged {
        /// How many monitors there are now.
        count: usize,
    },
    /// The configuration file was re-read.
    Reload {
        /// Path that was read.
        path: String,
    },
    /// Management was paused or resumed.
    Pause {
        /// True while Mochi is paused.
        paused: bool,
    },
    /// The Windows session was locked, unlocked or resumed.
    SessionChange {
        /// What happened to the session.
        kind: SessionChangeKind,
    },
    /// The daemon is shutting down. Always the last notification on a pipe.
    Stop,
}

/// One line on a subscriber pipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// What happened.
    #[serde(flatten)]
    pub event: NotificationEvent,
    /// The full state after the event, when the subscriber asked for it.
    /// Omitted by default because it is large.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

impl Notification {
    /// A notification without a state snapshot.
    pub fn new(event: NotificationEvent) -> Self {
        Self { event, state: None }
    }

    /// Attaches a state snapshot.
    pub fn with_state(mut self, state: serde_json::Value) -> Self {
        self.state = Some(state);
        self
    }
}

impl From<NotificationEvent> for Notification {
    fn from(event: NotificationEvent) -> Self {
        Self::new(event)
    }
}
