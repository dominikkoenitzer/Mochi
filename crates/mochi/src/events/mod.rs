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
    /// A thread panicked, so the daemon is no longer whole.
    Panicked,
    /// The IPC acceptor could not carry on.
    ///
    /// Without a pipe there is no `mochic stop`, and `mochic stop` is how the
    /// desktop gets its windows back. A daemon that keeps tiling with no way
    /// to reach it is strictly worse than one that exits, because exiting runs
    /// the restore path.
    IpcLost,
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

/// How long a producer thread is given to stop before it is left behind.
pub(crate) const STOP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Waits for a producer thread to finish, and gives up rather than hanging.
///
/// `std` has no timed join, and an unbounded one here is not safe. Each
/// producer is stopped with `PostThreadMessageW`, which FAILS once the target
/// thread's message queue is full, and a window storming location-change events
/// does exactly that. The call then logs the failure and the join blocks on a
/// thread nothing will ever wake.
///
/// The desktop is already restored by the time any of this runs, so the only
/// thing left to lose is the process exiting. Losing that is not harmless: the
/// single-instance lock is held until the process dies, so a daemon stuck here
/// cannot be stopped with `mochic stop` (the pipe is already down) and a fresh
/// one refuses to start. The user is left killing it from Task Manager.
///
/// Dropping the handle detaches the thread; process exit takes it with it.
pub(crate) fn join_before(
    handle: std::thread::JoinHandle<()>,
    deadline: std::time::Duration,
    what: &str,
) {
    let until = std::time::Instant::now() + deadline;
    while !handle.is_finished() {
        if std::time::Instant::now() >= until {
            tracing::error!(
                thread = what,
                "did not stop within {:?}, leaving it behind and shutting down anyway",
                deadline
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if handle.join().is_err() {
        tracing::error!(thread = what, "the thread panicked");
    } else {
        tracing::debug!(thread = what, "stopped");
    }
}

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

    #[test]
    fn a_thread_that_will_not_stop_is_left_behind_rather_than_hung_on() {
        // The failure this exists for: `PostThreadMessageW` fails once the
        // target thread's message queue is full, which a window storming
        // location-change events does, so the thread is never woken and an
        // unbounded join never returns. The desktop is already restored by
        // then, so the cost of hanging is a daemon that cannot be stopped and
        // holds the single-instance lock so a new one cannot start.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let stuck = std::thread::spawn(move || {
            // Blocks until the sender is dropped, which this test never does
            // until afterwards. Stands in for a thread with a wedged queue.
            let _ = rx.recv();
        });

        let started = std::time::Instant::now();
        join_before(stuck, std::time::Duration::from_millis(200), "stuck");
        let waited = started.elapsed();

        assert!(
            waited < std::time::Duration::from_secs(2),
            "join_before hung for {waited:?} instead of giving up"
        );
        assert!(
            waited >= std::time::Duration::from_millis(150),
            "it gave up before the deadline it was given: {waited:?}"
        );
        drop(tx);
    }

    #[test]
    fn a_thread_that_stops_is_waited_for_properly() {
        // And the ordinary case still joins rather than detaching: a producer
        // that stops must be fully finished before its windows are touched.
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&done);
        let quick = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        join_before(quick, std::time::Duration::from_secs(5), "quick");
        assert!(
            done.load(std::sync::atomic::Ordering::SeqCst),
            "join_before returned before the thread had finished"
        );
    }
}
