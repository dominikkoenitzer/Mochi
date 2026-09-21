//! What the daemon knows on top of the tiling model.
//!
//! The monitor / workspace / container / window tree lives in
//! [`mochi_core::State`], which [`crate::wm::WindowManager`] owns. Everything
//! here is the rest: the facts about this process, the visual settings that no
//! command can put into the model yet, and the JSON document `mochic state`
//! prints.
//!
//! [`snapshot`] is the only place that decides what that document looks like.
//! Its shape is the daemon's public API, so it is built by hand from the model
//! rather than derived from `mochi-core`'s internals, which are free to change.

use std::path::{Path, PathBuf};

use mochi_client::{AnimationStyle, BooleanState, BorderStyle};
use mochi_core::config::BorderColours;
use mochi_core::model::{Container, Monitor, Window, Workspace};
use mochi_core::{Rect, State as CoreState};
use serde::Serialize;
use serde_json::{Value, json};

use crate::platform::Hwnd;

/// Runtime settings a client can flip without touching the configuration file.
///
/// These are the ones `mochi-core` has no use for: it decides where windows go,
/// not what they look like. The tiling settings all live in
/// [`mochi_core::State`] instead, so there is exactly one copy of each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Settings {
    /// Fade unfocused windows.
    pub transparency: bool,
    /// The alpha unfocused windows are drawn with, 0 to 255.
    pub transparency_alpha: u8,
    /// Draw a border around the focused window.
    pub border: bool,
    /// Border thickness in logical pixels.
    pub border_width: i32,
    /// How far the border sits outside the frame.
    pub border_offset: i32,
    /// Border corner shape.
    pub border_style: BorderStyle,
    /// The border colour of each kind of window. A colour the file and the
    /// commands never set stays `None` and the renderer draws its own.
    pub border_colours: BorderColours,
    /// Play move and resize animations.
    pub animation: bool,
    /// Animation length in milliseconds.
    pub animation_duration: u64,
    /// Animation frame rate.
    pub animation_fps: u32,
    /// Animation easing curve.
    pub animation_style: AnimationStyle,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            transparency: false,
            transparency_alpha: 200,
            border: false,
            border_width: 6,
            border_offset: -1,
            border_style: BorderStyle::System,
            border_colours: BorderColours::default(),
            animation: false,
            animation_duration: 250,
            animation_fps: 60,
            animation_style: AnimationStyle::Linear,
        }
    }
}

impl Settings {
    /// Applies an `enable`/`disable` argument to a field.
    pub fn set(field: &mut bool, state: BooleanState) {
        *field = state.is_enabled();
    }

    /// Copies the visual keys of a configuration file over these settings.
    ///
    /// Only the keys the file actually carries are touched, so a reload never
    /// resets a setting a command changed and the file says nothing about.
    pub fn apply(&mut self, config: &mochi_core::config::Config) {
        if let Some(value) = config.border {
            self.border = value;
        }
        if let Some(value) = config.border_width {
            self.border_width = value;
        }
        if let Some(value) = config.border_offset {
            self.border_offset = value;
        }
        if let Some(style) = config.border_style {
            self.border_style = match style {
                mochi_core::config::BorderStyle::System => BorderStyle::System,
                mochi_core::config::BorderStyle::Rounded => BorderStyle::Rounded,
                mochi_core::config::BorderStyle::Square => BorderStyle::Square,
            };
        }
        if let Some(colours) = config.border_colours {
            self.border_colours = colours;
        }
        if let Some(value) = config.transparency {
            self.transparency = value;
        }
        if let Some(value) = config.transparency_alpha {
            self.transparency_alpha = value;
        }
        if let Some(animation) = &config.animation {
            if let Some(value) = animation.enabled {
                self.animation = value;
            }
            if let Some(value) = animation.duration {
                self.animation_duration = value;
            }
            if let Some(value) = animation.fps {
                self.animation_fps = value;
            }
            // The fourth key. Without it `mochic state` reported the default
            // curve for the whole session while the renderer eased with the
            // one from the file, and a file that set a style could never undo
            // an `animation-style` command the way every other visual key can.
            if let Some(value) = animation.style {
                self.animation_style = client_animation_style(value);
            }
        }
    }
}

/// The wire spelling of a configuration file's easing curve.
///
/// Derived by inverting [`crate::wm::animation_style_of`] rather than written
/// out a second time, so the two mappings cannot drift apart. A curve the wire
/// has no name for falls back to the default instead of failing a reload.
fn client_animation_style(style: mochi_core::animation::AnimationStyle) -> AnimationStyle {
    AnimationStyle::ALL
        .iter()
        .copied()
        .find(|&candidate| crate::wm::animation_style_of(candidate) == style)
        .unwrap_or(AnimationStyle::Linear)
}

/// The facts about this daemon process that the tiling model does not carry.
#[derive(Debug, Clone, Serialize)]
pub struct State {
    /// Version of the running daemon.
    pub version: &'static str,
    /// True when every write is only logged.
    pub dry_run: bool,
    /// Configuration file in use.
    pub config_path: PathBuf,
    /// The community rule file the configuration pointed at, if any.
    pub app_config_path: Option<PathBuf>,
    /// Window classes forced into management by `--manage-class`.
    pub manage_classes: Vec<String>,
    /// Border, transparency and animation settings as the managers have them.
    pub settings: Settings,
    /// Registered subscriber pipe names.
    pub subscribers: Vec<String>,
}

impl State {
    /// An empty state pointing at a configuration file.
    pub fn new(config_path: PathBuf, dry_run: bool) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            dry_run,
            config_path,
            app_config_path: None,
            manage_classes: Vec::new(),
            settings: Settings::default(),
            subscribers: Vec::new(),
        }
    }

    /// The configuration file in use.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }
}

/// Where a window actually is on screen, as opposed to where the model says
/// it should be.
///
/// The model stores no rectangle: `rect` on a window in this snapshot is the
/// TILE it was assigned, and every window of a stacked container reports the
/// same one. That is useful, and it is not the same question as "where is this
/// window". A window Mochi could not move, could not uncloak, or that ignored
/// the rectangle it was given reads as perfectly placed, which is exactly the
/// case somebody running `mochic state` is trying to diagnose. The daemon
/// measures the real thing and hands it over here.
pub type OnScreen = std::collections::HashMap<isize, (Rect, bool)>;

/// The JSON document `mochic state` prints.
///
/// The shape is monitors, then their workspaces, then the containers of each
/// workspace, then the windows of each container, with the rectangle the last
/// layout gave them. Everything a status bar or a script needs is reachable
/// without a second command:
///
/// ```json
/// {
///   "version": "0.1.0", "dry_run": false, "paused": false,
///   "focused_monitor": 0, "focused_window": 852368,
///   "monitors": [{ "index": 0, "workspaces": [{ "containers": [ ... ] }] }]
/// }
/// ```
pub fn snapshot(
    session: &State,
    core: &CoreState,
    foreground: Option<Hwnd>,
    on_screen: &OnScreen,
) -> Value {
    json!({
        "version": session.version,
        "dry_run": session.dry_run,
        "paused": core.is_paused,
        "config_path": session.config_path.display().to_string(),
        "app_config_path": session.app_config_path.as_ref().map(|p| p.display().to_string()),
        "manage_classes": session.manage_classes,
        "focused_monitor": core.focused_monitor_idx(),
        "focused_workspace": core.focused_indices().map(|(_, w)| w).ok(),
        "focused_window": core.focused_window_id().map(mochi_core::WindowId::get),
        "foreground_window": foreground.map(Hwnd::as_i64),
        "window_count": core.all_window_ids().count(),
        "monitors": core
            .monitors()
            .iter()
            .enumerate()
            .map(|(index, monitor)| monitor_json(core, index, monitor, on_screen))
            .collect::<Vec<_>>(),
        "settings": session.settings,
        "behaviour": {
            "window_hiding_behaviour": core.window_hiding_behaviour,
            "cross_monitor_move_behaviour": core.cross_monitor_move_behaviour,
            "unmanaged_window_operation_behaviour": core.unmanaged_window_operation_behaviour,
            "window_container_behaviour": core.window_container_behaviour,
            "focus_follows_mouse": core.focus_follows_mouse,
            "mouse_follows_focus": core.mouse_follows_focus,
            "float_override": core.float_override,
            "resize_delta": core.resize_delta,
            "default_workspace_padding": core.default_workspace_padding,
            "default_container_padding": core.default_container_padding,
        },
        "rules": core.rules.len(),
        "subscribers": session.subscribers,
    })
}

fn monitor_json(core: &CoreState, index: usize, monitor: &Monitor, on_screen: &OnScreen) -> Value {
    json!({
        "index": index,
        "id": monitor.id,
        "name": monitor.name,
        "device": monitor.device,
        "device_id": monitor.device_id,
        "size": rect_json(monitor.size),
        "work_area": rect_json(monitor.work_area),
        "dpi": monitor.dpi,
        "scale": monitor.scale_factor(),
        "focused_workspace": monitor.focused_workspace_idx(),
        "last_focused_workspace": monitor.last_focused_workspace,
        "workspaces": monitor
            .workspaces()
            .iter()
            .enumerate()
            .map(|(idx, workspace)| workspace_json(core, index, monitor, idx, workspace, on_screen))
            .collect::<Vec<_>>(),
    })
}

fn workspace_json(
    core: &CoreState,
    monitor_idx: usize,
    monitor: &Monitor,
    index: usize,
    workspace: &Workspace,
    on_screen: &OnScreen,
) -> Value {
    let work_area = core
        .work_area_for(monitor_idx, index)
        .unwrap_or(monitor.work_area);
    // The same scale the daemon actually applies, so the reported rectangle is
    // the one on screen rather than the one a 96 DPI screen would have got.
    let full = workspace.full_rect_scaled(
        work_area,
        core.default_workspace_padding,
        core.default_container_padding,
        core.padding_scale(monitor_idx),
    );
    let layout = workspace.latest_layout();

    json!({
        "index": index,
        "name": monitor.workspace_name(index),
        "layout": workspace.effective_layout().to_string(),
        "flip": {
            "horizontal": workspace.layout_flip.is_flipped(mochi_core::Axis::Horizontal),
            "vertical": workspace.layout_flip.is_flipped(mochi_core::Axis::Vertical),
        },
        "tile": workspace.tile,
        "monocle": workspace.is_monocle(),
        "maximized": workspace.is_maximized(),
        "visible": monitor.focused_workspace_idx() == index,
        "work_area": rect_json(work_area),
        "workspace_padding": workspace.workspace_padding.unwrap_or(core.default_workspace_padding),
        "container_padding": workspace.container_padding.unwrap_or(core.default_container_padding),
        "focused_container": workspace.focused_container_idx(),
        "focused_window": workspace.focused_window_id().map(mochi_core::WindowId::get),
        "containers": workspace
            .containers()
            .iter()
            .enumerate()
            .map(|(idx, container)| {
                container_json(container, idx, layout.get(idx).copied(), on_screen)
            })
            .collect::<Vec<_>>(),
        "monocle_container": workspace
            .monocle_container()
            .map(|container| container_json(container, 0, Some(full), on_screen)),
        "maximized_window": workspace
            .maximized_window()
            .map(|window| window_json(window, Some(work_area), true, on_screen)),
        "floating_windows": workspace
            .floating_windows()
            .iter()
            .map(|window| window_json(window, None, true, on_screen))
            .collect::<Vec<_>>(),
    })
}

fn container_json(
    container: &Container,
    index: usize,
    rect: Option<Rect>,
    on_screen: &OnScreen,
) -> Value {
    let focused = container.focused_window_id();
    json!({
        "index": index,
        "rect": rect.map(rect_json),
        "focused_window": focused.map(mochi_core::WindowId::get),
        "stack": container.is_stack(),
        "windows": container
            .windows()
            .iter()
            .map(|window| window_json(window, rect, Some(window.id) == focused, on_screen))
            .collect::<Vec<_>>(),
    })
}

fn window_json(window: &Window, rect: Option<Rect>, shown: bool, on_screen: &OnScreen) -> Value {
    let measured = on_screen.get(&window.id.get());
    json!({
        "hwnd": window.id.get(),
        "title": window.title,
        "exe": window.exe,
        "class": window.class,
        "path": window.path,
        // Where the model says it belongs: the tile, shared by every window of
        // a stacked container, and null for a floating one the model does not
        // place.
        "rect": rect.map(rect_json),
        // Where it actually is, read off the desktop. Absent when the window
        // could not be read, which is what a window that has just died looks
        // like. When this disagrees with `rect`, the window is not where Mochi
        // believes it is, and that is worth knowing.
        "actual_rect": measured.map(|&(rect, _)| rect_json(rect)),
        // The model's answer: the window its container is showing. A stack
        // shows one of its windows and hides the rest.
        "visible": shown,
        // The desktop's answer, which is not the same question: false for a
        // window that is cloaked or hidden however that came about.
        "on_screen": measured.map(|&(_, visible)| visible),
    })
}

fn rect_json(rect: Rect) -> Value {
    json!({
        "left": rect.left,
        "top": rect.top,
        "right": rect.right,
        "bottom": rect.bottom,
        "width": rect.width(),
        "height": rect.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mochi_core::model::Monitor as CoreMonitor;
    use mochi_core::{Layout, Window as CoreWindow};

    #[test]
    fn a_window_that_is_not_where_the_model_thinks_says_so() {
        // The case this exists for. A window Mochi was not allowed to move
        // stays where it opened, but the model still has it down for the tile
        // it was assigned, so `rect` describes a window that is somewhere else
        // entirely and the snapshot reads as a perfectly tiled desktop. That
        // is precisely the state somebody runs `mochic state` to diagnose.
        let mut core = core();
        core.add_window(CoreWindow::new(0x111).with_exe("WindowsTerminal.exe"))
            .unwrap();

        let stranded = Rect::new(0, 0, 400, 300);
        let mut measured = OnScreen::new();
        measured.insert(0x111, (stranded, true));

        let json = snapshot(&session(), &core, Some(Hwnd(0x111)), &measured);
        let window = &json["monitors"][0]["workspaces"][0]["containers"][0]["windows"][0];

        let tile = &window["rect"];
        assert!(!tile.is_null(), "the model still assigns it a tile");
        assert_eq!(window["actual_rect"]["right"], 400);
        assert_eq!(window["actual_rect"]["bottom"], 300);
        assert_ne!(
            tile["right"], window["actual_rect"]["right"],
            "the whole point: the two disagree and the snapshot shows it"
        );
        assert_eq!(window["on_screen"], true);
    }

    #[test]
    fn a_window_that_could_not_be_read_reports_nothing_rather_than_a_guess() {
        // A window that died between the model being read and the desktop
        // being measured. Reporting the tile as though it were measured would
        // be worse than reporting nothing.
        let mut core = core();
        core.add_window(CoreWindow::new(0x111)).unwrap();

        let json = snapshot(&session(), &core, None, &nothing_measured());
        let window = &json["monitors"][0]["workspaces"][0]["containers"][0]["windows"][0];

        assert!(window["actual_rect"].is_null());
        assert!(window["on_screen"].is_null());
        assert!(!window["rect"].is_null(), "the model half still reports");
    }

    /// No window could be measured, which is what every test that does not
    /// care about the real desktop wants: the snapshot still reports the
    /// model, and `actual_rect` and `on_screen` come back null.
    fn nothing_measured() -> OnScreen {
        OnScreen::new()
    }

    fn core() -> CoreState {
        let mut core = CoreState::new();
        core.default_workspace_padding = 14;
        core.default_container_padding = 10;
        let screen = Rect::new(0, 0, 1920, 1080);
        let mut monitor = CoreMonitor::new(0x1234, screen, Rect::new(0, 0, 1920, 1032))
            .with_name("DISPLAY1")
            .with_device(r"\\.\DISPLAY1", "Odyssey G80SD")
            .with_dpi(144);
        monitor.ensure_workspaces(9);
        core.add_monitor(monitor);
        core
    }

    fn session() -> State {
        State::new(PathBuf::from(r"C:\Users\x\mochi.json"), true)
    }

    #[test]
    fn the_snapshot_walks_monitors_workspaces_containers_and_windows() {
        let mut core = core();
        core.add_window(
            CoreWindow::new(0x111)
                .with_title("Cargo.toml")
                .with_exe("Code.exe")
                .with_class("Chrome_WidgetWin_1"),
        )
        .unwrap();
        core.add_window(
            CoreWindow::new(0x222)
                .with_title("Mochi")
                .with_exe("firefox.exe"),
        )
        .unwrap();

        let json = snapshot(&session(), &core, Some(Hwnd(0x222)), &nothing_measured());

        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["dry_run"], true);
        assert_eq!(json["paused"], false);
        assert_eq!(json["window_count"], 2);
        assert_eq!(json["focused_monitor"], 0);
        assert_eq!(json["focused_window"], 0x222);
        assert_eq!(json["foreground_window"], 0x222);

        let monitor = &json["monitors"][0];
        assert_eq!(monitor["name"], "DISPLAY1");
        assert_eq!(monitor["device"], r"\\.\DISPLAY1");
        assert_eq!(monitor["dpi"], 144);
        assert_eq!(monitor["work_area"]["bottom"], 1032);
        assert_eq!(monitor["workspaces"].as_array().unwrap().len(), 9);

        let workspace = &monitor["workspaces"][0];
        assert_eq!(workspace["name"], "1");
        assert_eq!(workspace["layout"], "BSP");
        assert_eq!(workspace["visible"], true);
        assert_eq!(workspace["workspace_padding"], 14);
        assert_eq!(workspace["container_padding"], 10);
        assert_eq!(workspace["containers"].as_array().unwrap().len(), 2);
        assert_eq!(monitor["workspaces"][1]["visible"], false);

        // The first container keeps the left half of the padded work area.
        let first = &workspace["containers"][0];
        assert_eq!(first["windows"][0]["hwnd"], 0x111);
        assert_eq!(first["windows"][0]["title"], "Cargo.toml");
        assert_eq!(first["windows"][0]["exe"], "Code.exe");
        assert_eq!(
            first["rect"]["left"].as_i64(),
            core.rect_for_window(mochi_core::WindowId(0x111))
                .map(|r| i64::from(r.left))
        );
        assert!(
            first["rect"]["left"].as_i64().unwrap() >= 14,
            "padded away from the edge"
        );
        assert!(first["rect"]["width"].as_i64().unwrap() > 0);
        assert_eq!(first["stack"], false);
    }

    #[test]
    fn monocle_and_floating_windows_show_up_in_their_own_fields() {
        let mut core = core();
        core.add_window(CoreWindow::new(1)).unwrap();
        core.add_window(CoreWindow::new(2)).unwrap();
        core.toggle_monocle().unwrap();

        let json = snapshot(&session(), &core, None, &nothing_measured());
        let workspace = &json["monitors"][0]["workspaces"][0];
        assert_eq!(workspace["monocle"], true);
        assert_eq!(workspace["monocle_container"]["windows"][0]["hwnd"], 2);
        assert!(
            workspace["monocle_container"]["rect"]["width"]
                .as_i64()
                .unwrap()
                > 0
        );

        core.toggle_monocle().unwrap();
        core.toggle_float().unwrap();
        let json = snapshot(&session(), &core, None, &nothing_measured());
        let workspace = &json["monitors"][0]["workspaces"][0];
        assert_eq!(workspace["monocle"], false);
        assert_eq!(workspace["floating_windows"][0]["hwnd"], 2);
        assert_eq!(workspace["containers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_behaviour_block_reports_what_the_configuration_set() {
        let mut core = core();
        core.window_container_behaviour = mochi_core::model::WindowContainerBehaviour::Append;
        core.mouse_follows_focus = false;
        let json = snapshot(&session(), &core, None, &nothing_measured());
        assert_eq!(json["behaviour"]["window_hiding_behaviour"], "Cloak");
        assert_eq!(json["behaviour"]["cross_monitor_move_behaviour"], "Swap");
        assert_eq!(json["behaviour"]["window_container_behaviour"], "Append");
        assert_eq!(json["behaviour"]["mouse_follows_focus"], false);
        assert_eq!(json["behaviour"]["resize_delta"], 50);
    }

    #[test]
    fn a_stack_is_reported_as_one_container_with_two_windows() {
        let mut core = core();
        core.window_container_behaviour = mochi_core::model::WindowContainerBehaviour::Append;
        core.add_window(CoreWindow::new(1)).unwrap();
        core.add_window(CoreWindow::new(2)).unwrap();

        let workspace =
            snapshot(&session(), &core, None, &nothing_measured())["monitors"][0]["workspaces"][0]
                .clone();
        assert_eq!(workspace["containers"].as_array().unwrap().len(), 1);
        assert_eq!(workspace["containers"][0]["stack"], true);
        assert_eq!(workspace["containers"][0]["windows"][0]["visible"], false);
        assert_eq!(workspace["containers"][0]["windows"][1]["visible"], true);
    }

    #[test]
    fn boolean_settings_follow_the_command_line_spelling() {
        let mut s = Settings::default();
        Settings::set(&mut s.border, BooleanState::Enable);
        assert!(s.border);
        Settings::set(&mut s.border, BooleanState::Disable);
        assert!(!s.border);
    }

    #[test]
    fn the_visual_keys_of_a_configuration_land_in_the_settings() {
        let config = mochi_core::config::Config::from_json(
            r#"{
              "border": true,
              "border_width": 6,
              "border_offset": -1,
              "border_style": "Rounded",
              "transparency": true,
              "transparency_alpha": 235,
              "animation": { "enabled": true, "duration": 250, "fps": 60 }
            }"#,
        )
        .unwrap();

        let mut settings = Settings::default();
        settings.apply(&config);
        assert!(settings.border);
        assert_eq!(settings.border_width, 6);
        assert_eq!(settings.border_offset, -1);
        assert_eq!(settings.border_style, BorderStyle::Rounded);
        assert!(settings.transparency);
        assert_eq!(settings.transparency_alpha, 235);
        assert!(settings.animation);
        assert_eq!(settings.animation_duration, 250);

        // A key the file does not carry keeps whatever it had.
        settings.animation_fps = 120;
        settings.apply(&mochi_core::config::Config::default());
        assert_eq!(settings.animation_fps, 120);
        assert_eq!(settings.border_style, BorderStyle::Rounded);
    }

    #[test]
    fn the_layout_name_follows_the_layout_rules() {
        let mut core = core();
        core.add_window(CoreWindow::new(1)).unwrap();
        core.workspace_mut(0, 0)
            .unwrap()
            .layout_rules
            .insert(1, Layout::Columns);
        let json = snapshot(&session(), &core, None, &nothing_measured());
        assert_eq!(json["monitors"][0]["workspaces"][0]["layout"], "Columns");
    }
}
