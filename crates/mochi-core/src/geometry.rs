//! Pure geometry: rectangles, axes and directions.
//!
//! [`Rect`] stores absolute screen coordinates. `right` and `bottom` are the
//! exclusive far edges, *not* a width and a height. Coordinates are physical
//! pixels; scaling for DPI is the daemon's job.

use serde::{Deserialize, Serialize};

/// The dimension an operation works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Axis {
    /// The x dimension: widths, left and right edges.
    Horizontal,
    /// The y dimension: heights, top and bottom edges.
    Vertical,
}

impl Axis {
    /// The other axis.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

impl std::fmt::Display for Axis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Horizontal => f.write_str("horizontal"),
            Self::Vertical => f.write_str("vertical"),
        }
    }
}

impl std::str::FromStr for Axis {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "horizontal" | "h" | "x" => Ok(Self::Horizontal),
            "vertical" | "v" | "y" => Ok(Self::Vertical),
            _ => Err(crate::Error::Parse {
                kind: "axis",
                value: s.to_string(),
            }),
        }
    }
}

/// A cardinal direction, used for directional focus and movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Direction {
    /// Towards smaller x.
    Left,
    /// Towards larger x.
    Right,
    /// Towards smaller y.
    Up,
    /// Towards larger y.
    Down,
}

impl Direction {
    /// Every direction, in a stable order.
    pub const ALL: [Direction; 4] = [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ];

    /// The direction pointing the other way.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }

    /// The axis this direction moves along.
    #[must_use]
    pub const fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    /// `true` for the directions that increase the coordinate along their axis.
    #[must_use]
    pub const fn is_forward(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Left => f.write_str("left"),
            Self::Right => f.write_str("right"),
            Self::Up => f.write_str("up"),
            Self::Down => f.write_str("down"),
        }
    }
}

impl std::str::FromStr for Direction {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "left" | "h" => Ok(Self::Left),
            "right" | "l" => Ok(Self::Right),
            "up" | "k" => Ok(Self::Up),
            "down" | "j" => Ok(Self::Down),
            _ => Err(crate::Error::Parse {
                kind: "direction",
                value: s.to_string(),
            }),
        }
    }
}

/// Per-edge pixel deltas that shrink (positive) or grow (negative) an area.
///
/// This is the shape the existing config format uses for `work_area_offset`,
/// so an existing config migrates unchanged.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Offset {
    /// Pixels taken off the left edge.
    #[serde(default)]
    pub left: i32,
    /// Pixels taken off the top edge.
    #[serde(default)]
    pub top: i32,
    /// Pixels taken off the right edge.
    #[serde(default)]
    pub right: i32,
    /// Pixels taken off the bottom edge.
    #[serde(default)]
    pub bottom: i32,
}

impl Offset {
    /// A new offset.
    #[must_use]
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Applies the offset to `area`, shrinking it on each side.
    ///
    /// Saturating, because these four numbers come straight out of the
    /// configuration file with no validation and are applied on the tiling
    /// path. A `left` of `i32::MAX` wraps in a release build rather than
    /// panicking, and a wrapped edge is worse than a clamped one: the
    /// rectangle inverts, `width()` wraps in turn, and windows are positioned
    /// at nonsense coordinates somewhere off screen. Two offsets are applied in
    /// sequence here, so two individually sane values can reach the same place.
    #[must_use]
    pub const fn apply(&self, area: Rect) -> Rect {
        Rect::new(
            area.left.saturating_add(self.left),
            area.top.saturating_add(self.top),
            area.right.saturating_sub(self.right),
            area.bottom.saturating_sub(self.bottom),
        )
    }

    /// The same offset in the physical pixels of a display at `scale`.
    ///
    /// Offsets are written in logical pixels like the paddings, because they
    /// exist to reserve room for a status bar and a bar is sized in logical
    /// pixels too. Applying them raw reserved the right number of pixels only
    /// at 100 percent: at 150 percent a 40 pixel bar occupies 60 physical
    /// pixels and the top row of tiles sat 20 pixels underneath it.
    #[must_use]
    pub fn scaled(self, scale: f32) -> Self {
        if scale == 1.0 {
            return self;
        }
        let at = |v: i32| (v as f32 * scale).round() as i32;
        Self::new(at(self.left), at(self.top), at(self.right), at(self.bottom))
    }
}

/// An axis-aligned rectangle in physical screen pixels.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Rect {
    /// The inclusive left edge.
    pub left: i32,
    /// The inclusive top edge.
    pub top: i32,
    /// The exclusive right edge.
    pub right: i32,
    /// The exclusive bottom edge.
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

    /// A rectangle from an origin plus a width and a height.
    #[must_use]
    pub const fn from_size(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::new(x, y, x + width, y + height)
    }

    /// The width in pixels. Negative for an inverted rectangle.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    /// The height in pixels. Negative for an inverted rectangle.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    /// `true` when the rectangle covers no pixels.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// The covered pixel count. Widened so that 4K rectangles cannot overflow.
    #[must_use]
    pub const fn area(&self) -> i64 {
        if self.is_empty() {
            return 0;
        }
        self.width() as i64 * self.height() as i64
    }

    /// The x coordinate of the centre.
    #[must_use]
    pub const fn center_x(&self) -> i32 {
        self.left + self.width() / 2
    }

    /// The y coordinate of the centre.
    #[must_use]
    pub const fn center_y(&self) -> i32 {
        self.top + self.height() / 2
    }

    /// The centre point.
    #[must_use]
    pub const fn center(&self) -> (i32, i32) {
        (self.center_x(), self.center_y())
    }

    /// The centre coordinate along `axis`.
    #[must_use]
    pub const fn center_on(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.center_x(),
            Axis::Vertical => self.center_y(),
        }
    }

    /// The near edge along `axis`.
    #[must_use]
    pub const fn start(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.left,
            Axis::Vertical => self.top,
        }
    }

    /// The far edge along `axis`.
    #[must_use]
    pub const fn end(&self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.right,
            Axis::Vertical => self.bottom,
        }
    }

    /// The width or the height, depending on `axis`.
    #[must_use]
    pub const fn extent(&self, axis: Axis) -> i32 {
        self.end(axis) - self.start(axis)
    }

    /// Shrinks the rectangle by `padding` on every side.
    ///
    /// The result can be inverted; use [`Rect::padded_clamped`] when the input
    /// may be smaller than twice the padding.
    #[must_use]
    pub const fn padded(&self, padding: i32) -> Self {
        Self::new(
            self.left + padding,
            self.top + padding,
            self.right - padding,
            self.bottom - padding,
        )
    }

    /// Shrinks the rectangle by at most `padding` on every side, never
    /// inverting it and never taking its last pixel.
    ///
    /// A rectangle that has at least one pixel in a dimension keeps at least
    /// one, because a window with a zero width or height is a window nobody can
    /// see or click.
    #[must_use]
    pub fn padded_clamped(&self, padding: i32) -> Self {
        if padding <= 0 {
            return *self;
        }
        let x = padding.min((self.width() - 1).max(0) / 2);
        let y = padding.min((self.height() - 1).max(0) / 2);
        Self::new(self.left + x, self.top + y, self.right - x, self.bottom - y)
    }

    /// Moves the rectangle without resizing it.
    #[must_use]
    pub const fn translated(&self, dx: i32, dy: i32) -> Self {
        Self::new(
            self.left + dx,
            self.top + dy,
            self.right + dx,
            self.bottom + dy,
        )
    }

    /// `true` when the point lies inside, using half-open edges.
    #[must_use]
    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// `true` when `other` lies entirely inside `self`.
    #[must_use]
    pub const fn contains_rect(&self, other: &Rect) -> bool {
        other.left >= self.left
            && other.top >= self.top
            && other.right <= self.right
            && other.bottom <= self.bottom
    }

    /// `true` when the two rectangles share at least one pixel.
    #[must_use]
    pub const fn intersects(&self, other: &Rect) -> bool {
        self.left < other.right
            && other.left < self.right
            && self.top < other.bottom
            && other.top < self.bottom
    }

    /// The shared area, or `None` when the rectangles are disjoint.
    #[must_use]
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        if !self.intersects(other) {
            return None;
        }
        Some(Rect::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        ))
    }

    /// The smallest rectangle containing both.
    #[must_use]
    pub fn union(&self, other: &Rect) -> Rect {
        Rect::new(
            self.left.min(other.left),
            self.top.min(other.top),
            self.right.max(other.right),
            self.bottom.max(other.bottom),
        )
    }

    /// `true` when the two rectangles overlap along `axis`.
    #[must_use]
    pub const fn overlaps_on(&self, other: &Rect, axis: Axis) -> bool {
        self.start(axis) < other.end(axis) && other.start(axis) < self.end(axis)
    }

    /// Splits at the absolute x coordinate `x` into a left and a right part.
    #[must_use]
    pub const fn split_at_x(&self, x: i32) -> (Rect, Rect) {
        (
            Rect::new(self.left, self.top, x, self.bottom),
            Rect::new(x, self.top, self.right, self.bottom),
        )
    }

    /// Splits at the absolute y coordinate `y` into a top and a bottom part.
    #[must_use]
    pub const fn split_at_y(&self, y: i32) -> (Rect, Rect) {
        (
            Rect::new(self.left, self.top, self.right, y),
            Rect::new(self.left, y, self.right, self.bottom),
        )
    }

    /// Splits the given dimension at the absolute coordinate `at`.
    #[must_use]
    pub const fn split(&self, axis: Axis, at: i32) -> (Rect, Rect) {
        match axis {
            Axis::Horizontal => self.split_at_x(at),
            Axis::Vertical => self.split_at_y(at),
        }
    }

    /// Splits the given dimension down the middle.
    #[must_use]
    pub const fn halve(&self, axis: Axis) -> (Rect, Rect) {
        self.split(axis, self.start(axis) + self.extent(axis) / 2)
    }

    /// Mirrors the rectangle inside `within` along `axis`.
    #[must_use]
    pub const fn mirrored(&self, within: &Rect, axis: Axis) -> Rect {
        match axis {
            Axis::Horizontal => Rect::new(
                within.left + within.right - self.right,
                self.top,
                within.left + within.right - self.left,
                self.bottom,
            ),
            Axis::Vertical => Rect::new(
                self.left,
                within.top + within.bottom - self.bottom,
                self.right,
                within.top + within.bottom - self.top,
            ),
        }
    }

    /// `true` when `other` lies wholly beyond `self` in `direction`.
    ///
    /// The test is on the facing edges, not the centres, because tiles and
    /// monitors never overlap: a rectangle that starts before `self` ends is
    /// not in that direction at all, however far its centre has drifted.
    #[must_use]
    pub const fn is_in_direction(&self, other: &Rect, direction: Direction) -> bool {
        match direction {
            Direction::Left => other.right <= self.left,
            Direction::Right => other.left >= self.right,
            Direction::Up => other.bottom <= self.top,
            Direction::Down => other.top >= self.bottom,
        }
    }

    /// The gap in pixels between the two facing edges when `other` lies in
    /// `direction`, or `None` when it does not.
    ///
    /// The gap is `0` for rectangles that touch or overlap along the axis.
    #[must_use]
    pub fn distance_in_direction(&self, other: &Rect, direction: Direction) -> Option<i32> {
        if !self.is_in_direction(other, direction) {
            return None;
        }
        let gap = match direction {
            Direction::Left => self.left - other.right,
            Direction::Right => other.left - self.right,
            Direction::Up => self.top - other.bottom,
            Direction::Down => other.top - self.bottom,
        };
        Some(gap.max(0))
    }

    /// Linearly interpolates towards `other`. `t` is clamped to `0.0..=1.0`.
    #[must_use]
    pub fn lerp(&self, other: &Rect, t: f64) -> Rect {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: i32, b: i32| a + ((f64::from(b - a) * t).round() as i32);
        Rect::new(
            mix(self.left, other.left),
            mix(self.top, other.top),
            mix(self.right, other.right),
            mix(self.bottom, other.bottom),
        )
    }
}

/// Picks the rectangle nearest to `rects[from]` in `direction`.
///
/// Candidates that overlap the origin on the perpendicular axis win over ones
/// that do not, then the smallest gap wins, then the closest centre, then the
/// lowest index. Returns `None` when nothing lies in that direction.
#[must_use]
pub fn nearest_in_direction(rects: &[Rect], from: usize, direction: Direction) -> Option<usize> {
    let origin = rects.get(from)?;
    let perpendicular = direction.axis().other();

    rects
        .iter()
        .enumerate()
        .filter(|(idx, _)| *idx != from)
        .filter_map(|(idx, candidate)| {
            let gap = origin.distance_in_direction(candidate, direction)?;
            let aligned = !origin.overlaps_on(candidate, perpendicular);
            let offset =
                (origin.center_on(perpendicular) - candidate.center_on(perpendicular)).abs();
            Some((aligned, gap, offset, idx))
        })
        .min()
        .map(|(_, _, _, idx)| idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_shrinks_every_side() {
        let r = Rect::new(0, 0, 100, 50).padded(10);
        assert_eq!(r, Rect::new(10, 10, 90, 40));
        assert_eq!(r.width(), 80);
        assert_eq!(r.height(), 30);
    }

    #[test]
    fn padded_clamped_never_inverts() {
        let r = Rect::new(0, 0, 10, 4).padded_clamped(50);
        assert!(!r.is_empty() || r.width() == 0);
        assert!(r.width() >= 0 && r.height() >= 0);
        assert_eq!(
            Rect::new(0, 0, 100, 100).padded_clamped(0),
            Rect::new(0, 0, 100, 100)
        );
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(0, 0));
        assert!(!r.contains(10, 10));
    }

    #[test]
    fn from_size_round_trips() {
        let r = Rect::from_size(10, 20, 30, 40);
        assert_eq!(r, Rect::new(10, 20, 40, 60));
        assert_eq!((r.width(), r.height()), (30, 40));
    }

    #[test]
    fn area_is_zero_for_inverted() {
        assert_eq!(Rect::new(0, 0, 10, 10).area(), 100);
        assert_eq!(Rect::new(10, 10, 0, 0).area(), 0);
        assert_eq!(Rect::new(0, 0, 3840, 2160).area(), 8_294_400);
    }

    #[test]
    fn centre_is_the_middle() {
        let r = Rect::new(0, 0, 100, 50);
        assert_eq!(r.center(), (50, 25));
        assert_eq!(r.center_on(Axis::Horizontal), 50);
        assert_eq!(r.center_on(Axis::Vertical), 25);
    }

    #[test]
    fn intersection_and_union() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 50, 150, 150);
        assert_eq!(a.intersection(&b), Some(Rect::new(50, 50, 100, 100)));
        assert_eq!(a.union(&b), Rect::new(0, 0, 150, 150));

        let c = Rect::new(200, 200, 300, 300);
        assert_eq!(a.intersection(&c), None);
        assert!(!a.intersects(&c));
    }

    #[test]
    fn touching_rects_do_not_intersect() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(100, 0, 200, 100);
        assert!(!a.intersects(&b));
        assert_eq!(a.intersection(&b), None);
    }

    #[test]
    fn contains_rect_is_inclusive() {
        let outer = Rect::new(0, 0, 100, 100);
        assert!(outer.contains_rect(&outer));
        assert!(outer.contains_rect(&Rect::new(10, 10, 90, 90)));
        assert!(!outer.contains_rect(&Rect::new(10, 10, 110, 90)));
    }

    #[test]
    fn splits_cover_the_whole_rect() {
        let r = Rect::new(0, 0, 100, 60);
        let (a, b) = r.split_at_x(30);
        assert_eq!(a, Rect::new(0, 0, 30, 60));
        assert_eq!(b, Rect::new(30, 0, 100, 60));
        assert_eq!(a.area() + b.area(), r.area());

        let (t, d) = r.split_at_y(20);
        assert_eq!(t, Rect::new(0, 0, 100, 20));
        assert_eq!(d, Rect::new(0, 20, 100, 60));
        assert_eq!(t.area() + d.area(), r.area());
    }

    #[test]
    fn halve_splits_down_the_middle() {
        let r = Rect::new(0, 0, 100, 60);
        assert_eq!(r.halve(Axis::Horizontal).0, Rect::new(0, 0, 50, 60));
        assert_eq!(r.halve(Axis::Vertical).1, Rect::new(0, 30, 100, 60));
    }

    #[test]
    fn mirroring_is_an_involution() {
        let area = Rect::new(0, 0, 100, 100);
        let r = Rect::new(0, 0, 40, 100);
        let flipped = r.mirrored(&area, Axis::Horizontal);
        assert_eq!(flipped, Rect::new(60, 0, 100, 100));
        assert_eq!(flipped.mirrored(&area, Axis::Horizontal), r);

        let v = r.mirrored(&area, Axis::Vertical);
        assert_eq!(v.mirrored(&area, Axis::Vertical), r);
    }

    #[test]
    fn direction_helpers() {
        assert_eq!(Direction::Left.opposite(), Direction::Right);
        assert_eq!(Direction::Up.axis(), Axis::Vertical);
        assert!(Direction::Down.is_forward());
        assert!(!Direction::Up.is_forward());
        assert_eq!(Axis::Horizontal.other(), Axis::Vertical);
    }

    #[test]
    fn distance_in_direction_reports_the_gap() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(120, 0, 220, 100);
        assert_eq!(a.distance_in_direction(&b, Direction::Right), Some(20));
        assert_eq!(b.distance_in_direction(&a, Direction::Left), Some(20));
        assert_eq!(a.distance_in_direction(&b, Direction::Left), None);
        assert_eq!(a.distance_in_direction(&b, Direction::Up), None);
    }

    #[test]
    fn touching_rects_have_a_zero_gap() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(100, 0, 200, 100);
        assert_eq!(a.distance_in_direction(&b, Direction::Right), Some(0));
    }

    #[test]
    fn an_overlapping_rect_is_in_no_direction_at_all() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(50, 0, 150, 100);
        for direction in Direction::ALL {
            assert_eq!(a.distance_in_direction(&b, direction), None, "{direction}");
        }
    }

    #[test]
    fn nearest_in_direction_prefers_the_aligned_neighbour() {
        // A BSP-like arrangement: one tall window on the left, two stacked right.
        let rects = [
            Rect::new(0, 0, 500, 1000),
            Rect::new(500, 0, 1000, 500),
            Rect::new(500, 500, 1000, 1000),
        ];
        assert_eq!(
            nearest_in_direction(&rects, 1, Direction::Left),
            Some(0),
            "left of the top-right window is the tall one"
        );
        assert_eq!(nearest_in_direction(&rects, 1, Direction::Down), Some(2));
        assert_eq!(nearest_in_direction(&rects, 2, Direction::Up), Some(1));
        assert_eq!(nearest_in_direction(&rects, 0, Direction::Left), None);
        assert_eq!(nearest_in_direction(&rects, 1, Direction::Right), None);
    }

    #[test]
    fn nearest_in_direction_skips_misaligned_candidates() {
        let rects = [
            Rect::new(0, 0, 100, 100),
            // directly to the right
            Rect::new(100, 0, 200, 100),
            // further right and far below: same gap class, worse alignment
            Rect::new(100, 900, 200, 1000),
        ];
        assert_eq!(nearest_in_direction(&rects, 0, Direction::Right), Some(1));
    }

    #[test]
    fn nearest_in_direction_handles_an_out_of_range_origin() {
        assert_eq!(nearest_in_direction(&[], 0, Direction::Left), None);
    }

    #[test]
    fn lerp_moves_from_start_to_end() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(100, 100, 300, 300);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
        assert_eq!(a.lerp(&b, 0.5), Rect::new(50, 50, 200, 200));
        assert_eq!(a.lerp(&b, 5.0), b, "t is clamped");
        assert_eq!(a.lerp(&b, -1.0), a, "t is clamped");
    }

    #[test]
    fn offset_shrinks_the_work_area() {
        let area = Rect::new(0, 0, 1920, 1080);
        let offset = Offset::new(0, 40, 0, 40);
        assert_eq!(offset.apply(area), Rect::new(0, 40, 1920, 1040));
        assert_eq!(Offset::default().apply(area), area);
    }

    #[test]
    fn axis_and_direction_parse_from_cli_words() {
        use std::str::FromStr;
        assert_eq!(Axis::from_str("horizontal"), Ok(Axis::Horizontal));
        assert_eq!(Axis::from_str("VERTICAL"), Ok(Axis::Vertical));
        assert!(Axis::from_str("diagonal").is_err());
        assert_eq!(Direction::from_str("left"), Ok(Direction::Left));
        assert_eq!(Direction::from_str("j"), Ok(Direction::Down));
        assert!(Direction::from_str("sideways").is_err());
        assert_eq!(Direction::Left.to_string(), "left");
        assert_eq!(Axis::Vertical.to_string(), "vertical");
    }
}
