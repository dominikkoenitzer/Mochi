//! Layouts: pure functions from an area and a container count to rectangles.
//!
//! Every layout tiles its area exactly. With a container padding of zero the
//! returned rectangles cover the area with no gap and no overlap, which is what
//! the tests assert for one to six containers.
//!
//! Resize deltas are per container [`Rect`]s of pixel offsets, one per edge.
//! A container's far-edge delta and its neighbour's near-edge delta push the
//! same boundary, and every boundary is clamped so no container can be squeezed
//! below a fifth of its fair share.

mod bsp;
pub mod split;

use serde::{Deserialize, Serialize};

use crate::geometry::{Axis, Rect};
use crate::model::CycleDirection;
pub use split::MIN_TILE_SIZE;
use split::{
    boundary_delta, boundary_deltas, divide, divide_weighted, extend_with_rects, rect_from_slice,
    shared_boundary_delta, slices_to_rects,
};

/// Whether a resize grows or shrinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
pub enum Sizing {
    /// Make the container bigger along the axis.
    Increase,
    /// Make the container smaller along the axis.
    Decrease,
}

impl Sizing {
    /// `delta` for an increase, `-delta` for a decrease.
    #[must_use]
    pub const fn signed(self, delta: i32) -> i32 {
        match self {
            Self::Increase => delta,
            Self::Decrease => -delta,
        }
    }

    /// The other way round.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Increase => Self::Decrease,
            Self::Decrease => Self::Increase,
        }
    }
}

impl std::fmt::Display for Sizing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Increase => f.write_str("increase"),
            Self::Decrease => f.write_str("decrease"),
        }
    }
}

impl std::str::FromStr for Sizing {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "increase" | "grow" | "+" => Ok(Self::Increase),
            "decrease" | "shrink" | "-" => Ok(Self::Decrease),
            _ => Err(crate::Error::Parse {
                kind: "sizing",
                value: s.to_string(),
            }),
        }
    }
}

/// Which axes a layout is mirrored on.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(default)]
pub struct Flip {
    /// Mirror left to right.
    pub horizontal: bool,
    /// Mirror top to bottom.
    pub vertical: bool,
}

impl Flip {
    /// No mirroring.
    pub const NONE: Flip = Flip {
        horizontal: false,
        vertical: false,
    };

    /// Mirrored left to right.
    #[must_use]
    pub const fn horizontal() -> Self {
        Self {
            horizontal: true,
            vertical: false,
        }
    }

    /// Mirrored top to bottom.
    #[must_use]
    pub const fn vertical() -> Self {
        Self {
            horizontal: false,
            vertical: true,
        }
    }

    /// Mirrored on both axes.
    #[must_use]
    pub const fn both() -> Self {
        Self {
            horizontal: true,
            vertical: true,
        }
    }

    /// `true` when nothing is mirrored.
    #[must_use]
    pub const fn is_none(self) -> bool {
        !self.horizontal && !self.vertical
    }

    /// Flips `axis` on or off.
    #[must_use]
    pub const fn toggled(self, axis: Axis) -> Self {
        match axis {
            Axis::Horizontal => Self {
                horizontal: !self.horizontal,
                ..self
            },
            Axis::Vertical => Self {
                vertical: !self.vertical,
                ..self
            },
        }
    }

    /// `true` when `axis` is mirrored.
    #[must_use]
    pub const fn is_flipped(self, axis: Axis) -> bool {
        match axis {
            Axis::Horizontal => self.horizontal,
            Axis::Vertical => self.vertical,
        }
    }

    /// Mirrors `rect` inside `area` on every flipped axis.
    #[must_use]
    pub fn apply(self, area: Rect, rect: Rect) -> Rect {
        let mut rect = rect;
        if self.horizontal {
            rect = rect.mirrored(&area, Axis::Horizontal);
        }
        if self.vertical {
            rect = rect.mirrored(&area, Axis::Vertical);
        }
        rect
    }
}

/// How containers are arranged inside a workspace.
///
/// The JSON names are the ones of the existing config format, so
/// `"layout": "BSP"` in a migrated config keeps working.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum Layout {
    /// Binary space partition: every new container halves the space left over
    /// by the previous one, alternating between vertical and horizontal splits.
    #[default]
    #[serde(rename = "BSP", alias = "Bsp", alias = "bsp")]
    Bsp,
    /// Equal width columns, left to right.
    Columns,
    /// Equal height rows, top to bottom.
    Rows,
    /// One container on the left, the rest stacked vertically on the right.
    VerticalStack,
    /// One container on top, the rest side by side underneath.
    HorizontalStack,
    /// The main container in the middle, the second one on the left and the
    /// rest stacked on the right. For very wide screens.
    UltrawideVerticalStack,
    /// A grid of roughly square cells.
    Grid,
}

impl Layout {
    /// Every layout, in the order `cycle-layout` walks through them.
    pub const ALL: [Layout; 7] = [
        Layout::Bsp,
        Layout::Columns,
        Layout::Rows,
        Layout::VerticalStack,
        Layout::HorizontalStack,
        Layout::UltrawideVerticalStack,
        Layout::Grid,
    ];

    /// The next or previous layout, wrapping.
    #[must_use]
    pub fn cycle(self, direction: CycleDirection) -> Self {
        let idx = Self::ALL.iter().position(|l| *l == self).unwrap_or(0);
        let next = direction.step(idx, Self::ALL.len()).unwrap_or(0);
        Self::ALL[next]
    }

    /// The rectangle for each of `len` containers.
    ///
    /// `area` is the workspace area with its workspace padding already taken
    /// off. `container_padding` is shrunk off every returned rectangle, never
    /// far enough to invert it. `flip` mirrors the finished arrangement.
    /// `resize_dimensions` holds one optional per-edge delta per container;
    /// shorter slices and `None` entries count as no delta.
    ///
    /// The returned vector always has exactly `len` elements.
    #[must_use]
    pub fn calculate(
        &self,
        area: Rect,
        len: usize,
        container_padding: i32,
        flip: Flip,
        resize_dimensions: &[Option<Rect>],
    ) -> Vec<Rect> {
        self.calculate_with_min(
            area,
            len,
            container_padding,
            flip,
            resize_dimensions,
            MIN_TILE_SIZE,
        )
    }

    /// The same as [`Layout::calculate`] with a minimum tile size of your own.
    ///
    /// [`MIN_TILE_SIZE`] is in logical pixels, so a daemon that tiles a display
    /// at 150 percent scaling passes `MIN_TILE_SIZE * 3 / 2` here. An area too
    /// small to give every tile that much scales the minimum down rather than
    /// leaving a gap.
    #[must_use]
    pub fn calculate_with_min(
        &self,
        area: Rect,
        len: usize,
        container_padding: i32,
        flip: Flip,
        resize_dimensions: &[Option<Rect>],
        min_tile_size: i32,
    ) -> Vec<Rect> {
        if len == 0 {
            return Vec::new();
        }
        let min = min_tile_size.max(0);

        let mut rects = match self {
            Self::Bsp => bsp::calculate(area, len, resize_dimensions, min),
            Self::Columns => columns(area, len, resize_dimensions, min),
            Self::Rows => rows(area, len, resize_dimensions, min),
            Self::VerticalStack => {
                main_and_stack(area, len, resize_dimensions, Axis::Horizontal, min)
            }
            Self::HorizontalStack => {
                main_and_stack(area, len, resize_dimensions, Axis::Vertical, min)
            }
            Self::UltrawideVerticalStack => ultrawide(area, len, resize_dimensions, min),
            Self::Grid => grid(area, len, resize_dimensions, min),
        };

        debug_assert_eq!(rects.len(), len, "{self:?} returned the wrong count");

        if !flip.is_none() {
            for rect in &mut rects {
                *rect = flip.apply(area, *rect);
            }
        }

        if container_padding > 0 {
            for rect in &mut rects {
                *rect = rect.padded_clamped(container_padding);
            }
        }

        rects
    }
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bsp => f.write_str("BSP"),
            Self::Columns => f.write_str("Columns"),
            Self::Rows => f.write_str("Rows"),
            Self::VerticalStack => f.write_str("VerticalStack"),
            Self::HorizontalStack => f.write_str("HorizontalStack"),
            Self::UltrawideVerticalStack => f.write_str("UltrawideVerticalStack"),
            Self::Grid => f.write_str("Grid"),
        }
    }
}

impl std::str::FromStr for Layout {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalised: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        match normalised.to_ascii_lowercase().as_str() {
            "bsp" => Ok(Self::Bsp),
            "columns" => Ok(Self::Columns),
            "rows" => Ok(Self::Rows),
            "verticalstack" => Ok(Self::VerticalStack),
            "horizontalstack" => Ok(Self::HorizontalStack),
            "ultrawideverticalstack" => Ok(Self::UltrawideVerticalStack),
            "grid" => Ok(Self::Grid),
            _ => Err(crate::Error::Parse {
                kind: "layout",
                value: s.to_string(),
            }),
        }
    }
}

fn columns(area: Rect, len: usize, resize: &[Option<Rect>], min: i32) -> Vec<Rect> {
    let deltas = boundary_deltas(resize, 0, len, Axis::Horizontal);
    let slices = divide(area.left, area.right, len, &deltas, min);
    let mut rects = Vec::with_capacity(len);
    extend_with_rects(&mut rects, area, Axis::Horizontal, &slices);
    rects
}

fn rows(area: Rect, len: usize, resize: &[Option<Rect>], min: i32) -> Vec<Rect> {
    let deltas = boundary_deltas(resize, 0, len, Axis::Vertical);
    let slices = divide(area.top, area.bottom, len, &deltas, min);
    let mut rects = Vec::with_capacity(len);
    extend_with_rects(&mut rects, area, Axis::Vertical, &slices);
    rects
}

/// One main container plus a stack of the rest.
///
/// `main_axis` is the axis the main container is split off along: horizontal
/// gives the main container the left half and stacks the rest vertically on
/// the right.
fn main_and_stack(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    main_axis: Axis,
    min: i32,
) -> Vec<Rect> {
    if len == 1 {
        return vec![area];
    }

    // Every container in the stack sits on the main cut, so a resize from any
    // of them moves it, not just one from the first.
    let split_delta = shared_boundary_delta(resize, 0..1, 1..len, main_axis);
    let halves = divide(
        area.start(main_axis),
        area.end(main_axis),
        2,
        &[split_delta],
        min,
    );
    let main = rect_from_slice(area, main_axis, halves[0].0, halves[0].1);
    let stack_area = rect_from_slice(area, main_axis, halves[1].0, halves[1].1);

    let stack_axis = main_axis.other();
    let stack_deltas = boundary_deltas(resize, 1, len - 1, stack_axis);
    let slices = divide(
        stack_area.start(stack_axis),
        stack_area.end(stack_axis),
        len - 1,
        &stack_deltas,
        min,
    );

    let mut rects = Vec::with_capacity(len);
    rects.push(main);
    extend_with_rects(&mut rects, stack_area, stack_axis, &slices);
    rects
}

fn ultrawide(area: Rect, len: usize, resize: &[Option<Rect>], min: i32) -> Vec<Rect> {
    match len {
        1 => vec![area],
        2 => {
            // Secondary on the left, primary on the right.
            let delta = boundary_delta(resize, 1, 0, Axis::Horizontal);
            let slices = divide(area.left, area.right, 2, &[delta], min);
            vec![
                rect_from_slice(area, Axis::Horizontal, slices[1].0, slices[1].1),
                rect_from_slice(area, Axis::Horizontal, slices[0].0, slices[0].1),
            ]
        }
        _ => {
            // A quarter for the secondary, a half for the primary, a quarter
            // for the stack.
            let left_delta = boundary_delta(resize, 1, 0, Axis::Horizontal);
            // The whole stack sits on the cut between the primary and itself.
            let right_delta = shared_boundary_delta(resize, 0..1, 2..len, Axis::Horizontal);
            let slices = divide_weighted(
                area.left,
                area.right,
                &[1, 2, 1],
                &[left_delta, right_delta],
                min,
            );
            let cell = |i: usize| rect_from_slice(area, Axis::Horizontal, slices[i].0, slices[i].1);
            let (secondary, primary, stack_area) = (cell(0), cell(1), cell(2));

            let stack_len = len - 2;
            let stack_deltas = boundary_deltas(resize, 2, stack_len, Axis::Vertical);
            let stack_slices = divide(
                stack_area.top,
                stack_area.bottom,
                stack_len,
                &stack_deltas,
                min,
            );

            let mut rects = Vec::with_capacity(len);
            rects.push(primary);
            rects.push(secondary);
            extend_with_rects(&mut rects, stack_area, Axis::Vertical, &stack_slices);
            rects
        }
    }
}

fn grid(area: Rect, len: usize, resize: &[Option<Rect>], min: i32) -> Vec<Rect> {
    let columns_count = grid_columns(len);
    let base = len / columns_count;
    let remainder = len % columns_count;

    // The leftover rows go to the rightmost columns, so the grid fills up from
    // the left the way a reading eye expects.
    let rows_per_column: Vec<usize> = (0..columns_count)
        .map(|c| base + usize::from(c >= columns_count - remainder))
        .collect();

    let first_of_column: Vec<usize> = rows_per_column
        .iter()
        .scan(0, |acc, n| {
            let first = *acc;
            *acc += n;
            Some(first)
        })
        .collect();

    // Every cell of the two columns either side of a column boundary sits on
    // it, so they all get a push at it, not just the top cell of each column.
    let end_of_column = |c: usize| first_of_column.get(c + 1).copied().unwrap_or(len);
    let column_deltas: Vec<i32> = (0..columns_count.saturating_sub(1))
        .map(|c| {
            shared_boundary_delta(
                resize,
                first_of_column[c]..end_of_column(c),
                first_of_column[c + 1]..end_of_column(c + 1),
                Axis::Horizontal,
            )
        })
        .collect();
    let column_slices = divide(area.left, area.right, columns_count, &column_deltas, min);
    let column_rects = slices_to_rects(area, Axis::Horizontal, &column_slices);

    let mut rects = Vec::with_capacity(len);
    for (column, rows_in_column) in rows_per_column.iter().copied().enumerate() {
        let column_rect = column_rects[column];
        let first = first_of_column[column];
        let deltas = boundary_deltas(resize, first, rows_in_column, Axis::Vertical);
        let slices = divide(
            column_rect.top,
            column_rect.bottom,
            rows_in_column,
            &deltas,
            min,
        );
        extend_with_rects(&mut rects, column_rect, Axis::Vertical, &slices);
    }
    rects
}

/// The number of columns a grid of `len` cells uses: the ceiling of the square
/// root, so the cells stay as square as possible.
fn grid_columns(len: usize) -> usize {
    if len == 0 {
        return 1;
    }
    let mut columns = 1_usize;
    while columns * columns < len {
        columns += 1;
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);

    /// The rectangles must tile `area` exactly: no overlap and full coverage.
    fn assert_tiles(rects: &[Rect], area: Rect) {
        let covered: i64 = rects.iter().map(Rect::area).sum();
        assert_eq!(
            covered,
            area.area(),
            "coverage mismatch for {rects:#?} in {area:?}"
        );
        for (i, a) in rects.iter().enumerate() {
            assert!(!a.is_empty(), "rect {i} is empty: {a:?}");
            assert!(area.contains_rect(a), "rect {i} escapes the area: {a:?}");
            for (j, b) in rects.iter().enumerate().skip(i + 1) {
                assert!(!a.intersects(b), "rects {i} and {j} overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn zero_containers_give_zero_rects() {
        for layout in Layout::ALL {
            assert!(
                layout.calculate(AREA, 0, 10, Flip::NONE, &[]).is_empty(),
                "{layout} with no containers"
            );
        }
    }

    #[test]
    fn every_layout_tiles_the_area_for_one_to_six_containers() {
        for layout in Layout::ALL {
            for len in 1..=6 {
                let rects = layout.calculate(AREA, len, 0, Flip::NONE, &[]);
                assert_eq!(rects.len(), len, "{layout} with {len}");
                assert_tiles(&rects, AREA);
            }
        }
    }

    #[test]
    fn every_layout_tiles_an_offset_and_a_portrait_area() {
        let areas = [
            Rect::new(1920, 0, 3000, 1920), // the portrait monitor on the right
            Rect::new(-1080, 200, 0, 1000), // a monitor left of the origin
            Rect::new(0, 0, 3840, 2160),
        ];
        for area in areas {
            for layout in Layout::ALL {
                for len in 1..=6 {
                    let rects = layout.calculate(area, len, 0, Flip::NONE, &[]);
                    assert_eq!(rects.len(), len);
                    assert_tiles(&rects, area);
                }
            }
        }
    }

    #[test]
    fn container_padding_shrinks_every_rect_symmetrically() {
        for layout in Layout::ALL {
            for len in 1..=6 {
                let bare = layout.calculate(AREA, len, 0, Flip::NONE, &[]);
                let padded = layout.calculate(AREA, len, 10, Flip::NONE, &[]);
                assert_eq!(bare.len(), padded.len());
                for (b, p) in bare.iter().zip(padded.iter()) {
                    assert_eq!(p.left, b.left + 10, "{layout}");
                    assert_eq!(p.top, b.top + 10, "{layout}");
                    assert_eq!(p.right, b.right - 10, "{layout}");
                    assert_eq!(p.bottom, b.bottom - 10, "{layout}");
                }
            }
        }
    }

    #[test]
    fn absurd_padding_never_inverts_a_rect() {
        for layout in Layout::ALL {
            for len in 1..=6 {
                for rect in layout.calculate(AREA, len, 5000, Flip::NONE, &[]) {
                    assert!(rect.width() >= 0 && rect.height() >= 0, "{layout} {rect:?}");
                }
            }
        }
    }

    #[test]
    fn a_tiny_area_never_panics() {
        for layout in Layout::ALL {
            for len in 0..=6 {
                for size in [0, 1, 3, 7] {
                    let area = Rect::new(0, 0, size, size);
                    let rects = layout.calculate(area, len, 2, Flip::NONE, &[]);
                    assert_eq!(rects.len(), len, "{layout} {len} {size}");
                }
            }
        }
    }

    #[test]
    fn columns_are_equal_and_ordered() {
        let rects = Layout::Columns.calculate(AREA, 4, 0, Flip::NONE, &[]);
        assert_eq!(rects[0], Rect::new(0, 0, 480, 1080));
        assert_eq!(rects[3], Rect::new(1440, 0, 1920, 1080));
        assert!(rects.windows(2).all(|w| w[0].left < w[1].left));
    }

    #[test]
    fn rows_are_equal_and_ordered() {
        let rects = Layout::Rows.calculate(AREA, 3, 0, Flip::NONE, &[]);
        assert_eq!(rects[0], Rect::new(0, 0, 1920, 360));
        assert_eq!(rects[2], Rect::new(0, 720, 1920, 1080));
    }

    #[test]
    fn vertical_stack_puts_the_main_container_on_the_left() {
        let one = Layout::VerticalStack.calculate(AREA, 1, 0, Flip::NONE, &[]);
        assert_eq!(one, vec![AREA]);

        let rects = Layout::VerticalStack.calculate(AREA, 3, 0, Flip::NONE, &[]);
        assert_eq!(
            rects[0],
            Rect::new(0, 0, 960, 1080),
            "main is the left half"
        );
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 540));
        assert_eq!(rects[2], Rect::new(960, 540, 1920, 1080));
    }

    #[test]
    fn horizontal_stack_puts_the_main_container_on_top() {
        let rects = Layout::HorizontalStack.calculate(AREA, 3, 0, Flip::NONE, &[]);
        assert_eq!(rects[0], Rect::new(0, 0, 1920, 540));
        assert_eq!(rects[1], Rect::new(0, 540, 960, 1080));
        assert_eq!(rects[2], Rect::new(960, 540, 1920, 1080));
    }

    #[test]
    fn ultrawide_centres_the_primary_container() {
        let one = Layout::UltrawideVerticalStack.calculate(AREA, 1, 0, Flip::NONE, &[]);
        assert_eq!(one, vec![AREA]);

        let two = Layout::UltrawideVerticalStack.calculate(AREA, 2, 0, Flip::NONE, &[]);
        assert_eq!(
            two[0],
            Rect::new(960, 0, 1920, 1080),
            "primary on the right"
        );
        assert_eq!(two[1], Rect::new(0, 0, 960, 1080), "secondary on the left");

        let four = Layout::UltrawideVerticalStack.calculate(AREA, 4, 0, Flip::NONE, &[]);
        assert_eq!(
            four[0],
            Rect::new(480, 0, 1440, 1080),
            "primary in the middle"
        );
        assert_eq!(four[1], Rect::new(0, 0, 480, 1080), "secondary on the left");
        assert_eq!(four[2], Rect::new(1440, 0, 1920, 540), "stack top right");
        assert_eq!(four[3], Rect::new(1440, 540, 1920, 1080));
    }

    #[test]
    fn grid_column_counts_are_square_ish() {
        assert_eq!(grid_columns(0), 1);
        assert_eq!(grid_columns(1), 1);
        assert_eq!(grid_columns(2), 2);
        assert_eq!(grid_columns(4), 2);
        assert_eq!(grid_columns(5), 3);
        assert_eq!(grid_columns(9), 3);
        assert_eq!(grid_columns(10), 4);
    }

    #[test]
    fn grid_fills_left_to_right() {
        let four = Layout::Grid.calculate(AREA, 4, 0, Flip::NONE, &[]);
        assert_eq!(four[0], Rect::new(0, 0, 960, 540));
        assert_eq!(four[1], Rect::new(0, 540, 960, 1080));
        assert_eq!(four[2], Rect::new(960, 0, 1920, 540));
        assert_eq!(four[3], Rect::new(960, 540, 1920, 1080));

        let three = Layout::Grid.calculate(AREA, 3, 0, Flip::NONE, &[]);
        assert_eq!(
            three[0],
            Rect::new(0, 0, 960, 1080),
            "the short column is first"
        );
        assert_eq!(three[1], Rect::new(960, 0, 1920, 540));
        assert_eq!(three[2], Rect::new(960, 540, 1920, 1080));
    }

    #[test]
    fn flipping_horizontally_mirrors_left_to_right() {
        let normal = Layout::Columns.calculate(AREA, 3, 0, Flip::NONE, &[]);
        let flipped = Layout::Columns.calculate(AREA, 3, 0, Flip::horizontal(), &[]);
        assert_eq!(flipped[0], normal[2]);
        assert_eq!(flipped[2], normal[0]);
        assert_tiles(&flipped, AREA);
    }

    #[test]
    fn flipping_vertically_mirrors_top_to_bottom() {
        let normal = Layout::Rows.calculate(AREA, 3, 0, Flip::NONE, &[]);
        let flipped = Layout::Rows.calculate(AREA, 3, 0, Flip::vertical(), &[]);
        assert_eq!(flipped[0], normal[2]);
        assert_eq!(flipped[2], normal[0]);
    }

    #[test]
    fn flipping_both_axes_still_tiles() {
        for layout in Layout::ALL {
            for len in 1..=6 {
                let rects = layout.calculate(AREA, len, 0, Flip::both(), &[]);
                assert_tiles(&rects, AREA);
            }
        }
    }

    #[test]
    fn flipping_twice_is_the_identity() {
        for layout in Layout::ALL {
            let normal = layout.calculate(AREA, 5, 0, Flip::NONE, &[]);
            let twice: Vec<Rect> = layout
                .calculate(AREA, 5, 0, Flip::both(), &[])
                .into_iter()
                .map(|r| Flip::both().apply(AREA, r))
                .collect();
            assert_eq!(twice, normal, "{layout}");
        }
    }

    #[test]
    fn flip_helpers() {
        assert!(Flip::NONE.is_none());
        assert!(!Flip::horizontal().is_none());
        assert!(Flip::horizontal().is_flipped(Axis::Horizontal));
        assert!(!Flip::horizontal().is_flipped(Axis::Vertical));
        assert_eq!(Flip::NONE.toggled(Axis::Vertical), Flip::vertical());
        assert_eq!(Flip::vertical().toggled(Axis::Vertical), Flip::NONE);
        assert_eq!(Flip::horizontal().toggled(Axis::Vertical), Flip::both());
        assert_eq!(Flip::default(), Flip::NONE);
    }

    #[test]
    fn resizing_moves_a_boundary_and_still_tiles() {
        let resize = vec![Some(Rect::new(0, 0, 200, 0)), None, None];
        for layout in Layout::ALL {
            let rects = layout.calculate(AREA, 3, 0, Flip::NONE, &resize);
            assert_tiles(&rects, AREA);
        }

        let columns = Layout::Columns.calculate(AREA, 3, 0, Flip::NONE, &resize);
        assert_eq!(columns[0].width(), 840, "640 plus the 200 pixel delta");
        assert_eq!(columns[1].left, 840);
        assert_eq!(columns[2], Rect::new(1280, 0, 1920, 1080), "untouched");
    }

    #[test]
    fn a_neighbours_near_edge_delta_moves_the_same_boundary() {
        let far = vec![Some(Rect::new(0, 0, 200, 0)), None];
        let near = vec![None, Some(Rect::new(200, 0, 0, 0))];
        assert_eq!(
            Layout::Columns.calculate(AREA, 2, 0, Flip::NONE, &far),
            Layout::Columns.calculate(AREA, 2, 0, Flip::NONE, &near)
        );
    }

    #[test]
    fn resize_deltas_are_clamped() {
        let resize = vec![Some(Rect::new(0, 0, 100_000, 0)), None];
        let rects = Layout::Columns.calculate(AREA, 2, 0, Flip::NONE, &resize);
        assert_tiles(&rects, AREA);
        assert!(rects[1].width() > 0, "the second column survives");
    }

    #[test]
    fn short_and_absent_resize_slices_are_ignored() {
        for layout in Layout::ALL {
            let full = layout.calculate(AREA, 5, 0, Flip::NONE, &[]);
            assert_eq!(layout.calculate(AREA, 5, 0, Flip::NONE, &[None]), full);
            assert_eq!(
                layout.calculate(AREA, 5, 0, Flip::NONE, &[None, None, None, None, None]),
                full
            );
        }
    }

    #[test]
    fn layouts_cycle_in_both_directions_and_wrap() {
        assert_eq!(Layout::Bsp.cycle(CycleDirection::Next), Layout::Columns);
        assert_eq!(Layout::Bsp.cycle(CycleDirection::Previous), Layout::Grid);
        assert_eq!(Layout::Grid.cycle(CycleDirection::Next), Layout::Bsp);

        let mut layout = Layout::Bsp;
        for _ in 0..Layout::ALL.len() {
            layout = layout.cycle(CycleDirection::Next);
        }
        assert_eq!(layout, Layout::Bsp, "a full lap returns to the start");
    }

    #[test]
    fn layout_json_names_match_the_existing_config_format() {
        assert_eq!(serde_json::to_string(&Layout::Bsp).unwrap(), "\"BSP\"");
        assert_eq!(
            serde_json::to_string(&Layout::UltrawideVerticalStack).unwrap(),
            "\"UltrawideVerticalStack\""
        );
        assert_eq!(
            serde_json::from_str::<Layout>("\"BSP\"").unwrap(),
            Layout::Bsp
        );
        assert_eq!(
            serde_json::from_str::<Layout>("\"Bsp\"").unwrap(),
            Layout::Bsp,
            "the rust spelling is accepted too"
        );
        assert_eq!(Layout::default(), Layout::Bsp);
    }

    #[test]
    fn layouts_parse_from_cli_words() {
        use std::str::FromStr;
        assert_eq!(Layout::from_str("bsp"), Ok(Layout::Bsp));
        assert_eq!(Layout::from_str("BSP"), Ok(Layout::Bsp));
        assert_eq!(
            Layout::from_str("vertical-stack"),
            Ok(Layout::VerticalStack)
        );
        assert_eq!(
            Layout::from_str("ultrawide_vertical_stack"),
            Ok(Layout::UltrawideVerticalStack)
        );
        assert!(Layout::from_str("spiral").is_err());
        assert_eq!(Layout::Bsp.to_string(), "BSP");
    }

    #[test]
    fn sizing_helpers() {
        use std::str::FromStr;
        assert_eq!(Sizing::Increase.signed(50), 50);
        assert_eq!(Sizing::Decrease.signed(50), -50);
        assert_eq!(Sizing::Increase.opposite(), Sizing::Decrease);
        assert_eq!(Sizing::from_str("increase"), Ok(Sizing::Increase));
        assert_eq!(Sizing::from_str("DECREASE"), Ok(Sizing::Decrease));
        assert!(Sizing::from_str("sideways").is_err());
        assert_eq!(Sizing::Increase.to_string(), "increase");
    }

    /// A rough guard against the layout arithmetic growing a hidden cost.
    /// Ignored by default, because a timing assertion on a busy machine is a
    /// flake waiting to happen: `cargo test -p mochi-core -- --ignored`.
    #[test]
    #[ignore = "timing"]
    fn ten_thousand_bsp_layouts_of_eight_containers_stay_under_two_hundred_millis() {
        let resize = [None; 8];
        let start = std::time::Instant::now();
        let mut produced = 0_usize;
        for _ in 0..10_000 {
            produced += Layout::Bsp
                .calculate(AREA, 8, 10, Flip::NONE, &resize)
                .len();
        }
        let elapsed = start.elapsed();
        assert_eq!(produced, 80_000);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "10k BSP layouts of eight containers took {elapsed:?} in debug"
        );
    }
}
