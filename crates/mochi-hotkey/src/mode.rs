//! Which set of bindings is live: the top level of the file, or a `mode` block.
//!
//! The state is one small integer, so the keyboard hook can keep it on its own
//! thread and switch it on the press that asked, with nothing shared and
//! nothing allocated.

use crate::parse::{Action, Binding, Bindings};
use crate::trigger::Trigger;

/// The name of the bindings outside every `mode` block, and what `mode
/// default` switches back to.
pub const DEFAULT_MODE: &str = "default";

/// One mode of one set of [`Bindings`]: the top level, or a `mode` block.
///
/// Only meaningful for the bindings it came from. Looked up in any other set,
/// an id either finds that set's block at the same place or nothing at all,
/// which is why the daemon goes back to [`ModeId::DEFAULT`] on every reload.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ModeId(usize);

impl ModeId {
    /// The bindings outside every `mode` block.
    pub const DEFAULT: Self = Self(0);

    /// The id of the block at `index` of [`Bindings::modes`].
    pub(crate) const fn of_index(index: usize) -> Self {
        Self(index + 1)
    }

    /// Where the block sits in [`Bindings::modes`], `None` for the top level.
    pub(crate) const fn index(self) -> Option<usize> {
        self.0.checked_sub(1)
    }

    /// True for the bindings outside every `mode` block.
    pub const fn is_default(self) -> bool {
        self.0 == 0
    }
}

/// The active mode, and the one rule for changing it.
///
/// A press is matched against the active mode's bindings only. A binding that
/// fires a [`Action::Mode`] switches to the mode it names before anything else
/// is looked up, so the next press already belongs to it. A key the active
/// mode does not bind matches nothing, and the caller hands it to the
/// application untouched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModeState {
    active: ModeId,
}

impl ModeState {
    /// Starts in [`ModeId::DEFAULT`].
    pub const fn new() -> Self {
        Self {
            active: ModeId::DEFAULT,
        }
    }

    /// The mode the next press is matched in.
    pub const fn active(self) -> ModeId {
        self.active
    }

    /// Goes back to the top-level bindings.
    pub const fn reset(&mut self) {
        self.active = ModeId::DEFAULT;
    }

    /// Matches one press in the active mode and follows a mode switch.
    ///
    /// `admit` gets the last word on a binding that matched: one it turns
    /// down is treated as no match at all, and a mode switch it turns down
    /// does not switch. The daemon uses it for game mode and for turning the
    /// hotkeys off.
    ///
    /// A switch to a name these bindings do not define goes back to the top
    /// level rather than nowhere. The parser never lets such a binding
    /// through, so this is only the safe answer to an impossible question.
    pub fn press<'a>(
        &mut self,
        bindings: &'a Bindings,
        trigger: Trigger,
        admit: impl FnOnce(&Action) -> bool,
    ) -> Option<&'a Binding> {
        let binding = bindings.get_in(self.active, trigger)?;
        if !admit(&binding.action) {
            return None;
        }
        if let Action::Mode(name) = &binding.action {
            self.active = bindings.mode_id(name).unwrap_or_default();
        }
        Some(binding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mochi_client::{Axis, Command, Sizing};

    const FILE: &str = "\
alt + shift + s : mode resize
alt + h         : focus left
mode resize {
    h           : resize-axis horizontal decrease
    l           : resize-axis horizontal increase
    m           : mode move
    esc         : mode default
    enter       : mode default
}
mode move {
    h           : move left
    esc         : mode default
    r           : mode resize
}
";

    fn trigger(text: &str) -> Trigger {
        text.parse().expect("the test spells its triggers right")
    }

    fn bindings() -> Bindings {
        Bindings::parse(FILE).expect("the test file parses")
    }

    /// Presses a key and says what it did: the binding's source, or `None`
    /// when the press would reach the application.
    fn press(state: &mut ModeState, bindings: &Bindings, key: &str) -> Option<String> {
        state
            .press(bindings, trigger(key), |_| true)
            .map(|binding| binding.source.clone())
    }

    #[test]
    fn it_starts_in_the_default_mode() {
        let state = ModeState::new();
        assert!(state.active().is_default());
        assert_eq!(state, ModeState::default());
        assert_eq!(bindings().mode_name(state.active()), "default");
    }

    #[test]
    fn entering_a_mode_swaps_the_whole_set_of_bindings() {
        let bindings = bindings();
        let mut state = ModeState::new();

        // Before: `h` alone is nobody's, `alt + h` is Mochi's.
        assert_eq!(press(&mut state, &bindings, "h"), None);
        assert_eq!(
            press(&mut state, &bindings, "alt + h").as_deref(),
            Some("focus left")
        );

        assert_eq!(
            press(&mut state, &bindings, "alt + shift + s").as_deref(),
            Some("mode resize")
        );
        assert_eq!(bindings.mode_name(state.active()), "resize");

        // Inside: `h` resizes, and the top level is not consulted at all.
        let resize = state
            .press(&bindings, trigger("h"), |_| true)
            .expect("h is bound in resize");
        assert_eq!(
            resize.action,
            Action::Command(Command::ResizeAxis {
                axis: Axis::Horizontal,
                sizing: Sizing::Decrease,
            })
        );
        assert_eq!(press(&mut state, &bindings, "alt + h"), None);
        // A command leaves the mode where it was, so resizing can go on.
        assert_eq!(bindings.mode_name(state.active()), "resize");
    }

    #[test]
    fn a_key_the_mode_does_not_bind_goes_to_the_application() {
        let bindings = bindings();
        let mut state = ModeState::new();
        press(&mut state, &bindings, "alt + shift + s");

        for key in ["a", "j", "shift + h", "alt + shift + s", "space"] {
            assert_eq!(press(&mut state, &bindings, key), None, "{key} was taken");
        }
        // And passing a key through changes nothing.
        assert_eq!(bindings.mode_name(state.active()), "resize");
    }

    #[test]
    fn escape_and_enter_both_leave() {
        let bindings = bindings();
        for key in ["esc", "enter"] {
            let mut state = ModeState::new();
            press(&mut state, &bindings, "alt + shift + s");
            assert_eq!(
                press(&mut state, &bindings, key).as_deref(),
                Some("mode default")
            );
            assert!(state.active().is_default(), "{key} did not leave");
            // Back to the top level: `h` is the application's again.
            assert_eq!(press(&mut state, &bindings, "h"), None);
            assert!(press(&mut state, &bindings, "alt + h").is_some());
        }
    }

    #[test]
    fn entering_a_mode_from_a_mode_switches_straight_to_it() {
        let bindings = bindings();
        let mut state = ModeState::new();
        press(&mut state, &bindings, "alt + shift + s");
        press(&mut state, &bindings, "m");
        assert_eq!(bindings.mode_name(state.active()), "move");
        assert_eq!(
            press(&mut state, &bindings, "h").as_deref(),
            Some("move left")
        );
        press(&mut state, &bindings, "r");
        assert_eq!(bindings.mode_name(state.active()), "resize");
        press(&mut state, &bindings, "esc");
        assert!(state.active().is_default());
    }

    #[test]
    fn a_switch_the_caller_turns_down_does_not_switch() {
        let bindings = bindings();
        let mut state = ModeState::new();
        let taken = state.press(&bindings, trigger("alt + shift + s"), |action| {
            !matches!(action, Action::Mode(_))
        });
        assert!(taken.is_none());
        assert!(state.active().is_default());
    }

    #[test]
    fn reset_goes_back_to_the_top_level() {
        let bindings = bindings();
        let mut state = ModeState::new();
        press(&mut state, &bindings, "alt + shift + s");
        state.reset();
        assert!(state.active().is_default());
        assert!(press(&mut state, &bindings, "alt + h").is_some());
    }

    #[test]
    fn an_id_from_other_bindings_finds_nothing_and_names_default() {
        let bindings = bindings();
        let mut state = ModeState::new();
        press(&mut state, &bindings, "alt + shift + s");

        // The daemon resets on a reload, but a stale id must still be harmless.
        let reloaded = Bindings::parse("alt + h : focus left").unwrap();
        assert_eq!(reloaded.mode_name(state.active()), "default");
        assert_eq!(press(&mut state, &reloaded, "h"), None);
        assert_eq!(press(&mut state, &reloaded, "alt + h"), None);
    }

    #[test]
    fn mode_ids_follow_the_file() {
        let bindings = bindings();
        assert_eq!(bindings.mode_id("default"), Some(ModeId::DEFAULT));
        assert_eq!(bindings.mode_id("DEFAULT"), Some(ModeId::DEFAULT));
        let resize = bindings.mode_id("resize").unwrap();
        let moving = bindings.mode_id("Move").unwrap();
        assert_ne!(resize, moving);
        assert_eq!(bindings.mode_name(resize), "resize");
        assert_eq!(bindings.mode_name(moving), "move");
        assert_eq!(bindings.mode_id("launch"), None);
    }
}
