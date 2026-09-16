//! The test window itself, and the [`TestWindows`] handle that owns a batch of
//! them.
//!
//! One window is one thread with its own message loop, all inside a single
//! process. That is the honest shape for a tiling test: a window manager moves
//! windows from the outside with `SetWindowPos`, and a window that shares a
//! message loop with its neighbours would hide exactly the cross-thread
//! behaviour that goes wrong in practice.
//!
//! The windows are created with `WS_EX_TOOLWINDOW` and the class
//! [`crate::TEST_WINDOW_CLASS`]. Tool windows are ignored by every window
//! manager that follows the usual manageability rules, so a
//! batch of these can be spawned on a working desktop without another manager
//! grabbing them, and without them showing up in the taskbar or in Alt-Tab.

use std::ffi::c_void;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DT_CENTER,
    DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint,
    FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FW_SEMIBOLD, FillRect, HBRUSH, HDC, InvalidateRect,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW, HICON,
    IDC_ARROW, IsIconic, LoadCursorW, MINMAXINFO, MSG, PostQuitMessage, RegisterClassW, SW_SHOWNA,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, WINDOW_LONG_PTR_INDEX, WM_CLOSE, WM_DESTROY,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_SETTEXT, WM_WINDOWPOSCHANGED, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
};
use windows::core::PCWSTR;

use crate::error::{Error, Result, hresult_from_win32};
use crate::event::{EventKind, WindowEvent, now_ms};
use crate::geometry::Rect;
use crate::info::{MonitorInfo, TestWindowInfo};
use crate::wide::{to_wide, to_wide_unterminated};
use crate::win32;
use crate::{DEFAULT_TITLE_PREFIX, TEST_WINDOW_CLASS};

/// Where the pointer to the per-window state lives, in the class extra bytes.
const STATE_SLOT: WINDOW_LONG_PTR_INDEX = WINDOW_LONG_PTR_INDEX(0);

/// One based, shared by every batch in the process so two `spawn` calls never
/// produce two windows called `MochiTest 1`.
static NEXT_INDEX: AtomicU32 = AtomicU32::new(1);

/// How often the close path looks whether the windows are gone.
const POLL: Duration = Duration::from_millis(15);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | (r as u32))
}

/// Pastel fills, one per window, so a screenshot of a tiling run can be read at
/// a glance. Deliberately light: the label is drawn in a dark ink.
const PALETTE: [COLORREF; 8] = [
    rgb(244, 187, 205),
    rgb(186, 224, 210),
    rgb(190, 215, 243),
    rgb(219, 205, 240),
    rgb(247, 217, 196),
    rgb(250, 237, 203),
    rgb(201, 228, 222),
    rgb(242, 198, 222),
];

/// The label colour.
const INK: COLORREF = rgb(44, 44, 64);

/// What a batch of test windows should look like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnOptions {
    /// How many windows to create.
    pub count: u32,
    /// Index into [`crate::monitors`], in `EnumDisplayMonitors` order.
    pub monitor: usize,
    /// Title prefix; the window index is appended, as in `MochiTest 2`.
    pub title_prefix: String,
    /// Print the JSON event stream to stdout. The binary turns this on, a test
    /// that only reads rects leaves it off to keep its output clean.
    pub emit_events: bool,
    /// Window size in physical pixels. `None` picks something that fits the
    /// monitor and scales with its DPI.
    pub size: Option<(i32, i32)>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            count: 3,
            monitor: 0,
            title_prefix: DEFAULT_TITLE_PREFIX.to_string(),
            emit_events: false,
            size: None,
        }
    }
}

impl SpawnOptions {
    /// Options for `count` windows on `monitor`, everything else default.
    #[must_use]
    pub fn new(count: u32, monitor: usize) -> Self {
        Self {
            count,
            monitor,
            ..Self::default()
        }
    }
}

/// One window of a batch, as the owner sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestWindow {
    /// The handle, as a plain number.
    pub hwnd: i64,
    /// The index that went into the title.
    pub index: u32,
    /// The title the window was created with.
    pub title: String,
}

/// A batch of live test windows. Dropping it closes every window it owns.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::time::Duration;
/// use mochi_testbed::TestWindows;
///
/// let windows = TestWindows::spawn(4, 0)?;
/// let first = windows.handles()[0];
/// // ... start the daemon with --manage-class MochiTestWindow ...
/// let tiled = windows.wait_for_rect(first, |r| r.width() > 0, Duration::from_secs(5))?;
/// println!("first window ended up at {tiled}");
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct TestWindows {
    spawned: Vec<TestWindow>,
    threads: Vec<JoinHandle<()>>,
    alive: Arc<AtomicUsize>,
}

impl TestWindows {
    /// Spawns `count` windows on the monitor at `monitor`, staggered so every
    /// one of them is visible and clickable before anything tiles them.
    ///
    /// # Errors
    /// When the monitor index does not exist or a window cannot be created.
    pub fn spawn(count: u32, monitor: usize) -> Result<Self> {
        Self::spawn_with(&SpawnOptions::new(count, monitor))
    }

    /// Spawns a batch described by [`SpawnOptions`].
    ///
    /// # Errors
    /// When the monitor index does not exist, a thread cannot be started, or a
    /// window is not created within ten seconds.
    pub fn spawn_with(options: &SpawnOptions) -> Result<Self> {
        win32::ensure_per_monitor_v2();
        let monitor = win32::monitor_at(options.monitor)?;

        let mut batch = Self {
            spawned: Vec::new(),
            threads: Vec::new(),
            alive: Arc::new(AtomicUsize::new(0)),
        };

        for slot in 0..options.count {
            let index = NEXT_INDEX.fetch_add(1, Ordering::SeqCst);
            let title = format!("{} {index}", options.title_prefix);
            let rect = placement(&monitor, slot, options.size);
            let color = PALETTE[(index as usize) % PALETTE.len()];
            let emit = options.emit_events;

            let (tx, rx) = mpsc::channel::<std::result::Result<i64, String>>();
            let alive = Arc::clone(&batch.alive);
            alive.fetch_add(1, Ordering::SeqCst);

            let thread_title = title.clone();
            let thread = std::thread::Builder::new()
                .name(format!("mochi-testwin-{index}"))
                .spawn(move || {
                    match unsafe { create_window(index, &thread_title, rect, color, emit) } {
                        Ok(hwnd) => {
                            let _ = tx.send(Ok(win32::as_i64(hwnd)));
                            drop(tx);
                            unsafe { pump_messages() };
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                        }
                    }
                    alive.fetch_sub(1, Ordering::SeqCst);
                })
                .map_err(Error::Io)?;

            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(hwnd)) => {
                    batch.spawned.push(TestWindow { hwnd, index, title });
                    batch.threads.push(thread);
                }
                Ok(Err(message)) => return Err(Error::Other(message)),
                Err(_) => {
                    return Err(Error::timeout(format!("test window {index} to be created")));
                }
            }
        }

        Ok(batch)
    }

    /// The handles of the batch, in spawn order.
    #[must_use]
    pub fn handles(&self) -> Vec<i64> {
        self.spawned.iter().map(|w| w.hwnd).collect()
    }

    /// What the batch looked like at spawn time: handle, index and title.
    #[must_use]
    pub fn spawned(&self) -> &[TestWindow] {
        &self.spawned
    }

    /// The live state of every window of the batch. Windows that have been
    /// closed behind the handle's back are left out, so this is also the way to
    /// see that a `close --hwnd` from another process took effect.
    #[must_use]
    pub fn windows(&self) -> Vec<TestWindowInfo> {
        self.spawned
            .iter()
            .filter_map(|w| win32::window_info(w.hwnd).ok())
            .collect()
    }

    /// How many windows the batch started with and has not closed yet.
    #[must_use]
    pub fn len(&self) -> usize {
        self.spawned.len()
    }

    /// True when the batch owns no window.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spawned.is_empty()
    }

    /// How many of the windows are still alive on the desktop.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.spawned
            .iter()
            .filter(|w| win32::window_exists(w.hwnd))
            .count()
    }

    /// Waits until `GetWindowRect` of one window satisfies the predicate.
    ///
    /// # Errors
    /// [`Error::Timeout`] when it never did, [`Error::NotFound`] when the
    /// window died while waiting.
    pub fn wait_for_rect(
        &self,
        hwnd: i64,
        predicate: impl Fn(Rect) -> bool,
        timeout: Duration,
    ) -> Result<Rect> {
        win32::wait_for_rect(hwnd, predicate, timeout)
    }

    /// The same on the perceived frame, `DWMWA_EXTENDED_FRAME_BOUNDS`.
    ///
    /// # Errors
    /// As [`TestWindows::wait_for_rect`].
    pub fn wait_for_frame(
        &self,
        hwnd: i64,
        predicate: impl Fn(Rect) -> bool,
        timeout: Duration,
    ) -> Result<Rect> {
        win32::wait_for_frame(hwnd, predicate, timeout)
    }

    /// Closes every window of the batch and waits for the threads to finish.
    ///
    /// Idempotent, and called again by `Drop`.
    ///
    /// # Errors
    /// [`Error::Timeout`] when a window ignored `WM_CLOSE` for five seconds.
    /// The batch is emptied either way, so a test never leaks a handle it then
    /// tries to close twice.
    pub fn close_all(&mut self) -> Result<()> {
        if self.spawned.is_empty() {
            self.threads.clear();
            return Ok(());
        }

        let handles = self.handles();
        for &hwnd in &handles {
            let _ = win32::close_window(hwnd);
        }
        let gone = win32::wait_until_gone(&handles, Duration::from_secs(5));

        // The message loops end on WM_QUIT, a moment after the window is gone.
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.alive.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        if self.alive.load(Ordering::SeqCst) == 0 {
            for thread in self.threads.drain(..) {
                let _ = thread.join();
            }
        } else {
            // A thread that is somehow still pumping must not block the test
            // that is trying to clean up. Detach it; the process exit takes it.
            self.threads.clear();
        }

        self.spawned.clear();
        gone
    }
}

impl Drop for TestWindows {
    fn drop(&mut self) {
        let _ = self.close_all();
    }
}

/// Where window number `slot` of a batch goes: staggered down and right from
/// the top left of the work area, wrapping before it leaves the monitor.
fn placement(monitor: &MonitorInfo, slot: u32, size: Option<(i32, i32)>) -> Rect {
    let work = monitor.work_area;
    let margin = monitor.scale(32);
    let stagger = monitor.scale(48);

    let (width, height) = size.unwrap_or_else(|| {
        (
            monitor.scale(720).min(work.width() * 2 / 3),
            monitor.scale(480).min(work.height() * 2 / 3),
        )
    });
    let width = width.clamp(160, work.width().max(160));
    let height = height.clamp(120, work.height().max(120));

    let room_x = (work.width() - width - 2 * margin).max(1);
    let room_y = (work.height() - height - 2 * margin).max(1);
    let x = work.left + margin + (slot as i32 * stagger) % room_x;
    let y = work.top + margin + (slot as i32 * stagger) % room_y;

    // Never hang off the edge, even on a monitor smaller than the window.
    let x = x.min(work.right - width).max(work.left);
    let y = y.min(work.bottom - height).max(work.top);

    Rect::from_size(x, y, width, height)
}

/// Per-window state, owned by the window: created before `CreateWindowExW`,
/// freed in `WM_NCDESTROY`.
struct WindowState {
    index: u32,
    color: COLORREF,
    emit: bool,
}

fn class_name_wide() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| to_wide(TEST_WINDOW_CLASS))
}

fn font_name_wide() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| to_wide("Segoe UI"))
}

fn ensure_class() -> Result<()> {
    static REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| register_class().map_err(|e| e.to_string()))
        .clone()
        .map_err(Error::Other)
}

fn register_class() -> Result<()> {
    unsafe {
        let module = GetModuleHandleW(None).map_err(|e| Error::win32("GetModuleHandleW", &e))?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: size_of::<*mut WindowState>() as i32,
            hInstance: HINSTANCE(module.0),
            hIcon: HICON::default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // No class brush: WM_PAINT fills the whole client area itself, so
            // resizing does not flash the default grey.
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name_wide().as_ptr()),
        };
        if RegisterClassW(&class) == 0 {
            let error = windows::core::Error::from_thread();
            // ERROR_CLASS_ALREADY_EXISTS: another batch got there first.
            if error.code().0 != hresult_from_win32(1410) {
                return Err(Error::win32("RegisterClassW", &error));
            }
        }
        Ok(())
    }
}

unsafe fn create_window(
    index: u32,
    title: &str,
    rect: Rect,
    color: COLORREF,
    emit: bool,
) -> Result<HWND> {
    ensure_class()?;

    let state = Box::into_raw(Box::new(WindowState { index, color, emit }));
    let title_wide = to_wide(title);
    let module =
        unsafe { GetModuleHandleW(None) }.map_err(|e| Error::win32("GetModuleHandleW", &e))?;

    let hwnd = unsafe {
        CreateWindowExW(
            // The bit that keeps every other window manager off these windows.
            WS_EX_TOOLWINDOW,
            PCWSTR(class_name_wide().as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            // A normal frame, so DWM draws the Windows 11 caption, the rounded
            // corners and the invisible resize border that tiling has to
            // compensate for.
            WS_OVERLAPPEDWINDOW,
            rect.left,
            rect.top,
            rect.width().max(160),
            rect.height().max(120),
            None,
            None,
            Some(HINSTANCE(module.0)),
            Some(state.cast::<c_void>()),
        )
    };

    match hwnd {
        Ok(hwnd) => {
            // Announce the window before showing it, so the stream reads
            // spawned, then the pos event that showing it produces.
            if emit {
                unsafe { emit_event(hwnd, index, EventKind::Spawned) };
            }
            // Show without activating: spawning a batch must not steal focus
            // from whatever the user is doing.
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWNA) };
            Ok(hwnd)
        }
        Err(e) => {
            // The state box belongs to the window from WM_NCCREATE onwards and
            // is freed in WM_NCDESTROY. Creation can fail on either side of
            // that, so it is leaked here rather than risking a double free.
            // One failed window leaks a few bytes, once.
            Err(Error::win32("CreateWindowExW", &e))
        }
    }
}

unsafe fn pump_messages() {
    let mut message = MSG::default();
    loop {
        // GetMessageW returns 0 for WM_QUIT and -1 for an error; both end it.
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 <= 0 {
            return;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, STATE_SLOT) } as *const WindowState;
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { &*pointer })
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = lparam.0 as *const CREATESTRUCTW;
                if !create.is_null() {
                    let state = (*create).lpCreateParams.cast::<WindowState>();
                    if !state.is_null() {
                        SetWindowLongPtrW(hwnd, STATE_SLOT, state as isize);
                        // The index also goes into GWLP_USERDATA, where another
                        // process can read it without any shared memory.
                        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*state).index as isize);
                    }
                }
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            // The client area is painted in full, so never erase it first.
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGED => {
                emit_if_enabled(hwnd, EventKind::Pos);
                // The label shows the current rect, so it has to be redrawn.
                let _ = InvalidateRect(Some(hwnd), None, false);
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            WM_DPICHANGED => {
                emit_if_enabled(hwnd, EventKind::Dpi);
                // Deliberately *not* honouring the suggested rectangle: a test
                // window never moves itself, so every rect change in the event
                // stream came from the window manager under test.
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }
            WM_SETTEXT => {
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                let _ = InvalidateRect(Some(hwnd), None, false);
                result
            }
            WM_GETMINMAXINFO => {
                let result = DefWindowProcW(hwnd, message, wparam, lparam);
                let info = lparam.0 as *mut MINMAXINFO;
                if !info.is_null() {
                    // Let a layout make these windows small. A real app has a
                    // minimum size, but a test window fighting the tiler would
                    // only hide the tiler's own mistakes.
                    (*info).ptMinTrackSize = POINT { x: 120, y: 80 };
                }
                result
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                emit_if_enabled(hwnd, EventKind::Closed);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_NCDESTROY => {
                let pointer = SetWindowLongPtrW(hwnd, STATE_SLOT, 0) as *mut WindowState;
                if !pointer.is_null() {
                    drop(Box::from_raw(pointer));
                }
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Emits an event when this window was spawned with the stream turned on.
unsafe fn emit_if_enabled(hwnd: HWND, kind: EventKind) {
    if let Some(state) = unsafe { state_of(hwnd) }
        && state.emit
    {
        unsafe { emit_event(hwnd, state.index, kind) };
    }
}

unsafe fn emit_event(hwnd: HWND, index: u32, kind: EventKind) {
    let handle = win32::as_i64(hwnd);
    let rect = win32::window_rect(handle).unwrap_or_default();
    let event = WindowEvent {
        event: kind,
        hwnd: handle,
        index,
        title: unsafe { win32::window_title(hwnd) },
        rect,
        frame: win32::frame_bounds(handle).unwrap_or(rect),
        minimized: unsafe { IsIconic(hwnd) }.as_bool(),
        dpi: unsafe { GetDpiForWindow(hwnd) }.max(96),
        ts_ms: now_ms(),
    };
    // A closed stdout must never take a window procedure down with it.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_json_line());
    let _ = out.flush();
}

unsafe fn paint(hwnd: HWND) {
    let Some(state) = (unsafe { state_of(hwnd) }) else {
        return;
    };

    let mut paint_info = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut paint_info) };
    if hdc.is_invalid() {
        return;
    }

    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };

    unsafe {
        let brush = CreateSolidBrush(state.color);
        FillRect(hdc, &client, brush);
        let _ = DeleteObject(brush.into());

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, INK);
    }

    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
    let height = (client.bottom - client.top).max(1);
    let big = (height / 5).clamp(18 * dpi / 96, 64 * dpi / 96);
    let small = (big / 3).max(11 * dpi / 96);

    let handle = win32::as_i64(hwnd);
    let rect = win32::window_rect(handle).unwrap_or_default();
    let title = unsafe { win32::window_title(hwnd) };

    let title_band = RECT {
        top: client.top + height / 8,
        bottom: client.top + height * 3 / 5,
        ..client
    };
    let id_band = RECT {
        top: title_band.bottom,
        bottom: title_band.bottom + small * 2,
        ..client
    };
    let rect_band = RECT {
        top: id_band.bottom,
        bottom: id_band.bottom + small * 2,
        ..client
    };

    unsafe {
        draw_line(hdc, big, &title, title_band);
        draw_line(
            hdc,
            small,
            &format!("#{} · 0x{:x}", state.index, handle),
            id_band,
        );
        draw_line(hdc, small, &format!("{rect} · {dpi} dpi"), rect_band);
        let _ = EndPaint(hwnd, &paint_info);
    }
}

unsafe fn draw_line(hdc: HDC, font_height: i32, text: &str, band: RECT) {
    if text.is_empty() || band.bottom <= band.top {
        return;
    }
    unsafe {
        let font = CreateFontW(
            -font_height,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            CLEARTYPE_QUALITY,
            0,
            PCWSTR(font_name_wide().as_ptr()),
        );
        let previous = SelectObject(hdc, font.into());
        let mut buffer = to_wide_unterminated(text);
        let mut band = band;
        DrawTextW(
            hdc,
            &mut buffer,
            &mut band,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        SelectObject(hdc, previous);
        let _ = DeleteObject(font.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(dpi: u32) -> MonitorInfo {
        MonitorInfo {
            index: 0,
            handle: 1,
            device: r"\\.\DISPLAY1".into(),
            rect: Rect::from_size(0, 0, 1920, 1080),
            work_area: Rect::from_size(0, 0, 1920, 1032),
            dpi,
            primary: true,
        }
    }

    #[test]
    fn every_window_of_a_batch_lands_inside_the_work_area() {
        let monitor = monitor(96);
        for slot in 0..12 {
            let rect = placement(&monitor, slot, None);
            assert!(
                monitor.work_area.contains(&rect),
                "slot {slot} at {rect} left {}",
                monitor.work_area
            );
        }
    }

    #[test]
    fn the_windows_are_staggered_rather_than_stacked() {
        let monitor = monitor(96);
        let first = placement(&monitor, 0, None);
        let second = placement(&monitor, 1, None);
        assert_ne!(first.left, second.left);
        assert_ne!(first.top, second.top);
        assert_eq!(first.width(), second.width());
    }

    #[test]
    fn a_high_dpi_monitor_gets_larger_windows() {
        let small = placement(&monitor(96), 0, None);
        let large = placement(&monitor(192), 0, None);
        assert!(large.width() > small.width(), "{large} vs {small}");
    }

    #[test]
    fn an_explicit_size_is_honoured() {
        let rect = placement(&monitor(96), 0, Some((640, 400)));
        assert_eq!((rect.width(), rect.height()), (640, 400));
    }

    #[test]
    fn a_tiny_work_area_still_produces_a_usable_window() {
        let mut tiny = monitor(96);
        tiny.rect = Rect::from_size(0, 0, 200, 150);
        tiny.work_area = tiny.rect;
        let rect = placement(&tiny, 3, None);
        assert!(rect.width() >= 133 && rect.height() >= 100, "{rect}");
    }

    #[test]
    fn default_options_match_the_command_line_defaults() {
        let options = SpawnOptions::default();
        assert_eq!(options.count, 3);
        assert_eq!(options.monitor, 0);
        assert_eq!(options.title_prefix, DEFAULT_TITLE_PREFIX);
        assert!(!options.emit_events);
    }

    #[test]
    fn the_palette_is_light_enough_for_dark_text() {
        for color in PALETTE {
            let (r, g, b) = (
                color.0 & 0xff,
                (color.0 >> 8) & 0xff,
                (color.0 >> 16) & 0xff,
            );
            assert!(r + g + b > 500, "0x{:06x} is too dark for the ink", color.0);
        }
    }
}
