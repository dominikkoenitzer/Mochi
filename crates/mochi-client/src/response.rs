//! What the daemon writes back for every command it receives.

use serde::{Deserialize, Serialize};

/// The daemon's answer to a [`crate::Command`].
///
/// Exactly one response is written per command, on its own line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "kebab-case")]
pub enum Response {
    /// The command was accepted and carried out. Nothing to report.
    Ok,
    /// The command failed. `message` is meant to be shown to a human.
    Error {
        /// Human readable failure reason.
        message: String,
    },
    /// The full daemon state, as produced by `{"cmd":"state"}`.
    State {
        /// Opaque state document. Its shape is owned by the daemon.
        state: serde_json::Value,
    },
    /// The answer to a `{"cmd":"query"}`.
    Query {
        /// A single JSON scalar: number, string or boolean.
        answer: serde_json::Value,
    },
    /// What Mochi makes of one window, as produced by `{"cmd":"why"}`.
    Why {
        /// Opaque explanation document. Its shape is owned by the daemon; the
        /// paragraph `mochic why` prints is only a rendering of it.
        why: serde_json::Value,
    },
    /// The bindings the hotkey daemon holds, as produced by `{"cmd":"hotkeys"}`.
    Hotkeys {
        /// Opaque bindings document. Its shape is owned by the daemon; the
        /// table `mochic hotkeys` prints is only a rendering of it.
        hotkeys: serde_json::Value,
    },
}

impl Response {
    /// Builds an [`Response::Error`] from anything printable.
    pub fn error(message: impl std::fmt::Display) -> Self {
        Self::Error {
            message: message.to_string(),
        }
    }

    /// True for everything but [`Response::Error`].
    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Error { .. })
    }

    /// The error message, if this is an error.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Error { message } => Some(message),
            _ => None,
        }
    }
}
