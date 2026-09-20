//! A workspace: a ring of containers plus everything that is not tiled.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::geometry::{Direction, Rect, nearest_in_direction};
use crate::layout::{Flip, Layout, MIN_TILE_SIZE};

use super::monitor::scale_padding;

use super::container::Container;
use super::ring::{CycleDirection, Ring};
use super::window::{Window, WindowId};

/// Where a window came from, so it can be put back when a mode is toggled off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
struct Restore {
    /// The window holding the spot this one came out of: the sibling it shared
    /// a stack with, or the container it sat behind when it had a tile to
    /// itself. `None` means it was the first container in the ring.
    ///
    /// A handle and not an index on purpose. The ring keeps changing shape
    /// while a window is maximized, because windows close and containers are
    /// promoted, stacked and inserted, and every one of those shifts the
    /// indices to their right. An index taken before all that hands the window
    /// back into a stranger's stack; a handle still points at the container
    /// the user left it in.
    anchor: Option<WindowId>,
    /// The index inside that container's stack, or in the floating list.
    window: usize,
    /// `true` when the window was alone and its container went away with it.
    own_container: bool,
    /// `true` when the window came out of the floating list.
    floating: bool,
}

/// A workspace: the containers it tiles plus its floating, monocle and
/// maximized exceptions.
///
/// The container ring holds only the tiled containers. A window that is
/// floating, maximized or in monocle mode has been lifted out of that ring and
/// is stored on its own, which is what keeps the layout arithmetic simple.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Workspace {
    /// The workspace name, as configured. `None` means "show the index".
    pub name: Option<String>,
    /// The layout used when no layout rule applies.
    pub layout: Layout,
    /// Which axes the layout is mirrored on.
    pub layout_flip: Flip,
    /// Layout per container count: the rule with the highest key that is at
    /// most the current container count wins.
    pub layout_rules: BTreeMap<usize, Layout>,
    /// The tiled containers.
    containers: Ring<Container>,
    /// Windows that are managed but never tiled.
    floating_windows: Ring<Window>,
    /// `true` when the focused window is one of the floating ones.
    focus_is_floating: bool,
    /// The container that is currently filling the whole workspace, if any.
    monocle_container: Option<Container>,
    /// The window the monocled container goes back behind, or `None` when it
    /// was the first in the ring. A handle for the same reason as
    /// [`Restore::anchor`].
    monocle_restore: Option<WindowId>,
    /// The window that is currently maximized, if any.
    maximized_window: Option<Window>,
    maximized_restore: Option<Restore>,
    /// Padding around the whole workspace. `None` uses the global default.
    pub workspace_padding: Option<i32>,
    /// Padding around each container. `None` uses the global default.
    pub container_padding: Option<i32>,
    /// The width a tile of this workspace may never shrink below, in logical
    /// pixels.
    ///
    /// Carried here because the layout is computed from the workspace. The
    /// value is the global `minimum_window_width`, which
    /// [`State::workspace_mut`](super::State::workspace_mut) stamps on every
    /// time it hands a workspace out, so a workspace created on demand long
    /// after the configuration was read still tiles to the configured floor.
    pub minimum_window_width: i32,
    /// The height a tile of this workspace may never shrink below, in logical
    /// pixels. The counterpart of [`Workspace::minimum_window_width`].
    pub minimum_window_height: i32,
    /// `false` turns tiling off for this workspace without unmanaging anything.
    pub tile: bool,
    /// `true` makes every new window float.
    pub float_override: bool,
    /// `true` applies the monitor's window based work area offset here.
    pub apply_window_based_work_area_offset: bool,
    /// Per container per edge pixel deltas from the resize commands.
    ///
    /// Always the same length as the container ring after a layout update.
    pub resize_dimensions: Vec<Option<Rect>>,
    /// The rectangles from the last layout update, one per container.
    ///
    /// The daemon diffs against this to work out which windows actually have
    /// to move, and the directional focus commands read it to find neighbours.
    latest_layout: Vec<Rect>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            name: None,
            layout: Layout::default(),
            layout_flip: Flip::NONE,
            layout_rules: BTreeMap::new(),
            containers: Ring::new(),
            floating_windows: Ring::new(),
            focus_is_floating: false,
            monocle_container: None,
            monocle_restore: None,
            maximized_window: None,
            maximized_restore: None,
            workspace_padding: None,
            container_padding: None,
            minimum_window_width: MIN_TILE_SIZE,
            minimum_window_height: MIN_TILE_SIZE,
            tile: true,
            float_override: false,
            apply_window_based_work_area_offset: false,
            resize_dimensions: Vec::new(),
            latest_layout: Vec::new(),
        }
    }
}

impl Workspace {
    /// An empty workspace with the default layout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty workspace with a name.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }

    // -- containers ---------------------------------------------------------

    /// The tiled containers.
    #[must_use]
    pub fn containers(&self) -> &Ring<Container> {
        &self.containers
    }

    /// The tiled containers, mutably.
    ///
    /// Reordering through this bypasses the resize deltas, so prefer the named
    /// operations where one exists.
    pub fn containers_mut(&mut self) -> &mut Ring<Container> {
        &mut self.containers
    }

    /// The index of the focused container.
    #[must_use]
    pub fn focused_container_idx(&self) -> usize {
        self.containers.focused_idx()
    }

    /// The focused container.
    #[must_use]
    pub fn focused_container(&self) -> Option<&Container> {
        self.containers.focused()
    }

    /// The focused container, mutably.
    pub fn focused_container_mut(&mut self) -> Option<&mut Container> {
        self.containers.focused_mut()
    }

    /// Focuses a container by index. Returns `false` when out of range.
    pub fn focus_container(&mut self, idx: usize) -> bool {
        let ok = self.containers.focus(idx);
        if ok {
            self.focus_is_floating = false;
        }
        ok
    }

    /// The container holding a window, by index.
    #[must_use]
    pub fn container_idx_for_window(&self, id: WindowId) -> Option<usize> {
        self.containers.position(|c| c.contains(id))
    }

    // -- floating, monocle, maximized ---------------------------------------

    /// The windows that are managed but never tiled.
    #[must_use]
    pub fn floating_windows(&self) -> &Ring<Window> {
        &self.floating_windows
    }

    /// The floating windows, mutably.
    pub fn floating_windows_mut(&mut self) -> &mut Ring<Window> {
        &mut self.floating_windows
    }

    /// `true` when the focused window is a floating one.
    #[must_use]
    pub fn focus_is_floating(&self) -> bool {
        self.focus_is_floating && !self.floating_windows.is_empty()
    }

    /// The container that fills the whole workspace, if monocle mode is on.
    #[must_use]
    pub fn monocle_container(&self) -> Option<&Container> {
        self.monocle_container.as_ref()
    }

    /// `true` when monocle mode is on.
    #[must_use]
    pub fn is_monocle(&self) -> bool {
        self.monocle_container.is_some()
    }

    /// The maximized window, if there is one.
    #[must_use]
    pub fn maximized_window(&self) -> Option<&Window> {
        self.maximized_window.as_ref()
    }

    /// `true` when a window is maximized.
    #[must_use]
    pub fn is_maximized(&self) -> bool {
        self.maximized_window.is_some()
    }

    // -- membership ---------------------------------------------------------

    /// `true` when the workspace holds no window at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
            && self.floating_windows.is_empty()
            && self.monocle_container.is_none()
            && self.maximized_window.is_none()
    }

    /// Every window the workspace knows about, tiled or not.
    pub fn all_windows(&self) -> impl Iterator<Item = &Window> + '_ {
        self.containers
            .iter()
            .flat_map(|c| c.windows().iter())
            .chain(
                self.monocle_container
                    .iter()
                    .flat_map(|c| c.windows().iter()),
            )
            .chain(self.maximized_window.iter())
            .chain(self.floating_windows.iter())
    }

    /// Every window handle the workspace knows about.
    pub fn all_window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.all_windows().map(|w| w.id)
    }

    /// `true` when the window lives in this workspace.
    #[must_use]
    pub fn contains_window(&self, id: WindowId) -> bool {
        self.all_window_ids().any(|w| w == id)
    }

    /// The window with that handle, wherever it is.
    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.all_windows().find(|w| w.id == id)
    }

    /// The handles that have to be on screen when this workspace is shown.
    #[must_use]
    pub fn visible_window_ids(&self) -> Vec<WindowId> {
        let mut ids = Vec::new();
        if let Some(window) = &self.maximized_window {
            ids.push(window.id);
        } else if let Some(container) = &self.monocle_container {
            ids.extend(container.focused_window_id());
        } else {
            ids.extend(
                self.containers
                    .iter()
                    .filter_map(Container::focused_window_id),
            );
        }
        ids.extend(self.floating_windows.iter().map(|w| w.id));
        ids
    }

    /// The handles that have to be hidden while this workspace is shown: the
    /// windows buried in a stack, and everything a monocle or a maximize
    /// covers.
    #[must_use]
    pub fn hidden_window_ids(&self) -> Vec<WindowId> {
        let visible = self.visible_window_ids();
        self.all_window_ids()
            .filter(|id| !visible.contains(id))
            .collect()
    }

    // -- focus --------------------------------------------------------------

    /// The handle of the focused window, whatever mode the workspace is in.
    #[must_use]
    pub fn focused_window_id(&self) -> Option<WindowId> {
        if let Some(window) = &self.maximized_window {
            return Some(window.id);
        }
        if let Some(container) = &self.monocle_container {
            return container.focused_window_id();
        }
        if self.focus_is_floating() {
            return self.floating_windows.focused().map(|w| w.id);
        }
        self.containers
            .focused()
            .and_then(Container::focused_window_id)
    }

    /// Points every focus index at `id`. Returns `false` when the window is not
    /// in this workspace.
    pub fn focus_window(&mut self, id: WindowId) -> bool {
        if self.maximized_window.as_ref().is_some_and(|w| w.id == id) {
            return true;
        }
        if self
            .monocle_container
            .as_mut()
            .is_some_and(|c| c.focus_window(id))
        {
            return true;
        }
        if let Some(idx) = self.floating_windows.position(|w| w.id == id) {
            self.floating_windows.focus(idx);
            self.focus_is_floating = true;
            return true;
        }
        if let Some(idx) = self.container_idx_for_window(id) {
            self.containers.focus(idx);
            self.focus_is_floating = false;
            if let Some(container) = self.containers.focused_mut() {
                container.focus_window(id);
            }
            return true;
        }
        false
    }

    // -- layout -------------------------------------------------------------

    /// The layout that applies to `len` containers, honouring the layout rules.
    #[must_use]
    pub fn resolve_layout(&self, len: usize) -> Layout {
        self.layout_rules
            .range(..=len)
            .next_back()
            .map_or(self.layout, |(_, layout)| *layout)
    }

    /// The layout in force right now.
    #[must_use]
    pub fn effective_layout(&self) -> Layout {
        self.resolve_layout(self.containers.len())
    }

    /// The work area with the workspace padding taken off.
    #[must_use]
    pub fn tiling_area(&self, work_area: Rect, default_workspace_padding: i32) -> Rect {
        self.tiling_area_scaled(work_area, default_workspace_padding, 1.0)
    }

    /// The work area with the workspace padding taken off, in the physical
    /// pixels of a display with that scale factor.
    ///
    /// The padding is configured in logical pixels, so a 14 pixel padding on a
    /// display at 150 percent scaling takes 21 physical pixels off each side.
    #[must_use]
    pub fn tiling_area_scaled(
        &self,
        work_area: Rect,
        default_workspace_padding: i32,
        scale: f32,
    ) -> Rect {
        let padding = self.workspace_padding.unwrap_or(default_workspace_padding);
        work_area.padded_clamped(scale_padding(padding, scale))
    }

    /// The rectangle a monocle or maximized window fills.
    #[must_use]
    pub fn full_rect(
        &self,
        work_area: Rect,
        default_workspace_padding: i32,
        default_container_padding: i32,
    ) -> Rect {
        self.full_rect_scaled(
            work_area,
            default_workspace_padding,
            default_container_padding,
            1.0,
        )
    }

    /// The rectangle a monocle or maximized window fills, with both paddings
    /// scaled for a display that is not at 96 DPI.
    #[must_use]
    pub fn full_rect_scaled(
        &self,
        work_area: Rect,
        default_workspace_padding: i32,
        default_container_padding: i32,
        scale: f32,
    ) -> Rect {
        let padding = self.container_padding.unwrap_or(default_container_padding);
        self.tiling_area_scaled(work_area, default_workspace_padding, scale)
            .padded_clamped(scale_padding(padding, scale))
    }

    /// The floor a tile of this workspace may not shrink below, in logical
    /// pixels, per axis.
    ///
    /// Every cut a layout makes divides exactly one axis, so each takes the
    /// floor that belongs to it: a width of 300 and a height of 200 means no
    /// tile narrower than 300 and none shorter than 200, rather than 300 on
    /// both.
    ///
    /// An absurd value is not clamped here. The layouts scale a minimum that
    /// cannot fit back down themselves, so a minimum larger than the screen
    /// still tiles the area exactly instead of overflowing it.
    #[must_use]
    pub fn minimum_tile_size(&self) -> crate::layout::MinSize {
        crate::layout::MinSize {
            width: self.minimum_window_width,
            height: self.minimum_window_height,
        }
    }

    /// Recomputes the tiled rectangles and stores them in `latest_layout`.
    ///
    /// Returns the new rectangles. A workspace with tiling turned off gets an
    /// empty layout, which tells the daemon to move nothing.
    pub fn update_layout(
        &mut self,
        work_area: Rect,
        default_workspace_padding: i32,
        default_container_padding: i32,
    ) -> &[Rect] {
        self.update_layout_scaled(
            work_area,
            default_workspace_padding,
            default_container_padding,
            1.0,
        )
    }

    /// Recomputes the tiled rectangles for a display with that scale factor.
    ///
    /// Both paddings and the minimum tile size are logical pixel values, so
    /// they are multiplied by `scale` before the layout runs. A scale of 1.0
    /// is exactly [`Workspace::update_layout`]. The minimum is
    /// [`Workspace::minimum_tile_size`], which is
    /// [`MIN_TILE_SIZE`] until a configuration says otherwise.
    pub fn update_layout_scaled(
        &mut self,
        work_area: Rect,
        default_workspace_padding: i32,
        default_container_padding: i32,
        scale: f32,
    ) -> &[Rect] {
        let len = self.containers.len();
        self.resize_dimensions.resize(len, None);

        if self.tile {
            let area = self.tiling_area_scaled(work_area, default_workspace_padding, scale);
            let padding = scale_padding(
                self.container_padding.unwrap_or(default_container_padding),
                scale,
            );
            let rects = self.resolve_layout(len).calculate_with_min(
                area,
                len,
                padding,
                self.layout_flip,
                &self.resize_dimensions,
                {
                    let min = self.minimum_tile_size();
                    crate::layout::MinSize {
                        width: scale_padding(min.width, scale),
                        height: scale_padding(min.height, scale),
                    }
                },
            );
            // Reusing the buffer keeps the per event path free of one free and
            // one allocation on every retile.
            self.latest_layout.clear();
            self.latest_layout.extend_from_slice(&rects);
        } else {
            self.latest_layout.clear();
        }

        &self.latest_layout
    }

    /// The rectangles from the last [`Workspace::update_layout`].
    #[must_use]
    pub fn latest_layout(&self) -> &[Rect] {
        &self.latest_layout
    }

    /// Overwrites the cached layout. Only useful in tests and when restoring a
    /// saved state.
    pub fn set_latest_layout(&mut self, rects: Vec<Rect>) {
        self.latest_layout = rects;
    }

    /// The index of the container that lies in `direction` from the focused
    /// one, according to the last layout update.
    #[must_use]
    pub fn container_idx_in_direction(&self, direction: Direction) -> Option<usize> {
        nearest_in_direction(
            &self.latest_layout,
            self.containers.focused_idx(),
            direction,
        )
    }

    /// Drops every resize delta, returning the layout to its default sizes.
    pub fn clear_resize_dimensions(&mut self) {
        self.resize_dimensions.clear();
        self.resize_dimensions.resize(self.containers.len(), None);
    }

    /// Replaces the delta for one container.
    pub fn set_resize_dimension(&mut self, idx: usize, delta: Option<Rect>) {
        if self.resize_dimensions.len() < self.containers.len() {
            self.resize_dimensions.resize(self.containers.len(), None);
        }
        if let Some(slot) = self.resize_dimensions.get_mut(idx) {
            *slot = delta;
        }
    }

    /// The delta for one container.
    #[must_use]
    pub fn resize_dimension(&self, idx: usize) -> Option<Rect> {
        self.resize_dimensions.get(idx).copied().flatten()
    }

    // -- adding and removing windows ----------------------------------------

    /// Adds a window in its own container at the end and focuses it.
    pub fn add_window(&mut self, window: Window) -> usize {
        if let Some(idx) = self.container_idx_for_window(window.id) {
            self.containers.focus(idx);
            self.focus_is_floating = false;
            return idx;
        }
        let idx = self.containers.push_focused(Container::from_window(window));
        self.resize_dimensions.resize(self.containers.len(), None);
        self.focus_is_floating = false;
        idx
    }

    /// Adds a window in its own container next to the focused one.
    pub fn insert_window(&mut self, idx: usize, window: Window) -> usize {
        if let Some(existing) = self.container_idx_for_window(window.id) {
            self.containers.focus(existing);
            self.focus_is_floating = false;
            return existing;
        }
        let idx = self
            .containers
            .insert_focused(idx, Container::from_window(window));
        self.resize_dimensions
            .insert(idx.min(self.resize_dimensions.len()), None);
        self.resize_dimensions.resize(self.containers.len(), None);
        self.focus_is_floating = false;
        idx
    }

    /// Adds a window to the floating list and focuses it.
    pub fn add_floating_window(&mut self, window: Window) {
        if let Some(idx) = self.floating_windows.position(|w| w.id == window.id) {
            self.floating_windows.focus(idx);
        } else {
            self.floating_windows.push_focused(window);
        }
        self.focus_is_floating = true;
    }

    /// Removes a window from wherever it is in this workspace.
    ///
    /// Containers that end up empty are dropped, and monocle or maximize mode
    /// is switched off when its window disappears.
    pub fn remove_window(&mut self, id: WindowId) -> Option<Window> {
        if self.maximized_window.as_ref().is_some_and(|w| w.id == id) {
            self.maximized_restore = None;
            return self.maximized_window.take();
        }

        if self
            .monocle_container
            .as_ref()
            .is_some_and(|c| c.contains(id))
        {
            let window = self
                .monocle_container
                .as_mut()
                .and_then(|c| c.remove_window(id));
            if self
                .monocle_container
                .as_ref()
                .is_some_and(Container::is_empty)
            {
                self.monocle_container = None;
                self.monocle_restore = None;
            }
            return window;
        }

        if let Some(idx) = self.floating_windows.position(|w| w.id == id) {
            let window = self.floating_windows.remove(idx);
            if self.floating_windows.is_empty() {
                self.focus_is_floating = false;
            }
            return window;
        }

        let idx = self.container_idx_for_window(id)?;
        let window = self
            .containers
            .get_mut(idx)
            .and_then(|c| c.remove_window(id));
        if self.containers.get(idx).is_some_and(Container::is_empty) {
            self.containers.remove(idx);
            if idx < self.resize_dimensions.len() {
                self.resize_dimensions.remove(idx);
            }
        }
        window
    }

    /// Removes and returns the focused window.
    pub fn remove_focused_window(&mut self) -> Option<Window> {
        let id = self.focused_window_id()?;
        self.remove_window(id)
    }

    /// Removes the focused container and returns it.
    pub fn remove_focused_container(&mut self) -> Option<Container> {
        let idx = self.containers.focused_idx();
        let container = self.containers.remove(idx)?;
        if idx < self.resize_dimensions.len() {
            self.resize_dimensions.remove(idx);
        }
        Some(container)
    }

    /// Puts a whole container back into the ring at `idx` and focuses it.
    pub fn insert_container(&mut self, idx: usize, container: Container) -> usize {
        let idx = self.containers.insert_focused(idx, container);
        self.resize_dimensions
            .insert(idx.min(self.resize_dimensions.len()), None);
        self.resize_dimensions.resize(self.containers.len(), None);
        self.focus_is_floating = false;
        idx
    }

    /// Appends everything another workspace holds to this one.
    ///
    /// Containers keep their order and their stacks, floating windows stay
    /// floating, and a monocle or maximized window arrives as a plain
    /// container: those modes belong to the workspace the window is leaving,
    /// not to the window. The focus stays where it already was here, so
    /// taking in the windows of a display that was unplugged does not steal
    /// it.
    ///
    /// This is how the daemon rescues the windows of a monitor that is gone,
    /// workspace by workspace rather than window by window, so the containers,
    /// the stacks and the floating list survive the unplug.
    pub fn absorb(&mut self, other: Workspace) {
        let restore = self.containers.focused_idx();
        let had_containers = !self.containers.is_empty();

        let mut incoming: Vec<Container> = other.containers.into_vec();
        if let Some(container) = other.monocle_container {
            incoming.push(container);
        }
        if let Some(window) = other.maximized_window
            && !incoming.iter().any(|c| c.contains(window.id))
        {
            // A maximized window that was alone is not in any container.
            incoming.push(Container::from_window(window));
        }

        for container in incoming {
            let at = self.containers.len();
            self.insert_container(at, container);
        }
        for window in other.floating_windows.into_vec() {
            self.floating_windows.push(window);
        }

        if had_containers {
            self.containers.focus(restore);
        } else {
            self.containers.focus(0);
        }
    }

    // -- modes --------------------------------------------------------------

    /// The window holding the spot in front of the container at `idx`, so a
    /// container that is lifted out of the ring can find its way back after
    /// the ring has moved. `None` when it is the first container.
    fn anchor_before(&self, idx: usize) -> Option<WindowId> {
        idx.checked_sub(1)
            .and_then(|previous| self.containers.get(previous))
            .and_then(Container::focused_window_id)
    }

    /// The index right behind the container holding `anchor`: the front of the
    /// ring when there is no anchor, the back when the anchor itself is gone.
    fn slot_behind(&self, anchor: Option<WindowId>) -> usize {
        match anchor {
            None => 0,
            Some(id) => self
                .container_idx_for_window(id)
                .map_or(self.containers.len(), |idx| idx + 1),
        }
    }

    /// Turns monocle mode on for the focused container, or off again.
    ///
    /// Returns `true` when monocle mode is on afterwards.
    pub fn toggle_monocle(&mut self) -> bool {
        if let Some(container) = self.monocle_container.take() {
            let anchor = self.monocle_restore.take();
            let at = self.slot_behind(anchor);
            self.insert_container(at, container);
            false
        } else {
            // The mirror of the guard in `toggle_maximize`. Maximize is the
            // other mode that lifts a window out of the ring, so while it is on
            // the ring's focused container is not the window the user is
            // looking at: monocling it would lift a hidden window out, leave
            // the workspace in two exclusive modes at once, and the follow-up
            // un-maximize would then send its restore to the monocled window
            // instead, stranding the maximized one at full size for good.
            if self.is_maximized() {
                return false;
            }
            let anchor = self.anchor_before(self.containers.focused_idx());
            if let Some(container) = self.remove_focused_container() {
                self.monocle_container = Some(container);
                self.monocle_restore = anchor;
                self.focus_is_floating = false;
                true
            } else {
                false
            }
        }
    }

    /// Maximizes the focused window, or restores the maximized one.
    ///
    /// Returns `true` when a window is maximized afterwards. Un-maximizing puts
    /// the window back where it came from when that spot still exists, and next
    /// to it otherwise. Monocle mode refuses the maximize, because it already
    /// fills the workspace and the two modes are exclusive.
    pub fn toggle_maximize(&mut self) -> bool {
        if let Some(window) = self.maximized_window.take() {
            match self.maximized_restore.take() {
                Some(Restore {
                    window: window_idx,
                    floating: true,
                    ..
                }) => {
                    self.floating_windows.insert_focused(window_idx, window);
                    self.focus_is_floating = true;
                }
                Some(Restore {
                    anchor,
                    window: window_idx,
                    own_container: false,
                    ..
                }) => match anchor.and_then(|id| self.container_idx_for_window(id)) {
                    Some(idx) => {
                        self.containers.focus(idx);
                        self.focus_is_floating = false;
                        if let Some(target) = self.containers.get_mut(idx) {
                            target.insert_window(window_idx, window);
                        }
                    }
                    // Everything it shared the stack with is gone, so there is
                    // no stack left to rejoin.
                    None => {
                        self.add_window(window);
                    }
                },
                Some(Restore { anchor, .. }) => {
                    let at = self.slot_behind(anchor);
                    self.insert_container(at, Container::from_window(window));
                }
                None => {
                    self.add_window(window);
                }
            }
            return false;
        }

        // Monocle is the other mode that lifts a container out of the ring, so
        // while it is on the ring's focused container is never the window the
        // user is looking at. Maximizing it would blow up a hidden window,
        // hide the monocled one behind it and leave the workspace in two
        // exclusive modes at once. A monocle already fills the workspace,
        // which is all a maximize would do, so this refuses instead.
        if self.is_monocle() {
            return false;
        }

        if self.focus_is_floating() {
            let window_idx = self.floating_windows.focused_idx();
            let Some(window) = self.floating_windows.remove_focused() else {
                return false;
            };
            if self.floating_windows.is_empty() {
                self.focus_is_floating = false;
            }
            self.maximized_window = Some(window);
            self.maximized_restore = Some(Restore {
                anchor: None,
                window: window_idx,
                own_container: false,
                floating: true,
            });
            return true;
        }

        let container_idx = self.containers.focused_idx();
        let Some(container) = self.containers.focused_mut() else {
            return false;
        };
        let window_idx = container.windows().focused_idx();
        let Some(window) = container.remove_focused_window() else {
            return false;
        };
        let own_container = container.is_empty();
        // The sibling that stays behind holds the spot for the way back.
        let mut anchor = container.focused_window_id();
        if own_container {
            self.containers.remove(container_idx);
            if container_idx < self.resize_dimensions.len() {
                self.resize_dimensions.remove(container_idx);
            }
            anchor = self.anchor_before(container_idx);
        }
        self.maximized_window = Some(window);
        self.maximized_restore = Some(Restore {
            anchor,
            window: window_idx,
            own_container,
            floating: false,
        });
        true
    }

    /// Moves the focused window between the tiled containers and the floating
    /// list.
    ///
    /// Returns `true` when the window floats afterwards.
    pub fn toggle_float(&mut self) -> bool {
        if self.focus_is_floating() {
            if let Some(window) = self.floating_windows.remove_focused() {
                if self.floating_windows.is_empty() {
                    self.focus_is_floating = false;
                }
                let idx = self.containers.focused_idx();
                let at = if self.containers.is_empty() {
                    0
                } else {
                    idx + 1
                };
                self.insert_container(at, Container::from_window(window));
            }
            return false;
        }

        if let Some(window) = self.remove_focused_window() {
            self.add_floating_window(window);
            return true;
        }
        false
    }

    /// Moves a specific window into the floating list.
    pub fn float_window(&mut self, id: WindowId) -> bool {
        if self.floating_windows.position(|w| w.id == id).is_some() {
            return false;
        }
        match self.remove_window(id) {
            Some(window) => {
                self.add_floating_window(window);
                true
            }
            None => false,
        }
    }

    // -- rearranging --------------------------------------------------------

    /// Moves the focused container to the front of the ring.
    ///
    /// Resize deltas stay with the position, not with the container, the same
    /// rule [`Workspace::swap_focused_container`] follows: rearranging windows
    /// does not drag the layout's proportions along with them.
    ///
    /// Returns `false` when there is nothing to promote.
    pub fn promote_focused_container(&mut self) -> bool {
        let from = self.containers.focused_idx();
        if self.containers.len() < 2 || from == 0 {
            return false;
        }
        let Some(container) = self.containers.remove(from) else {
            return false;
        };
        // Held aside across the remove and the insert. Letting those two touch
        // the deltas dropped the promoted container's own resize and shifted
        // every other one along by a place, so promoting a window you had just
        // resized threw that resize away and quietly restyled the rest of the
        // workspace.
        let deltas = std::mem::take(&mut self.resize_dimensions);
        self.insert_container(0, container);
        self.resize_dimensions = deltas;
        self.resize_dimensions.resize(self.containers.len(), None);
        true
    }

    /// Swaps the focused container with the one at `idx` and follows it.
    ///
    /// Resize deltas stay with the position, not with the container, so moving
    /// a window around does not drag the layout's proportions along with it.
    pub fn swap_focused_container(&mut self, idx: usize) -> bool {
        let from = self.containers.focused_idx();
        if from == idx || !self.containers.swap(from, idx) {
            return false;
        }
        self.containers.focus(idx);
        self.focus_is_floating = false;
        true
    }

    /// Moves the focused window out of its stack into a container of its own,
    /// placed right after it.
    ///
    /// Returns `false` when the focused window is not part of a stack.
    pub fn unstack_focused_window(&mut self) -> bool {
        let idx = self.containers.focused_idx();
        if !self.containers.get(idx).is_some_and(Container::is_stack) {
            return false;
        }
        let Some(window) = self
            .containers
            .get_mut(idx)
            .and_then(Container::remove_focused_window)
        else {
            return false;
        };
        self.insert_container(idx + 1, Container::from_window(window));
        true
    }

    /// Collapses every tiled container into one stack, in the order the
    /// containers are in.
    ///
    /// The focused window keeps the focus, which is what makes this usable as
    /// a toggle: stack everything to read one window at a time, unstack to get
    /// the tiling back, and the window you were looking at is still the one in
    /// front.
    ///
    /// Floating windows are left alone. They are not in the container ring, so
    /// collapsing the ring has nothing to say about them.
    ///
    /// Returns `false` when there is nothing to collapse.
    pub fn stack_all(&mut self) -> bool {
        if self.containers.len() < 2 {
            return false;
        }
        let focused = self.focused_window_id();

        let mut windows = Vec::new();
        for container in self.containers.iter_mut() {
            windows.append(&mut container.drain());
        }
        let mut stack = Container::new();
        for window in windows {
            stack.add_window(window);
        }
        if let Some(id) = focused {
            stack.focus_window(id);
        }

        self.containers.clear();
        self.containers.push(stack);
        self.containers.focus(0);
        self.resize_dimensions.clear();
        self.resize_dimensions.resize(1, None);
        self.focus_is_floating = false;
        true
    }

    /// Gives every window in every stack a container of its own.
    ///
    /// The reverse of [`Workspace::stack_all`], and the way out of a workspace
    /// somebody stacked by hand one window at a time. Order is preserved, so a
    /// stack of three becomes three neighbouring containers in the order they
    /// were stacked.
    ///
    /// Returns `false` when no container holds more than one window.
    pub fn unstack_all(&mut self) -> bool {
        if !self.containers.iter().any(Container::is_stack) {
            return false;
        }
        let focused = self.focused_window_id();

        let mut windows = Vec::new();
        for container in self.containers.iter_mut() {
            windows.append(&mut container.drain());
        }

        self.containers.clear();
        self.resize_dimensions.clear();
        for window in windows {
            self.containers.push(Container::from_window(window));
        }
        self.resize_dimensions.resize(self.containers.len(), None);

        // The focus follows the window, not the position: the window that was
        // on top of a stack is somewhere in the middle of the ring now.
        let idx = focused
            .and_then(|id| self.containers.position(|c| c.contains(id)))
            .unwrap_or(0);
        self.containers.focus(idx);
        self.focus_is_floating = false;
        true
    }

    /// Moves the focused window into the container at `target`, stacking it on
    /// top.
    ///
    /// Returns `false` when there is no such container or it is the one the
    /// window is already in.
    pub fn stack_focused_window_into(&mut self, target: usize) -> bool {
        let from = self.containers.focused_idx();
        if from == target || target >= self.containers.len() {
            return false;
        }
        let Some(window) = self
            .containers
            .get_mut(from)
            .and_then(Container::remove_focused_window)
        else {
            return false;
        };

        let mut target = target;
        if self.containers.get(from).is_some_and(Container::is_empty) {
            self.containers.remove(from);
            if from < self.resize_dimensions.len() {
                self.resize_dimensions.remove(from);
            }
            if target > from {
                target -= 1;
            }
        }

        if let Some(container) = self.containers.get_mut(target) {
            container.add_window(window);
            self.containers.focus(target);
            self.focus_is_floating = false;
            true
        } else {
            // Should not happen, but never drop a window on the floor.
            self.add_window(window);
            false
        }
    }

    /// Moves the focus one container along the ring, wrapping.
    pub fn cycle_container_focus(&mut self, direction: CycleDirection) -> Option<usize> {
        self.focus_is_floating = false;
        self.containers.cycle_focus(direction)
    }

    /// Swaps the focused container with its neighbour in the ring and follows it.
    pub fn cycle_container_move(&mut self, direction: CycleDirection) -> Option<usize> {
        self.focus_is_floating = false;
        self.containers.move_focused(direction)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn promoting_a_container_leaves_the_resize_deltas_where_they_are() {
        let mut ws = Workspace::new();
        for id in 1..=4 {
            ws.add_window(Window::new(id));
        }
        let delta = Rect::new(0, 0, 40, 0);
        ws.set_resize_dimension(2, Some(delta));
        ws.set_resize_dimension(0, Some(Rect::new(0, 0, 10, 0)));

        ws.containers_mut().focus(2);
        assert!(ws.promote_focused_container());

        // Deltas belong to positions, which is the rule `swap_focused_container`
        // documents. Removing one and inserting a blank threw away the resize
        // of the very container being promoted and slid every other one along.
        assert_eq!(ws.resize_dimension(0), Some(Rect::new(0, 0, 10, 0)));
        assert_eq!(ws.resize_dimension(2), Some(delta));
        assert_eq!(ws.containers().len(), 4);
    }
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1920, 1080);

    fn workspace_with(count: usize) -> Workspace {
        let mut ws = Workspace::new();
        for id in 1..=count {
            ws.add_window(Window::new(id as isize));
        }
        ws.update_layout(WORK_AREA, 0, 0);
        ws
    }

    #[test]
    fn a_new_workspace_is_empty_and_tiles() {
        let ws = Workspace::new();
        assert!(ws.is_empty());
        assert!(ws.tile);
        assert_eq!(ws.layout, Layout::Bsp);
        assert_eq!(ws.focused_window_id(), None);
        assert!(ws.latest_layout().is_empty());
        assert_eq!(Workspace::named("code").name.as_deref(), Some("code"));
    }

    #[test]
    fn adding_windows_makes_one_container_each() {
        let ws = workspace_with(3);
        assert_eq!(ws.containers().len(), 3);
        assert_eq!(ws.focused_window_id(), Some(WindowId(3)));
        assert_eq!(ws.latest_layout().len(), 3);
        assert_eq!(ws.resize_dimensions.len(), 3);
        assert!(ws.contains_window(WindowId(2)));
        assert!(!ws.contains_window(WindowId(9)));
        assert_eq!(ws.window(WindowId(1)).map(|w| w.id), Some(WindowId(1)));
    }

    #[test]
    fn adding_the_same_window_twice_only_focuses_it() {
        let mut ws = workspace_with(3);
        assert_eq!(ws.add_window(Window::new(1)), 0);
        assert_eq!(ws.containers().len(), 3);
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert_eq!(ws.insert_window(0, Window::new(2)), 1);
        assert_eq!(ws.containers().len(), 3);
    }

    #[test]
    fn inserting_puts_the_container_where_asked() {
        let mut ws = workspace_with(3);
        assert_eq!(ws.insert_window(0, Window::new(9)), 0);
        assert_eq!(ws.containers().len(), 4);
        assert_eq!(ws.focused_container_idx(), 0);
        assert_eq!(ws.resize_dimensions.len(), 4);
    }

    #[test]
    fn removing_a_window_drops_its_empty_container() {
        let mut ws = workspace_with(3);
        assert_eq!(
            ws.remove_window(WindowId(2)).map(|w| w.id),
            Some(WindowId(2))
        );
        assert_eq!(ws.containers().len(), 2);
        ws.update_layout(WORK_AREA, 0, 0);
        assert_eq!(ws.resize_dimensions.len(), 2);
        assert!(ws.remove_window(WindowId(2)).is_none());
    }

    #[test]
    fn layout_rules_pick_a_layout_by_container_count() {
        let mut ws = Workspace::new();
        ws.layout = Layout::Bsp;
        ws.layout_rules.insert(1, Layout::VerticalStack);
        ws.layout_rules.insert(4, Layout::Columns);

        assert_eq!(
            ws.resolve_layout(0),
            Layout::Bsp,
            "no rule below the first key"
        );
        assert_eq!(ws.resolve_layout(1), Layout::VerticalStack);
        assert_eq!(ws.resolve_layout(3), Layout::VerticalStack);
        assert_eq!(ws.resolve_layout(4), Layout::Columns);
        assert_eq!(ws.resolve_layout(9), Layout::Columns);

        for _ in 0..4 {
            ws.add_window(Window::new(1));
        }
        assert_eq!(
            ws.effective_layout(),
            Layout::VerticalStack,
            "one container"
        );
    }

    #[test]
    fn update_layout_honours_padding() {
        let mut ws = Workspace::new();
        ws.add_window(Window::new(1));
        ws.workspace_padding = Some(14);
        ws.container_padding = Some(10);
        let rects = ws.update_layout(WORK_AREA, 0, 0).to_vec();
        assert_eq!(rects, vec![Rect::new(24, 24, 1896, 1056)]);
        assert_eq!(ws.tiling_area(WORK_AREA, 0), Rect::new(14, 14, 1906, 1066));
        assert_eq!(ws.full_rect(WORK_AREA, 0, 0), Rect::new(24, 24, 1896, 1056));
    }

    #[test]
    fn padding_falls_back_to_the_defaults() {
        let mut ws = Workspace::new();
        ws.add_window(Window::new(1));
        let rects = ws.update_layout(WORK_AREA, 14, 10).to_vec();
        assert_eq!(rects, vec![Rect::new(24, 24, 1896, 1056)]);
    }

    #[test]
    fn turning_tiling_off_produces_no_rects() {
        let mut ws = workspace_with(3);
        ws.tile = false;
        assert!(ws.update_layout(WORK_AREA, 0, 0).is_empty());
        assert!(ws.latest_layout().is_empty());
    }

    #[test]
    fn directional_lookup_uses_the_latest_layout() {
        let mut ws = workspace_with(3);
        // BSP: 1 on the left, 2 top right, 3 bottom right.
        ws.focus_container(0);
        assert_eq!(ws.container_idx_in_direction(Direction::Right), Some(1));
        assert_eq!(ws.container_idx_in_direction(Direction::Left), None);
        ws.focus_container(1);
        assert_eq!(ws.container_idx_in_direction(Direction::Down), Some(2));
        assert_eq!(ws.container_idx_in_direction(Direction::Left), Some(0));
    }

    #[test]
    fn focus_window_points_every_index_at_it() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_window(WindowId(1)));
        assert_eq!(ws.focused_container_idx(), 0);
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert!(!ws.focus_window(WindowId(99)));
    }

    #[test]
    fn visible_and_hidden_windows_in_a_plain_workspace() {
        let ws = workspace_with(3);
        assert_eq!(
            ws.visible_window_ids(),
            vec![WindowId(1), WindowId(2), WindowId(3)]
        );
        assert!(ws.hidden_window_ids().is_empty());
    }

    #[test]
    fn stacked_windows_are_hidden() {
        let mut ws = workspace_with(2);
        assert!(ws.focus_container(0));
        ws.containers_mut()
            .focused_mut()
            .unwrap()
            .add_window(Window::new(9));
        assert_eq!(ws.visible_window_ids(), vec![WindowId(9), WindowId(2)]);
        assert_eq!(ws.hidden_window_ids(), vec![WindowId(1)]);
    }

    #[test]
    fn toggling_float_moves_a_window_out_and_back() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_window(WindowId(2)));
        assert!(ws.toggle_float());
        assert_eq!(ws.containers().len(), 2);
        assert_eq!(ws.floating_windows().len(), 1);
        assert!(ws.focus_is_floating());
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
        assert!(ws.visible_window_ids().contains(&WindowId(2)));

        assert!(!ws.toggle_float());
        assert_eq!(ws.containers().len(), 3);
        assert!(ws.floating_windows().is_empty());
        assert!(!ws.focus_is_floating());
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
    }

    #[test]
    fn floating_a_window_by_handle() {
        let mut ws = workspace_with(3);
        assert!(ws.float_window(WindowId(1)));
        assert_eq!(ws.floating_windows().len(), 1);
        assert!(!ws.float_window(WindowId(1)), "already floating");
        assert!(!ws.float_window(WindowId(99)));
    }

    #[test]
    fn floating_the_only_window_leaves_no_containers() {
        let mut ws = workspace_with(1);
        assert!(ws.toggle_float());
        assert!(ws.containers().is_empty());
        assert!(!ws.is_empty());
        assert!(!ws.toggle_float());
        assert_eq!(ws.containers().len(), 1);
    }

    #[test]
    fn monocle_lifts_the_container_out_and_puts_it_back() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_container(1));
        assert!(ws.toggle_monocle());
        assert!(ws.is_monocle());
        assert_eq!(ws.containers().len(), 2);
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
        assert_eq!(ws.visible_window_ids(), vec![WindowId(2)]);
        assert_eq!(ws.hidden_window_ids(), vec![WindowId(1), WindowId(3)]);

        assert!(!ws.toggle_monocle());
        assert!(!ws.is_monocle());
        assert_eq!(ws.containers().len(), 3);
        assert_eq!(ws.focused_container_idx(), 1);
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
    }

    #[test]
    fn monocle_on_an_empty_workspace_does_nothing() {
        let mut ws = Workspace::new();
        assert!(!ws.toggle_monocle());
        assert!(!ws.is_monocle());
    }

    #[test]
    fn removing_the_monocle_window_leaves_monocle_mode() {
        let mut ws = workspace_with(2);
        ws.toggle_monocle();
        let id = ws.focused_window_id().unwrap();
        assert!(ws.remove_window(id).is_some());
        assert!(!ws.is_monocle());
    }

    #[test]
    fn maximize_pulls_a_lone_window_out_and_puts_it_back_in_place() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_window(WindowId(2)));
        assert!(ws.toggle_maximize());
        assert!(ws.is_maximized());
        assert_eq!(ws.containers().len(), 2);
        assert_eq!(ws.maximized_window().map(|w| w.id), Some(WindowId(2)));
        assert_eq!(ws.visible_window_ids(), vec![WindowId(2)]);

        assert!(!ws.toggle_maximize());
        assert_eq!(ws.containers().len(), 3);
        assert_eq!(ws.focused_container_idx(), 1);
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
    }

    #[test]
    fn maximize_keeps_a_stacked_window_in_its_stack() {
        let mut ws = workspace_with(2);
        assert!(ws.focus_container(0));
        ws.containers_mut()
            .focused_mut()
            .unwrap()
            .add_window(Window::new(9));
        assert!(ws.toggle_maximize());
        assert_eq!(ws.containers().len(), 2, "the stack survives");
        assert!(!ws.toggle_maximize());
        assert_eq!(ws.containers().get(0).unwrap().len(), 2);
        assert_eq!(ws.focused_window_id(), Some(WindowId(9)));
    }

    #[test]
    fn removing_the_maximized_window_leaves_maximized_mode() {
        let mut ws = workspace_with(2);
        ws.toggle_maximize();
        assert!(ws.remove_window(WindowId(2)).is_some());
        assert!(!ws.is_maximized());
    }

    #[test]
    fn a_floating_window_maximizes_and_goes_back_to_floating() {
        let mut ws = workspace_with(2);
        assert!(ws.toggle_float());
        assert!(ws.toggle_maximize());
        assert!(ws.is_maximized());
        assert!(
            ws.floating_windows().is_empty(),
            "it is lifted out while maximized"
        );
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
        assert_eq!(ws.containers().len(), 1, "it never joins the tiled ring");

        assert!(!ws.toggle_maximize());
        assert!(!ws.is_maximized());
        assert_eq!(ws.floating_windows().len(), 1);
        assert!(ws.focus_is_floating());
        assert_eq!(ws.containers().len(), 1);
    }

    #[test]
    fn promote_moves_the_focused_container_to_the_front() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_container(2));
        assert!(ws.promote_focused_container());
        assert_eq!(ws.focused_container_idx(), 0);
        assert_eq!(ws.focused_window_id(), Some(WindowId(3)));
        assert!(!ws.promote_focused_container(), "already at the front");
        assert!(!workspace_with(1).promote_focused_container());
    }

    #[test]
    fn swapping_containers_follows_the_focus() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_container(0));
        assert!(ws.swap_focused_container(2));
        assert_eq!(ws.focused_container_idx(), 2);
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert!(!ws.swap_focused_container(2), "same index");
        assert!(!ws.swap_focused_container(9));
    }

    #[test]
    fn stacking_and_unstacking() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_container(0));
        assert!(ws.stack_focused_window_into(1));
        assert_eq!(ws.containers().len(), 2);
        assert_eq!(ws.focused_container_idx(), 0, "indices shifted down");
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert!(ws.containers().get(0).unwrap().is_stack());

        assert!(ws.unstack_focused_window());
        assert_eq!(ws.containers().len(), 3);
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert!(!ws.unstack_focused_window(), "not a stack any more");
    }

    #[test]
    fn stacking_into_a_missing_container_is_a_no_op() {
        let mut ws = workspace_with(2);
        assert!(!ws.stack_focused_window_into(9));
        assert!(!ws.stack_focused_window_into(ws.focused_container_idx()));
        assert_eq!(ws.containers().len(), 2);
    }

    #[test]
    fn cycling_containers_moves_focus_and_order() {
        let mut ws = workspace_with(3);
        assert!(ws.focus_container(0));
        assert_eq!(ws.cycle_container_focus(CycleDirection::Next), Some(1));
        assert_eq!(ws.cycle_container_focus(CycleDirection::Previous), Some(0));
        assert_eq!(ws.cycle_container_move(CycleDirection::Next), Some(1));
        assert_eq!(ws.focused_window_id(), Some(WindowId(1)));
        assert_eq!(
            ws.containers()
                .get(0)
                .and_then(Container::focused_window_id),
            Some(WindowId(2))
        );
    }

    #[test]
    fn resize_dimensions_are_kept_in_step_with_the_containers() {
        let mut ws = workspace_with(3);
        ws.set_resize_dimension(0, Some(Rect::new(0, 0, 100, 0)));
        assert_eq!(ws.resize_dimension(0), Some(Rect::new(0, 0, 100, 0)));
        assert_eq!(ws.resize_dimension(1), None);
        assert_eq!(ws.resize_dimension(99), None);

        ws.update_layout(WORK_AREA, 0, 0);
        assert_eq!(ws.latest_layout()[0].width(), 1060, "960 plus 100");

        ws.clear_resize_dimensions();
        ws.update_layout(WORK_AREA, 0, 0);
        assert_eq!(ws.latest_layout()[0].width(), 960);
    }

    #[test]
    fn removing_a_container_removes_its_resize_delta() {
        let mut ws = workspace_with(3);
        ws.set_resize_dimension(2, Some(Rect::new(0, 0, 100, 0)));
        ws.remove_window(WindowId(1));
        assert_eq!(ws.resize_dimensions.len(), 2);
        assert_eq!(ws.resize_dimension(1), Some(Rect::new(0, 0, 100, 0)));
    }

    #[test]
    fn set_latest_layout_overrides_the_cache() {
        let mut ws = Workspace::new();
        ws.set_latest_layout(vec![Rect::new(0, 0, 10, 10)]);
        assert_eq!(ws.latest_layout().len(), 1);
    }

    #[test]
    fn round_trips_through_json() {
        let mut ws = workspace_with(3);
        ws.toggle_monocle();
        let json = serde_json::to_string(&ws).unwrap();
        assert_eq!(serde_json::from_str::<Workspace>(&json).unwrap(), ws);
    }

    #[test]
    fn missing_json_keys_fall_back_to_the_defaults() {
        let ws: Workspace = serde_json::from_str(r#"{"name":"1"}"#).unwrap();
        assert_eq!(ws.name.as_deref(), Some("1"));
        assert_eq!(ws.layout, Layout::Bsp);
        assert!(ws.tile);
    }

    // -- regressions --------------------------------------------------------

    fn container_ids(ws: &Workspace) -> Vec<Vec<WindowId>> {
        ws.containers()
            .iter()
            .map(|c| c.window_ids().collect())
            .collect()
    }

    #[test]
    fn maximizing_while_monocle_is_on_is_refused() {
        let mut ws = workspace_with(2);
        assert!(ws.toggle_monocle());

        assert!(
            !ws.toggle_maximize(),
            "monocle already fills the workspace, the two modes are exclusive"
        );
        assert!(!ws.is_maximized());
        assert!(ws.is_monocle());
        assert_eq!(ws.focused_window_id(), Some(WindowId(2)));
        assert_eq!(ws.visible_window_ids(), vec![WindowId(2)]);
    }

    #[test]
    fn un_maximizing_puts_the_window_back_in_its_own_stack_after_a_container_closed() {
        let mut ws = workspace_with(4);
        assert!(ws.focus_window(WindowId(3)));
        assert!(ws.stack_focused_window_into(1));
        assert!(ws.toggle_maximize());
        assert!(ws.remove_window(WindowId(1)).is_some());

        assert!(!ws.toggle_maximize());

        assert_eq!(
            container_ids(&ws),
            vec![vec![WindowId(2), WindowId(3)], vec![WindowId(4)]],
            "window 3 belongs in the stack it came from, not in window 4's"
        );
        assert_eq!(ws.focused_window_id(), Some(WindowId(3)));
    }

    #[test]
    fn un_maximizing_lands_behind_its_old_neighbour_after_a_container_was_inserted() {
        let mut ws = workspace_with(3);
        assert!(ws.toggle_maximize());
        ws.insert_window(0, Window::new(9));

        assert!(!ws.toggle_maximize());

        assert_eq!(
            container_ids(&ws),
            vec![
                vec![WindowId(9)],
                vec![WindowId(1)],
                vec![WindowId(2)],
                vec![WindowId(3)]
            ],
            "the container that was inserted to the left shifted the restore spot"
        );
    }

    #[test]
    fn leaving_monocle_lands_behind_its_old_neighbour_after_a_container_was_inserted() {
        let mut ws = workspace_with(3);
        assert!(ws.toggle_monocle());
        ws.insert_window(0, Window::new(9));

        assert!(!ws.toggle_monocle());

        assert_eq!(
            container_ids(&ws),
            vec![
                vec![WindowId(9)],
                vec![WindowId(1)],
                vec![WindowId(2)],
                vec![WindowId(3)]
            ],
            "the container that was inserted to the left shifted the restore spot"
        );
    }
}
