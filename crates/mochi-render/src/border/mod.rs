//! Borders: one layered frame window per managed container.
//!
//! The daemon does not drive individual windows. It sends the whole desired end
//! state to [`BorderManager::update`] after every layout pass, and the manager
//! works out which frames to create, move, recolour or take down. Everything it
//! does not mention is left exactly as it is, which is what makes a per frame
//! call cheap enough to make on every animation frame.
//!
//! The look comes straight from the config file: [`BorderStyle`] and
//! [`BorderColours`] are `mochi-core`'s own types, and the extension traits
//! here answer the two questions a painter has that a config file cannot.
//!
//! ```no_run
//! use mochi_render::{BorderConfig, BorderKind, BorderManager, BorderSpec, Rect, WindowHandle};
//!
//! # fn main() -> mochi_render::Result<()> {
//! let borders = BorderManager::new(BorderConfig::default())?;
//! borders.update(
//!     Some(BorderSpec::new(
//!         WindowHandle(0x1234),
//!         Rect::new(100, 100, 900, 700),
//!         BorderKind::Single,
//!     )),
//!     vec![],
//! )?;
//! # Ok(())
//! # }
//! ```

use mochi_core::config::{BorderColours, BorderStyle, Colour, Config};
use serde::{Deserialize, Serialize};

use crate::{Rect, WindowHandle};

mod diff;
#[cfg(windows)]
mod manager;
#[cfg(windows)]
mod window;

pub use diff::{BorderChanges, BorderDiff};
#[cfg(windows)]
pub use manager::BorderManager;
#[cfg(windows)]
pub use window::BorderWindow;

/// The accent the rice uses for anything focused, for a config that names no
/// colour of its own.
pub const DEFAULT_FOCUSED: Colour = Colour::new(0xff, 0xbb, 0xdf);

/// Catppuccin Mocha Surface0, the colour of everything unfocused.
pub const DEFAULT_UNFOCUSED: Colour = Colour::new(0x31, 0x32, 0x44);

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

/// The shape question a painter has to ask the configured style.
pub trait BorderStyleExt {
    /// Resolves [`BorderStyle::System`] against the running Windows version.
    #[must_use]
    fn is_rounded(&self) -> bool;
}

impl BorderStyleExt for BorderStyle {
    fn is_rounded(&self) -> bool {
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

/// The colour question a painter has to ask the configured colours.
pub trait BorderColoursExt {
    /// The colour for one kind of container, falling back to
    /// [`DEFAULT_FOCUSED`] and [`DEFAULT_UNFOCUSED`] for anything the config
    /// file leaves out.
    #[must_use]
    fn resolve(&self, kind: BorderKind) -> Colour;
}

impl BorderColoursExt for BorderColours {
    fn resolve(&self, kind: BorderKind) -> Colour {
        match kind {
            BorderKind::Single => self.single.unwrap_or(DEFAULT_FOCUSED),
            BorderKind::Stack => self.stack.unwrap_or(DEFAULT_FOCUSED),
            BorderKind::Monocle => self.monocle.unwrap_or(DEFAULT_FOCUSED),
            BorderKind::Floating => self.floating.unwrap_or(DEFAULT_FOCUSED),
            BorderKind::Unfocused => self.unfocused.unwrap_or(DEFAULT_UNFOCUSED),
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
    /// One colour per container kind, resolved through [`BorderColoursExt`].
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

impl From<&Config> for BorderConfig {
    /// Reads the five `border*` keys of the config file. A file that does not
    /// mention borders does not get any.
    fn from(config: &Config) -> Self {
        let fallback = Self::default();
        Self {
            enabled: config.border.unwrap_or(false),
            width: config.border_width.unwrap_or(fallback.width),
            offset: config.border_offset.unwrap_or(fallback.offset),
            style: config.border_style.unwrap_or_default(),
            colours: config.border_colours.unwrap_or_default(),
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
        assert_eq!(
            config.colours.resolve(BorderKind::Single).to_hex(),
            "#ffbbdf"
        );
        assert_eq!(
            config.colours.resolve(BorderKind::Unfocused).to_hex(),
            "#313244"
        );
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
            assert_eq!(colours.resolve(kind), DEFAULT_FOCUSED);
        }
        assert_eq!(
            colours.resolve(BorderKind::Unfocused),
            DEFAULT_UNFOCUSED,
            "the unfocused colour has to be the odd one out"
        );
    }

    #[test]
    fn a_configured_colour_wins_over_the_default() {
        let colours: BorderColours = serde_json::from_str(r##"{"unfocused":"#000000"}"##).unwrap();
        assert_eq!(colours.resolve(BorderKind::Unfocused), Colour::new(0, 0, 0));
        assert_eq!(colours.resolve(BorderKind::Single), DEFAULT_FOCUSED);
    }

    #[test]
    fn a_partial_config_keeps_the_defaults() {
        let config: BorderConfig =
            serde_json::from_str(r##"{"width":8,"colours":{"unfocused":"#000000"}}"##).unwrap();
        assert_eq!(config.width, 8);
        assert_eq!(config.offset, -1, "untouched keys keep their default");
        assert_eq!(
            config.colours.resolve(BorderKind::Unfocused),
            Colour::new(0, 0, 0)
        );
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

    #[test]
    fn the_config_file_drives_the_border() {
        let config = Config::from_json(
            r##"{"border":true,"border_width":6,"border_offset":-1,"border_style":"Rounded",
                 "border_colours":{"single":"#ffbbdf","unfocused":"#313244"}}"##,
        )
        .unwrap();
        let borders = BorderConfig::from(&config);
        assert!(borders.enabled);
        assert_eq!(borders.width, 6);
        assert_eq!(borders.offset, -1);
        assert!(borders.style.is_rounded());
        assert_eq!(borders.colours.resolve(BorderKind::Single), DEFAULT_FOCUSED);

        assert!(
            !BorderConfig::from(&Config::default()).enabled,
            "a config that says nothing gets no borders"
        );
    }
}
