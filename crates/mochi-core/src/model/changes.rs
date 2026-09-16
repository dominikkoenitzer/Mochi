//! What a mutating operation changed, so the daemon knows what to redraw.
//!
//! Nothing in this crate talks to Win32. Every operation instead returns a
//! [`Changes`] describing the work the daemon has to do: which workspaces need
//! a fresh layout, which windows have to be shown, hidden, focused, maximized
//! or closed.

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;

use super::window::WindowId;

/// A workspace, addressed by its monitor and its index on that monitor.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct WorkspaceRef {
    /// The index of the monitor in the state's monitor ring.
    pub monitor: usize,
    /// The index of the workspace on that monitor.
    pub workspace: usize,
}

impl WorkspaceRef {
    /// A reference to one workspace.
    #[must_use]
    pub const fn new(monitor: usize, workspace: usize) -> Self {
        Self { monitor, workspace }
    }
}

/// The side effects of one operation.
///
/// Empty fields mean "nothing to do". The daemon should apply the fields in
/// the order they are documented: hide, retile, show, maximize, focus.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct Changes {
    /// Workspaces whose layout has to be recomputed and applied.
    pub retiled: Vec<WorkspaceRef>,
    /// Windows that have to be brought on screen.
    pub show: Vec<WindowId>,
    /// Windows that have to be taken off screen.
    pub hide: Vec<WindowId>,
    /// Windows that have to be minimized.
    pub minimize: Vec<WindowId>,
    /// Windows that have to be taken out of a maximized or minimized state.
    pub restore: Vec<WindowId>,
    /// The window that has to be maximized, if any.
    pub maximize: Option<WindowId>,
    /// The window that has to be closed, if any.
    pub close: Option<WindowId>,
    /// The window that has to end up with the foreground focus, if any.
    pub focus: Option<WindowId>,
    /// Where the cursor has to be warped to, when `mouse_follows_focus` is on.
    pub warp_mouse_to: Option<Rect>,
    /// `true` when the focused monitor changed, so a bar can update.
    pub focused_monitor_changed: bool,
}

impl Changes {
    /// No side effects at all.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// `true` when there is nothing for the daemon to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.retiled.is_empty()
            && self.show.is_empty()
            && self.hide.is_empty()
            && self.minimize.is_empty()
            && self.restore.is_empty()
            && self.maximize.is_none()
            && self.close.is_none()
            && self.focus.is_none()
            && self.warp_mouse_to.is_none()
            && !self.focused_monitor_changed
    }

    /// Marks a workspace as needing a fresh layout. Duplicates are dropped.
    #[must_use]
    pub fn retile(mut self, monitor: usize, workspace: usize) -> Self {
        self.add_retile(monitor, workspace);
        self
    }

    /// Marks a workspace as needing a fresh layout, in place.
    pub fn add_retile(&mut self, monitor: usize, workspace: usize) {
        let target = WorkspaceRef::new(monitor, workspace);
        if !self.retiled.contains(&target) {
            self.retiled.push(target);
        }
    }

    /// Sets the window to focus.
    #[must_use]
    pub fn focus(mut self, id: impl Into<Option<WindowId>>) -> Self {
        self.focus = id.into();
        self
    }

    /// Adds windows to show.
    #[must_use]
    pub fn showing(mut self, ids: impl IntoIterator<Item = WindowId>) -> Self {
        self.show.extend(ids);
        self
    }

    /// Adds windows to hide.
    #[must_use]
    pub fn hiding(mut self, ids: impl IntoIterator<Item = WindowId>) -> Self {
        self.hide.extend(ids);
        self
    }

    /// Folds `other` into `self`.
    pub fn merge(&mut self, other: Changes) {
        for target in other.retiled {
            if !self.retiled.contains(&target) {
                self.retiled.push(target);
            }
        }
        self.show.extend(other.show);
        self.hide.extend(other.hide);
        self.minimize.extend(other.minimize);
        self.restore.extend(other.restore);
        self.maximize = other.maximize.or(self.maximize);
        self.close = other.close.or(self.close);
        self.focus = other.focus.or(self.focus);
        self.warp_mouse_to = other.warp_mouse_to.or(self.warp_mouse_to);
        self.focused_monitor_changed |= other.focused_monitor_changed;
    }

    /// Drops any window that appears in both `show` and `hide`, keeping it
    /// shown, and removes duplicates. A workspace switch that moves a window
    /// between two visible workspaces should not blink.
    pub fn settle(&mut self) {
        self.hide.retain(|id| !self.show.contains(id));
        dedup_keeping_order(&mut self.show);
        dedup_keeping_order(&mut self.hide);
    }
}

fn dedup_keeping_order(ids: &mut Vec<WindowId>) {
    let mut seen: Vec<WindowId> = Vec::with_capacity(ids.len());
    ids.retain(|id| {
        if seen.contains(id) {
            false
        } else {
            seen.push(*id);
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_change_set_is_empty() {
        assert!(Changes::none().is_empty());
        assert!(!Changes::none().retile(0, 1).is_empty());
        assert!(!Changes::none().focus(WindowId(1)).is_empty());
        assert!(!Changes::none().showing([WindowId(1)]).is_empty());
        assert!(!Changes::none().hiding([WindowId(1)]).is_empty());
    }

    #[test]
    fn retiling_the_same_workspace_twice_only_lists_it_once() {
        let changes = Changes::none().retile(0, 1).retile(0, 1).retile(1, 0);
        assert_eq!(
            changes.retiled,
            vec![WorkspaceRef::new(0, 1), WorkspaceRef::new(1, 0)]
        );
    }

    #[test]
    fn merging_keeps_the_newer_scalar_and_appends_the_lists() {
        let mut a = Changes::none()
            .retile(0, 0)
            .focus(WindowId(1))
            .showing([WindowId(1)]);
        let b = Changes::none()
            .retile(0, 0)
            .retile(1, 1)
            .focus(WindowId(2))
            .hiding([WindowId(3)]);
        a.merge(b);

        assert_eq!(a.retiled.len(), 2);
        assert_eq!(a.focus, Some(WindowId(2)), "the later focus wins");
        assert_eq!(a.show, vec![WindowId(1)]);
        assert_eq!(a.hide, vec![WindowId(3)]);
    }

    #[test]
    fn merging_keeps_the_older_scalar_when_the_newer_has_none() {
        let mut a = Changes::none().focus(WindowId(1));
        a.merge(Changes::none().retile(0, 0));
        assert_eq!(a.focus, Some(WindowId(1)));
    }

    #[test]
    fn merging_carries_every_field() {
        let mut a = Changes::none();
        let b = Changes {
            minimize: vec![WindowId(1)],
            restore: vec![WindowId(2)],
            maximize: Some(WindowId(3)),
            close: Some(WindowId(4)),
            warp_mouse_to: Some(Rect::new(0, 0, 1, 1)),
            focused_monitor_changed: true,
            ..Changes::none()
        };
        a.merge(b);
        assert_eq!(a.minimize, vec![WindowId(1)]);
        assert_eq!(a.restore, vec![WindowId(2)]);
        assert_eq!(a.maximize, Some(WindowId(3)));
        assert_eq!(a.close, Some(WindowId(4)));
        assert!(a.warp_mouse_to.is_some());
        assert!(a.focused_monitor_changed);
    }

    #[test]
    fn settling_never_hides_a_window_it_also_shows() {
        let mut changes = Changes::none()
            .showing([WindowId(1), WindowId(2)])
            .hiding([WindowId(2), WindowId(3)]);
        changes.settle();
        assert_eq!(changes.show, vec![WindowId(1), WindowId(2)]);
        assert_eq!(changes.hide, vec![WindowId(3)]);
    }

    #[test]
    fn settling_drops_duplicates_without_reordering() {
        let mut changes = Changes::none()
            .showing([WindowId(3), WindowId(1), WindowId(3)])
            .hiding([WindowId(2), WindowId(2)]);
        changes.settle();
        assert_eq!(changes.show, vec![WindowId(3), WindowId(1)]);
        assert_eq!(changes.hide, vec![WindowId(2)]);
    }

    #[test]
    fn focus_accepts_a_bare_handle_or_an_option() {
        assert_eq!(Changes::none().focus(WindowId(1)).focus, Some(WindowId(1)));
        assert_eq!(Changes::none().focus(None).focus, None);
    }

    #[test]
    fn round_trips_through_json() {
        let changes = Changes::none()
            .retile(0, 1)
            .focus(WindowId(5))
            .hiding([WindowId(6)]);
        let json = serde_json::to_string(&changes).unwrap();
        assert_eq!(serde_json::from_str::<Changes>(&json).unwrap(), changes);
    }
}
