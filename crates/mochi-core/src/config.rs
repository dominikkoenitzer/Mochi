//! The on disk configuration.
//!
//! Every key that the existing config format also has is spelled exactly the
//! way that format spells it, so an existing config file migrates by renaming
//! it. Unknown keys are ignored rather than rejected, and every field is
//! optional, so a config written for a newer or older version still loads.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::animation::{AnimationStyle, DEFAULT_DURATION_MS, DEFAULT_FPS};
use crate::error::{Error, Result};
use crate::geometry::{Offset, Rect};
use crate::layout::Layout;
use crate::model::{
    FocusFollowsMouseImplementation, HidingBehaviour, Monitor, MoveBehaviour, OperationBehaviour,
    State, WindowContainerBehaviour, Workspace,
};
use crate::rules::{MatchingRule, RuleSets};

/// A 24 bit colour.
///
/// Reads `"#rrggbb"`, `{ "r": 255, "g": 187, "b": 223 }` and a raw Win32
/// `COLORREF` integer. Always writes `"#rrggbb"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Colour {
    /// The red channel.
    pub r: u8,
    /// The green channel.
    pub g: u8,
    /// The blue channel.
    pub b: u8,
}

impl Colour {
    /// A colour from its three channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// A colour from a Win32 `COLORREF`, which stores blue in the high byte.
    #[must_use]
    pub const fn from_colorref(value: u32) -> Self {
        Self {
            r: (value & 0xFF) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: ((value >> 16) & 0xFF) as u8,
        }
    }

    /// The Win32 `COLORREF` for this colour.
    #[must_use]
    pub const fn to_colorref(self) -> u32 {
        (self.b as u32) << 16 | (self.g as u32) << 8 | self.r as u32
    }

    /// The colour as `0xRRGGBB`.
    #[must_use]
    pub const fn to_rgb(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    /// The colour as `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Parses `#rrggbb`, `rrggbb`, `#rgb` or `rgb`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Parse`] for anything else.
    pub fn parse(text: &str) -> Result<Self> {
        let digits = text.trim().trim_start_matches('#');
        let invalid = || Error::Parse {
            kind: "colour",
            value: text.to_string(),
        };
        let channel = |from: usize, to: usize| {
            u8::from_str_radix(digits.get(from..to).unwrap_or_default(), 16).map_err(|_| invalid())
        };
        match digits.len() {
            6 => Ok(Self::new(channel(0, 2)?, channel(2, 4)?, channel(4, 6)?)),
            3 => {
                let double = |c: u8| c * 17;
                Ok(Self::new(
                    double(channel(0, 1)?),
                    double(channel(1, 2)?),
                    double(channel(2, 3)?),
                ))
            }
            _ => Err(invalid()),
        }
    }
}

impl std::fmt::Display for Colour {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl std::str::FromStr for Colour {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

impl Serialize for Colour {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ColourRepr {
    Hex(String),
    Channels { r: u8, g: u8, b: u8 },
    ColorRef(u32),
}

impl<'de> Deserialize<'de> for Colour {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        match ColourRepr::deserialize(deserializer)? {
            ColourRepr::Hex(text) => Colour::parse(&text).map_err(D::Error::custom),
            ColourRepr::Channels { r, g, b } => Ok(Colour::new(r, g, b)),
            ColourRepr::ColorRef(raw) => Ok(Colour::from_colorref(raw)),
        }
    }
}

impl schemars::JsonSchema for Colour {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Colour".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A colour as \"#rrggbb\", as {r, g, b} or as a Win32 COLORREF integer.",
            "anyOf": [
                { "type": "string", "pattern": "^#?([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$" },
                {
                    "type": "object",
                    "properties": {
                        "r": { "type": "integer", "minimum": 0, "maximum": 255 },
                        "g": { "type": "integer", "minimum": 0, "maximum": 255 },
                        "b": { "type": "integer", "minimum": 0, "maximum": 255 }
                    },
                    "required": ["r", "g", "b"]
                },
                { "type": "integer", "minimum": 0 }
            ]
        })
    }
}

/// The shape of the focus border.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum BorderStyle {
    /// Follow whatever Windows is doing.
    #[default]
    System,
    /// Always rounded corners.
    Rounded,
    /// Always square corners.
    Square,
}

/// The border colour for each window state.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
pub struct BorderColours {
    /// A focused window that is alone in its container.
    pub single: Option<Colour>,
    /// A focused window that shares its container with others.
    pub stack: Option<Colour>,
    /// A focused window in monocle mode.
    pub monocle: Option<Colour>,
    /// A focused window that is floating.
    pub floating: Option<Colour>,
    /// Every window that is not focused.
    pub unfocused: Option<Colour>,
}

/// Window movement animation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
pub struct AnimationConfig {
    /// Turns animation on.
    pub enabled: Option<bool>,
    /// How long one animation takes, in milliseconds.
    pub duration: Option<u64>,
    /// The easing curve.
    pub style: Option<AnimationStyle>,
    /// How many frames per second to draw.
    pub fps: Option<u32>,
}

impl AnimationConfig {
    /// `true` when animation is on. Off by default.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    /// The configured duration, or the default.
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.duration.unwrap_or(DEFAULT_DURATION_MS)
    }

    /// The configured frame rate, or the default.
    #[must_use]
    pub fn fps(&self) -> u32 {
        self.fps.unwrap_or(DEFAULT_FPS)
    }

    /// The configured easing curve, or the default.
    #[must_use]
    pub fn style(&self) -> AnimationStyle {
        self.style.unwrap_or_default()
    }
}

/// When the tab bar above a stacked container is drawn.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum StackbarMode {
    /// Always draw it.
    Always,
    /// Only draw it for containers with more than one window.
    OnStack,
    /// Never draw it. The default: nothing is ever drawn above a window
    /// unless the configuration asks for it.
    #[default]
    Never,
}

/// What a stackbar tab says.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum StackbarLabel {
    /// The window title.
    #[default]
    Title,
    /// The name of the owning executable.
    Process,
}

/// The look of one stackbar tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct StackbarTabs {
    /// Tab width in pixels.
    pub width: Option<i32>,
    /// Text colour of the focused tab.
    pub focused_text: Option<Colour>,
    /// Text colour of the other tabs.
    pub unfocused_text: Option<Colour>,
    /// Tab background colour.
    pub background: Option<Colour>,
    /// Font family name.
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size: Option<i32>,
}

/// The tab bar drawn above a stacked container.
///
/// Types only for now; nothing in this crate draws anything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct StackbarConfig {
    /// Bar height in pixels.
    pub height: Option<i32>,
    /// When to draw the bar.
    pub mode: Option<StackbarMode>,
    /// What a tab says.
    pub label: Option<StackbarLabel>,
    /// The look of the tabs.
    pub tabs: Option<StackbarTabs>,
}

/// One workspace, as configured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// The workspace name, as reported by `mochic state`.
    pub name: Option<String>,
    /// The layout to start with.
    pub layout: Option<Layout>,
    /// Padding around the whole workspace.
    pub workspace_padding: Option<i32>,
    /// Padding around each container.
    pub container_padding: Option<i32>,
    /// Layout per container count.
    pub layout_rules: Option<BTreeMap<usize, Layout>>,
    /// Windows matching these rules open here the first time they are seen.
    pub initial_workspace_rules: Option<Vec<MatchingRule>>,
    /// Windows matching these rules always end up here.
    pub workspace_rules: Option<Vec<MatchingRule>>,
    /// Apply the monitor's window based work area offset to this workspace.
    pub apply_window_based_work_area_offset: Option<bool>,
    /// Make every window opened here float.
    pub float_override: Option<bool>,
    /// Whether a new window stacks onto the focused container here.
    pub window_container_behaviour: Option<WindowContainerBehaviour>,
}

impl WorkspaceConfig {
    /// Copies the configured values onto a workspace, leaving the rest alone.
    pub fn apply_to(&self, workspace: &mut Workspace) {
        if self.name.is_some() {
            workspace.name.clone_from(&self.name);
        }
        if let Some(layout) = self.layout {
            workspace.layout = layout;
        }
        if let Some(rules) = &self.layout_rules {
            workspace.layout_rules.clone_from(rules);
        }
        if self.workspace_padding.is_some() {
            workspace.workspace_padding = self.workspace_padding;
        }
        if self.container_padding.is_some() {
            workspace.container_padding = self.container_padding;
        }
        if let Some(value) = self.apply_window_based_work_area_offset {
            workspace.apply_window_based_work_area_offset = value;
        }
        if let Some(value) = self.float_override {
            workspace.float_override = value;
        }
    }
}

/// One monitor, as configured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct MonitorConfig {
    /// The workspaces on this monitor, in order.
    pub workspaces: Vec<WorkspaceConfig>,
    /// A work area offset just for this monitor.
    pub work_area_offset: Option<Offset>,
    /// An offset for workspaces that opt in.
    pub window_based_work_area_offset: Option<Offset>,
    /// The container count up to which the offset above applies.
    pub window_based_work_area_offset_limit: Option<usize>,
}

/// The whole configuration file.
///
/// Everything is optional. [`Config::default`] is what you get from an empty
/// JSON object, and unknown keys are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Config {
    /// The community application rules file to load on top of these rules.
    pub app_specific_configuration_path: Option<PathBuf>,

    /// How windows on an inactive workspace are taken off screen.
    pub window_hiding_behaviour: Option<HidingBehaviour>,
    /// What moving a container past a monitor edge does.
    pub cross_monitor_move_behaviour: Option<MoveBehaviour>,
    /// What a command aimed at an unmanaged window does.
    pub unmanaged_window_operation_behaviour: Option<OperationBehaviour>,
    /// Whether a new window stacks onto the focused container.
    pub window_container_behaviour: Option<WindowContainerBehaviour>,
    /// Whether hovering a window focuses it, and with which implementation.
    pub focus_follows_mouse: Option<FocusFollowsMouseImplementation>,
    /// Warp the cursor onto a window when it takes focus.
    pub mouse_follows_focus: Option<bool>,
    /// Make every new window float.
    pub float_override: Option<bool>,
    /// How many pixels one `resize-axis` step moves a boundary.
    pub resize_delta: Option<i32>,
    /// The width below which a tile is not allowed to shrink.
    ///
    /// In logical pixels, the same units as `default_workspace_padding` and
    /// scaled the same way: the number in the file is the value at 100
    /// percent, and it is multiplied by the scale factor of the monitor the
    /// workspace lands on, so a floor of `300` is 450 physical pixels on a
    /// display at 150 percent. Defaults to
    /// [`MIN_TILE_SIZE`](crate::layout::MIN_TILE_SIZE), which is what every
    /// layout used before this key existed.
    ///
    /// The layouts take one minimum per cut, not one per axis, so the floor
    /// that binds on both axes is the larger of this and
    /// `minimum_window_height`. A value too large for the screen is not an
    /// error: the layouts scale a minimum that cannot fit back down, so the
    /// tiles still cover the area exactly.
    pub minimum_window_width: Option<i32>,
    /// The height below which a tile is not allowed to shrink. The
    /// counterpart of `minimum_window_width`, in the same units.
    pub minimum_window_height: Option<i32>,

    /// Padding around a workspace that has none of its own.
    pub default_workspace_padding: Option<i32>,
    /// Padding around a container in a workspace that has none of its own.
    pub default_container_padding: Option<i32>,
    /// A work area offset applied to every monitor without one of its own.
    pub work_area_offset: Option<Offset>,
    /// An alias the existing config format also accepts for `work_area_offset`.
    pub global_work_area_offset: Option<Offset>,

    /// Draw a border around the focused window.
    pub border: Option<bool>,
    /// Border thickness in pixels.
    pub border_width: Option<i32>,
    /// How far the border sits outside the window rectangle.
    pub border_offset: Option<i32>,
    /// The shape of the border.
    pub border_style: Option<BorderStyle>,
    /// The border colour for each window state.
    pub border_colours: Option<BorderColours>,

    /// Make unfocused windows transparent.
    pub transparency: Option<bool>,
    /// The alpha value applied to unfocused windows, 0 to 255.
    pub transparency_alpha: Option<u8>,

    /// Window movement animation.
    pub animation: Option<AnimationConfig>,
    /// The tab bar above a stacked container.
    pub stackbar: Option<StackbarConfig>,

    /// Windows that are never touched at all.
    pub ignore_rules: Option<Vec<MatchingRule>>,
    /// Windows that are managed even though the heuristics say no.
    pub manage_rules: Option<Vec<MatchingRule>>,
    /// Windows that are managed but never tiled.
    pub floating_applications: Option<Vec<MatchingRule>>,
    /// Applications that keep a hidden window alive in the tray.
    pub tray_and_multi_window_applications: Option<Vec<MatchingRule>>,
    /// Applications that reuse one window and only change its title.
    pub object_name_change_applications: Option<Vec<MatchingRule>>,
    /// Applications whose border sits outside the window rectangle.
    pub border_overflow_applications: Option<Vec<MatchingRule>>,
    /// Layered windows that should be managed anyway.
    ///
    /// The alias is the spelling a migrated file uses. Without it the list is
    /// dropped in silence, which is the one thing this module promises cannot
    /// happen to a key that both formats have.
    #[serde(alias = "layered_applications")]
    pub layered_whitelist: Option<Vec<MatchingRule>>,
    /// Windows that must never be made transparent.
    pub transparency_ignore_rules: Option<Vec<MatchingRule>>,
    /// Applications that need an extra beat before their window is ready.
    pub slow_application_identifiers: Option<Vec<MatchingRule>>,

    /// The monitors, in the order they should be indexed.
    pub monitors: Option<Vec<MonitorConfig>>,
    /// Pins a `monitors` entry to the display that covers exactly this
    /// rectangle of the virtual desktop.
    ///
    /// The key is the index of the entry, the value its display's full
    /// rectangle, the `size` that `mochic state` reports for that monitor.
    /// See [`Config::monitor_assignments`] for how the two preference keys
    /// and the positional fallback interact.
    pub monitor_index_preferences: Option<BTreeMap<usize, Rect>>,
    /// Pins a `monitors` entry to one physical display.
    ///
    /// The key is the index of the entry, the value an identifier of the
    /// display: its [`Monitor::device_id`], its [`Monitor::name`] or its
    /// [`Monitor::device`]. See [`Config::monitor_assignments`], which also
    /// explains why a model name cannot tell two identical panels apart.
    pub display_index_preferences: Option<BTreeMap<usize, String>>,
}

impl Config {
    /// Parses a configuration file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`] when the text is not valid JSON or a value has
    /// the wrong type. Unknown keys are not an error.
    pub fn from_json(json: &str) -> Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Serialises the configuration back to pretty printed JSON.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`] when a value cannot be represented.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// The work area offset, accepting either spelling.
    #[must_use]
    pub fn work_area_offset(&self) -> Option<Offset> {
        self.work_area_offset.or(self.global_work_area_offset)
    }

    /// Every rule list from this file, gathered into one set.
    #[must_use]
    pub fn rule_sets(&self) -> RuleSets {
        RuleSets {
            ignore_rules: self.ignore_rules.clone().unwrap_or_default(),
            // The same rules again, marked as the user's own. A community rule
            // file is merged in later with `extend`, which deliberately does
            // not add to this list, so a manage rule from one cannot overrule
            // what the user wrote in their own file.
            own_ignore_rules: self.ignore_rules.clone().unwrap_or_default(),
            manage_rules: self.manage_rules.clone().unwrap_or_default(),
            floating_applications: self.floating_applications.clone().unwrap_or_default(),
            tray_and_multi_window_applications: self
                .tray_and_multi_window_applications
                .clone()
                .unwrap_or_default(),
            object_name_change_applications: self
                .object_name_change_applications
                .clone()
                .unwrap_or_default(),
            border_overflow_applications: self
                .border_overflow_applications
                .clone()
                .unwrap_or_default(),
            layered_whitelist: self.layered_whitelist.clone().unwrap_or_default(),
            transparency_ignore_rules: self.transparency_ignore_rules.clone().unwrap_or_default(),
            slow_application_identifiers: self
                .slow_application_identifiers
                .clone()
                .unwrap_or_default(),
        }
    }

    /// The rules that decide which workspace a window opens on.
    ///
    /// Returns one entry per rule as `(monitor, workspace, rule, initial_only)`,
    /// where `monitor` is the index of the `monitors` entry the rule was
    /// written under. That is the index of the monitor it configures only when
    /// the entry has no preference pinning it elsewhere; see
    /// [`Config::monitor_assignments`].
    #[must_use]
    pub fn workspace_rules(&self, state: &State) -> Vec<(usize, usize, MatchingRule, bool)> {
        // The monitor a rule routes to is the one its entry configures, which
        // is not the entry's own position once anything is pinned. Sending a
        // window to the index of the entry would open it on whichever screen
        // Windows happened to enumerate there, which is the guess the pinning
        // exists to replace.
        let assignments = self.monitor_assignments(state);
        let mut rules = Vec::new();
        for (entry, monitor) in self.monitors.iter().flatten().enumerate() {
            // An entry whose display is not attached configures nothing, so
            // its rules have nowhere to send a window and are left out rather
            // than pointed at a screen that is merely present.
            let Some(monitor_idx) = assignments.get(entry).copied().flatten() else {
                continue;
            };
            for (workspace_idx, workspace) in monitor.workspaces.iter().enumerate() {
                for rule in workspace.initial_workspace_rules.iter().flatten() {
                    rules.push((monitor_idx, workspace_idx, rule.clone(), true));
                }
                for rule in workspace.workspace_rules.iter().flatten() {
                    rules.push((monitor_idx, workspace_idx, rule.clone(), false));
                }
            }
        }
        rules
    }

    /// Copies the global settings and the rules onto a state, and configures
    /// the workspaces of every monitor that already exists.
    ///
    /// Which monitor an entry configures is decided by
    /// [`Config::monitor_assignments`]: a display or rectangle preference
    /// where the entry has one, its position otherwise. An entry pinned to a
    /// display that is not attached configures nothing. Workspaces are
    /// created on demand.
    pub fn apply_to(&self, state: &mut State) {
        if let Some(value) = self.window_hiding_behaviour {
            state.window_hiding_behaviour = value;
        }
        if let Some(value) = self.cross_monitor_move_behaviour {
            state.cross_monitor_move_behaviour = value;
        }
        if let Some(value) = self.unmanaged_window_operation_behaviour {
            state.unmanaged_window_operation_behaviour = value;
        }
        if let Some(value) = self.window_container_behaviour {
            state.window_container_behaviour = value;
        }
        if self.focus_follows_mouse.is_some() {
            state.focus_follows_mouse = self.focus_follows_mouse;
        }
        if let Some(value) = self.mouse_follows_focus {
            state.mouse_follows_focus = value;
        }
        if let Some(value) = self.float_override {
            state.float_override = value;
        }
        if let Some(value) = self.resize_delta {
            state.resize_delta = value;
        }
        if let Some(value) = self.minimum_window_width {
            state.minimum_window_width = value;
        }
        if let Some(value) = self.minimum_window_height {
            state.minimum_window_height = value;
        }
        if let Some(value) = self.default_workspace_padding {
            state.default_workspace_padding = value;
        }
        if let Some(value) = self.default_container_padding {
            state.default_container_padding = value;
        }
        if let Some(offset) = self.work_area_offset() {
            state.work_area_offset = Some(offset);
        }

        state.rules = self.rule_sets();

        let assignments = self.monitor_assignments(state);
        for (idx, monitor_config) in self.monitors.iter().flatten().enumerate() {
            // `None` is an entry that is held back: pinned to a display that
            // is not attached, or with no monitor left to fall back to.
            let Some(monitor_idx) = assignments.get(idx).copied().flatten() else {
                continue;
            };
            let Some(monitor) = state.monitors_mut().get_mut(monitor_idx) else {
                continue;
            };
            if monitor_config.work_area_offset.is_some() {
                monitor.work_area_offset = monitor_config.work_area_offset;
            }
            if monitor_config.window_based_work_area_offset.is_some() {
                monitor.window_based_work_area_offset =
                    monitor_config.window_based_work_area_offset;
            }
            if let Some(limit) = monitor_config.window_based_work_area_offset_limit {
                monitor.window_based_work_area_offset_limit = limit;
            }
            configure_monitor(monitor_config, monitor);
        }
    }

    /// Configures one monitor from whichever entry assigns to it.
    ///
    /// For a display that has just been attached. [`Config::apply_to`] shapes
    /// every monitor at once, which is right when the configuration is being
    /// loaded and wrong on a display change: it would put every workspace back
    /// to its configured layout and padding, throwing away what a `mochic`
    /// command had set on screens that were never unplugged.
    ///
    /// Returns false when no entry configures this monitor, so the caller can
    /// fall back to something better than a single empty workspace.
    pub fn apply_to_monitor(&self, state: &mut State, monitor_idx: usize) -> bool {
        let assignments = self.monitor_assignments(state);
        let Some(entry) = assignments
            .iter()
            .position(|slot| *slot == Some(monitor_idx))
        else {
            return false;
        };
        let Some(monitor_config) = self.monitors.iter().flatten().nth(entry) else {
            return false;
        };
        let Some(monitor) = state.monitors_mut().get_mut(monitor_idx) else {
            return false;
        };
        if monitor_config.work_area_offset.is_some() {
            monitor.work_area_offset = monitor_config.work_area_offset;
        }
        if monitor_config.window_based_work_area_offset.is_some() {
            monitor.window_based_work_area_offset = monitor_config.window_based_work_area_offset;
        }
        if let Some(limit) = monitor_config.window_based_work_area_offset_limit {
            monitor.window_based_work_area_offset_limit = limit;
        }
        configure_monitor(monitor_config, monitor);
        true
    }

    /// Resolves which monitor of `state` each `monitors` entry configures.
    ///
    /// The result has one slot per entry, holding the index of the monitor
    /// that entry configures, or `None` when the entry is held back and
    /// configures nothing at all this time. No monitor is ever assigned twice.
    ///
    /// An entry is matched by `display_index_preferences` first, then by
    /// `monitor_index_preferences`, then by its own position, because that is
    /// the order from the most stable identifier to the least stable one. A
    /// display id survives a reboot, a cable swap and a `DisplayPort`
    /// renegotiation. A rectangle survives those too, but not moving a display
    /// around the virtual desktop or changing its resolution. The position
    /// survives nothing: Windows hands the monitors out in the order it
    /// enumerated them, and a renegotiation can reverse that order, which is
    /// how a portrait panel's workspaces, layouts and padding end up on a 4K
    /// screen.
    ///
    /// An entry that names a display or a rectangle which is not attached is
    /// held back rather than falling through to the positional match: applying
    /// a portrait panel's nine workspaces to a 4K screen is the failure this
    /// key exists to prevent, and a monitor that is configured by nothing
    /// simply keeps the workspaces it already has until its display is back.
    /// An entry that names both keys is decided by the display id alone, the
    /// stronger of the two statements, and held when that display is missing.
    ///
    /// An entry that names neither key takes the monitor at its own position
    /// when no pinned entry claimed it, and the first monitor still unclaimed
    /// otherwise. In a file with no preferences at all that is exactly the
    /// positional match this always did: entry `n` configures monitor `n`.
    ///
    /// # What a display preference is compared against
    ///
    /// The value is compared against [`Monitor::device_id`], then
    /// [`Monitor::name`], then [`Monitor::device`]. Those are the identifiers
    /// the model already recognises a display by, in the order
    /// [`State::reconcile_monitors`] prefers them, so a pin cannot name a
    /// different display than the one the daemon carried across a
    /// reconfiguration.
    ///
    /// [`Monitor::device_id`] is `EnumDisplayDevicesW`'s `DeviceString`, which
    /// is a model name such as `Odyssey G8` or `Generic PnP Monitor` and never
    /// a serial number. Two panels of the same model therefore carry the same
    /// id, and this key cannot tell them apart: the first unclaimed one wins,
    /// and the other entry falls back to the monitor left over. Pin at least
    /// one of two identical panels by rectangle instead.
    #[must_use]
    pub fn monitor_assignments(&self, state: &State) -> Vec<Option<usize>> {
        let count = self.monitors.as_ref().map_or(0, Vec::len);
        let monitors = state.monitors();
        let mut assigned = vec![None; count];
        let mut claimed = vec![false; monitors.len()];
        let mut pinned = vec![false; count];

        for (idx, slot) in assigned.iter_mut().enumerate() {
            let display = self
                .display_index_preferences
                .as_ref()
                .and_then(|prefs| prefs.get(&idx));
            let rect = self
                .monitor_index_preferences
                .as_ref()
                .and_then(|prefs| prefs.get(&idx));
            if display.is_none() && rect.is_none() {
                continue;
            }
            pinned[idx] = true;
            let found = monitors.iter().enumerate().position(|(at, monitor)| {
                !claimed[at]
                    && match display {
                        Some(id) => names_display(monitor, id),
                        // Only reached when the entry named no display.
                        None => rect.is_some_and(|rect| monitor.size == *rect),
                    }
            });
            if let Some(at) = found {
                claimed[at] = true;
                *slot = Some(at);
            }
        }

        for (idx, slot) in assigned.iter_mut().enumerate() {
            if pinned[idx] {
                continue;
            }
            let at = if claimed.get(idx).is_some_and(|taken| !taken) {
                Some(idx)
            } else {
                claimed.iter().position(|taken| !taken)
            };
            if let Some(at) = at {
                claimed[at] = true;
                *slot = Some(at);
            }
        }

        assigned
    }
}

/// `true` when a `display_index_preferences` value names this display.
///
/// See [`Config::monitor_assignments`] for why these three fields and in this
/// order.
fn names_display(monitor: &Monitor, id: &str) -> bool {
    (!monitor.device_id.is_empty() && monitor.device_id == id)
        || (!monitor.name.is_empty() && monitor.name == id)
        || (!monitor.device.is_empty() && monitor.device == id)
}

/// The JSON schema for [`Config`], pretty printed.
///
/// `mochic schema` writes this out so an editor can complete the config file.
#[must_use]
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(Config);
    serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{\"type\":\"object\"}".to_string())
}

/// Gives a monitor the workspaces its entry describes.
fn configure_monitor(monitor_config: &MonitorConfig, monitor: &mut crate::model::Monitor) {
    monitor.ensure_workspaces(monitor_config.workspaces.len());
    for (workspace_idx, workspace_config) in monitor_config.workspaces.iter().enumerate() {
        if let Some(workspace) = monitor.workspaces_mut().get_mut(workspace_idx) {
            workspace_config.apply_to(workspace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::layout::MIN_TILE_SIZE;

    /// A verbatim copy of the config this crate has to keep working with.
    const REAL_CONFIG: &str = r##"{
  "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
  "app_specific_configuration_path": "$Env:USERPROFILE/applications.json",
  "window_hiding_behaviour": "Cloak",
  "cross_monitor_move_behaviour": "Insert",
  "mouse_follows_focus": false,
  "default_workspace_padding": 14,
  "default_container_padding": 10,
  "border": true,
  "border_width": 6,
  "border_offset": -1,
  "border_style": "Rounded",
  "transparency": true,
  "transparency_alpha": 235,
  "border_colours": {
    "single": "#ffbbdf",
    "stack": "#ffbbdf",
    "monocle": "#ffbbdf",
    "floating": "#ffbbdf",
    "unfocused": "#313244"
  },
  "animation": {
    "enabled": true,
    "duration": 250,
    "style": "EaseOutQuad",
    "fps": 60
  },
  "ignore_rules": [
    { "kind": "Path", "id": "steamapps\\common", "matching_strategy": "Contains" },
    { "kind": "Path", "id": "Epic Games", "matching_strategy": "Contains" },
    { "kind": "Path", "id": "Riot Games", "matching_strategy": "Contains" },
    { "kind": "Exe", "id": "RobloxPlayerBeta.exe", "matching_strategy": "Equals" },
    { "kind": "Path", "id": "XboxGames", "matching_strategy": "Contains" },
    { "kind": "Path", "id": "EA Games", "matching_strategy": "Contains" },
    { "kind": "Exe", "id": "wallpaper32.exe", "matching_strategy": "Equals" },
    { "kind": "Exe", "id": "wallpaper64.exe", "matching_strategy": "Equals" },
    { "kind": "Exe", "id": "zebar.exe", "matching_strategy": "Equals" },
    { "kind": "Exe", "id": "TranslucentTB.exe", "matching_strategy": "Equals" },
    { "kind": "Exe", "id": "PowerToys.PowerAccent.exe", "matching_strategy": "Equals" },
    { "kind": "Title", "id": "[Pp]icture.in.[Pp]icture", "matching_strategy": "Regex" },
    { "kind": "Title", "id": " - Peek", "matching_strategy": "EndsWith" }
  ],
  "monitors": [
    {
      "workspaces": [
        { "name": "1", "layout": "BSP" },
        { "name": "2", "layout": "BSP" },
        { "name": "3", "layout": "BSP" },
        { "name": "4", "layout": "BSP" },
        { "name": "5", "layout": "BSP" },
        { "name": "6", "layout": "BSP" },
        { "name": "7", "layout": "BSP" },
        { "name": "8", "layout": "BSP" },
        { "name": "9", "layout": "BSP" }
      ]
    },
    {
      "workspaces": [
        { "name": "1", "layout": "BSP" },
        { "name": "2", "layout": "BSP" },
        { "name": "3", "layout": "BSP" },
        { "name": "4", "layout": "BSP" },
        { "name": "5", "layout": "BSP" },
        { "name": "6", "layout": "BSP" },
        { "name": "7", "layout": "BSP" },
        { "name": "8", "layout": "BSP" },
        { "name": "9", "layout": "BSP" }
      ]
    }
  ]
}"##;

    /// His file again, with the two keys this whole feature exists for.
    ///
    /// The first entry is pinned to the 4K main screen, the second to the
    /// portrait panel. The portrait entry carries marks that say which entry
    /// it is, a portrait layout and its own padding, because that is exactly
    /// what must never end up on the 4K screen.
    const PINNED_CONFIG: &str = r##"{
  "$schema": "https://raw.githubusercontent.com/dominikkoenitzer/Mochi/main/schema.json",
  "app_specific_configuration_path": "$Env:USERPROFILE/applications.json",
  "window_hiding_behaviour": "Cloak",
  "cross_monitor_move_behaviour": "Insert",
  "mouse_follows_focus": false,
  "default_workspace_padding": 14,
  "default_container_padding": 10,
  "display_index_preferences": {
    "0": "Odyssey G8",
    "1": "AW2521HF"
  },
  "monitors": [
    {
      "workspaces": [
        { "name": "1", "layout": "BSP" },
        { "name": "2", "layout": "BSP" },
        { "name": "3", "layout": "BSP" },
        { "name": "4", "layout": "BSP" },
        { "name": "5", "layout": "BSP" },
        { "name": "6", "layout": "BSP" },
        { "name": "7", "layout": "BSP" },
        { "name": "8", "layout": "BSP" },
        { "name": "9", "layout": "BSP" }
      ]
    },
    {
      "workspaces": [
        { "name": "1", "layout": "Rows", "workspace_padding": 6 },
        { "name": "2", "layout": "Rows", "workspace_padding": 6 },
        { "name": "3", "layout": "Rows", "workspace_padding": 6 },
        { "name": "4", "layout": "Rows", "workspace_padding": 6 },
        { "name": "5", "layout": "Rows", "workspace_padding": 6 },
        { "name": "6", "layout": "Rows", "workspace_padding": 6 },
        { "name": "7", "layout": "Rows", "workspace_padding": 6 },
        { "name": "8", "layout": "Rows", "workspace_padding": 6 },
        { "name": "9", "layout": "Rows", "workspace_padding": 6 }
      ]
    }
  ]
}"##;

    #[test]
    fn the_real_user_config_loads_unchanged() {
        let config = Config::from_json(REAL_CONFIG).expect("the config has to load as is");

        assert_eq!(
            config.app_specific_configuration_path,
            Some(PathBuf::from("$Env:USERPROFILE/applications.json"))
        );
        assert_eq!(config.window_hiding_behaviour, Some(HidingBehaviour::Cloak));
        assert_eq!(
            config.cross_monitor_move_behaviour,
            Some(MoveBehaviour::Insert)
        );
        assert_eq!(config.mouse_follows_focus, Some(false));
        assert_eq!(config.default_workspace_padding, Some(14));
        assert_eq!(config.default_container_padding, Some(10));
        assert_eq!(config.border, Some(true));
        assert_eq!(config.border_width, Some(6));
        assert_eq!(config.border_offset, Some(-1));
        assert_eq!(config.border_style, Some(BorderStyle::Rounded));
        assert_eq!(config.transparency, Some(true));
        assert_eq!(config.transparency_alpha, Some(235));
    }

    #[test]
    fn the_real_config_keeps_the_kawaii_pink_border() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let colours = config.border_colours.expect("border_colours");
        let pink = Colour::new(0xff, 0xbb, 0xdf);
        assert_eq!(colours.single, Some(pink));
        assert_eq!(colours.stack, Some(pink));
        assert_eq!(colours.monocle, Some(pink));
        assert_eq!(colours.floating, Some(pink));
        assert_eq!(colours.unfocused, Some(Colour::new(0x31, 0x32, 0x44)));
    }

    #[test]
    fn the_real_config_keeps_the_animation_settings() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let animation = config.animation.expect("animation");
        assert!(animation.is_enabled());
        assert_eq!(animation.duration_ms(), 250);
        assert_eq!(animation.style(), AnimationStyle::EaseOutQuad);
        assert_eq!(animation.fps(), 60);
    }

    #[test]
    fn the_real_config_keeps_every_ignore_rule() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let sets = config.rule_sets();
        assert_eq!(sets.ignore_rules.len(), 13);
        assert!(sets.validate().is_empty(), "every regex compiles");

        use crate::rules::WindowInfo;
        assert!(sets.should_ignore(&WindowInfo::new(
            "Game",
            "",
            "game.exe",
            r"D:\SteamLibrary\steamapps\common\Game\game.exe"
        )));
        assert!(sets.should_ignore(&WindowInfo::new("", "", "zebar.exe", "")));
        assert!(sets.should_ignore(&WindowInfo::new(
            "Picture-in-Picture",
            "",
            "firefox.exe",
            ""
        )));
        assert!(sets.should_ignore(&WindowInfo::new("shot.png - Peek", "", "Peek.exe", "")));
        assert!(!sets.should_ignore(&WindowInfo::new(
            "README.md",
            "",
            "Code.exe",
            r"C:\Program Files\Code.exe"
        )));
    }

    #[test]
    fn the_real_config_has_two_monitors_with_nine_bsp_workspaces() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let monitors = config.monitors.as_ref().expect("monitors");
        assert_eq!(monitors.len(), 2);
        for monitor in monitors {
            assert_eq!(monitor.workspaces.len(), 9);
            for (idx, workspace) in monitor.workspaces.iter().enumerate() {
                assert_eq!(
                    workspace.name.as_deref(),
                    Some((idx + 1).to_string().as_str())
                );
                assert_eq!(workspace.layout, Some(Layout::Bsp));
            }
        }
    }

    #[test]
    fn the_schema_key_and_any_other_unknown_key_is_ignored() {
        let config = Config::from_json(
            r#"{ "$schema": "x", "something_from_the_future": 42, "border": true }"#,
        )
        .unwrap();
        assert_eq!(config.border, Some(true));
    }

    #[test]
    fn an_empty_object_is_the_default_config() {
        assert_eq!(Config::from_json("{}").unwrap(), Config::default());
        assert!(Config::from_json("nope").is_err());
        assert!(
            Config::from_json(r#"{"border_width":"six"}"#).is_err(),
            "a wrong type is still an error"
        );
    }

    #[test]
    fn colours_parse_from_every_accepted_shape() {
        assert_eq!(
            Colour::parse("#ffbbdf").unwrap(),
            Colour::new(255, 187, 223)
        );
        assert_eq!(Colour::parse("ffbbdf").unwrap(), Colour::new(255, 187, 223));
        assert_eq!(
            Colour::parse("#FFBBDF").unwrap(),
            Colour::new(255, 187, 223)
        );
        assert_eq!(Colour::parse("#f0a").unwrap(), Colour::new(255, 0, 170));
        assert_eq!(
            Colour::parse("  #ffbbdf  ").unwrap(),
            Colour::new(255, 187, 223)
        );
        assert!(Colour::parse("#ff").is_err());
        assert!(Colour::parse("pink").is_err());
        assert!(Colour::parse("#gggggg").is_err());

        assert_eq!(
            serde_json::from_str::<Colour>(r#"{"r":255,"g":187,"b":223}"#).unwrap(),
            Colour::new(255, 187, 223)
        );
        assert_eq!(
            serde_json::from_str::<Colour>("14662655").unwrap(),
            Colour::from_colorref(14_662_655)
        );
        assert!(serde_json::from_str::<Colour>(r#""nope""#).is_err());
    }

    #[test]
    fn colours_serialise_as_hex() {
        let pink = Colour::new(255, 187, 223);
        assert_eq!(pink.to_hex(), "#ffbbdf");
        assert_eq!(pink.to_string(), "#ffbbdf");
        assert_eq!(serde_json::to_string(&pink).unwrap(), "\"#ffbbdf\"");
        assert_eq!(serde_json::from_str::<Colour>("\"#ffbbdf\"").unwrap(), pink);
        assert_eq!("#ffbbdf".parse::<Colour>().unwrap(), pink);
    }

    #[test]
    fn colours_convert_to_win32_and_back() {
        let pink = Colour::new(255, 187, 223);
        assert_eq!(pink.to_rgb(), 0x00ff_bbdf);
        assert_eq!(pink.to_colorref(), 0x00df_bbff);
        assert_eq!(Colour::from_colorref(pink.to_colorref()), pink);
        assert_eq!(Colour::default(), Colour::new(0, 0, 0));
    }

    #[test]
    fn animation_defaults_are_sane() {
        let animation = AnimationConfig::default();
        assert!(!animation.is_enabled());
        assert_eq!(animation.duration_ms(), DEFAULT_DURATION_MS);
        assert_eq!(animation.fps(), DEFAULT_FPS);
        assert_eq!(animation.style(), AnimationStyle::Linear);
    }

    #[test]
    fn layout_rules_and_workspace_rules_deserialise() {
        let config = Config::from_json(
            r#"{
                "monitors": [{
                    "workspaces": [{
                        "name": "code",
                        "layout": "VerticalStack",
                        "workspace_padding": 20,
                        "container_padding": 5,
                        "layout_rules": { "1": "BSP", "3": "Columns" },
                        "initial_workspace_rules": [
                            { "kind": "Exe", "id": "Code.exe", "matching_strategy": "Equals" }
                        ],
                        "workspace_rules": [
                            { "kind": "Exe", "id": "firefox.exe", "matching_strategy": "Equals" }
                        ],
                        "apply_window_based_work_area_offset": true,
                        "float_override": true
                    }],
                    "work_area_offset": { "left": 0, "top": 40, "right": 0, "bottom": 0 },
                    "window_based_work_area_offset_limit": 2
                }]
            }"#,
        )
        .unwrap();

        let monitor = &config.monitors.as_ref().unwrap()[0];
        assert_eq!(monitor.work_area_offset, Some(Offset::new(0, 40, 0, 0)));
        assert_eq!(monitor.window_based_work_area_offset_limit, Some(2));

        let workspace = &monitor.workspaces[0];
        assert_eq!(workspace.layout, Some(Layout::VerticalStack));
        assert_eq!(workspace.workspace_padding, Some(20));
        assert_eq!(workspace.container_padding, Some(5));
        let rules = workspace.layout_rules.as_ref().unwrap();
        assert_eq!(rules.get(&1), Some(&Layout::Bsp));
        assert_eq!(rules.get(&3), Some(&Layout::Columns));

        // One monitor, no preferences, so the entry configures the monitor at
        // its own position exactly as it always did.
        let mut state = State::new();
        state.add_monitor(Monitor::new(
            1,
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 3840, 2160),
        ));
        let all = config.workspace_rules(&state);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, 0);
        assert_eq!(all[0].1, 0);
        assert!(all[0].3, "the first one is an initial rule");
        assert!(!all[1].3);
    }

    #[test]
    fn stackbar_types_deserialise() {
        let config = Config::from_json(
            r##"{
                "stackbar": {
                    "height": 40,
                    "mode": "Always",
                    "label": "Process",
                    "tabs": {
                        "width": 200,
                        "focused_text": "#ffbbdf",
                        "unfocused_text": "#313244",
                        "background": "#1e1e2e",
                        "font_family": "JetBrainsMono Nerd Font",
                        "font_size": 12
                    }
                }
            }"##,
        )
        .unwrap();
        let stackbar = config.stackbar.expect("stackbar");
        assert_eq!(stackbar.height, Some(40));
        assert_eq!(stackbar.mode, Some(StackbarMode::Always));
        assert_eq!(stackbar.label, Some(StackbarLabel::Process));
        let tabs = stackbar.tabs.expect("tabs");
        assert_eq!(tabs.width, Some(200));
        assert_eq!(tabs.focused_text, Some(Colour::new(255, 187, 223)));
        assert_eq!(tabs.font_family.as_deref(), Some("JetBrainsMono Nerd Font"));
        assert_eq!(StackbarMode::default(), StackbarMode::Never);
        assert_eq!(StackbarLabel::default(), StackbarLabel::Title);
    }

    #[test]
    fn index_preferences_deserialise() {
        let config = Config::from_json(
            r#"{
                "monitor_index_preferences": {
                    "0": { "left": 0, "top": 0, "right": 3840, "bottom": 2160 }
                },
                "display_index_preferences": { "1": "AW2521HF-12345" }
            }"#,
        )
        .unwrap();
        assert_eq!(
            config.monitor_index_preferences.as_ref().unwrap().get(&0),
            Some(&Rect::new(0, 0, 3840, 2160))
        );
        assert_eq!(
            config
                .display_index_preferences
                .as_ref()
                .unwrap()
                .get(&1)
                .map(String::as_str),
            Some("AW2521HF-12345")
        );
    }

    #[test]
    fn either_work_area_offset_spelling_works() {
        let a = Config::from_json(r#"{"work_area_offset":{"top":40}}"#).unwrap();
        assert_eq!(a.work_area_offset(), Some(Offset::new(0, 40, 0, 0)));
        let b = Config::from_json(r#"{"global_work_area_offset":{"top":40}}"#).unwrap();
        assert_eq!(b.work_area_offset(), Some(Offset::new(0, 40, 0, 0)));
        assert_eq!(Config::default().work_area_offset(), None);
    }

    #[test]
    fn applying_the_real_config_configures_a_live_state() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let mut state = State::new();
        state.add_monitor(Monitor::new(
            1,
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 3840, 2120),
        ));
        state.add_monitor(Monitor::new(
            2,
            Rect::new(3840, 0, 4920, 1920),
            Rect::new(3840, 0, 4920, 1880),
        ));

        config.apply_to(&mut state);

        assert_eq!(state.window_hiding_behaviour, HidingBehaviour::Cloak);
        assert_eq!(state.cross_monitor_move_behaviour, MoveBehaviour::Insert);
        assert!(!state.mouse_follows_focus);
        assert_eq!(state.default_workspace_padding, 14);
        assert_eq!(state.default_container_padding, 10);
        assert_eq!(state.rules.ignore_rules.len(), 13);

        for monitor_idx in 0..2 {
            let monitor = state.monitors().get(monitor_idx).unwrap();
            assert_eq!(monitor.workspaces().len(), 9);
            for workspace_idx in 0..9 {
                let workspace = monitor.workspaces().get(workspace_idx).unwrap();
                assert_eq!(workspace.layout, Layout::Bsp);
                assert_eq!(
                    workspace.name.as_deref(),
                    Some((workspace_idx + 1).to_string().as_str())
                );
            }
        }
    }

    /// His 4K main screen, as the platform reports it. `gdi` is the GDI device
    /// name, the thing that can swap when `DisplayPort` renegotiates.
    fn odyssey_4k(id: isize, gdi: &str) -> Monitor {
        Monitor::new(id, Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 3840, 2120))
            .with_name(gdi.trim_start_matches(r"\\.\"))
            .with_device(gdi, "Odyssey G8")
    }

    /// His 1080p secondary, rotated into portrait, to the right of the 4K one.
    fn aw2521hf_portrait(id: isize, gdi: &str) -> Monitor {
        Monitor::new(
            id,
            Rect::new(3840, 0, 4920, 1920),
            Rect::new(3840, 0, 4920, 1880),
        )
        .with_name(gdi.trim_start_matches(r"\\.\"))
        .with_device(gdi, "AW2521HF")
    }

    /// The marks that say a monitor got the portrait entry of [`PINNED_CONFIG`].
    fn is_portrait_entry(monitor: &Monitor) -> bool {
        monitor.workspaces().len() == 9
            && monitor
                .workspaces()
                .iter()
                .all(|w| w.layout == Layout::Rows && w.workspace_padding == Some(6))
    }

    /// The marks that say a monitor got the 4K entry of [`PINNED_CONFIG`].
    fn is_main_entry(monitor: &Monitor) -> bool {
        monitor.workspaces().len() == 9
            && monitor
                .workspaces()
                .iter()
                .all(|w| w.layout == Layout::Bsp && w.workspace_padding.is_none())
    }

    /// The name of the first workspace on a monitor, which is how the smaller
    /// tests below say which entry that monitor got.
    fn first_workspace_name(state: &State, monitor: usize) -> Option<String> {
        state
            .monitors()
            .get(monitor)
            .unwrap()
            .workspaces()
            .get(0)
            .unwrap()
            .name
            .clone()
    }

    #[test]
    fn his_displays_in_the_enumeration_order_windows_usually_reports() {
        let config = Config::from_json(PINNED_CONFIG).unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        assert!(is_main_entry(state.monitors().get(0).unwrap()));
        assert!(is_portrait_entry(state.monitors().get(1).unwrap()));
    }

    #[test]
    fn his_displays_in_the_other_order_still_get_the_entries_meant_for_them() {
        // The DisplayPort renegotiation case: Windows enumerates the portrait
        // panel first, so a positional match puts its nine workspaces, its
        // layout and its padding on the 4K screen.
        let config = Config::from_json(PINNED_CONFIG).unwrap();
        let mut state = State::new();
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY1"));
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        let portrait = state.monitors().get(0).unwrap();
        let main = state.monitors().get(1).unwrap();
        assert_eq!(portrait.device_id, "AW2521HF");
        assert_eq!(main.device_id, "Odyssey G8");
        assert!(
            is_portrait_entry(portrait),
            "the portrait panel got the wrong entry"
        );
        assert!(is_main_entry(main), "the 4K screen got the portrait entry");
    }

    #[test]
    fn an_entry_pinned_to_a_display_that_is_not_attached_is_held() {
        // Only the portrait panel is plugged in. The entry for the 4K screen
        // has to wait for the 4K screen rather than land on the panel.
        let config = Config::from_json(PINNED_CONFIG).unwrap();
        let mut state = State::new();
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY1"));

        config.apply_to(&mut state);

        assert!(
            is_portrait_entry(state.monitors().get(0).unwrap()),
            "the only attached display got the entry for the one that is not"
        );
    }

    #[test]
    fn a_rectangle_preference_pins_an_entry_to_the_display_at_that_position() {
        let config = Config::from_json(
            r#"{
                "monitor_index_preferences": {
                    "0": { "left": 0, "top": 0, "right": 3840, "bottom": 2160 },
                    "1": { "left": 3840, "top": 0, "right": 4920, "bottom": 1920 }
                },
                "monitors": [
                    { "workspaces": [{ "name": "main", "layout": "BSP" }] },
                    { "workspaces": [{ "name": "side", "layout": "Rows" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY1"));
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        assert_eq!(first_workspace_name(&state, 0).as_deref(), Some("side"));
        assert_eq!(first_workspace_name(&state, 1).as_deref(), Some("main"));
    }

    #[test]
    fn a_rectangle_that_matches_no_attached_display_holds_the_entry() {
        let config = Config::from_json(
            r#"{
                "monitor_index_preferences": {
                    "0": { "left": 0, "top": 0, "right": 1280, "bottom": 1024 }
                },
                "monitors": [
                    { "workspaces": [{ "name": "elsewhere", "layout": "BSP" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));

        config.apply_to(&mut state);

        assert_eq!(
            first_workspace_name(&state, 0),
            None,
            "an entry pinned to a display that is not attached was applied anyway"
        );
    }

    #[test]
    fn a_display_preference_beats_a_rectangle_preference_for_the_same_entry() {
        let config = Config::from_json(
            r#"{
                "monitor_index_preferences": {
                    "0": { "left": 0, "top": 0, "right": 3840, "bottom": 2160 }
                },
                "display_index_preferences": { "0": "AW2521HF" },
                "monitors": [
                    { "workspaces": [{ "name": "pinned", "layout": "BSP" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        assert_eq!(
            first_workspace_name(&state, 1).as_deref(),
            Some("pinned"),
            "the display id has to win over the rectangle"
        );
        assert_eq!(first_workspace_name(&state, 0), None);
    }

    #[test]
    fn an_entry_that_names_neither_key_still_matches_by_position() {
        // The behaviour every config without a preference relies on.
        let config = Config::from_json(
            r#"{
                "monitors": [
                    { "workspaces": [{ "name": "first", "layout": "BSP" }] },
                    { "workspaces": [{ "name": "second", "layout": "Rows" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY1"));
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        assert_eq!(first_workspace_name(&state, 0).as_deref(), Some("first"));
        assert_eq!(first_workspace_name(&state, 1).as_deref(), Some("second"));
    }

    #[test]
    fn an_unpinned_entry_takes_a_monitor_no_pin_claimed() {
        // One entry pinned, one not. The unpinned one fills what is left
        // rather than fighting the pin for the same display.
        let config = Config::from_json(
            r#"{
                "display_index_preferences": { "0": "AW2521HF" },
                "monitors": [
                    { "workspaces": [{ "name": "pinned", "layout": "BSP" }] },
                    { "workspaces": [{ "name": "rest", "layout": "Rows" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY2"));

        config.apply_to(&mut state);

        assert_eq!(first_workspace_name(&state, 1).as_deref(), Some("pinned"));
        assert_eq!(first_workspace_name(&state, 0).as_deref(), Some("rest"));
    }

    #[test]
    fn a_workspace_rule_routes_to_the_monitor_its_entry_is_pinned_to() {
        // The first entry is pinned to the portrait panel, which Windows
        // enumerates second, so the rule written under it has to open its
        // windows on monitor one.
        let config = Config::from_json(
            r#"{
                "display_index_preferences": { "0": "AW2521HF" },
                "monitors": [
                    {
                        "workspaces": [{
                            "name": "portrait",
                            "workspace_rules": [
                                { "kind": "Exe", "id": "firefox.exe", "matching_strategy": "Equals" }
                            ]
                        }]
                    },
                    { "workspaces": [{ "name": "main" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));
        state.add_monitor(aw2521hf_portrait(2, r"\\.\DISPLAY2"));

        let rules = config.workspace_rules(&state);

        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].0, 1,
            "the rule routed by the index of its entry, not by the display it is pinned to"
        );
    }

    #[test]
    fn a_workspace_rule_under_an_entry_whose_display_is_gone_routes_nowhere() {
        // Only the 4K screen is plugged in. `apply_to` holds the portrait
        // entry back rather than configuring the 4K screen with it, and its
        // rule has to be held back with it.
        let config = Config::from_json(
            r#"{
                "display_index_preferences": { "0": "AW2521HF" },
                "monitors": [
                    {
                        "workspaces": [{
                            "name": "portrait",
                            "workspace_rules": [
                                { "kind": "Exe", "id": "firefox.exe", "matching_strategy": "Equals" }
                            ]
                        }]
                    }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(odyssey_4k(1, r"\\.\DISPLAY1"));

        assert!(
            config.workspace_rules(&state).is_empty(),
            "a rule for a display that is not attached opened windows on the 4K screen"
        );
    }

    #[test]
    fn two_panels_of_the_same_model_share_one_device_id() {
        // `device_id` is the model name from `EnumDisplayDevicesW`, not a
        // serial, so this key cannot tell two identical panels apart: the
        // first unclaimed one wins and the other entry falls back to the
        // monitor left over. Pin one of them by rectangle instead.
        let config = Config::from_json(
            r#"{
                "display_index_preferences": { "0": "Generic PnP Monitor" },
                "monitors": [
                    { "workspaces": [{ "name": "one", "layout": "BSP" }] },
                    { "workspaces": [{ "name": "two", "layout": "Rows" }] }
                ]
            }"#,
        )
        .unwrap();
        let mut state = State::new();
        state.add_monitor(
            Monitor::new(1, Rect::new(0, 0, 1920, 1080), Rect::new(0, 0, 1920, 1040))
                .with_device(r"\\.\DISPLAY1", "Generic PnP Monitor"),
        );
        state.add_monitor(
            Monitor::new(
                2,
                Rect::new(1920, 0, 3840, 1080),
                Rect::new(1920, 0, 3840, 1040),
            )
            .with_device(r"\\.\DISPLAY2", "Generic PnP Monitor"),
        );

        config.apply_to(&mut state);

        assert_eq!(first_workspace_name(&state, 0).as_deref(), Some("one"));
        assert_eq!(first_workspace_name(&state, 1).as_deref(), Some("two"));
    }

    /// A state with one 4K screen and one workspace holding `len` windows,
    /// configured by `json`.
    fn tiled(json: &str, layout: Layout, len: isize) -> (State, Rect) {
        use crate::model::Window;

        let config = Config::from_json(json).expect("the config parses");
        let mut state = State::new();
        state.add_monitor(Monitor::new(
            1,
            Rect::new(0, 0, 3840, 2160),
            Rect::new(0, 0, 3840, 2160),
        ));
        state
            .monitors_mut()
            .get_mut(0)
            .expect("the monitor is there")
            .ensure_workspaces(1);
        config.apply_to(&mut state);

        let work_area = state.work_area_for(0, 0).expect("a work area");
        let workspace_padding = state.default_workspace_padding;
        let container_padding = state.default_container_padding;
        let workspace = state.workspace_mut(0, 0).expect("the workspace");
        workspace.layout = layout;
        for id in 1..=len {
            workspace.add_window(Window::new(id));
        }
        workspace.update_layout(work_area, workspace_padding, container_padding);
        (state, work_area)
    }

    /// The tiles of the only workspace of `state`.
    fn tiles(state: &State) -> Vec<Rect> {
        state.workspace(0, 0).unwrap().latest_layout().to_vec()
    }

    #[test]
    fn a_configured_minimum_is_the_floor_every_tile_keeps() {
        // Twelve containers on a 4K screen: BSP squeezes the last of them down
        // to 64x67, right onto the old hardcoded floor, which is what makes a
        // configurable one worth having and this test worth reading.
        const NO_PADDING: &str =
            r#"{ "default_workspace_padding": 0, "default_container_padding": 0"#;
        let (loose, _) = tiled(&format!("{NO_PADDING} }}"), Layout::Bsp, 12);
        assert!(
            tiles(&loose)
                .iter()
                .any(|rect| rect.width() < 300 || rect.height() < 300),
            "without the key nothing is squeezed, so the floor proves nothing"
        );

        let (state, _) = tiled(
            &format!(
                "{NO_PADDING}, \"minimum_window_width\": 300, \"minimum_window_height\": 250 }}"
            ),
            Layout::Bsp,
            12,
        );
        assert_eq!(state.minimum_window_width, 300);
        assert_eq!(state.minimum_window_height, 250);
        assert_eq!(
            state.workspace(0, 0).unwrap().minimum_tile_size(),
            crate::layout::MinSize {
                width: 300,
                height: 250
            },
            "each axis keeps its own floor"
        );

        for (idx, rect) in tiles(&state).iter().enumerate() {
            assert!(
                rect.width() >= 300 && rect.height() >= 250,
                "tile {idx} is {}x{}, below the configured floor",
                rect.width(),
                rect.height()
            );
        }

        // The two are genuinely independent. The proof is a tile that is
        // shorter than the *width* floor: if one number bound both axes, the
        // larger would have raised every height to 300 as well.
        assert!(
            tiles(&state).iter().any(|rect| rect.height() < 300),
            "no tile is shorter than the width floor, so the two axes are not              independent and a height of 250 was silently raised to 300"
        );
    }

    #[test]
    fn a_config_without_the_minimum_tiles_exactly_as_it_always_did() {
        // The layout the crate computed before the key existed, from the entry
        // point that still hardcodes MIN_TILE_SIZE.
        for layout in Layout::ALL {
            for len in 1..=12_isize {
                let (state, work_area) = tiled(r#"{}"#, layout, len);
                let workspace = state.workspace(0, 0).unwrap();
                assert_eq!(
                    workspace.minimum_tile_size(),
                    crate::layout::MinSize::square(MIN_TILE_SIZE)
                );
                let expected = layout.calculate(
                    work_area.padded_clamped(state.default_workspace_padding),
                    len as usize,
                    state.default_container_padding,
                    workspace.layout_flip,
                    &workspace.resize_dimensions,
                );
                assert_eq!(tiles(&state), expected, "{layout} with {len} containers");
            }
        }
    }

    #[test]
    fn a_minimum_larger_than_the_screen_still_tiles_the_area_exactly() {
        // Absurd values are the user's to type. The layouts scale a minimum
        // that cannot fit back down, so the worst this can do is give every
        // tile an equal share, never a panic and never an inverted rectangle.
        for minimum in [100_000, i32::MAX] {
            let json = format!(
                r#"{{ "default_workspace_padding": 0, "default_container_padding": 0,
                      "minimum_window_width": {minimum}, "minimum_window_height": 1 }}"#
            );
            for layout in Layout::ALL {
                for len in 1..=12_isize {
                    let (state, _) = tiled(&json, layout, len);
                    let rects = tiles(&state);
                    assert_eq!(rects.len(), len as usize);
                    let mut covered = 0_i64;
                    for rect in &rects {
                        assert!(
                            rect.right >= rect.left && rect.bottom >= rect.top,
                            "{layout} with {len} containers inverted a tile: {rect:?}"
                        );
                        assert!(
                            rect.left >= 0
                                && rect.top >= 0
                                && rect.right <= 3840
                                && rect.bottom <= 2160,
                            "{layout} with {len} containers left the screen: {rect:?}"
                        );
                        covered += i64::from(rect.width()) * i64::from(rect.height());
                    }
                    assert_eq!(
                        covered,
                        3840 * 2160,
                        "{layout} with {len} containers stopped tiling its area exactly"
                    );
                }
            }
        }
    }

    #[test]
    fn applying_a_config_leaves_unset_keys_alone() {
        let mut state = State::new();
        state.default_container_padding = 42;
        Config::default().apply_to(&mut state);
        assert_eq!(state.default_container_padding, 42);
        assert_eq!(state.window_hiding_behaviour, HidingBehaviour::Cloak);
    }

    #[test]
    fn a_config_with_more_monitors_than_the_machine_has_is_fine() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let mut state = State::new();
        config.apply_to(&mut state);
        assert!(state.monitors().is_empty());
    }

    #[test]
    fn a_migrated_file_keeps_the_list_it_spells_differently() {
        // Both formats have this list and spell it differently. Unknown keys
        // are ignored by design, so without the alias a migrated file loses it
        // without a word, which is exactly what this module says cannot happen
        // to a key both formats carry.
        let config = Config::from_json(
            r#"{"layered_applications":[{"kind":"Exe","id":"a.exe","matching_strategy":"Equals"}]}"#,
        )
        .expect("a migrated file parses");
        assert_eq!(
            config.layered_whitelist.as_ref().map(Vec::len),
            Some(1),
            "the list was dropped in silence"
        );
    }

    #[test]
    fn the_config_round_trips_through_json() {
        let config = Config::from_json(REAL_CONFIG).unwrap();
        let json = config.to_json().unwrap();
        let back = Config::from_json(&json).unwrap();
        assert_eq!(back, config);
    }

    #[test]
    fn the_json_schema_describes_the_config() {
        let schema = json_schema();
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let properties = parsed
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("the schema has properties");
        for key in [
            "app_specific_configuration_path",
            "window_hiding_behaviour",
            "cross_monitor_move_behaviour",
            "mouse_follows_focus",
            "default_workspace_padding",
            "default_container_padding",
            "border",
            "border_width",
            "border_offset",
            "border_style",
            "border_colours",
            "transparency",
            "transparency_alpha",
            "animation",
            "ignore_rules",
            "manage_rules",
            "floating_applications",
            "monitors",
            "focus_follows_mouse",
            "unmanaged_window_operation_behaviour",
            "stackbar",
            "work_area_offset",
            "monitor_index_preferences",
            "display_index_preferences",
        ] {
            assert!(properties.contains_key(key), "the schema is missing {key}");
        }
    }

    #[test]
    fn the_json_schema_is_structurally_a_schema_for_every_field_of_the_config() {
        let parsed: serde_json::Value = serde_json::from_str(&json_schema()).unwrap();
        assert!(
            parsed
                .get("$schema")
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "an editor needs the dialect to validate against"
        );
        assert_eq!(
            parsed.get("type").and_then(serde_json::Value::as_str),
            Some("object")
        );
        let properties = parsed
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("the schema has properties");

        // The field list comes from the serialised default, so a field added
        // to Config is checked here without anybody remembering to add it.
        let default = serde_json::to_value(Config::default()).unwrap();
        let fields = default
            .as_object()
            .expect("a config serialises to an object");
        assert!(fields.len() > 10, "the default lost its fields");
        for key in fields.keys() {
            assert!(
                properties.contains_key(key),
                "the schema is missing the field {key}"
            );
        }

        // And every key the real config file uses has to be in there too.
        let real: serde_json::Value = serde_json::from_str(REAL_CONFIG).unwrap();
        for key in real
            .as_object()
            .expect("the fixture is an object")
            .keys()
            .filter(|key| key.as_str() != "$schema")
        {
            assert!(
                properties.contains_key(key),
                "the schema is missing the configured key {key}"
            );
        }
    }
}
