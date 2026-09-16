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

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
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
    BringWindowToTop, EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetClassNameW,
    GetForegroundWindow, GetLayeredWindowAttributes, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, LWA_ALPHA,
    MONITORINFOF_PRIMARY, PostMessageW, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, SetWindowTextW, ShowWindow, WM_CLOSE,
};
use windows::core::PCWSTR;

use crate::TEST_WINDOW_CLASS;
use crate::error::{Error, Result};
use crate::geometry::Rect;
use crate::info::{MonitorInfo, TestWindowInfo, WS_EX_LAYERED_BIT};
use crate::wide::{from_wide, to_wide};

/// How often the `wait_*` helpers look again.
///
/// Ten milliseconds: fast enough that a test measuring how long a retile took
/// is not reading the poll interval instead, slow enough that a wait costs
/// nothing while a layout settles.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// A monotonic deadline. `Instant` never goes backwards, so a wait cannot be
/// extended or cut short by the wall clock being adjusted under it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadline {
    start: std::time::Instant,
    timeout: std::time::Duration,
}

impl Deadline {
    /// Starts a deadline `timeout` from now.
    pub(crate) fn new(timeout: std::time::Duration) -> Self {
        Self {
            start: std::time::Instant::now(),
            timeout,
        }
    }

    /// True once the timeout has passed.
    pub(crate) fn passed(&self) -> bool {
        self.start.elapsed() >= self.timeout
    }

    /// How long the caller has been waiting.
    pub(crate) fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }
}

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
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;

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
        ex_style,
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        cloaked: cloaked_raw(hwnd),
        foreground: unsafe { GetForegroundWindow() } == hwnd,
        alpha: alpha_raw(hwnd, ex_style),
        owner: owner_raw(hwnd),
    })
}

/// `DWMWA_CLOAKED`: true when DWM is hiding the window, which is how a tiling
/// manager hides a workspace without touching the window's own visibility.
#[must_use]
pub fn cloaked(hwnd: i64) -> bool {
    cloaked_raw(as_hwnd(hwnd))
}

fn cloaked_raw(hwnd: HWND) -> bool {
    let mut value: u32 = 0;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            std::ptr::from_mut(&mut value).cast(),
            size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && value != 0
}

/// Cloaks or uncloaks a window through DWM, the way a manager hides one.
///
/// # Errors
/// When DWM refuses the attribute, which means the window is gone.
pub fn set_cloaked(hwnd: i64, cloaked: bool) -> Result<()> {
    let value: u32 = u32::from(cloaked);
    unsafe {
        DwmSetWindowAttribute(
            as_hwnd(hwnd),
            DWMWA_CLOAK,
            std::ptr::from_ref(&value).cast(),
            size_of::<u32>() as u32,
        )
    }
    .map_err(|e| Error::win32("DwmSetWindowAttribute", &e))
}

/// The layered alpha of a window, or `None` when nothing made it layered.
#[must_use]
pub fn window_alpha(hwnd: i64) -> Option<u8> {
    let target = as_hwnd(hwnd);
    let ex_style = unsafe { GetWindowLongPtrW(target, GWL_EXSTYLE) } as u32;
    alpha_raw(target, ex_style)
}

fn alpha_raw(hwnd: HWND, ex_style: u32) -> Option<u8> {
    if ex_style & WS_EX_LAYERED_BIT == 0 {
        return None;
    }
    let mut alpha: u8 = 0;
    let mut flags = LWA_ALPHA;
    let ok = unsafe {
        GetLayeredWindowAttributes(hwnd, None, Some(&mut alpha), Some(&mut flags)).is_ok()
    };
    // A layered window whose alpha was never set reports LWA_COLORKEY only.
    (ok && flags.0 & LWA_ALPHA.0 != 0).then_some(alpha)
}

/// Gives a window a transparency, or takes it away again, the way the visuals
/// module of a window manager does.
///
/// # Errors
/// When the window is gone or refuses the layered attribute.
pub fn set_window_alpha(hwnd: i64, alpha: Option<u8>) -> Result<()> {
    let target = as_hwnd(hwnd);
    if !window_exists(hwnd) {
        return Err(Error::not_found(format!("hwnd 0x{hwnd:x}")));
    }
    let ex_style = unsafe { GetWindowLongPtrW(target, GWL_EXSTYLE) } as u32;
    match alpha {
        Some(value) => {
            if ex_style & WS_EX_LAYERED_BIT == 0 {
                unsafe {
                    SetWindowLongPtrW(target, GWL_EXSTYLE, (ex_style | WS_EX_LAYERED_BIT) as isize)
                };
            }
            unsafe { SetLayeredWindowAttributes(target, COLORREF(0), value, LWA_ALPHA) }
                .map_err(|e| Error::win32("SetLayeredWindowAttributes", &e))
        }
        None => {
            if ex_style & WS_EX_LAYERED_BIT != 0 {
                unsafe {
                    SetWindowLongPtrW(
                        target,
                        GWL_EXSTYLE,
                        (ex_style & !WS_EX_LAYERED_BIT) as isize,
                    )
                };
            }
            Ok(())
        }
    }
}

/// The owner of an owned popup, `None` for an ordinary top level window.
#[must_use]
pub fn window_owner(hwnd: i64) -> Option<i64> {
    owner_raw(as_hwnd(hwnd))
}

fn owner_raw(hwnd: HWND) -> Option<i64> {
    match unsafe { GetWindow(hwnd, GW_OWNER) } {
        Ok(owner) if !owner.is_invalid() => Some(as_i64(owner)),
        _ => None,
    }
}

/// The window in the foreground right now, as a handle number.
#[must_use]
pub fn foreground_window() -> i64 {
    as_i64(unsafe { GetForegroundWindow() })
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
    let deadline = Deadline::new(timeout);
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
        if deadline.passed() {
            return Err(Error::timeout(format!(
                "the {what} of hwnd 0x{hwnd:x} after {:?}, last seen {}",
                deadline.elapsed(),
                last.map_or_else(|| "nothing".to_string(), |r| r.to_string())
            )));
        }
        std::thread::sleep(POLL);
    }
}

/// Waits until a freshly created window is visible and its frame has stopped
/// moving: two identical, non-empty readings of the extended frame bounds a
/// poll apart.
///
/// This is what `spawn` waits on. Without it a test can read the rect DWM had
/// while the window was still being shown, and a layout assertion then fails
/// against a window that was never there.
///
/// # Errors
/// [`Error::Timeout`] when the window never settled, [`Error::NotFound`] when
/// it died while waiting.
pub fn wait_until_settled(hwnd: i64, timeout: std::time::Duration) -> Result<Rect> {
    let deadline = Deadline::new(timeout);
    let mut previous: Option<Rect> = None;
    loop {
        if !window_exists(hwnd) {
            return Err(Error::not_found(format!(
                "hwnd 0x{hwnd:x} died before it settled"
            )));
        }

        let visible = unsafe { IsWindowVisible(as_hwnd(hwnd)) }.as_bool();
        let frame = frame_bounds(hwnd).ok().filter(|r| !r.is_empty());
        if visible && let Some(frame) = frame {
            if previous == Some(frame) {
                return Ok(frame);
            }
            previous = Some(frame);
        } else {
            previous = None;
        }

        if deadline.passed() {
            return Err(Error::timeout(format!(
                "hwnd 0x{hwnd:x} to be visible with a stable frame after {:?}, last seen {}",
                deadline.elapsed(),
                previous.map_or_else(|| "nothing".to_string(), |r| r.to_string())
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
    let deadline = Deadline::new(timeout);
    loop {
        let alive: Vec<String> = hwnds
            .iter()
            .filter(|&&h| window_exists(h))
            .map(|h| format!("0x{h:x}"))
            .collect();
        if alive.is_empty() {
            return Ok(());
        }
        if deadline.passed() {
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
