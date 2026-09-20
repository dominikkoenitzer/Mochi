//! The `mochic` command-line grammar.
//!
//! It lives here rather than in the `mochic` binary because the hotkey daemon
//! binds keys to the very words a user would type at a prompt. With one grammar
//! in the workspace a binding cannot drift away from the command line it was
//! copied from, and `mochic --help` stays the single source of truth for both.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::{
    AnimationStyle, Axis, BooleanState, BorderStyle, Command, CycleDirection, Direction, Layout,
    MatchingStrategy, QueryTarget, RuleIdentifier, Sizing, WindowKind,
};

/// The whole `mochic` command line: a subcommand and its arguments.
#[derive(Parser)]
#[command(
    name = "mochic",
    version,
    about = "Control the Mochi window manager",
    arg_required_else_help = true
)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Cmd,
}

/// One `mochic` subcommand, with the arguments it takes.
///
/// Mostly one per [`Command`]; the handful that `mochic` carries out itself are
/// the ones [`Cmd::to_command`] answers `None` for.
#[derive(Subcommand)]
pub enum Cmd {
    // --- lifecycle -------------------------------------------------------
    /// Start the daemon, detached from this console
    Start {
        /// Pass a configuration file to the daemon
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Pass a hotkey file to the daemon
        #[arg(long, value_name = "PATH")]
        hotkeys: Option<PathBuf>,
        /// Start the daemon with no keys bound
        #[arg(long, conflicts_with = "hotkeys")]
        no_hotkeys: bool,
        /// Start the daemon in dry-run mode, which moves nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop the daemon and restore all windows
    Stop,
    /// Write a starting mochi.json and hotkey file, if there is none
    Quickstart,
    /// Print the JSON schema of the configuration file
    Schema,
    /// Pause and resume window management
    TogglePause,
    /// Hand the whole keyboard to a game, and take it back again
    ToggleGameMode,
    /// Re-read the configuration file and the hotkey file
    ReloadConfiguration,
    /// Recompute and re-apply every layout
    Retile,

    // --- introspection ---------------------------------------------------
    /// Print the daemon state as pretty JSON
    State,
    /// Print a single value from the daemon
    Query {
        /// What to ask for
        #[arg(value_enum)]
        target: QueryTarget,
    },
    /// Print the hotkey bindings the daemon holds
    Hotkeys {
        /// Print the raw bindings document instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Stream notifications to stdout until interrupted
    Subscribe {
        /// Pipe name to create, without the \\.\pipe\ prefix
        name: String,
    },
    /// Register an existing named pipe for notifications
    SubscribePipe {
        /// Pipe name, without the \\.\pipe\ prefix
        name: String,
    },
    /// Stop sending notifications to a named pipe
    UnsubscribePipe {
        /// Pipe name, without the \\.\pipe\ prefix
        name: String,
    },

    // --- focus and movement ----------------------------------------------
    /// Move focus in a direction
    Focus {
        /// left, right, up or down
        #[arg(value_enum)]
        direction: Direction,
    },
    /// Move the focused window in a direction
    Move {
        /// left, right, up or down
        #[arg(value_enum)]
        direction: Direction,
    },
    /// Grow or shrink the focused window along an axis
    ResizeAxis {
        /// horizontal or vertical
        #[arg(value_enum)]
        axis: Axis,
        /// increase or decrease
        #[arg(value_enum)]
        sizing: Sizing,
    },
    /// Swap the focused window with the first window of its workspace
    Promote,

    // --- window state ----------------------------------------------------
    /// Toggle the focused window between tiled and floating
    ToggleFloat,
    /// Toggle the focused window between tiled and maximized
    ToggleMaximize,
    /// Toggle monocle mode for the focused window
    ToggleMonocle,
    /// Minimize the focused window
    Minimize,
    /// Ask the focused window to close
    Close,
    /// Manage the focused window even if a rule would skip it
    Manage,
    /// Stop managing the focused window
    Unmanage,

    // --- stacks -----------------------------------------------------------
    /// Stack the focused window onto its neighbour
    Stack {
        /// left, right, up or down
        #[arg(value_enum)]
        direction: Direction,
    },
    /// Pull the focused window out of its stack
    Unstack,
    /// Step through the windows of the focused stack
    CycleStack {
        /// next or previous
        #[arg(value_enum)]
        direction: CycleDirection,
    },

    // --- layouts ----------------------------------------------------------
    /// Step through the layout ring
    CycleLayout {
        /// next or previous
        #[arg(value_enum)]
        direction: CycleDirection,
    },
    /// Set the layout of the focused workspace
    ChangeLayout {
        /// Layout name
        #[arg(value_enum)]
        layout: Layout,
    },
    /// Mirror the layout along an axis
    FlipLayout {
        /// horizontal or vertical
        #[arg(value_enum)]
        axis: Axis,
    },

    // --- workspaces -------------------------------------------------------
    /// Focus a workspace by its zero-based index
    FocusWorkspace {
        /// Zero-based workspace index
        index: usize,
    },
    /// Move the focused window to a workspace and follow it
    MoveToWorkspace {
        /// Zero-based workspace index
        index: usize,
    },
    /// Step through the workspaces of the focused monitor
    CycleWorkspace {
        /// next or previous
        #[arg(value_enum)]
        direction: CycleDirection,
    },
    /// Go back to the previously focused workspace
    FocusLastWorkspace,
    /// Set the outer padding of one workspace
    WorkspacePadding {
        /// Zero-based monitor index
        monitor: usize,
        /// Zero-based workspace index
        workspace: usize,
        /// Padding in logical pixels
        size: i32,
    },
    /// Set the padding between containers of one workspace
    ContainerPadding {
        /// Zero-based monitor index
        monitor: usize,
        /// Zero-based workspace index
        workspace: usize,
        /// Padding in logical pixels
        size: i32,
    },

    // --- monitors ---------------------------------------------------------
    /// Focus a monitor by its zero-based index
    FocusMonitor {
        /// Zero-based monitor index
        index: usize,
    },
    /// Move the focused window to a monitor and follow it
    MoveToMonitor {
        /// Zero-based monitor index
        index: usize,
    },
    /// Step through the monitor ring
    CycleMonitor {
        /// next or previous
        #[arg(value_enum)]
        direction: CycleDirection,
    },

    // --- input behaviour --------------------------------------------------
    /// Focus whatever window the mouse moves over
    FocusFollowsMouse {
        /// enable or disable
        #[arg(value_enum)]
        state: BooleanState,
    },
    /// Warp the mouse to a newly focused window
    MouseFollowsFocus {
        /// enable or disable
        #[arg(value_enum)]
        state: BooleanState,
    },
    /// Turn the hotkey daemon on or off
    SetHotkeys {
        /// enable or disable
        #[arg(value_enum)]
        state: BooleanState,
    },

    // --- visuals ----------------------------------------------------------
    /// Toggle transparency for unfocused windows
    ToggleTransparency,
    /// Turn the focus border on or off
    Border {
        /// enable or disable
        #[arg(value_enum)]
        state: BooleanState,
    },
    /// Set the border thickness in logical pixels
    BorderWidth {
        /// Thickness in logical pixels
        width: i32,
    },
    /// Set how far the border sits outside the window frame
    BorderOffset {
        /// Offset in logical pixels, negative pulls it inwards
        offset: i32,
    },
    /// Set the border colour for one kind of window
    BorderColour {
        /// Red channel, 0 to 255
        r: u8,
        /// Green channel, 0 to 255
        g: u8,
        /// Blue channel, 0 to 255
        b: u8,
        /// Which windows the colour applies to
        #[arg(long, value_enum, default_value_t = WindowKind::Single)]
        window_kind: WindowKind,
    },
    /// Set the border shape
    BorderStyle {
        /// system, rounded or square
        #[arg(value_enum)]
        style: BorderStyle,
    },
    /// Turn move and resize animations on or off
    Animation {
        /// enable or disable
        #[arg(value_enum)]
        state: BooleanState,
    },
    /// Set the animation duration in milliseconds
    AnimationDuration {
        /// Duration in milliseconds
        duration: u64,
    },
    /// Set the animation easing curve
    AnimationStyle {
        /// Easing curve
        #[arg(value_enum)]
        style: AnimationStyle,
    },
    /// Set the animation frame rate
    AnimationFps {
        /// Frames per second
        fps: u32,
    },

    // --- rules ------------------------------------------------------------
    /// Float windows that match a rule
    FloatRule {
        /// exe, class, title or path
        #[arg(value_enum)]
        identifier: RuleIdentifier,
        /// Value to match against
        id: String,
        /// How to compare
        #[arg(long, value_enum, default_value_t = MatchingStrategy::Equals)]
        matching_strategy: MatchingStrategy,
    },
    /// Ignore windows that match a rule
    IgnoreRule {
        /// exe, class, title or path
        #[arg(value_enum)]
        identifier: RuleIdentifier,
        /// Value to match against
        id: String,
        /// How to compare
        #[arg(long, value_enum, default_value_t = MatchingStrategy::Equals)]
        matching_strategy: MatchingStrategy,
    },
}

impl Cmd {
    /// The protocol command this subcommand sends, or `None` for the ones
    /// `mochic` handles by itself (`start`, `quickstart`, `schema`,
    /// `subscribe`).
    pub fn to_command(&self) -> Option<Command> {
        let command = match self {
            // These three never reach a daemon: two of them run before one
            // exists, the third only prints a file.
            Cmd::Start { .. } | Cmd::Quickstart | Cmd::Schema => return None,

            // `subscribe` is not one command either. It creates its own pipe
            // first and only then registers it, so answering `subscribe-pipe`
            // here would let a key register a pipe that nobody ever created.
            Cmd::Subscribe { .. } => return None,

            Cmd::Stop => Command::Stop,
            Cmd::TogglePause => Command::TogglePause,
            Cmd::ToggleGameMode => Command::ToggleGameMode,
            Cmd::ReloadConfiguration => Command::ReloadConfiguration,
            Cmd::Retile => Command::Retile,
            Cmd::State => Command::State,
            Cmd::Query { target } => Command::Query { target: *target },
            // Table or raw JSON is a decision `mochic` makes on its own, the
            // daemon answers the same document either way.
            Cmd::Hotkeys { json: _ } => Command::Hotkeys,
            Cmd::SubscribePipe { name } => Command::SubscribePipe { name: name.clone() },
            Cmd::UnsubscribePipe { name } => Command::UnsubscribePipe { name: name.clone() },
            Cmd::Focus { direction } => Command::Focus {
                direction: *direction,
            },
            Cmd::Move { direction } => Command::Move {
                direction: *direction,
            },
            Cmd::ResizeAxis { axis, sizing } => Command::ResizeAxis {
                axis: *axis,
                sizing: *sizing,
            },
            Cmd::Promote => Command::Promote,
            Cmd::ToggleFloat => Command::ToggleFloat,
            Cmd::ToggleMaximize => Command::ToggleMaximize,
            Cmd::ToggleMonocle => Command::ToggleMonocle,
            Cmd::Minimize => Command::Minimize,
            Cmd::Close => Command::Close,
            Cmd::Manage => Command::Manage,
            Cmd::Unmanage => Command::Unmanage,
            Cmd::Stack { direction } => Command::Stack {
                direction: *direction,
            },
            Cmd::Unstack => Command::Unstack,
            Cmd::CycleStack { direction } => Command::CycleStack {
                direction: *direction,
            },
            Cmd::CycleLayout { direction } => Command::CycleLayout {
                direction: *direction,
            },
            Cmd::ChangeLayout { layout } => Command::ChangeLayout { layout: *layout },
            Cmd::FlipLayout { axis } => Command::FlipLayout { axis: *axis },
            Cmd::FocusWorkspace { index } => Command::FocusWorkspace { index: *index },
            Cmd::MoveToWorkspace { index } => Command::MoveToWorkspace { index: *index },
            Cmd::CycleWorkspace { direction } => Command::CycleWorkspace {
                direction: *direction,
            },
            Cmd::FocusLastWorkspace => Command::FocusLastWorkspace,
            Cmd::WorkspacePadding {
                monitor,
                workspace,
                size,
            } => Command::WorkspacePadding {
                monitor: *monitor,
                workspace: *workspace,
                size: *size,
            },
            Cmd::ContainerPadding {
                monitor,
                workspace,
                size,
            } => Command::ContainerPadding {
                monitor: *monitor,
                workspace: *workspace,
                size: *size,
            },
            Cmd::FocusMonitor { index } => Command::FocusMonitor { index: *index },
            Cmd::MoveToMonitor { index } => Command::MoveToMonitor { index: *index },
            Cmd::CycleMonitor { direction } => Command::CycleMonitor {
                direction: *direction,
            },
            Cmd::FocusFollowsMouse { state } => Command::FocusFollowsMouse { state: *state },
            Cmd::MouseFollowsFocus { state } => Command::MouseFollowsFocus { state: *state },
            Cmd::SetHotkeys { state } => Command::SetHotkeys { state: *state },
            Cmd::ToggleTransparency => Command::ToggleTransparency,
            Cmd::Border { state } => Command::Border { state: *state },
            Cmd::BorderWidth { width } => Command::BorderWidth { width: *width },
            Cmd::BorderOffset { offset } => Command::BorderOffset { offset: *offset },
            Cmd::BorderColour {
                r,
                g,
                b,
                window_kind,
            } => Command::BorderColour {
                kind: *window_kind,
                r: *r,
                g: *g,
                b: *b,
            },
            Cmd::BorderStyle { style } => Command::BorderStyle { style: *style },
            Cmd::Animation { state } => Command::Animation { state: *state },
            Cmd::AnimationDuration { duration } => Command::AnimationDuration {
                duration: *duration,
            },
            Cmd::AnimationStyle { style } => Command::AnimationStyle { style: *style },
            Cmd::AnimationFps { fps } => Command::AnimationFps { fps: *fps },
            Cmd::FloatRule {
                identifier,
                id,
                matching_strategy,
            } => Command::FloatRule {
                identifier: *identifier,
                id: id.clone(),
                matching_strategy: *matching_strategy,
            },
            Cmd::IgnoreRule {
                identifier,
                id,
                matching_strategy,
            } => Command::IgnoreRule {
                identifier: *identifier,
                id: id.clone(),
                matching_strategy: *matching_strategy,
            },
        };
        Some(command)
    }
}

/// Parses one hotkey binding's words (`["focus", "left"]`, no leading `mochic`)
/// into the command it sends. The error is one line, meant for a human reading
/// a config error, and names what was wrong.
pub fn command_from_args(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Err("a binding needs a command, `focus left` for example".to_owned());
    };

    // Going through clap instead of matching on the words by hand is the whole
    // point of this function: one grammar, so a binding and the command line can
    // never disagree about what `resize-axis horizontal increase` means.
    let argv = std::iter::once(String::from("mochic")).chain(args.iter().cloned());
    let cli =
        Cli::try_parse_from(argv).map_err(|e| format!("`{}`: {}", args.join(" "), one_line(&e)))?;

    cli.command
        .to_command()
        .ok_or_else(|| format!("`{first}` cannot be bound to a key, `mochic` runs it itself"))
}

/// Squashes a clap error, a paragraph with a usage block and a help hint, into
/// the single line a configuration error can afford.
fn one_line(error: &clap::Error) -> String {
    use clap::error::ErrorKind;

    // A help or version flag is not a failure to clap, so it arrives here as an
    // error carrying the whole help text. Nobody wants that in a config error.
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        return "expected a command, not a help or version flag".to_owned();
    }

    let rendered = error.render().to_string();
    let first = rendered
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("could not be parsed");
    first.strip_prefix("error: ").unwrap_or(first).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_definition_is_valid() {
        Cli::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Command {
        let mut full = vec!["mochic"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full)
            .unwrap()
            .command
            .to_command()
            .unwrap()
    }

    fn binding(line: &str) -> Result<Command, String> {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        command_from_args(&args)
    }

    #[test]
    fn every_binding_in_the_users_whkdrc_parses() {
        // The left column is exactly what every binding in `.config/whkdrc`
        // runs, with the old client name swapped for `mochic`.
        let cases: &[(&[&str], Command)] = &[
            (
                &["focus", "left"],
                Command::Focus {
                    direction: Direction::Left,
                },
            ),
            (
                &["focus", "down"],
                Command::Focus {
                    direction: Direction::Down,
                },
            ),
            (
                &["move", "up"],
                Command::Move {
                    direction: Direction::Up,
                },
            ),
            (
                &["move", "right"],
                Command::Move {
                    direction: Direction::Right,
                },
            ),
            (
                &["resize-axis", "horizontal", "increase"],
                Command::ResizeAxis {
                    axis: Axis::Horizontal,
                    sizing: Sizing::Increase,
                },
            ),
            (
                &["resize-axis", "vertical", "decrease"],
                Command::ResizeAxis {
                    axis: Axis::Vertical,
                    sizing: Sizing::Decrease,
                },
            ),
            (&["toggle-float"], Command::ToggleFloat),
            (&["toggle-maximize"], Command::ToggleMaximize),
            (&["toggle-monocle"], Command::ToggleMonocle),
            (&["minimize"], Command::Minimize),
            (&["close"], Command::Close),
            (
                &["cycle-layout", "next"],
                Command::CycleLayout {
                    direction: CycleDirection::Next,
                },
            ),
            (
                &["flip-layout", "horizontal"],
                Command::FlipLayout {
                    axis: Axis::Horizontal,
                },
            ),
            (
                &["flip-layout", "vertical"],
                Command::FlipLayout {
                    axis: Axis::Vertical,
                },
            ),
            (
                &["cycle-workspace", "previous"],
                Command::CycleWorkspace {
                    direction: CycleDirection::Previous,
                },
            ),
            (&["focus-last-workspace"], Command::FocusLastWorkspace),
            (
                &["focus-workspace", "0"],
                Command::FocusWorkspace { index: 0 },
            ),
            (
                &["focus-workspace", "8"],
                Command::FocusWorkspace { index: 8 },
            ),
            (
                &["move-to-workspace", "5"],
                Command::MoveToWorkspace { index: 5 },
            ),
            (&["toggle-pause"], Command::TogglePause),
            (&["reload-configuration"], Command::ReloadConfiguration),
            (&["retile"], Command::Retile),
            (&["stop"], Command::Stop),
        ];

        for (args, expected) in cases {
            assert_eq!(&parse(args), expected, "for {args:?}");
        }
    }

    #[test]
    fn the_extra_commands_parse_too() {
        assert_eq!(parse(&["state"]), Command::State);
        assert_eq!(parse(&["promote"]), Command::Promote);
        assert_eq!(parse(&["unstack"]), Command::Unstack);
        assert_eq!(
            parse(&["query", "monitor-count"]),
            Command::Query {
                target: QueryTarget::MonitorCount
            }
        );
        assert_eq!(
            parse(&["change-layout", "ultrawide-vertical-stack"]),
            Command::ChangeLayout {
                layout: Layout::UltrawideVerticalStack
            }
        );
        assert_eq!(
            parse(&["workspace-padding", "1", "2", "14"]),
            Command::WorkspacePadding {
                monitor: 1,
                workspace: 2,
                size: 14
            }
        );
        assert_eq!(
            parse(&["border-colour", "255", "187", "223"]),
            Command::BorderColour {
                kind: WindowKind::Single,
                r: 255,
                g: 187,
                b: 223
            }
        );
        assert_eq!(
            parse(&[
                "border-colour",
                "49",
                "50",
                "68",
                "--window-kind",
                "unfocused"
            ]),
            Command::BorderColour {
                kind: WindowKind::Unfocused,
                r: 49,
                g: 50,
                b: 68
            }
        );
        assert_eq!(
            parse(&["float-rule", "exe", "wt.exe"]),
            Command::FloatRule {
                identifier: RuleIdentifier::Exe,
                id: "wt.exe".into(),
                matching_strategy: MatchingStrategy::Equals
            }
        );
        assert_eq!(
            parse(&[
                "ignore-rule",
                "path",
                "steamapps\\common",
                "--matching-strategy",
                "contains"
            ]),
            Command::IgnoreRule {
                identifier: RuleIdentifier::Path,
                id: "steamapps\\common".into(),
                matching_strategy: MatchingStrategy::Contains
            }
        );
        assert_eq!(
            parse(&["subscribe-pipe", "bar"]),
            Command::SubscribePipe { name: "bar".into() }
        );
        assert_eq!(
            parse(&["unsubscribe-pipe", "bar"]),
            Command::UnsubscribePipe { name: "bar".into() }
        );
        assert_eq!(
            parse(&["focus-follows-mouse", "enable"]),
            Command::FocusFollowsMouse {
                state: BooleanState::Enable
            }
        );
        assert_eq!(
            parse(&["animation-style", "ease-out-quad"]),
            Command::AnimationStyle {
                style: AnimationStyle::EaseOutQuad
            }
        );
    }

    #[test]
    fn the_hotkey_subcommands_parse() {
        assert_eq!(parse(&["hotkeys"]), Command::Hotkeys);
        // --json changes what `mochic` prints, not what it sends.
        assert_eq!(parse(&["hotkeys", "--json"]), Command::Hotkeys);
        assert_eq!(
            parse(&["set-hotkeys", "disable"]),
            Command::SetHotkeys {
                state: BooleanState::Disable
            }
        );
        assert_eq!(parse(&["toggle-game-mode"]), Command::ToggleGameMode);
    }

    #[test]
    fn bad_arguments_are_rejected() {
        assert!(Cli::try_parse_from(["mochic", "focus", "sideways"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "focus"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "focus-workspace", "-1"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "not-a-command"]).is_err());
        assert!(Cli::try_parse_from(["mochic"]).is_err());
    }

    #[test]
    fn a_binding_sends_what_the_command_line_would_send() {
        assert_eq!(
            binding("focus left").unwrap(),
            Command::Focus {
                direction: Direction::Left
            }
        );
        assert_eq!(
            binding("resize-axis horizontal increase").unwrap(),
            Command::ResizeAxis {
                axis: Axis::Horizontal,
                sizing: Sizing::Increase
            }
        );
        assert_eq!(
            binding("move-to-workspace 5").unwrap(),
            Command::MoveToWorkspace { index: 5 }
        );
        assert_eq!(
            binding("toggle-game-mode").unwrap(),
            Command::ToggleGameMode
        );
        assert_eq!(binding("hotkeys").unwrap(), Command::Hotkeys);
    }

    #[test]
    fn stopping_mochi_can_be_bound_to_a_key() {
        assert_eq!(binding("stop").unwrap(), Command::Stop);
    }

    #[test]
    fn the_client_side_subcommands_cannot_be_bound_to_a_key() {
        // `subscribe` is in the list because `mochic` creates the pipe before
        // it registers it; bound to a key it would register a pipe that does
        // not exist, which is not what typing the same words does.
        for line in ["start", "quickstart", "schema", "subscribe bar"] {
            let message = binding(line).unwrap_err();
            assert!(
                message.contains("cannot be bound to a key"),
                "for {line}: {message}"
            );
        }
    }

    #[test]
    fn a_broken_binding_fails_on_one_line() {
        for line in ["focus sideways", "focus", "not-a-command", "--help"] {
            let message = binding(line).unwrap_err();
            assert_eq!(message.lines().count(), 1, "for {line}: {message}");
            assert!(!message.contains("Usage:"), "for {line}: {message}");
        }
        assert!(binding("focus sideways").unwrap_err().contains("sideways"));
        assert!(
            command_from_args(&[])
                .unwrap_err()
                .contains("needs a command")
        );
    }
}
