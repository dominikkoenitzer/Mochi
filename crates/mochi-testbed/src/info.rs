//! The plain data the testbed hands out: one struct per window, one per monitor.
//!
//! Nothing here calls Win32, so the types can be parsed by anything that reads
//! the JSON the `mochi-testwin` binary prints.

use crate::geometry::Rect;
use serde::{Deserialize, Serialize};

/// `WS_EX_TOOLWINDOW`, the bit that keeps a window out of the taskbar and out
/// of every window manager that follows the usual manageability rules.
pub const WS_EX_TOOLWINDOW_BIT: u32 = 0x0000_0080;

/// Everything the testbed reports about one test window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestWindowInfo {
    /// The window handle as a plain number, so it survives JSON.
    pub hwnd: i64,
    /// The index the window was spawned with, one based in the title.
    pub index: u32,
    /// Current window title, which `rename` changes.
    pub title: String,
    /// Window class, always [`crate::TEST_WINDOW_CLASS`].
    pub class: String,
    /// Process that owns the window, always a `mochi-testwin` host.
    pub pid: u32,
    /// `GetWindowRect`, which includes the invisible resize border.
    pub rect: Rect,
    /// `DWMWA_EXTENDED_FRAME_BOUNDS`, what the user perceives as the window.
    pub frame: Rect,
    /// Index into the monitor list, `None` when the window sits nowhere sane.
    pub monitor: Option<usize>,
    /// `GWL_STYLE`.
    pub style: u32,
    /// `GWL_EXSTYLE`. Always carries [`WS_EX_TOOLWINDOW_BIT`].
    pub ex_style: u32,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// `IsIconic`.
    pub minimized: bool,
}

impl TestWindowInfo {
    /// The handle in the form the daemon logs use.
    #[must_use]
    pub fn hwnd_hex(&self) -> String {
        format!("0x{:x}", self.hwnd)
    }

    /// True when the window carries the tool window bit, which is the whole
    /// reason these windows can be spawned while another manager is running.
    #[must_use]
    pub const fn is_tool_window(&self) -> bool {
        self.ex_style & WS_EX_TOOLWINDOW_BIT != 0
    }

    /// The invisible border as left, top, right and bottom insets: how far the
    /// window rect sticks out beyond the frame the user sees. This is exactly
    /// the amount a tiling manager has to compensate for.
    #[must_use]
    pub const fn border(&self) -> (i32, i32, i32, i32) {
        (
            self.frame.left - self.rect.left,
            self.frame.top - self.rect.top,
            self.rect.right - self.frame.right,
            self.rect.bottom - self.frame.bottom,
        )
    }
}

/// Everything the testbed reports about one monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorInfo {
    /// Position in `EnumDisplayMonitors` order. This is what `--monitor` takes.
    pub index: usize,
    /// The raw `HMONITOR` as a number, for cross-checking against the daemon.
    pub handle: i64,
    /// GDI device name, for example `\\.\DISPLAY1`.
    pub device: String,
    /// The full monitor rectangle in virtual screen coordinates.
    pub rect: Rect,
    /// The monitor rectangle minus the taskbar and other appbars.
    pub work_area: Rect,
    /// Effective DPI. 96 is 100% scaling.
    pub dpi: u32,
    /// True for the monitor that holds the origin of the virtual screen.
    pub primary: bool,
}

impl MonitorInfo {
    /// DPI scaling as a factor, 1.0 at 96 DPI.
    #[must_use]
    pub fn scale_factor(&self) -> f64 {
        f64::from(self.dpi) / 96.0
    }

    /// Scales a value given in logical pixels to this monitor.
    #[must_use]
    pub const fn scale(&self, logical: i32) -> i32 {
        logical * self.dpi as i32 / 96
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> TestWindowInfo {
        TestWindowInfo {
            hwnd: 0x1234,
            index: 2,
            title: "MochiTest 2".into(),
            class: crate::TEST_WINDOW_CLASS.into(),
            pid: 4242,
            rect: Rect::new(-7, 0, 1287, 807),
            frame: Rect::new(0, 0, 1280, 800),
            monitor: Some(0),
            style: 0,
            ex_style: WS_EX_TOOLWINDOW_BIT,
            visible: true,
            minimized: false,
        }
    }

    #[test]
    fn the_border_is_the_difference_between_rect_and_frame() {
        assert_eq!(info().border(), (7, 0, 7, 7));
    }

    #[test]
    fn test_windows_are_always_tool_windows() {
        assert!(info().is_tool_window());
        let mut plain = info();
        plain.ex_style = 0;
        assert!(!plain.is_tool_window());
    }

    #[test]
    fn handles_are_shown_in_hex_and_serialised_as_numbers() {
        assert_eq!(info().hwnd_hex(), "0x1234");
        let json = serde_json::to_string(&info()).unwrap();
        assert!(json.contains(r#""hwnd":4660"#), "{json}");
    }

    #[test]
    fn dpi_scaling_follows_the_monitor() {
        let m = MonitorInfo {
            index: 0,
            handle: 1,
            device: r"\\.\DISPLAY1".into(),
            rect: Rect::from_size(0, 0, 3840, 2160),
            work_area: Rect::from_size(0, 0, 3840, 2112),
            dpi: 144,
            primary: true,
        };
        assert!((m.scale_factor() - 1.5).abs() < f64::EPSILON);
        assert_eq!(m.scale(100), 150);
    }
}
