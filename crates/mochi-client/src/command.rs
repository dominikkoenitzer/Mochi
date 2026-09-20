//! Commands a client can send to the daemon.
//!
//! The wire form is an internally tagged JSON object: the variant name goes into
//! the `cmd` field in kebab-case, the payload fields sit next to it.
//!
//! ```text
//! {"cmd":"focus","direction":"left"}
//! {"cmd":"resize-axis","axis":"horizontal","sizing":"increase"}
//! ```

use serde::{Deserialize, Serialize};

/// Implements [`std::str::FromStr`] and [`std::fmt::Display`] for a fieldless
/// enum using the same kebab-case spelling serde uses, and derives
/// `clap::ValueEnum` when the `clap` feature is on.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
        #[serde(rename_all = "kebab-case")]
        pub enum $name {
            $(
                #[doc = concat!("Spelled `", $text, "`.")]
                $(#[$vmeta])*
                $variant
            ),*
        }

        impl $name {
            /// The kebab-case spelling used on the wire and on the command line.
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $text ),* }
            }

            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[ $( Self::$variant ),* ];
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = ParseEnumError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $( if s.eq_ignore_ascii_case($text) { return Ok(Self::$variant); } )*
                Err(ParseEnumError { kind: stringify!($name), value: s.to_owned() })
            }
        }
    };
}

/// Returned when a string does not name a variant of one of the wire enums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseEnumError {
    /// Name of the enum that was being parsed.
    pub kind: &'static str,
    /// The input that did not match.
    pub value: String,
}

impl std::fmt::Display for ParseEnumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` is not a valid {}", self.value, self.kind)
    }
}

impl std::error::Error for ParseEnumError {}

wire_enum! {
    /// A spatial direction, used for focus, movement and stacking.
    pub enum Direction {
        Left => "left",
        Right => "right",
        Up => "up",
        Down => "down",
    }
}

wire_enum! {
    /// The axis a resize or a layout flip applies to.
    pub enum Axis {
        Horizontal => "horizontal",
        Vertical => "vertical",
    }
}

wire_enum! {
    /// Whether a resize grows or shrinks along its axis.
    pub enum Sizing {
        Increase => "increase",
        Decrease => "decrease",
    }
}

wire_enum! {
    /// Which way to step through a ring: layouts, workspaces, monitors, stacks.
    pub enum CycleDirection {
        Next => "next",
        Previous => "previous",
    }
}

wire_enum! {
    /// An on/off switch as spelled on the command line.
    pub enum BooleanState {
        Enable => "enable",
        Disable => "disable",
    }
}

impl BooleanState {
    /// `true` for [`BooleanState::Enable`].
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enable)
    }
}

wire_enum! {
    /// The layouts Mochi knows about. The arrangement itself lives in `mochi-core`.
    pub enum Layout {
        Bsp => "bsp",
        Columns => "columns",
        Rows => "rows",
        VerticalStack => "vertical-stack",
        HorizontalStack => "horizontal-stack",
        UltrawideVerticalStack => "ultrawide-vertical-stack",
        Grid => "grid",
    }
}

wire_enum! {
    /// Border shape.
    pub enum BorderStyle {
        System => "system",
        Rounded => "rounded",
        Square => "square",
    }
}

wire_enum! {
    /// Easing curve for move and resize animations.
    pub enum AnimationStyle {
        Linear => "linear",
        EaseInSine => "ease-in-sine",
        EaseOutSine => "ease-out-sine",
        EaseInOutSine => "ease-in-out-sine",
        EaseInQuad => "ease-in-quad",
        EaseOutQuad => "ease-out-quad",
        EaseInOutQuad => "ease-in-out-quad",
        EaseInCubic => "ease-in-cubic",
        EaseOutCubic => "ease-out-cubic",
        EaseInOutCubic => "ease-in-out-cubic",
    }
}

wire_enum! {
    /// Which kind of window a border colour applies to.
    pub enum WindowKind {
        Single => "single",
        Stack => "stack",
        Monocle => "monocle",
        Floating => "floating",
        Unfocused => "unfocused",
    }
}

wire_enum! {
    /// The window property a rule matches on.
    pub enum RuleIdentifier {
        Exe => "exe",
        Class => "class",
        Title => "title",
        Path => "path",
    }
}

wire_enum! {
    /// How the `id` of a rule is compared against the window property.
    pub enum MatchingStrategy {
        Equals => "equals",
        Contains => "contains",
        StartsWith => "starts-with",
        EndsWith => "ends-with",
        Regex => "regex",
    }
}

wire_enum! {
    /// A single scalar the daemon can answer without dumping the whole state.
    pub enum QueryTarget {
        FocusedMonitorIndex => "focused-monitor-index",
        FocusedWorkspaceIndex => "focused-workspace-index",
        FocusedContainerIndex => "focused-container-index",
        FocusedWindowIndex => "focused-window-index",
        FocusedWorkspaceName => "focused-workspace-name",
        MonitorCount => "monitor-count",
        WindowCount => "window-count",
        Paused => "paused",
        DryRun => "dry-run",
        ConfigPath => "config-path",
        Version => "version",
    }
}

/// A command sent from a client to the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Command {
    // --- lifecycle -------------------------------------------------------
    /// Start the daemon. Only meaningful to `mochic`; a running daemon answers Ok.
    Start,
    /// Exit cleanly, restoring every managed window first.
    Stop,
    /// Write a default mochi.json to `%USERPROFILE%` if there is none.
    Quickstart,
    /// Stop and resume management without exiting.
    TogglePause,
    /// Hand the whole keyboard to a game: pause tiling and suspend every
    /// hotkey except the one bound to this command, so it can be undone.
    ToggleGameMode,
    /// Re-read the configuration file.
    ReloadConfiguration,
    /// Recompute and re-apply every layout.
    Retile,

    // --- introspection ---------------------------------------------------
    /// Dump the full daemon state as JSON.
    State,
    /// Ask for a single scalar.
    Query {
        /// What to answer.
        target: QueryTarget,
    },
    /// Ask for the bindings the hotkey daemon currently holds.
    Hotkeys,

    // --- focus and movement ----------------------------------------------
    /// Move focus in a direction.
    Focus {
        /// Direction to move focus in.
        direction: Direction,
    },
    /// Move the focused window in a direction.
    Move {
        /// Direction to move the window in.
        direction: Direction,
    },
    /// Grow or shrink the focused window along an axis.
    ResizeAxis {
        /// Axis to resize along.
        axis: Axis,
        /// Grow or shrink.
        sizing: Sizing,
    },
    /// Swap the focused window with the first window of its workspace.
    Promote,

    // --- window state ----------------------------------------------------
    /// Toggle the focused window between tiled and floating.
    ToggleFloat,
    /// Toggle the focused window between tiled and maximized.
    ToggleMaximize,
    /// Toggle monocle mode for the focused window.
    ToggleMonocle,
    /// Minimize the focused window.
    Minimize,
    /// Ask the focused window to close.
    Close,
    /// Start managing the focused window even if a rule would skip it.
    Manage,
    /// Stop managing the focused window and leave it where it is.
    Unmanage,

    // --- stacks -----------------------------------------------------------
    /// Stack the focused window onto its neighbour in a direction.
    Stack {
        /// Which neighbour to stack onto.
        direction: Direction,
    },
    /// Pull the focused window back out of its stack.
    Unstack,
    /// Step through the windows of the focused stack.
    CycleStack {
        /// Which way to step.
        direction: CycleDirection,
    },

    // --- layouts ----------------------------------------------------------
    /// Step through the layout ring of the focused workspace.
    CycleLayout {
        /// Which way to step.
        direction: CycleDirection,
    },
    /// Set the layout of the focused workspace.
    ChangeLayout {
        /// Layout to switch to.
        layout: Layout,
    },
    /// Mirror the layout of the focused workspace along an axis.
    FlipLayout {
        /// Axis to flip along.
        axis: Axis,
    },

    // --- workspaces -------------------------------------------------------
    /// Focus a workspace by its zero-based index on the focused monitor.
    FocusWorkspace {
        /// Zero-based workspace index.
        index: usize,
    },
    /// Move the focused window to a workspace and follow it.
    MoveToWorkspace {
        /// Zero-based workspace index.
        index: usize,
    },
    /// Step through the workspaces of the focused monitor.
    CycleWorkspace {
        /// Which way to step.
        direction: CycleDirection,
    },
    /// Return to the workspace that was focused before the current one.
    FocusLastWorkspace,
    /// Set the outer padding of one workspace.
    WorkspacePadding {
        /// Zero-based monitor index.
        monitor: usize,
        /// Zero-based workspace index.
        workspace: usize,
        /// Padding in logical pixels.
        size: i32,
    },
    /// Set the padding between the containers of one workspace.
    ContainerPadding {
        /// Zero-based monitor index.
        monitor: usize,
        /// Zero-based workspace index.
        workspace: usize,
        /// Padding in logical pixels.
        size: i32,
    },

    // --- monitors ---------------------------------------------------------
    /// Focus a monitor by its zero-based index.
    FocusMonitor {
        /// Zero-based monitor index.
        index: usize,
    },
    /// Move the focused window to a monitor and follow it.
    MoveToMonitor {
        /// Zero-based monitor index.
        index: usize,
    },
    /// Step through the monitor ring.
    CycleMonitor {
        /// Which way to step.
        direction: CycleDirection,
    },

    // --- input behaviour --------------------------------------------------
    /// Focus whatever window the mouse moves over.
    FocusFollowsMouse {
        /// Turn it on or off.
        state: BooleanState,
    },
    /// Warp the mouse to the centre of a newly focused window.
    MouseFollowsFocus {
        /// Turn it on or off.
        state: BooleanState,
    },
    /// Turn the hotkey daemon on or off without stopping Mochi.
    SetHotkeys {
        /// Turn it on or off.
        state: BooleanState,
    },

    // --- visuals ----------------------------------------------------------
    /// Toggle transparency for unfocused windows.
    ToggleTransparency,
    /// Turn the focus border on or off.
    Border {
        /// Turn it on or off.
        state: BooleanState,
    },
    /// Set the border thickness in logical pixels.
    BorderWidth {
        /// Thickness in logical pixels.
        width: i32,
    },
    /// Set how far the border sits outside the window frame.
    BorderOffset {
        /// Offset in logical pixels, negative pulls the border inwards.
        offset: i32,
    },
    /// Set the border colour for one kind of window.
    BorderColour {
        /// Which windows the colour applies to.
        #[serde(default = "default_window_kind")]
        kind: WindowKind,
        /// Red channel.
        r: u8,
        /// Green channel.
        g: u8,
        /// Blue channel.
        b: u8,
    },
    /// Set the border shape.
    BorderStyle {
        /// Border shape.
        style: BorderStyle,
    },
    /// Turn move and resize animations on or off.
    Animation {
        /// Turn it on or off.
        state: BooleanState,
    },
    /// Set the animation duration in milliseconds.
    AnimationDuration {
        /// Duration in milliseconds.
        duration: u64,
    },
    /// Set the animation easing curve.
    AnimationStyle {
        /// Easing curve.
        style: AnimationStyle,
    },
    /// Set the animation frame rate.
    AnimationFps {
        /// Frames per second.
        fps: u32,
    },

    // --- rules ------------------------------------------------------------
    /// Add a rule that floats matching windows instead of tiling them.
    FloatRule {
        /// Window property to match on.
        identifier: RuleIdentifier,
        /// Value to match against.
        id: String,
        /// How to compare.
        #[serde(default = "default_matching_strategy")]
        matching_strategy: MatchingStrategy,
    },
    /// Add a rule that makes Mochi ignore matching windows entirely.
    IgnoreRule {
        /// Window property to match on.
        identifier: RuleIdentifier,
        /// Value to match against.
        id: String,
        /// How to compare.
        #[serde(default = "default_matching_strategy")]
        matching_strategy: MatchingStrategy,
    },

    // --- subscriptions ----------------------------------------------------
    /// Send every notification to a named pipe created by the subscriber.
    ///
    /// The subscriber creates the pipe, the daemon connects to it as a client.
    SubscribePipe {
        /// Pipe name without the `\\.\pipe\` prefix.
        name: String,
    },
    /// Stop sending notifications to a subscriber pipe.
    UnsubscribePipe {
        /// Pipe name without the `\\.\pipe\` prefix.
        name: String,
    },
}

fn default_window_kind() -> WindowKind {
    WindowKind::Single
}

fn default_matching_strategy() -> MatchingStrategy {
    MatchingStrategy::Equals
}

impl Command {
    /// The `cmd` tag this command serialises to, handy for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::Quickstart => "quickstart",
            Self::TogglePause => "toggle-pause",
            Self::ToggleGameMode => "toggle-game-mode",
            Self::ReloadConfiguration => "reload-configuration",
            Self::Retile => "retile",
            Self::State => "state",
            Self::Query { .. } => "query",
            Self::Hotkeys => "hotkeys",
            Self::Focus { .. } => "focus",
            Self::Move { .. } => "move",
            Self::ResizeAxis { .. } => "resize-axis",
            Self::Promote => "promote",
            Self::ToggleFloat => "toggle-float",
            Self::ToggleMaximize => "toggle-maximize",
            Self::ToggleMonocle => "toggle-monocle",
            Self::Minimize => "minimize",
            Self::Close => "close",
            Self::Manage => "manage",
            Self::Unmanage => "unmanage",
            Self::Stack { .. } => "stack",
            Self::Unstack => "unstack",
            Self::CycleStack { .. } => "cycle-stack",
            Self::CycleLayout { .. } => "cycle-layout",
            Self::ChangeLayout { .. } => "change-layout",
            Self::FlipLayout { .. } => "flip-layout",
            Self::FocusWorkspace { .. } => "focus-workspace",
            Self::MoveToWorkspace { .. } => "move-to-workspace",
            Self::CycleWorkspace { .. } => "cycle-workspace",
            Self::FocusLastWorkspace => "focus-last-workspace",
            Self::WorkspacePadding { .. } => "workspace-padding",
            Self::ContainerPadding { .. } => "container-padding",
            Self::FocusMonitor { .. } => "focus-monitor",
            Self::MoveToMonitor { .. } => "move-to-monitor",
            Self::CycleMonitor { .. } => "cycle-monitor",
            Self::FocusFollowsMouse { .. } => "focus-follows-mouse",
            Self::MouseFollowsFocus { .. } => "mouse-follows-focus",
            Self::SetHotkeys { .. } => "set-hotkeys",
            Self::ToggleTransparency => "toggle-transparency",
            Self::Border { .. } => "border",
            Self::BorderWidth { .. } => "border-width",
            Self::BorderOffset { .. } => "border-offset",
            Self::BorderColour { .. } => "border-colour",
            Self::BorderStyle { .. } => "border-style",
            Self::Animation { .. } => "animation",
            Self::AnimationDuration { .. } => "animation-duration",
            Self::AnimationStyle { .. } => "animation-style",
            Self::AnimationFps { .. } => "animation-fps",
            Self::FloatRule { .. } => "float-rule",
            Self::IgnoreRule { .. } => "ignore-rule",
            Self::SubscribePipe { .. } => "subscribe-pipe",
            Self::UnsubscribePipe { .. } => "unsubscribe-pipe",
        }
    }

    /// True for the commands `mochic` handles on its own without a daemon.
    pub fn is_client_side(&self) -> bool {
        matches!(self, Self::Start { .. } | Self::Quickstart)
    }
}
