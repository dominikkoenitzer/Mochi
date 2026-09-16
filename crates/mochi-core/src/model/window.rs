//! Window handles and the metadata the daemon caches for them.

use serde::{Deserialize, Serialize};

use crate::rules::WindowInfo;

/// An opaque platform window handle.
///
/// On Windows this is an `HWND`. Nothing in this crate looks inside it; the
/// daemon fills it in and hands it back to Win32.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(transparent)]
pub struct WindowId(pub isize);

impl WindowId {
    /// The raw handle value.
    #[must_use]
    pub const fn get(self) -> isize {
        self.0
    }
}

impl From<isize> for WindowId {
    fn from(raw: isize) -> Self {
        Self(raw)
    }
}

impl From<WindowId> for isize {
    fn from(id: WindowId) -> Self {
        id.0
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

/// A managed window: the handle plus the metadata rules match against.
///
/// The strings are a cache. The daemon refreshes them when Windows tells it a
/// title or a process changed; nothing in this crate reads them except
/// [`crate::rules`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Window {
    /// The platform handle.
    pub id: WindowId,
    /// The window title, as shown in the title bar.
    #[serde(default)]
    pub title: String,
    /// The file name of the owning process, for example `firefox.exe`.
    #[serde(default)]
    pub exe: String,
    /// The Win32 window class name.
    #[serde(default)]
    pub class: String,
    /// The full path of the owning executable.
    #[serde(default)]
    pub path: String,
}

impl Window {
    /// A window with a handle and no metadata yet.
    #[must_use]
    pub fn new(id: impl Into<WindowId>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    /// Sets the title, builder style.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Sets the executable name, builder style.
    #[must_use]
    pub fn with_exe(mut self, exe: impl Into<String>) -> Self {
        self.exe = exe.into();
        self
    }

    /// Sets the window class, builder style.
    #[must_use]
    pub fn with_class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the executable path, builder style.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// Borrows the cached metadata in the shape the rule engine wants.
    #[must_use]
    pub fn info(&self) -> WindowInfo<'_> {
        WindowInfo {
            title: &self.title,
            class: &self.class,
            exe: &self.exe,
            path: &self.path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_id_round_trips() {
        let id = WindowId::from(0x1234_isize);
        assert_eq!(id.get(), 0x1234);
        assert_eq!(isize::from(id), 0x1234);
        assert_eq!(id.to_string(), "0x1234");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "4660", "serialises as a bare number");
        assert_eq!(serde_json::from_str::<WindowId>(&json).unwrap(), id);
    }

    #[test]
    fn builders_fill_the_cache() {
        let w = Window::new(7)
            .with_title("README.md - Code")
            .with_exe("Code.exe")
            .with_class("Chrome_WidgetWin_1")
            .with_path(r"C:\Program Files\Code\Code.exe");
        assert_eq!(w.id, WindowId(7));
        let info = w.info();
        assert_eq!(info.exe, "Code.exe");
        assert_eq!(info.class, "Chrome_WidgetWin_1");
        assert_eq!(info.title, "README.md - Code");
        assert!(info.path.ends_with("Code.exe"));
    }

    #[test]
    fn missing_metadata_deserialises_to_empty_strings() {
        let w: Window = serde_json::from_str(r#"{"id":42}"#).unwrap();
        assert_eq!(w.id, WindowId(42));
        assert!(w.title.is_empty());
        assert!(w.exe.is_empty());
    }
}
