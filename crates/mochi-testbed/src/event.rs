//! The JSON event stream a spawned host prints to stdout.
//!
//! One event is one line, so a test can read the stream with a line reader and
//! `serde_json::from_str` without any framing of its own. Every line that
//! belongs to the stream has an `event` key; command results printed by the
//! other subcommands have an `ok` key instead.

use crate::geometry::Rect;
use serde::{Deserialize, Serialize};

/// Why an event was emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// The window has just been created and shown.
    Spawned,
    /// `WM_WINDOWPOSCHANGED`: the window was moved, resized or restacked.
    /// This is the one a tiling test watches.
    Pos,
    /// `WM_DPICHANGED`: the window crossed to a monitor with another scaling.
    /// The test window deliberately does *not* resize itself in response.
    Dpi,
    /// `WM_DESTROY`: the window is gone.
    Closed,
}

/// One line of the event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowEvent {
    /// Why this line was printed.
    pub event: EventKind,
    /// The window handle as a plain number.
    pub hwnd: i64,
    /// The index the window was spawned with.
    pub index: u32,
    /// The window title at the time of the event.
    pub title: String,
    /// `GetWindowRect`, invisible border included.
    pub rect: Rect,
    /// `DWMWA_EXTENDED_FRAME_BOUNDS`, the perceived frame.
    pub frame: Rect,
    /// `IsIconic` at the time of the event.
    pub minimized: bool,
    /// The DPI of the window at the time of the event.
    pub dpi: u32,
    /// Milliseconds since the Unix epoch, for measuring how long a retile took.
    pub ts_ms: u64,
}

impl WindowEvent {
    /// The single line this event is printed as, newline not included.
    ///
    /// Serialisation of a fixed set of plain fields cannot fail; if it somehow
    /// did, a diagnostic line is returned rather than a panic in a window
    /// procedure.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|e| format!(r#"{{"event":"error","message":"{e}"}}"#))
    }
}

/// Milliseconds since the Unix epoch. Saturates at zero before 1970.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> WindowEvent {
        WindowEvent {
            event: EventKind::Pos,
            hwnd: 0x1234,
            index: 1,
            title: "MochiTest 1".into(),
            rect: Rect::from_size(0, 0, 1280, 800),
            frame: Rect::from_size(7, 0, 1266, 793),
            minimized: false,
            dpi: 144,
            ts_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn an_event_is_one_line_of_json() {
        let line = event().to_json_line();
        assert!(!line.contains('\n'));
        assert!(line.contains(r#""event":"pos""#), "{line}");
        assert!(line.contains(r#""hwnd":4660"#), "{line}");
    }

    #[test]
    fn the_line_round_trips() {
        let line = event().to_json_line();
        assert_eq!(serde_json::from_str::<WindowEvent>(&line).unwrap(), event());
    }

    #[test]
    fn the_kinds_are_lower_case_in_json() {
        for (kind, text) in [
            (EventKind::Spawned, "\"spawned\""),
            (EventKind::Pos, "\"pos\""),
            (EventKind::Dpi, "\"dpi\""),
            (EventKind::Closed, "\"closed\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), text);
        }
    }

    #[test]
    fn the_clock_is_past_2020() {
        assert!(now_ms() > 1_577_836_800_000);
    }
}
