//! Pure window-management logic for Mochi.
//!
//! Everything in this crate is platform independent and unit testable. There
//! is no Win32 here, no threads and no IO: the daemon owns one [`State`],
//! feeds events into it, and gets a [`Changes`] back telling it which windows
//! to move, show, hide or focus.
//!
//! # Modules
//!
//! - [`geometry`] rectangles, axes, directions
//! - [`model`] the monitor, workspace, container and window tree
//! - [`layout`] pure layout functions, BSP first
//! - [`rules`] which windows to ignore, float or manage
//! - [`config`] the JSON configuration file
//! - [`animation`] easing curves and frame generation
//! - [`ops`] every command, as methods on [`State`]
//!
//! # Example
//!
//! ```
//! use mochi_core::geometry::Rect;
//! use mochi_core::model::{Monitor, State, Window, WindowId};
//!
//! let mut state = State::new();
//! state.default_workspace_padding = 0;
//! state.default_container_padding = 0;
//!
//! let screen = Rect::new(0, 0, 1920, 1080);
//! state.add_monitor(Monitor::new(1, screen, screen));
//!
//! state.add_window(Window::new(1).with_exe("Code.exe"))?;
//! let changes = state.add_window(Window::new(2).with_exe("firefox.exe"))?;
//!
//! // The first window keeps the left half, the new one takes the right.
//! assert_eq!(state.rect_for_window(WindowId(1)), Some(Rect::new(0, 0, 960, 1080)));
//! assert_eq!(changes.focus, Some(WindowId(2)));
//! # Ok::<(), mochi_core::Error>(())
//! ```

#![warn(missing_docs)]
#![warn(clippy::doc_markdown)]

pub mod animation;
pub mod config;
pub mod error;
pub mod geometry;
pub mod layout;
pub mod model;
pub mod ops;
pub mod rules;

pub use error::{Error, Result};
pub use geometry::{Axis, Direction, Offset, Rect};
pub use layout::{Flip, Layout, Sizing};
pub use model::{Changes, Container, CycleDirection, Monitor, State, Window, WindowId, Workspace};
pub use rules::{MatchingRule, RuleSets, WindowInfo};

/// The highest workspace index the manager will create on demand.
///
/// Nine is what the hotkeys go up to, and a workspace that cannot be reached
/// from the keyboard is a place for windows to get lost in.
pub const MAX_WORKSPACES: usize = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crate_root_reexports_what_the_daemon_needs() {
        let mut state = State::new();
        let screen = Rect::new(0, 0, 1920, 1080);
        state.add_monitor(Monitor::new(1, screen, screen));
        state.default_workspace_padding = 0;
        state.default_container_padding = 0;

        state.add_window(Window::new(1)).unwrap();
        state.add_window(Window::new(2)).unwrap();

        assert_eq!(state.workspace(0, 0).unwrap().layout, Layout::Bsp);
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(960, 0, 1920, 1080))
        );

        let changes: Changes = state.focus_direction(Direction::Left).unwrap();
        assert_eq!(changes.focus, Some(WindowId(1)));
    }

    #[test]
    fn max_workspaces_covers_every_hotkey_in_the_whkdrc() {
        // alt + 1 .. alt + 9 map to indices 0 .. 8.
        assert!(State::check_workspace_idx(8).is_ok());
        assert_eq!(MAX_WORKSPACES, 9);
    }
}
