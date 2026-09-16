//! The one primitive every layout is built from: divide a range into slices
//! and let resize deltas nudge the boundaries between them.

use crate::geometry::{Axis, Rect};

/// A slice may never shrink below this fraction of its fair share.
const MIN_SHARE_DIVISOR: i32 = 5;

/// Divides `start..end` into `parts` slices with the given relative `weights`,
/// then moves internal boundary `i` by `deltas[i]` pixels.
///
/// Boundaries are clamped so every slice keeps at least a fifth of its fair
/// share, and so the slices always tile the range exactly with no gap and no
/// overlap. Missing deltas count as zero.
pub(crate) fn divide_weighted(
    start: i32,
    end: i32,
    weights: &[u32],
    deltas: &[i32],
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
    let min = if extent <= parts as i32 {
        0
    } else {
        ((extent / parts as i32) / MIN_SHARE_DIVISOR).max(1)
    };

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
pub(crate) fn divide(start: i32, end: i32, parts: usize, deltas: &[i32]) -> Vec<(i32, i32)> {
    divide_weighted(start, end, &vec![1_u32; parts], deltas)
}

/// Turns the slices of one axis into rectangles inside `area`.
pub(crate) fn slices_to_rects(area: Rect, axis: Axis, slices: &[(i32, i32)]) -> Vec<Rect> {
    slices
        .iter()
        .map(|(a, b)| match axis {
            Axis::Horizontal => Rect::new(*a, area.top, *b, area.bottom),
            Axis::Vertical => Rect::new(area.left, *a, area.right, *b),
        })
        .collect()
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

    fn widths(slices: &[(i32, i32)]) -> Vec<i32> {
        slices.iter().map(|(a, b)| b - a).collect()
    }

    #[test]
    fn dividing_into_one_returns_the_whole_range() {
        assert_eq!(divide(0, 100, 1, &[]), vec![(0, 100)]);
        assert_eq!(divide(0, 100, 0, &[]), vec![]);
    }

    #[test]
    fn equal_division_tiles_the_range_exactly() {
        for parts in 1..=9_usize {
            let slices = divide(0, 1000, parts, &[]);
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
        let slices = divide(0, 1000, 2, &[100]);
        assert_eq!(slices, vec![(0, 600), (600, 1000)]);

        let slices = divide(0, 1000, 2, &[-100]);
        assert_eq!(slices, vec![(0, 400), (400, 1000)]);
    }

    #[test]
    fn deltas_are_clamped_to_a_fifth_of_the_fair_share() {
        // Two parts of 500 each, minimum 100.
        let slices = divide(0, 1000, 2, &[10_000]);
        assert_eq!(slices, vec![(0, 900), (900, 1000)]);

        let slices = divide(0, 1000, 2, &[-10_000]);
        assert_eq!(slices, vec![(0, 100), (100, 1000)]);
    }

    #[test]
    fn a_huge_delta_still_leaves_room_for_every_later_slice() {
        let slices = divide(0, 1000, 4, &[100_000, 0, 0]);
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
                let slices = divide(0, extent, parts, &[1000, -1000, 5]);
                assert_eq!(slices.len(), parts);
                assert_eq!(slices[0].0, 0);
                assert_eq!(slices[parts - 1].1, extent.max(0));
            }
        }
    }

    #[test]
    fn saturating_deltas_do_not_overflow() {
        let slices = divide(0, 1000, 2, &[i32::MAX]);
        assert_eq!(slices[1].1, 1000);
        let slices = divide(0, 1000, 2, &[i32::MIN]);
        assert_eq!(slices[0].0, 0);
    }

    #[test]
    fn weighted_division_respects_the_weights() {
        let slices = divide_weighted(0, 1000, &[1, 2, 1], &[]);
        assert_eq!(widths(&slices), vec![250, 500, 250]);
        assert_eq!(slices[2].1, 1000);
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
