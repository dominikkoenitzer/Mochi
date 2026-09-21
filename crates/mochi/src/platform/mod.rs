//! Everything that touches Win32.
//!
//! All write operations go through the [`Platform`] trait so that they can be
//! swapped for a logger with `--dry-run`, and so the tiling logic in
//! `mochi-core` can be exercised without a desktop.

use anyhow::Result;
use mochi_core::Rect;

pub mod dpi;
pub mod types;
pub mod wide;

mod appview;
mod dry_run;
mod win32;

pub use dry_run::DryRunPlatform;
pub use types::{
    FRAME_HOST, Hwnd, MonitorInfo, Unmanageable, WindowInfo, is_frame_host, is_manageable,
    is_manageable_with,
};
pub use win32::Win32Platform;

/// Nothing on this system can cloak this window: the shell has no view for it
/// and DWM only cloaks windows of the calling process.
///
/// The window manager hides such a window instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloakUnsupported;

impl std::fmt::Display for CloakUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this window cannot be cloaked")
    }
}

impl std::error::Error for CloakUnsupported {}

/// Where a window should end up in the z-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZOrder {
    /// Leave the z-order alone.
    #[default]
    Keep,
    /// Put the window at the top of its band.
    Top,
    /// Put the window at the bottom.
    Bottom,
    /// Make the window topmost.
    TopMost,
    /// Take the window out of the topmost band.
    NoTopMost,
}

/// One window move, as handed to [`Platform::set_positions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlacement {
    /// Window to move.
    pub hwnd: Hwnd,
    /// Target rectangle, in physical pixels, describing the perceived frame.
    /// The implementation compensates for the invisible resize border itself.
    pub rect: Rect,
    /// Where the window should sit in the z-order.
    pub z: ZOrder,
}

impl WindowPlacement {
    /// A placement that only moves the window.
    pub const fn new(hwnd: Hwnd, rect: Rect) -> Self {
        Self {
            hwnd,
            rect,
            z: ZOrder::Keep,
        }
    }
}

/// How a window should be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowState {
    /// Restore a minimized or maximized window to its normal size.
    Restore,
    /// Minimize the window.
    Minimize,
    /// Maximize the window.
    Maximize,
    /// Show the window without activating it.
    ShowNoActivate,
    /// Hide the window outright. Prefer cloaking, which is far less invasive.
    Hide,
}

/// The whole Win32 surface Mochi uses, behind one trait.
///
/// Read operations are safe to run next to another window manager. Every write
/// operation is marked as such and is a no-op under `--dry-run`.
pub trait Platform: Send + Sync {
    /// A name for logs: `win32` or `dry-run`.
    fn name(&self) -> &'static str;

    // ---- reads ----------------------------------------------------------

    /// Enumerates the attached monitors in `EnumDisplayMonitors` order.
    fn monitors(&self) -> Result<Vec<MonitorInfo>>;

    /// Enumerates every top-level window, manageable or not.
    fn windows(&self) -> Result<Vec<WindowInfo>>;

    /// Reads everything about one window.
    fn window_info(&self, hwnd: Hwnd) -> Result<WindowInfo>;

    /// The current foreground window, if there is one.
    fn foreground_window(&self) -> Option<Hwnd>;

    /// The top-level window under a screen point.
    fn window_at(&self, x: i32, y: i32) -> Option<Hwnd>;

    /// The mouse position in virtual screen coordinates.
    fn cursor_position(&self) -> Result<(i32, i32)>;

    /// Whether Windows currently has the window maximized.
    ///
    /// Deliberately its own call rather than a field of [`WindowInfo`]: this is
    /// asked on the location-change path, which is the flood path, and reading
    /// a whole `WindowInfo` there would cost a process handle open and a dozen
    /// cross-process calls per event. `IsZoomed` is a flag read.
    fn is_maximized(&self, hwnd: Hwnd) -> bool;

    /// Whether the window belongs to a process that outranks Mochi.
    ///
    /// Asked by opening the process, not by reading a token: Windows refuses
    /// a normal process even `PROCESS_QUERY_LIMITED_INFORMATION` on an
    /// elevated one, so the refusal itself is the answer and costs one call.
    ///
    /// This is worth surfacing for a reason beyond tiling. The same boundary
    /// blocks a low-level keyboard hook: while a window that outranks Mochi
    /// holds the focus, Windows delivers it none of the key presses, so every
    /// binding silently stops working and nothing on screen says why.
    fn outranks_us(&self, hwnd: Hwnd) -> bool;

    /// Whether the window is on screen right now: visible, and not cloaked.
    ///
    /// Its own call for the same reason as [`Platform::is_maximized`]: this is
    /// asked about every window that should be on screen, on every event, and
    /// reading a whole [`WindowInfo`] there would cost a process handle open
    /// and a dozen cross-process calls each time. Both halves are flag reads.
    ///
    /// A handle that is no longer a window answers `true`, so that a window
    /// which has simply died is left to the destroy path rather than being
    /// swept up here.
    fn is_on_screen(&self, hwnd: Hwnd) -> bool;

    /// Windows that have turned a call down since this was last asked, and so
    /// can no longer be tiled.
    ///
    /// Whether Mochi is allowed to touch a window is only ever discovered by
    /// trying, and trying happens deep inside a layout pass, often on the
    /// animation thread. This is how that discovery gets back to the model,
    /// which has to let the window go: one it cannot move keeps a tile it can
    /// never be put in, so the tile stays empty, its border is drawn around
    /// nothing and every other window on the screen is squeezed around a hole.
    ///
    /// Draining rather than reading, so each window is reported once. The
    /// default is empty, which is right for every platform that has no such
    /// notion.
    fn take_unreachable(&self) -> Vec<Hwnd> {
        Vec::new()
    }

    // ---- writes ---------------------------------------------------------

    /// WRITE. Moves and resizes several windows in one `DeferWindowPos` batch.
    fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()>;

    /// WRITE. Moves and resizes one window.
    fn set_position(&self, placement: &WindowPlacement) -> Result<()> {
        self.set_positions(std::slice::from_ref(placement))
    }

    /// WRITE. Hides or reveals a window with `DWMWA_CLOAK`.
    fn set_cloaked(&self, hwnd: Hwnd, cloaked: bool) -> Result<()>;

    /// WRITE. Calls `ShowWindow`.
    fn show(&self, hwnd: Hwnd, state: ShowState) -> Result<()>;

    /// WRITE. Brings a window to the foreground, thread-input dance included.
    fn focus(&self, hwnd: Hwnd) -> Result<()>;

    /// WRITE. Takes the keyboard off every window, leaving the desktop with it.
    ///
    /// Used when the focused monitor moves to a workspace that has no window on
    /// it. Nothing can be focused there, and leaving the keyboard on the window
    /// the user just navigated away from means typing into a screen they are no
    /// longer looking at.
    fn focus_desktop(&self) -> Result<()>;

    /// WRITE. Posts `WM_CLOSE`, which asks the window to close but never forces it.
    fn close(&self, hwnd: Hwnd) -> Result<()>;

    /// WRITE. Sets per-window alpha, or clears it with `None`.
    fn set_transparency(&self, hwnd: Hwnd, alpha: Option<u8>) -> Result<()>;

    /// WRITE. Adds or removes `WS_EX_TOPMOST`.
    fn set_topmost(&self, hwnd: Hwnd, topmost: bool) -> Result<()>;

    /// WRITE. Warps the mouse, for `mouse-follows-focus`.
    fn set_cursor_position(&self, x: i32, y: i32) -> Result<()>;
}

/// Builds the platform the daemon should use.
///
/// With `dry_run` the reads are still real; only the writes become log lines.
/// The result is shared with the mouse tracker, hence the `Arc`.
pub fn new(dry_run: bool) -> std::sync::Arc<dyn Platform> {
    if dry_run {
        std::sync::Arc::new(DryRunPlatform::new(Win32Platform::new()))
    } else {
        std::sync::Arc::new(Win32Platform::new())
    }
}
