//! Borders: one layered frame window per managed container.
//!
//! The daemon does not drive individual windows. It sends the whole desired end
//! state to [`BorderManager::set_borders`] after every layout pass, and the
//! border thread works out which frames to create, move, recolour or hide.
//!
//! ```no_run
//! use mochi_render::{BorderConfig, BorderKind, BorderManager, BorderSpec, Rect, WindowHandle};
//!
//! # fn main() -> mochi_render::Result<()> {
//! let borders = BorderManager::new(BorderConfig::default())?;
//! borders.set_borders(vec![BorderSpec {
//!     target: WindowHandle(0x1234),
//!     rect: Rect::new(100, 100, 900, 700),
//!     kind: BorderKind::Single,
//! }])?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::{Rect, WindowHandle};

#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod window;

#[cfg(windows)]
pub use manager::BorderManager;
#[cfg(windows)]
pub use window::BorderWindow;

/// What a container is, which decides which colour its border gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BorderKind {
    /// A container holding exactly one window, and focused.
    #[default]
    Single,
    /// A focused container holding a stack of windows.
    Stack,
    /// The focused container while the workspace is in monocle mode.
    Monocle,
    /// A focused floating window.
    Floating,
    /// Anything that does not have the focus.
    Unfocused,
}

/// Which shape the frame takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BorderStyle {
    /// Follow the operating system: rounded on Windows 11, square before that.
    #[default]
    System,
    /// Always rounded, with the radius Windows 11 uses, scaled for the monitor.
    Rounded,
    /// Always square.
    Square,
}

impl BorderStyle {
    /// Resolves [`BorderStyle::System`] against the running Windows version.
    #[must_use]
    pub fn is_rounded(self) -> bool {
        match self {
            BorderStyle::Rounded => true,
            BorderStyle::Square => false,
            #[cfg(windows)]
            BorderStyle::System => crate::win::os_rounds_corners(),
            #[cfg(not(windows))]
            BorderStyle::System => true,
        }
    }
}

/// One colour per container kind.
///
/// The field names match the `border_colours` block of the config file, so an
/// existing config migrates with a rename of the surrounding keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderColours {
    /// A focused container with one window.
    pub single: Color,
    /// A focused container with a stack of windows.
    pub stack: Color,
    /// The focused container in monocle mode.
    pub monocle: Color,
    /// A focused floating window.
    pub floating: Color,
    /// Everything that is not focused.
    pub unfocused: Color,
}

impl BorderColours {
    /// The colour for one kind of container.
    #[must_use]
    pub const fn for_kind(&self, kind: BorderKind) -> Color {
        match kind {
            BorderKind::Single => self.single,
            BorderKind::Stack => self.stack,
            BorderKind::Monocle => self.monocle,
            BorderKind::Floating => self.floating,
            BorderKind::Unfocused => self.unfocused,
        }
    }
}

impl Default for BorderColours {
    /// Catppuccin Mocha with the pink accent: one accent colour for everything
    /// focused, Surface0 for everything else.
    fn default() -> Self {
        const ACCENT: Color = Color::rgb(0xff, 0xbb, 0xdf);
        const SURFACE0: Color = Color::rgb(0x31, 0x32, 0x44);
        Self {
            single: ACCENT,
            stack: ACCENT,
            monocle: ACCENT,
            floating: ACCENT,
            unfocused: SURFACE0,
        }
    }
}

/// How the borders look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BorderConfig {
    /// Draw borders at all.
    pub enabled: bool,
    /// How thick the frame is, in physical pixels. Not scaled by DPI: a 6 px
    /// border is 6 px on the 4K monitor and on the 1080p one.
    pub width: i32,
    /// How far the frame sits outside the window, in physical pixels, on top of
    /// the width.
    ///
    /// The default `-1` laps the inner edge of the frame one pixel over the
    /// window, which hides the seam. The rect the daemon passes in has already
    /// had the invisible Windows 11 resize border taken off it, so this knob is
    /// purely cosmetic.
    pub offset: i32,
    /// Square, rounded, or whatever the OS does.
    pub style: BorderStyle,
    /// One colour per container kind.
    pub colours: BorderColours,
}

impl Default for BorderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 6,
            offset: -1,
            style: BorderStyle::Rounded,
            colours: BorderColours::default(),
        }
    }
}

/// One border the daemon wants on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderSpec {
    /// The window the frame belongs to. It is only used as the z-order anchor
    /// and as the identity of the border; this crate never changes it.
    pub target: WindowHandle,
    /// Where the window is, in physical screen pixels, with the invisible
    /// resize border already removed.
    pub rect: Rect,
    /// Which colour to use.
    pub kind: BorderKind,
}

impl BorderSpec {
    /// A spec from its parts.
    #[must_use]
    pub const fn new(target: WindowHandle, rect: Rect, kind: BorderKind) -> Self {
        Self { target, rect, kind }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_is_the_rice() {
        let config = BorderConfig::default();
        assert!(config.enabled);
        assert_eq!(config.width, 6);
        assert_eq!(config.offset, -1);
        assert_eq!(config.style, BorderStyle::Rounded);
        assert_eq!(config.colours.single.to_hex(), "#ffbbdf");
        assert_eq!(config.colours.unfocused.to_hex(), "#313244");
    }

    #[test]
    fn every_kind_maps_to_a_colour() {
        let colours = BorderColours::default();
        for kind in [
            BorderKind::Single,
            BorderKind::Stack,
            BorderKind::Monocle,
            BorderKind::Floating,
        ] {
            assert_eq!(colours.for_kind(kind), colours.single);
        }
        assert_eq!(
            colours.for_kind(BorderKind::Unfocused),
            colours.unfocused,
            "the unfocused colour has to be the odd one out"
        );
    }

    #[test]
    fn a_partial_config_keeps_the_defaults() {
        let config: BorderConfig =
            serde_json::from_str(r##"{"width":8,"colours":{"unfocused":"#000000"}}"##).unwrap();
        assert_eq!(config.width, 8);
        assert_eq!(config.offset, -1, "untouched keys keep their default");
        assert_eq!(config.colours.unfocused, Color::rgb(0, 0, 0));
        assert_eq!(config.colours.single.to_hex(), "#ffbbdf");
    }

    #[test]
    fn the_style_resolves_to_a_shape() {
        assert!(BorderStyle::Rounded.is_rounded());
        assert!(!BorderStyle::Square.is_rounded());
        // System depends on the OS; all that matters is that it answers.
        let _ = BorderStyle::System.is_rounded();
    }

    #[test]
    fn config_round_trips_through_json() {
        let config = BorderConfig::default();
        let text = serde_json::to_string(&config).unwrap();
        let back: BorderConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(config, back);
    }
}
