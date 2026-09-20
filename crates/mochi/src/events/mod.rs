//! Everything that can wake the window manager loop.
//!
//! Four producers feed one `std::sync::mpsc` channel:
//!
//! * [`winevent`] installs `SetWinEventHook` hooks on their own thread,
//! * [`message_window`] owns a hidden window for display, DPI and session news,
//! * [`hotkey`] holds a low-level keyboard hook and reports bound key presses,
//! * [`crate::ipc`] turns pipe traffic into [`Event::Command`].
//!
//! Nothing else is allowed to touch the state, which is why there is not a
//! single lock in the daemon.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use mochi_client::{Command, Response, SessionChangeKind};

use crate::platform::Hwnd;

pub mod hotkey;
pub mod message_window;
pub mod mouse;
pub mod winevent;

/// Raw `EVENT_*` identifiers from `winuser.h` that Mochi hooks.
pub mod raw {
    /// A window was created.
    pub const EVENT_OBJECT_CREATE: u32 = 0x8000;
    /// A window was destroyed.
    pub const EVENT_OBJECT_DESTROY: u32 = 0x8001;
    /// A window became visible.
    pub const EVENT_OBJECT_SHOW: u32 = 0x8002;
    /// A window became hidden.
    pub const EVENT_OBJECT_HIDE: u32 = 0x8003;
    /// A window moved or resized. Very chatty.
    pub const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
    /// A window title changed.
    pub const EVENT_OBJECT_NAMECHANGE: u32 = 0x800C;
    /// DWM started hiding a window.
    pub const EVENT_OBJECT_CLOAKED: u32 = 0x8017;
    /// DWM stopped hiding a window.
    pub const EVENT_OBJECT_UNCLOAKED: u32 = 0x8018;
    /// The foreground window changed.
    pub const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
    /// The user grabbed a title bar or a resize grip.
    pub const EVENT_SYSTEM_MOVESIZESTART: u32 = 0x000A;
    /// The user let go of a title bar or a resize grip.
    pub const EVENT_SYSTEM_MOVESIZEEND: u32 = 0x000B;
    /// A window started minimizing.
    pub const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
    /// A window finished restoring from minimized.
    pub const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;

    /// `OBJID_WINDOW`: the event is about the window itself, not a control in it.
    pub const OBJID_WINDOW: i32 = 0;
    /// `CHILDID_SELF`: the event is about the object, not one of its children.
    pub const CHILDID_SELF: i32 = 0;
}

/// Something that happened to a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowEventKind {
    /// `EVENT_OBJECT_CREATE`.
    Created,
    /// `EVENT_OBJECT_DESTROY`.
    Destroyed,
    /// `EVENT_OBJECT_SHOW`.
    Shown,
    /// `EVENT_OBJECT_HIDE`.
    Hidden,
    /// `EVENT_OBJECT_CLOAKED`.
    Cloaked,
    /// `EVENT_OBJECT_UNCLOAKED`.
    Uncloaked,
    /// `EVENT_SYSTEM_FOREGROUND`.
    Foreground,
    /// `EVENT_SYSTEM_MINIMIZESTART`.
    MinimizeStart,
    /// `EVENT_SYSTEM_MINIMIZEEND`.
    MinimizeEnd,
    /// `EVENT_SYSTEM_MOVESIZESTART`.
    MoveSizeStart,
    /// `EVENT_SYSTEM_MOVESIZEEND`.
    MoveSizeEnd,
    /// `EVENT_OBJECT_LOCATIONCHANGE`.
    LocationChange,
    /// `EVENT_OBJECT_NAMECHANGE`.
    NameChange,
}

impl WindowEventKind {
    /// Maps a raw `EVENT_*` value, or `None` for events Mochi does not care about.
    pub const fn from_raw(event: u32) -> Option<Self> {
        Some(match event {
            raw::EVENT_OBJECT_CREATE => Self::Created,
            raw::EVENT_OBJECT_DESTROY => Self::Destroyed,
            raw::EVENT_OBJECT_SHOW => Self::Shown,
            raw::EVENT_OBJECT_HIDE => Self::Hidden,
            raw::EVENT_OBJECT_CLOAKED => Self::Cloaked,
            raw::EVENT_OBJECT_UNCLOAKED => Self::Uncloaked,
            raw::EVENT_SYSTEM_FOREGROUND => Self::Foreground,
            raw::EVENT_SYSTEM_MINIMIZESTART => Self::MinimizeStart,
            raw::EVENT_SYSTEM_MINIMIZEEND => Self::MinimizeEnd,
            raw::EVENT_SYSTEM_MOVESIZESTART => Self::MoveSizeStart,
            raw::EVENT_SYSTEM_MOVESIZEEND => Self::MoveSizeEnd,
            raw::EVENT_OBJECT_LOCATIONCHANGE => Self::LocationChange,
            raw::EVENT_OBJECT_NAMECHANGE => Self::NameChange,
            _ => return None,
        })
    }

    /// A short name for logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Destroyed => "destroyed",
            Self::Shown => "shown",
            Self::Hidden => "hidden",
            Self::Cloaked => "cloaked",
            Self::Uncloaked => "uncloaked",
            Self::Foreground => "foreground",
            Self::MinimizeStart => "minimize-start",
            Self::MinimizeEnd => "minimize-end",
            Self::MoveSizeStart => "move-size-start",
            Self::MoveSizeEnd => "move-size-end",
            Self::LocationChange => "location-change",
            Self::NameChange => "name-change",
        }
    }

    /// True for the flood of events that fire while a window is being dragged.
    ///
    /// The loop logs these at trace level so a debug log stays readable.
    pub const fn is_noisy(self) -> bool {
        matches!(self, Self::LocationChange)
    }
}

impl std::fmt::Display for WindowEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Something that happened to the set of monitors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MonitorEventKind {
    /// `WM_DISPLAYCHANGE`: resolution or colour depth changed.
    DisplayChange,
    /// `WM_SETTINGCHANGE` with `SPI_SETWORKAREA`: an appbar moved.
    WorkAreaChange,
    /// `WM_DPICHANGED`.
    DpiChange,
}

impl MonitorEventKind {
    /// A short name for logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DisplayChange => "display-change",
            Self::WorkAreaChange => "work-area-change",
            Self::DpiChange => "dpi-change",
        }
    }
}

/// Why the daemon is shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// A client sent `stop`.
    Command,
    /// Ctrl-C or a console close.
    Signal,
}

/// The one-shot channel a command handler answers on.
///
/// Dropping it without answering makes the client see a closed connection,
/// which is why [`Reply::send`] takes `self`: you answer exactly once.
#[derive(Debug)]
pub struct Reply(Sender<Response>);

impl Reply {
    /// Creates a reply handle and the receiver the IPC thread waits on.
    pub fn channel() -> (Self, Receiver<Response>) {
        let (tx, rx) = channel();
        (Self(tx), rx)
    }

    /// Answers the client. A client that hung up is not an error.
    pub fn send(self, response: Response) {
        if self.0.send(response).is_err() {
            tracing::debug!("a client hung up before its answer was ready");
        }
    }
}

/// Anything the window manager loop reacts to.
#[derive(Debug)]
pub enum Event {
    /// A `SetWinEventHook` event about a top-level window.
    Window {
        /// What happened.
        kind: WindowEventKind,
        /// The window it happened to.
        hwnd: Hwnd,
    },
    /// The monitor layout changed.
    Monitors(MonitorEventKind),
    /// The Windows session was locked, unlocked or resumed.
    Session(SessionChangeKind),
    /// The mouse moved onto a different window, for `focus-follows-mouse`.
    MouseFocus {
        /// The window under the cursor.
        hwnd: Hwnd,
    },
    /// The configuration file on disk changed.
    ConfigChanged(PathBuf),
    /// A bound key was pressed. The press was swallowed before it reached the
    /// desktop, so this event is the only thing that still knows about it.
    Hotkey {
        /// The keys that were held, for the log.
        trigger: mochi_hotkey::Trigger,
        /// What the user bound them to.
        action: Box<mochi_hotkey::Action>,
    },
    /// A client sent a command and is waiting for the answer.
    Command {
        /// What was asked.
        command: Box<Command>,
        /// Where to send the answer.
        reply: Reply,
    },
    /// Wind down.
    Shutdown(ShutdownReason),
}

impl Event {
    /// Wraps a command and its reply channel.
    pub fn command(command: Command, reply: Reply) -> Self {
        Self::Command {
            command: Box::new(command),
            reply,
        }
    }
}

/// The sending half of the one channel the loop listens on.
pub type EventSender = Sender<Event>;
/// The receiving half of the one channel the loop listens on.
pub type EventReceiver = Receiver<Event>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hooked_event_maps_to_a_kind() {
        let pairs = [
            (raw::EVENT_OBJECT_CREATE, WindowEventKind::Created),
            (raw::EVENT_OBJECT_DESTROY, WindowEventKind::Destroyed),
            (raw::EVENT_OBJECT_SHOW, WindowEventKind::Shown),
            (raw::EVENT_OBJECT_HIDE, WindowEventKind::Hidden),
            (raw::EVENT_OBJECT_CLOAKED, WindowEventKind::Cloaked),
            (raw::EVENT_OBJECT_UNCLOAKED, WindowEventKind::Uncloaked),
            (raw::EVENT_SYSTEM_FOREGROUND, WindowEventKind::Foreground),
            (
                raw::EVENT_SYSTEM_MINIMIZESTART,
                WindowEventKind::MinimizeStart,
            ),
            (raw::EVENT_SYSTEM_MINIMIZEEND, WindowEventKind::MinimizeEnd),
            (
                raw::EVENT_SYSTEM_MOVESIZESTART,
                WindowEventKind::MoveSizeStart,
            ),
            (raw::EVENT_SYSTEM_MOVESIZEEND, WindowEventKind::MoveSizeEnd),
            (
                raw::EVENT_OBJECT_LOCATIONCHANGE,
                WindowEventKind::LocationChange,
            ),
            (raw::EVENT_OBJECT_NAMECHANGE, WindowEventKind::NameChange),
        ];
        for (raw_event, kind) in pairs {
            assert_eq!(
                WindowEventKind::from_raw(raw_event),
                Some(kind),
                "0x{raw_event:04x} did not map to {kind}"
            );
        }
    }

    #[test]
    fn events_in_a_hooked_range_that_we_ignore_map_to_none() {
        // EVENT_SYSTEM_DIALOGSTART and EVENT_OBJECT_FOCUS fall inside the ranges
        // the hooks cover but are of no use to a tiling manager.
        assert_eq!(WindowEventKind::from_raw(0x0010), None);
        assert_eq!(WindowEventKind::from_raw(0x8005), None);
        assert_eq!(WindowEventKind::from_raw(0xFFFF), None);
    }

    #[test]
    fn only_location_change_is_marked_noisy() {
        assert!(WindowEventKind::LocationChange.is_noisy());
        assert!(!WindowEventKind::Foreground.is_noisy());
        assert!(!WindowEventKind::Destroyed.is_noisy());
    }

    #[test]
    fn a_reply_carries_one_response() {
        let (reply, rx) = Reply::channel();
        reply.send(Response::Ok);
        assert_eq!(rx.recv().unwrap(), Response::Ok);
    }

    #[test]
    fn a_dropped_reply_closes_the_channel() {
        let (reply, rx) = Reply::channel();
        drop(reply);
        assert!(rx.recv().is_err());
    }
}
