//! Per-monitor DPI awareness.
//!
//! The embedded manifest (see `build.rs`) is what actually matters: it makes the
//! process per-monitor v2 aware before any window is created, which is the only
//! way to get correct results from `GetWindowRect` on a mixed DPI desktop.
//! [`ensure_per_monitor_v2`] is a belt-and-braces fallback for the case where
//! the binary is run in a way that loses the manifest, for example under a
//! debugger that re-launches it.

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetAwarenessFromDpiAwarenessContext,
    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
};

/// Best effort switch to per-monitor v2 DPI awareness.
///
/// Returns true when the process ends up per-monitor aware, whether that came
/// from the manifest or from this call.
pub fn ensure_per_monitor_v2() -> bool {
    // ERROR_ACCESS_DENIED here just means the manifest already set it, which is
    // the expected case and not worth a warning.
    let set = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let awareness = unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
    // DPI_AWARENESS_PER_MONITOR_AWARE == 2
    let per_monitor = awareness.0 == 2;
    tracing::debug!(
        set = set.is_ok(),
        awareness = awareness.0,
        per_monitor,
        "dpi awareness"
    );
    per_monitor
}
