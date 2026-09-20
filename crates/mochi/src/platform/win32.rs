//! The real Win32 implementation of [`Platform`].
//!
//! Reads are safe to run next to another window manager. Writes are the part
//! that moves windows, and every one of them is a single method here so that
//! `--dry-run` can shadow the lot.

use std::ffi::c_void;

use anyhow::{Context, Result, anyhow};
use mochi_core::Rect;

use windows::Win32::Foundation::{COLORREF, CloseHandle, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAK, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONULL, MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    BeginDeferWindowPos, BringWindowToTop, DeferWindowPos, EndDeferWindowPos, EnumChildWindows,
    EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW,
    GetCursorPos, GetForegroundWindow, GetShellWindow, GetWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST,
    IsIconic, IsWindow, IsWindowVisible, IsZoomed, LWA_ALPHA, PostMessageW, SET_WINDOW_POS_FLAGS,
    SW_HIDE, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetCursorPos, SetForegroundWindow,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_CLOSE,
    WindowFromPoint,
};

use super::appview::{self, ViewCloakError};
use super::types::{Hwnd, MonitorId, MonitorInfo, WindowInfo, ex_style};
use super::wide::{file_name, from_wide};
use super::{CloakUnsupported, Platform, ShowState, WindowPlacement, ZOrder};

/// `MONITORINFOF_PRIMARY`, missing from the `windows` crate metadata.
const MONITORINFOF_PRIMARY: u32 = 1;

/// The process that hosts every UWP window.
///
/// Reading the executable off the window itself makes Calculator, Settings and
/// the Store the same program, so an `exe` rule can neither pick one out nor
/// leave the others alone. [`hosted_process`] looks past it.
const FRAME_HOST: &str = "ApplicationFrameHost.exe";

/// Longest window title Mochi reads. Longer titles are truncated, not rejected.
const TITLE_BUFFER: usize = 512;
/// Longest class name Win32 allows is 256 characters, plus the NUL that
/// `GetClassNameW` always writes: it copies at most `nMaxCount - 1` characters,
/// so a 256 buffer silently truncates a maximum length class name.
const CLASS_BUFFER: usize = 257;
/// `MAX_PATH` is a lie on modern Windows, so use a generous buffer.
const PATH_BUFFER: usize = 1024;

/// Talks to the real desktop.
#[derive(Debug, Clone, Copy, Default)]
pub struct Win32Platform;

impl Win32Platform {
    /// Creates the platform. Cheap, holds nothing.
    pub const fn new() -> Self {
        Self
    }

    /// Fallback for [`Platform::set_positions`] when a batch has to be abandoned.
    ///
    /// Slower and not atomic, but it moves the windows it can and reports the
    /// ones it cannot instead of losing the whole layout.
    fn set_positions_individually(&self, placements: &[WindowPlacement]) -> Result<()> {
        let mut failed = 0usize;
        for p in placements {
            if !is_window(p.hwnd) {
                continue;
            }
            let h = hwnd(p.hwnd);
            let target = compensate_invisible_border(window_rect(h), dwm_frame(h), p.rect);
            let (insert_after, z_flags) = z_order_args(p.z);
            if let Err(e) = unsafe {
                SetWindowPos(
                    h,
                    insert_after,
                    target.left,
                    target.top,
                    target.width(),
                    target.height(),
                    MOVE_FLAGS | z_flags,
                )
            } {
                failed += 1;
                tracing::debug!(hwnd = %p.hwnd, error = %e, "SetWindowPos failed");
            }
        }
        if failed == 0 {
            Ok(())
        } else {
            Err(anyhow!(
                "{failed} of {} windows could not be positioned",
                placements.len()
            ))
        }
    }
}

/// `GetWindowRect`, or an empty rectangle when the window is already gone.
fn window_rect(h: HWND) -> Rect {
    let mut r = RECT::default();
    if unsafe { GetWindowRect(h, &raw mut r) }.is_ok() {
        rect(r)
    } else {
        Rect::default()
    }
}

fn hwnd(h: Hwnd) -> HWND {
    HWND(h.0 as *mut c_void)
}

fn from_hwnd(h: HWND) -> Hwnd {
    Hwnd(h.0 as isize)
}

fn rect(r: RECT) -> Rect {
    Rect::new(r.left, r.top, r.right, r.bottom)
}

/// True when the handle still refers to a live window.
pub fn is_window(h: Hwnd) -> bool {
    !h.is_null() && unsafe { IsWindow(Some(hwnd(h))) }.as_bool()
}

// ---------------------------------------------------------------------------
// reads
// ---------------------------------------------------------------------------

unsafe extern "system" fn collect_monitors(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    lparam: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: lparam is the Vec we handed to EnumDisplayMonitors below.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HMONITOR>) };
    list.push(monitor);
    true.into()
}

unsafe extern "system" fn collect_windows(window: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam is the Vec we handed to EnumWindows below.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(window);
    true.into()
}

/// Enumerates raw monitor handles in the order Windows reports them.
fn monitor_handles() -> Vec<HMONITOR> {
    let mut list: Vec<HMONITOR> = Vec::with_capacity(4);
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitors),
            LPARAM(&raw mut list as isize),
        )
    };
    if !ok.as_bool() {
        tracing::warn!("EnumDisplayMonitors reported a failure, monitor list may be short");
    }
    list
}

/// Enumerates raw top-level window handles.
fn window_handles() -> Result<Vec<HWND>> {
    let mut list: Vec<HWND> = Vec::with_capacity(128);
    unsafe { EnumWindows(Some(collect_windows), LPARAM(&raw mut list as isize)) }
        .context("EnumWindows failed")?;
    Ok(list)
}

/// Reads the friendly monitor name, for example `Odyssey G80SD`.
fn device_description(device_name: &str) -> String {
    let wide = super::wide::to_wide(device_name);
    let mut dd = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let ok =
        unsafe { EnumDisplayDevicesW(windows::core::PCWSTR(wide.as_ptr()), 0, &raw mut dd, 0) };
    if ok.as_bool() {
        from_wide(&dd.DeviceString)
    } else {
        String::new()
    }
}

fn monitor_info(monitor: HMONITOR) -> Result<MonitorInfo> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    if !ok.as_bool() {
        return Err(anyhow!("GetMonitorInfoW failed for {monitor:?}"));
    }

    let device_name = from_wide(&info.szDevice);
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    if let Err(e) =
        unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y) }
    {
        tracing::debug!(%device_name, error = %e, "GetDpiForMonitor failed, assuming 96");
    }

    Ok(MonitorInfo {
        id: MonitorId(monitor.0 as isize),
        device_description: device_description(&device_name),
        device_name,
        size: rect(info.monitorInfo.rcMonitor),
        work_area: rect(info.monitorInfo.rcWork),
        dpi: dpi_x,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

fn window_title(h: HWND) -> String {
    let mut buf = [0u16; TITLE_BUFFER];
    let len = unsafe { GetWindowTextW(h, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        from_wide(&buf[..len as usize])
    }
}

fn window_class(h: HWND) -> String {
    let mut buf = [0u16; CLASS_BUFFER];
    let len = unsafe { GetClassNameW(h, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        from_wide(&buf[..len as usize])
    }
}

/// Full image path of the process that owns a window.
///
/// Empty for elevated processes when Mochi is not elevated: `OpenProcess`
/// refuses even `PROCESS_QUERY_LIMITED_INFORMATION` across that boundary.
fn process_path(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return String::new(),
    };
    let mut buf = [0u16; PATH_BUFFER];
    let mut len = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    match result {
        Ok(()) => from_wide(&buf[..len as usize]),
        Err(_) => String::new(),
    }
}

fn dwm_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && cloaked != 0
}

fn dwm_frame(h: HWND) -> Rect {
    let mut r = RECT::default();
    let ok = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    };
    if ok.is_ok() { rect(r) } else { Rect::default() }
}

/// What [`hosted_child`] carries through `EnumChildWindows`.
struct HostedSearch {
    /// The frame host's own process, which every uninteresting child shares.
    host: u32,
    /// The first child process that is not the host's.
    found: u32,
}

/// The process of the application inside a UWP frame host window.
///
/// The application owns a child window in its own process, so the first child
/// whose process is not the host's is the application itself. `None` when the
/// window has no such child, which is what a frame host with nothing in it
/// looks like while the app is still starting.
fn hosted_process(h: HWND, host: u32) -> Option<u32> {
    let mut search = HostedSearch { host, found: 0 };
    // SAFETY: the callback only reads the HostedSearch this pointer came from,
    // and the enumeration finishes before this function returns.
    let _ = unsafe {
        EnumChildWindows(
            Some(h),
            Some(hosted_child),
            LPARAM(std::ptr::from_mut(&mut search) as isize),
        )
    };
    (search.found != 0).then_some(search.found)
}

unsafe extern "system" fn hosted_child(child: HWND, lparam: LPARAM) -> windows::core::BOOL {
    // SAFETY: lparam is the HostedSearch that hosted_process just handed over.
    let search = unsafe { &mut *(lparam.0 as *mut HostedSearch) };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(child, Some(&raw mut pid)) };
    if pid != 0 && pid != search.host {
        search.found = pid;
        return false.into();
    }
    true.into()
}

/// The process a window should be reported as, looking past the UWP host.
///
/// `hosted` is only consulted for a frame host window, and only replaces it
/// when it answers with a path: a UWP window whose application cannot be read
/// is still better described by the host than by nothing at all.
fn resolve_process(
    pid: u32,
    path: String,
    hosted: impl FnOnce() -> Option<(u32, String)>,
) -> (u32, String) {
    if file_name(&path).eq_ignore_ascii_case(FRAME_HOST)
        && let Some((app_pid, app_path)) = hosted()
        && !app_path.is_empty()
    {
        return (app_pid, app_path);
    }
    (pid, path)
}

fn read_window(h: HWND) -> WindowInfo {
    let mut host_pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&raw mut host_pid)) };
    let (pid, path) = resolve_process(host_pid, process_path(host_pid), || {
        hosted_process(h, host_pid).map(|app| (app, process_path(app)))
    });

    let mut r = RECT::default();
    let window_rect = if unsafe { GetWindowRect(h, &raw mut r) }.is_ok() {
        rect(r)
    } else {
        Rect::default()
    };

    let owner = unsafe { GetWindow(h, GW_OWNER) }.ok().map(from_hwnd);
    let monitor = {
        let m = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONULL) };
        if m.is_invalid() {
            None
        } else {
            Some(MonitorId(m.0 as isize))
        }
    };

    WindowInfo {
        hwnd: from_hwnd(h),
        title: window_title(h),
        class: window_class(h),
        exe: file_name(&path).to_owned(),
        path,
        pid,
        style: unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32,
        ex_style: unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32,
        rect: window_rect,
        frame: dwm_frame(h),
        cloaked: dwm_cloaked(h),
        visible: unsafe { IsWindowVisible(h) }.as_bool(),
        minimized: unsafe { IsIconic(h) }.as_bool(),
        maximized: unsafe { IsZoomed(h) }.as_bool(),
        owner,
        monitor,
    }
}

// ---------------------------------------------------------------------------
// writes
// ---------------------------------------------------------------------------

/// Converts a target frame rectangle into the rectangle `SetWindowPos` wants.
///
/// Since Vista a window rect is bigger than what the user sees: the resize grip
/// is drawn outside the visible frame. Tiling against the window rect leaves
/// visible gaps, so Mochi tiles against `DWMWA_EXTENDED_FRAME_BOUNDS` and adds
/// the difference back here.
pub fn compensate_invisible_border(window_rect: Rect, frame: Rect, target: Rect) -> Rect {
    if frame.width() <= 0 || frame.height() <= 0 || window_rect.width() <= 0 {
        return target;
    }
    Rect::new(
        target.left - (frame.left - window_rect.left),
        target.top - (frame.top - window_rect.top),
        target.right + (window_rect.right - frame.right),
        target.bottom + (window_rect.bottom - frame.bottom),
    )
}

fn z_order_args(z: ZOrder) -> (Option<HWND>, SET_WINDOW_POS_FLAGS) {
    match z {
        ZOrder::Keep => (None, SWP_NOZORDER),
        ZOrder::Top => (Some(HWND_TOP), SET_WINDOW_POS_FLAGS(0)),
        ZOrder::Bottom => (Some(HWND_BOTTOM), SET_WINDOW_POS_FLAGS(0)),
        ZOrder::TopMost => (Some(HWND_TOPMOST), SET_WINDOW_POS_FLAGS(0)),
        ZOrder::NoTopMost => (Some(HWND_NOTOPMOST), SET_WINDOW_POS_FLAGS(0)),
    }
}

/// Flags shared by every Mochi move: never steal focus, never repaint twice.
/// Flags shared by the batched and the one at a time move paths.
///
/// `SWP_NOSENDCHANGING` is deliberately absent: `DeferWindowPos` rejects it
/// with `ERROR_INVALID_PARAMETER`, which would push every layout onto the slow
/// path.
const MOVE_FLAGS: SET_WINDOW_POS_FLAGS =
    SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0 | SWP_FRAMECHANGED.0);

/// Whether [`take_foreground`] also pulls the window to the top of the z-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Raise {
    Yes,
    No,
}

/// Hands the keyboard to `target`, thread-input dance included.
///
/// Windows only lets the foreground thread give focus away. Attaching our input
/// queue to both the outgoing and the incoming thread borrows that right for the
/// duration of the call. Returns whether `SetForegroundWindow` accepted it.
fn take_foreground(target: HWND, raise: Raise) -> bool {
    let current = unsafe { GetCurrentThreadId() };
    let target_thread = unsafe { GetWindowThreadProcessId(target, None) };
    let foreground = unsafe { GetForegroundWindow() };
    let foreground_thread = if foreground.is_invalid() {
        0
    } else {
        unsafe { GetWindowThreadProcessId(foreground, None) }
    };

    let attached_foreground = foreground_thread != 0
        && foreground_thread != current
        && unsafe { AttachThreadInput(current, foreground_thread, true) }.as_bool();
    let attached_target = target_thread != 0
        && target_thread != current
        && target_thread != foreground_thread
        && unsafe { AttachThreadInput(current, target_thread, true) }.as_bool();

    if raise == Raise::Yes && unsafe { IsIconic(target) }.as_bool() {
        let _ = unsafe { ShowWindow(target, SW_RESTORE) };
    }
    let ok = unsafe { SetForegroundWindow(target) }.as_bool();
    if raise == Raise::Yes {
        let _ = unsafe { BringWindowToTop(target) };
    }

    if attached_target {
        let _ = unsafe { AttachThreadInput(current, target_thread, false) };
    }
    if attached_foreground {
        let _ = unsafe { AttachThreadInput(current, foreground_thread, false) };
    }
    ok
}

impl Platform for Win32Platform {
    fn name(&self) -> &'static str {
        "win32"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        let mut out = Vec::new();
        for m in monitor_handles() {
            match monitor_info(m) {
                Ok(info) => out.push(info),
                Err(e) => tracing::warn!(error = %e, "skipping a monitor"),
            }
        }
        Ok(out)
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        let handles = window_handles()?;
        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            out.push(read_window(h));
        }
        Ok(out)
    }

    fn window_info(&self, h: Hwnd) -> Result<WindowInfo> {
        if !is_window(h) {
            return Err(anyhow!("{h} is not a window"));
        }
        Ok(read_window(hwnd(h)))
    }

    fn foreground_window(&self) -> Option<Hwnd> {
        let h = unsafe { GetForegroundWindow() };
        if h.is_invalid() {
            None
        } else {
            Some(from_hwnd(h))
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<Hwnd> {
        let h = unsafe { WindowFromPoint(POINT { x, y }) };
        if h.is_invalid() {
            return None;
        }
        let root = unsafe { GetAncestor(h, GA_ROOT) };
        if root.is_invalid() {
            Some(from_hwnd(h))
        } else {
            Some(from_hwnd(root))
        }
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&raw mut p) }.context("GetCursorPos failed")?;
        Ok((p.x, p.y))
    }

    fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
        if placements.is_empty() {
            return Ok(());
        }
        let mut hdwp = unsafe { BeginDeferWindowPos(placements.len() as i32) }
            .context("BeginDeferWindowPos failed")?;

        for p in placements {
            if !is_window(p.hwnd) {
                tracing::debug!(hwnd = %p.hwnd, "skipping a placement for a dead window");
                continue;
            }
            let h = hwnd(p.hwnd);
            let target = compensate_invisible_border(window_rect(h), dwm_frame(h), p.rect);
            let (insert_after, z_flags) = z_order_args(p.z);
            match unsafe {
                DeferWindowPos(
                    hdwp,
                    h,
                    insert_after,
                    target.left,
                    target.top,
                    target.width(),
                    target.height(),
                    MOVE_FLAGS | z_flags,
                )
            } {
                Ok(next) => hdwp = next,
                Err(e) => {
                    // Microsoft is explicit about this: "If a call to
                    // DeferWindowPos fails, the application should abandon the
                    // window-positioning operation and not call
                    // EndDeferWindowPos." So the batch is dropped on the floor
                    // rather than ended, which is why there is no cleanup here.
                    //
                    // Dropping the batch would leave every window in it where it
                    // was, so the rest of the work is finished one window at a
                    // time instead. A half applied layout is bad; a layout that
                    // silently did nothing is worse, because the user cannot
                    // tell it from a hang.
                    tracing::warn!(
                        hwnd = %p.hwnd,
                        error = %e,
                        "DeferWindowPos failed, finishing this layout one window at a time"
                    );
                    return self.set_positions_individually(placements);
                }
            }
        }

        unsafe { EndDeferWindowPos(hdwp) }.context("EndDeferWindowPos failed")
    }

    fn set_cloaked(&self, h: Hwnd, cloaked: bool) -> Result<()> {
        // The shell cloaks any window it tracks, whichever process owns it.
        match appview::set_cloak(hwnd(h), cloaked) {
            Ok(()) => return Ok(()),
            Err(e @ ViewCloakError::NoView) => {
                tracing::debug!(hwnd = %h, error = %e, "falling back to DWM cloaking");
            }
            Err(e) => tracing::warn!(hwnd = %h, error = %e, "falling back to DWM cloaking"),
        }
        // DWM only cloaks windows of the calling process, so this covers
        // Mochi's own windows and nothing else.
        let value: i32 = i32::from(cloaked);
        let dwm = unsafe {
            DwmSetWindowAttribute(
                hwnd(h),
                DWMWA_CLOAK,
                (&raw const value).cast::<c_void>(),
                size_of::<i32>() as u32,
            )
        };
        dwm.map_err(|e| {
            anyhow::Error::new(CloakUnsupported)
                .context(format!("DWMWA_CLOAK={cloaked} failed for {h}: {e}"))
        })
    }

    fn show(&self, h: Hwnd, state: ShowState) -> Result<()> {
        let cmd = match state {
            ShowState::Restore => SW_RESTORE,
            ShowState::Minimize => SW_MINIMIZE,
            ShowState::Maximize => SW_MAXIMIZE,
            ShowState::ShowNoActivate => SW_SHOWNOACTIVATE,
            ShowState::Hide => SW_HIDE,
        };
        // ShowWindow returns the previous visibility, not a success flag.
        let _ = unsafe { ShowWindow(hwnd(h), cmd) };
        Ok(())
    }

    fn focus(&self, h: Hwnd) -> Result<()> {
        if !is_window(h) {
            return Err(anyhow!("{h} is not a window"));
        }
        if take_foreground(hwnd(h), Raise::Yes) {
            Ok(())
        } else {
            Err(anyhow!("SetForegroundWindow refused to focus {h}"))
        }
    }

    fn focus_desktop(&self) -> Result<()> {
        let shell = unsafe { GetShellWindow() };
        if shell.is_invalid() {
            return Err(anyhow!("there is no shell window to hand the keyboard to"));
        }
        // Not raised. The desktop belongs underneath everything, and asking for
        // it to be brought to the top is the one thing here that could put it
        // over the windows still visible on the other screen.
        if take_foreground(shell, Raise::No) {
            Ok(())
        } else {
            Err(anyhow!("SetForegroundWindow refused to focus the desktop"))
        }
    }

    fn close(&self, h: Hwnd) -> Result<()> {
        unsafe { PostMessageW(Some(hwnd(h)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
            .with_context(|| format!("WM_CLOSE to {h} failed"))
    }

    fn set_transparency(&self, h: Hwnd, alpha: Option<u8>) -> Result<()> {
        let target = hwnd(h);
        let current = unsafe { GetWindowLongPtrW(target, GWL_EXSTYLE) } as u32;
        match alpha {
            Some(a) => {
                if current & ex_style::WS_EX_LAYERED == 0 {
                    unsafe {
                        SetWindowLongPtrW(
                            target,
                            GWL_EXSTYLE,
                            (current | ex_style::WS_EX_LAYERED) as isize,
                        )
                    };
                }
                unsafe { SetLayeredWindowAttributes(target, COLORREF(0), a, LWA_ALPHA) }
                    .with_context(|| format!("SetLayeredWindowAttributes failed for {h}"))
            }
            None => {
                if current & ex_style::WS_EX_LAYERED != 0 {
                    // Go fully opaque first, otherwise the window can flash.
                    let _ =
                        unsafe { SetLayeredWindowAttributes(target, COLORREF(0), 255, LWA_ALPHA) };
                    unsafe {
                        SetWindowLongPtrW(
                            target,
                            GWL_EXSTYLE,
                            (current & !ex_style::WS_EX_LAYERED) as isize,
                        )
                    };
                }
                Ok(())
            }
        }
    }

    fn set_topmost(&self, h: Hwnd, topmost: bool) -> Result<()> {
        let insert_after = if topmost {
            HWND_TOPMOST
        } else {
            HWND_NOTOPMOST
        };
        unsafe {
            SetWindowPos(
                hwnd(h),
                Some(insert_after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        }
        .with_context(|| format!("SetWindowPos topmost={topmost} failed for {h}"))
    }

    fn set_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        unsafe { SetCursorPos(x, y) }.context("SetCursorPos failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn an_ordinary_window_is_its_own_process() {
        let asked = Cell::new(false);
        let (pid, path) =
            resolve_process(42, r"C:\Program Files\Editor\Code.exe".to_owned(), || {
                asked.set(true);
                None
            });
        assert_eq!(pid, 42);
        assert_eq!(path, r"C:\Program Files\Editor\Code.exe");
        assert!(
            !asked.get(),
            "only a frame host window is worth a child walk"
        );
    }

    #[test]
    fn a_uwp_window_is_the_application_inside_the_host() {
        let host = format!(r"C:\Windows\System32\{FRAME_HOST}");
        let (pid, path) = resolve_process(42, host, || {
            Some((
                77,
                r"C:\Program Files\WindowsApps\CalculatorApp.exe".to_owned(),
            ))
        });
        assert_eq!(pid, 77);
        assert_eq!(file_name(&path), "CalculatorApp.exe");
    }

    #[test]
    fn a_host_with_nothing_in_it_stays_the_host() {
        let host = format!(r"C:\Windows\System32\{FRAME_HOST}");
        let (pid, path) = resolve_process(42, host.clone(), || None);
        assert_eq!(pid, 42);
        assert_eq!(path, host);

        // A child whose process cannot be opened reads as an empty path, and
        // that is worse than naming the host.
        let (pid, path) = resolve_process(42, host.clone(), || Some((77, String::new())));
        assert_eq!(pid, 42);
        assert_eq!(path, host);
    }

    #[test]
    fn border_compensation_grows_the_target_by_the_invisible_frame() {
        // A typical Chromium window: 7px of invisible border left, right, bottom.
        let window_rect = Rect::new(-7, 0, 1287, 807);
        let frame = Rect::new(0, 0, 1280, 800);
        let target = Rect::new(100, 100, 1100, 900);
        assert_eq!(
            compensate_invisible_border(window_rect, frame, target),
            Rect::new(93, 100, 1107, 907)
        );
    }

    #[test]
    fn border_compensation_is_a_no_op_without_a_dwm_frame() {
        let target = Rect::new(10, 10, 110, 110);
        assert_eq!(
            compensate_invisible_border(Rect::new(0, 0, 100, 100), Rect::default(), target),
            target
        );
    }

    #[test]
    fn enumerating_this_desktop_finds_monitors_and_windows() {
        let p = Win32Platform::new();
        let monitors = p.monitors().unwrap();
        assert!(!monitors.is_empty(), "a desktop has at least one monitor");
        for m in &monitors {
            assert!(m.size.width() > 0 && m.size.height() > 0);
            assert!(m.dpi >= 96);
            assert!(m.device_name.starts_with(r"\\.\DISPLAY"));
        }
        assert!(
            p.windows().unwrap().len() > 1,
            "a running desktop has windows"
        );
    }

    #[test]
    fn a_dead_handle_is_not_a_window() {
        assert!(!is_window(Hwnd::NULL));
        assert!(!is_window(Hwnd(0x7fff_ffff)));
    }
}
