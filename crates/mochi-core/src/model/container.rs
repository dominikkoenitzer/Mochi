//! A container: the stack of windows that share one tile.

use serde::{Deserialize, Serialize};

use super::ring::{CycleDirection, Ring};
use super::window::{Window, WindowId};

/// A stack of windows sharing one rectangle.
///
/// Only the focused window is visible; the rest are hidden by whatever
/// [`crate::model::HidingBehaviour`] the daemon is configured with. A container
/// with one window is the normal case, several windows make a stack.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Container {
    windows: Ring<Window>,
}

impl Container {
    /// An empty container. Workspaces drop these as soon as they appear.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A container holding one window.
    #[must_use]
    pub fn from_window(window: Window) -> Self {
        Self {
            windows: Ring::from_vec(vec![window]),
        }
    }

    /// The windows, focused one included.
    #[must_use]
    pub fn windows(&self) -> &Ring<Window> {
        &self.windows
    }

    /// The windows, mutably.
    pub fn windows_mut(&mut self) -> &mut Ring<Window> {
        &mut self.windows
    }

    /// The number of stacked windows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// `true` when the container holds no windows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// `true` when more than one window shares this tile.
    #[must_use]
    pub fn is_stack(&self) -> bool {
        self.windows.len() > 1
    }

    /// The window that is actually visible.
    #[must_use]
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.focused()
    }

    /// The window that is actually visible, mutably.
    pub fn focused_window_mut(&mut self) -> Option<&mut Window> {
        self.windows.focused_mut()
    }

    /// The handle of the visible window.
    #[must_use]
    pub fn focused_window_id(&self) -> Option<WindowId> {
        self.windows.focused().map(|w| w.id)
    }

    /// Every handle in the stack, in order.
    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.windows.iter().map(|w| w.id)
    }

    /// The handles that are currently hidden behind the focused one.
    pub fn hidden_window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        let focused = self.windows.focused_idx();
        self.windows
            .iter()
            .enumerate()
            .filter(move |(idx, _)| *idx != focused)
            .map(|(_, w)| w.id)
    }

    /// The position of a window in the stack.
    #[must_use]
    pub fn idx_for(&self, id: WindowId) -> Option<usize> {
        self.windows.position(|w| w.id == id)
    }

    /// `true` when the window is in this stack.
    #[must_use]
    pub fn contains(&self, id: WindowId) -> bool {
        self.idx_for(id).is_some()
    }

    /// The window with that handle.
    #[must_use]
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// The window with that handle, mutably.
    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    /// Adds a window on top of the stack and focuses it.
    ///
    /// A window that is already in the stack is only focused, never duplicated.
    pub fn add_window(&mut self, window: Window) -> usize {
        if let Some(idx) = self.idx_for(window.id) {
            self.windows.focus(idx);
            return idx;
        }
        self.windows.push_focused(window)
    }

    /// Inserts a window at `idx` and focuses it.
    pub fn insert_window(&mut self, idx: usize, window: Window) -> usize {
        if let Some(existing) = self.idx_for(window.id) {
            self.windows.focus(existing);
            return existing;
        }
        self.windows.insert_focused(idx, window)
    }

    /// Removes a window by handle.
    pub fn remove_window(&mut self, id: WindowId) -> Option<Window> {
        let idx = self.idx_for(id)?;
        self.windows.remove(idx)
    }

    /// Removes the visible window.
    pub fn remove_focused_window(&mut self) -> Option<Window> {
        self.windows.remove_focused()
    }

    /// Focuses a window by handle. Returns `false` when it is not in the stack.
    pub fn focus_window(&mut self, id: WindowId) -> bool {
        match self.idx_for(id) {
            Some(idx) => self.windows.focus(idx),
            None => false,
        }
    }

    /// Moves the focus one window along the stack, wrapping.
    pub fn cycle_focus(&mut self, direction: CycleDirection) -> Option<WindowId> {
        self.windows.cycle_focus(direction)?;
        self.focused_window_id()
    }

    /// Takes every window out of the container, leaving it empty.
    pub fn drain(&mut self) -> Vec<Window> {
        std::mem::take(&mut self.windows).into_vec()
    }
}

impl From<Window> for Container {
    fn from(window: Window) -> Self {
        Self::from_window(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> Container {
        let mut c = Container::from_window(Window::new(1));
        c.add_window(Window::new(2));
        c.add_window(Window::new(3));
        c
    }

    #[test]
    fn a_new_container_is_empty() {
        let c = Container::new();
        assert!(c.is_empty());
        assert!(!c.is_stack());
        assert_eq!(c.focused_window_id(), None);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn one_window_is_not_a_stack() {
        let c = Container::from(Window::new(1));
        assert!(!c.is_stack());
        assert_eq!(c.focused_window_id(), Some(WindowId(1)));
        assert_eq!(c.hidden_window_ids().count(), 0);
    }

    #[test]
    fn adding_focuses_the_new_window() {
        let c = stack();
        assert!(c.is_stack());
        assert_eq!(c.len(), 3);
        assert_eq!(c.focused_window_id(), Some(WindowId(3)));
        assert_eq!(
            c.hidden_window_ids().collect::<Vec<_>>(),
            vec![WindowId(1), WindowId(2)]
        );
        assert_eq!(
            c.window_ids().collect::<Vec<_>>(),
            vec![WindowId(1), WindowId(2), WindowId(3)]
        );
    }

    #[test]
    fn adding_a_window_twice_only_focuses_it() {
        let mut c = stack();
        assert_eq!(c.add_window(Window::new(1)), 0);
        assert_eq!(c.len(), 3);
        assert_eq!(c.focused_window_id(), Some(WindowId(1)));
        assert_eq!(c.insert_window(0, Window::new(2)), 1);
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn inserting_puts_the_window_where_asked() {
        let mut c = stack();
        assert_eq!(c.insert_window(0, Window::new(9)), 0);
        assert_eq!(
            c.window_ids().collect::<Vec<_>>(),
            vec![WindowId(9), WindowId(1), WindowId(2), WindowId(3)]
        );
        assert_eq!(c.focused_window_id(), Some(WindowId(9)));
    }

    #[test]
    fn removing_by_handle_works_and_reports_misses() {
        let mut c = stack();
        assert_eq!(
            c.remove_window(WindowId(2)).map(|w| w.id),
            Some(WindowId(2))
        );
        assert_eq!(c.len(), 2);
        assert!(c.remove_window(WindowId(2)).is_none());
        assert!(!c.contains(WindowId(2)));
        assert!(c.contains(WindowId(3)));
    }

    #[test]
    fn removing_the_focused_window_keeps_one_visible() {
        let mut c = stack();
        assert_eq!(c.remove_focused_window().map(|w| w.id), Some(WindowId(3)));
        assert_eq!(c.focused_window_id(), Some(WindowId(2)));
    }

    #[test]
    fn focusing_and_cycling() {
        let mut c = stack();
        assert!(c.focus_window(WindowId(1)));
        assert_eq!(c.focused_window_id(), Some(WindowId(1)));
        assert!(!c.focus_window(WindowId(99)));
        assert_eq!(c.cycle_focus(CycleDirection::Next), Some(WindowId(2)));
        assert_eq!(c.cycle_focus(CycleDirection::Previous), Some(WindowId(1)));
        assert_eq!(
            c.cycle_focus(CycleDirection::Previous),
            Some(WindowId(3)),
            "cycling wraps"
        );
        assert_eq!(Container::new().cycle_focus(CycleDirection::Next), None);
    }

    #[test]
    fn metadata_is_reachable_by_handle() {
        let mut c = Container::from_window(Window::new(1).with_exe("a.exe"));
        assert_eq!(c.window(WindowId(1)).map(|w| w.exe.as_str()), Some("a.exe"));
        if let Some(w) = c.window_mut(WindowId(1)) {
            w.title = "hello".into();
        }
        assert_eq!(c.focused_window().map(|w| w.title.as_str()), Some("hello"));
        assert!(c.window(WindowId(2)).is_none());
        assert!(c.window_mut(WindowId(2)).is_none());
    }

    #[test]
    fn draining_empties_the_container() {
        let mut c = stack();
        let windows = c.drain();
        assert_eq!(windows.len(), 3);
        assert!(c.is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let c = stack();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Container>(&json).unwrap(), c);
    }
}
