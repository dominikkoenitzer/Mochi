//! The stackbar: a row of tabs above a stacked container.
//!
//! One window of a stack is visible at a time, so the bar is the only way to
//! see what else is in there. Each tab is a window; clicking one asks the
//! daemon to focus it through the callback given to
//! [`StackbarManager::new`].
//!
//! Like the borders, the daemon sends the whole desired end state and the
//! stackbar thread works out the difference.

use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::{Rect, WindowHandle};

mod layout;
#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod window;

pub use layout::TabLayout;
#[cfg(windows)]
pub use manager::StackbarManager;

/// When to show a stackbar at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum StackbarMode {
    /// Never, the default: the stack still works, it just has no tabs.
    #[default]
    Never,
    /// On every container, even one holding a single window.
    Always,
    /// Only on containers holding more than one window.
    OnStack,
}

impl StackbarMode {
    /// Whether a container with `windows` windows gets a bar.
    #[must_use]
    pub const fn shows(self, windows: usize) -> bool {
        match self {
            StackbarMode::Never => false,
            StackbarMode::Always => windows > 0,
            StackbarMode::OnStack => windows > 1,
        }
    }
}

/// What to write on a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum StackbarLabel {
    /// The executable name, without the extension: short and stable.
    #[default]
    Process,
    /// The window title: informative, and it changes under you.
    Title,
}

/// How the stackbar looks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StackbarConfig {
    /// When to show the bar.
    pub mode: StackbarMode,
    /// How tall the bar is, in physical pixels.
    pub height: i32,
    /// How wide one tab would like to be. Tabs shrink to share the bar when
    /// there are too many of them.
    pub tab_width: i32,
    /// Process name or window title.
    pub label: StackbarLabel,
    /// The font family. `None` uses the shell's UI font.
    pub font_family: Option<String>,
    /// The font size in points.
    pub font_size: f32,
    /// The tab of the window that is on top.
    pub focused_background: Color,
    /// The other tabs.
    pub unfocused_background: Color,
    /// Text on the focused tab.
    pub focused_text: Color,
    /// Text on the other tabs.
    pub unfocused_text: Color,
}

impl Default for StackbarConfig {
    /// Catppuccin Mocha again: the pink accent for the focused tab with the
    /// dark base as its text, Surface0 with the normal text colour for the
    /// rest.
    fn default() -> Self {
        Self {
            mode: StackbarMode::Never,
            height: 40,
            tab_width: 220,
            label: StackbarLabel::Process,
            font_family: None,
            font_size: 12.0,
            focused_background: Color::rgb(0xff, 0xbb, 0xdf),
            unfocused_background: Color::rgb(0x31, 0x32, 0x44),
            focused_text: Color::rgb(0x1e, 0x1e, 0x2e),
            unfocused_text: Color::rgb(0xcd, 0xd6, 0xf4),
        }
    }
}

/// One tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackbarTab {
    /// The window this tab focuses when it is clicked.
    pub target: WindowHandle,
    /// What to write on it, already resolved to a process name or a title by
    /// the daemon, which is the side that can read either.
    pub label: String,
    /// `true` for the window currently on top of the stack.
    pub focused: bool,
}

impl StackbarTab {
    /// A tab from its parts.
    #[must_use]
    pub fn new(target: WindowHandle, label: impl Into<String>, focused: bool) -> Self {
        Self {
            target,
            label: label.into(),
            focused,
        }
    }
}

/// One stackbar the daemon wants on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackbarSpec {
    /// The container this bar belongs to. Any number the daemon keeps stable
    /// for the life of the container will do; it is the identity of the bar.
    pub id: u64,
    /// The container's rectangle. The bar goes along the top of it.
    pub rect: Rect,
    /// The window to sit directly above in the z-order, normally the focused
    /// window of the stack. This crate never modifies that window.
    pub above: WindowHandle,
    /// The tabs, left to right.
    pub tabs: Vec<StackbarTab>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mode_decides_which_containers_get_a_bar() {
        assert!(!StackbarMode::Never.shows(5));
        assert!(StackbarMode::Always.shows(1));
        assert!(!StackbarMode::Always.shows(0));
        assert!(!StackbarMode::OnStack.shows(1));
        assert!(StackbarMode::OnStack.shows(2));
    }

    #[test]
    fn the_default_is_off_and_themed() {
        let config = StackbarConfig::default();
        assert_eq!(config.mode, StackbarMode::Never, "stackbar is opt in");
        assert_eq!(config.focused_background.to_hex(), "#ffbbdf");
        assert_eq!(config.unfocused_background.to_hex(), "#313244");
        assert_eq!(config.height, 40);
    }

    #[test]
    fn a_partial_config_keeps_the_defaults() {
        let config: StackbarConfig =
            serde_json::from_str(r#"{"mode":"OnStack","height":32}"#).unwrap();
        assert_eq!(config.mode, StackbarMode::OnStack);
        assert_eq!(config.height, 32);
        assert_eq!(config.tab_width, 220);
        assert_eq!(config.label, StackbarLabel::Process);
    }

    #[test]
    fn config_round_trips_through_json() {
        let config = StackbarConfig {
            mode: StackbarMode::OnStack,
            font_family: Some("JetBrainsMono Nerd Font".to_string()),
            ..Default::default()
        };
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<StackbarConfig>(&text).unwrap(),
            config
        );
    }
}
