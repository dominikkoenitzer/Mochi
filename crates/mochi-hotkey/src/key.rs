//! Virtual key codes and the names a hotkey file spells them with.
//!
//! The table is the only place in Mochi that knows a key by name. It carries
//! the raw Win32 codes as literals rather than pulling in the `windows` crate,
//! so the parser builds and its tests run on any platform while the keyboard
//! hook that consumes a [`Key`] stays Windows only.
//!
//! Every code has exactly one canonical spelling, the one [`Key::name`] hands
//! back, plus any number of aliases that [`Key::from_name`] also accepts. The
//! comparison is ASCII case insensitive, so `Enter`, `enter` and `RETURN` all
//! arrive at the same key.

use serde::{Deserialize, Serialize};

/// A Win32 virtual key code, the `wParam` of a low level keyboard hook.
///
/// ```
/// use mochi_hotkey::Key;
///
/// let h = Key::from_name("H").unwrap();
/// assert_eq!(h.vk(), 0x48);
/// assert_eq!(h.name(), Some("h"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Key(u16);

impl Key {
    /// Wraps a raw virtual key code, named or not.
    pub const fn new(vk: u16) -> Self {
        Self(vk)
    }

    /// The raw virtual key code.
    pub const fn vk(self) -> u16 {
        self.0
    }

    /// Looks a key up by any of its spellings, ignoring ASCII case.
    ///
    /// Returns `None` for a name the table does not know, which is what the
    /// parser turns into an "unknown key name" error.
    pub fn from_name(name: &str) -> Option<Self> {
        CANONICAL
            .iter()
            .chain(ALIASES)
            .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
            .map(|&(_, vk)| Self(vk))
    }

    /// The canonical spelling, or `None` for a code that has no name.
    ///
    /// Round-trips: `Key::from_name(k.name()?) == Some(k)`.
    pub fn name(self) -> Option<&'static str> {
        CANONICAL
            .iter()
            .find(|(_, vk)| *vk == self.0)
            .map(|&(spelling, _)| spelling)
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "vk(0x{:02X})", self.0),
        }
    }
}

/// Every key Mochi can bind, spelled the way [`Key::name`] gives it back.
///
/// No two rows share a virtual key code; alternative spellings live in
/// [`ALIASES`]. The numbers are the Win32 `VK_*` constants written out so this
/// crate stays free of Windows-only dependencies.
const CANONICAL: &[(&str, u16)] = &[
    // Letters, VK_A through VK_Z. Windows has no separate codes for the
    // shifted forms, so a capital A is this key plus the shift modifier.
    ("a", 0x41),
    ("b", 0x42),
    ("c", 0x43),
    ("d", 0x44),
    ("e", 0x45),
    ("f", 0x46),
    ("g", 0x47),
    ("h", 0x48),
    ("i", 0x49),
    ("j", 0x4A),
    ("k", 0x4B),
    ("l", 0x4C),
    ("m", 0x4D),
    ("n", 0x4E),
    ("o", 0x4F),
    ("p", 0x50),
    ("q", 0x51),
    ("r", 0x52),
    ("s", 0x53),
    ("t", 0x54),
    ("u", 0x55),
    ("v", 0x56),
    ("w", 0x57),
    ("x", 0x58),
    ("y", 0x59),
    ("z", 0x5A),
    // Digits along the top row, VK_0 through VK_9.
    ("0", 0x30),
    ("1", 0x31),
    ("2", 0x32),
    ("3", 0x33),
    ("4", 0x34),
    ("5", 0x35),
    ("6", 0x36),
    ("7", 0x37),
    ("8", 0x38),
    ("9", 0x39),
    // Function keys, VK_F1 through VK_F24.
    ("f1", 0x70),
    ("f2", 0x71),
    ("f3", 0x72),
    ("f4", 0x73),
    ("f5", 0x74),
    ("f6", 0x75),
    ("f7", 0x76),
    ("f8", 0x77),
    ("f9", 0x78),
    ("f10", 0x79),
    ("f11", 0x7A),
    ("f12", 0x7B),
    ("f13", 0x7C),
    ("f14", 0x7D),
    ("f15", 0x7E),
    ("f16", 0x7F),
    ("f17", 0x80),
    ("f18", 0x81),
    ("f19", 0x82),
    ("f20", 0x83),
    ("f21", 0x84),
    ("f22", 0x85),
    ("f23", 0x86),
    ("f24", 0x87),
    // Numeric keypad digits, VK_NUMPAD0 through VK_NUMPAD9. The keypad
    // Enter shares VK_RETURN with the main one and is told apart only by
    // the extended-key flag, so it gets no name of its own here.
    ("numpad0", 0x60),
    ("numpad1", 0x61),
    ("numpad2", 0x62),
    ("numpad3", 0x63),
    ("numpad4", 0x64),
    ("numpad5", 0x65),
    ("numpad6", 0x66),
    ("numpad7", 0x67),
    ("numpad8", 0x68),
    ("numpad9", 0x69),
    // Numeric keypad operators.
    ("multiply", 0x6A), // VK_MULTIPLY
    ("add", 0x6B),      // VK_ADD
    ("subtract", 0x6D), // VK_SUBTRACT
    ("decimal", 0x6E),  // VK_DECIMAL
    ("divide", 0x6F),   // VK_DIVIDE
    // Navigation and editing.
    ("backspace", 0x08),   // VK_BACK
    ("tab", 0x09),         // VK_TAB
    ("enter", 0x0D),       // VK_RETURN
    ("pause", 0x13),       // VK_PAUSE
    ("capslock", 0x14),    // VK_CAPITAL
    ("esc", 0x1B),         // VK_ESCAPE
    ("space", 0x20),       // VK_SPACE
    ("pageup", 0x21),      // VK_PRIOR
    ("pagedown", 0x22),    // VK_NEXT
    ("end", 0x23),         // VK_END
    ("home", 0x24),        // VK_HOME
    ("left", 0x25),        // VK_LEFT
    ("up", 0x26),          // VK_UP
    ("right", 0x27),       // VK_RIGHT
    ("down", 0x28),        // VK_DOWN
    ("printscreen", 0x2C), // VK_SNAPSHOT
    ("insert", 0x2D),      // VK_INSERT
    ("delete", 0x2E),      // VK_DELETE
    ("apps", 0x5D),        // VK_APPS, the context-menu key
    ("scrolllock", 0x91),  // VK_SCROLL
    // Browser and media keys, the ones a keyboard puts on its top row.
    ("browserback", 0xA6),      // VK_BROWSER_BACK
    ("browserforward", 0xA7),   // VK_BROWSER_FORWARD
    ("browserrefresh", 0xA8),   // VK_BROWSER_REFRESH
    ("browserstop", 0xA9),      // VK_BROWSER_STOP
    ("browsersearch", 0xAA),    // VK_BROWSER_SEARCH
    ("browserfavorites", 0xAB), // VK_BROWSER_FAVORITES
    ("browserhome", 0xAC),      // VK_BROWSER_HOME
    ("volumemute", 0xAD),       // VK_VOLUME_MUTE
    ("volumedown", 0xAE),       // VK_VOLUME_DOWN
    ("volumeup", 0xAF),         // VK_VOLUME_UP
    ("medianext", 0xB0),        // VK_MEDIA_NEXT_TRACK
    ("mediaprev", 0xB1),        // VK_MEDIA_PREV_TRACK
    ("mediastop", 0xB2),        // VK_MEDIA_STOP
    ("mediaplaypause", 0xB3),   // VK_MEDIA_PLAY_PAUSE
    ("launchmail", 0xB4),       // VK_LAUNCH_MAIL
    ("launchmedia", 0xB5),      // VK_LAUNCH_MEDIA_SELECT
    ("launchapp1", 0xB6),       // VK_LAUNCH_APP1
    ("launchapp2", 0xB7),       // VK_LAUNCH_APP2
    // Punctuation. Windows calls these OEM keys because the engraving depends
    // on the layout; the names below are the US spelling of each code.
    ("semicolon", 0xBA), // VK_OEM_1
    ("plus", 0xBB),      // VK_OEM_PLUS
    ("comma", 0xBC),     // VK_OEM_COMMA
    ("minus", 0xBD),     // VK_OEM_MINUS
    ("period", 0xBE),    // VK_OEM_PERIOD
    ("slash", 0xBF),     // VK_OEM_2
    ("backtick", 0xC0),  // VK_OEM_3
    ("lbracket", 0xDB),  // VK_OEM_4
    ("backslash", 0xDC), // VK_OEM_5
    ("rbracket", 0xDD),  // VK_OEM_6
    ("quote", 0xDE),     // VK_OEM_7
    ("oem_8", 0xDF),     // VK_OEM_8, unlabelled on a US layout
];

/// Alternative spellings, each pointing at a code [`CANONICAL`] already names.
///
/// The `oem_*` rows are here because a file written to the common hotkey-file
/// conventions spells punctuation by its Windows code name; they resolve to the
/// same keys as the friendly names above.
const ALIASES: &[(&str, u16)] = &[
    ("return", 0x0D), // enter
    ("escape", 0x1B), // esc
    ("pgup", 0x21),   // pageup
    ("pgdn", 0x22),   // pagedown
    ("del", 0x2E),    // delete
    ("grave", 0xC0),  // backtick
    ("oem_1", 0xBA),  // semicolon
    ("oem_2", 0xBF),  // slash
    ("oem_3", 0xC0),  // backtick
    ("oem_4", 0xDB),  // lbracket
    ("oem_5", 0xDC),  // backslash
    ("oem_6", 0xDD),  // rbracket
    ("oem_7", 0xDE),  // quote
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_name_round_trips_through_from_name() {
        for &(spelling, vk) in CANONICAL {
            let key = Key::from_name(spelling)
                .unwrap_or_else(|| panic!("{spelling} is in the table but does not look up"));
            assert_eq!(key.vk(), vk, "for {spelling}");
            assert_eq!(key.name(), Some(spelling), "for {spelling}");
        }
    }

    #[test]
    fn no_two_canonical_names_share_a_virtual_key() {
        let mut seen = std::collections::HashMap::new();
        for &(spelling, vk) in CANONICAL {
            if let Some(first) = seen.insert(vk, spelling) {
                panic!("0x{vk:02X} is named both {first} and {spelling}");
            }
        }
    }

    #[test]
    fn no_spelling_is_listed_twice() {
        let mut seen = std::collections::HashSet::new();
        for &(spelling, _) in CANONICAL.iter().chain(ALIASES) {
            assert!(seen.insert(spelling), "{spelling} appears twice");
        }
    }

    #[test]
    fn every_alias_stands_in_for_a_canonical_name() {
        for &(spelling, vk) in ALIASES {
            let key = Key::from_name(spelling).unwrap();
            assert_eq!(key.vk(), vk);
            assert!(
                key.name().is_some(),
                "{spelling} points at the unnamed code 0x{vk:02X}"
            );
            assert_ne!(key.name(), Some(spelling), "{spelling} is not an alias");
        }
    }

    #[test]
    fn aliases_agree_with_the_keys_they_stand_for() {
        let same = |alias: &str, canonical: &str| {
            assert_eq!(
                Key::from_name(alias),
                Key::from_name(canonical),
                "{alias} should be {canonical}"
            );
        };
        same("return", "enter");
        same("escape", "esc");
        same("del", "delete");
        same("pgup", "pageup");
        same("pgdn", "pagedown");
        same("grave", "backtick");
        same("oem_1", "semicolon");
        same("oem_5", "backslash");
    }

    #[test]
    fn a_lookup_ignores_case() {
        assert_eq!(Key::from_name("H"), Key::from_name("h"));
        assert_eq!(Key::from_name("F11"), Key::from_name("f11"));
        assert_eq!(Key::from_name("PageUp"), Key::from_name("pageup"));
        assert_eq!(Key::from_name("OEM_3"), Key::from_name("backtick"));
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert_eq!(Key::from_name("hyper"), None);
        assert_eq!(Key::from_name("f25"), None);
        assert_eq!(Key::from_name(""), None);
        assert_eq!(Key::from_name("numpadenter"), None);
    }

    #[test]
    fn letters_digits_and_function_keys_sit_where_windows_puts_them() {
        assert_eq!(Key::from_name("a").unwrap().vk(), 0x41);
        assert_eq!(Key::from_name("z").unwrap().vk(), 0x5A);
        assert_eq!(Key::from_name("0").unwrap().vk(), 0x30);
        assert_eq!(Key::from_name("9").unwrap().vk(), 0x39);
        assert_eq!(Key::from_name("f1").unwrap().vk(), 0x70);
        assert_eq!(Key::from_name("f24").unwrap().vk(), 0x87);
        assert_eq!(Key::from_name("numpad0").unwrap().vk(), 0x60);
        assert_eq!(Key::from_name("numpad9").unwrap().vk(), 0x69);
    }

    #[test]
    fn a_code_without_a_name_displays_as_its_number() {
        assert_eq!(Key::new(0x07).to_string(), "vk(0x07)");
        assert_eq!(Key::new(0xFF).to_string(), "vk(0xFF)");
        assert_eq!(Key::new(0x48).to_string(), "h");
    }
}
