//! The rectangle the testbed speaks in.
//!
//! This is deliberately a private copy rather than `mochi_core::Rect`: the
//! testbed must keep working when the crate under test is mid-refactor or does
//! not compile at all, so it depends on nothing from the workspace. The layout
//! is the same one Win32 uses, `right` and `bottom` are the exclusive far
//! edges, and every coordinate is a physical pixel in virtual screen space.

use serde::{Deserialize, Serialize};

/// A rectangle in physical pixels, far edges exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge, inclusive.
    pub left: i32,
    /// Top edge, inclusive.
    pub top: i32,
    /// Right edge, exclusive.
    pub right: i32,
    /// Bottom edge, exclusive.
    pub bottom: i32,
}

impl Rect {
    /// A rectangle from its four edges.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A rectangle from a corner and a size, which is how `SetWindowPos` thinks.
    #[must_use]
    pub const fn from_size(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::new(x, y, x + width, y + height)
    }

    /// Width in pixels. Negative for an inverted rectangle.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    /// Height in pixels. Negative for an inverted rectangle.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// True when the rectangle covers no pixel at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// Area in pixels. `i64` because a 4K desktop overflows `i32` quickly.
    #[must_use]
    pub const fn area(&self) -> i64 {
        if self.is_empty() {
            return 0;
        }
        self.width() as i64 * self.height() as i64
    }

    /// The midpoint, rounded towards the top left.
    #[must_use]
    pub const fn center(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }

    /// The overlapping part of two rectangles, or `None` when they do not touch.
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let r = Self::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        );
        (!r.is_empty()).then_some(r)
    }

    /// True when the two rectangles share at least one pixel.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.intersection(other).is_some()
    }

    /// True when `other` lies entirely inside `self`. Empty rectangles are inside.
    #[must_use]
    pub fn contains(&self, other: &Self) -> bool {
        if other.is_empty() {
            return true;
        }
        other.left >= self.left
            && other.top >= self.top
            && other.right <= self.right
            && other.bottom <= self.bottom
    }

    /// Grows the rectangle by `dx` on both vertical edges and `dy` on both
    /// horizontal ones. Negative values shrink it.
    #[must_use]
    pub const fn inflate(&self, dx: i32, dy: i32) -> Self {
        Self::new(
            self.left - dx,
            self.top - dy,
            self.right + dx,
            self.bottom + dy,
        )
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{},{} {}x{}",
            self.left,
            self.top,
            self.width(),
            self.height()
        )
    }
}

#[cfg(windows)]
impl From<windows::Win32::Foundation::RECT> for Rect {
    fn from(r: windows::Win32::Foundation::RECT) -> Self {
        Self::new(r.left, r.top, r.right, r.bottom)
    }
}

#[cfg(windows)]
impl From<Rect> for windows::Win32::Foundation::RECT {
    fn from(r: Rect) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_helpers_agree_with_the_edges() {
        let r = Rect::from_size(10, 20, 300, 400);
        assert_eq!(r, Rect::new(10, 20, 310, 420));
        assert_eq!((r.width(), r.height()), (300, 400));
        assert_eq!(r.area(), 120_000);
        assert_eq!(r.center(), (160, 220));
        assert!(!r.is_empty());
    }

    #[test]
    fn an_inverted_or_flat_rect_is_empty_and_has_no_area() {
        assert!(Rect::new(10, 10, 10, 50).is_empty());
        assert!(Rect::new(10, 10, 5, 50).is_empty());
        assert_eq!(Rect::new(10, 10, 5, 50).area(), 0);
    }

    #[test]
    fn touching_rects_do_not_intersect() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(100, 0, 200, 100);
        assert!(!a.intersects(&b));
        assert_eq!(a.intersection(&b), None);
    }

    #[test]
    fn overlapping_rects_report_the_shared_area() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(90, 90, 200, 200);
        assert_eq!(a.intersection(&b), Some(Rect::new(90, 90, 100, 100)));
    }

    #[test]
    fn containment_is_inclusive_on_the_edges() {
        let area = Rect::new(0, 0, 100, 100);
        assert!(area.contains(&Rect::new(0, 0, 100, 100)));
        assert!(!area.contains(&Rect::new(0, 0, 101, 100)));
        assert!(area.contains(&Rect::default()));
    }

    #[test]
    fn inflate_moves_all_four_edges() {
        assert_eq!(
            Rect::new(10, 10, 20, 20).inflate(2, 3),
            Rect::new(8, 7, 22, 23)
        );
    }

    #[test]
    fn display_is_the_form_used_in_logs() {
        assert_eq!(
            Rect::from_size(0, 0, 1920, 1080).to_string(),
            "0,0 1920x1080"
        );
    }

    #[test]
    fn json_keeps_the_four_edges() {
        let json = serde_json::to_string(&Rect::new(1, 2, 3, 4)).unwrap();
        assert_eq!(json, r#"{"left":1,"top":2,"right":3,"bottom":4}"#);
        assert_eq!(
            serde_json::from_str::<Rect>(&json).unwrap(),
            Rect::new(1, 2, 3, 4)
        );
    }
}
