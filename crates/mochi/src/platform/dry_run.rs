//! A [`Platform`] that reads the real desktop but only logs its writes.
//!
//! This is what `mochi --dry-run` runs on. Everything the daemon would do to a
//! window turns into a `dry-run` log line at info level, which makes it safe to
//! start next to another window manager.

use anyhow::Result;

use super::types::{Hwnd, MonitorInfo, WindowInfo};
use super::{Platform, ShowState, WindowPlacement};

/// Wraps another platform and swallows every write.
pub struct DryRunPlatform<P: Platform = super::Win32Platform> {
    inner: P,
}

impl<P: Platform> DryRunPlatform<P> {
    /// Wraps `inner`, keeping its reads and dropping its writes.
    pub const fn new(inner: P) -> Self {
        Self { inner }
    }

    /// The platform whose reads are used.
    pub const fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P: Platform> Platform for DryRunPlatform<P> {
    fn name(&self) -> &'static str {
        "dry-run"
    }

    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        self.inner.monitors()
    }

    fn windows(&self) -> Result<Vec<WindowInfo>> {
        self.inner.windows()
    }

    fn window_info(&self, hwnd: Hwnd) -> Result<WindowInfo> {
        self.inner.window_info(hwnd)
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
        // A read, so it passes through: a dry run reports the desktop as it is.
        self.inner.is_maximized(hwnd)
    }

    fn outranks_us(&self, hwnd: Hwnd) -> bool {
        // A read, so it passes through.
        self.inner.outranks_us(hwnd)
    }

    fn is_on_screen(&self, hwnd: Hwnd) -> bool {
        // A read, so it passes through: a dry run reports the desktop as it is.
        self.inner.is_on_screen(hwnd)
    }

    fn set_positions(&self, placements: &[WindowPlacement]) -> Result<()> {
        for p in placements {
            tracing::info!(
                hwnd = %p.hwnd,
                left = p.rect.left,
                top = p.rect.top,
                width = p.rect.width(),
                height = p.rect.height(),
                z = ?p.z,
                "dry-run: set_position"
            );
        }
        Ok(())
    }

    fn set_cloaked(&self, hwnd: Hwnd, cloaked: bool) -> Result<()> {
        tracing::info!(%hwnd, cloaked, "dry-run: set_cloaked");
        Ok(())
    }

    fn show(&self, hwnd: Hwnd, state: ShowState) -> Result<()> {
        tracing::info!(%hwnd, ?state, "dry-run: show");
        Ok(())
    }

    fn focus(&self, hwnd: Hwnd) -> Result<()> {
        tracing::info!(%hwnd, "dry-run: focus");
        Ok(())
    }

    fn focus_desktop(&self) -> Result<()> {
        tracing::info!("dry-run: focus the desktop");
        Ok(())
    }

    fn close(&self, hwnd: Hwnd) -> Result<()> {
        tracing::info!(%hwnd, "dry-run: close");
        Ok(())
    }

    fn set_transparency(&self, hwnd: Hwnd, alpha: Option<u8>) -> Result<()> {
        tracing::info!(%hwnd, ?alpha, "dry-run: set_transparency");
        Ok(())
    }

    fn set_topmost(&self, hwnd: Hwnd, topmost: bool) -> Result<()> {
        tracing::info!(%hwnd, topmost, "dry-run: set_topmost");
        Ok(())
    }

    fn set_cursor_position(&self, x: i32, y: i32) -> Result<()> {
        tracing::info!(x, y, "dry-run: set_cursor_position");
        Ok(())
    }
}
