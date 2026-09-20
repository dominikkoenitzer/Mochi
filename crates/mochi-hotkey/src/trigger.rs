//! The left-hand side of a binding: a set of modifiers plus one key.
//!
//! A trigger is four booleans and a `u16`, so it is [`Copy`] and cheap to hash.
//! That matters because the keyboard hook builds one on every key press and
//! looks it up; nothing here allocates.

use serde::{Deserialize, Serialize};

use crate::key::Key;

/// The modifier keys held down alongside the trigger key.
///
/// Left and right variants are not told apart: `alt` matches either Alt key.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Modifiers {
    /// Either Alt key.
    pub alt: bool,
    /// Either Ctrl key.
    pub ctrl: bool,
    /// Either Shift key.
    pub shift: bool,
    /// Either Windows key, also spelled `super` or `meta`.
    pub win: bool,
}

impl Modifiers {
    /// No modifier at all, the same as [`Modifiers::default`].
    pub const NONE: Self = Self {
        alt: false,
        ctrl: false,
        shift: false,
        win: false,
    };

    /// Builds a set from four flags, in the order they are displayed.
    pub const fn from_flags(alt: bool, ctrl: bool, shift: bool, win: bool) -> Self {
        Self {
            alt,
            ctrl,
            shift,
            win,
        }
    }

    /// How many modifiers are held.
    ///
    /// The hook uses it to prefer the most specific binding when two could
    /// match, and the parser uses it to spot a trigger with no modifier.
    pub fn count(self) -> u32 {
        u32::from(self.alt) + u32::from(self.ctrl) + u32::from(self.shift) + u32::from(self.win)
    }

    /// Reads one modifier name, ignoring ASCII case.
    ///
    /// `ctrl` and `control` are the same modifier, as are `win`, `super` and
    /// `meta`. Returns `false` for anything that is not a modifier at all.
    pub(crate) fn apply_name(&mut self, name: &str) -> bool {
        let field = match name.to_ascii_lowercase().as_str() {
            "alt" => &mut self.alt,
            "ctrl" | "control" => &mut self.ctrl,
            "shift" => &mut self.shift,
            "win" | "super" | "meta" => &mut self.win,
            _ => return false,
        };
        *field = true;
        true
    }
}

impl std::fmt::Display for Modifiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut first = true;
        for (held, name) in [
            (self.alt, "alt"),
            (self.ctrl, "ctrl"),
            (self.shift, "shift"),
            (self.win, "win"),
        ] {
            if held {
                if !first {
                    f.write_str(" + ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

/// A key combination: the modifiers and the one key that fires the binding.
///
/// ```
/// use mochi_hotkey::Trigger;
///
/// let trigger: Trigger = "ALT + Shift + H".parse().unwrap();
/// assert_eq!(trigger.to_string(), "alt + shift + h");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Trigger {
    /// The modifiers that must be held.
    pub modifiers: Modifiers,
    /// The key that fires the binding.
    pub key: Key,
}

impl Trigger {
    /// Builds a trigger from its two halves.
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }
}

impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.modifiers.count() > 0 {
            write!(f, "{} + {}", self.modifiers, self.key)
        } else {
            write!(f, "{}", self.key)
        }
    }
}

impl std::str::FromStr for Trigger {
    /// A sentence saying which part of the trigger was wrong, ready to become
    /// the `message` of a [`crate::ParseError`].
    type Err = String;

    /// Reads `mod + mod + key`, ignoring case and whitespace around the `+`.
    ///
    /// Modifiers may come in any order, but only the last token is allowed to
    /// be the key, so an unreadable token can be blamed on the right half.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let tokens: Vec<&str> = s.split('+').map(str::trim).collect();
        let mut modifiers = Modifiers::NONE;
        let mut key = None;

        for (index, token) in tokens.iter().enumerate() {
            let last = index + 1 == tokens.len();
            if token.is_empty() {
                return Err(if last {
                    "the trigger ends in a `+` with no key after it".to_owned()
                } else {
                    "the trigger has an empty modifier between two `+`".to_owned()
                });
            }
            if modifiers.apply_name(token) {
                continue;
            }
            match Key::from_name(token) {
                Some(found) => {
                    if let Some(previous) = key.replace(found) {
                        return Err(format!(
                            "a trigger takes exactly one key, this one has `{previous}` and `{token}`"
                        ));
                    }
                }
                None if last => return Err(format!("`{token}` is not a key name")),
                None => return Err(format!("`{token}` is not a modifier")),
            }
        }

        match key {
            Some(key) => Ok(Self { modifiers, key }),
            None => Err(format!(
                "`{}` is all modifiers and no key",
                s.trim().to_ascii_lowercase()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(s: &str) -> Trigger {
        s.parse().unwrap()
    }

    fn error(s: &str) -> String {
        s.parse::<Trigger>().unwrap_err()
    }

    #[test]
    fn a_trigger_reads_its_modifiers_in_any_order() {
        let expected = Trigger::new(
            Modifiers::from_flags(true, false, true, false),
            Key::from_name("h").unwrap(),
        );
        assert_eq!(trigger("alt + shift + h"), expected);
        assert_eq!(trigger("shift + alt + h"), expected);
        assert_eq!(trigger("ALT+SHIFT+H"), expected);
        assert_eq!(trigger("  alt   +shift+   h  "), expected);
    }

    #[test]
    fn a_bare_key_needs_no_modifier() {
        let f1 = trigger("f1");
        assert_eq!(f1.modifiers, Modifiers::NONE);
        assert_eq!(f1.modifiers.count(), 0);
        assert_eq!(f1.to_string(), "f1");
    }

    #[test]
    fn modifier_aliases_land_on_the_same_flag() {
        assert_eq!(trigger("control + a"), trigger("ctrl + a"));
        assert_eq!(trigger("super + a"), trigger("win + a"));
        assert_eq!(trigger("meta + a"), trigger("win + a"));
    }

    #[test]
    fn a_repeated_modifier_is_harmless() {
        assert_eq!(trigger("alt + alt + h"), trigger("alt + h"));
    }

    #[test]
    fn display_uses_the_canonical_spelling_and_order() {
        assert_eq!(
            trigger("SHIFT + Control + Meta + ALT + F5").to_string(),
            "alt + ctrl + shift + win + f5"
        );
        assert_eq!(trigger("win + return").to_string(), "win + enter");
    }

    #[test]
    fn a_trigger_round_trips_through_its_display_form() {
        for text in ["alt + h", "alt + shift + 1", "ctrl + win + backtick", "f12"] {
            assert_eq!(trigger(text).to_string(), text);
        }
    }

    #[test]
    fn an_unknown_modifier_says_it_is_a_modifier_problem() {
        assert_eq!(error("hyper + h"), "`hyper` is not a modifier");
    }

    #[test]
    fn an_unknown_key_says_it_is_a_key_problem() {
        assert_eq!(error("alt + wiggle"), "`wiggle` is not a key name");
    }

    #[test]
    fn a_trigger_with_no_key_is_rejected() {
        assert_eq!(
            error("alt + shift"),
            "`alt + shift` is all modifiers and no key"
        );
    }

    #[test]
    fn a_trigger_with_two_keys_is_rejected() {
        assert!(error("alt + h + j").contains("exactly one key"));
    }

    #[test]
    fn an_empty_or_dangling_trigger_is_rejected() {
        assert!(error("").contains("no key"));
        assert!(error("alt + ").contains("no key after it"));
        assert!(error("alt + + h").contains("empty modifier"));
    }

    #[test]
    fn counting_modifiers_matches_the_flags() {
        assert_eq!(Modifiers::NONE.count(), 0);
        assert_eq!(Modifiers::from_flags(true, false, false, false).count(), 1);
        assert_eq!(Modifiers::from_flags(true, true, true, true).count(), 4);
        assert_eq!(Modifiers::default(), Modifiers::NONE);
    }

    #[test]
    fn an_empty_modifier_set_displays_as_nothing() {
        assert_eq!(Modifiers::NONE.to_string(), "");
        assert_eq!(
            Modifiers::from_flags(false, true, false, true).to_string(),
            "ctrl + win"
        );
    }
}
