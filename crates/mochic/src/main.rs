//! `mochic`: sends commands to the running Mochi daemon.
//!
//! One subcommand per protocol command, with the same argument shapes the
//! hotkey file already uses. Anything that fails prints one line to stderr and
//! exits non-zero, so `whkd` bindings fail loudly instead of silently.

mod process;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use mochi_client::{
    AnimationStyle, Axis, BooleanState, BorderStyle, Command, CycleDirection, Direction, Error,
    Layout, MatchingStrategy, QueryTarget, Response, RuleIdentifier, Sizing, WindowKind,
};

#[derive(Parser)]
#[command(
    name = "mochic",
    version,
    about = "Control the Mochi window manager",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    // --- lifecycle -------------------------------------------------------
    /// Start the daemon, detached from this console
    Start {
        /// Also start whkd
        #[arg(long)]
        whkd: bool,
        /// Pass a configuration file to the daemon
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Start the daemon in dry-run mode, which moves nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop the daemon and restore all windows
    Stop {
        /// Also stop whkd
        #[arg(long)]
        whkd: bool,
    },
    /// Write a default mochi.json to %USERPROFILE% if there is none
    Quickstart,
    /// Print the JSON schema of the configuration file
    Schema,
    /// Pause and resume window management
    TogglePause,
    /// Re-read the configuration file
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

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mochic: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    // Three subcommands do not travel over the pipe.
    match cli.command {
        Cmd::Start {
            whkd,
            ref config,
            dry_run,
        } => return start(whkd, config.as_deref(), dry_run),
        Cmd::Quickstart => return quickstart(),
        Cmd::Schema => {
            println!("{}", mochi_core::config::json_schema());
            return Ok(());
        }
        Cmd::Subscribe { ref name } => return subscribe(name),
        _ => {}
    }

    let stop_whkd = matches!(cli.command, Cmd::Stop { whkd: true });
    let command = to_command(cli.command);
    let response = send(&command)?;

    match response {
        Response::Ok => {}
        Response::State { state } => println!("{}", serde_json::to_string_pretty(&state)?),
        Response::Query { answer } => println!("{}", scalar(&answer)),
        Response::Error { message } => bail!(message),
    }

    if stop_whkd {
        process::stop_whkd()?;
    }
    Ok(())
}

/// Sends a command, turning "no daemon" into a message a human can act on.
fn send(command: &Command) -> Result<Response> {
    match mochi_client::send(command) {
        Ok(response) => Ok(response),
        Err(Error::NotRunning) => bail!(
            "mochi is not running. Start it with `mochic start`, or `mochic start --whkd` to bring the hotkeys up too."
        ),
        Err(Error::Daemon(message)) => bail!(message),
        Err(e) => Err(e).context("could not talk to mochi"),
    }
}

/// Prints a JSON scalar without its quotes, so shells can use the value directly.
fn scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn start(whkd: bool, config: Option<&std::path::Path>, dry_run: bool) -> Result<()> {
    if mochi_client::is_running() {
        println!("mochi is already running");
    } else {
        let mut args = Vec::new();
        if dry_run {
            args.push("--dry-run".to_owned());
        }
        if let Some(path) = config {
            args.push("--config".to_owned());
            args.push(path.display().to_string());
        }
        process::start_daemon(&args)?;
    }
    if whkd {
        process::start_whkd()?;
    }
    Ok(())
}

fn quickstart() -> Result<()> {
    let profile = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .context("USERPROFILE is not set")?;
    let path = profile.join("mochi.json");
    if path.exists() {
        println!("{} already exists, leaving it alone", path.display());
        return Ok(());
    }
    std::fs::write(&path, DEFAULT_CONFIG)
        .with_context(|| format!("could not write {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// The stub configuration `quickstart` writes.
///
/// Kept minimal on purpose: `mochi-core` owns the schema and fills this in.
const DEFAULT_CONFIG: &str = r#"{
  "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
  "window_hiding_behaviour": "Cloak",
  "default_workspace_padding": 10,
  "default_container_padding": 10,
  "monitors": []
}
"#;

fn subscribe(name: &str) -> Result<()> {
    let subscription = match mochi_client::subscribe(name) {
        Ok(s) => s,
        Err(Error::NotRunning) => {
            bail!("mochi is not running. Start it with `mochic start`.")
        }
        Err(e) => return Err(e).context("could not subscribe"),
    };
    eprintln!(
        "subscribed on {}{name}, press Ctrl-C to stop",
        mochi_client::PIPE_PREFIX
    );
    for notification in subscription {
        match notification {
            Ok(n) => println!("{}", serde_json::to_string(&n)?),
            Err(e) => bail!("the subscriber pipe failed: {e}"),
        }
    }
    Ok(())
}

/// Maps the CLI shape onto the wire shape.
fn to_command(cmd: Cmd) -> Command {
    match cmd {
        // Handled before this point.
        Cmd::Start { whkd, .. } => Command::Start { whkd },
        Cmd::Quickstart | Cmd::Schema => Command::Quickstart,
        Cmd::Subscribe { name } => Command::SubscribePipe { name },

        Cmd::Stop { whkd } => Command::Stop { whkd },
        Cmd::TogglePause => Command::TogglePause,
        Cmd::ReloadConfiguration => Command::ReloadConfiguration,
        Cmd::Retile => Command::Retile,
        Cmd::State => Command::State,
        Cmd::Query { target } => Command::Query { target },
        Cmd::SubscribePipe { name } => Command::SubscribePipe { name },
        Cmd::UnsubscribePipe { name } => Command::UnsubscribePipe { name },
        Cmd::Focus { direction } => Command::Focus { direction },
        Cmd::Move { direction } => Command::Move { direction },
        Cmd::ResizeAxis { axis, sizing } => Command::ResizeAxis { axis, sizing },
        Cmd::Promote => Command::Promote,
        Cmd::ToggleFloat => Command::ToggleFloat,
        Cmd::ToggleMaximize => Command::ToggleMaximize,
        Cmd::ToggleMonocle => Command::ToggleMonocle,
        Cmd::Minimize => Command::Minimize,
        Cmd::Close => Command::Close,
        Cmd::Manage => Command::Manage,
        Cmd::Unmanage => Command::Unmanage,
        Cmd::Stack { direction } => Command::Stack { direction },
        Cmd::Unstack => Command::Unstack,
        Cmd::CycleStack { direction } => Command::CycleStack { direction },
        Cmd::CycleLayout { direction } => Command::CycleLayout { direction },
        Cmd::ChangeLayout { layout } => Command::ChangeLayout { layout },
        Cmd::FlipLayout { axis } => Command::FlipLayout { axis },
        Cmd::FocusWorkspace { index } => Command::FocusWorkspace { index },
        Cmd::MoveToWorkspace { index } => Command::MoveToWorkspace { index },
        Cmd::CycleWorkspace { direction } => Command::CycleWorkspace { direction },
        Cmd::FocusLastWorkspace => Command::FocusLastWorkspace,
        Cmd::WorkspacePadding {
            monitor,
            workspace,
            size,
        } => Command::WorkspacePadding {
            monitor,
            workspace,
            size,
        },
        Cmd::ContainerPadding {
            monitor,
            workspace,
            size,
        } => Command::ContainerPadding {
            monitor,
            workspace,
            size,
        },
        Cmd::FocusMonitor { index } => Command::FocusMonitor { index },
        Cmd::MoveToMonitor { index } => Command::MoveToMonitor { index },
        Cmd::CycleMonitor { direction } => Command::CycleMonitor { direction },
        Cmd::FocusFollowsMouse { state } => Command::FocusFollowsMouse { state },
        Cmd::MouseFollowsFocus { state } => Command::MouseFollowsFocus { state },
        Cmd::ToggleTransparency => Command::ToggleTransparency,
        Cmd::Border { state } => Command::Border { state },
        Cmd::BorderWidth { width } => Command::BorderWidth { width },
        Cmd::BorderOffset { offset } => Command::BorderOffset { offset },
        Cmd::BorderColour {
            r,
            g,
            b,
            window_kind,
        } => Command::BorderColour {
            kind: window_kind,
            r,
            g,
            b,
        },
        Cmd::BorderStyle { style } => Command::BorderStyle { style },
        Cmd::Animation { state } => Command::Animation { state },
        Cmd::AnimationDuration { duration } => Command::AnimationDuration { duration },
        Cmd::AnimationStyle { style } => Command::AnimationStyle { style },
        Cmd::AnimationFps { fps } => Command::AnimationFps { fps },
        Cmd::FloatRule {
            identifier,
            id,
            matching_strategy,
        } => Command::FloatRule {
            identifier,
            id,
            matching_strategy,
        },
        Cmd::IgnoreRule {
            identifier,
            id,
            matching_strategy,
        } => Command::IgnoreRule {
            identifier,
            id,
            matching_strategy,
        },
    }
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
        to_command(Cli::try_parse_from(full).unwrap().command)
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
            (&["stop", "--whkd"], Command::Stop { whkd: true }),
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
    fn bad_arguments_are_rejected() {
        assert!(Cli::try_parse_from(["mochic", "focus", "sideways"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "focus"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "focus-workspace", "-1"]).is_err());
        assert!(Cli::try_parse_from(["mochic", "not-a-command"]).is_err());
        assert!(Cli::try_parse_from(["mochic"]).is_err());
    }

    #[test]
    fn query_scalars_print_without_json_quotes() {
        assert_eq!(scalar(&serde_json::json!("0.1.0")), "0.1.0");
        assert_eq!(scalar(&serde_json::json!(2)), "2");
        assert_eq!(scalar(&serde_json::json!(true)), "true");
    }

    /// The schema committed at the repository root is what the CLI prints.
    ///
    /// Configuration files point their `$schema` at that file, so a change to
    /// the config types that is not written back would leave every editor
    /// validating against a stale schema.
    #[test]
    fn the_committed_schema_is_current() {
        let committed = include_str!("../../../schema.json");
        assert_eq!(
            committed.trim(),
            mochi_core::config::json_schema().trim(),
            "schema.json is out of date, regenerate it with `mochic schema > schema.json`"
        );
    }

    #[test]
    fn the_quickstart_stub_is_valid_json() {
        let value: serde_json::Value = serde_json::from_str(DEFAULT_CONFIG).unwrap();
        assert!(value.is_object());
    }
}
