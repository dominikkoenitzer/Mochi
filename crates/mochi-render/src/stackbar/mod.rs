//! The stackbar: a row of tabs above a stacked container.
//!
//! One window of a stack is visible at a time, so the bar is the only way to
//! see what else is in there. Each tab is a window; clicking one asks the
//! daemon to focus it through the callback given to
//! [`StackbarManager::new`].
//!
//! Like the borders, the daemon sends the whole desired end state and the
//! stackbar thread works out the difference.
//!
//! The configuration is `mochi-core`'s [`StackbarConfig`], the `stackbar` block
//! of the config file, where every value is optional. [`StackbarStyle`] is that
//! block with every value resolved, which is what the painter works from.

use mochi_core::config::{Colour, StackbarConfig, StackbarLabel, StackbarMode};

use crate::{Rect, WindowHandle};

mod layout;
#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod window;

pub use layout::TabLayout;
#[cfg(windows)]
pub use manager::StackbarManager;

/// The question the thread has to ask the configured mode.
pub trait StackbarModeExt {
    /// Whether a container with `windows` windows gets a bar.
    #[must_use]
    fn shows(self, windows: usize) -> bool;
}

impl StackbarModeExt for StackbarMode {
    fn shows(self, windows: usize) -> bool {
        match self {
            StackbarMode::Never => false,
            StackbarMode::Always => windows > 0,
            StackbarMode::OnStack => windows > 1,
        }
    }
}

/// The `stackbar` block with every value resolved.
///
/// The config file leaves everything optional, and the painter needs a number
/// for every one of them. The defaults are Catppuccin Mocha with the pink
/// accent: the accent for the tab that is on top with the dark base as its
/// text, Surface0 with the normal text colour for the rest.
///
/// One thing has no key of its own: the config file has a single tab
/// `background`, which is the colour of the bar and of every tab that is not on
/// top. The focused tab always uses the accent.
#[derive(Debug, Clone, PartialEq)]
pub struct StackbarStyle {
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
    pub focused_background: Colour,
    /// The other tabs, and the bar behind them.
    pub unfocused_background: Colour,
    /// Text on the focused tab.
    pub focused_text: Colour,
    /// Text on the other tabs.
    pub unfocused_text: Colour,
}

impl Default for StackbarStyle {
    fn default() -> Self {
        Self {
            mode: StackbarMode::default(),
            height: 40,
            tab_width: 220,
            label: StackbarLabel::default(),
            font_family: None,
            font_size: 12.0,
            focused_background: Colour::new(0xff, 0xbb, 0xdf),
            unfocused_background: Colour::new(0x31, 0x32, 0x44),
            focused_text: Colour::new(0x1e, 0x1e, 0x2e),
            unfocused_text: Colour::new(0xcd, 0xd6, 0xf4),
        }
    }
}

impl From<&StackbarConfig> for StackbarStyle {
    fn from(config: &StackbarConfig) -> Self {
        let fallback = Self::default();
        let tabs = config.tabs.clone().unwrap_or_default();
        Self {
            mode: config.mode.unwrap_or(fallback.mode),
            height: config.height.unwrap_or(fallback.height),
            tab_width: tabs.width.unwrap_or(fallback.tab_width),
            label: config.label.unwrap_or(fallback.label),
            font_family: tabs.font_family,
            font_size: tabs
                .font_size
                .map_or(fallback.font_size, |points| points as f32),
            focused_background: fallback.focused_background,
            unfocused_background: tabs.background.unwrap_or(fallback.unfocused_background),
            focused_text: tabs.focused_text.unwrap_or(fallback.focused_text),
            unfocused_text: tabs.unfocused_text.unwrap_or(fallback.unfocused_text),
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
    fn an_empty_block_resolves_to_the_rice() {
        let style = StackbarStyle::from(&StackbarConfig::default());
        assert_eq!(style, StackbarStyle::default());
        assert_eq!(style.mode, StackbarMode::OnStack, "bars on stacks only");
        assert_eq!(style.focused_background.to_hex(), "#ffbbdf");
        assert_eq!(style.unfocused_background.to_hex(), "#313244");
        assert_eq!(style.height, 40);
        assert_eq!(style.tab_width, 220);
    }

    #[test]
    fn the_config_block_resolves_to_a_style() {
        let config: StackbarConfig = serde_json::from_str(
            r##"{
                "height": 32,
                "mode": "Always",
                "label": "Process",
                "tabs": {
                    "width": 200,
                    "focused_text": "#111111",
                    "unfocused_text": "#222222",
                    "background": "#333333",
                    "font_family": "JetBrainsMono Nerd Font",
                    "font_size": 14
                }
            }"##,
        )
        .unwrap();

        let style = StackbarStyle::from(&config);
        assert_eq!(style.mode, StackbarMode::Always);
        assert_eq!(style.height, 32);
        assert_eq!(style.tab_width, 200);
        assert_eq!(style.label, StackbarLabel::Process);
        assert_eq!(
            style.font_family.as_deref(),
            Some("JetBrainsMono Nerd Font")
        );
        assert!((style.font_size - 14.0).abs() < f32::EPSILON);
        assert_eq!(style.focused_text, Colour::new(0x11, 0x11, 0x11));
        assert_eq!(style.unfocused_text, Colour::new(0x22, 0x22, 0x22));
        assert_eq!(style.unfocused_background, Colour::new(0x33, 0x33, 0x33));
        assert_eq!(
            style.focused_background,
            StackbarStyle::default().focused_background,
            "the accent has no key of its own"
        );
    }

    #[test]
    fn a_partial_block_keeps_the_defaults() {
        let config: StackbarConfig =
            serde_json::from_str(r#"{"mode":"OnStack","height":32}"#).unwrap();
        let style = StackbarStyle::from(&config);
        assert_eq!(style.mode, StackbarMode::OnStack);
        assert_eq!(style.height, 32);
        assert_eq!(style.tab_width, 220);
        assert_eq!(style.label, StackbarLabel::Title);
    }
}
