//! Per-window transparency for unfocused windows.
//!
//! This is the one place in the crate that touches a window Mochi did not
//! create, so it is deliberately small: set a style bit, set an alpha, put both
//! back.
//!
//! # The gotcha
//!
//! `WS_EX_LAYERED` changes how a window is composited, and not every toolkit
//! copes:
//!
//! - **Electron** apps (Discord, VS Code, Spotify) often go black, keep a stale
//!   frame or stop repainting until they are resized. Chromium draws through its
//!   own compositor and does not always notice that the layered style arrived.
//! - **UWP and WinUI** windows (Settings, Terminal, Photos) are hosted in
//!   `ApplicationFrameWindow` and the alpha lands on the frame rather than the
//!   content, or is ignored outright.
//! - Windows that already set `WS_EX_LAYERED` themselves, for their own
//!   translucency or their own colour key, are ruined by
//!   [`clear_alpha`] taking the bit away again. Check [`is_layered`] **before**
//!   the first [`set_alpha`] and leave those windows alone.
//! - A layered window loses hardware overlay, so video and games can drop
//!   frames.
//!
//! The daemon therefore keeps a `transparency_ignore` list of rules, exactly
//! like the ignore rules for tiling, and skips those windows here. There is no
//! way to detect the problem from the outside, so the list is the only cure.

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    WINDOW_EX_STYLE, WS_EX_LAYERED,
};

use crate::Result;

/// Fully opaque, the alpha a window has when nothing has touched it.
pub const OPAQUE: u8 = 255;

/// `true` when the window already has `WS_EX_LAYERED`.
///
/// Call this before the first [`set_alpha`] on a window and remember the
/// answer: a window that was layered on its own must never have the bit taken
/// away again.
#[must_use]
pub fn is_layered(hwnd: HWND) -> bool {
    crate::win::ex_style(hwnd).contains(WS_EX_LAYERED)
}

/// Fades a window to `alpha`, where 0 is invisible and 255 is opaque.
///
/// Sets `WS_EX_LAYERED` if the window does not have it yet, then applies a
/// whole-window alpha with `LWA_ALPHA`. An alpha of 255 is applied as an alpha
/// too, rather than by removing the style, so that the daemon can go back and
/// forth without the window flickering; use [`clear_alpha`] to really put it
/// back the way it was.
///
/// # Errors
///
/// [`crate::RenderError::Win32`] when the window has gone away or refuses the
/// style.
pub fn set_alpha(hwnd: HWND, alpha: u8) -> Result<()> {
    if !crate::win::is_window(hwnd) {
        return Err(windows::core::Error::from_thread().into());
    }

    if !is_layered(hwnd) {
        let style = current_ex_style(hwnd) | WS_EX_LAYERED;
        set_ex_style(hwnd, style)?;
    }

    // SAFETY: the window is live and now layered, which is what
    // SetLayeredWindowAttributes requires. LWA_ALPHA ignores the colour key, so
    // the zero passed for it is not read.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA) }?;
    Ok(())
}

/// Puts a window back to fully opaque and removes `WS_EX_LAYERED`.
///
/// Do not call this on a window that was already layered before Mochi saw it;
/// see [`is_layered`].
///
/// # Errors
///
/// [`crate::RenderError::Win32`] when the window has gone away or refuses the
/// style.
pub fn clear_alpha(hwnd: HWND) -> Result<()> {
    if !crate::win::is_window(hwnd) {
        return Err(windows::core::Error::from_thread().into());
    }

    if !is_layered(hwnd) {
        return Ok(());
    }

    // Back to opaque first: taking the style away from a window that is
    // currently faded can leave the last composited frame on screen until
    // something repaints it.
    // SAFETY: the window is live and layered.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), OPAQUE, LWA_ALPHA) }?;

    let style = WINDOW_EX_STYLE(current_ex_style(hwnd).0 & !WS_EX_LAYERED.0);
    set_ex_style(hwnd, style)?;
    Ok(())
}

/// The window's extended style bits.
fn current_ex_style(hwnd: HWND) -> WINDOW_EX_STYLE {
    // SAFETY: the caller has checked the handle with IsWindow. A window that
    // dies in between returns 0, which the callers treat as "no bits set".
    WINDOW_EX_STYLE(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32)
}

/// Writes the window's extended style bits back.
fn set_ex_style(hwnd: HWND, style: WINDOW_EX_STYLE) -> Result<()> {
    // SetWindowLongPtrW returns the previous value and reports failure by
    // returning zero with a set error code, so the thread error is cleared
    // first to tell the two apart.
    // SAFETY: the handle was checked by the caller; only the extended style
    // word is written, with a value derived from the one just read.
    let previous = unsafe {
        windows::Win32::Foundation::SetLastError(windows::Win32::Foundation::WIN32_ERROR(0));
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style.0 as isize)
    };

    if previous == 0 {
        let error = windows::core::Error::from_thread();
        if error.code().is_err() {
            return Err(error.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handle that is certainly not a window. The functions must refuse it
    /// rather than reach into the window manager.
    #[test]
    fn a_dead_handle_is_refused() {
        let dead = HWND(std::ptr::null_mut());
        assert!(!is_layered(dead));
        assert!(set_alpha(dead, 235).is_err());
        assert!(clear_alpha(dead).is_err());
    }

    #[test]
    fn opaque_is_full_alpha() {
        assert_eq!(OPAQUE, u8::MAX);
    }
}
