//! What changed since the last border pass. Pure code, no Win32.
//!
//! The daemon calls [`crate::BorderManager::update`] after every layout pass
//! and [`crate::BorderManager::follow_frame`] on every animation frame, which
//! at 144 fps is often. Both go through a [`BorderDiff`], so a border that has
//! not changed is not repainted, not moved, and not even sent to the border
//! thread.

use std::collections::BTreeMap;

use crate::WindowHandle;
use crate::animation::FrameUpdate;
use crate::border::BorderSpec;

/// The work one pass leaves for the border thread.
///
/// The three buckets are what the thread has to do, cheapest last:
/// [`BorderChanges::added`] needs a window, a paint and a position,
/// [`BorderChanges::repainted`] needs a paint because the colour changed, and
/// [`BorderChanges::moved`] only needs a `SetWindowPos`. A border that is in
/// none of them is left alone entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BorderChanges {
    /// Windows that had no border before.
    pub added: Vec<BorderSpec>,
    /// The kind changed, so the colour did: repaint, wherever it is now.
    pub repainted: Vec<BorderSpec>,
    /// Same kind, new rectangle: move it, do not touch its pixels.
    pub moved: Vec<BorderSpec>,
    /// Windows that no longer want a border.
    pub removed: Vec<WindowHandle>,
}

impl BorderChanges {
    /// `true` when nothing at all has to happen, which is the common case for
    /// an idle desktop.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.repainted.is_empty()
            && self.moved.is_empty()
            && self.removed.is_empty()
    }

    /// How many borders this pass touches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.added.len() + self.repainted.len() + self.moved.len() + self.removed.len()
    }

    /// Every spec that has to be handed to a border window, in the order the
    /// thread applies them.
    pub fn specs(&self) -> impl Iterator<Item = &BorderSpec> {
        self.added
            .iter()
            .chain(self.repainted.iter())
            .chain(self.moved.iter())
    }
}

/// The last state the border thread was told about.
///
/// Held by the manager handle, not by the thread, so an unchanged pass costs
/// one hash lookup per window and no cross thread traffic at all.
#[derive(Debug, Clone, Default)]
pub struct BorderDiff {
    last: BTreeMap<isize, BorderSpec>,
}

impl BorderDiff {
    /// An empty diff: nothing is on screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last: BTreeMap::new(),
        }
    }

    /// How many borders are on screen.
    #[must_use]
    pub fn len(&self) -> usize {
        self.last.len()
    }

    /// `true` when no border is on screen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.last.is_empty()
    }

    /// The kind a window's border currently has, if it has one.
    #[must_use]
    pub fn kind(&self, handle: WindowHandle) -> Option<crate::BorderKind> {
        self.last.get(&handle.0).map(|spec| spec.kind)
    }

    /// Forgets everything, so that the next pass re-adds every border.
    ///
    /// Used when the borders come off the screen and when the configuration
    /// changes, because a new configuration repaints whatever is left.
    pub fn invalidate(&mut self) {
        self.last.clear();
    }

    /// Diffs a complete desired set against the last one.
    ///
    /// A window named twice keeps its last spec. A spec whose kind changed is
    /// repainted even when it also moved, because a repaint puts the frame in
    /// its new place anyway.
    pub fn diff(&mut self, specs: Vec<BorderSpec>) -> BorderChanges {
        let mut next: BTreeMap<isize, BorderSpec> = BTreeMap::new();
        let mut order: Vec<isize> = Vec::with_capacity(specs.len());
        for spec in specs {
            if next.insert(spec.target.0, spec).is_none() {
                order.push(spec.target.0);
            }
        }

        let mut changes = BorderChanges::default();
        for key in order {
            let spec = next[&key];
            match self.last.get(&key) {
                None => changes.added.push(spec),
                Some(previous) if previous.kind != spec.kind => changes.repainted.push(spec),
                Some(previous) if previous.rect != spec.rect => changes.moved.push(spec),
                Some(_) => {}
            }
        }
        changes.removed = self
            .last
            .keys()
            .filter(|key| !next.contains_key(key))
            .map(|key| WindowHandle(*key))
            .collect();

        self.last = next;
        changes
    }

    /// Follows one animation frame: the borders of the windows that already
    /// have one move with them.
    ///
    /// Nothing is added and nothing is taken down, because an animation frame
    /// says where windows are, not which of them should have a border. A window
    /// without a border is skipped, and a frame that did not actually move a
    /// window costs nothing.
    pub fn follow(&mut self, frame: &[FrameUpdate]) -> BorderChanges {
        let mut changes = BorderChanges::default();
        for update in frame {
            let Some(spec) = self.last.get_mut(&update.handle.0) else {
                continue;
            };
            if spec.rect == update.rect {
                continue;
            }
            spec.rect = update.rect;
            changes.moved.push(*spec);
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use mochi_core::Rect;

    use super::*;
    use crate::BorderKind;

    const A: WindowHandle = WindowHandle(0x1111);
    const B: WindowHandle = WindowHandle(0x2222);

    fn spec(handle: WindowHandle, rect: Rect, kind: BorderKind) -> BorderSpec {
        BorderSpec::new(handle, rect, kind)
    }

    const LEFT: Rect = Rect::new(0, 0, 800, 600);
    const RIGHT: Rect = Rect::new(800, 0, 1600, 600);

    #[test]
    fn the_first_pass_adds_everything() {
        let mut diff = BorderDiff::new();
        assert!(diff.is_empty());

        let changes = diff.diff(vec![
            spec(A, LEFT, BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ]);
        assert_eq!(changes.added.len(), 2);
        assert_eq!(changes.added[0].target, A, "the input order is kept");
        assert!(changes.moved.is_empty());
        assert!(changes.repainted.is_empty());
        assert!(changes.removed.is_empty());
        assert_eq!(diff.len(), 2);
    }

    #[test]
    fn the_same_pass_twice_does_nothing_at_all() {
        let mut diff = BorderDiff::new();
        let specs = vec![
            spec(A, LEFT, BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ];
        let _ = diff.diff(specs.clone());

        let changes = diff.diff(specs);
        assert!(changes.is_empty(), "an idle desktop costs nothing");
        assert_eq!(changes.len(), 0);
    }

    #[test]
    fn a_window_that_only_moved_is_moved_not_repainted() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![
            spec(A, LEFT, BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ]);

        let changes = diff.diff(vec![
            spec(A, Rect::new(10, 10, 810, 610), BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ]);
        assert_eq!(changes.moved.len(), 1);
        assert_eq!(changes.moved[0].target, A);
        assert!(changes.repainted.is_empty(), "the colour did not change");
        assert!(changes.added.is_empty());
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_focus_change_repaints_both_windows_and_moves_neither() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![
            spec(A, LEFT, BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ]);

        let changes = diff.diff(vec![
            spec(A, LEFT, BorderKind::Unfocused),
            spec(B, RIGHT, BorderKind::Single),
        ]);
        assert_eq!(changes.repainted.len(), 2);
        assert!(changes.moved.is_empty());
        assert_eq!(changes.specs().count(), 2);
    }

    #[test]
    fn a_window_that_changed_kind_and_moved_is_repainted_once() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![spec(A, LEFT, BorderKind::Single)]);

        let changes = diff.diff(vec![spec(A, RIGHT, BorderKind::Stack)]);
        assert_eq!(changes.repainted.len(), 1);
        assert_eq!(changes.repainted[0].rect, RIGHT);
        assert!(changes.moved.is_empty());
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn a_window_that_is_gone_is_taken_down() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![
            spec(A, LEFT, BorderKind::Single),
            spec(B, RIGHT, BorderKind::Unfocused),
        ]);

        let changes = diff.diff(vec![spec(A, LEFT, BorderKind::Single)]);
        assert_eq!(changes.removed, vec![B]);
        assert!(changes.added.is_empty());
        assert_eq!(diff.len(), 1);

        let changes = diff.diff(Vec::new());
        assert_eq!(changes.removed, vec![A]);
        assert!(diff.is_empty());
    }

    #[test]
    fn a_window_named_twice_keeps_its_last_spec() {
        let mut diff = BorderDiff::new();
        let changes = diff.diff(vec![
            spec(A, LEFT, BorderKind::Unfocused),
            spec(A, RIGHT, BorderKind::Single),
        ]);
        assert_eq!(changes.added.len(), 1);
        assert_eq!(changes.added[0].kind, BorderKind::Single);
        assert_eq!(changes.added[0].rect, RIGHT);
        assert_eq!(diff.kind(A), Some(BorderKind::Single));
    }

    #[test]
    fn invalidating_re_adds_everything_next_time() {
        let mut diff = BorderDiff::new();
        let specs = vec![spec(A, LEFT, BorderKind::Single)];
        let _ = diff.diff(specs.clone());

        diff.invalidate();
        assert!(diff.is_empty());
        let changes = diff.diff(specs);
        assert_eq!(changes.added.len(), 1, "a new configuration repaints");
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_frame_moves_the_borders_that_exist_and_adds_none() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![spec(A, LEFT, BorderKind::Single)]);

        let frame = [
            FrameUpdate {
                handle: A,
                rect: Rect::new(100, 0, 900, 600),
                finished: false,
            },
            FrameUpdate {
                handle: B,
                rect: RIGHT,
                finished: false,
            },
        ];
        let changes = diff.follow(&frame);
        assert_eq!(changes.moved.len(), 1, "B has no border to move");
        assert_eq!(changes.moved[0].target, A);
        assert_eq!(
            changes.moved[0].kind,
            BorderKind::Single,
            "the kind rides along"
        );
        assert!(changes.added.is_empty());
        assert!(changes.removed.is_empty());
    }

    #[test]
    fn a_frame_that_moved_nothing_costs_nothing() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![spec(A, LEFT, BorderKind::Single)]);

        let frame = [FrameUpdate {
            handle: A,
            rect: LEFT,
            finished: true,
        }];
        assert!(diff.follow(&frame).is_empty());
    }

    #[test]
    fn the_layout_pass_after_an_animation_sees_no_change() {
        let mut diff = BorderDiff::new();
        let _ = diff.diff(vec![spec(A, LEFT, BorderKind::Single)]);

        // The animation walks the window to its target and the borders follow.
        let last = FrameUpdate {
            handle: A,
            rect: RIGHT,
            finished: true,
        };
        assert_eq!(diff.follow(&[last]).moved.len(), 1);

        // The daemon then declares the same end state it asked for.
        let changes = diff.diff(vec![spec(A, RIGHT, BorderKind::Single)]);
        assert!(changes.is_empty(), "the frame already put it there");
    }
}
