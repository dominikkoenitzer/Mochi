//! Disposable Win32 windows for testing Mochi on a real desktop.
//!
//! The problem this solves: Mochi is developed on a machine where another
//! window manager is running and where the user's real applications must not be
//! disturbed. The testbed spawns plain Win32 windows that carry
//! `WS_EX_TOOLWINDOW` and the class [`TEST_WINDOW_CLASS`]. Tool windows are
//! skipped by every window manager that follows the usual manageability rules,
//! and they appear neither in the taskbar nor in Alt-Tab, so
//! a batch of them can sit on a working desktop and be tiled by nothing until
//! the daemon under test is told to manage exactly that class:
//!
//! ```text
//! mochi --manage-class MochiTestWindow
//! ```
//!
//! Apart from that flag the windows are ordinary: `WS_OVERLAPPEDWINDOW`, so DWM
//! draws a normal Windows 11 frame with rounded corners and the invisible
//! resize border that a tiling manager has to compensate for.
//!
//! Two ways in:
//!
//! * The `mochi-testwin` binary, for driving a session by hand.
//! * [`TestWindows`] plus [`layout_assert`], for integration tests.
//!
//! See `crates/mochi-testbed/README.md` for the contract with the daemon and
//! for a manual tiling session.

pub mod geometry;
pub mod layout_assert;

mod error;
mod event;
mod info;

pub use error::{Error, Result};
pub use event::{EventKind, WindowEvent, now_ms};
pub use geometry::Rect;
pub use info::{MonitorInfo, TestWindowInfo, WS_EX_LAYERED_BIT, WS_EX_TOOLWINDOW_BIT};

/// The class every test window carries, and the only thing the daemon needs to
/// know about the testbed.
pub const TEST_WINDOW_CLASS: &str = "MochiTestWindow";

/// The class of the hidden owner window behind a `spawn --owned` popup. It is
/// never shown and never listed, and it is deliberately not
/// [`TEST_WINDOW_CLASS`] so that the desktop wide listing cannot return it.
pub const OWNER_WINDOW_CLASS: &str = "MochiTestOwnerWindow";

/// The default title prefix; the window index is appended, as in `MochiTest 2`.
pub const DEFAULT_TITLE_PREFIX: &str = "MochiTest";

/// The daemon flag that opts these windows into being managed. Test only: with
/// it the daemon manages the class regardless of the usual tool window rule.
pub const DAEMON_MANAGE_FLAG: &str = "--manage-class";

#[cfg(windows)]
pub mod control;

#[cfg(windows)]
mod wide;
#[cfg(windows)]
mod win32;
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub use win32::{
    POLL, cloaked, close_window, ensure_per_monitor_v2, focus_window, foreground_window,
    frame_bounds, in_menu_mode, list_windows, minimize_window, monitor_at, monitors, move_window,
    rename_window, resize_window, restore_window, set_cloaked, set_rect, set_window_alpha,
    wait_for_frame, wait_for_rect, wait_until_gone, wait_until_settled, window_alpha,
    window_exists, window_info, window_owner, window_rect,
};
#[cfg(windows)]
pub use window::{SpawnOptions, TestWindow, TestWindows};

#[cfg(test)]
mod tests {
    #[test]
    fn the_contract_with_the_daemon_is_spelled_the_same_everywhere() {
        assert_eq!(super::TEST_WINDOW_CLASS, "MochiTestWindow");
        assert_eq!(super::DAEMON_MANAGE_FLAG, "--manage-class");
        assert_eq!(super::DEFAULT_TITLE_PREFIX, "MochiTest");
    }
}
