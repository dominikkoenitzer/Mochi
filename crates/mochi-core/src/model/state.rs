//! The whole window manager state: a ring of monitors plus the global settings.

use serde::{Deserialize, Serialize};

use crate::MAX_WORKSPACES;
use crate::error::{Error, Result};
use crate::geometry::{Direction, Offset, Rect, nearest_in_direction};
use crate::rules::RuleSets;

use super::monitor::Monitor;
use super::ring::Ring;
use super::window::{Window, WindowId};
use super::workspace::Workspace;

/// How a window is taken off screen when its workspace is not visible.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum HidingBehaviour {
    /// `ShowWindow(SW_HIDE)`. Fast, but some applications dislike it.
    Hide,
    /// Minimize the window. Plays nicely with the taskbar, animates.
    Minimize,
    /// Mark the window as cloaked in DWM. Invisible but still composited.
    #[default]
    Cloak,
}

/// What happens when a container is moved past the edge of its monitor.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum MoveBehaviour {
    /// Swap places with the container that is focused on the target monitor.
    #[default]
    Swap,
    /// Insert the container into the target workspace at the edge it entered
    /// through.
    Insert,
    /// Do nothing at the edge.
    NoOp,
}

/// What happens when a command is aimed at a window the manager does not own.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum OperationBehaviour {
    /// Run the command anyway.
    #[default]
    Op,
    /// Refuse the command.
    NoOp,
}

/// Which implementation of focus follows mouse is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum FocusFollowsMouseImplementation {
    /// The Windows accessibility setting.
    Windows,
    /// Mochi's own mouse tracking.
    Mochi,
}

/// Whether a new window joins the focused container or gets one of its own.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum WindowContainerBehaviour {
    /// Give every new window its own container.
    #[default]
    Create,
    /// Stack every new window onto the focused container.
    Append,
}

/// Everything the window manager knows.
///
/// One thread in the daemon owns exactly one of these. Every command is a
/// method that mutates it and hands back a [`super::Changes`] describing what
/// the daemon has to do to the real windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct State {
    /// The displays, with the focused one marked.
    monitors: Ring<Monitor>,
    /// Warp the cursor to the middle of a window when it takes focus.
    pub mouse_follows_focus: bool,
    /// What moving a container past a monitor edge does.
    pub cross_monitor_move_behaviour: MoveBehaviour,
    /// How windows on an inactive workspace are taken off screen.
    pub window_hiding_behaviour: HidingBehaviour,
    /// Whether hovering a window focuses it, and with which implementation.
    pub focus_follows_mouse: Option<FocusFollowsMouseImplementation>,
    /// What a command aimed at an unmanaged window does.
    pub unmanaged_window_operation_behaviour: OperationBehaviour,
    /// Whether a new window stacks onto the focused container.
    pub window_container_behaviour: WindowContainerBehaviour,
    /// While paused, nothing is tiled, hidden or moved.
    pub is_paused: bool,
    /// The workspace padding a workspace uses when it has none of its own.
    pub default_workspace_padding: i32,
    /// The container padding a workspace uses when it has none of its own.
    pub default_container_padding: i32,
    /// How many pixels one `resize-axis` step moves a boundary.
    pub resize_delta: i32,
    /// A work area offset applied to every monitor without one of its own.
    pub work_area_offset: Option<Offset>,
    /// Make every new window float, everywhere.
    pub float_override: bool,
    /// Multiply the paddings and the minimum tile size by the monitor scale
    /// factor, so a 14 pixel padding looks the same on every display.
    pub scale_padding_with_dpi: bool,
    /// Windows that appeared while the manager was paused.
    ///
    /// They are tracked but not managed: nothing is moved, hidden or focused
    /// for them until [`State::toggle_pause`] turns tiling back on or
    /// [`State::retile`] runs, which is when they join the focused workspace
    /// through the normal rules.
    pub pending_windows: Vec<Window>,
    /// The rules that decide what happens to a window when it appears.
    pub rules: RuleSets,
}

impl Default for State {
    fn default() -> Self {
        Self {
            monitors: Ring::new(),
            mouse_follows_focus: true,
            cross_monitor_move_behaviour: MoveBehaviour::default(),
            window_hiding_behaviour: HidingBehaviour::default(),
            focus_follows_mouse: None,
            unmanaged_window_operation_behaviour: OperationBehaviour::default(),
            window_container_behaviour: WindowContainerBehaviour::default(),
            is_paused: false,
            default_workspace_padding: 10,
            default_container_padding: 10,
            resize_delta: 50,
            work_area_offset: None,
            float_override: false,
            scale_padding_with_dpi: true,
            pending_windows: Vec::new(),
            rules: RuleSets::new(),
        }
    }
}

/// The daemon's view of the window manager. An alias for [`State`].
pub type WindowManager = State;

impl State {
    /// An empty state with no monitors.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- monitors -----------------------------------------------------------

    /// The displays.
    #[must_use]
    pub fn monitors(&self) -> &Ring<Monitor> {
        &self.monitors
    }

    /// The displays, mutably.
    pub fn monitors_mut(&mut self) -> &mut Ring<Monitor> {
        &mut self.monitors
    }

    /// The index of the focused monitor.
    #[must_use]
    pub fn focused_monitor_idx(&self) -> usize {
        self.monitors.focused_idx()
    }

    /// The focused monitor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoFocusedMonitor`] when there are no monitors.
    pub fn focused_monitor(&self) -> Result<&Monitor> {
        self.monitors.focused().ok_or(Error::NoFocusedMonitor)
    }

    /// The focused monitor, mutably.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoFocusedMonitor`] when there are no monitors.
    pub fn focused_monitor_mut(&mut self) -> Result<&mut Monitor> {
        self.monitors.focused_mut().ok_or(Error::NoFocusedMonitor)
    }

    /// The focused workspace of the focused monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused monitor or it has no
    /// workspaces.
    pub fn focused_workspace(&self) -> Result<&Workspace> {
        let monitor_idx = self.focused_monitor_idx();
        self.focused_monitor()?
            .focused_workspace()
            .ok_or(Error::NoFocusedWorkspace(monitor_idx))
    }

    /// The focused workspace of the focused monitor, mutably.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused monitor or it has no
    /// workspaces.
    pub fn focused_workspace_mut(&mut self) -> Result<&mut Workspace> {
        let monitor_idx = self.focused_monitor_idx();
        self.focused_monitor_mut()?
            .focused_workspace_mut()
            .ok_or(Error::NoFocusedWorkspace(monitor_idx))
    }

    /// The monitor and workspace indices that are focused right now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoFocusedMonitor`] when there are no monitors.
    pub fn focused_indices(&self) -> Result<(usize, usize)> {
        let monitor = self.focused_monitor()?;
        Ok((self.focused_monitor_idx(), monitor.focused_workspace_idx()))
    }

    /// Adds a monitor and returns its index.
    pub fn add_monitor(&mut self, monitor: Monitor) -> usize {
        self.monitors.push(monitor);
        self.monitors.len() - 1
    }

    /// Removes a monitor by index.
    pub fn remove_monitor(&mut self, idx: usize) -> Option<Monitor> {
        self.monitors.remove(idx)
    }

    /// The index of the monitor with that platform handle.
    #[must_use]
    pub fn monitor_idx_for_id(&self, id: isize) -> Option<usize> {
        self.monitors.position(|m| m.id == id)
    }

    /// The index of the monitor whose display rectangle contains the point.
    #[must_use]
    pub fn monitor_idx_at(&self, x: i32, y: i32) -> Option<usize> {
        self.monitors.position(|m| m.size.contains(x, y))
    }

    /// The index of the monitor that lies in `direction` from the focused one.
    #[must_use]
    pub fn monitor_idx_in_direction(&self, direction: Direction) -> Option<usize> {
        let rects: Vec<Rect> = self.monitors.iter().map(|m| m.size).collect();
        nearest_in_direction(&rects, self.monitors.focused_idx(), direction)
    }

    // -- windows ------------------------------------------------------------

    /// Where a window lives, as a monitor and workspace index pair.
    #[must_use]
    pub fn locate_window(&self, id: WindowId) -> Option<(usize, usize)> {
        self.monitors
            .iter()
            .enumerate()
            .find_map(|(m, monitor)| monitor.workspace_idx_for_window(id).map(|w| (m, w)))
    }

    /// The window with that handle, wherever it is.
    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.monitors
            .iter()
            .flat_map(|m| m.workspaces().iter())
            .find_map(|w| w.window(id))
    }

    /// `true` when any workspace holds the window.
    #[must_use]
    pub fn is_managed(&self, id: WindowId) -> bool {
        self.locate_window(id).is_some()
    }

    /// The handle of the focused window.
    #[must_use]
    pub fn focused_window_id(&self) -> Option<WindowId> {
        self.focused_workspace().ok()?.focused_window_id()
    }

    /// Every window handle the manager owns.
    pub fn all_window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.monitors.iter().flat_map(Monitor::all_window_ids)
    }

    /// The handles that should be on screen right now, across every monitor.
    #[must_use]
    pub fn visible_window_ids(&self) -> Vec<WindowId> {
        self.monitors
            .iter()
            .filter_map(|m| m.focused_workspace())
            .flat_map(Workspace::visible_window_ids)
            .collect()
    }

    // -- geometry -----------------------------------------------------------

    /// The work area a workspace tiles into, with every offset applied.
    #[must_use]
    pub fn work_area_for(&self, monitor: usize, workspace: usize) -> Option<Rect> {
        self.monitors
            .get(monitor)
            .map(|m| m.work_area_for(workspace, self.work_area_offset))
    }

    /// The rectangle a window currently occupies according to the last layout
    /// update, if it is tiled.
    #[must_use]
    pub fn rect_for_window(&self, id: WindowId) -> Option<Rect> {
        let (monitor, workspace) = self.locate_window(id)?;
        let workspace = self.monitors.get(monitor)?.workspaces().get(workspace)?;
        let container = workspace.container_idx_for_window(id)?;
        workspace.latest_layout().get(container).copied()
    }

    // -- helpers used by the operations -------------------------------------

    /// Checks that a workspace index is one we are willing to create.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceIndexOutOfRange`] beyond [`MAX_WORKSPACES`].
    pub fn check_workspace_idx(idx: usize) -> Result<()> {
        if idx >= MAX_WORKSPACES {
            return Err(Error::WorkspaceIndexOutOfRange(idx));
        }
        Ok(())
    }

    /// Checks that a workspace index can be used on that monitor.
    ///
    /// A workspace the configuration created is always allowed, however many
    /// there are: [`MAX_WORKSPACES`] only caps the ones the manager creates on
    /// demand for an index nobody configured.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceIndexOutOfRange`] for an index that neither
    /// exists nor can be created.
    pub fn check_workspace_idx_on(&self, monitor: usize, idx: usize) -> Result<()> {
        let configured = self
            .monitors
            .get(monitor)
            .map_or(0, |m| m.workspaces().len());
        if idx < configured || idx < MAX_WORKSPACES {
            return Ok(());
        }
        Err(Error::WorkspaceIndexOutOfRange(idx))
    }

    /// The factor the paddings and the minimum tile size are multiplied by on
    /// that monitor, or 1.0 when [`State::scale_padding_with_dpi`] is off.
    #[must_use]
    pub fn padding_scale(&self, monitor: usize) -> f32 {
        if !self.scale_padding_with_dpi {
            return 1.0;
        }
        self.monitors
            .get(monitor)
            .map_or(1.0, Monitor::scale_factor)
    }

    /// A mutable borrow of one workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when either index is out of range.
    pub fn workspace_mut(&mut self, monitor: usize, workspace: usize) -> Result<&mut Workspace> {
        self.monitors
            .get_mut(monitor)
            .ok_or(Error::MonitorNotFound(monitor))?
            .workspaces_mut()
            .get_mut(workspace)
            .ok_or(Error::WorkspaceNotFound { monitor, workspace })
    }

    /// A borrow of one workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when either index is out of range.
    pub fn workspace(&self, monitor: usize, workspace: usize) -> Result<&Workspace> {
        self.monitors
            .get(monitor)
            .ok_or(Error::MonitorNotFound(monitor))?
            .workspaces()
            .get(workspace)
            .ok_or(Error::WorkspaceNotFound { monitor, workspace })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_monitors() -> State {
        let mut state = State::new();
        // The 4K main display on the left, the portrait one to its right.
        let mut main = Monitor::new(1, Rect::new(0, 0, 3840, 2160), Rect::new(0, 0, 3840, 2120))
            .with_name("DISPLAY1");
        main.ensure_workspaces(9);
        let mut side = Monitor::new(
            2,
            Rect::new(3840, 0, 4920, 1920),
            Rect::new(3840, 0, 4920, 1880),
        )
        .with_name("DISPLAY2");
        side.ensure_workspaces(9);
        state.add_monitor(main);
        state.add_monitor(side);
        state
    }

    #[test]
    fn an_empty_state_has_no_focus() {
        let state = State::new();
        assert!(state.monitors().is_empty());
        assert_eq!(
            state.focused_monitor().unwrap_err(),
            Error::NoFocusedMonitor
        );
        assert_eq!(
            State::new().focused_workspace().unwrap_err(),
            Error::NoFocusedMonitor
        );
        assert_eq!(state.focused_window_id(), None);
        assert!(state.visible_window_ids().is_empty());
    }

    #[test]
    fn defaults_match_the_documented_behaviour() {
        let state = State::new();
        assert_eq!(state.window_hiding_behaviour, HidingBehaviour::Cloak);
        assert_eq!(state.cross_monitor_move_behaviour, MoveBehaviour::Swap);
        assert_eq!(
            state.unmanaged_window_operation_behaviour,
            OperationBehaviour::Op
        );
        assert_eq!(
            state.window_container_behaviour,
            WindowContainerBehaviour::Create
        );
        assert!(!state.is_paused);
        assert!(state.mouse_follows_focus);
        assert_eq!(state.focus_follows_mouse, None);
        assert_eq!(state.resize_delta, 50);
    }

    #[test]
    fn monitors_are_found_by_handle_and_by_point() {
        let state = two_monitors();
        assert_eq!(state.monitor_idx_for_id(2), Some(1));
        assert_eq!(state.monitor_idx_for_id(9), None);
        assert_eq!(state.monitor_idx_at(100, 100), Some(0));
        assert_eq!(state.monitor_idx_at(4000, 100), Some(1));
        assert_eq!(state.monitor_idx_at(-1, -1), None);
    }

    #[test]
    fn the_monitor_to_the_right_is_the_portrait_one() {
        let mut state = two_monitors();
        assert_eq!(state.monitor_idx_in_direction(Direction::Right), Some(1));
        assert_eq!(state.monitor_idx_in_direction(Direction::Left), None);
        assert_eq!(state.monitor_idx_in_direction(Direction::Up), None);

        assert!(state.monitors_mut().focus(1));
        assert_eq!(state.monitor_idx_in_direction(Direction::Left), Some(0));
        assert_eq!(state.monitor_idx_in_direction(Direction::Right), None);
        assert!(!state.monitors_mut().focus(9));
    }

    #[test]
    fn windows_are_located_across_monitors() {
        let mut state = two_monitors();
        state
            .workspace_mut(1, 3)
            .unwrap()
            .add_window(Window::new(7).with_exe("a.exe"));
        assert_eq!(state.locate_window(WindowId(7)), Some((1, 3)));
        assert!(state.is_managed(WindowId(7)));
        assert!(!state.is_managed(WindowId(8)));
        assert_eq!(
            state.window(WindowId(7)).map(|w| w.exe.as_str()),
            Some("a.exe")
        );
        assert!(state.window(WindowId(8)).is_none());
        assert_eq!(
            state.all_window_ids().collect::<Vec<_>>(),
            vec![WindowId(7)]
        );
    }

    #[test]
    fn only_the_focused_workspace_of_each_monitor_is_visible() {
        let mut state = two_monitors();
        state
            .workspace_mut(0, 0)
            .unwrap()
            .add_window(Window::new(1));
        state
            .workspace_mut(0, 1)
            .unwrap()
            .add_window(Window::new(2));
        state
            .workspace_mut(1, 0)
            .unwrap()
            .add_window(Window::new(3));
        assert_eq!(
            state.visible_window_ids(),
            vec![WindowId(1), WindowId(3)],
            "the window on workspace 2 of the main monitor stays hidden"
        );
    }

    #[test]
    fn workspace_accessors_report_bad_indices() {
        let mut state = two_monitors();
        assert!(state.workspace(0, 0).is_ok());
        assert_eq!(
            state.workspace(9, 0).unwrap_err(),
            Error::MonitorNotFound(9)
        );
        assert_eq!(
            state.workspace(0, 99).unwrap_err(),
            Error::WorkspaceNotFound {
                monitor: 0,
                workspace: 99
            }
        );
        assert!(state.workspace_mut(9, 0).is_err());
        assert!(state.workspace_mut(0, 99).is_err());
    }

    #[test]
    fn workspace_indices_are_capped() {
        assert!(State::check_workspace_idx(0).is_ok());
        assert!(State::check_workspace_idx(MAX_WORKSPACES - 1).is_ok());
        assert_eq!(
            State::check_workspace_idx(MAX_WORKSPACES).unwrap_err(),
            Error::WorkspaceIndexOutOfRange(MAX_WORKSPACES)
        );
    }

    #[test]
    fn the_work_area_comes_from_the_monitor_and_the_global_offset() {
        let mut state = two_monitors();
        assert_eq!(state.work_area_for(0, 0), Some(Rect::new(0, 0, 3840, 2120)));
        state.work_area_offset = Some(Offset::new(0, 40, 0, 0));
        assert_eq!(
            state.work_area_for(0, 0),
            Some(Rect::new(0, 40, 3840, 2120))
        );
        assert_eq!(state.work_area_for(9, 0), None);
    }

    #[test]
    fn rect_for_window_reads_the_latest_layout() {
        let mut state = two_monitors();
        let work_area = state.work_area_for(0, 0).unwrap();
        let workspace = state.workspace_mut(0, 0).unwrap();
        workspace.add_window(Window::new(1));
        workspace.add_window(Window::new(2));
        workspace.update_layout(work_area, 0, 0);

        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(0, 0, 1920, 2120))
        );
        assert_eq!(state.rect_for_window(WindowId(9)), None);
    }

    #[test]
    fn focused_indices_track_the_rings() {
        let mut state = two_monitors();
        assert_eq!(state.focused_indices().unwrap(), (0, 0));
        assert!(state.monitors_mut().focus(1));
        state.focused_monitor_mut().unwrap().focus_workspace(4);
        assert_eq!(state.focused_indices().unwrap(), (1, 4));
        assert!(State::new().focused_indices().is_err());
    }

    #[test]
    fn removing_a_monitor_keeps_the_ring_sane() {
        let mut state = two_monitors();
        assert!(state.remove_monitor(0).is_some());
        assert_eq!(state.monitors().len(), 1);
        assert_eq!(state.focused_monitor_idx(), 0);
        assert!(state.remove_monitor(9).is_none());
    }

    #[test]
    fn a_monitor_without_workspaces_reports_it() {
        let mut state = State::new();
        let mut monitor = Monitor::new(1, Rect::default(), Rect::default());
        monitor.workspaces_mut().clear();
        state.add_monitor(monitor);
        assert_eq!(
            state.focused_workspace().unwrap_err(),
            Error::NoFocusedWorkspace(0)
        );
        assert!(state.focused_workspace_mut().is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let mut state = two_monitors();
        state
            .workspace_mut(0, 0)
            .unwrap()
            .add_window(Window::new(1));
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), state);
    }

    #[test]
    fn missing_json_keys_fall_back_to_the_defaults() {
        let state: State = serde_json::from_str(r#"{"is_paused":true}"#).unwrap();
        assert!(state.is_paused);
        assert_eq!(state.default_container_padding, 10);
    }
}
