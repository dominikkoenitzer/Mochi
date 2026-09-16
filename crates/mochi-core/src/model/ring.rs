//! A list with a focused element, used at every level of the tree.

use serde::{Deserialize, Serialize};

/// Which way a cycling operation moves through a [`Ring`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum CycleDirection {
    /// Towards lower indices, wrapping to the end.
    Previous,
    /// Towards higher indices, wrapping to the start.
    Next,
}

impl CycleDirection {
    /// The other way round.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Previous => Self::Next,
            Self::Next => Self::Previous,
        }
    }

    /// The index `len` elements can move to from `idx`, wrapping at both ends.
    ///
    /// Returns `None` for an empty ring.
    #[must_use]
    pub const fn step(self, idx: usize, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        Some(match self {
            Self::Next => (idx + 1) % len,
            Self::Previous => {
                if idx == 0 {
                    len - 1
                } else {
                    idx - 1
                }
            }
        })
    }
}

impl std::fmt::Display for CycleDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Previous => f.write_str("previous"),
            Self::Next => f.write_str("next"),
        }
    }
}

impl std::str::FromStr for CycleDirection {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "previous" | "prev" => Ok(Self::Previous),
            "next" => Ok(Self::Next),
            _ => Err(crate::Error::Parse {
                kind: "cycle direction",
                value: s.to_string(),
            }),
        }
    }
}

/// A `Vec` that remembers which element is focused.
///
/// Every level of the tree is a ring: monitors in the state, workspaces on a
/// monitor, containers in a workspace and windows in a container. Mutating
/// methods keep the focus pointing at a sensible element, and the focus index
/// is clamped on read so a hand-edited state file can never panic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ring<T> {
    elements: Vec<T>,
    focused: usize,
}

impl<T> Default for Ring<T> {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            focused: 0,
        }
    }
}

impl<T> Ring<T> {
    /// An empty ring.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            elements: Vec::new(),
            focused: 0,
        }
    }

    /// A ring over `elements`, focused on the first one.
    #[must_use]
    pub const fn from_vec(elements: Vec<T>) -> Self {
        Self {
            elements,
            focused: 0,
        }
    }

    /// The number of elements.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.elements.len()
    }

    /// `true` when the ring holds nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// The elements in order.
    #[must_use]
    pub fn elements(&self) -> &[T] {
        &self.elements
    }

    /// The elements in order, mutably. The order cannot be changed this way.
    pub fn elements_mut(&mut self) -> &mut [T] {
        &mut self.elements
    }

    /// Consumes the ring and returns its elements.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.elements
    }

    /// Iterates the elements in order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.elements.iter()
    }

    /// Iterates the elements in order, mutably.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.elements.iter_mut()
    }

    /// The focused index, clamped into range. `0` for an empty ring.
    #[must_use]
    pub fn focused_idx(&self) -> usize {
        if self.elements.is_empty() {
            0
        } else {
            self.focused.min(self.elements.len() - 1)
        }
    }

    /// The focused element.
    #[must_use]
    pub fn focused(&self) -> Option<&T> {
        self.elements.get(self.focused_idx())
    }

    /// The focused element, mutably.
    pub fn focused_mut(&mut self) -> Option<&mut T> {
        let idx = self.focused_idx();
        self.elements.get_mut(idx)
    }

    /// The element at `idx`.
    #[must_use]
    pub fn get(&self, idx: usize) -> Option<&T> {
        self.elements.get(idx)
    }

    /// The element at `idx`, mutably.
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut T> {
        self.elements.get_mut(idx)
    }

    /// Focuses `idx`. Returns `false` and changes nothing when out of range.
    pub fn focus(&mut self, idx: usize) -> bool {
        if idx < self.elements.len() {
            self.focused = idx;
            true
        } else {
            false
        }
    }

    /// Focuses `idx`, clamped into range.
    pub fn focus_clamped(&mut self, idx: usize) {
        self.focused = idx.min(self.elements.len().saturating_sub(1));
    }

    /// The index one step away in `direction`, wrapping.
    #[must_use]
    pub fn next_idx(&self, direction: CycleDirection) -> Option<usize> {
        direction.step(self.focused_idx(), self.elements.len())
    }

    /// Moves the focus one step in `direction`, wrapping. Returns the new index.
    pub fn cycle_focus(&mut self, direction: CycleDirection) -> Option<usize> {
        let idx = self.next_idx(direction)?;
        self.focused = idx;
        Some(idx)
    }

    /// Swaps the focused element with its neighbour in `direction` and follows
    /// it. Returns the new index of the focused element.
    pub fn move_focused(&mut self, direction: CycleDirection) -> Option<usize> {
        let from = self.focused_idx();
        let to = self.next_idx(direction)?;
        if from == to {
            return Some(from);
        }
        self.elements.swap(from, to);
        self.focused = to;
        Some(to)
    }

    /// Appends an element without moving the focus.
    pub fn push(&mut self, element: T) {
        self.elements.push(element);
    }

    /// Appends an element and focuses it. Returns its index.
    pub fn push_focused(&mut self, element: T) -> usize {
        self.elements.push(element);
        self.focused = self.elements.len() - 1;
        self.focused
    }

    /// Inserts at `idx`, clamped to the end. The previously focused element
    /// stays focused.
    pub fn insert(&mut self, idx: usize, element: T) -> usize {
        let idx = idx.min(self.elements.len());
        let focused = self.focused_idx();
        self.elements.insert(idx, element);
        self.focused = if idx <= focused && self.elements.len() > 1 {
            focused + 1
        } else {
            focused
        };
        idx
    }

    /// Inserts at `idx`, clamped to the end, and focuses the new element.
    pub fn insert_focused(&mut self, idx: usize, element: T) -> usize {
        let idx = self.insert(idx, element);
        self.focused = idx;
        idx
    }

    /// Removes and returns the element at `idx`.
    ///
    /// The focus follows the element it was on, or stays in place when the
    /// focused element itself was removed.
    pub fn remove(&mut self, idx: usize) -> Option<T> {
        if idx >= self.elements.len() {
            return None;
        }
        let focused = self.focused_idx();
        let element = self.elements.remove(idx);
        self.focused = if idx < focused {
            focused - 1
        } else {
            focused.min(self.elements.len().saturating_sub(1))
        };
        Some(element)
    }

    /// Removes and returns the focused element.
    pub fn remove_focused(&mut self) -> Option<T> {
        if self.elements.is_empty() {
            return None;
        }
        self.remove(self.focused_idx())
    }

    /// Swaps two elements. Returns `false` when either index is out of range.
    pub fn swap(&mut self, a: usize, b: usize) -> bool {
        if a >= self.elements.len() || b >= self.elements.len() {
            return false;
        }
        self.elements.swap(a, b);
        true
    }

    /// The index of the first element matching `predicate`.
    pub fn position<F: FnMut(&T) -> bool>(&self, predicate: F) -> Option<usize> {
        self.elements.iter().position(predicate)
    }

    /// Drops every element and resets the focus.
    pub fn clear(&mut self) {
        self.elements.clear();
        self.focused = 0;
    }
}

impl<T> From<Vec<T>> for Ring<T> {
    fn from(elements: Vec<T>) -> Self {
        Self::from_vec(elements)
    }
}

impl<T> FromIterator<T> for Ring<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_vec(iter.into_iter().collect())
    }
}

impl<'a, T> IntoIterator for &'a Ring<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Ring<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T> IntoIterator for Ring<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.elements.into_iter()
    }
}

impl<T: schemars::JsonSchema> schemars::JsonSchema for Ring<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Ring_of_{}", T::schema_name()).into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let inner = generator.subschema_for::<Vec<T>>();
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "elements": inner,
                "focused": { "type": "integer", "minimum": 0 },
            },
            "required": ["elements", "focused"],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> Ring<u32> {
        Ring::from_vec(vec![0, 1, 2, 3])
    }

    #[test]
    fn a_new_ring_is_empty_and_focuses_nothing() {
        let r: Ring<u32> = Ring::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.focused(), None);
        assert_eq!(r.focused_idx(), 0);
        assert_eq!(r.next_idx(CycleDirection::Next), None);
    }

    #[test]
    fn focus_rejects_out_of_range() {
        let mut r = ring();
        assert!(r.focus(3));
        assert_eq!(r.focused(), Some(&3));
        assert!(!r.focus(4));
        assert_eq!(r.focused_idx(), 3);
        r.focus_clamped(99);
        assert_eq!(r.focused_idx(), 3);
    }

    #[test]
    fn cycle_focus_wraps_both_ways() {
        let mut r = ring();
        assert_eq!(r.cycle_focus(CycleDirection::Previous), Some(3));
        assert_eq!(r.cycle_focus(CycleDirection::Next), Some(0));
        for _ in 0..4 {
            r.cycle_focus(CycleDirection::Next);
        }
        assert_eq!(r.focused_idx(), 0, "a full lap returns to the start");
    }

    #[test]
    fn move_focused_swaps_and_follows() {
        let mut r = ring();
        assert_eq!(r.move_focused(CycleDirection::Next), Some(1));
        assert_eq!(r.elements(), &[1, 0, 2, 3]);
        assert_eq!(r.focused(), Some(&0));

        assert_eq!(r.move_focused(CycleDirection::Previous), Some(0));
        assert_eq!(r.elements(), &[0, 1, 2, 3]);
        assert_eq!(r.focused(), Some(&0));

        assert_eq!(r.move_focused(CycleDirection::Previous), Some(3));
        assert_eq!(r.elements(), &[3, 1, 2, 0]);
    }

    #[test]
    fn move_focused_on_a_single_element_is_a_no_op() {
        let mut r = Ring::from_vec(vec![7]);
        assert_eq!(r.move_focused(CycleDirection::Next), Some(0));
        assert_eq!(r.elements(), &[7]);
    }

    #[test]
    fn insert_keeps_the_same_element_focused() {
        let mut r = ring();
        r.focus(2);
        r.insert(0, 99);
        assert_eq!(r.elements(), &[99, 0, 1, 2, 3]);
        assert_eq!(r.focused(), Some(&2), "still on the same value");

        r.insert(5, 100);
        assert_eq!(
            r.focused(),
            Some(&2),
            "inserting after the focus changes nothing"
        );
    }

    #[test]
    fn insert_focused_focuses_the_new_element() {
        let mut r = ring();
        let idx = r.insert_focused(2, 99);
        assert_eq!(idx, 2);
        assert_eq!(r.focused(), Some(&99));
    }

    #[test]
    fn push_focused_focuses_the_tail() {
        let mut r = ring();
        assert_eq!(r.push_focused(9), 4);
        assert_eq!(r.focused(), Some(&9));
    }

    #[test]
    fn remove_before_the_focus_keeps_the_same_element() {
        let mut r = ring();
        r.focus(2);
        assert_eq!(r.remove(0), Some(0));
        assert_eq!(r.focused(), Some(&2));
    }

    #[test]
    fn removing_the_focus_lands_on_the_next_element() {
        let mut r = ring();
        r.focus(1);
        assert_eq!(r.remove(1), Some(1));
        assert_eq!(r.elements(), &[0, 2, 3]);
        assert_eq!(r.focused(), Some(&2));
    }

    #[test]
    fn removing_the_last_element_clamps_the_focus() {
        let mut r = ring();
        r.focus(3);
        assert_eq!(r.remove_focused(), Some(3));
        assert_eq!(r.focused(), Some(&2));
        assert_eq!(r.remove(99), None);
    }

    #[test]
    fn remove_from_an_empty_ring_is_none() {
        let mut r: Ring<u32> = Ring::new();
        assert_eq!(r.remove_focused(), None);
        assert_eq!(r.remove(0), None);
    }

    #[test]
    fn swap_validates_indices() {
        let mut r = ring();
        assert!(r.swap(0, 3));
        assert_eq!(r.elements(), &[3, 1, 2, 0]);
        assert!(!r.swap(0, 4));
    }

    #[test]
    fn a_hand_edited_focus_index_is_clamped_on_read() {
        let r: Ring<u32> = serde_json::from_str(r#"{"elements":[1,2],"focused":97}"#).unwrap();
        assert_eq!(r.focused_idx(), 1);
        assert_eq!(r.focused(), Some(&2));
    }

    #[test]
    fn round_trips_through_json() {
        let mut r = ring();
        r.focus(2);
        let json = serde_json::to_string(&r).unwrap();
        let back: Ring<u32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn iteration_and_collection() {
        let r: Ring<u32> = (0..3).collect();
        assert_eq!(r.iter().sum::<u32>(), 3);
        assert_eq!((&r).into_iter().count(), 3);
        let mut r2 = r.clone();
        for e in &mut r2 {
            *e += 1;
        }
        assert_eq!(r2.into_vec(), vec![1, 2, 3]);
        assert_eq!(r.position(|e| *e == 2), Some(2));
        assert_eq!(r.position(|e| *e == 9), None);
    }

    #[test]
    fn clear_resets_the_focus() {
        let mut r = ring();
        r.focus(3);
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.focused_idx(), 0);
    }

    #[test]
    fn cycle_direction_parses_and_inverts() {
        use std::str::FromStr;
        assert_eq!(CycleDirection::from_str("next"), Ok(CycleDirection::Next));
        assert_eq!(
            CycleDirection::from_str("prev"),
            Ok(CycleDirection::Previous)
        );
        assert!(CycleDirection::from_str("sideways").is_err());
        assert_eq!(CycleDirection::Next.opposite(), CycleDirection::Previous);
        assert_eq!(CycleDirection::Next.to_string(), "next");
        assert_eq!(CycleDirection::Next.step(0, 0), None);
    }
}
