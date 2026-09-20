//! The one primitive every layout is built from: divide a range into slices
//! and let resize deltas nudge the boundaries between them.

use crate::geometry::{Axis, Rect};

/// A slice may never shrink below this fraction of its fair share.
const MIN_SHARE_DIVISOR: i32 = 5;

/// The smallest a tile is ever allowed to get, in logical pixels.
///
/// Every boundary clamp in this crate keeps this much room on both sides, all
/// the way down a nested layout, so no number of `resize-axis` presses can
/// squeeze a window down to a sliver. An area that is too small to give every
/// tile that much scales the minimum down instead, so the layout still tiles
/// exactly.
///
/// The value is in logical pixels. The daemon multiplies it by the monitor
/// scale factor for a display that is not at 96 DPI, the same way the paddings
/// are scaled.
pub const MIN_TILE_SIZE: i32 = 64;

/// The fair-share floor: a fifth of one slice's share of the range.
fn share_floor(extent: i32, parts: usize) -> i32 {
    let parts = parts.max(1) as i32;
    if extent <= parts {
        0
    } else {
        ((extent / parts) / MIN_SHARE_DIVISOR).max(1)
    }
}

/// The minimum every one of `parts` equal slices keeps: the fair-share floor
/// raised to `min_tile`, and scaled back down when that does not fit.
fn uniform_minimum(extent: i32, parts: usize, min_tile: i32) -> i32 {
    let min = share_floor(extent, parts).max(min_tile.max(0));
    if i64::from(min) * parts.max(1) as i64 > i64::from(extent) {
        extent / parts.max(1) as i32
    } else {
        min
    }
}

/// The boundary between two slices of `start..end`.
///
/// The natural boundary is the middle, `delta` moves it, and the clamps keep
/// `min_first` pixels on the near side and `min_rest` on the far side. When
/// the two minimums do not both fit they are scaled down together, so a
/// pathologically small range still produces two slices that are not inverted.
///
/// This is the allocation free path the BSP layout runs for every cut.
pub(crate) fn split_two(start: i32, end: i32, delta: i32, min_first: i32, min_rest: i32) -> i32 {
    let extent = end - start;
    if extent <= 0 {
        return end;
    }

    let floor = share_floor(extent, 2);
    let mut first = floor.max(min_first.max(0));
    let mut rest = floor.max(min_rest.max(0));
    let demand = i64::from(first) + i64::from(rest);
    if demand > i64::from(extent) && demand > 0 {
        first = (i64::from(first) * i64::from(extent) / demand) as i32;
        rest = (i64::from(rest) * i64::from(extent) / demand) as i32;
    }

    let low = start + first;
    let high = end - rest;
    if low >= high {
        return low;
    }
    (start + extent / 2).saturating_add(delta).clamp(low, high)
}

/// Divides `start..end` into `parts` slices with the given relative `weights`,
/// then moves internal boundary `i` by `deltas[i]` pixels.
///
/// Boundaries are clamped so every slice keeps at least a fifth of its fair
/// share and at least `min_tile` pixels, and so the slices always tile the
/// range exactly with no gap and no overlap. Missing deltas count as zero.
pub(crate) fn divide_weighted(
    start: i32,
    end: i32,
    weights: &[u32],
    deltas: &[i32],
    min_tile: i32,
) -> Vec<(i32, i32)> {
    let parts = weights.len();
    if parts == 0 {
        return Vec::new();
    }
    if parts == 1 {
        return vec![(start, end)];
    }

    let extent = end - start;
    let total: i64 = weights.iter().map(|w| i64::from(*w)).sum();
    if extent <= 0 || total <= 0 {
        return vec![(start, end); parts];
    }

    // The floor keeps the clamp satisfiable even on a pathologically small area.
    let min = uniform_minimum(extent, parts, min_tile);

    let mut bounds = Vec::with_capacity(parts + 1);
    bounds.push(start);
    let mut running: i64 = 0;
    for weight in &weights[..parts - 1] {
        running += i64::from(*weight);
        bounds.push(start + (i64::from(extent) * running / total) as i32);
    }
    bounds.push(end);

    for i in 0..parts - 1 {
        let delta = deltas.get(i).copied().unwrap_or(0);
        let low = bounds[i] + min;
        let high = end - min * (parts - 1 - i) as i32;
        let target = bounds[i + 1].saturating_add(delta);
        bounds[i + 1] = if low >= high {
            low
        } else {
            target.clamp(low, high)
        };
    }

    (0..parts).map(|i| (bounds[i], bounds[i + 1])).collect()
}

/// Divides `start..end` into `parts` equal slices, nudged by `deltas`.
pub(crate) fn divide(
    start: i32,
    end: i32,
    parts: usize,
    deltas: &[i32],
    min_tile: i32,
) -> Vec<(i32, i32)> {
    divide_weighted(start, end, &vec![1_u32; parts], deltas, min_tile)
}

/// Turns the slices of one axis into rectangles inside `area`.
pub(crate) fn slices_to_rects(area: Rect, axis: Axis, slices: &[(i32, i32)]) -> Vec<Rect> {
    slices
        .iter()
        .map(|(a, b)| rect_from_slice(area, axis, *a, *b))
        .collect()
}

/// Appends the slices of one axis to `out` as rectangles inside `area`.
///
/// The same as [`slices_to_rects`] with no vector in between: a layout builds
/// one output vector and every division writes straight into it.
pub(crate) fn extend_with_rects(
    out: &mut Vec<Rect>,
    area: Rect,
    axis: Axis,
    slices: &[(i32, i32)],
) {
    out.extend(
        slices
            .iter()
            .map(|(a, b)| rect_from_slice(area, axis, *a, *b)),
    );
}

/// One slice of `area` along `axis`, as a rectangle.
pub(crate) const fn rect_from_slice(area: Rect, axis: Axis, from: i32, to: i32) -> Rect {
    match axis {
        Axis::Horizontal => Rect::new(from, area.top, to, area.bottom),
        Axis::Vertical => Rect::new(area.left, from, area.right, to),
    }
}

/// The resize delta that moves the boundary between container `first` and
/// container `second` along `axis`.
///
/// A container's far-edge delta and its neighbour's near-edge delta both push
/// the same boundary, so they add up. Commands only ever write one of the two.
pub(crate) fn boundary_delta(
    resize_dimensions: &[Option<Rect>],
    first: usize,
    second: usize,
    axis: Axis,
) -> i32 {
    let far = resize_dimensions
        .get(first)
        .copied()
        .flatten()
        .map_or(0, |r| r.end(axis));
    let near = resize_dimensions
        .get(second)
        .copied()
        .flatten()
        .map_or(0, |r| r.start(axis));
    far.saturating_add(near)
}

/// The resize delta that moves a boundary more than two containers sit on,
/// such as the cut between the main container and a whole stack.
///
/// The containers in `before` face the boundary with their far edge and the
/// ones in `after` with their near edge, and every one of them gets a push at
/// it. The pushes add up the same way the two facing edges of a boundary
/// between two containers already do.
///
/// Summing is what makes a keypress from any window on the boundary count.
/// Reading one representative pair drops the delta of every other window on it
/// on the floor, and picking the largest or the average would make one
/// window's press undo another's. A sum also keeps a single window's presses
/// exactly reversible: unwinding its own stored delta takes away exactly what
/// it contributed, whatever the other windows have stored.
pub(crate) fn shared_boundary_delta(
    resize_dimensions: &[Option<Rect>],
    before: std::ops::Range<usize>,
    after: std::ops::Range<usize>,
    axis: Axis,
) -> i32 {
    let far = before
        .filter_map(|i| resize_dimensions.get(i).copied().flatten())
        .fold(0_i32, |sum, delta| sum.saturating_add(delta.end(axis)));
    after
        .filter_map(|i| resize_dimensions.get(i).copied().flatten())
        .fold(far, |sum, delta| sum.saturating_add(delta.start(axis)))
}

/// The deltas for the `count - 1` boundaries between containers
/// `first..first + count`.
pub(crate) fn boundary_deltas(
    resize_dimensions: &[Option<Rect>],
    first: usize,
    count: usize,
    axis: Axis,
) -> Vec<i32> {
    (0..count.saturating_sub(1))
        .map(|i| boundary_delta(resize_dimensions, first + i, first + i + 1, axis))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asking for no minimum tile size leaves only the fair-share floor.
    const NO_MIN: i32 = 0;

    fn widths(slices: &[(i32, i32)]) -> Vec<i32> {
        slices.iter().map(|(a, b)| b - a).collect()
    }

    fn divided(start: i32, end: i32, parts: usize, deltas: &[i32]) -> Vec<(i32, i32)> {
        divide(start, end, parts, deltas, MIN_TILE_SIZE)
    }

    #[test]
    fn dividing_into_one_returns_the_whole_range() {
        assert_eq!(divided(0, 100, 1, &[]), vec![(0, 100)]);
        assert_eq!(divided(0, 100, 0, &[]), vec![]);
    }

    #[test]
    fn equal_division_tiles_the_range_exactly() {
        for parts in 1..=9_usize {
            let slices = divided(0, 1000, parts, &[]);
            assert_eq!(slices.len(), parts);
            assert_eq!(slices[0].0, 0);
            assert_eq!(slices[parts - 1].1, 1000);
            for pair in slices.windows(2) {
                assert_eq!(pair[0].1, pair[1].0, "no gap and no overlap");
            }
            let total: i32 = widths(&slices).iter().sum();
            assert_eq!(total, 1000);
        }
    }

    #[test]
    fn deltas_move_the_boundary() {
        let slices = divided(0, 1000, 2, &[100]);
        assert_eq!(slices, vec![(0, 600), (600, 1000)]);

        let slices = divided(0, 1000, 2, &[-100]);
        assert_eq!(slices, vec![(0, 400), (400, 1000)]);
    }

    #[test]
    fn deltas_are_clamped_to_a_fifth_of_the_fair_share() {
        // Two parts of 500 each, minimum 100.
        let slices = divided(0, 1000, 2, &[10_000]);
        assert_eq!(slices, vec![(0, 900), (900, 1000)]);

        let slices = divided(0, 1000, 2, &[-10_000]);
        assert_eq!(slices, vec![(0, 100), (100, 1000)]);
    }

    #[test]
    fn a_huge_delta_still_leaves_room_for_every_later_slice() {
        let slices = divided(0, 1000, 4, &[100_000, 0, 0]);
        assert_eq!(slices.len(), 4);
        assert_eq!(slices[3].1, 1000);
        for (a, b) in &slices {
            assert!(b >= a, "no inverted slice: {a}..{b}");
        }
        for pair in slices.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
        }
    }

    #[test]
    fn a_tiny_range_never_panics() {
        for extent in 0..12 {
            for parts in 1..=6 {
                let slices = divided(0, extent, parts, &[1000, -1000, 5]);
                assert_eq!(slices.len(), parts);
                assert_eq!(slices[0].0, 0);
                assert_eq!(slices[parts - 1].1, extent.max(0));
            }
        }
    }

    #[test]
    fn saturating_deltas_do_not_overflow() {
        let slices = divided(0, 1000, 2, &[i32::MAX]);
        assert_eq!(slices[1].1, 1000);
        let slices = divided(0, 1000, 2, &[i32::MIN]);
        assert_eq!(slices[0].0, 0);
    }

    #[test]
    fn weighted_division_respects_the_weights() {
        let slices = divide_weighted(0, 1000, &[1, 2, 1], &[], MIN_TILE_SIZE);
        assert_eq!(widths(&slices), vec![250, 500, 250]);
        assert_eq!(slices[2].1, 1000);
    }

    #[test]
    fn no_slice_is_squeezed_below_the_minimum_tile_size() {
        // Twelve slices of a 1080 pixel range: the fair share is 90, so the
        // fifth-of-the-share floor on its own would allow 18 pixel slices.
        for delta in [i32::MAX, 100_000, 1_000, -1_000, -100_000, i32::MIN] {
            let deltas = vec![delta; 11];
            let slices = divided(0, 1080, 12, &deltas);
            for (i, width) in widths(&slices).iter().enumerate() {
                assert!(
                    *width >= MIN_TILE_SIZE,
                    "slice {i} is {width} px wide with delta {delta}"
                );
            }
        }
    }

    #[test]
    fn a_range_too_small_for_the_minimum_shares_it_out_evenly() {
        // Eight slices of 200 pixels cannot all be 64 wide, so the minimum
        // drops to the fair share and the range still tiles exactly.
        let slices = divided(0, 200, 8, &[10_000, 0, 0, 0, 0, 0, 0]);
        assert_eq!(slices[0].0, 0);
        assert_eq!(slices[7].1, 200);
        for (i, width) in widths(&slices).iter().enumerate() {
            assert!(*width >= 25, "slice {i} is {width} px wide");
        }
    }

    #[test]
    fn the_fair_share_floor_still_applies_without_a_minimum_tile_size() {
        assert_eq!(
            divide(0, 1000, 2, &[10_000], NO_MIN),
            vec![(0, 900), (900, 1000)]
        );
        assert_eq!(uniform_minimum(1000, 2, NO_MIN), 100);
        assert_eq!(uniform_minimum(1000, 12, NO_MIN), 16);
        assert_eq!(uniform_minimum(1000, 12, MIN_TILE_SIZE), 64);
        assert_eq!(uniform_minimum(200, 8, MIN_TILE_SIZE), 25);
        assert_eq!(uniform_minimum(4, 8, MIN_TILE_SIZE), 0);
    }

    #[test]
    fn two_slices_keep_their_own_minimums() {
        // The far side of a BSP cut needs room for three more tiles.
        let three = 3 * MIN_TILE_SIZE;
        assert_eq!(split_two(0, 1000, 100_000, MIN_TILE_SIZE, three), 808);
        assert_eq!(split_two(0, 1000, -100_000, MIN_TILE_SIZE, three), 100);
        assert_eq!(split_two(0, 1000, 0, MIN_TILE_SIZE, MIN_TILE_SIZE), 500);
        // Nothing fits: the two minimums shrink together.
        assert_eq!(split_two(0, 100, 100_000, MIN_TILE_SIZE, three), 25);
        // A degenerate range never inverts.
        assert_eq!(split_two(10, 10, 500, MIN_TILE_SIZE, MIN_TILE_SIZE), 10);
        assert_eq!(split_two(10, 5, 500, MIN_TILE_SIZE, MIN_TILE_SIZE), 5);
    }

    #[test]
    fn slices_become_rects_along_the_right_axis() {
        let area = Rect::new(0, 0, 100, 50);
        let cols = slices_to_rects(area, Axis::Horizontal, &[(0, 40), (40, 100)]);
        assert_eq!(
            cols,
            vec![Rect::new(0, 0, 40, 50), Rect::new(40, 0, 100, 50)]
        );
        let rows = slices_to_rects(area, Axis::Vertical, &[(0, 20), (20, 50)]);
        assert_eq!(
            rows,
            vec![Rect::new(0, 0, 100, 20), Rect::new(0, 20, 100, 50)]
        );
        assert_eq!(
            rect_from_slice(area, Axis::Horizontal, 0, 40),
            Rect::new(0, 0, 40, 50)
        );
    }

    #[test]
    fn boundary_deltas_add_the_two_facing_edges() {
        let resize = vec![
            Some(Rect::new(0, 0, 40, 0)),
            Some(Rect::new(10, 0, 0, 0)),
            None,
        ];
        assert_eq!(boundary_delta(&resize, 0, 1, Axis::Horizontal), 50);
        assert_eq!(boundary_delta(&resize, 1, 2, Axis::Horizontal), 0);
        assert_eq!(boundary_delta(&resize, 9, 10, Axis::Horizontal), 0);
        assert_eq!(
            boundary_deltas(&resize, 0, 3, Axis::Horizontal),
            vec![50, 0]
        );
        assert_eq!(
            boundary_deltas(&resize, 0, 1, Axis::Horizontal),
            Vec::<i32>::new()
        );
        assert_eq!(
            boundary_deltas(&[], 0, 0, Axis::Vertical),
            Vec::<i32>::new()
        );
    }
}
