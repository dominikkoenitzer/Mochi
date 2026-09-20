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
    Changes, Container, CycleDirection, Monitor, MoveBehaviour, Ring, State, Window,
    WindowContainerBehaviour, WindowId, Workspace,
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
        let scale = self.padding_scale(monitor);
        if let Ok(target) = self.workspace_mut(monitor, workspace) {
            target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
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

    /// `true` when the focused window of that workspace is not in its
    /// container ring.
    ///
    /// [`Workspace::focused_window_id`] resolves a maximized, monocled or
    /// floating window before it ever looks at the ring, and all three have
    /// been lifted out of it. Every op that rearranges containers works
    /// through the ring's focus index instead, so with one of those focused it
    /// would take hold of a completely different window: `move-to-monitor`
    /// teleported an unrelated tile to the other screen and dragged the focus
    /// along with it.
    ///
    /// The ops refuse in that case rather than moving the focused window.
    /// Refusing is the choice because there is no honest translation of the
    /// command: a floating window has no tile to trade places with, a monocle
    /// belongs to the workspace it was turned on in and cannot travel with the
    /// window, and a maximized window is a mode, not a container. Doing
    /// nothing is the only outcome that can never touch a window the user did
    /// not point at. The modes are one keystroke away from being off, and then
    /// the move does exactly what it says.
    fn focus_outside_the_ring(&self, monitor: usize, workspace: usize) -> bool {
        self.workspace(monitor, workspace).is_ok_and(|target| {
            target.is_maximized() || target.is_monocle() || target.focus_is_floating()
        })
    }

    /// The window that is focused right now, for a follow-up focus change.
    fn focused_window_after(&self) -> Option<WindowId> {
        self.focused_workspace()
            .ok()
            .and_then(Workspace::focused_window_id)
    }

    /// Adds the windows that appeared while the manager was paused.
    ///
    /// They go to the focused workspace through [`State::add_window_to`], so
    /// the ignore, float and workspace rules still get their say.
    fn take_pending_windows(&mut self) -> Changes {
        let mut changes = Changes::none();
        if self.pending_windows.is_empty() {
            return changes;
        }
        let Ok((monitor, workspace)) = self.focused_indices() else {
            return changes;
        };
        for window in std::mem::take(&mut self.pending_windows) {
            if let Ok(added) = self.add_window_to(monitor, workspace, window) {
                changes.merge(added);
            }
        }
        changes
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
        changes.merge(self.take_pending_windows());
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
        let Some(id) = target
            .focused_container_mut()
            .and_then(|c| c.cycle_focus(direction))
        else {
            return Ok(Changes::none());
        };
        // The window the container now shows has been sitting wherever the
        // layout last left it, which for a freshly stacked window is its old
        // tile. Without the retile it comes forward in the wrong place.
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(Some(id)));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Focuses a window the daemon saw take the foreground.
    ///
    /// A no-op while paused, like every other mutating command: this one moves
    /// the focused monitor and workspace of the model, and the retile that
    /// unpausing does would then be aimed at whichever workspace the user
    /// happened to click on in the meantime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WindowNotFound`] when the window is not managed.
    pub fn focus_window(&mut self, id: WindowId) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
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
    /// Does nothing when the focused window is outside the container ring; see
    /// `focus_outside_the_ring`.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn move_direction(&mut self, direction: Direction) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }

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
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
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
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
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
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
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
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
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

    /// Collapses every container in the focused workspace into one stack.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn stack_all(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.stack_all() {
            return Ok(Changes::none());
        }
        let id = target.focused_window_id();
        let mut changes = self.retiled(monitor, workspace);
        changes.merge(self.focus_changes(id));
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Gives every stacked window in the focused workspace its own container.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace.
    pub fn unstack_all(&mut self) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        if !target.unstack_all() {
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
    /// Does nothing while monocle mode is on: the two modes both fill the
    /// workspace and are exclusive.
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
        let was_maximized = target.is_maximized();
        let maximized = target.toggle_maximize();
        if maximized == was_maximized {
            // Nothing happened: monocle mode refuses the maximize, and an
            // empty workspace has nothing to maximize. Falling through would
            // tell the daemon to restore a window that was never maximized.
            return Ok(Changes::none());
        }
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
        let before = self.visible_window_ids();
        let target = self.workspace_mut(monitor, workspace)?;
        let Some(id) = target.focused_window_id() else {
            return Ok(Changes::none());
        };
        let was_maximized = target.is_maximized();
        target.remove_window(id);
        let mut changes = self.retiled(monitor, workspace);
        if was_maximized {
            // The model forgets the window was maximized, but Windows does
            // not: without this the user un-minimizes it and it comes back
            // filling the screen with nothing in the model saying so. The
            // daemon restores before it minimizes, so the order works out.
            changes.restore.push(id);
        }
        changes.minimize.push(id);
        changes.merge(self.focus_changes(self.focused_window_after()));
        // Minimizing the visible window of a stack leaves the one underneath
        // cloaked while it holds the tile, and in monocle mode it blanks the
        // whole workspace. The delta is what brings the replacement back.
        self.visibility_delta(&before, &mut changes);
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
    /// container happens to sit on. The near edge is only for a container with
    /// no boundary on its far side: a far edge the clamp refuses stays the one
    /// the command works on, so the same number of presses back always lands
    /// on the layout it started from. A resize that the clamps refuse leaves
    /// the layout untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace or no container.
    pub fn resize_axis(&mut self, axis: Axis, sizing: Sizing) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
        let delta = sizing.signed(self.resize_delta);
        let Some(work_area) = self.work_area_for(monitor, workspace) else {
            return Err(Error::MonitorNotFound(monitor));
        };
        let workspace_padding = self.default_workspace_padding;
        let container_padding = self.default_container_padding;
        let scale = self.padding_scale(monitor);

        let target = self.workspace_mut(monitor, workspace)?;
        if target.containers().is_empty() {
            return Err(Error::NoFocusedContainer);
        }
        let idx = target.focused_container_idx();

        target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
        let original = target.resize_dimension(idx);
        let before = target.latest_layout().get(idx).copied();

        for far_edge in [true, false] {
            let next = Self::nudged(original.unwrap_or_default(), axis, far_edge, delta);
            target.set_resize_dimension(idx, Some(next));
            target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
            if target.latest_layout().get(idx).copied() != before {
                // The step is stored the size it was asked for, even when the
                // clamp could only grant part of it. Storing what the boundary
                // really did instead throws the rest of the step away, and
                // then the same number of presses back lands short of where
                // the layout started: leaning on the key drifts. The stored
                // delta still cannot run away, because a press that moves
                // nothing at all is not stored at all, so it stays within one
                // delta of what the clamp allows.
                return Ok(Changes::none().retile(monitor, workspace));
            }

            // Nothing moved. Either this edge is no boundary at all and the
            // container sits at that side of the layout, or the clamp refuses
            // this direction. Pushing the other way tells the two apart, and
            // only the first is worth falling back to the near edge for: a
            // container grown from both sides cannot be shrunk back by the
            // same number of presses, because every one of them comes off
            // whichever edge is tried first.
            let probe = Self::nudged(original.unwrap_or_default(), axis, far_edge, -delta);
            target.set_resize_dimension(idx, Some(probe));
            target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
            if target.latest_layout().get(idx).copied() != before {
                break;
            }
        }

        target.set_resize_dimension(idx, original);
        target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
        Ok(Changes::none())
    }

    /// Grows or shrinks the focused container by moving the edge it names.
    ///
    /// [`State::resize_axis`] picks the edge itself, which is the right thing
    /// on a layout where only one of the two is a boundary. This one moves the
    /// edge it was given and nothing else, so a binding per edge does what the
    /// key it sits under looks like it does. A resize that the clamps refuse
    /// leaves the layout untouched, exactly as on the axis command.
    ///
    /// # Errors
    ///
    /// Returns an error when there is no focused workspace or no container.
    pub fn resize_edge(&mut self, direction: Direction, sizing: Sizing) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self.focused_indices()?;
        if self.focus_outside_the_ring(monitor, workspace) {
            return Ok(Changes::none());
        }
        let delta = sizing.signed(self.resize_delta);
        let Some(work_area) = self.work_area_for(monitor, workspace) else {
            return Err(Error::MonitorNotFound(monitor));
        };
        let workspace_padding = self.default_workspace_padding;
        let container_padding = self.default_container_padding;
        let scale = self.padding_scale(monitor);

        let target = self.workspace_mut(monitor, workspace)?;
        if target.containers().is_empty() {
            return Err(Error::NoFocusedContainer);
        }
        let idx = target.focused_container_idx();

        target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
        let original = target.resize_dimension(idx);
        let before = target.latest_layout().get(idx).copied();

        // Right and down are the far edges of their axis; left and up are the
        // near ones, which `nudged` already counts the other way round so that
        // `increase` always grows the container.
        let (axis, far_edge) = match direction {
            Direction::Left => (Axis::Horizontal, false),
            Direction::Right => (Axis::Horizontal, true),
            Direction::Up => (Axis::Vertical, false),
            Direction::Down => (Axis::Vertical, true),
        };
        let next = Self::nudged(original.unwrap_or_default(), axis, far_edge, delta);
        target.set_resize_dimension(idx, Some(next));
        target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
        if target.latest_layout().get(idx).copied() != before {
            // Stored as asked for rather than as granted, for the reason
            // `resize_axis` spells out: anything else drifts under a held key.
            return Ok(Changes::none().retile(monitor, workspace));
        }

        // That edge is no boundary, or the clamp refused. Either way there is
        // no second edge to fall back to: this command was given one.
        target.set_resize_dimension(idx, original);
        target.update_layout_scaled(work_area, workspace_padding, container_padding, scale);
        Ok(Changes::none())
    }

    /// One resize delta with `by` pixels added to the edge the command is
    /// working on. The near edge counts the other way round, because pulling
    /// it back is what makes the container grow.
    fn nudged(
        mut delta: crate::geometry::Rect,
        axis: Axis,
        far_edge: bool,
        by: i32,
    ) -> crate::geometry::Rect {
        match (axis, far_edge) {
            (Axis::Horizontal, true) => delta.right += by,
            (Axis::Horizontal, false) => delta.left -= by,
            (Axis::Vertical, true) => delta.bottom += by,
            (Axis::Vertical, false) => delta.top -= by,
        }
        delta
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
        let monitor = self.focused_monitor_idx();
        self.check_workspace_idx_on(monitor, idx)?;
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
        let monitor = self.focused_monitor_idx();
        self.check_workspace_idx_on(monitor, idx)?;
        self.focused_monitor_mut()?.ensure_workspaces(idx + 1);
        self.move_focused_container(monitor, idx, follow)
    }

    /// Where the workspace with this name is, searching every monitor.
    ///
    /// The search starts at the focused monitor so that a name used on more
    /// than one screen resolves to the one in front of you, which is the one
    /// you meant. Names are compared case insensitively: a name is typed at a
    /// keyboard, not parsed.
    #[must_use]
    pub fn named_workspace(&self, name: &str) -> Option<(usize, usize)> {
        let count = self.monitors().len();
        let first = self.focused_monitor_idx();
        (0..count)
            .map(|step| (first + step) % count)
            .find_map(|monitor| {
                let idx = self.monitors().get(monitor)?.workspaces().position(|w| {
                    w.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                })?;
                Some((monitor, idx))
            })
    }

    /// Focuses the workspace with this name, wherever it is.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceNameNotFound`] when no workspace carries it.
    pub fn focus_named_workspace(&mut self, name: &str) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self
            .named_workspace(name)
            .ok_or_else(|| Error::WorkspaceNameNotFound(name.to_string()))?;
        let mut changes = self.focus_monitor(monitor)?;
        changes.merge(self.focus_workspace(workspace)?);
        Ok(changes)
    }

    /// Moves the focused container to the workspace with this name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WorkspaceNameNotFound`] when no workspace carries it.
    pub fn move_to_named_workspace(&mut self, name: &str, follow: bool) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (monitor, workspace) = self
            .named_workspace(name)
            .ok_or_else(|| Error::WorkspaceNameNotFound(name.to_string()))?;
        self.move_focused_container(monitor, workspace, follow)
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
    /// Does nothing when the focused window is outside the container ring; see
    /// `focus_outside_the_ring`.
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
        if self.focus_outside_the_ring(from_monitor, from_workspace) {
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
        let decision = self.rules.decide(&window.info());
        if decision == RuleDecision::Ignore {
            return Ok(Changes::none());
        }
        if self.is_paused {
            // Nothing may move while the manager is paused, but a window that
            // is forgotten here would never be tiled at all, so it waits.
            if !self.is_managed(window.id)
                && !self.pending_windows.iter().any(|w| w.id == window.id)
            {
                self.pending_windows.push(window);
            }
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
        if let Some(idx) = self.pending_windows.iter().position(|w| w.id == id) {
            self.pending_windows.remove(idx);
            return Ok(Changes::none());
        }
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
        let was_maximized = self
            .workspace(monitor, workspace)?
            .maximized_window()
            .is_some_and(|window| window.id == id);
        if !self.workspace_mut(monitor, workspace)?.float_window(id) {
            return Ok(Changes::none());
        }
        let mut changes = self.retiled(monitor, workspace);
        if was_maximized {
            // Floating drops the maximized state from the model; the real
            // window stays SW_SHOWMAXIMIZED unless it is restored here, and a
            // maximized window ignores the rectangle the layout gives it.
            changes.restore.push(id);
        }
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// Drops a tiled window at a point on the virtual desktop.
    ///
    /// This is the model half of drag and drop: the daemon notices the user
    /// let go of a managed window, hands the cursor position over, and applies
    /// the returned [`Changes`]. The retile in them is what snaps the dragged
    /// window back into a tile, so a drop that lands nowhere useful still
    /// returns one.
    ///
    /// - Dropped on another container of the same monitor, the two containers
    ///   swap places, deltas and all, and the dragged one keeps the focus.
    /// - Dropped on its own tile, or on nothing at all, only the retile comes
    ///   back and the window snaps home.
    /// - Dropped on another monitor, the container moves to the focused
    ///   workspace of that monitor and is inserted at the container it landed
    ///   on, or appended when it landed on free space. The destination monitor
    ///   and its new container take the focus, since that is where the cursor
    ///   now is.
    /// - A floating window is left where the user dropped it: nothing moves
    ///   and the change set is empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WindowNotFound`] when the window is not managed.
    pub fn swap_window_at_point(&mut self, window: WindowId, x: i32, y: i32) -> Result<Changes> {
        if self.is_paused {
            return Ok(Changes::none());
        }
        let (from_monitor, from_workspace) = self
            .locate_window(window)
            .ok_or(Error::WindowNotFound(window))?;
        let Some(from_container) = self
            .workspace(from_monitor, from_workspace)?
            .container_idx_for_window(window)
        else {
            // A floating window was never in a tile, so there is nothing to
            // swap it with and nothing to snap it back to.
            return Ok(Changes::none());
        };

        let snap_back = self.retiled(from_monitor, from_workspace);
        let Some(to_monitor) = self.monitor_idx_at(x, y) else {
            return Ok(snap_back);
        };
        let hit = self.container_at_point(to_monitor, x, y);

        if to_monitor == from_monitor {
            // Only a drop on the workspace the window is on can swap: the
            // hit test ran against the focused workspace of this monitor.
            let focused_workspace = self
                .monitors()
                .get(to_monitor)
                .map_or(from_workspace, |m| m.focused_workspace_idx());
            let (Some(target), true) = (hit, focused_workspace == from_workspace) else {
                return Ok(snap_back);
            };
            if target == from_container {
                return Ok(snap_back);
            }
            let workspace = self.workspace_mut(from_monitor, from_workspace)?;
            workspace.focus_container(from_container);
            if !workspace.swap_focused_container(target) {
                return Ok(snap_back);
            }
            return Ok(self.retiled(from_monitor, from_workspace));
        }

        let to_workspace = self
            .monitors()
            .get(to_monitor)
            .ok_or(Error::MonitorNotFound(to_monitor))?
            .focused_workspace_idx();

        let before = self.visible_window_ids();
        let source = self.workspace_mut(from_monitor, from_workspace)?;
        source.focus_container(from_container);
        let Some(container) = source.remove_focused_container() else {
            return Ok(snap_back);
        };

        let destination = self.workspace_mut(to_monitor, to_workspace)?;
        let at = hit
            .unwrap_or(usize::MAX)
            .min(destination.containers().len());
        destination.insert_container(at, container);

        let mut changes = self.retiled(from_monitor, from_workspace);
        changes.merge(self.retiled(to_monitor, to_workspace));
        self.monitors_mut().focus(to_monitor);
        changes.focused_monitor_changed = true;
        self.visibility_delta(&before, &mut changes);
        Ok(changes)
    }

    /// `true` when two monitors are the same display.
    ///
    /// The device id is the only identifier that survives a reboot, a cable
    /// swap or a resolution change, so it decides when both sides have one.
    /// The friendly name is the fallback, and the platform handle the last
    /// resort for a monitor that was built without either.
    fn same_display(a: &Monitor, b: &Monitor) -> bool {
        if !a.device_id.is_empty() && !b.device_id.is_empty() {
            return a.device_id == b.device_id;
        }
        if !a.name.is_empty() && !b.name.is_empty() {
            return a.name == b.name;
        }
        a.id == b.id
    }

    /// Rebuilds the monitor ring from the displays the platform reports now.
    ///
    /// The daemon calls this once per display change event, with the monitors
    /// it just enumerated, and applies the returned [`Changes`] in one go:
    /// they carry the hide and show lists as well as the retiles, because a
    /// display going away moves whole workspaces around.
    ///
    /// - A display that is still there keeps its workspaces, its windows, its
    ///   focus and its own offsets; only the geometry, the DPI, the platform
    ///   handle and the identity strings are refreshed from the new value.
    /// - A display that is gone hands its workspaces to the first remaining
    ///   monitor: workspace one to workspace one, workspace two to workspace
    ///   two, created when the survivor does not have that many. Floating
    ///   windows follow their workspace, so nothing is ever lost.
    /// - A display that is new arrives with as many empty workspaces as the
    ///   monitors that were already there have, or one.
    /// - The ring ends up in the order the platform gave, and the focus stays
    ///   on the same physical display when it survived, otherwise it falls
    ///   back to the first one.
    ///
    /// An empty list is ignored: a moment without any display, which happens
    /// while a machine wakes up, is no reason to throw the model away.
    pub fn reconcile_monitors(&mut self, incoming: Vec<Monitor>) -> Changes {
        if incoming.is_empty() {
            return Changes::none();
        }

        let before = self.visible_window_ids();
        let focused_before = self.focused_monitor_idx();
        let mut old: Vec<Option<Monitor>> = std::mem::replace(self.monitors_mut(), Ring::new())
            .into_vec()
            .into_iter()
            .map(Some)
            .collect();
        let default_workspaces = old
            .iter()
            .flatten()
            .map(|m| m.workspaces().len())
            .max()
            .unwrap_or(1)
            .max(1);

        let mut ring: Vec<Monitor> = Vec::with_capacity(incoming.len());
        let mut consumed: Vec<Option<usize>> = Vec::with_capacity(incoming.len());

        for mut monitor in incoming {
            let found = old.iter().position(|slot| {
                slot.as_ref()
                    .is_some_and(|m| Self::same_display(m, &monitor))
            });
            match found.and_then(|idx| old[idx].take().map(|m| (idx, m))) {
                Some((idx, mut kept)) => {
                    kept.id = monitor.id;
                    kept.size = monitor.size;
                    kept.work_area = monitor.work_area;
                    kept.dpi = monitor.dpi;
                    if !monitor.name.is_empty() {
                        kept.name = monitor.name;
                    }
                    if !monitor.device.is_empty() {
                        kept.device = monitor.device;
                    }
                    if !monitor.device_id.is_empty() {
                        kept.device_id = monitor.device_id;
                    }
                    ring.push(kept);
                    consumed.push(Some(idx));
                }
                None => {
                    monitor.ensure_workspaces(default_workspaces);
                    ring.push(monitor);
                    consumed.push(None);
                }
            }
        }

        // Whatever nobody claimed is unplugged. Its windows move onto the
        // first monitor that is left, workspace by workspace.
        let vanished: Vec<Monitor> = old.into_iter().flatten().collect();
        if let Some(survivor) = ring.first_mut() {
            for mut gone in vanished {
                let workspaces = std::mem::replace(gone.workspaces_mut(), Ring::new()).into_vec();
                for (idx, workspace) in workspaces.into_iter().enumerate() {
                    survivor.ensure_workspaces(idx + 1);
                    if let Some(target) = survivor.workspaces_mut().get_mut(idx) {
                        target.absorb(workspace);
                    }
                }
            }
        }

        *self.monitors_mut() = Ring::from_vec(ring);
        let focus = consumed
            .iter()
            .position(|slot| *slot == Some(focused_before))
            .unwrap_or(0);
        self.monitors_mut().focus_clamped(focus);

        let mut changes = Changes::none();
        for monitor in 0..self.monitors().len() {
            let Some(workspace) = self
                .monitors()
                .get(monitor)
                .map(Monitor::focused_workspace_idx)
            else {
                continue;
            };
            changes.merge(self.retiled(monitor, workspace));
        }
        changes.focused_monitor_changed = true;
        self.visibility_delta(&before, &mut changes);
        changes.show.extend(self.visible_window_ids());
        changes.settle();
        changes
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
    use crate::model::HidingBehaviour;
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
        assert!(
            changes
                .retiled
                .iter()
                .any(|target| target.monitor == 0 && target.workspace == 0),
            "the window coming forward is still on the tile it had before the stack"
        );
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
    fn stacking_the_whole_workspace_keeps_the_window_you_were_looking_at() {
        let mut state = with_windows(4);
        state.focus_window(WindowId(2)).unwrap();

        let changes = state.stack_all().unwrap();
        let workspace = state.workspace(0, 0).unwrap();
        assert_eq!(workspace.containers().len(), 1);
        assert_eq!(workspace.containers().get(0).unwrap().len(), 4);
        // The point of the command: one window on screen, and it is the one
        // that was focused rather than whichever happened to be first.
        assert_eq!(focused(&state), Some(WindowId(2)));
        for id in [1, 3, 4] {
            assert!(changes.hide.contains(&WindowId(id)), "{id} stayed visible");
        }

        // And back, with the same window still in hand.
        let changes = state.unstack_all().unwrap();
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 4);
        assert_eq!(focused(&state), Some(WindowId(2)));
        for id in [1, 3, 4] {
            assert!(changes.show.contains(&WindowId(id)), "{id} stayed hidden");
        }
    }

    #[test]
    fn unstacking_everything_keeps_the_order_the_windows_were_stacked_in() {
        let mut state = with_windows(3);
        state.stack_all().unwrap();
        state.unstack_all().unwrap();

        let order: Vec<_> = state
            .workspace(0, 0)
            .unwrap()
            .containers()
            .iter()
            .map(|c| c.focused_window_id().unwrap())
            .collect();
        assert_eq!(order, vec![WindowId(1), WindowId(2), WindowId(3)]);
    }

    #[test]
    fn stacking_a_workspace_that_is_already_one_container_changes_nothing() {
        let mut state = with_windows(1);
        assert!(state.stack_all().unwrap().is_empty());
        // Nothing is stacked, so there is nothing to take apart either.
        assert!(state.unstack_all().unwrap().is_empty());
    }

    // -- named workspaces ---------------------------------------------------

    #[test]
    fn a_workspace_is_found_by_its_name_whatever_the_case() {
        let mut state = state();
        state
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .workspaces_mut()
            .get_mut(3)
            .unwrap()
            .name = Some("Code".to_string());

        assert_eq!(state.named_workspace("code"), Some((0, 3)));
        assert_eq!(state.named_workspace("CODE"), Some((0, 3)));
        assert_eq!(state.named_workspace("codex"), None);

        state.focus_named_workspace("code").unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 3));
    }

    #[test]
    fn a_name_used_on_both_screens_resolves_to_the_one_in_front_of_you() {
        let mut state = state();
        for monitor in 0..2 {
            state
                .monitors_mut()
                .get_mut(monitor)
                .unwrap()
                .workspaces_mut()
                .get_mut(2)
                .unwrap()
                .name = Some("mail".to_string());
        }

        state.focus_monitor(1).unwrap();
        // Not monitor 0, which is what a plain search from the front would
        // have found: the workspace you meant is the one on the screen you are
        // looking at.
        assert_eq!(state.named_workspace("mail"), Some((1, 2)));
    }

    #[test]
    fn a_name_nothing_carries_is_an_error_rather_than_a_silent_no_op() {
        let mut state = with_windows(2);
        assert!(matches!(
            state.focus_named_workspace("nowhere"),
            Err(Error::WorkspaceNameNotFound(name)) if name == "nowhere"
        ));
        assert!(state.move_to_named_workspace("nowhere", true).is_err());
    }

    #[test]
    fn a_window_can_be_sent_to_a_named_workspace_without_following_it() {
        let mut state = with_windows(2);
        state
            .monitors_mut()
            .get_mut(1)
            .unwrap()
            .workspaces_mut()
            .get_mut(4)
            .unwrap()
            .name = Some("media".to_string());

        state.move_to_named_workspace("media", false).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 0));
        assert_eq!(state.workspace(1, 4).unwrap().containers().len(), 1);
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
    fn resizing_one_edge_moves_that_edge_and_no_other() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(2)).unwrap();
        state
            .resize_edge(Direction::Left, Sizing::Increase)
            .unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(910, 0, 1920, 1080)),
            "the left edge moved out by one step"
        );

        state
            .resize_edge(Direction::Left, Sizing::Decrease)
            .unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(960, 0, 1920, 1080))
        );
    }

    #[test]
    fn resizing_an_edge_that_is_the_screen_edge_does_nothing() {
        // The whole point of naming the edge: `resize_axis` would quietly
        // move the other one instead, which on a per edge binding is the key
        // doing something the user did not press.
        let mut state = with_windows(2);
        state.focus_window(WindowId(2)).unwrap();
        let before = state.rect_for_window(WindowId(2));
        let changes = state
            .resize_edge(Direction::Right, Sizing::Increase)
            .unwrap();
        assert!(changes.is_empty());
        assert_eq!(state.rect_for_window(WindowId(2)), before);
        assert_eq!(state.workspace(0, 0).unwrap().resize_dimension(1), None);
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
    fn a_saturated_resize_steps_back_by_at_most_one_delta_and_then_by_a_full_one() {
        // Twenty presses run into the clamp long before the twentieth. The
        // press that reached the clamp only got part of a step, so the first
        // press back hands that part back: less than a full delta, but never
        // nothing. Every press after that is a full step.
        //
        // This used to be a full step straight away, because the press that
        // reached the clamp stored what the boundary really moved instead of
        // the step it asked for. That is what made the same number of presses
        // each way drift; see `resizing_is_reversible_and_bounded`.
        for axis in [Axis::Horizontal, Axis::Vertical] {
            for sizing in [Sizing::Increase, Sizing::Decrease] {
                let mut state = with_windows(3);
                state.focus_window(WindowId(2)).unwrap();
                for _ in 0..20 {
                    state.resize_axis(axis, sizing).unwrap();
                }
                let saturated = state.rect_for_window(WindowId(2)).unwrap();

                state.resize_axis(axis, sizing.opposite()).unwrap();
                let first = state.rect_for_window(WindowId(2)).unwrap();
                let back = (saturated.extent(axis) - first.extent(axis)).abs();
                assert!(
                    back > 0 && back <= state.resize_delta,
                    "{axis:?} {sizing:?} stepped back by {back}, not by at most one delta"
                );

                state.resize_axis(axis, sizing.opposite()).unwrap();
                let second = state.rect_for_window(WindowId(2)).unwrap();
                assert_eq!(
                    (first.extent(axis) - second.extent(axis)).abs(),
                    state.resize_delta,
                    "{axis:?} {sizing:?} did not give a full second step back"
                );
            }
        }
    }

    #[test]
    fn a_clamped_resize_stores_the_step_it_asked_for() {
        let mut state = with_windows(2);
        state.focus_window(WindowId(1)).unwrap();
        for _ in 0..40 {
            state
                .resize_axis(Axis::Horizontal, Sizing::Increase)
                .unwrap();
        }
        let stored = state.workspace(0, 0).unwrap().resize_dimension(0).unwrap();
        let achieved = state.rect_for_window(WindowId(1)).unwrap().width() - 960;
        assert!(
            stored.right >= achieved,
            "the stored delta is the step that was asked for, not the part of it \
             the clamp granted: {} against {achieved}",
            stored.right
        );
        // The presses after the clamp move nothing at all and are not stored
        // at all, so the delta stays within one step of what the layout can do
        // with it and one press back is always visible.
        assert!(stored.right < achieved + state.resize_delta);
        assert!(stored.right < 40 * state.resize_delta);
    }

    #[test]
    fn no_tile_is_ever_resized_below_the_minimum_tile_size() {
        let mut state = with_windows(4);
        for idx in 1..=4 {
            state.focus_window(WindowId(idx)).unwrap();
            for axis in [Axis::Horizontal, Axis::Vertical] {
                for _ in 0..50 {
                    state.resize_axis(axis, Sizing::Decrease).unwrap();
                    state.resize_axis(axis, Sizing::Increase).unwrap();
                }
            }
        }
        for rect in state.workspace(0, 0).unwrap().latest_layout() {
            assert!(
                rect.width() >= crate::layout::MIN_TILE_SIZE
                    && rect.height() >= crate::layout::MIN_TILE_SIZE,
                "{rect:?} is smaller than the minimum tile"
            );
        }
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
    fn a_configured_workspace_past_the_creation_cap_still_works() {
        let mut state = with_windows(1);
        state
            .monitors_mut()
            .get_mut(0)
            .unwrap()
            .ensure_workspaces(12);

        state.focus_workspace(11).unwrap();
        assert_eq!(state.focused_indices().unwrap(), (0, 11));

        state.focus_workspace(0).unwrap();
        state.move_to_workspace(11, true).unwrap();
        assert_eq!(state.locate_window(WindowId(1)), Some((0, 11)));

        // Twelve workspaces exist, a thirteenth is still out of range, and the
        // monitor that was left at nine workspaces keeps the old cap.
        assert_eq!(
            state.focus_workspace(12).unwrap_err(),
            Error::WorkspaceIndexOutOfRange(12)
        );
        assert!(state.check_workspace_idx_on(0, 11).is_ok());
        assert!(state.check_workspace_idx_on(1, 11).is_err());
        assert!(State::check_workspace_idx(11).is_err());
    }

    #[test]
    fn a_window_that_appears_while_paused_waits_instead_of_being_dropped() {
        let mut state = with_windows(1);
        state.toggle_pause().unwrap();

        let changes = state.add_window(Window::new(2)).unwrap();
        assert!(
            changes.is_empty(),
            "nothing moves while the manager is paused"
        );
        assert!(!state.is_managed(WindowId(2)));
        assert_eq!(state.pending_windows.len(), 1);

        // A second event for the same window does not queue it twice.
        state.add_window(Window::new(2)).unwrap();
        assert_eq!(state.pending_windows.len(), 1);

        state.toggle_pause().unwrap();
        assert!(state.pending_windows.is_empty());
        assert!(state.is_managed(WindowId(2)));
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(960, 0, 1920, 1080))
        );
    }

    #[test]
    fn retile_picks_up_the_windows_that_waited() {
        let mut state = with_windows(1);
        state.toggle_pause().unwrap();
        state.add_window(Window::new(2)).unwrap();
        state.add_window(Window::new(3)).unwrap();
        assert!(state.retile().unwrap().is_empty(), "still paused");

        state.is_paused = false;
        let changes = state.retile().unwrap();
        assert!(state.is_managed(WindowId(2)) && state.is_managed(WindowId(3)));
        assert!(changes.show.contains(&WindowId(3)));
        assert_eq!(state.workspace(0, 0).unwrap().containers().len(), 3);
    }

    #[test]
    fn a_window_that_closes_again_while_paused_is_forgotten() {
        let mut state = with_windows(1);
        state.toggle_pause().unwrap();
        state.add_window(Window::new(2)).unwrap();
        assert!(state.remove_window(WindowId(2)).unwrap().is_empty());
        assert!(state.pending_windows.is_empty());

        state.toggle_pause().unwrap();
        assert!(!state.is_managed(WindowId(2)));
    }

    #[test]
    fn an_ignored_window_is_not_queued_while_paused() {
        let mut state = with_windows(1);
        state.rules.ignore_rules.push(MatchingRule::simple(
            ApplicationIdentifier::Exe,
            "ignored.exe",
            MatchingStrategy::Equals,
        ));
        state.toggle_pause().unwrap();
        state
            .add_window(Window::new(2).with_exe("ignored.exe"))
            .unwrap();
        assert!(state.pending_windows.is_empty());
    }

    #[test]
    fn the_paddings_scale_with_the_monitor_dpi() {
        // The real pair: the 4K panel at 150 percent and the portrait one at
        // 100 percent. Fourteen logical pixels are 21 physical on the 4K.
        let mut state = State::new();
        let main_area = Rect::new(0, 0, 3840, 2160);
        let side_area = Rect::new(3840, 0, 4920, 1920);
        state.add_monitor(Monitor::new(1, main_area, main_area).with_dpi(144));
        state.add_monitor(Monitor::new(2, side_area, side_area).with_dpi(96));
        state.default_workspace_padding = 14;
        state.default_container_padding = 0;

        assert!((state.padding_scale(0) - 1.5).abs() < f32::EPSILON);
        assert!((state.padding_scale(1) - 1.0).abs() < f32::EPSILON);

        state.add_window(Window::new(1)).unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(21, 21, 3819, 2139)),
            "14 logical pixels are 21 physical ones at 150 percent"
        );

        state.focus_monitor(1).unwrap();
        state.add_window(Window::new(2)).unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(2)),
            Some(Rect::new(3854, 14, 4906, 1906)),
            "the portrait panel is at 100 percent, so the padding is literal"
        );

        state.scale_padding_with_dpi = false;
        state.retile().unwrap();
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(14, 14, 3826, 2146))
        );
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

    // -- drop swap ----------------------------------------------------------

    fn container_ids(state: &State, monitor: usize, workspace: usize) -> Vec<WindowId> {
        state
            .workspace(monitor, workspace)
            .unwrap()
            .containers()
            .iter()
            .filter_map(Container::focused_window_id)
            .collect()
    }

    #[test]
    fn a_point_hit_tests_the_tiles_of_the_focused_workspace() {
        let state = with_windows(2);
        assert_eq!(state.container_at_point(0, 10, 10), Some(0));
        assert_eq!(state.container_at_point(0, 1000, 500), Some(1));
        assert_eq!(state.container_at_point(0, 5000, 10), None, "off the tiles");
        assert_eq!(state.container_at_point(1, 2000, 10), None, "no containers");
        assert_eq!(state.container_at_point(9, 10, 10), None, "no monitor");
    }

    #[test]
    fn dropping_a_window_on_another_tile_swaps_the_two_containers() {
        let mut state = with_windows(2);
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);

        let changes = state.swap_window_at_point(WindowId(1), 1000, 500).unwrap();

        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(2), WindowId(1)]);
        assert_eq!(changes.retiled, vec![crate::model::WorkspaceRef::new(0, 0)]);
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(960, 0, 1920, 1080)),
            "the dragged window took the tile it was dropped on"
        );
        assert_eq!(
            state.workspace(0, 0).unwrap().focused_container_idx(),
            1,
            "the focus follows the dragged container"
        );
    }

    #[test]
    fn dropping_a_window_on_its_own_tile_only_retiles() {
        let mut state = with_windows(3);
        let before = container_ids(&state, 0, 0);
        let rect = state.rect_for_window(WindowId(1)).unwrap();

        let changes = state
            .swap_window_at_point(WindowId(1), rect.left + 5, rect.top + 5)
            .unwrap();

        assert_eq!(container_ids(&state, 0, 0), before, "nothing moved");
        assert_eq!(changes.retiled, vec![crate::model::WorkspaceRef::new(0, 0)]);
        assert!(changes.show.is_empty() && changes.hide.is_empty());
    }

    #[test]
    fn dropping_a_window_off_every_monitor_snaps_it_back() {
        let mut state = with_windows(2);
        let before = container_ids(&state, 0, 0);

        let changes = state.swap_window_at_point(WindowId(1), -500, -500).unwrap();

        assert_eq!(container_ids(&state, 0, 0), before);
        assert_eq!(changes.retiled, vec![crate::model::WorkspaceRef::new(0, 0)]);
    }

    #[test]
    fn dropping_a_window_on_free_space_of_its_own_monitor_snaps_it_back() {
        let mut state = state();
        state.default_workspace_padding = 200;
        state.add_window(Window::new(1)).unwrap();
        let before = container_ids(&state, 0, 0);

        // The workspace padding leaves a margin that belongs to no tile.
        let changes = state.swap_window_at_point(WindowId(1), 5, 5).unwrap();

        assert_eq!(container_ids(&state, 0, 0), before);
        assert_eq!(changes.retiled, vec![crate::model::WorkspaceRef::new(0, 0)]);
    }

    #[test]
    fn dropping_a_window_on_another_monitor_moves_it_there() {
        let mut state = with_windows(2);
        state.add_window_to(1, 0, Window::new(5)).unwrap();
        state.add_window_to(1, 0, Window::new(6)).unwrap();
        let target = state.rect_for_window(WindowId(5)).unwrap();

        let changes = state
            .swap_window_at_point(WindowId(1), target.left + 5, target.top + 5)
            .unwrap();

        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(2)]);
        assert_eq!(
            container_ids(&state, 1, 0),
            vec![WindowId(1), WindowId(5), WindowId(6)],
            "inserted at the container it was dropped on"
        );
        assert!(changes.focused_monitor_changed);
        assert_eq!(state.focused_monitor_idx(), 1);
        assert!(state.visible_window_ids().contains(&WindowId(1)));
        assert_eq!(changes.retiled.len(), 2);
    }

    #[test]
    fn dropping_a_window_on_the_free_space_of_another_monitor_appends_it() {
        let mut state = with_windows(2);
        state.add_window_to(1, 0, Window::new(5)).unwrap();
        state.default_workspace_padding = 400;
        state.retile().unwrap();

        let changes = state.swap_window_at_point(WindowId(1), 1925, 5).unwrap();

        assert_eq!(
            container_ids(&state, 1, 0),
            vec![WindowId(5), WindowId(1)],
            "a drop on free space goes to the back"
        );
        assert!(!changes.retiled.is_empty());
    }

    #[test]
    fn dropping_a_floating_window_changes_nothing() {
        let mut state = with_windows(2);
        state.float_window(WindowId(1)).unwrap();
        let changes = state.swap_window_at_point(WindowId(1), 1000, 500).unwrap();
        assert!(changes.is_empty());
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(2)]);
    }

    #[test]
    fn dropping_an_unmanaged_window_is_an_error_and_a_paused_drop_does_nothing() {
        let mut state = with_windows(2);
        assert_eq!(
            state
                .swap_window_at_point(WindowId(99), 10, 10)
                .unwrap_err(),
            Error::WindowNotFound(WindowId(99))
        );
        state.is_paused = true;
        assert!(
            state
                .swap_window_at_point(WindowId(1), 1000, 500)
                .unwrap()
                .is_empty()
        );
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);
    }

    // -- monitor reconciliation ---------------------------------------------

    fn display(id: isize, name: &str, rect: Rect) -> Monitor {
        Monitor::new(id, rect, rect).with_name(name)
    }

    #[test]
    fn reconciling_an_added_monitor_keeps_everything_and_gives_it_workspaces() {
        let mut state = with_windows(2);
        let third = Rect::new(3000, 0, 4920, 1080);
        let changes = state.reconcile_monitors(vec![
            display(1, "DISPLAY1", MAIN),
            display(2, "DISPLAY2", SIDE),
            display(3, "DISPLAY3", third),
        ]);

        assert_eq!(state.monitors().len(), 3);
        assert_eq!(
            state.monitors().get(2).unwrap().workspaces().len(),
            9,
            "as many as the monitors that were already there"
        );
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);
        assert_eq!(state.monitors().get(2).unwrap().size, third);
        assert_eq!(changes.retiled.len(), 3);
        assert!(changes.focused_monitor_changed);
    }

    #[test]
    fn reconciling_updates_the_geometry_of_a_display_that_stayed() {
        let mut state = with_windows(2);
        let resized = Rect::new(0, 0, 2560, 1440);
        state.reconcile_monitors(vec![
            display(11, "DISPLAY1", resized).with_dpi(144),
            display(2, "DISPLAY2", SIDE),
        ]);

        let main = state.monitors().get(0).unwrap();
        assert_eq!(main.size, resized);
        assert_eq!(main.work_area, resized);
        assert_eq!(main.dpi, 144);
        assert_eq!(main.id, 11, "the fresh platform handle wins");
        assert_eq!(main.workspaces().len(), 9, "the workspaces survived");
        assert_eq!(
            state.rect_for_window(WindowId(1)),
            Some(Rect::new(0, 0, 1280, 1440)),
            "and the layout followed the new size"
        );
    }

    #[test]
    fn reconciling_a_monitor_away_rescues_its_windows() {
        let mut state = with_windows(2);
        state.add_window_to(1, 0, Window::new(5)).unwrap();
        state.add_window_to(1, 0, Window::new(7)).unwrap();
        state.float_window(WindowId(7)).unwrap();
        state.add_window_to(1, 3, Window::new(6)).unwrap();

        let changes = state.reconcile_monitors(vec![display(1, "DISPLAY1", MAIN)]);

        assert_eq!(state.monitors().len(), 1);
        assert_eq!(
            container_ids(&state, 0, 0),
            vec![WindowId(1), WindowId(2), WindowId(5)],
            "appended to the same-index workspace"
        );
        assert_eq!(
            state
                .workspace(0, 0)
                .unwrap()
                .floating_windows()
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![WindowId(7)],
            "floating windows follow their workspace"
        );
        assert_eq!(container_ids(&state, 0, 3), vec![WindowId(6)]);
        assert_eq!(state.all_window_ids().count(), 5, "nothing was lost");

        assert!(state.visible_window_ids().contains(&WindowId(5)));
        assert!(
            changes.show.contains(&WindowId(5)),
            "and it is put on screen"
        );
        assert!(!changes.hide.contains(&WindowId(5)));
        assert!(
            !state.visible_window_ids().contains(&WindowId(6)),
            "the one on workspace four stays where it was"
        );
        assert!(state.rect_for_window(WindowId(5)).is_some());
        assert_eq!(state.focused_monitor_idx(), 0);
    }

    #[test]
    fn reconciling_a_new_order_reorders_the_ring_and_keeps_the_focused_display() {
        let mut state = with_windows(2);
        state.focus_monitor(1).unwrap();

        let changes = state.reconcile_monitors(vec![
            display(7, "DISPLAY2", SIDE),
            display(8, "DISPLAY1", MAIN),
        ]);

        assert_eq!(state.monitors().get(0).unwrap().name, "DISPLAY2");
        assert_eq!(state.monitors().get(1).unwrap().name, "DISPLAY1");
        assert_eq!(
            state.focused_monitor_idx(),
            0,
            "the focus stayed on the same display, which moved"
        );
        assert_eq!(container_ids(&state, 1, 0), vec![WindowId(1), WindowId(2)]);
        assert!(changes.focused_monitor_changed);
    }

    #[test]
    fn reconciling_matches_by_device_id_before_the_name() {
        let mut state = State::new();
        state.default_workspace_padding = 0;
        state.default_container_padding = 0;
        state.add_monitor(
            Monitor::new(1, MAIN, MAIN)
                .with_name("DISPLAY1")
                .with_device(r"\.\DISPLAY1", "G8-12345"),
        );
        state.add_window(Window::new(1)).unwrap();

        // Windows renumbered the friendly names after a reboot.
        state.reconcile_monitors(vec![
            Monitor::new(4, MAIN, MAIN)
                .with_name("DISPLAY2")
                .with_device(r"\.\DISPLAY2", "G8-12345"),
        ]);

        assert_eq!(state.monitors().len(), 1);
        assert_eq!(state.monitors().get(0).unwrap().name, "DISPLAY2");
        assert_eq!(
            container_ids(&state, 0, 0),
            vec![WindowId(1)],
            "the same panel kept its windows"
        );
    }

    #[test]
    fn reconciling_with_no_displays_at_all_is_ignored() {
        let mut state = with_windows(2);
        let changes = state.reconcile_monitors(Vec::new());
        assert!(changes.is_empty());
        assert_eq!(state.monitors().len(), 2);
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);
    }

    // -- regressions --------------------------------------------------------

    #[test]
    fn minimizing_the_visible_window_of_a_stack_shows_the_one_underneath() {
        let mut state = with_windows(3);
        state.stack(Direction::Left).unwrap();
        assert_eq!(state.visible_window_ids(), vec![WindowId(3), WindowId(2)]);

        let changes = state.minimize_focused_window().unwrap();

        assert_eq!(changes.minimize, vec![WindowId(3)]);
        assert_eq!(
            changes.show,
            vec![WindowId(1)],
            "the window the stack now shows is still cloaked: {changes:?}"
        );
        assert_eq!(state.visible_window_ids(), vec![WindowId(1), WindowId(2)]);
    }

    #[test]
    fn minimizing_a_maximized_window_takes_it_out_of_the_maximized_state() {
        let mut state = with_windows(2);
        state.toggle_maximize().unwrap();

        let changes = state.minimize_focused_window().unwrap();

        assert_eq!(changes.minimize, vec![WindowId(2)]);
        assert_eq!(
            changes.restore,
            vec![WindowId(2)],
            "the real window is still SW_SHOWMAXIMIZED: {changes:?}"
        );
        assert!(!state.workspace(0, 0).unwrap().is_maximized());
    }

    #[test]
    fn floating_a_maximized_window_takes_it_out_of_the_maximized_state() {
        let mut state = with_windows(2);
        state.toggle_maximize().unwrap();

        let changes = state.float_window(WindowId(2)).unwrap();

        assert_eq!(
            changes.restore,
            vec![WindowId(2)],
            "the real window is still SW_SHOWMAXIMIZED: {changes:?}"
        );
        assert!(!state.workspace(0, 0).unwrap().is_maximized());
        assert_eq!(state.workspace(0, 0).unwrap().floating_windows().len(), 1);
    }

    #[test]
    fn focusing_a_window_while_paused_leaves_the_model_alone() {
        let mut state = with_windows(1);
        state.focus_workspace(1).unwrap();
        state.add_window(Window::new(2)).unwrap();
        state.toggle_pause().unwrap();

        let changes = state.focus_window(WindowId(1)).unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(
            state.focused_indices().unwrap(),
            (0, 1),
            "a paused manager keeps the workspace it had"
        );
    }

    #[test]
    fn moving_a_floating_window_to_another_monitor_leaves_the_tiled_ones_alone() {
        let mut state = with_windows(2);
        state.toggle_float().unwrap();

        let changes = state.move_to_monitor(1, true).unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1)]);
        assert!(state.workspace(1, 0).unwrap().is_empty());
        assert_eq!(focused(&state), Some(WindowId(2)));
    }

    #[test]
    fn moving_a_monocled_window_to_another_workspace_leaves_the_ring_alone() {
        let mut state = with_windows(2);
        state.toggle_monocle().unwrap();

        let changes = state.move_to_workspace(1, false).unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert!(state.workspace(0, 1).unwrap().is_empty());
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1)]);
    }

    #[test]
    fn promoting_while_a_floating_window_is_focused_moves_nothing() {
        let mut state = with_windows(3);
        state.toggle_float().unwrap();

        let changes = state.promote().unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);
    }

    #[test]
    fn resizing_while_a_floating_window_is_focused_leaves_the_tiles_alone() {
        let mut state = with_windows(3);
        state.toggle_float().unwrap();

        let changes = state
            .resize_axis(Axis::Horizontal, Sizing::Increase)
            .unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(state.workspace(0, 0).unwrap().resize_dimension(0), None);
    }

    #[test]
    fn stacking_while_a_floating_window_is_focused_leaves_the_stacks_alone() {
        let mut state = with_windows(3);
        state.toggle_float().unwrap();

        let changes = state.stack(Direction::Left).unwrap();

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(container_ids(&state, 0, 0), vec![WindowId(1), WindowId(2)]);
    }

    #[test]
    fn a_window_moved_behind_a_maximized_one_never_takes_the_foreground() {
        let mut state = with_windows(1);
        state.focus_monitor(1).unwrap();
        state.add_window(Window::new(2)).unwrap();
        state.toggle_maximize().unwrap();
        state.focus_monitor(0).unwrap();

        let changes = state.move_to_monitor(1, true).unwrap();

        assert!(changes.hide.contains(&WindowId(1)), "{changes:?}");
        assert_ne!(
            changes.focus,
            Some(WindowId(1)),
            "the daemon would cloak the window and then type into it: {changes:?}"
        );
    }
}
