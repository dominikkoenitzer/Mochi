//! The Win32 half of the testbed: enumerate monitors, find test windows and
//! push them around.
//!
//! Every function here takes handles as plain `i64` so that callers, tests and
//! the JSON on the wire all speak the same language, and so that nothing in the
//! public API is tied to a raw pointer that is not `Send`.
//!
//! All of these are ordinary cross-process Win32 calls: a second invocation of
//! `mochi-testwin` drives the windows of the first one through exactly the same
//! path a window manager would use.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, InvalidateRect, MONITOR_DEFAULTTONULL,
    MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetAwarenessFromDpiAwarenessContext,
    GetDpiForMonitor, GetThreadDpiAwarenessContext, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetClassNameW,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, MONITORINFOF_PRIMARY,
    PostMessageW, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, SetWindowTextW, ShowWindow, WM_CLOSE,
};
use windows::core::PCWSTR;

use crate::TEST_WINDOW_CLASS;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::info::{MonitorInfo, TestWindowInfo};
use crate::wide::{from_wide, to_wide};

/// How often the `wait_*` helpers look again. Roughly one frame at 60 Hz.
const POLL: std::time::Duration = std::time::Duration::from_millis(15);

/// Turns a handle number back into an `HWND`.
pub(crate) const fn as_hwnd(hwnd: i64) -> HWND {
    HWND(hwnd as isize as *mut c_void)
}

/// Turns an `HWND` into the handle number used everywhere else.
pub(crate) fn as_i64(hwnd: HWND) -> i64 {
    hwnd.0 as isize as i64
}

/// Best effort switch to per-monitor v2 DPI awareness.
///
/// The daemon gets this from an embedded manifest. The testbed has no manifest
/// on purpose, so that it stays a plain `cargo test` dependency, and calls this
/// instead. It has to run before the first window is created, which is why
/// every entry point into the crate calls it first. Returns true when the
/// process ends up per-monitor aware; without that, `GetWindowRect` lies on a
/// mixed DPI desktop.
pub fn ensure_per_monitor_v2() -> bool {
    unsafe {
        // An error here normally means awareness was already set, which is fine.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()).0 == 2
    }
}

/// Enumerates the attached monitors in `EnumDisplayMonitors` order, which is
/// the order `--monitor` indexes into.
///
/// # Errors
/// When `EnumDisplayMonitors` fails outright.
pub fn monitors() -> Result<Vec<MonitorInfo>> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(std::ptr::from_mut(&mut handles) as isize),
        )
        .ok()
        .map_err(|e| Error::win32("EnumDisplayMonitors", &e))?;
    }

    Ok(handles
        .into_iter()
        .enumerate()
        .filter_map(|(index, handle)| monitor_info(index, handle))
        .collect())
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> windows::core::BOOL {
    unsafe {
        let handles = &mut *(lparam.0 as *mut Vec<HMONITOR>);
        handles.push(monitor);
    }
    true.into()
}

fn monitor_info(index: usize, handle: HMONITOR) -> Option<MonitorInfo> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(handle, std::ptr::from_mut(&mut info).cast()) };
    if !ok.as_bool() {
        return None;
    }

    let mut dpi_x = 96;
    let mut dpi_y = 96;
    unsafe {
        let _ = GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    }

    Some(MonitorInfo {
        index,
        handle: handle.0 as isize as i64,
        device: from_wide(&info.szDevice),
        rect: info.monitorInfo.rcMonitor.into(),
        work_area: info.monitorInfo.rcWork.into(),
        dpi: dpi_x.max(96),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// The monitor at `index`, with a message that says what was available.
///
/// # Errors
/// When there is no monitor with that index.
pub fn monitor_at(index: usize) -> Result<MonitorInfo> {
    let all = monitors()?;
    let count = all.len();
    all.into_iter().nth(index).ok_or_else(|| {
        Error::not_found(format!(
            "monitor {index}, this desktop has {count} monitor(s), indexed from 0"
        ))
    })
}

/// Every window of the [`crate::TEST_WINDOW_CLASS`] class on the desktop,
/// whichever process spawned it.
///
/// # Errors
/// When `EnumWindows` fails outright. Windows that die during the walk are
/// dropped from the result rather than turned into an error.
pub fn list_windows() -> Result<Vec<TestWindowInfo>> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(std::ptr::from_mut(&mut handles) as isize),
        )
        .map_err(|e| Error::win32("EnumWindows", &e))?;
    }

    let monitors = monitors().unwrap_or_default();
    Ok(handles
        .into_iter()
        .filter_map(|hwnd| info_for(hwnd, &monitors).ok())
        .collect())
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    unsafe {
        if class_name(hwnd) == TEST_WINDOW_CLASS {
            let handles = &mut *(lparam.0 as *mut Vec<HWND>);
            handles.push(hwnd);
        }
    }
    true.into()
}

/// Everything the testbed knows about one window.
///
/// # Errors
/// When the handle is not a window any more.
pub fn window_info(hwnd: i64) -> Result<TestWindowInfo> {
    info_for(as_hwnd(hwnd), &monitors().unwrap_or_default())
}

fn info_for(hwnd: HWND, monitors: &[MonitorInfo]) -> Result<TestWindowInfo> {
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(Error::not_found(format!("hwnd 0x{:x}", as_i64(hwnd))));
    }
    let rect = window_rect_raw(hwnd)?;
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
    let monitor = monitors
        .iter()
        .find(|m| m.handle == monitor.0 as isize as i64)
        .map(|m| m.index);
    let mut pid = 0;
    let _ = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };

    Ok(TestWindowInfo {
        hwnd: as_i64(hwnd),
        index: unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as u32,
        title: unsafe { window_title(hwnd) },
        class: unsafe { class_name(hwnd) },
        pid,
        rect,
        frame: frame_bounds_raw(hwnd).unwrap_or(rect),
        monitor,
        style: unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32,
        ex_style: unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32,
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
    })
}

unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    from_wide(&buf[..len as usize])
}

pub(crate) unsafe fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    from_wide(&buf[..len as usize])
}

/// True when the handle still refers to a live window.
#[must_use]
pub fn window_exists(hwnd: i64) -> bool {
    unsafe { IsWindow(Some(as_hwnd(hwnd))) }.as_bool()
}

/// `GetWindowRect`: the outer rectangle, invisible resize border included.
///
/// # Errors
/// When the window is gone.
pub fn window_rect(hwnd: i64) -> Result<Rect> {
    window_rect_raw(as_hwnd(hwnd))
}

fn window_rect_raw(hwnd: HWND) -> Result<Rect> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| Error::win32("GetWindowRect", &e))?;
    Ok(rect.into())
}

/// `DWMWA_EXTENDED_FRAME_BOUNDS`: the rectangle the user perceives.
///
/// # Errors
/// When DWM has nothing for that window, which happens while it is minimized.
pub fn frame_bounds(hwnd: i64) -> Result<Rect> {
    frame_bounds_raw(as_hwnd(hwnd))
}

fn frame_bounds_raw(hwnd: HWND) -> Result<Rect> {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            std::ptr::from_mut(&mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .map_err(|e| Error::win32("DwmGetWindowAttribute", &e))?;
    Ok(rect.into())
}

/// Moves a window without resizing it.
///
/// # Errors
/// When `SetWindowPos` is refused, which normally means the window is gone.
pub fn move_window(hwnd: i64, x: i32, y: i32) -> Result<()> {
    unsafe {
        SetWindowPos(
            as_hwnd(hwnd),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| Error::win32("SetWindowPos", &e))
}

/// Resizes a window without moving it.
///
/// # Errors
/// When `SetWindowPos` is refused.
pub fn resize_window(hwnd: i64, width: i32, height: i32) -> Result<()> {
    unsafe {
        SetWindowPos(
            as_hwnd(hwnd),
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| Error::win32("SetWindowPos", &e))
}

/// Moves and resizes in one call, the way a layout does.
///
/// # Errors
/// When `SetWindowPos` is refused.
pub fn set_rect(hwnd: i64, rect: Rect) -> Result<()> {
    unsafe {
        SetWindowPos(
            as_hwnd(hwnd),
            None,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| Error::win32("SetWindowPos", &e))
}

/// Brings a window to the foreground.
///
/// Returns false when Windows refused the foreground change, which is normal:
/// a console process that is not itself in the foreground does not own the
/// foreground lock. The window is raised either way.
///
/// # Errors
/// When the handle is not a window.
pub fn focus_window(hwnd: i64) -> Result<bool> {
    let target = as_hwnd(hwnd);
    if !window_exists(hwnd) {
        return Err(Error::not_found(format!("hwnd 0x{hwnd:x}")));
    }
    unsafe {
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }

        // The usual thread input dance: attach to whoever owns the foreground
        // so that SetForegroundWindow is allowed to do its job.
        let me = GetCurrentThreadId();
        let foreground = GetForegroundWindow();
        let other = if foreground.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };
        let attached = other != 0 && other != me && AttachThreadInput(me, other, true).as_bool();

        let _ = BringWindowToTop(target);
        let ok = SetForegroundWindow(target).as_bool();

        if attached {
            let _ = AttachThreadInput(me, other, false);
        }
        Ok(ok)
    }
}

/// Asks a window to close, exactly as clicking its close button would.
///
/// # Errors
/// When the message could not be posted, which means the window is already gone.
pub fn close_window(hwnd: i64) -> Result<()> {
    unsafe { PostMessageW(Some(as_hwnd(hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| Error::win32("PostMessageW", &e))
}

/// Minimizes a window.
///
/// # Errors
/// When the handle is not a window.
pub fn minimize_window(hwnd: i64) -> Result<()> {
    if !window_exists(hwnd) {
        return Err(Error::not_found(format!("hwnd 0x{hwnd:x}")));
    }
    let _ = unsafe { ShowWindow(as_hwnd(hwnd), SW_MINIMIZE) };
    Ok(())
}

/// Restores a minimized or maximized window.
///
/// # Errors
/// When the handle is not a window.
pub fn restore_window(hwnd: i64) -> Result<()> {
    if !window_exists(hwnd) {
        return Err(Error::not_found(format!("hwnd 0x{hwnd:x}")));
    }
    let _ = unsafe { ShowWindow(as_hwnd(hwnd), SW_RESTORE) };
    Ok(())
}

/// Sets a new title, which the window repaints into its label.
///
/// # Errors
/// When `SetWindowTextW` is refused.
pub fn rename_window(hwnd: i64, title: &str) -> Result<()> {
    let wide = to_wide(title);
    unsafe { SetWindowTextW(as_hwnd(hwnd), PCWSTR(wide.as_ptr())) }
        .map_err(|e| Error::win32("SetWindowTextW", &e))?;
    let _ = unsafe { InvalidateRect(Some(as_hwnd(hwnd)), None, false) };
    Ok(())
}

/// Polls `GetWindowRect` until the predicate is happy.
///
/// # Errors
/// [`Error::Timeout`] when the predicate never held, or [`Error::NotFound`]
/// when the window died while waiting.
pub fn wait_for_rect(
    hwnd: i64,
    predicate: impl Fn(Rect) -> bool,
    timeout: std::time::Duration,
) -> Result<Rect> {
    wait_for(hwnd, window_rect, predicate, timeout, "window rect")
}

/// The same as [`wait_for_rect`] but on the perceived frame, which is what a
/// tiling assertion should be written against.
///
/// # Errors
/// As [`wait_for_rect`].
pub fn wait_for_frame(
    hwnd: i64,
    predicate: impl Fn(Rect) -> bool,
    timeout: std::time::Duration,
) -> Result<Rect> {
    wait_for(
        hwnd,
        frame_bounds,
        predicate,
        timeout,
        "extended frame bounds",
    )
}

fn wait_for(
    hwnd: i64,
    read: impl Fn(i64) -> Result<Rect>,
    predicate: impl Fn(Rect) -> bool,
    timeout: std::time::Duration,
    what: &str,
) -> Result<Rect> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = None;
    loop {
        if !window_exists(hwnd) {
            return Err(Error::not_found(format!(
                "hwnd 0x{hwnd:x} died while waiting for its {what}"
            )));
        }
        if let Ok(rect) = read(hwnd) {
            if predicate(rect) {
                return Ok(rect);
            }
            last = Some(rect);
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::timeout(format!(
                "the {what} of hwnd 0x{hwnd:x} after {:?}, last seen {}",
                timeout,
                last.map_or_else(|| "nothing".to_string(), |r| r.to_string())
            )));
        }
        std::thread::sleep(POLL);
    }
}

/// Waits until none of the handles is a window any more.
///
/// # Errors
/// [`Error::Timeout`] with the handles that are still alive.
pub fn wait_until_gone(hwnds: &[i64], timeout: std::time::Duration) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let alive: Vec<String> = hwnds
            .iter()
            .filter(|&&h| window_exists(h))
            .map(|h| format!("0x{h:x}"))
            .collect();
        if alive.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::timeout(format!(
                "{} test window(s) to close: {}",
                alive.len(),
                alive.join(", ")
            )));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_survive_the_round_trip_through_a_pointer() {
        for h in [0_i64, 1, 0x1234_5678, -1] {
            assert_eq!(as_i64(as_hwnd(h)), h);
        }
    }

    #[test]
    fn the_desktop_has_at_least_one_monitor() {
        // A machine with no display is possible in CI; only check consistency.
        let monitors = monitors().expect("EnumDisplayMonitors");
        for (i, m) in monitors.iter().enumerate() {
            assert_eq!(m.index, i);
            assert!(m.dpi >= 96, "{m:?}");
            assert!(!m.rect.is_empty(), "{m:?}");
            assert!(m.rect.contains(&m.work_area), "{m:?}");
        }
        assert!(monitors.iter().filter(|m| m.primary).count() <= 1);
    }

    #[test]
    fn asking_for_a_monitor_that_is_not_there_says_how_many_there_are() {
        let err = monitor_at(99).unwrap_err().to_string();
        assert!(err.contains("monitor 99"), "{err}");
    }

    #[test]
    fn a_null_handle_is_not_a_window() {
        assert!(!window_exists(0));
        assert!(window_info(0).is_err());
    }

    #[test]
    fn listing_test_windows_never_returns_anything_else() {
        for w in list_windows().expect("EnumWindows") {
            assert_eq!(w.class, TEST_WINDOW_CLASS);
        }
    }
}
