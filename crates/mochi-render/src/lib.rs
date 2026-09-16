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
//! - [`geometry`] holds the frame maths, [`color`] the Direct2D conversions.
//!
//! The types the config file names are `mochi-core`'s own: [`Rect`],
//! [`AnimationStyle`], [`AnimationConfig`], [`BorderStyle`], [`BorderColours`],
//! [`Colour`] and [`StackbarConfig`]. This crate adds the render specific ones
//! on top and, where a painter needs something a config file cannot say, a
//! small extension trait: [`AnimationConfigExt`], [`BorderStyleExt`],
//! [`BorderColoursExt`], [`StackbarModeExt`] and [`ColourExt`].
//!
//! # Threads
//!
//! Each visual owns one thread with its own message loop. The daemon only ever
//! sends the desired end state ("these containers should have borders now") and
//! never waits for a reply, so a stuck compositor cannot stall tiling.
//!
//! # How the daemon drives the three managers
//!
//! One pass, in order:
//!
//! 1. **Retile.** The layout produces a target rectangle per window.
//! 2. **Animate.** Every window that moved gets an [`animation::AnimationJob`].
//!    The apply callback runs once per frame with the whole frame in it.
//! 3. **Borders follow.** The callback hands that same frame to
//!    [`BorderManager::follow_frame`], which moves the border of every window
//!    that has one and touches nothing else. The end state goes to
//!    [`BorderManager::update`] once per pass, which is what decides who has a
//!    border and which colour it is.
//! 4. **Transparency.** [`TransparencyManager::update`] takes the whole
//!    unfocused set, fades what is new in it and puts back what left it.
//!
//! Each of the three keeps its own last state, so a pass that changed nothing
//! makes no Win32 calls at all.
//!
//! ```no_run
//! use mochi_render::{
//!     AnimationConfig, AnimationConfigExt, Animator, BorderConfig, BorderKind, BorderManager,
//!     BorderSpec, FrameUpdate, Rect, TransparencyManager, WindowHandle,
//! };
//!
//! # fn main() -> mochi_render::Result<()> {
//! let borders = BorderManager::new(BorderConfig::default())?;
//! let mut translucent = TransparencyManager::new(235);
//!
//! // 1. Retile: the layout says where the two windows go.
//! let focused = WindowHandle(0x1234);
//! let other = WindowHandle(0x5678);
//! let was = Rect::new(0, 0, 800, 600);
//! let now = Rect::new(100, 100, 900, 700);
//!
//! // 2. Animate, and 3. let the borders follow every frame.
//! let following = borders.clone();
//! let animator = Animator::new(move |frame: &[FrameUpdate]| {
//!     for update in frame {
//!         // one DeferWindowPos batch for the windows themselves
//!         let _ = (update.handle, update.rect, update.finished);
//!     }
//!     // and one diffed batch for their borders
//!     let _ = following.follow_frame(frame);
//! })?;
//!
//! // The end state first, so every border knows its colour before it moves.
//! borders.update(
//!     Some(BorderSpec::new(focused, now, BorderKind::Single)),
//!     vec![BorderSpec::new(
//!         other,
//!         Rect::new(900, 100, 1700, 700),
//!         BorderKind::Unfocused,
//!     )],
//! )?;
//!
//! let animation = AnimationConfig::default();
//! animator.animate(vec![animation.job(focused, was, now)])?;
//!
//! // 4. Everything that is not focused fades.
//! translucent.update(&[other])?;
//! # Ok(())
//! # }
//! ```
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
pub mod geometry;
pub mod stackbar;

#[cfg(windows)]
pub mod transparency;

#[cfg(windows)]
mod win;

pub use animation::{
    AnimationConfigExt, AnimationJob, AnimationStyleExt, Animator, FrameUpdate, Timeline,
};
pub use border::{
    BorderChanges, BorderColoursExt, BorderConfig, BorderDiff, BorderKind, BorderSpec,
    BorderStyleExt,
};
pub use color::ColourExt;
pub use stackbar::{StackbarModeExt, StackbarSpec, StackbarStyle, StackbarTab, TabLayout};

pub use mochi_core::Rect;
pub use mochi_core::animation::AnimationStyle;
pub use mochi_core::config::{
    AnimationConfig, BorderColours, BorderStyle, Colour, StackbarConfig, StackbarLabel,
    StackbarMode, StackbarTabs,
};

#[cfg(windows)]
pub use border::BorderManager;
#[cfg(windows)]
pub use stackbar::StackbarManager;
#[cfg(windows)]
pub use transparency::{TransparencyManager, Win32Alpha, WindowAlpha};

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
}

/// The crate result type.
pub type Result<T> = std::result::Result<T, RenderError>;
