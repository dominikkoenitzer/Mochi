//! A [`Platform`] whose monitors come from a file instead of from Windows.
//!
//! This is what `mochi --simulate-monitors <PATH>` runs on, and it exists for
//! the end-to-end suite. A CI runner has exactly one display, and the cases
//! that go wrong on a real desk need more than that: a portrait screen next to
//! a landscape one, the two enumerated in the other order after a driver
//! reset, one of them unplugged and plugged back in. Everything else stays
//! real. Windows are listed, moved, cloaked and focused by the wrapped
//! platform, so the daemon tiles real windows into rectangles that only exist
//! in the file.
//!
//! The file is read again on every enumeration. A test that rewrites it and
//! then sends the daemon `WM_DISPLAYCHANGE` takes the same path a monitor
//! being attached or removed takes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use mochi_core::Rect;
use serde::Deserialize;

use super::types::{Hwnd, MonitorId, MonitorInfo, WindowInfo, WindowShape};
use super::{Platform, ShowState, WindowPlacement};

/// One monitor as the file spells it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimulatedMonitor {
    /// The GDI name the monitor reports, `\\.\DISPLAY2` for instance.
    device_name: String,
    /// The whole monitor, in virtual screen coordinates.
    size: Rect,
    /// The part of it a taskbar leaves. The whole monitor when left out.
    #[serde(default)]
    work_area: Option<Rect>,
    /// Effective DPI, 96 when left out.
    #[serde(default = "default_dpi")]
    dpi: u32,
}

const fn default_dpi() -> u32 {
    96
}

/// Parses the file's contents into what [`Platform::monitors`] hands back.
///
/// The handle of each monitor is derived from its rectangle. Real handles do
/// not survive a display change either, but the daemon also uses them to tell
/// which monitor a window is on, and a handle that stays the same for the same
/// rectangle keeps that answer stable across a rewrite of the file.
fn parse(text: &str) -> Result<Vec<MonitorInfo>> {
    let monitors: Vec<SimulatedMonitor> =
        serde_json::from_str(text).context("the simulated monitors are not a JSON array")?;
    Ok(monitors
        .into_iter()
        .map(|monitor| {
            let size = monitor.size;
            MonitorInfo {
                id: id_for(size),
                device_name: monitor.device_name,
                device_description: "Simulated monitor".to_owned(),
                size,
                work_area: monitor.work_area.unwrap_or(size),
                dpi: monitor.dpi,
                primary: size.left == 0 && size.top == 0,
            }
        })
        .collect())
}

/// A handle for a monitor at `size` that no real `HMONITOR` will collide with.
fn id_for(size: Rect) -> MonitorId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for value in [size.left, size.top, size.right, size.bottom] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    // Negative, so it can never be a pointer Windows handed out.
    MonitorId(-((hash >> 2) as isize) - 1)
}

/// The monitor whose rectangle holds the centre of `rect`.
fn monitor_under(monitors: &[MonitorInfo], rect: Rect) -> Option<MonitorId> {
    let x = rect.left + rect.width() / 2;
    let y = rect.top + rect.height() / 2;
    monitors
        .iter()
        .find(|m| x >= m.size.left && x < m.size.right && y >= m.size.top && y < m.size.bottom)
        .map(|m| m.id)
}

/// Wraps another platform and replaces its monitors with the ones in a file.
pub struct SimulatedMonitors {
    inner: Arc<dyn Platform>,
    path: PathBuf,
}

impl SimulatedMonitors {
    /// Wraps `inner`, reading the monitors from `path`.
    ///
    /// # Errors
    /// When the file cannot be read or holds no monitor, so a typo in a test
    /// fails at start-up rather than as a daemon tiling the real display.
    pub fn new(inner: Arc<dyn Platform>, path: &Path) -> Result<Self> {
        let platform = Self {
            inner,
            path: path.to_owned(),
        };
        let monitors = platform.monitors()?;
        anyhow::ensure!(!monitors.is_empty(), "{} lists no monitor", path.display());
        for monitor in &monitors {
            tracing::warn!(
                device = %monitor.device_name,
                size = ?monitor.size,
                work_area = ?monitor.work_area,
                dpi = monitor.dpi,
                "simulated monitor"
            );
        }
        Ok(platform)
    }

    /// The monitors as the file holds them right now.
    fn read(&self) -> Result<Vec<MonitorInfo>> {
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("could not read {}", self.path.display()))?;
        parse(&text).with_context(|| format!("in {}", self.path.display()))
    }

    /// `info` with its monitor taken from the file rather than from Windows.
    fn relocate(&self, mut info: WindowInfo, monitors: &[MonitorInfo]) -> WindowInfo {
        info.monitor = monitor_under(monitors, info.rect);
        info
    }
}

impl Platform for SimulatedMonitors {
    fn name(&self) -> &'static str {
        "simulated-monitors"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        // A file caught halfway through a rewrite reads as an empty list,
        // which the daemon already treats as "keep what you have".
        match self.read() {
            Ok(monitors) => Ok(monitors),
            Err(error) => {
                tracing::warn!(%error, "could not read the simulated monitors");
                Ok(Vec::new())
            }
        }
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        let monitors = self.read().unwrap_or_default();
        Ok(self
            .inner
            .windows()?
            .into_iter()
            .map(|info| self.relocate(info, &monitors))
            .collect())
    }

    fn window_info(&self, hwnd: Hwnd) -> Result<WindowInfo> {
        let monitors = self.read().unwrap_or_default();
        Ok(self.relocate(self.inner.window_info(hwnd)?, &monitors))
    }

    fn foreground_window(&self) -> Option<Hwnd> {
        self.inner.foreground_window()
    }

    fn window_at(&self, x: i32, y: i32) -> Option<Hwnd> {
        self.inner.window_at(x, y)
    }

    fn cursor_position(&self) -> Result<(i32, i32)> {
        self.inner.cursor_position()
    }

    fn is_maximized(&self, hwnd: Hwnd) -> bool {
        self.inner.is_maximized(hwnd)
    }

    fn window_shape(&self, hwnd: Hwnd) -> Option<WindowShape> {
        self.inner.window_shape(hwnd)
    }

    fn visible_frame(&self, hwnd: Hwnd) -> Option<Rect> {
        self.inner.visible_frame(hwnd)
    }

    fn min_size(&self, hwnd: Hwnd) -> Option<mochi_core::layout::MinSize> {
        self.inner.min_size(hwnd)
    }

    fn outranks_us(&self, hwnd: Hwnd) -> bool {
        self.inner.outranks_us(hwnd)
    }

    fn is_on_screen(&self, hwnd: Hwnd) -> bool {
        self.inner.is_on_screen(hwnd)
    }

    fn take_unreachable(&self) -> Vec<Hwnd> {
        self.inner.take_unreachable()
    }

    fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
        self.inner.set_positions(placements)
    }

    fn set_position(&self, placement: &WindowPlacement) -> Result<()> {
        self.inner.set_position(placement)
    }

    fn set_cloaked(&self, hwnd: Hwnd, cloaked: bool) -> Result<()> {
        self.inner.set_cloaked(hwnd, cloaked)
    }

    fn show(&self, hwnd: Hwnd, state: ShowState) -> Result<()> {
        self.inner.show(hwnd, state)
    }

    fn focus(&self, hwnd: Hwnd) -> Result<()> {
        self.inner.focus(hwnd)
    }

    fn focus_desktop(&self) -> Result<()> {
        self.inner.focus_desktop()
    }

    fn close(&self, hwnd: Hwnd) -> Result<()> {
        self.inner.close(hwnd)
    }

    fn set_transparency(&self, hwnd: Hwnd, alpha: Option<u8>) -> Result<()> {
        self.inner.set_transparency(hwnd, alpha)
    }

    fn set_topmost(&self, hwnd: Hwnd, topmost: bool) -> Result<()> {
        self.inner.set_topmost(hwnd, topmost)
    }

    fn set_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        self.inner.set_cursor_position(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESK: &str = r#"[
        { "device_name": "\\\\.\\DISPLAY2",
          "size": { "left": 3840, "top": 120, "right": 4920, "bottom": 2040 } },
        { "device_name": "\\\\.\\DISPLAY1",
          "size": { "left": 0, "top": 0, "right": 3840, "bottom": 2160 },
          "work_area": { "left": 0, "top": 0, "right": 3840, "bottom": 2112 },
          "dpi": 144 }
    ]"#;

    #[test]
    fn the_file_is_read_in_its_own_order_with_defaults_filled_in() {
        let monitors = parse(DESK).unwrap();
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].device_name, r"\\.\DISPLAY2");
        assert_eq!(monitors[0].work_area, monitors[0].size);
        assert_eq!(monitors[0].dpi, 96);
        assert!(!monitors[0].primary);
        assert_eq!(monitors[1].work_area, Rect::new(0, 0, 3840, 2112));
        assert_eq!(monitors[1].dpi, 144);
        assert!(monitors[1].primary);
    }

    #[test]
    fn a_monitor_keeps_its_handle_for_as_long_as_it_keeps_its_rectangle() {
        let first = parse(DESK).unwrap();
        let reordered = parse(&format!(
            "[{}]",
            r#"{ "device_name": "\\\\.\\DISPLAY9",
                 "size": { "left": 3840, "top": 120, "right": 4920, "bottom": 2040 } }"#
        ))
        .unwrap();
        assert_eq!(first[0].id, reordered[0].id);
        assert_ne!(first[0].id, first[1].id);
        assert!(first.iter().all(|m| m.id.0 < 0));
    }

    #[test]
    fn a_window_belongs_to_the_monitor_under_its_centre() {
        let monitors = parse(DESK).unwrap();
        let on_portrait = Rect::new(3900, 200, 4800, 900);
        let on_landscape = Rect::new(100, 100, 900, 700);
        let between = Rect::new(-500, -500, -100, -100);
        assert_eq!(monitor_under(&monitors, on_portrait), Some(monitors[0].id));
        assert_eq!(monitor_under(&monitors, on_landscape), Some(monitors[1].id));
        assert_eq!(monitor_under(&monitors, between), None);
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        assert!(parse(r#"[{ "device_name": "x", "rect": {} }]"#).is_err());
        assert!(parse("{}").is_err());
    }
}
