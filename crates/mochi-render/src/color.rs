//! Turning a configured colour into the shapes Direct2D wants.
//!
//! The colour type itself is [`mochi_core::config::Colour`], the one the config
//! file parses, so a border colour travels from the JSON file to the brush
//! without being converted into a second representation on the way. Colours are
//! straight (not premultiplied) 8-bit sRGB, which is also what Direct2D wants:
//! it premultiplies internally when the render target says so, so nothing is
//! converted here either.

use mochi_core::config::Colour;

/// Fully transparent black: the colour of every pixel a border window does not
/// paint.
///
/// [`Colour`] has no alpha channel, because the config file has no way to write
/// one, so the one colour in this crate that needs alpha is this constant.
#[cfg(windows)]
pub const TRANSPARENT: windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F =
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

/// The conversions the painters need from a configured colour.
pub trait ColourExt {
    /// The four channels as the floats Direct2D wants, alpha always opaque.
    #[must_use]
    fn to_f32(self) -> [f32; 4];

    /// The colour as an opaque Direct2D colour.
    #[cfg(windows)]
    #[must_use]
    fn to_d2d(self) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
}

impl ColourExt for Colour {
    fn to_f32(self) -> [f32; 4] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
            1.0,
        ]
    }

    #[cfg(windows)]
    fn to_d2d(self) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        let [r, g, b, a] = self.to_f32();
        windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r, g, b, a }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_f32_is_normalised_and_opaque() {
        let [r, g, b, a] = Colour::new(255, 0, 255).to_f32();
        assert!((r - 1.0).abs() < f32::EPSILON);
        assert!(g.abs() < f32::EPSILON);
        assert!((b - 1.0).abs() < f32::EPSILON);
        assert!(
            (a - 1.0).abs() < f32::EPSILON,
            "a border is never see-through"
        );
    }

    #[test]
    fn the_rice_colours_survive_the_trip() {
        let pink = Colour::parse("#ffbbdf").unwrap();
        let [r, g, b, _] = pink.to_f32();
        assert!((r - 1.0).abs() < f32::EPSILON);
        assert!((g - 187.0 / 255.0).abs() < f32::EPSILON);
        assert!((b - 223.0 / 255.0).abs() < f32::EPSILON);
    }

    #[cfg(windows)]
    #[test]
    fn the_clear_colour_is_invisible() {
        assert!(TRANSPARENT.a.abs() < f32::EPSILON);
    }
}
