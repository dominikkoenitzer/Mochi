//! Shared Win32 plumbing: the worker thread pattern, DPI lookup, window class
//! registration and the layered Direct2D surface the border windows paint into.
//!
//! Nothing in here is public. Every `unsafe` block is small and states the
//! invariant it relies on.

pub(crate) mod surface;
pub(crate) mod worker;

use std::sync::OnceLock;

use mochi_core::Rect;
use windows::Win32::Foundation::{HMODULE, HWND, POINT};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, IsWindow, IsWindowVisible, WINDOW_EX_STYLE, WS_EX_TOPMOST,
};
use windows::core::PCWSTR;

pub(crate) use surface::LayeredSurface;
pub(crate) use worker::{WorkerHandle, spawn_worker};

/// The DPI Windows calls 100%.
pub(crate) const BASE_DPI: u32 = 96;

/// A NUL terminated UTF-16 copy of `text`, ready for a `PCWSTR` argument.
pub(crate) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The module handle this DLL or exe was loaded with, for window classes.
pub(crate) fn module_handle() -> HMODULE {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;

    // SAFETY: GetModuleHandleW(NULL) returns the handle of the running module
    // and cannot fail for the null argument. The handle is process-lifetime and
    // must not be freed.
    unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or(HMODULE(std::ptr::null_mut()))
}

/// `true` when the handle still names a live window.
pub(crate) fn is_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    // SAFETY: IsWindow accepts any value, including stale handles; that is the
    // whole point of calling it.
    unsafe { IsWindow(Some(hwnd)) }.as_bool()
}

/// `true` when the window is on screen rather than hidden or cloaked away.
pub(crate) fn is_window_visible(hwnd: HWND) -> bool {
    // SAFETY: as above, IsWindowVisible tolerates any handle value.
    unsafe { IsWindowVisible(hwnd) }.as_bool()
}

/// The extended style bits of a window, or zero when it has gone away.
pub(crate) fn ex_style(hwnd: HWND) -> WINDOW_EX_STYLE {
    if !is_window(hwnd) {
        return WINDOW_EX_STYLE(0);
    }
    // SAFETY: the handle was just checked with IsWindow. A window that dies
    // between the two calls makes this return 0, which is handled.
    let bits = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    WINDOW_EX_STYLE(bits as u32)
}

/// `true` when the window sits in the always-on-top band of the z-order.
///
/// A border can only be stacked directly above a topmost window if it is
/// topmost itself, so the border windows copy this bit from their target.
pub(crate) fn is_topmost(hwnd: HWND) -> bool {
    ex_style(hwnd).contains(WS_EX_TOPMOST)
}

/// Puts `window` directly above `target` in the z-order, optionally moving it
/// to `place` in the same call.
///
/// This is what keeps a border or a stackbar attached to its window instead of
/// floating over unrelated ones.
///
/// `SetWindowPos`'s `hWndInsertAfter` names the window that should **precede**
/// ours, that is, end up above it, so passing the target would put the frame
/// *under* the window it belongs to and the overlapping inner pixel of the
/// frame would be painted over. The window directly above the target is what
/// has to be passed instead, which is `GW_HWNDPREV`.
///
/// Windows refuses to put an ordinary window above a topmost one, so the
/// topmost bit is copied from the target and dropped again when the target
/// loses it. A null or dead target means "no anchor": the window goes topmost,
/// which is what the demo and a target that died mid-frame both want.
///
/// Returns the new topmost state, which the caller remembers for next time.
///
/// # Errors
///
/// [`crate::RenderError::Win32`] when `SetWindowPos` fails, which normally
/// means our own window has gone.
pub(crate) fn stack_above(
    window: HWND,
    target: HWND,
    currently_topmost: bool,
    place: Option<Rect>,
) -> crate::Result<bool> {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowPos,
    };

    let anchored = !target.0.is_null() && is_window(target);
    let wants_topmost = if anchored { is_topmost(target) } else { true };

    if wants_topmost != currently_topmost {
        let band = if wants_topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        // SAFETY: our own window; position, size and activation untouched. This
        // only moves it between the topmost and the ordinary band.
        unsafe {
            SetWindowPos(
                window,
                Some(band),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }?;
    }

    // Where to slot in, and whether the z-order needs touching at all.
    let (insert_after, keep_zorder) = if anchored {
        match window_above(target) {
            // Already directly above the target: leave the z-order alone, or
            // the window below would be raised past its neighbours.
            Some(previous) if previous == window => (HWND_TOP, true),
            // Slot in where that window is.
            Some(previous) if wants_topmost || !is_topmost(previous) => (previous, false),
            // The window above the target is topmost and we are not, so the top
            // of the ordinary band is as close as we can get.
            Some(_) | None => (HWND_TOP, false),
        }
    } else {
        (HWND_TOPMOST, false)
    };

    let rect = place.unwrap_or_default();
    let mut flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
    if keep_zorder {
        flags |= SWP_NOZORDER;
    }
    if place.is_none() {
        flags |= SWP_NOMOVE | SWP_NOSIZE;
    }

    // SAFETY: `window` is one of ours and `insert_after` is either a live
    // window handle read out of the z-order or one of the sentinels.
    unsafe {
        SetWindowPos(
            window,
            Some(insert_after),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            flags,
        )
    }?;

    Ok(wants_topmost)
}

/// The window sitting directly above `hwnd` in the z-order, if any.
fn window_above(hwnd: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{GW_HWNDPREV, GetWindow};

    // SAFETY: GetWindow only reads the window list and tolerates a stale
    // handle by failing. A null result means the window is already at the top.
    let previous = unsafe { GetWindow(hwnd, GW_HWNDPREV) }.ok()?;
    (!previous.0.is_null()).then_some(previous)
}

/// The monitor a rectangle's centre lands on.
fn monitor_for_rect(rect: Rect) -> HMONITOR {
    let point = POINT {
        x: rect.center_x(),
        y: rect.center_y(),
    };
    // SAFETY: MonitorFromPoint takes a plain POINT and always returns a handle
    // because of MONITOR_DEFAULTTONEAREST.
    unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) }
}

/// The effective DPI of the monitor a rectangle sits on, 96 when unknown.
///
/// Only the corner radius uses this. Border widths stay in physical pixels so
/// that a 6 px border is 6 px on both the 4K and the 1080p monitor.
pub(crate) fn dpi_for_rect(rect: Rect) -> u32 {
    let monitor = monitor_for_rect(rect);
    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    // SAFETY: both out parameters are live stack slots and the monitor handle
    // comes straight from MonitorFromPoint.
    let ok = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    if ok.is_err() || dpi_x == 0 {
        return BASE_DPI;
    }
    dpi_x
}

/// `true` when the running Windows rounds the corners of top level windows,
/// which is Windows 11 and later.
///
/// `RtlGetVersion` is used rather than `GetVersionEx` because the latter lies
/// about the version unless the exe carries a compatibility manifest.
pub(crate) fn os_rounds_corners() -> bool {
    static ROUNDS: OnceLock<bool> = OnceLock::new();

    *ROUNDS.get_or_init(|| {
        use windows::Wdk::System::SystemServices::RtlGetVersion;
        use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a correctly sized, initialised OSVERSIONINFOW on the
        // stack, which is exactly what RtlGetVersion writes into.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status.is_err() {
            return true;
        }
        info.dwBuildNumber >= 22000
    })
}
