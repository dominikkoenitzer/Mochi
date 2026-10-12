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
    boundary_delta, boundary_deltas, divide_with_minimums, extend_with_rects, rect_from_slice,
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

/// The smallest a tile may get, per axis.
///
/// One number cannot say this. A tiling user who wants a floor usually wants a
/// *width* floor, so a column of text stays readable, and cares much less about
/// height; collapsing the two into a single number either under-enforces the
/// one they asked for or silently raises the one they did not. Every cut a
/// layout makes divides exactly one axis, so every cut knows which of the two
/// applies to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MinSize {
    /// The narrowest a tile may be.
    pub width: i32,
    /// The shortest a tile may be.
    pub height: i32,
}

impl MinSize {
    /// The same floor on both axes.
    #[must_use]
    pub const fn square(size: i32) -> Self {
        Self {
            width: size,
            height: size,
        }
    }

    /// The floor that binds a cut along `axis`.
    ///
    /// A horizontal cut divides the area left to right, so what it constrains
    /// is width.
    #[must_use]
    pub const fn along(self, axis: Axis) -> i32 {
        match axis {
            Axis::Horizontal => self.width,
            Axis::Vertical => self.height,
        }
    }

    /// Both floors at zero or above.
    #[must_use]
    pub const fn clamped(self) -> Self {
        Self {
            width: if self.width < 0 { 0 } else { self.width },
            height: if self.height < 0 { 0 } else { self.height },
        }
    }
}

impl Default for MinSize {
    fn default() -> Self {
        Self::square(MIN_TILE_SIZE)
    }
}

/// Everything that limits how small the layout may make a tile.
///
/// Two kinds of floor. `tile` is the user's: the configured minimum every tile
/// keeps, whatever is in it. `windows` is the applications': many of them
/// refuse to shrink below a size of their own, and a tile smaller than that is
/// one the window spills out of, over its neighbour and past the edge of the
/// screen, while the border goes on outlining the tile it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Minimums<'a> {
    /// The floor every tile keeps, padding included.
    pub tile: MinSize,
    /// The smallest visible frame the window in each container accepts, by
    /// container index, in physical pixels and without the container
    /// padding. `None`, or an index past the end, is a window that has not
    /// said.
    pub windows: &'a [Option<MinSize>],
}

impl Minimums<'_> {
    /// The smallest slice the layout may cut for each of `len` containers:
    /// the tile floor, raised for a window that needs more, padding included.
    fn per_slice(&self, len: usize, container_padding: i32) -> Vec<MinSize> {
        let floor = self.tile.clamped();
        let padding = container_padding.max(0).saturating_mul(2);
        (0..len)
            .map(|i| match self.windows.get(i).copied().flatten() {
                Some(window) => {
                    let window = window.clamped();
                    MinSize {
                        width: floor.width.max(window.width.saturating_add(padding)),
                        height: floor.height.max(window.height.saturating_add(padding)),
                    }
                }
                None => floor,
            })
            .collect()
    }
}

/// The floors of `mins` along one axis.
fn along(mins: &[MinSize], axis: Axis) -> Vec<i32> {
    mins.iter().map(|min| min.along(axis)).collect()
}

/// The largest floor along `axis` among `mins`: what a slice that every one
/// of them spans the whole of has to keep.
fn widest(mins: &[MinSize], axis: Axis) -> i32 {
    mins.iter().map(|min| min.along(axis)).max().unwrap_or(0)
}

/// Grows `start..end` to at least `min` long, inside `low..high` when it fits.
///
/// The span keeps its start where it can, the way a window that refuses a
/// size keeps its position and grows to the right and downwards, and moves
/// back only as far as it must to stay inside. A span longer than the whole
/// range starts at `low` and is left to overflow `high`: there is no place for
/// it that does not.
fn grow_within(start: i32, end: i32, min: i32, low: i32, high: i32) -> (i32, i32) {
    if end.saturating_sub(start) >= min {
        return (start, end);
    }
    let mut start = start;
    if start.saturating_add(min) > high {
        start = high.saturating_sub(min).max(low);
    }
    (start, start.saturating_add(min))
}

/// The rectangle a window with that minimum really covers when it is given
/// `tile`, kept inside `bounds`.
fn fit_minimum(tile: Rect, min: MinSize, bounds: Rect) -> Rect {
    let (left, right) = grow_within(tile.left, tile.right, min.width, bounds.left, bounds.right);
    let (top, bottom) = grow_within(tile.top, tile.bottom, min.height, bounds.top, bounds.bottom);
    Rect::new(left, top, right, bottom)
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
            MinSize::square(MIN_TILE_SIZE),
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
        min_tile_size: MinSize,
    ) -> Vec<Rect> {
        self.calculate_fitted(
            area,
            len,
            container_padding,
            flip,
            resize_dimensions,
            Minimums {
                tile: min_tile_size,
                windows: &[],
            },
        )
    }

    /// The same as [`Layout::calculate_with_min`], leaving every window at
    /// least the size it accepts.
    ///
    /// A cut that would leave a window less than its minimum moves until it
    /// does not, and the room comes out of the tiles that can spare it: in a
    /// BSP a whole branch of the tree gives way, not only the tile next door.
    /// While no window's minimum is larger than the tile it would have had,
    /// the result is exactly [`Layout::calculate_with_min`].
    ///
    /// When the area is genuinely too small for every minimum, the shortfall
    /// is shared out and the rectangle handed back for such a window is the
    /// one it will really cover: grown to its minimum and kept inside the
    /// area. Its border is then drawn around the window and not around a tile
    /// it overflows, and it does not spill past the edge of the screen. Those
    /// rectangles overlap their neighbours, which is the one case where the
    /// result does not tile the area exactly.
    #[must_use]
    pub fn calculate_fitted(
        &self,
        area: Rect,
        len: usize,
        container_padding: i32,
        flip: Flip,
        resize_dimensions: &[Option<Rect>],
        minimums: Minimums<'_>,
    ) -> Vec<Rect> {
        if len == 0 {
            return Vec::new();
        }
        let mins = minimums.per_slice(len, container_padding);
        let floor = minimums.tile.clamped();

        let mut rects = match self {
            Self::Bsp => bsp::calculate(area, len, resize_dimensions, &mins, floor),
            Self::Columns => columns(area, len, resize_dimensions, &mins, floor),
            Self::Rows => rows(area, len, resize_dimensions, &mins, floor),
            Self::VerticalStack => {
                main_and_stack(area, len, resize_dimensions, Axis::Horizontal, &mins, floor)
            }
            Self::HorizontalStack => {
                main_and_stack(area, len, resize_dimensions, Axis::Vertical, &mins, floor)
            }
            Self::UltrawideVerticalStack => ultrawide(area, len, resize_dimensions, &mins, floor),
            Self::Grid => grid(area, len, resize_dimensions, &mins, floor),
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

        // A window that cannot shrink to its tile is going to be bigger than
        // the tile whatever the layout says, so the layout says where it will
        // really be. Everything downstream takes its rectangle from here: the
        // placement, the border, `mochic state` and the neighbour a direction
        // command finds.
        if minimums.windows.iter().any(Option::is_some) {
            let bounds = area.padded_clamped(container_padding.max(0));
            for (rect, min) in rects.iter_mut().zip(minimums.windows) {
                if let Some(min) = min {
                    *rect = fit_minimum(*rect, min.clamped(), bounds);
                }
            }
        }

        // Nothing downstream may be handed a tile a window cannot exist in.
        //
        // Tiling the area exactly is what every layout here is for, and this
        // is the one place it gives way, because the alternative is worse. A
        // rectangle of zero or negative size reaches `SetWindowPos` as a width
        // of zero: the window is still managed, still focusable, still in the
        // ring, and completely invisible, which the user cannot tell from a
        // window that failed to open.
        //
        // It only bites when the area genuinely cannot be divided that far,
        // and the way to get there is a minimum tile size of zero: every cut
        // halves what is left, so with no floor to stop it the remainder
        // decays exponentially and runs out of pixels. Measured on a 4K
        // screen, a minimum of 0 produces its first empty tile at 26 windows,
        // 1 at 60 and 2 at 100 - raising the floor only moves it, which is why
        // this is a guarantee about the output rather than arithmetic in the
        // splitter. For every real configuration it changes nothing: a tile
        // that is already a pixel or more is left exactly as it was.
        for rect in &mut rects {
            if rect.width() < 1 {
                *rect = Rect::new(rect.left, rect.top, rect.left + 1, rect.bottom);
            }
            if rect.height() < 1 {
                *rect = Rect::new(rect.left, rect.top, rect.right, rect.top + 1);
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

/// Equal slices of `start..end`, one per entry of `mins`, each kept at least
/// its own floor. `floor` is the configured tile floor alone.
fn divide_even(start: i32, end: i32, deltas: &[i32], mins: &[i32], floor: i32) -> Vec<(i32, i32)> {
    divide_with_minimums(start, end, &vec![1_u32; mins.len()], deltas, mins, floor)
}

fn columns(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
    let deltas = boundary_deltas(resize, 0, len, Axis::Horizontal);
    let slices = divide_even(
        area.left,
        area.right,
        &deltas,
        &along(mins, Axis::Horizontal),
        floor.width,
    );
    let mut rects = Vec::with_capacity(len);
    extend_with_rects(&mut rects, area, Axis::Horizontal, &slices);
    rects
}

fn rows(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
    let deltas = boundary_deltas(resize, 0, len, Axis::Vertical);
    let slices = divide_even(
        area.top,
        area.bottom,
        &deltas,
        &along(mins, Axis::Vertical),
        floor.height,
    );
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
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
    if len == 1 {
        return vec![area];
    }

    // Every container in the stack sits on the main cut, so a resize from any
    // of them moves it, not just one from the first. Every one of them also
    // spans the whole stack, so the stack is as wide as its widest needs.
    let split_delta = shared_boundary_delta(resize, 0..1, 1..len, main_axis);
    let halves = divide_even(
        area.start(main_axis),
        area.end(main_axis),
        &[split_delta],
        &[mins[0].along(main_axis), widest(&mins[1..], main_axis)],
        floor.along(main_axis),
    );
    let main = rect_from_slice(area, main_axis, halves[0].0, halves[0].1);
    let stack_area = rect_from_slice(area, main_axis, halves[1].0, halves[1].1);

    let stack_axis = main_axis.other();
    let stack_deltas = boundary_deltas(resize, 1, len - 1, stack_axis);
    let slices = divide_even(
        stack_area.start(stack_axis),
        stack_area.end(stack_axis),
        &stack_deltas,
        &along(&mins[1..], stack_axis),
        floor.along(stack_axis),
    );

    let mut rects = Vec::with_capacity(len);
    rects.push(main);
    extend_with_rects(&mut rects, stack_area, stack_axis, &slices);
    rects
}

fn ultrawide(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
    match len {
        1 => vec![area],
        2 => {
            // Secondary on the left, primary on the right.
            let delta = boundary_delta(resize, 1, 0, Axis::Horizontal);
            let slices = divide_even(
                area.left,
                area.right,
                &[delta],
                &[mins[1].width, mins[0].width],
                floor.width,
            );
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
            let slices = divide_with_minimums(
                area.left,
                area.right,
                &[1, 2, 1],
                &[left_delta, right_delta],
                &[
                    mins[1].width,
                    mins[0].width,
                    widest(&mins[2..], Axis::Horizontal),
                ],
                floor.width,
            );
            let cell = |i: usize| rect_from_slice(area, Axis::Horizontal, slices[i].0, slices[i].1);
            let (secondary, primary, stack_area) = (cell(0), cell(1), cell(2));

            let stack_len = len - 2;
            let stack_deltas = boundary_deltas(resize, 2, stack_len, Axis::Vertical);
            let stack_slices = divide_even(
                stack_area.top,
                stack_area.bottom,
                &stack_deltas,
                &along(&mins[2..], Axis::Vertical),
                floor.height,
            );

            let mut rects = Vec::with_capacity(len);
            rects.push(primary);
            rects.push(secondary);
            extend_with_rects(&mut rects, stack_area, Axis::Vertical, &stack_slices);
            rects
        }
    }
}

fn grid(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
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
    // Every cell spans its whole column, so a column is as wide as its widest
    // cell needs.
    let column_mins: Vec<i32> = (0..columns_count)
        .map(|c| {
            widest(
                &mins[first_of_column[c]..end_of_column(c)],
                Axis::Horizontal,
            )
        })
        .collect();
    let column_slices = divide_even(
        area.left,
        area.right,
        &column_deltas,
        &column_mins,
        floor.width,
    );
    let column_rects = slices_to_rects(area, Axis::Horizontal, &column_slices);

    let mut rects = Vec::with_capacity(len);
    for (column, rows_in_column) in rows_per_column.iter().copied().enumerate() {
        let column_rect = column_rects[column];
        let first = first_of_column[column];
        let deltas = boundary_deltas(resize, first, rows_in_column, Axis::Vertical);
        let slices = divide_even(
            column_rect.top,
            column_rect.bottom,
            &deltas,
            &along(&mins[first..first + rows_in_column], Axis::Vertical),
            floor.height,
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
mod invariant_tests {
    use super::*;

    /// No layout, at any window count, with any minimum, may hand back a tile
    /// a window cannot exist in.
    ///
    /// A rectangle of zero or negative size reaches `SetWindowPos` as a width
    /// of zero: the window stays managed, focusable and in the ring, and is
    /// completely invisible. Before this was guaranteed, a minimum tile size of
    /// zero produced its first empty tile at 26 windows on a 4K screen.
    #[test]
    fn no_layout_ever_emits_a_tile_a_window_cannot_exist_in() {
        // His 4K work area, and a small one, because running out of pixels is
        // the whole point of the test.
        for area in [Rect::new(36, 36, 3804, 2052), Rect::new(0, 0, 320, 240)] {
            for layout in [
                Layout::Bsp,
                Layout::Columns,
                Layout::Rows,
                Layout::VerticalStack,
                Layout::HorizontalStack,
                Layout::UltrawideVerticalStack,
                Layout::Grid,
            ] {
                for min in [0, 1, 2, 64] {
                    for len in [1usize, 2, 7, 26, 60, 100] {
                        let deltas = vec![None; len];
                        let rects = layout.calculate_with_min(
                            area,
                            len,
                            10,
                            Flip::NONE,
                            &deltas,
                            MinSize::square(min),
                        );
                        assert_eq!(rects.len(), len, "{layout:?} returned the wrong count");
                        for (i, r) in rects.iter().enumerate() {
                            assert!(
                                r.width() >= 1 && r.height() >= 1,
                                "{layout:?} min={min} len={len}: tile {i} is {}x{}",
                                r.width(),
                                r.height()
                            );
                        }
                    }
                }
            }
        }
    }
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

    // -- window minimums ----------------------------------------------------

    const fn min(width: i32, height: i32) -> Option<MinSize> {
        Some(MinSize { width, height })
    }

    /// The layout with the default tile floor, no padding and these window
    /// minimums.
    fn fitted(layout: Layout, len: usize, windows: &[Option<MinSize>]) -> Vec<Rect> {
        layout.calculate_fitted(
            AREA,
            len,
            0,
            Flip::NONE,
            &[],
            Minimums {
                tile: MinSize::default(),
                windows,
            },
        )
    }

    fn assert_minimums_kept(rects: &[Rect], windows: &[Option<MinSize>]) {
        for (i, (rect, min)) in rects.iter().zip(windows).enumerate() {
            if let Some(min) = min {
                assert!(
                    rect.width() >= min.width && rect.height() >= min.height,
                    "window {i} accepts {}x{} and got {rect:?}",
                    min.width,
                    min.height
                );
            }
        }
    }

    #[test]
    fn four_demanding_windows_on_the_4k_panel_no_longer_overlap() {
        // His desktop: the 4K panel at 150 percent, workspace padding 14,
        // container padding 10, BSP with WhatsApp, Brave, Spotify and Discord.
        // The bottom right quarter used to be cut into two tiles of about 920
        // pixels, narrower than both Spotify and Discord accept, so the two
        // windows spilled over each other.
        use crate::model::{Window, WindowId, Workspace};

        let work_area = Rect::new(0, 0, 3840, 2088);
        let tiling_area = Rect::new(21, 21, 3819, 2067);
        // Logical minimums as the windows report them, and the physical
        // sizes they come to at 150 percent.
        let windows = [
            (
                MinSize {
                    width: 500,
                    height: 400,
                },
                (750, 600),
            ),
            (
                MinSize {
                    width: 515,
                    height: 88,
                },
                (772, 131),
            ),
            (
                MinSize {
                    width: 800,
                    height: 600,
                },
                (1200, 900),
            ),
            (
                MinSize {
                    width: 814,
                    height: 508,
                },
                (1221, 761),
            ),
        ];

        let blind = Layout::Bsp.calculate_with_min(
            tiling_area,
            4,
            15,
            Flip::NONE,
            &[],
            MinSize::square(96),
        );
        assert!(
            blind[2].width() < 1200 && blind[3].width() < 1221,
            "without the window minimums the bottom right is too narrow: {blind:?}"
        );

        let mut ws = Workspace::new();
        for (i, (logical, _)) in windows.iter().enumerate() {
            ws.add_window(Window::new(i as isize + 1).with_min_size(Some(*logical)));
        }
        let rects = ws
            .update_layout_scaled(work_area, 14, 10, 1.5, 1.5)
            .to_vec();
        assert_eq!(rects.len(), 4);

        for (i, (_, (width, height))) in windows.iter().enumerate() {
            let idx = ws
                .container_idx_for_window(WindowId(i as isize + 1))
                .unwrap();
            let rect = rects[idx];
            assert!(
                tiling_area.contains_rect(&rect),
                "window {i} escapes the tiling area: {rect:?}"
            );
            assert!(
                rect.width() >= *width && rect.height() >= *height,
                "window {i} accepts {width}x{height} and got {rect:?}"
            );
        }
        for (i, a) in rects.iter().enumerate() {
            for (j, b) in rects.iter().enumerate().skip(i + 1) {
                assert!(!a.intersects(b), "tiles {i} and {j} overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn window_minimums_that_never_bind_give_exactly_the_old_layout() {
        let resizes = [
            vec![],
            vec![Some(Rect::new(-120, 80, 200, -60)); 8],
            vec![Some(Rect::new(-100_000, 100_000, 100_000, -100_000)); 8],
            vec![
                Some(Rect::new(
                    i32::MIN / 2,
                    i32::MAX / 2,
                    i32::MAX / 2,
                    i32::MIN / 2
                ));
                8
            ],
        ];
        for layout in Layout::ALL {
            for len in 1..=8 {
                for resize in &resizes {
                    for padding in [0, 10] {
                        for flip in [Flip::NONE, Flip::both()] {
                            let tile = MinSize::default();
                            let old =
                                layout.calculate_with_min(AREA, len, padding, flip, resize, tile);
                            let none = vec![None; len];
                            let small = vec![min(10, 10); len];
                            for windows in [none.as_slice(), small.as_slice(), &[]] {
                                let new = layout.calculate_fitted(
                                    AREA,
                                    len,
                                    padding,
                                    flip,
                                    resize,
                                    Minimums { tile, windows },
                                );
                                assert_eq!(
                                    new, old,
                                    "{layout} with {len}, padding {padding}, {flip:?}, {windows:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_negative_window_minimum_changes_nothing() {
        let windows = [min(-500, i32::MIN), None, min(0, 0)];
        for layout in Layout::ALL {
            assert_eq!(
                layout.calculate_fitted(
                    AREA,
                    3,
                    10,
                    Flip::NONE,
                    &[],
                    Minimums {
                        tile: MinSize::default(),
                        windows: &windows,
                    },
                ),
                layout.calculate_with_min(AREA, 3, 10, Flip::NONE, &[], MinSize::default()),
                "{layout}"
            );
        }
    }

    #[test]
    fn a_short_window_list_counts_the_rest_as_unknown_and_a_long_one_is_cut_off() {
        for layout in Layout::ALL {
            assert_eq!(
                fitted(layout, 4, &[None, min(1000, 0)]),
                fitted(layout, 4, &[None, min(1000, 0), None, None]),
                "{layout}"
            );
            assert_eq!(
                fitted(layout, 2, &[None, None, min(5000, 5000)]),
                layout.calculate_with_min(AREA, 2, 0, Flip::NONE, &[], MinSize::default()),
                "{layout}"
            );
        }
    }

    #[test]
    fn the_container_padding_is_added_to_a_window_minimum() {
        let windows = [None, min(1200, 0)];
        let rects = Layout::Bsp.calculate_fitted(
            AREA,
            2,
            10,
            Flip::NONE,
            &[],
            Minimums {
                tile: MinSize::default(),
                windows: &windows,
            },
        );
        assert_eq!(rects[0], Rect::new(10, 10, 690, 1070));
        assert_eq!(rects[1], Rect::new(710, 10, 1910, 1070));
    }

    #[test]
    fn a_vertical_stack_widens_the_whole_stack_for_one_wide_window() {
        let windows = [None, None, min(1200, 0)];
        let rects = fitted(Layout::VerticalStack, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 720, 1080));
        assert_eq!(rects[1], Rect::new(720, 0, 1920, 540), "it spans the stack");
        assert_tiles(&rects, AREA);
        assert_minimums_kept(&rects, &windows);
    }

    #[test]
    fn a_vertical_stack_keeps_a_tall_stack_window_and_a_wide_main_one() {
        let windows = [min(1500, 0), min(0, 900), None];
        let rects = fitted(Layout::VerticalStack, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 1500, 1080));
        assert_eq!(rects[1], Rect::new(1500, 0, 1920, 900));
        assert_eq!(rects[2], Rect::new(1500, 900, 1920, 1080));
        assert_tiles(&rects, AREA);
    }

    #[test]
    fn a_horizontal_stack_keeps_a_tall_main_window_and_a_wide_stack_one() {
        let windows = [min(0, 800), min(1400, 0), None];
        let rects = fitted(Layout::HorizontalStack, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 1920, 800));
        assert_eq!(rects[1], Rect::new(0, 800, 1400, 1080));
        assert_eq!(rects[2], Rect::new(1400, 800, 1920, 1080));
        assert_tiles(&rects, AREA);
    }

    #[test]
    fn a_horizontal_stack_gives_the_whole_stack_the_height_of_its_tallest_window() {
        let windows = [None, None, min(0, 700)];
        let rects = fitted(Layout::HorizontalStack, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 1920, 380));
        assert_eq!(rects[1].height(), 700);
        assert_tiles(&rects, AREA);
        assert_minimums_kept(&rects, &windows);
    }

    #[test]
    fn ultrawide_with_two_keeps_either_windows_minimum() {
        // The primary is rects[0] on the right, the secondary rects[1] on
        // the left.
        let rects = fitted(Layout::UltrawideVerticalStack, 2, &[min(1500, 0), None]);
        assert_eq!(rects[0], Rect::new(420, 0, 1920, 1080));
        assert_eq!(rects[1], Rect::new(0, 0, 420, 1080));

        let rects = fitted(Layout::UltrawideVerticalStack, 2, &[None, min(1500, 0)]);
        assert_eq!(rects[0], Rect::new(1500, 0, 1920, 1080));
        assert_eq!(rects[1], Rect::new(0, 0, 1500, 1080));
    }

    #[test]
    fn ultrawide_with_more_keeps_the_primary_the_secondary_and_the_stack() {
        let rects = fitted(
            Layout::UltrawideVerticalStack,
            4,
            &[min(1200, 0), None, None, None],
        );
        assert_eq!(rects[0], Rect::new(360, 0, 1560, 1080), "primary");
        assert_eq!(rects[1], Rect::new(0, 0, 360, 1080), "secondary");
        assert_eq!(rects[2], Rect::new(1560, 0, 1920, 540));

        let rects = fitted(
            Layout::UltrawideVerticalStack,
            4,
            &[None, min(700, 0), None, None],
        );
        assert_eq!(rects[1], Rect::new(0, 0, 700, 1080), "secondary");
        assert_eq!(rects[0], Rect::new(700, 0, 1513, 1080), "primary");

        let windows = [None, None, None, min(800, 900)];
        let rects = fitted(Layout::UltrawideVerticalStack, 4, &windows);
        assert_eq!(rects[2], Rect::new(1120, 0, 1920, 180));
        assert_eq!(rects[3], Rect::new(1120, 180, 1920, 1080));
        assert_tiles(&rects, AREA);
        assert_minimums_kept(&rects, &windows);
    }

    #[test]
    fn a_grid_column_is_as_wide_as_its_widest_cell_needs() {
        let rects = fitted(Layout::Grid, 4, &[None, min(1400, 0), None, None]);
        assert_eq!(rects[0], Rect::new(0, 0, 1400, 540));
        assert_eq!(rects[1], Rect::new(0, 540, 1400, 1080));
        assert_eq!(rects[2], Rect::new(1400, 0, 1920, 540));

        let windows = [None, min(1000, 0), min(1300, 0)];
        let rects = fitted(Layout::Grid, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 620, 1080));
        assert_eq!(rects[1], Rect::new(620, 0, 1920, 540));
        assert_eq!(rects[2], Rect::new(620, 540, 1920, 1080));
        assert_minimums_kept(&rects, &windows);
    }

    #[test]
    fn a_grid_row_keeps_a_tall_cell_and_leaves_the_other_column_alone() {
        let rects = fitted(Layout::Grid, 4, &[None, None, min(0, 900), None]);
        assert_eq!(rects[0], Rect::new(0, 0, 960, 540));
        assert_eq!(rects[2], Rect::new(960, 0, 1920, 900));
        assert_eq!(rects[3], Rect::new(960, 900, 1920, 1080));
    }

    #[test]
    fn columns_and_rows_take_the_room_from_every_other_slice() {
        let rects = fitted(Layout::Columns, 4, &[None, min(1000, 0), None, None]);
        let widths: Vec<i32> = rects.iter().map(Rect::width).collect();
        assert_eq!(widths, vec![306, 1000, 307, 307]);
        assert_tiles(&rects, AREA);

        let rects = fitted(Layout::Rows, 3, &[min(0, 700), None, None]);
        assert_eq!(rects[0], Rect::new(0, 0, 1920, 700));
        assert_eq!(rects[1], Rect::new(0, 700, 1920, 890));
        assert_eq!(rects[2], Rect::new(0, 890, 1920, 1080));
    }

    #[test]
    fn every_layout_keeps_a_window_minimum_that_fits_and_still_tiles() {
        for layout in Layout::ALL {
            for len in 1..=6 {
                for idx in 0..len {
                    let mut windows = vec![None; len];
                    windows[idx] = min(400, 250);
                    let rects = fitted(layout, len, &windows);
                    assert_tiles(&rects, AREA);
                    assert_minimums_kept(&rects, &windows);
                }
            }
        }
    }

    #[test]
    fn a_window_wider_than_the_whole_area_starts_at_its_left_and_overflows_right() {
        let windows = [None, min(3000, 0)];
        let rects = Layout::Columns.calculate_fitted(
            AREA,
            2,
            10,
            Flip::NONE,
            &[],
            Minimums {
                tile: MinSize::default(),
                windows: &windows,
            },
        );
        assert_eq!(rects[1], Rect::new(10, 10, 3010, 1070));
        assert!(rects[0].width() >= 1 && rects[0].height() >= 1);
    }

    #[test]
    fn a_window_bigger_than_the_screen_leaves_every_other_tile_as_it_was() {
        // It covers the whole area whatever tile it gets, so taking room from
        // its neighbours for it would only squeeze them for nothing.
        for layout in Layout::ALL {
            for big in [min(32_767, 32_767), min(2000, 1100)] {
                let windows = [big, None, None, None];
                let rects = fitted(layout, 4, &windows);
                let plain =
                    layout.calculate_with_min(AREA, 4, 0, Flip::NONE, &[], MinSize::default());
                assert_eq!(rects[1..], plain[1..], "{layout} with {big:?}");
            }
        }
    }

    #[test]
    fn a_window_that_cannot_fit_beside_its_neighbours_leaves_them_their_floor() {
        // 1900 of 1920 fits on its own, but not beside three tiles that keep
        // the configured 64: those keep it, and the window takes the rest.
        for layout in [Layout::Columns, Layout::Bsp, Layout::VerticalStack] {
            let windows = [min(1900, 0), None, None, None];
            let rects = fitted(layout, 4, &windows);
            for (i, rect) in rects.iter().enumerate().skip(1) {
                assert!(
                    rect.width() >= MIN_TILE_SIZE,
                    "{layout}: tile {i} was squeezed to {rect:?}"
                );
            }
            assert!(rects[0].width() >= 1900, "{layout}: {:?}", rects[0]);
        }
    }

    #[test]
    fn a_floor_wider_than_a_quarter_keeps_the_ultrawide_layout_it_always_had() {
        // On a portrait screen a floor of 300 is more than the secondary's
        // quarter. A resize used to be clamped back onto that floor rather
        // than added on top of it, and still is.
        let area = Rect::new(0, 0, 1080, 1920);
        let resize = [None, Some(Rect::new(0, 0, 21, 0)), None];
        let rects = Layout::UltrawideVerticalStack.calculate_with_min(
            area,
            3,
            0,
            Flip::NONE,
            &resize,
            MinSize::square(300),
        );
        assert_eq!(rects[1].width(), 300, "{rects:?}");
    }

    #[test]
    fn a_window_at_the_right_edge_grows_leftwards_to_stay_inside() {
        // 1000 + 128 + 1000 does not fit in 1920, so the layout shares the
        // shortfall and both windows are handed the rectangle they really
        // cover: the left one grows right, the right one grows left.
        let windows = [min(1000, 0), None, min(1000, 0)];
        let rects = fitted(Layout::Columns, 3, &windows);
        assert_eq!(rects[0], Rect::new(0, 0, 1000, 1080));
        assert_eq!(rects[2], Rect::new(920, 0, 1920, 1080));
        for rect in &rects {
            assert!(AREA.contains_rect(rect), "{rect:?}");
        }
    }

    #[test]
    fn a_grown_rect_stays_inside_the_area_whatever_the_flip() {
        let windows = [min(1000, 600), None, min(1000, 600)];
        for flip in [
            Flip::NONE,
            Flip::horizontal(),
            Flip::vertical(),
            Flip::both(),
        ] {
            for layout in Layout::ALL {
                for padding in [0, 10] {
                    let rects = layout.calculate_fitted(
                        AREA,
                        3,
                        padding,
                        flip,
                        &[],
                        Minimums {
                            tile: MinSize::default(),
                            windows: &windows,
                        },
                    );
                    for (i, rect) in rects.iter().enumerate() {
                        assert!(
                            AREA.padded_clamped(padding).contains_rect(rect),
                            "{layout} {flip:?} padding {padding}: rect {i} escapes: {rect:?}"
                        );
                        assert!(rect.width() >= 1 && rect.height() >= 1);
                    }
                    assert_minimums_kept(&rects, &windows);
                }
            }
        }
    }

    #[test]
    fn absurd_window_minimums_never_panic_and_never_erase_a_tile() {
        let areas = [
            AREA,
            Rect::new(-1080, 200, 0, 1000),
            Rect::new(0, 0, 7, 7),
            Rect::new(0, 0, 0, 0),
        ];
        let absurd = [
            min(i32::MAX, i32::MAX),
            min(i32::MIN, i32::MAX),
            min(i32::MAX - 1, 0),
            min(100_000, 100_000),
            None,
        ];
        for area in areas {
            for layout in Layout::ALL {
                for len in 1..=6 {
                    for (k, first) in absurd.iter().enumerate() {
                        let windows: Vec<Option<MinSize>> =
                            (0..len).map(|i| absurd[(k + i) % absurd.len()]).collect();
                        assert_eq!(windows[0], *first);
                        for padding in [0, 15] {
                            let rects = layout.calculate_fitted(
                                area,
                                len,
                                padding,
                                Flip::both(),
                                &[],
                                Minimums {
                                    tile: MinSize::default(),
                                    windows: &windows,
                                },
                            );
                            assert_eq!(rects.len(), len);
                            for (i, rect) in rects.iter().enumerate() {
                                assert!(
                                    rect.width() >= 1 && rect.height() >= 1,
                                    "{layout} {len} in {area:?}: rect {i} is {rect:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
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
