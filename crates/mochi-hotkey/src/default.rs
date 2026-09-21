//! The hotkey file Mochi ships with.

/// The hotkey file `mochic quickstart` writes when there is none.
///
/// It is the whole feature set on one screen: every command worth a key appears
/// at least once, so the file doubles as the reference. The layout is the one
/// most tiling users already have in their fingers, vim keys for focus and the
/// same keys with Shift to move.
///
/// The tests below hold it to the grammar, so a command that is renamed or
/// dropped cannot leave a broken file behind for the next person who runs
/// `quickstart`.
pub const DEFAULT: &str = r"# Mochi hotkeys. Saved changes are picked up at once, no restart.
#
# Syntax:   modifier + modifier + key : command
# Commands: anything `mochic` takes, without the `mochic`. See `mochic --help`.
#           A line that does not start with a Mochi command is run by the shell.
#
# The way out is alt + shift + e. It stops Mochi, puts every window it was
# hiding back, takes the borders down and unbinds these keys, leaving the
# desktop the way it found it. It is the last line of this file.
.shell pwsh

# Focus
alt + h                 : focus left
alt + j                 : focus down
alt + k                 : focus up
alt + l                 : focus right

# The arrow keys are deliberately not bound here. Alt with an arrow is already
# taken across Windows: alt + left and alt + right are Back and Forward in
# every browser, and alt + up is the parent folder in Explorer. A window
# manager that binds them takes those away the moment it starts, everywhere,
# with nothing on screen to say why. Add them if you would rather have them:
#   alt + left : focus left
#   alt + down : focus down
#   alt + up   : focus up
#   alt + right: focus right

# Move the focused window. At a screen edge it crosses to the next monitor.
alt + shift + h         : move left
alt + shift + j         : move down
alt + shift + k         : move up
alt + shift + l         : move right
alt + shift + left      : move left
alt + shift + down      : move down
alt + shift + up        : move up
alt + shift + right     : move right

# Resize: u and p for width, i and o for height
alt + u                 : resize-axis horizontal decrease
alt + p                 : resize-axis horizontal increase
alt + i                 : resize-axis vertical decrease
alt + o                 : resize-axis vertical increase

# Window state
alt + shift + space     : toggle-float
alt + f                 : toggle-maximize
alt + shift + f         : toggle-monocle
alt + m                 : minimize
alt + shift + q         : close

# Layout
alt + v                 : cycle-layout next
alt + x                 : flip-layout horizontal
alt + y                 : flip-layout vertical
alt + return            : promote

# Workspaces
alt + a                 : cycle-workspace previous
alt + s                 : cycle-workspace next
alt + d                 : focus-last-workspace
alt + [1,2,3,4,5,6,7,8,9]         : focus-workspace [0,1,2,3,4,5,6,7,8]
alt + shift + [1,2,3,4,5,6,7,8,9] : move-to-workspace [0,1,2,3,4,5,6,7,8]

# Mochi itself. Game mode pauses tiling and gives the game every key but this
# one; press it again to come back.
alt + shift + g         : toggle-game-mode
alt + shift + p         : toggle-pause
alt + shift + r         : reload-configuration
alt + shift + w         : retile
alt + shift + e         : stop
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Bindings, Trigger};

    #[test]
    fn the_shipped_file_parses_with_nothing_left_over() {
        let (bindings, errors) = Bindings::parse_lossy(DEFAULT);
        assert!(
            errors.is_empty(),
            "the shipped hotkeys do not parse: {errors:?}"
        );
        assert!(bindings.len() >= 50, "only {} bindings", bindings.len());
    }

    #[test]
    fn every_line_of_the_shipped_file_is_a_mochi_command() {
        let bindings = Bindings::parse(DEFAULT).expect("the shipped hotkeys parse");
        for binding in bindings.iter() {
            assert!(
                matches!(binding.action, Action::Command(_)),
                "line {} is not a Mochi command: {}",
                binding.line,
                binding.source
            );
        }
    }

    #[test]
    fn the_shipped_file_leaves_the_windows_wide_alt_arrow_shortcuts_alone() {
        // alt + left and alt + right are Back and Forward in every browser on
        // Windows, and alt + up is the parent folder in Explorer. Binding them
        // takes those away from the user everywhere, silently, from the moment
        // Mochi starts. Whoever adds them back has to delete this test first
        // and read why.
        let bindings = Bindings::parse(DEFAULT).expect("the shipped hotkeys parse");
        for key in ["alt + left", "alt + right", "alt + up", "alt + down"] {
            assert!(
                bindings
                    .get(key.parse::<Trigger>().expect("the test spells its triggers right"))
                    .is_none(),
                "{key} is bound, and it belongs to Windows"
            );
        }
    }

    #[test]
    fn the_arrow_lines_the_file_offers_work_when_the_hash_is_deleted() {
        // The file tells the reader they can have the arrow keys back by
        // uncommenting four lines. That instruction has to be true, including
        // the indentation those lines are written with.
        let restored: String = DEFAULT
            .lines()
            .map(|line| line.strip_prefix('#').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("
");
        let bindings = Bindings::parse_lossy(&restored).0;
        for (key, command) in [
            ("alt + left", "focus left"),
            ("alt + down", "focus down"),
            ("alt + up", "focus up"),
            ("alt + right", "focus right"),
        ] {
            assert_eq!(
                bindings
                    .get(key.parse::<Trigger>().expect("spelled right"))
                    .map(|binding| binding.source.clone())
                    .as_deref(),
                Some(command),
                "uncommenting did not give back {key}"
            );
        }
    }

    #[test]
    fn the_shipped_file_always_has_a_way_out() {
        // A window manager that rearranges every window the moment it starts
        // has to be stoppable from the keyboard by someone who has not read
        // the manual. This is the one binding that must never go missing.
        let bindings = Bindings::parse(DEFAULT).expect("the shipped hotkeys parse");
        assert_eq!(
            bindings
                .get("alt + shift + e".parse::<Trigger>().expect("spelled right"))
                .map(|binding| binding.source.clone())
                .as_deref(),
            Some("stop"),
        );
        assert!(
            DEFAULT.contains("The way out is alt + shift + e"),
            "the file no longer says how to get out of it"
        );
    }

    #[test]
    fn the_shipped_file_binds_the_keys_the_documentation_promises() {
        let bindings = Bindings::parse(DEFAULT).expect("the shipped hotkeys parse");
        let bound = |text: &str| {
            bindings
                .get(
                    text.parse::<Trigger>()
                        .expect("the test spells its triggers right"),
                )
                .map(|binding| binding.source.clone())
        };
        assert_eq!(
            bound("alt + shift + g").as_deref(),
            Some("toggle-game-mode")
        );
        assert_eq!(bound("alt + h").as_deref(), Some("focus left"));
        assert_eq!(bound("alt + shift + h").as_deref(), Some("move left"));
        assert_eq!(bound("alt + 3").as_deref(), Some("focus-workspace 2"));
        assert_eq!(
            bound("alt + shift + 9").as_deref(),
            Some("move-to-workspace 8")
        );
    }
}
