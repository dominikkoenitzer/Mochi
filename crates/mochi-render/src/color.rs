//! The colour type used by every visual.
//!
//! Colours are written the way a config file writes them, `"#ffbbdf"`, and are
//! stored as straight (not premultiplied) 8-bit sRGB channels plus alpha.
//! Direct2D wants straight colours too and premultiplies internally when the
//! render target says so, so no conversion happens here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::RenderError;

/// An sRGB colour with an alpha channel.
///
/// Deserialises from `"#rgb"`, `"#rrggbb"`, `"#rrggbbaa"`, from an object
/// `{ "r": 255, "g": 187, "b": 223 }` with an optional `"a"`, or from a plain
/// number `0x00ff_bbdf`. Serialises back to the short hex string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red, 0-255.
    pub r: u8,
    /// Green, 0-255.
    pub g: u8,
    /// Blue, 0-255.
    pub b: u8,
    /// Alpha, 0 fully transparent, 255 fully opaque.
    pub a: u8,
}

impl Color {
    /// Fully transparent black, the colour of everything a border window does
    /// not paint.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// An opaque colour.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// A colour with an explicit alpha.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parses `#rgb`, `#rrggbb` or `#rrggbbaa`. The leading `#` is optional.
    ///
    /// # Errors
    ///
    /// [`RenderError::Color`] when the string is not one of those three shapes.
    pub fn from_hex(text: &str) -> crate::Result<Self> {
        let hex = text.trim().trim_start_matches('#');
        let bad = || RenderError::Color(text.to_string());
        let nibble = |i: usize| -> crate::Result<u8> {
            let byte = hex.as_bytes().get(i).copied().ok_or_else(bad)?;
            char::from(byte)
                .to_digit(16)
                .map(|d| d as u8)
                .ok_or_else(bad)
        };
        let byte = |i: usize| -> crate::Result<u8> { Ok(nibble(i)? << 4 | nibble(i + 1)?) };

        match hex.len() {
            3 => Ok(Self::rgb(
                nibble(0)? * 0x11,
                nibble(1)? * 0x11,
                nibble(2)? * 0x11,
            )),
            6 => Ok(Self::rgb(byte(0)?, byte(2)?, byte(4)?)),
            8 => Ok(Self::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
            _ => Err(bad()),
        }
    }

    /// The `#rrggbb` form, or `#rrggbbaa` when the colour is translucent.
    #[must_use]
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// The colour as a `0x00bbggrr` `COLORREF`, the order GDI uses.
    #[must_use]
    pub const fn to_colorref(self) -> u32 {
        (self.b as u32) << 16 | (self.g as u32) << 8 | (self.r as u32)
    }

    /// The colour with a different alpha.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// The four channels as the floats Direct2D wants.
    #[must_use]
    pub fn to_f32(self) -> [f32; 4] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
            f32::from(self.a) / 255.0,
        ]
    }
}

#[cfg(windows)]
impl Color {
    /// The colour as a Direct2D straight-alpha colour.
    #[must_use]
    pub fn to_d2d(self) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
        let [r, g, b, a] = self.to_f32();
        windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r, g, b, a }
    }
}

impl Default for Color {
    fn default() -> Self {
        Self::TRANSPARENT
    }
}

impl std::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl std::str::FromStr for Color {
    type Err = RenderError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

/// The three shapes a colour may take in a config file.
#[derive(Deserialize)]
#[serde(untagged)]
enum ColorRepr {
    Hex(String),
    Packed(u32),
    Channels {
        r: u8,
        g: u8,
        b: u8,
        #[serde(default = "opaque")]
        a: u8,
    },
}

const fn opaque() -> u8 {
    255
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match ColorRepr::deserialize(deserializer)? {
            ColorRepr::Hex(text) => Color::from_hex(&text).map_err(serde::de::Error::custom),
            // 0x00rrggbb, the packed form a JSON config writes as a number.
            ColorRepr::Packed(packed) => Ok(Color::rgb(
                ((packed >> 16) & 0xff) as u8,
                ((packed >> 8) & 0xff) as u8,
                (packed & 0xff) as u8,
            )),
            ColorRepr::Channels { r, g, b, a } => Ok(Color::rgba(r, g, b, a)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_rice_colours() {
        assert_eq!(
            Color::from_hex("#ffbbdf").unwrap(),
            Color::rgb(255, 187, 223)
        );
        assert_eq!(Color::from_hex("#313244").unwrap(), Color::rgb(49, 50, 68));
        assert_eq!(Color::from_hex("313244").unwrap(), Color::rgb(49, 50, 68));
    }

    #[test]
    fn parses_short_and_alpha_forms() {
        assert_eq!(Color::from_hex("#fff").unwrap(), Color::rgb(255, 255, 255));
        assert_eq!(Color::from_hex("#f0a").unwrap(), Color::rgb(255, 0, 170));
        assert_eq!(
            Color::from_hex("#ffbbdfeb").unwrap(),
            Color::rgba(255, 187, 223, 235)
        );
    }

    #[test]
    fn rejects_nonsense() {
        for bad in ["", "#", "#12", "#12345", "#gggggg", "pink"] {
            assert!(Color::from_hex(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn hex_round_trips() {
        let c = Color::rgb(255, 187, 223);
        assert_eq!(c.to_hex(), "#ffbbdf");
        assert_eq!(Color::from_hex(&c.to_hex()).unwrap(), c);
        let t = Color::rgba(1, 2, 3, 4);
        assert_eq!(t.to_hex(), "#01020304");
        assert_eq!(Color::from_hex(&t.to_hex()).unwrap(), t);
    }

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(Color::rgb(0xff, 0xbb, 0xdf).to_colorref(), 0x00df_bbff);
    }

    #[test]
    fn deserialises_every_shape() {
        let hex: Color = serde_json::from_str("\"#ffbbdf\"").unwrap();
        let object: Color = serde_json::from_str(r#"{"r":255,"g":187,"b":223}"#).unwrap();
        let packed: Color = serde_json::from_str("16759775").unwrap();
        assert_eq!(hex, Color::rgb(255, 187, 223));
        assert_eq!(object, hex);
        assert_eq!(packed, hex);
        let with_alpha: Color = serde_json::from_str(r#"{"r":1,"g":2,"b":3,"a":4}"#).unwrap();
        assert_eq!(with_alpha, Color::rgba(1, 2, 3, 4));
    }

    #[test]
    fn serialises_to_a_hex_string() {
        assert_eq!(
            serde_json::to_string(&Color::rgb(255, 187, 223)).unwrap(),
            "\"#ffbbdf\""
        );
    }

    #[test]
    fn to_f32_is_normalised() {
        let [r, g, b, a] = Color::rgba(255, 0, 255, 0).to_f32();
        assert!((r - 1.0).abs() < f32::EPSILON);
        assert!(g.abs() < f32::EPSILON);
        assert!((b - 1.0).abs() < f32::EPSILON);
        assert!(a.abs() < f32::EPSILON);
    }
}
