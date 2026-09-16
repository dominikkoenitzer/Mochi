//! What the daemon knows right now.
//!
//! Milestone two keeps a flat picture: the monitors and every top-level window
//! with a manageability verdict. The monitor / workspace / container tree is
//! `mochi-core`'s job and replaces [`State::windows`] when it lands; the
//! accessors here are the seam.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mochi_client::{BooleanState, Layout};
use serde::Serialize;

use crate::platform::{Hwnd, MonitorInfo, WindowInfo, is_manageable};

/// One window plus why Mochi does or does not tile it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackedWindow {
    /// Everything read from Win32.
    #[serde(flatten)]
    pub info: WindowInfo,
    /// True when the static heuristics accept the window.
    pub manageable: bool,
    /// Why it was rejected, absent when it was accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<&'static str>,
}

impl From<WindowInfo> for TrackedWindow {
    fn from(info: WindowInfo) -> Self {
        let verdict = is_manageable(&info);
        Self {
            manageable: verdict.is_ok(),
            skipped: verdict.err().map(|r| r.as_str()),
            info,
        }
    }
}

/// Runtime settings a client can flip without touching the configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// Focus whatever the mouse moves over.
    pub focus_follows_mouse: bool,
    /// Warp the mouse to a newly focused window.
    pub mouse_follows_focus: bool,
    /// Fade unfocused windows.
    pub transparency: bool,
    /// Draw a border around the focused window.
    pub border: bool,
    /// Border thickness in logical pixels.
    pub border_width: i32,
    /// How far the border sits outside the frame.
    pub border_offset: i32,
    /// Play move and resize animations.
    pub animation: bool,
    /// Animation length in milliseconds.
    pub animation_duration: u64,
    /// Animation frame rate.
    pub animation_fps: u32,
    /// Layout new workspaces start with.
    pub default_layout: Layout,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            focus_follows_mouse: false,
            mouse_follows_focus: false,
            transparency: false,
            border: false,
            border_width: 6,
            border_offset: -1,
            animation: false,
            animation_duration: 250,
            animation_fps: 60,
            default_layout: Layout::Bsp,
        }
    }
}

impl Settings {
    /// Applies an `enable`/`disable` argument to a field.
    pub fn set(field: &mut bool, state: BooleanState) {
        *field = state.is_enabled();
    }
}

/// The daemon's whole picture of the desktop.
#[derive(Debug, Clone, Serialize)]
pub struct State {
    /// Version of the running daemon.
    pub version: &'static str,
    /// True when every write is only logged.
    pub dry_run: bool,
    /// True while management is paused.
    pub paused: bool,
    /// Configuration file in use.
    pub config_path: PathBuf,
    /// Attached monitors, in enumeration order.
    pub monitors: Vec<MonitorInfo>,
    /// Every top-level window, keyed by handle so the output is stable.
    pub windows: BTreeMap<Hwnd, TrackedWindow>,
    /// The foreground window, as far as Mochi knows.
    pub focused: Option<Hwnd>,
    /// Runtime settings.
    pub settings: Settings,
    /// Registered subscriber pipe names.
    pub subscribers: Vec<String>,
}

impl State {
    /// An empty state pointing at a configuration file.
    pub fn new(config_path: PathBuf, dry_run: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run,
            paused: false,
            config_path,
            monitors: Vec::new(),
            windows: BTreeMap::new(),
            focused: None,
            settings: Settings::default(),
            subscribers: Vec::new(),
        }
    }

    /// Replaces the monitor list.
    pub fn set_monitors(&mut self, monitors: Vec<MonitorInfo>) {
        self.monitors = monitors;
    }

    /// Replaces the window list.
    pub fn set_windows(&mut self, windows: impl IntoIterator<Item = WindowInfo>) {
        self.windows = windows
            .into_iter()
            .map(|info| (info.hwnd, TrackedWindow::from(info)))
            .collect();
    }

    /// Inserts or refreshes one window. Returns true when it is newly tracked.
    pub fn upsert(&mut self, info: WindowInfo) -> bool {
        self.windows
            .insert(info.hwnd, TrackedWindow::from(info))
            .is_none()
    }

    /// Forgets a window. Returns what was there.
    pub fn forget(&mut self, hwnd: Hwnd) -> Option<TrackedWindow> {
        self.windows.remove(&hwnd)
    }

    /// A tracked window by handle.
    pub fn window(&self, hwnd: Hwnd) -> Option<&TrackedWindow> {
        self.windows.get(&hwnd)
    }

    /// Every window the static heuristics accept.
    pub fn manageable(&self) -> impl Iterator<Item = &TrackedWindow> {
        self.windows.values().filter(|w| w.manageable)
    }

    /// How many windows are tiling candidates.
    pub fn manageable_count(&self) -> usize {
        self.manageable().count()
    }

    /// The monitor a window sits on, by index into [`State::monitors`].
    pub fn monitor_index_of(&self, hwnd: Hwnd) -> Option<usize> {
        let id = self.window(hwnd)?.info.monitor?;
        self.monitors.iter().position(|m| m.id == id)
    }

    /// The configuration file in use.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// The JSON `mochic state` prints.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(
            |e| serde_json::json!({ "error": format!("state could not be serialised: {e}") }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::types::style;
    use mochi_core::Rect;

    fn window(hwnd: i64, title: &str) -> WindowInfo {
        WindowInfo {
            title: title.into(),
            class: "Chrome_WidgetWin_1".into(),
            exe: "Code.exe".into(),
            style: style::WS_VISIBLE | style::WS_CAPTION,
            rect: Rect::new(0, 0, 800, 600),
            frame: Rect::new(0, 0, 800, 600),
            visible: true,
            ..WindowInfo::placeholder(Hwnd(hwnd as isize))
        }
    }

    fn state() -> State {
        State::new(PathBuf::from(r"C:\Users\x\mochi.json"), true)
    }

    #[test]
    fn a_verdict_is_attached_to_every_window() {
        let mut s = state();
        let mut hidden = window(2, "Hidden");
        hidden.visible = false;
        s.set_windows([window(1, "Editor"), hidden]);

        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.manageable_count(), 1);
        assert_eq!(s.window(Hwnd(2)).unwrap().skipped, Some("not visible"));
        assert_eq!(s.window(Hwnd(1)).unwrap().skipped, None);
    }

    #[test]
    fn upsert_reports_whether_the_window_is_new() {
        let mut s = state();
        assert!(s.upsert(window(1, "First")));
        assert!(!s.upsert(window(1, "First renamed")));
        assert_eq!(s.window(Hwnd(1)).unwrap().info.title, "First renamed");
        assert!(s.forget(Hwnd(1)).is_some());
        assert!(s.forget(Hwnd(1)).is_none());
    }

    #[test]
    fn the_json_dump_carries_the_pieces_mochic_prints() {
        let mut s = state();
        s.set_windows([window(1, "Editor")]);
        let json = s.to_json();
        assert_eq!(json["dry_run"], true);
        assert_eq!(json["paused"], false);
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        assert!(json["monitors"].is_array());
        // Windows are keyed by handle and the info is flattened into the entry.
        assert_eq!(json["windows"]["1"]["title"], "Editor");
        assert_eq!(json["windows"]["1"]["manageable"], true);
        assert_eq!(json["settings"]["default_layout"], "bsp");
    }

    #[test]
    fn boolean_settings_follow_the_command_line_spelling() {
        let mut s = Settings::default();
        Settings::set(&mut s.focus_follows_mouse, BooleanState::Enable);
        assert!(s.focus_follows_mouse);
        Settings::set(&mut s.focus_follows_mouse, BooleanState::Disable);
        assert!(!s.focus_follows_mouse);
    }
}
