//! Every command the daemon and the CLI can run, as pure methods on [`State`].
//!
//! All of them return a [`Changes`] describing what the daemon has to do to
//! the real windows. Nothing here talks to Win32, which is why every one of
//! them is unit tested.
//!
//! While [`State::is_paused`] is set, every command except
//! [`State::toggle_pause`] and [`State::remove_window`] is a no-op, so the game
//! mode hotkey really does stop the window manager dead.

use crate::error::{Error, Result};
use crate::geometry::{Axis, Direction};
use crate::layout::{Layout, Sizing};
use crate::model::{
    Changes, Container, CycleDirection, MoveBehaviour, State, Window, WindowContainerBehaviour,
    WindowId, Workspace,
};
use crate::rules::RuleDecision;

impl State {
    // -- internal helpers ---------------------------------------------------

    /// Recomputes the layout of one workspace.
    fn refresh(&mut self, monitor: usize, workspace: usize) {
        let Some(work_area) = self.work_area_for(monitor, workspace) else {
            return;
        };
        let workspace_padding = self.default_workspace_padding;
        let container_padding = self.default_container_padding;
        if let Ok(target) = self.workspace_mut(monitor, workspace) {
            target.update_layout(work_area, workspace_padding, container_padding);
        }
    }

    /// Recomputes one workspace and reports it as needing a redraw.
    fn retiled(&mut self, monitor: usize, workspace: usize) -> Changes {
        self.refresh(monitor, workspace);
        Changes::none().retile(monitor, workspace)
    }

    /// The focus part of a change set, including the mouse warp when it is on.
    fn focus_changes(&self, id: Option<WindowId>) -> Changes {
        let mut changes = Changes::none();
        changes.focus = id;
        if self.mouse_follows_focus
            && let Some(id) = id
        {
            changes.warp_mouse_to = self.rect_for_window(id);
        }
        changes
    }

    /// Fills in `show` and `hide` from what changed since `before`.
    fn visibility_delta(&self, before: &[WindowId], changes: &mut Changes) {
        let after = self.visible_window_ids();
        changes
            .show
            .extend(after.iter().copied().filter(|id| !before.contains(id)));
        changes
            .hide
            .extend(before.iter().copied().filter(|id| !after.contains(id)));
        changes.settle();
    }

    /// The window that is focused right now, for a follow-up focus change.
    fn focused_window_after(&self) -> Option<WindowId> {
        self.focused_workspace()
            .ok()
            .and_then(Workspace::focused_window_id)
    }

    // -- pause --------------------------------------------------------------

    /// Turns the window manager off and on again without unmanaging anything.
    ///
    /// Unpausing retiles every visible workspace, so whatever the user did to
    /// their windows while paused is undone in one go.
    ///
    /// # Errors
    ///
    /// Never fails today; the result is there so the command surface is uniform.
    pub fn toggle_pause(&mut self) -> Result<Changes> {
        self.is_paused = !self.is_paused;
        if self.is_paused {
            return Ok(Changes::none());
        }
        self.retile()
    }

    /// Recomputes and reapplies the layout of every visible workspace.
    ///
    /// # Errors
    ///
    /// Never fails today; the result is there so the command surface is uniform.
    pub fn retile(&mut self) -> Result<Changes> {
        let mut changes = Changes::none();
        if self.is_paused {
            return Ok(changes);
        }
        for monitor in 0..self.monitors().len() {
            let Some(workspace) = self
                .monitors()
                .get(monitor)
                .map(|m| m.focused_workspace_idx())
            else {
                continue;
            };
            changes.merge(self.retiled(monitor, workspace));
        }
        changes.show.extend(self.visible_window_ids());
        changes.settle();
        Ok(changes)
    }

    /// Turns tiling off and on for the focused workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn toggle_tiling(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        target.tile = !target.tile;
        Ok(self.retiled(monitor, workspace))
    }

    // -- focus --------------------------------------------------------------

    /// Focuses the container in `direction`, crossing to the adjacent monitor
    /// when the focused container is already at the edge.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn focus_direction(&mut self, direction: Direction) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;

        if let Some(idx) = self
            .workspace(monitor, workspace)?
            .container_idx_in_direction(direction)
        {
            let target = self.workspace_mut(monitor, workspace)?;
            target.focus_container(idx);
            let id = target.focused_window_id();
            return Ok(self.focus_changes(id));
        }

        match self.monitor_idx_in_direction(direction) {
            Some(target) => self.focus_monitor(target),
            None => Ok(Changes::none()),
        }
    }

    /// Moves the focus one container along the ring, wrapping.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn cycle_focus(&mut self, direction: CycleDirection) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        target.cycle_container_focus(direction);
        let id = target.focused_window_id();
        Ok(self.focus_changes(id))
    }

    /// Moves the focus through the windows of the focused stack, wrapping.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn cycle_stack(&mut self, direction: CycleDirection) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        let id = target
            .focused_container_mut()
            .and_then(|c| c.cycle_focus(direction));
        let mut changes = self.focus_changes(id);
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Focuses a window the daemon saw take the foreground.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WindowNotFound`] when the window is not managed.
    pub fn focus_window(&mut self, id: WindowId) -> Result<Changes> {
        let (monitor, workspace) = self.locate_window(id).ok_or(Error::WindowNotFound(id))?;
        let before = self.visible_window_ids();
        self.monitors_mut().focus(monitor);
        if let Some(target) = self.monitors_mut().get_mut(monitor) {
            target.focus_workspace(workspace);
        }
        self.workspace_mut(monitor, workspace)?.focus_window(id);
        let mut changes = Changes::none().focus(id);
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    // -- moving containers around -------------------------------------------

    /// Swaps the focused container with the one in `direction`, or moves it to
    /// the adjacent monitor when it is already at the edge.
    ///
    /// What happens at the edge is decided by
    /// [`State::cross_monitor_move_behaviour`].
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn move_direction(&mut self, direction: Direction) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;

        if let Some(idx) = self
            .workspace(monitor, workspace)?
            .container_idx_in_direction(direction)
        {
            let target = self.workspace_mut(monitor, workspace)?;
            target.swap_focused_container(idx);
            let id = target.focused_window_id();
            let mut changes = self.retiled(monitor, workspace);
            changes.merge(self.focus_changes(id));
            return Ok(changes);
        }

        if self.cross_monitor_move_behaviour == MoveBehaviour::NoOp {
            return Ok(Changes::none());
        }
        let Some(target) = self.monitor_idx_in_direction(direction) else {
            return Ok(Changes::none());
        };
        let target_workspace = self
            .monitors()
            .get(target)
            .map_or(0, crate::model::Monitor::focused_workspace_idx);
        self.move_container_to(target, target_workspace, true, Some(direction))
    }

    /// Swaps the focused container with its neighbour in the ring, wrapping.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn cycle_move(&mut self, direction: CycleDirection) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        target.cycle_container_move(direction);
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        Ok(changes)
    }

    /// Moves the focused container to the front of the ring, where most
    /// layouts give it the biggest tile.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn promote(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.promote_focused_container() {
            return Ok(Changes::none());
        }
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        Ok(changes)
    }

    /// Focuses the container at the front of the ring.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn promote_focus(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.focus_container(0) {
            return Ok(Changes::none());
        }
        let id = target.focused_window_id();
        Ok(self.focus_changes(id))
    }

    // -- stacking -----------------------------------------------------------

    /// Stacks the focused window onto the container in `direction`.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn stack(&mut self, direction: Direction) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let Some(idx) = self
            .workspace(monitor, workspace)?
            .container_idx_in_direction(direction)
        else {
            return Ok(Changes::none());
        };
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.stack_focused_window_into(idx) {
            return Ok(Changes::none());
        }
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Takes the focused window out of its stack into a container of its own.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn unstack(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.unstack_focused_window() {
            return Ok(Changes::none());
        }
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    // -- window states ------------------------------------------------------

    /// Moves the focused window between the tiled containers and the floating
    /// list.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn toggle_float(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        target.toggle_float();
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Blows the focused container up to fill the workspace, or puts it back.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn toggle_monocle(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        target.toggle_monocle();
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Maximizes the focused window, or restores it.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn toggle_maximize(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        let maximized = target.toggle_maximize();
        let id = target.focused_window_id();

        let mut changes = self.retiled(monitor, workspace);
        if maximized {
            changes.maximize = id;
        } else {
            changes.restore.extend(id);
        }
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Minimizes the focused window and drops it from the workspace.
    ///
    /// The daemon adds it back when Windows says it was restored.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn minimize_focused_window(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        let Some(id) = target.focused_window_id() else {
            return Ok(Changes::none());
        };
        target.remove_window(id);
        let mut changes = self.retiled(monitor, workspace);
        changes.minimize.push(id);
        changes.merge(self.focus_changes(self.focused_window_after()));
        Ok(changes)
    }

    /// Asks the daemon to close the focused window.
    ///
    /// The window stays in the model until the daemon reports that it is gone.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn close_focused_window(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let mut changes = Changes::none();
        changes.close = self.focused_workspace()?.focused_window_id();
        Ok(changes)
    }

    // -- layouts ------------------------------------------------------------

    /// Sets the layout of the focused workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn change_layout(&mut self, layout: Layout) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        target.layout = layout;
        target.layout_rules.clear();
        Ok(self.retiled(monitor, workspace))
    }

    /// Moves the focused workspace to the next or previous layout.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn cycle_layout(&mut self, direction: CycleDirection) -> Result<Changes> {
        let next = self.focused_workspace()?.layout.cycle(direction);
        self.change_layout(next)
    }

    /// Mirrors the focused workspace's layout on `axis`.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn flip_layout(&mut self, axis: Axis) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let target = self.workspace_mut(monitor, workspace)?;
        target.layout_flip = target.layout_flip.toggled(axis);
        Ok(self.retiled(monitor, workspace))
    }

    /// Grows or shrinks the focused container along `axis`.
    ///
    /// Tries the container's far edge first and its near edge second, so the
    /// command does something sensible whichever side of the layout the
    /// container happens to sit on. A resize that the clamps refuse leaves the
    /// layout untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace or no container.
    pub fn resize_axis(&mut self, axis: Axis, sizing: Sizing) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let delta = sizing.signed(self.resize_delta);
        let Some(work_area) = self.work_area_for(monitor, workspace) else {
            return Err(Error::MonitorNotFound(monitor));
        };
        let workspace_padding = self.default_workspace_padding;
        let container_padding = self.default_container_padding;

        let target = self.workspace_mut(monitor, workspace)?;
        if target.containers().is_empty() {
            return Err(Error::NoFocusedContainer);
        }
        let idx = target.focused_container_idx();

        target.update_layout(work_area, workspace_padding, container_padding);
        let original = target.resize_dimension(idx);
        let before = target.latest_layout().get(idx).copied();

        for far_edge in [true, false] {
            let mut next = original.unwrap_or_default();
            match (axis, far_edge) {
                (Axis::Horizontal, true) => next.right += delta,
                (Axis::Horizontal, false) => next.left -= delta,
                (Axis::Vertical, true) => next.bottom += delta,
                (Axis::Vertical, false) => next.top -= delta,
            }
            target.set_resize_dimension(idx, Some(next));
            target.update_layout(work_area, workspace_padding, container_padding);
            if target.latest_layout().get(idx).copied() != before {
                return Ok(Changes::none().retile(monitor, workspace));
            }
        }

        target.set_resize_dimension(idx, original);
        target.update_layout(work_area, workspace_padding, container_padding);
        Ok(Changes::none())
    }

    // -- workspaces ---------------------------------------------------------

    /// Focuses a workspace on the focused monitor, creating it if needed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceIndexOutOfRange`] past
    /// [`crate::MAX_WORKSPACES`], or an error when there is no monitor.
    pub fn focus_workspace(&mut self, idx: usize) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        Self::check_workspace_idx(idx)?;
        let monitor = self.focused_monitor_idx();
        let before = self.visible_window_ids();

        let target = self.focused_monitor_mut()?;
        target.ensure_workspaces(idx + 1);
        target.focus_workspace(idx);

        let mut changes = self.retiled(monitor, idx);
        self.visibility_delta(&before, &mut changes);
        changes.merge(self.focus_changes(self.focused_window_after()));
        Ok(changes)
    }

    /// Goes back to the workspace that was focused before this one.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no monitor.
    pub fn focus_last_workspace(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let Some(last) = self.focused_monitor()?.last_focused_workspace else {
            return Ok(Changes::none());
        };
        self.focus_workspace(last)
    }

    /// Focuses the next or previous workspace on the focused monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no monitor.
    pub fn cycle_workspace(&mut self, direction: CycleDirection) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let monitor = self.focused_monitor()?;
        let Some(idx) = direction.step(monitor.focused_workspace_idx(), monitor.workspaces().len())
        else {
            return Ok(Changes::none());
        };
        self.focus_workspace(idx)
    }

    /// Moves the focused container to another workspace on the same monitor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceIndexOutOfRange`] past
    /// [`crate::MAX_WORKSPACES`], or an error when there is no workspace.
    pub fn move_to_workspace(&mut self, idx: usize, follow: bool) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        Self::check_workspace_idx(idx)?;
        let monitor = self.focused_monitor_idx();
        self.focused_monitor_mut()?.ensure_workspaces(idx + 1);
        self.move_focused_container(monitor, idx, follow)
    }

    // -- monitors -----------------------------------------------------------

    /// Focuses another monitor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MonitorNotFound`] for an index that does not exist.
    pub fn focus_monitor(&mut self, idx: usize) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        if idx >= self.monitors().len() {
            return Err(Error::MonitorNotFound(idx));
        }
        let previous = self.focused_monitor_idx();
        self.monitors_mut().focus(idx);
        let mut changes = self.focus_changes(self.focused_window_after());
        changes.focused_monitor_changed = previous != idx;
        Ok(changes)
    }

    /// Focuses the next or previous monitor.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no monitor.
    pub fn cycle_monitor(&mut self, direction: CycleDirection) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let Some(idx) = direction.step(self.focused_monitor_idx(), self.monitors().len()) else {
            return Ok(Changes::none());
        };
        self.focus_monitor(idx)
    }

    /// Moves the focused container to the focused workspace of another monitor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MonitorNotFound`] for an index that does not exist.
    pub fn move_to_monitor(&mut self, idx: usize, follow: bool) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let workspace = self
            .monitors()
            .get(idx)
            .ok_or(Error::MonitorNotFound(idx))?
            .focused_workspace_idx();
        self.move_focused_container(idx, workspace, follow)
    }

    /// Moves the focused container to any workspace on any monitor.
    ///
    /// Honours [`State::cross_monitor_move_behaviour`] when the destination is
    /// on another monitor: `Swap` trades places with whatever is focused at the
    /// destination, `Insert` slots the container in next to it. A move to
    /// another workspace of the same monitor never swaps, whatever the setting
    /// says, because the setting is about monitor edges.
    ///
    /// # Errors
    ///
    /// Returns an error when either end does not exist.
    pub fn move_focused_container(
        &mut self,
        to_monitor: usize,
        to_workspace: usize,
        follow: bool,
    ) -> Result<Changes> {
        self.move_container_to(to_monitor, to_workspace, follow, None)
    }

    /// The body of [`State::move_focused_container`].
    ///
    /// `entering_from` is the direction the container is travelling in when the
    /// move came from [`State::move_direction`] crossing a monitor edge. It
    /// decides which end of the destination ring an inserted container joins:
    /// a container travelling right or down enters through the near edge of
    /// the next monitor and goes to the front, one travelling left or up
    /// enters through the far edge and goes to the back. Without a direction
    /// the container lands next to whatever is focused at the destination.
    fn move_container_to(
        &mut self,
        to_monitor: usize,
        to_workspace: usize,
        follow: bool,
        entering_from: Option<Direction>,
    ) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (from_monitor, from_workspace) = self.focused_indices()?;
        if (from_monitor, from_workspace) == (to_monitor, to_workspace) {
            return Ok(Changes::none());
        }
        // Make sure the destination exists before anything is taken apart.
        self.workspace(to_monitor, to_workspace)?;

        let before = self.visible_window_ids();
        let source = self.workspace_mut(from_monitor, from_workspace)?;
        let source_idx = source.focused_container_idx();
        let Some(container) = source.remove_focused_container() else {
            return Ok(Changes::none());
        };
        let moved = container.focused_window_id();

        let swapping =
            self.cross_monitor_move_behaviour == MoveBehaviour::Swap && to_monitor != from_monitor;

        // The slot the container is going into has to be read before the
        // container that may be swapped out of it is removed, or the moved
        // container lands one place short of where it should.
        let destination = self.workspace_mut(to_monitor, to_workspace)?;
        let at = match entering_from {
            Some(direction) if !swapping => {
                if direction.is_forward() {
                    0
                } else {
                    destination.containers().len()
                }
            }
            _ => destination.focused_container_idx(),
        };

        let swap_back = if swapping {
            self.workspace_mut(to_monitor, to_workspace)?
                .remove_focused_container()
        } else {
            None
        };

        let destination = self.workspace_mut(to_monitor, to_workspace)?;
        destination.insert_container(at, container);

        if let Some(container) = swap_back {
            self.workspace_mut(from_monitor, from_workspace)?
                .insert_container(source_idx, container);
        }

        let mut changes = self.retiled(from_monitor, from_workspace);
        changes.merge(self.retiled(to_monitor, to_workspace));

        if follow {
            self.monitors_mut().focus(to_monitor);
            if let Some(target) = self.monitors_mut().get_mut(to_monitor) {
                target.focus_workspace(to_workspace);
            }
            changes.merge(self.focus_changes(moved));
            changes.focused_monitor_changed = from_monitor != to_monitor;
        } else {
            changes.merge(self.focus_changes(self.focused_window_after()));
        }

        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    // -- window lifecycle ---------------------------------------------------

    /// Adds a new window to the focused workspace, obeying the rules.
    ///
    /// Returns an empty change set for a window an ignore rule matches. The
    /// daemon routes a window to a specific workspace with
    /// [`State::add_window_to`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn add_window(&mut self, window: Window) -> Result<Changes> {
        let (monitor, workspace) = self.focused_indices()?;
        self.add_window_to(monitor, workspace, window)
    }

    /// Adds a new window to a specific workspace, obeying the rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace does not exist.
    pub fn add_window_to(
        &mut self,
        monitor: usize,
        workspace: usize,
        window: Window,
    ) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let decision = self.rules.decide(&window.info());
        if decision == RuleDecision::Ignore {
            return Ok(Changes::none());
        }
        if self.is_managed(window.id) {
            return self.focus_window(window.id);
        }

        let before = self.visible_window_ids();
        let stack_it = self.window_container_behaviour == WindowContainerBehaviour::Append;
        let float_override = self.float_override;

        let target = self.workspace_mut(monitor, workspace)?;
        let id = window.id;
        if decision == RuleDecision::Float || float_override || target.float_override {
            target.add_floating_window(window);
        } else if stack_it && target.focused_container().is_some() {
            if let Some(container) = target.focused_container_mut() {
                container.add_window(window);
            }
        } else {
            let at = if target.containers().is_empty() {
                0
            } else {
                target.focused_container_idx() + 1
            };
            target.insert_window(at, window);
        }

        let mut changes = self.retiled(monitor, workspace);
        // A window that opens on a workspace nobody is looking at, or behind a
        // monocle, has to be taken off screen: it is on screen right now
        // because Windows just created it, and it is not in `before`, so the
        // visibility delta alone would leave it there. It does not get the
        // foreground either, for the same reason.
        if self.visible_window_ids().contains(&id) {
            changes.merge(self.focus_changes(Some(id)));
        } else {
            changes.hide.push(id);
        }
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Drops a window that is gone from wherever it was.
    ///
    /// Works while paused, because a destroyed window must never stay in the
    /// model.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WindowNotFound`] when the window was not managed.
    pub fn remove_window(&mut self, id: WindowId) -> Result<Changes> {
        let (monitor, workspace) = self.locate_window(id).ok_or(Error::WindowNotFound(id))?;
        let before = self.visible_window_ids();
        self.workspace_mut(monitor, workspace)?.remove_window(id);

        if self.is_paused {
            return Ok(Changes::none());
        }

        let mut changes = self.retiled(monitor, workspace);
        if (monitor, workspace) == self.focused_indices()? {
            changes.merge(self.focus_changes(self.focused_window_after()));
        }
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Moves a managed window into the floating list of its workspace.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WindowNotFound`] when the window is not managed.
    pub fn float_window(&mut self, id: WindowId) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.locate_window(id).ok_or(Error::WindowNotFound(id))?;
        let before = self.visible_window_ids();
        if !self.workspace_mut(monitor, workspace)?.float_window(id) {
            return Ok(Changes::none());
        }
        let mut changes = self.retiled(monitor, workspace);
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// The container the focused window sits in, as a value the daemon can
    /// hand to a stackbar.
    #[must_use]
    pub fn focused_container(&self) -> Option<&Container> {
        self.focused_workspace().ok()?.focused_container()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::layout::Flip;
    use crate::model::{HidingBehaviour, Monitor};
    use crate::rules::{ApplicationIdentifier, MatchingRule, MatchingStrategy};

    const MAIN: Rect = Rect::new(0, 0, 1920, 1080);
    const SIDE: Rect = Rect::new(1920, 0, 3000, 1920);

    fn state() -> State {
        let mut state = State::new();
        state.default_workspace_padding = 0;
        state.default_container_padding = 0;
        let mut main = Monitor::new(1, MAIN, MAIN).with_name("DISPLAY1");
        main.ensure_workspaces(9);
        let mut side = Monitor::new(2, SIDE, SIDE).with_name("DISPLAY2");
        side.ensure_workspaces(9);
        state.add_monitor(main);
        state.add_monitor(side);
        state
    }

    fn with_windows(count: usize) -> State {
        let mut state = state();
        for id in 1..=count {
            state.add_window(Window::new(id as isize)).unwrap();
        }
        state
    }

    fn focused(state: &State) -> Option<WindowId> {
        state.focused_window_id()
    }

    // -- adding and removing ------------------------------------------------

    #[test]
    fn adding_windows_tiles_them_and_focuses_the_newest() {
        let state = with_windows(3);
        let workspace = state.workspace(0, 0).unwrap();
        assert_eq!(workspace.containers().len(), 3);
        assert_eq!(focused(&state), Some(WindowId(3)));
        assert_eq!(workspace.latest_layout().len(), 3);
        assert_eq!(workspace.latest_layout()[0], Rect::new(0, 0, 960, 1080));
    }

    #[test]
    fn a_new_window_lands_next_to_the_focused_one() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(1)).unwrap();
        state.add_window(Window::new(9)).unwrap();
        let ids: Vec<_> = state
            .workspace(0, 0)
            .unwrap()
            .containers()
            .iter()
            .filter_map(crate::model::Container::focused_window_id)
            .collect();
        assert_eq!(
            ids,
            vec![WindowId(1), WindowId(9), WindowId(2), WindowId(3)]
        );
    }

    #[test]
    fn an_ignored_window_is_never_added() {
        let mut state = state();
        state.rules.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "zebar.exe",
            MatchingStrategy::Equals,
        ));
        let changes = state
            .add_window(Window::new(1).with_exe("zebar.exe"))
            .unwrap();
        assert!(changes.is_empty());
        assert!(!state.is_managed(WindowId(1)));
    }

    #[test]
    fn a_floating_rule_puts_the_window_in_the_floating_list() {
        let mut state = state();
        state.rules.floating_applications.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "Calculator.exe",
            MatchingStrategy::Equals,
        ));
        state
            .add_window(Window::new(1).with_exe("Calculator.exe"))
            .unwrap();
        let workspace = state.workspace(0, 0).unwrap();
        assert!(workspace.containers().is_empty());
        assert_eq!(workspace.floating_windows().len(), 1);
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn float_override_floats_everything() {
        let mut state = state();
        state.float_override = true;
        state.add_window(Window::new(1)).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().floating_windows().len(), 1);
    }

    #[test]
    fn append_behaviour_stacks_new_windows() {
        let mut state = state();
        state.window_container_behaviour = WindowContainerBehaviour::Append;
        state.add_window(Window::new(1)).unwrap();
        state.add_window(Window::new(2)).unwrap();
        let workspace = state.workspace(0, 0).unwrap();
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers().get(0).unwrap().len(), 2);
        assert_eq!(focused(&state), Some(WindowId(2)));
    }

    #[test]
    fn adding_a_window_twice_only_focuses_it() {
        let mut state = with_windows(3);
        state.add_window(Window::new(1)).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 3);
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn removing_a_window_retiles_and_moves_the_focus() {
        let mut state = with_windows(3);
        let changes = state.remove_window(WindowId(3)).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 2);
        assert!(
            changes
                .retiled
                .contains(&crate::model::WorkspaceRef::new(0, 0))
        );
        assert_eq!(changes.focus, focused(&state));
        assert_eq!(
            state.remove_window(WindowId(99)).unwrap_err(),
            Error::WindowNotFound(WindowId(99))
        );
    }

    #[test]
    fn a_window_can_be_added_to_another_workspace() {
        let mut state = state();
        let changes = state.add_window_to(1, 4, Window::new(7)).unwrap();
        assert_eq!(state.locate_window(WindowId(7)), Some((1, 4)));
        assert!(
            changes.hide.contains(&WindowId(7)) || !changes.show.contains(&WindowId(7)),
            "a window added to an unfocused workspace is not shown"
        );
        assert!(state.add_window_to(0, 99, Window::new(8)).is_err());
    }

    // -- focus --------------------------------------------------------------

    #[test]
    fn directional_focus_walks_the_bsp_tree() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(1)).unwrap();

        state.focus_direction(Direction::Right).unwrap();
        assert_eq!(focused(&state), Some(WindowId(2)));

        state.focus_direction(Direction::Down).unwrap();
        assert_eq!(focused(&state), Some(WindowId(3)));

        state.focus_direction(Direction::Left).unwrap();
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn directional_focus_crosses_to_the_next_monitor_at_the_edge() {
        let mut state = with_windows(1);
        state.add_window_to(1, 0, Window::new(50)).unwrap();
        state.focus_window(WindowId(1)).unwrap();

        let changes = state.focus_direction(Direction::Right).unwrap();
        assert_eq!(state.focused_monitor_idx(), 1);
        assert_eq!(focused(&state), Some(WindowId(50)));
        assert!(changes.focused_monitor_changed);

        state.focus_direction(Direction::Left).unwrap();
        assert_eq!(state.focused_monitor_idx(), 0);
    }

    #[test]
    fn directional_focus_at_the_outer_edge_does_nothing() {
        let mut state = with_windows(1);
        assert!(state.focus_direction(Direction::Left).unwrap().is_empty());
        assert!(state.focus_direction(Direction::Up).unwrap().is_empty());
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn cycling_focus_wraps_through_the_containers() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(1)).unwrap();
        state.cycle_focus(CycleDirection::Next).unwrap();
        assert_eq!(focused(&state), Some(WindowId(2)));
        state.cycle_focus(CycleDirection::Previous).unwrap();
        state.cycle_focus(CycleDirection::Previous).unwrap();
        assert_eq!(focused(&state), Some(WindowId(3)), "wrapped round");
    }

    #[test]
    fn cycling_a_stack_swaps_which_window_is_visible() {
        let mut state = with_windows(2);
        state.window_container_behaviour = WindowContainerBehaviour::Append;
        state.add_window(Window::new(3)).unwrap();

        let changes = state.cycle_stack(CycleDirection::Next).unwrap();
        assert_eq!(focused(&state), Some(WindowId(2)));
        assert!(changes.show.contains(&WindowId(2)));
        assert!(changes.hide.contains(&WindowId(3)));
    }

    #[test]
    fn focusing_a_window_follows_it_to_its_monitor_and_workspace() {
        let mut state = with_windows(1);
        state.add_window_to(1, 5, Window::new(60)).unwrap();
        let changes = state.focus_window(WindowId(60)).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (1, 5));
        assert_eq!(changes.focus, Some(WindowId(60)));
        assert!(changes.show.contains(&WindowId(60)));
        assert!(state.focus_window(WindowId(99)).is_err());
    }

    #[test]
    fn mouse_follows_focus_reports_where_to_warp() {
        let mut state = with_windows(2);
        state.mouse_follows_focus = true;
        let changes = state.focus_direction(Direction::Left).unwrap();
        assert_eq!(changes.focus, Some(WindowId(1)));
        assert_eq!(changes.warp_mouse_to, Some(Rect::new(0, 0, 960, 1080)));
    }

    // -- moving -------------------------------------------------------------

    #[test]
    fn moving_a_window_swaps_it_with_its_neighbour() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();
        state.move_direction(Direction::Right).unwrap();

        let ids: Vec<_> = state
            .workspace(0, 0)
            .unwrap()
            .containers()
            .iter()
            .filter_map(crate::model::Container::focused_window_id)
            .collect();
        assert_eq!(ids, vec![WindowId(2), WindowId(1)]);
        assert_eq!(focused(&state), Some(WindowId(1)), "the focus follows");
    }

    #[test]
    fn moving_past_the_edge_inserts_on_the_next_monitor() {
        let mut state = with_windows(1);
        state.cross_monitor_move_behaviour = MoveBehaviour::Insert;
        state.add_window_to(1, 0, Window::new(50)).unwrap();
        state.focus_window(WindowId(1)).unwrap();

        state.move_direction(Direction::Right).unwrap();
        assert_eq!(state.locate_window(WindowId(1)), Some((1, 0)));
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 0);
        assert_eq!(state.workspace(1, 0).unwrap().containers().len(), 2);
        assert_eq!(state.focused_monitor_idx(), 1, "the focus follows");
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn moving_past_the_edge_can_swap_instead() {
        let mut state = with_windows(1);
        state.cross_monitor_move_behaviour = MoveBehaviour::Swap;
        state.add_window_to(1, 0, Window::new(50)).unwrap();
        state.focus_window(WindowId(1)).unwrap();

        state.move_direction(Direction::Right).unwrap();
        assert_eq!(state.locate_window(WindowId(1)), Some((1, 0)));
        assert_eq!(
            state.locate_window(WindowId(50)),
            Some((0, 0)),
            "the other window came back the other way"
        );
    }

    #[test]
    fn a_no_op_cross_monitor_behaviour_keeps_the_window_put() {
        let mut state = with_windows(1);
        state.cross_monitor_move_behaviour = MoveBehaviour::NoOp;
        state.add_window_to(1, 0, Window::new(50)).unwrap();
        state.focus_window(WindowId(1)).unwrap();
        assert!(state.move_direction(Direction::Right).unwrap().is_empty());
        assert_eq!(state.locate_window(WindowId(1)), Some((0, 0)));
    }

    #[test]
    fn cycle_move_reorders_the_ring() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(1)).unwrap();
        state.cycle_move(CycleDirection::Next).unwrap();
        let ids: Vec<_> = state
            .workspace(0, 0)
            .unwrap()
            .containers()
            .iter()
            .filter_map(crate::model::Container::focused_window_id)
            .collect();
        assert_eq!(ids, vec![WindowId(2), WindowId(1), WindowId(3)]);
        assert_eq!(focused(&state), Some(WindowId(1)));
    }

    #[test]
    fn promote_moves_the_window_to_the_biggest_tile() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(3)).unwrap();
        state.promote().unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(3)),
            Some(Rect::new(0, 0, 960, 1080))
        );
        assert_eq!(focused(&state), Some(WindowId(3)));
        assert!(state.promote().unwrap().is_empty(), "already promoted");
    }

    #[test]
    fn promote_focus_jumps_to_the_first_container() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(3)).unwrap();
        state.promote_focus().unwrap();
        assert_eq!(focused(&state), Some(WindowId(1)));
        assert!(State::new().promote_focus().is_err());
    }

    // -- stacking -----------------------------------------------------------

    #[test]
    fn stacking_and_unstacking_in_a_direction() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();

        let changes = state.stack(Direction::Right).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 1);
        assert_eq!(focused(&state), Some(WindowId(1)));
        assert!(changes.hide.contains(&WindowId(2)));

        let changes = state.unstack().unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 2);
        assert!(changes.show.contains(&WindowId(2)));
    }

    #[test]
    fn stacking_with_nothing_in_that_direction_is_a_no_op() {
        let mut state = with_windows(1);
        assert!(state.stack(Direction::Right).unwrap().is_empty());
        assert!(state.unstack().unwrap().is_empty());
    }

    // -- window states ------------------------------------------------------

    #[test]
    fn toggling_float_moves_the_window_out_of_the_layout_and_back() {
        let mut state = with_windows(2);
        state.toggle_float().unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 1);
        assert_eq!(state.workspace(0, 0).unwrap().floating_windows().len(), 1);
        assert_eq!(focused(&state), Some(WindowId(2)));

        state.toggle_float().unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 2);
        assert!(state.workspace(0, 0).unwrap().floating_windows().is_empty());
    }

    #[test]
    fn float_window_by_handle() {
        let mut state = with_windows(2);
        state.float_window(WindowId(1)).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().floating_windows().len(), 1);
        assert!(state.float_window(WindowId(1)).unwrap().is_empty());
        assert!(state.float_window(WindowId(99)).is_err());
    }

    #[test]
    fn monocle_hides_everything_else_and_gives_it_back() {
        let mut state = with_windows(3);
        let changes = state.toggle_monocle().unwrap();
        assert!(state.workspace(0, 0).unwrap().is_monocle());
        assert!(changes.hide.contains(&WindowId(1)));
        assert!(changes.hide.contains(&WindowId(2)));
        assert_eq!(changes.focus, Some(WindowId(3)));

        let changes = state.toggle_monocle().unwrap();
        assert!(!state.workspace(0, 0).unwrap().is_monocle());
        assert!(changes.show.contains(&WindowId(1)));
        assert_eq!(focused(&state), Some(WindowId(3)));
    }

    #[test]
    fn maximize_reports_the_window_to_maximize_and_to_restore() {
        let mut state = with_windows(2);
        let changes = state.toggle_maximize().unwrap();
        assert_eq!(changes.maximize, Some(WindowId(2)));
        assert!(changes.restore.is_empty());
        assert!(state.workspace(0, 0).unwrap().is_maximized());

        let changes = state.toggle_maximize().unwrap();
        assert_eq!(changes.maximize, None);
        assert_eq!(changes.restore, vec![WindowId(2)]);
        assert!(!state.workspace(0, 0).unwrap().is_maximized());
    }

    #[test]
    fn minimize_drops_the_window_and_retiles() {
        let mut state = with_windows(2);
        let changes = state.minimize_focused_window().unwrap();
        assert_eq!(changes.minimize, vec![WindowId(2)]);
        assert!(!state.is_managed(WindowId(2)));
        assert_eq!(focused(&state), Some(WindowId(1)));
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(MAIN),
            "the survivor takes the whole workspace"
        );
    }

    #[test]
    fn close_only_reports_the_window_and_leaves_the_model_alone() {
        let mut state = with_windows(2);
        let changes = state.close_focused_window().unwrap();
        assert_eq!(changes.close, Some(WindowId(2)));
        assert!(state.is_managed(WindowId(2)), "the destroy event does that");
    }

    #[test]
    fn minimize_and_close_on_an_empty_workspace_do_nothing() {
        let mut state = state();
        assert!(state.minimize_focused_window().unwrap().is_empty());
        assert!(state.close_focused_window().unwrap().is_empty());
    }

    // -- layouts ------------------------------------------------------------

    #[test]
    fn changing_the_layout_retiles() {
        let mut state = with_windows(3);
        state.change_layout(Layout::Columns).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().layout, Layout::Columns);
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(0, 0, 640, 1080))
        );
    }

    #[test]
    fn cycling_the_layout_walks_the_list() {
        let mut state = with_windows(2);
        state.cycle_layout(CycleDirection::Next).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().layout, Layout::Columns);
        state.cycle_layout(CycleDirection::Previous).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().layout, Layout::Bsp);
    }

    #[test]
    fn changing_the_layout_drops_the_layout_rules() {
        let mut state = with_windows(2);
        state
            .workspace_mut(0, 0)
            .unwrap()
            .layout_rules
            .insert(1, Layout::Grid);
        state.change_layout(Layout::Rows).unwrap();
        assert!(state.workspace(0, 0).unwrap().layout_rules.is_empty());
        assert_eq!(
            state.workspace(0, 0).unwrap().effective_layout(),
            Layout::Rows
        );
    }

    #[test]
    fn flipping_mirrors_the_layout_and_toggles_back() {
        let mut state = with_windows(2);
        state.flip_layout(Axis::Horizontal).unwrap();
        assert_eq!(
            state.workspace(0, 0).unwrap().layout_flip,
            Flip::horizontal()
        );
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(960, 0, 1920, 1080))
        );

        state.flip_layout(Axis::Vertical).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().layout_flip, Flip::both());
        state.flip_layout(Axis::Horizontal).unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().layout_flip, Flip::vertical());
    }

    // -- resizing -----------------------------------------------------------

    #[test]
    fn resizing_horizontally_moves_the_first_bsp_cut() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();
        state
            .resize_axis(Axis::Horizontal, Sizing::Increase)
            .unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(0, 0, 1010, 1080)),
            "960 plus the 50 pixel step"
        );

        state
            .resize_axis(Axis::Horizontal, Sizing::Decrease)
            .unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(0, 0, 960, 1080))
        );
    }

    #[test]
    fn resizing_works_from_the_other_side_of_the_cut_too() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(2)).unwrap();
        state
            .resize_axis(Axis::Horizontal, Sizing::Increase)
            .unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(910, 0, 1920, 1080)),
            "the near edge moved because the far edge is the screen edge"
        );
    }

    #[test]
    fn resizing_along_an_axis_with_no_cut_does_nothing() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();
        let before = state.rect_for_window(WindowId(1));
        let changes = state.resize_axis(Axis::Vertical, Sizing::Increase).unwrap();
        assert!(changes.is_empty());
        assert_eq!(state.rect_for_window(WindowId(1)), before);
        assert_eq!(state.workspace(0, 0).unwrap().resize_dimension(0), None);
    }

    #[test]
    fn resizing_the_vertical_cut_of_a_three_window_bsp() {
        let mut state = with_windows(3);
        state.focus_window(WindowId(2)).unwrap();
        state.resize_axis(Axis::Vertical, Sizing::Increase).unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(960, 0, 1920, 590))
        );
        assert_eq!(
            state.rect_for_window(WindowId(3)),
            Some(Rect::new(960, 590, 1920, 1080))
        );
    }

    #[test]
    fn resizing_is_clamped_and_repeated_steps_keep_the_layout_valid() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();
        for _ in 0..100 {
            state
                .resize_axis(Axis::Horizontal, Sizing::Increase)
                .unwrap();
        }
        let first = state.rect_for_window(WindowId(1)).unwrap();
        let second = state.rect_for_window(WindowId(2)).unwrap();
        assert!(second.width() > 0, "the other window never disappears");
        assert_eq!(first.right, second.left, "the tiling still holds");
    }

    #[test]
    fn resizing_without_a_container_is_an_error() {
        let mut state = state();
        assert_eq!(
            state
                .resize_axis(Axis::Horizontal, Sizing::Increase)
                .unwrap_err(),
            Error::NoFocusedContainer
        );
    }

    // -- workspaces ---------------------------------------------------------

    #[test]
    fn switching_workspaces_hides_one_set_and_shows_the_other() {
        let mut state = with_windows(2);
        let changes = state.focus_workspace(1).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 1));
        assert!(changes.hide.contains(&WindowId(1)));
        assert!(changes.hide.contains(&WindowId(2)));
        assert!(changes.show.is_empty());

        state.add_window(Window::new(3)).unwrap();
        let changes = state.focus_workspace(0).unwrap();
        assert!(changes.show.contains(&WindowId(1)));
        assert!(changes.hide.contains(&WindowId(3)));
        assert_eq!(changes.focus, Some(WindowId(2)));
    }

    #[test]
    fn workspaces_are_created_on_demand_up_to_the_limit() {
        let mut state = state();
        state
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .workspaces_mut()
            .clear();
        state
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .ensure_workspaces(1);
        assert_eq!(state.monitors().get(0).unwrap().workspaces().len(), 1);

        state.focus_workspace(8).unwrap();
        assert_eq!(state.monitors().get(0).unwrap().workspaces().len(), 9);
        assert_eq!(
            state.focus_workspace(crate::MAX_WORKSPACES).unwrap_err(),
            Error::WorkspaceIndexOutOfRange(crate::MAX_WORKSPACES)
        );
    }

    #[test]
    fn focus_last_workspace_goes_back_and_forth() {
        let mut state = with_windows(1);
        assert!(
            state.focus_last_workspace().unwrap().is_empty(),
            "nothing to go back to yet"
        );
        state.focus_workspace(4).unwrap();
        state.focus_last_workspace().unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 0));
        state.focus_last_workspace().unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 4));
    }

    #[test]
    fn cycling_workspaces_wraps_round() {
        let mut state = state();
        state.cycle_workspace(CycleDirection::Next).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 1));
        state.cycle_workspace(CycleDirection::Previous).unwrap();
        state.cycle_workspace(CycleDirection::Previous).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 8), "wrapped round");
    }

    #[test]
    fn moving_to_a_workspace_takes_the_window_along() {
        let mut state = with_windows(2);
        let changes = state.move_to_workspace(3, true).unwrap();
        assert_eq!(state.locate_window(WindowId(2)), Some((0, 3)));
        assert_eq!(state.focused_indices().unwrap(), (0, 3));
        assert_eq!(focused(&state), Some(WindowId(2)));
        assert!(changes.hide.contains(&WindowId(1)));
    }

    #[test]
    fn sending_to_a_workspace_stays_put() {
        let mut state = with_windows(2);
        let changes = state.move_to_workspace(3, false).unwrap();
        assert_eq!(state.locate_window(WindowId(2)), Some((0, 3)));
        assert_eq!(state.focused_indices().unwrap(), (0, 0));
        assert_eq!(focused(&state), Some(WindowId(1)));
        assert!(changes.hide.contains(&WindowId(2)));
    }

    #[test]
    fn moving_to_the_workspace_you_are_on_is_a_no_op() {
        let mut state = with_windows(2);
        assert!(state.move_to_workspace(0, true).unwrap().is_empty());
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 2);
    }

    // -- monitors -----------------------------------------------------------

    #[test]
    fn focusing_and_cycling_monitors() {
        let mut state = with_windows(1);
        state.add_window_to(1, 0, Window::new(50)).unwrap();

        let changes = state.focus_monitor(1).unwrap();
        assert_eq!(state.focused_monitor_idx(), 1);
        assert!(changes.focused_monitor_changed);
        assert_eq!(changes.focus, Some(WindowId(50)));

        state.cycle_monitor(CycleDirection::Next).unwrap();
        assert_eq!(state.focused_monitor_idx(), 0, "wrapped round");
        assert_eq!(
            state.focus_monitor(9).unwrap_err(),
            Error::MonitorNotFound(9)
        );
    }

    #[test]
    fn moving_to_another_monitor_by_index() {
        let mut state = with_windows(2);
        state.move_to_monitor(1, true).unwrap();
        assert_eq!(state.locate_window(WindowId(2)), Some((1, 0)));
        assert_eq!(state.focused_monitor_idx(), 1);
        assert!(state.move_to_monitor(9, true).is_err());
    }

    #[test]
    fn a_window_moved_to_a_hidden_workspace_is_hidden() {
        let mut state = with_windows(2);
        state.monitors_mut().get_mut(1).unwrap().focus_workspace(2);
        let changes = state.move_focused_container(1, 5, false).unwrap();
        assert_eq!(state.locate_window(WindowId(2)), Some((1, 5)));
        assert!(changes.hide.contains(&WindowId(2)));
    }

    // -- retile, pause and tiling -------------------------------------------

    #[test]
    fn retile_touches_every_visible_workspace() {
        let mut state = with_windows(2);
        state.add_window_to(1, 0, Window::new(50)).unwrap();
        let changes = state.retile().unwrap();
        assert_eq!(changes.retiled.len(), 2);
        assert!(changes.show.contains(&WindowId(50)));
    }

    #[test]
    fn pausing_stops_every_command() {
        let mut state = with_windows(2);
        assert!(state.toggle_pause().unwrap().is_empty());
        assert!(state.is_paused);

        assert!(state.focus_direction(Direction::Left).unwrap().is_empty());
        assert!(state.move_direction(Direction::Left).unwrap().is_empty());
        assert!(state.cycle_focus(CycleDirection::Next).unwrap().is_empty());
        assert!(state.cycle_move(CycleDirection::Next).unwrap().is_empty());
        assert!(state.cycle_stack(CycleDirection::Next).unwrap().is_empty());
        assert!(state.toggle_float().unwrap().is_empty());
        assert!(state.toggle_monocle().unwrap().is_empty());
        assert!(state.toggle_maximize().unwrap().is_empty());
        assert!(state.toggle_tiling().unwrap().is_empty());
        assert!(state.promote().unwrap().is_empty());
        assert!(state.promote_focus().unwrap().is_empty());
        assert!(state.stack(Direction::Left).unwrap().is_empty());
        assert!(state.unstack().unwrap().is_empty());
        assert!(state.change_layout(Layout::Grid).unwrap().is_empty());
        assert!(state.cycle_layout(CycleDirection::Next).unwrap().is_empty());
        assert!(state.flip_layout(Axis::Horizontal).unwrap().is_empty());
        assert!(
            state
                .resize_axis(Axis::Horizontal, Sizing::Increase)
                .unwrap()
                .is_empty()
        );
        assert!(state.focus_workspace(4).unwrap().is_empty());
        assert!(state.focus_last_workspace().unwrap().is_empty());
        assert!(
            state
                .cycle_workspace(CycleDirection::Next)
                .unwrap()
                .is_empty()
        );
        assert!(state.move_to_workspace(4, true).unwrap().is_empty());
        assert!(state.focus_monitor(1).unwrap().is_empty());
        assert!(
            state
                .cycle_monitor(CycleDirection::Next)
                .unwrap()
                .is_empty()
        );
        assert!(state.move_to_monitor(1, true).unwrap().is_empty());
        assert!(state.add_window(Window::new(9)).unwrap().is_empty());
        assert!(state.float_window(WindowId(1)).unwrap().is_empty());
        assert!(state.minimize_focused_window().unwrap().is_empty());
        assert!(state.close_focused_window().unwrap().is_empty());
        assert!(state.retile().unwrap().is_empty());

        assert_eq!(state.workspace(0, 0).unwrap().layout, Layout::Bsp);
        assert_eq!(state.focused_indices().unwrap(), (0, 0));
        assert!(!state.is_managed(WindowId(9)));
    }

    #[test]
    fn a_destroyed_window_still_leaves_the_model_while_paused() {
        let mut state = with_windows(2);
        state.toggle_pause().unwrap();
        assert!(state.remove_window(WindowId(2)).unwrap().is_empty());
        assert!(!state.is_managed(WindowId(2)));
    }

    #[test]
    fn unpausing_retiles_everything() {
        let mut state = with_windows(2);
        state.toggle_pause().unwrap();
        let changes = state.toggle_pause().unwrap();
        assert!(!state.is_paused);
        assert!(!changes.retiled.is_empty());
        assert!(changes.show.contains(&WindowId(1)));
    }

    #[test]
    fn turning_tiling_off_leaves_the_windows_where_they_are() {
        let mut state = with_windows(2);
        state.toggle_tiling().unwrap();
        assert!(!state.workspace(0, 0).unwrap().tile);
        assert!(state.workspace(0, 0).unwrap().latest_layout().is_empty());
        state.toggle_tiling().unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().latest_layout().len(), 2);
    }

    // -- the hotkeys the user actually has ----------------------------------

    #[test]
    fn every_command_in_the_whkdrc_runs() {
        let mut state = with_windows(3);
        state.window_hiding_behaviour = HidingBehaviour::Cloak;

        for direction in Direction::ALL {
            state.focus_direction(direction).unwrap();
        }
        for direction in Direction::ALL {
            state.move_direction(direction).unwrap();
        }
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for sizing in [Sizing::Increase, Sizing::Decrease] {
                state.resize_axis(axis, sizing).unwrap();
            }
        }
        state.toggle_float().unwrap();
        state.toggle_float().unwrap();
        state.toggle_maximize().unwrap();
        state.toggle_maximize().unwrap();
        state.toggle_monocle().unwrap();
        state.toggle_monocle().unwrap();
        state.minimize_focused_window().unwrap();
        state.close_focused_window().unwrap();
        state.cycle_layout(CycleDirection::Next).unwrap();
        state.flip_layout(Axis::Horizontal).unwrap();
        state.flip_layout(Axis::Vertical).unwrap();
        state.cycle_workspace(CycleDirection::Previous).unwrap();
        state.cycle_workspace(CycleDirection::Next).unwrap();
        state.focus_last_workspace().unwrap();
        for idx in 0..9 {
            state.focus_workspace(idx).unwrap();
        }
        state.focus_workspace(0).unwrap();
        for idx in 0..9 {
            state.move_to_workspace(idx, true).unwrap();
        }
        state.toggle_pause().unwrap();
        state.toggle_pause().unwrap();
        state.retile().unwrap();

        // Nothing was lost along the way.
        assert_eq!(state.all_window_ids().count(), 2, "one was minimized");
    }
}
