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

/// The boundary between two slices of `start..end`.
///
/// The natural boundary is the middle, `delta` moves it, and the clamps keep
/// `min_first` pixels on the near side and `min_rest` on the far side, each
/// raised to the fair-share floor while that fits. The floor gives way before
/// a minimum does: see [`slice_minimums`]. When the two minimums do not both
/// fit they are scaled down together, so a pathologically small range still
/// produces two slices that are not inverted.
///
/// This is the allocation free path the BSP layout runs for every cut.
pub(crate) fn split_two(start: i32, end: i32, delta: i32, min_first: i32, min_rest: i32) -> i32 {
    let extent = end - start;
    if extent <= 0 {
        return end;
    }

    let span = i64::from(extent);
    let floor = i64::from(share_floor(extent, 2));
    let (own_first, own_rest) = (i64::from(min_first.max(0)), i64::from(min_rest.max(0)));
    let (raised_first, raised_rest) = (own_first.max(floor), own_rest.max(floor));
    let (first, rest) = if raised_first + raised_rest <= span {
        (raised_first, raised_rest)
    } else if own_first + own_rest <= span {
        (own_first, own_rest)
    } else {
        let demand = raised_first + raised_rest;
        (raised_first * span / demand, raised_rest * span / demand)
    };

    let low = start + first as i32;
    let high = end - rest as i32;
    if low >= high {
        return low;
    }
    (start + extent / 2).saturating_add(delta).clamp(low, high)
}

/// The minimum each slice of `extent` keeps, from its own floor in `mins`.
///
/// Three steps. While it all fits, every slice keeps its own floor raised to
/// the fair-share floor. The fair-share floor only exists so that a resize
/// cannot squeeze a slice to a sliver, so it is the first to give way: when it
/// is what makes the range too short, the slices keep their own floors and
/// nothing more, and a window is never cut below the size it accepts to make
/// room for a floor its neighbour does not need. Only when the floors
/// themselves do not fit are the raised floors scaled down together, so the
/// shortfall is shared out in proportion rather than starving whichever slice
/// the clamps happen to reach last. With one floor for every slice, every
/// slice that cannot have it gets the same fair share of the range.
fn slice_minimums(extent: i32, mins: &[i32]) -> Vec<i32> {
    let floor = i64::from(share_floor(extent, mins.len()));
    let own: Vec<i64> = mins.iter().map(|min| i64::from((*min).max(0))).collect();
    let raised: Vec<i64> = own.iter().map(|min| (*min).max(floor)).collect();
    let extent = i64::from(extent.max(0));
    let (raised_total, own_total) = (raised.iter().sum::<i64>(), own.iter().sum::<i64>());
    let kept: Vec<i64> = if raised_total <= extent {
        raised
    } else if own_total <= extent {
        own
    } else {
        raised
            .iter()
            .map(|min| min * extent / raised_total)
            .collect()
    };
    kept.into_iter().map(|min| min as i32).collect()
}

/// Where the boundaries fall before any resize delta moves them.
///
/// Every slice gets its weighted share of the range, except one whose minimum
/// is larger than that share: it gets its minimum, and the slices that are
/// left share what remains by their weights again, until every share fits.
/// The room a demanding slice needs is taken from all of its neighbours in
/// proportion, not from whichever one happens to sit next to it. While no
/// minimum binds, this is the plain weighted division.
///
/// `mins` must already fit the range, as [`slice_minimums`] makes them.
fn base_bounds(start: i32, end: i32, weights: &[u32], mins: &[i32]) -> Vec<i32> {
    let parts = weights.len();
    let extent = i64::from(end - start);
    let mut pinned = vec![false; parts];
    let (free_extent, free_weight) = loop {
        let pinned_extent: i64 = (0..parts)
            .filter(|i| pinned[*i])
            .map(|i| i64::from(mins[i]))
            .sum();
        let free_extent = (extent - pinned_extent).max(0);
        let free_weight: i64 = (0..parts)
            .filter(|i| !pinned[*i])
            .map(|i| i64::from(weights[i]))
            .sum();
        if free_weight == 0 {
            break (free_extent, free_weight);
        }
        let mut changed = false;
        for i in 0..parts {
            if !pinned[i] && free_extent * i64::from(weights[i]) / free_weight < i64::from(mins[i])
            {
                pinned[i] = true;
                changed = true;
            }
        }
        if !changed {
            break (free_extent, free_weight);
        }
    };

    let mut bounds = Vec::with_capacity(parts + 1);
    bounds.push(start);
    let mut pinned_so_far: i64 = 0;
    let mut running: i64 = 0;
    for i in 0..parts - 1 {
        if pinned[i] {
            pinned_so_far += i64::from(mins[i]);
        } else {
            running += i64::from(weights[i]);
        }
        let free_so_far = if free_weight == 0 {
            0
        } else {
            free_extent * running / free_weight
        };
        bounds.push(start + (pinned_so_far + free_so_far) as i32);
    }
    bounds.push(end);
    bounds
}

/// Divides `start..end` into slices with the given relative `weights`, then
/// moves internal boundary `i` by `deltas[i]` pixels.
///
/// Boundaries are clamped so every slice keeps at least a fifth of its fair
/// share and at least its own minimum, and so the slices always tile the range
/// exactly with no gap and no overlap. Missing deltas count as zero.
///
/// `mins[i]` is the narrowest slice `i` may get: the configured minimum tile
/// size, or more for a window that accepts nothing smaller. A slice whose minimum is larger than its share
/// takes the room from the others before any delta is applied, and no delta
/// can push a boundary past a minimum. A range too short for every minimum
/// scales them all down together, so the slices still tile it exactly. A
/// minimum missing from `mins` counts as zero.
pub(crate) fn divide_with_minimums(
    start: i32,
    end: i32,
    weights: &[u32],
    deltas: &[i32],
    mins: &[i32],
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
    let mins: Vec<i32> = (0..parts)
        .map(|i| mins.get(i).copied().unwrap_or(0))
        .collect();
    let mins = slice_minimums(extent, &mins);
    let mut bounds = base_bounds(start, end, weights, &mins);

    // What every slice after boundary `i` needs between it and the end.
    let mut rest = vec![0_i32; parts + 1];
    for i in (0..parts).rev() {
        rest[i] = rest[i + 1].saturating_add(mins[i]);
    }

    for i in 0..parts - 1 {
        let delta = deltas.get(i).copied().unwrap_or(0);
        let low = bounds[i] + mins[i];
        let high = end - rest[i + 1];
        let target = bounds[i + 1].saturating_add(delta);
        bounds[i + 1] = if low >= high {
            low
        } else {
            target.clamp(low, high)
        };
    }

    (0..parts).map(|i| (bounds[i], bounds[i + 1])).collect()
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

    /// `parts` slices of equal weight, all with the same floor.
    fn divide(start: i32, end: i32, parts: usize, deltas: &[i32], min: i32) -> Vec<(i32, i32)> {
        divide_with_minimums(start, end, &vec![1; parts], deltas, &vec![min; parts])
    }

    /// Weighted slices, all with the same floor.
    fn divide_weighted(
        start: i32,
        end: i32,
        weights: &[u32],
        deltas: &[i32],
        min: i32,
    ) -> Vec<(i32, i32)> {
        divide_with_minimums(start, end, weights, deltas, &vec![min; weights.len()])
    }

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
        assert_eq!(slice_minimums(1000, &[NO_MIN; 2]), vec![100; 2]);
        assert_eq!(slice_minimums(1000, &[NO_MIN; 12]), vec![16; 12]);
        assert_eq!(slice_minimums(1000, &[MIN_TILE_SIZE; 12]), vec![64; 12]);
        assert_eq!(slice_minimums(200, &[MIN_TILE_SIZE; 8]), vec![25; 8]);
        assert_eq!(slice_minimums(4, &[MIN_TILE_SIZE; 8]), vec![0; 8]);
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
    fn a_minimum_that_fits_is_never_cut_for_the_fair_share_floor() {
        // A window that needs 1800 of 1920 pixels next to a 64 pixel tile
        // fits. The fair-share floor would ask 192 for the small side, and
        // giving way to it used to squeeze the window to 1736.
        assert_eq!(split_two(0, 1920, 0, 1800, MIN_TILE_SIZE), 1800);
        assert_eq!(split_two(0, 1920, 0, MIN_TILE_SIZE, 1800), 120);
        assert_eq!(
            divide_with_minimums(0, 1920, &[1, 1], &[], &[1800, MIN_TILE_SIZE]),
            vec![(0, 1800), (1800, 1920)]
        );
        // Three slices: the demanding one takes everything the floors of the
        // other two can give up, and they keep their own floors.
        let slices = divide_with_minimums(0, 1920, &[1, 1, 1], &[], &[64, 1700, 64]);
        assert_eq!(widths(&slices), vec![110, 1700, 110]);
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

    // -- a floor of its own for every slice -----------------------------------

    /// Slices of equal weight, each with its own floor.
    fn divide_mins(start: i32, end: i32, deltas: &[i32], mins: &[i32]) -> Vec<(i32, i32)> {
        divide_with_minimums(start, end, &vec![1; mins.len()], deltas, mins)
    }

    /// The slices cover `start..end` with no gap, no overlap and no inversion.
    fn assert_tiles_range(slices: &[(i32, i32)], start: i32, end: i32) {
        assert_eq!(slices.first().map(|s| s.0), Some(start), "{slices:?}");
        assert_eq!(slices.last().map(|s| s.1), Some(end), "{slices:?}");
        for (a, b) in slices {
            assert!(b >= a, "inverted slice {a}..{b} in {slices:?}");
        }
        for pair in slices.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "gap or overlap in {slices:?}");
        }
    }

    #[test]
    fn a_demanding_slice_in_the_middle_takes_its_room_from_both_sides() {
        let slices = divide_mins(0, 900, &[], &[0, 500, 0]);
        assert_eq!(slices, vec![(0, 200), (200, 700), (700, 900)]);
    }

    #[test]
    fn two_demanding_slices_both_keep_their_minimums() {
        let slices = divide_mins(0, 1000, &[], &[0, 400, 0, 400]);
        assert_eq!(widths(&slices), vec![100, 400, 100, 400]);
        assert_tiles_range(&slices, 0, 1000);
    }

    #[test]
    fn pinning_one_slice_can_pin_the_next_one_too() {
        // 450 is pinned first. The three left would then share 550 as 183
        // each, less than the 200 the third one needs, so it is pinned too.
        let slices = divide_mins(0, 1000, &[], &[0, 450, 200, 0]);
        assert_eq!(widths(&slices), vec![175, 450, 200, 175]);
    }

    #[test]
    fn what_a_demanding_slice_leaves_is_shared_by_the_weights() {
        let slices = divide_with_minimums(0, 1000, &[1, 2, 1], &[], &[0, 0, 400]);
        assert_eq!(widths(&slices), vec![200, 400, 400]);
    }

    #[test]
    fn a_minimum_up_to_its_fair_share_leaves_the_plain_division() {
        assert_eq!(
            divide_mins(0, 1920, &[], &[MIN_TILE_SIZE, 480, MIN_TILE_SIZE, 480]),
            divided(0, 1920, 4, &[])
        );
        assert_eq!(
            divide_with_minimums(0, 1920, &[1, 2, 1], &[], &[480, 960, 480]),
            divide_weighted(0, 1920, &[1, 2, 1], &[], MIN_TILE_SIZE)
        );
    }

    #[test]
    fn an_infeasible_set_of_minimums_shares_the_shortfall_in_proportion() {
        let slices = divide_mins(0, 1000, &[], &[600, 600, 300]);
        assert_eq!(widths(&slices), vec![400, 400, 200]);
        assert_tiles_range(&slices, 0, 1000);
    }

    #[test]
    fn an_infeasible_range_still_tiles_exactly_whatever_the_deltas() {
        for deltas in [
            [0, 0],
            [i32::MAX, i32::MIN],
            [i32::MIN, i32::MAX],
            [5_000, -5_000],
        ] {
            for mins in [
                [600, 600, 300],
                [2_000, 0, 0],
                [0, 0, 2_000],
                [999, 999, 999],
            ] {
                let slices = divide_mins(-1080, -80, &deltas, &mins);
                assert_tiles_range(&slices, -1080, -80);
            }
        }
    }

    #[test]
    fn no_delta_pushes_a_boundary_past_a_minimum() {
        assert_eq!(
            divide_mins(0, 1000, &[i32::MAX], &[0, 600]),
            vec![(0, 400), (400, 1000)]
        );
        assert_eq!(
            divide_mins(0, 1000, &[i32::MIN], &[600, 0]),
            vec![(0, 600), (600, 1000)]
        );
        // A demanding slice in the middle, pushed from both sides at once.
        for deltas in [
            [i32::MAX, i32::MIN],
            [i32::MIN, i32::MAX],
            [100_000, 100_000],
            [-100_000, -100_000],
        ] {
            let slices = divide_mins(0, 900, &deltas, &[0, 500, 0]);
            assert!(widths(&slices)[1] >= 500, "{deltas:?} gave {slices:?}");
            assert_tiles_range(&slices, 0, 900);
        }
    }

    #[test]
    fn every_slice_keeps_its_own_minimum_while_they_all_fit() {
        const EXTENT: i32 = 1200;
        let floors = [0, MIN_TILE_SIZE, 150, 300, 700];
        let pushes = [
            [0, 0],
            [i32::MAX, i32::MIN],
            [i32::MIN, i32::MAX],
            [300, -300],
            [-300, 300],
            [i32::MAX, i32::MAX],
        ];
        for a in floors {
            for b in floors {
                for c in floors {
                    let mins = [a, b, c];
                    // The fair-share floor is the fifth of a third: 80.
                    let demand: i32 = mins.iter().map(|m| (*m).max(80)).sum();
                    if demand > EXTENT {
                        continue;
                    }
                    for deltas in pushes {
                        let slices = divide_mins(0, EXTENT, &deltas, &mins);
                        assert_tiles_range(&slices, 0, EXTENT);
                        for (i, width) in widths(&slices).iter().enumerate() {
                            assert!(
                                *width >= mins[i],
                                "slice {i} of {mins:?} is {width} with {deltas:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn absurd_minimums_never_panic_or_overflow() {
        let absurd = [i32::MIN, -1, 0, i32::MAX - 1, i32::MAX];
        for a in absurd {
            for b in absurd {
                for c in absurd {
                    for (start, end) in [(0, 1000), (-1080, 0), (0, 0), (0, 3), (5, 2)] {
                        for deltas in [[0, 0], [i32::MAX, i32::MIN], [i32::MIN, i32::MAX]] {
                            let slices =
                                divide_with_minimums(start, end, &[1, 2, 1], &deltas, &[a, b, c]);
                            assert_eq!(slices.len(), 3);
                            if end > start {
                                assert_tiles_range(&slices, start, end);
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(slice_minimums(1000, &[i32::MAX, i32::MAX]), vec![500, 500]);
        assert_eq!(slice_minimums(1000, &[-5, i32::MIN]), vec![100, 100]);
        assert_eq!(slice_minimums(0, &[i32::MAX, 10]), vec![0, 0]);
    }

    #[test]
    fn a_minimum_missing_from_the_list_counts_as_zero() {
        assert_eq!(
            divide_with_minimums(0, 900, &[1, 1, 1], &[], &[500]),
            divide_mins(0, 900, &[], &[500, 0, 0])
        );
        assert_eq!(
            divide_with_minimums(0, 900, &[1, 1, 1], &[], &[]),
            divide(0, 900, 3, &[], NO_MIN)
        );
    }

    #[test]
    fn two_slices_keep_a_demanding_far_side_and_survive_absurd_minimums() {
        assert_eq!(split_two(0, 1000, 0, MIN_TILE_SIZE, 700), 300);
        assert_eq!(split_two(0, 1000, i32::MIN, 700, MIN_TILE_SIZE), 700);
        assert_eq!(split_two(0, 1000, 0, i32::MAX, i32::MAX), 500);
        for first in [i32::MIN, -1, 0, i32::MAX] {
            for rest in [i32::MIN, -1, 0, i32::MAX] {
                for delta in [i32::MIN, 0, i32::MAX] {
                    let at = split_two(-500, 500, delta, first, rest);
                    assert!(
                        (-500..=500).contains(&at),
                        "{first} {rest} {delta} put the cut at {at}"
                    );
                }
            }
        }
    }
}
