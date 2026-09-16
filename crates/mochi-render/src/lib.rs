//! Visuals for Mochi: window borders, per-window transparency, the stackbar and
//! the animation driver.
//!
//! Everything here is optional decoration. Tiling works with this crate switched
//! off, so nothing in it may block or panic the manager thread.
//!
//! # Layout
//!
//! - [`border`] draws one layered frame window per managed container. Owned by a
//!   dedicated thread, driven through the cheap [`border::BorderManager`] handle.
//! - [`stackbar`] draws the tab bar above a stacked container and reports clicks.
//! - [`transparency`] fades unfocused windows with `WS_EX_LAYERED`.
//! - [`animation`] interpolates rectangles on a timer thread and hands whole
//!   frames back to the daemon, which applies them with `DeferWindowPos`.
//! - [`easing`] holds the easing curves, [`geometry`] the frame maths,
//!   [`color`] the colour type.
//!
//! # Threads
//!
//! Each visual owns one thread with its own message loop. The daemon only ever
//! sends the desired end state ("these containers should have borders now") and
//! never waits for a reply, so a stuck compositor cannot stall tiling.
//!
//! # Safety
//!
//! The border and stackbar windows are created, moved and destroyed by this
//! crate alone. The only call that touches a foreign window is
//! [`transparency::set_alpha`], plus passing a target `HWND` to `SetWindowPos`
//! as the z-order anchor, which does not modify the target.

#![warn(missing_docs)]

pub mod animation;
pub mod border;
pub mod color;
pub mod easing;
pub mod geometry;
pub mod stackbar;

#[cfg(windows)]
pub mod transparency;

#[cfg(windows)]
mod win;

pub use animation::{AnimationConfig, AnimationJob, Animator, FrameUpdate};
pub use border::{BorderColours, BorderConfig, BorderKind, BorderSpec, BorderStyle};
pub use color::Color;
pub use easing::Easing;
pub use mochi_core::Rect;
pub use stackbar::{StackbarConfig, StackbarLabel, StackbarMode, StackbarSpec, StackbarTab};

#[cfg(windows)]
pub use border::BorderManager;
#[cfg(windows)]
pub use stackbar::StackbarManager;

/// A window handle, stored as the raw `HWND` value.
///
/// `HWND` itself is a raw pointer and therefore neither `Send` nor `Sync`, which
/// would stop the daemon from putting one in a channel message. This newtype is
/// the same number with the thread-safety markers a plain integer already has.
///
/// A handle is only ever dereferenced on the thread that received it, and always
/// after an `IsWindow` check, because the window may have died in the meantime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct WindowHandle(pub isize);

impl WindowHandle {
    /// The null handle: "no window".
    pub const NONE: Self = Self(0);

    /// `true` for the null handle.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// The raw value, for logging.
    #[must_use]
    pub const fn raw(self) -> isize {
        self.0
    }
}

impl std::fmt::Display for WindowHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

#[cfg(windows)]
impl WindowHandle {
    /// Borrows the handle as an `HWND`.
    #[must_use]
    pub fn hwnd(self) -> windows::Win32::Foundation::HWND {
        windows::Win32::Foundation::HWND(self.0 as *mut core::ffi::c_void)
    }
}

#[cfg(windows)]
impl From<windows::Win32::Foundation::HWND> for WindowHandle {
    fn from(hwnd: windows::Win32::Foundation::HWND) -> Self {
        Self(hwnd.0 as isize)
    }
}

#[cfg(windows)]
impl From<WindowHandle> for windows::Win32::Foundation::HWND {
    fn from(handle: WindowHandle) -> Self {
        handle.hwnd()
    }
}

/// Everything that can go wrong while drawing.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// A Win32 or COM call failed.
    #[cfg(windows)]
    #[error("win32 call failed: {0}")]
    Win32(#[from] windows::core::Error),

    /// The worker thread behind a manager handle has gone away.
    #[error("the {0} thread is not running")]
    ThreadGone(&'static str),

    /// A worker thread could not be started.
    #[error("could not start the {0} thread: {1}")]
    ThreadStart(&'static str, String),

    /// A colour string was not a colour.
    #[error("invalid colour {0:?}, expected #rgb, #rrggbb or #rrggbbaa")]
    Color(String),

    /// An easing name did not match any curve.
    #[error("unknown easing curve {0:?}")]
    Easing(String),
}

/// The crate result type.
pub type Result<T> = std::result::Result<T, RenderError>;
