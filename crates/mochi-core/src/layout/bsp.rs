//! Binary space partition, the daily driver layout.
//!
//! The first container takes half the area, the second takes half of what is
//! left with a cut the other way, and so on, alternating. The last two
//! containers share the final region evenly.
//!
//! The first cut divides the longer side. On a landscape screen it runs top
//! to bottom and the first two windows sit side by side; on a screen taller
//! than it is wide it runs left to right and they are stacked, each a full
//! width row, instead of two columns too narrow for most applications.

use crate::geometry::{Axis, Rect};

use super::MinSize;
use super::split::{boundary_delta, rect_from_slice, split_two};

/// The axis the first cut of `area` divides: the longer one, so a square or
/// landscape area is cut top to bottom and a portrait one left to right.
const fn first_cut_axis(area: Rect) -> Axis {
    if area.height() > area.width() {
        Axis::Vertical
    } else {
        Axis::Horizontal
    }
}

/// The axis cut number `idx` divides: `first` for the first one, alternating
/// from there.
const fn cut_axis(first: Axis, idx: usize) -> Axis {
    if idx.is_multiple_of(2) {
        first
    } else {
        first.other()
    }
}

/// What the containers from each index to the end need between them.
///
/// The containers after a cut share the region it leaves over, and that
/// region is cut again, so what it has to keep is not the floor of the next
/// tile but of the whole branch: along the axis of a later cut the two sides
/// add up, across it the larger of the two wins.
fn branch_minimums(first_axis: Axis, mins: &[MinSize]) -> Vec<MinSize> {
    let mut need = mins.to_vec();
    for idx in (0..mins.len().saturating_sub(1)).rev() {
        let (first, rest) = (mins[idx], need[idx + 1]);
        need[idx] = match cut_axis(first_axis, idx) {
            Axis::Horizontal => MinSize {
                width: first.width.saturating_add(rest.width),
                height: first.height.max(rest.height),
            },
            Axis::Vertical => MinSize {
                width: first.width.max(rest.width),
                height: first.height.saturating_add(rest.height),
            },
        };
    }
    need
}

/// The rectangle for each of `len` containers inside `area`.
///
/// `mins[i]` is the smallest tile container `i` may get. Every cut keeps that
/// much for the container it splits off, and for the region left over it
/// keeps what the whole rest of the tree needs, so a demanding window deep in
/// the tree takes its room from every branch above it rather than only from
/// its last neighbour.
pub(crate) fn calculate(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    mins: &[MinSize],
    floor: MinSize,
) -> Vec<Rect> {
    let first = first_cut_axis(area);
    let need = branch_minimums(first, mins);
    // What the same branches need from the configured tile floor alone, which
    // every tile keeps even when a window asks for more than there is.
    let floor_need = branch_minimums(first, &vec![floor; len]);
    let mut rects = Vec::with_capacity(len);
    let mut remaining = area;

    for idx in 0..len {
        if idx + 1 == len {
            rects.push(remaining);
            break;
        }

        let axis = cut_axis(first, idx);
        let delta = boundary_delta(resize, idx, idx + 1, axis);
        let start = remaining.start(axis);
        let end = remaining.end(axis);
        let at = split_two(
            start,
            end,
            delta,
            (mins[idx].along(axis), need[idx + 1].along(axis)),
            (floor.along(axis), floor_need[idx + 1].along(axis)),
        );

        rects.push(rect_from_slice(remaining, axis, start, at));
        remaining = rect_from_slice(remaining, axis, at, end);
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Flip, Layout};

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);

    /// The layout with the configured floor at the smallest of `mins`, the
    /// way a workspace hands over tiles whose windows ask for more.
    fn calculate(area: Rect, len: usize, resize: &[Option<Rect>], mins: &[MinSize]) -> Vec<Rect> {
        let floor = mins
            .iter()
            .fold(MinSize::square(i32::MAX), |low, min| MinSize {
                width: low.width.min(min.width),
                height: low.height.min(min.height),
            });
        super::calculate(area, len, resize, mins, floor.clamped())
    }

    fn bsp(len: usize) -> Vec<Rect> {
        Layout::Bsp.calculate(AREA, len, 0, Flip::NONE, &[])
    }

    #[test]
    fn one_container_takes_the_whole_area() {
        assert_eq!(bsp(1), vec![AREA]);
    }

    #[test]
    fn two_containers_split_left_and_right() {
        assert_eq!(
            bsp(2),
            vec![Rect::new(0, 0, 960, 1080), Rect::new(960, 0, 1920, 1080)]
        );
    }

    #[test]
    fn three_containers_stack_the_right_half() {
        assert_eq!(
            bsp(3),
            vec![
                Rect::new(0, 0, 960, 1080),
                Rect::new(960, 0, 1920, 540),
                Rect::new(960, 540, 1920, 1080),
            ]
        );
    }

    #[test]
    fn four_containers_quarter_the_bottom_right() {
        assert_eq!(
            bsp(4),
            vec![
                Rect::new(0, 0, 960, 1080),
                Rect::new(960, 0, 1920, 540),
                Rect::new(960, 540, 1440, 1080),
                Rect::new(1440, 540, 1920, 1080),
            ]
        );
    }

    #[test]
    fn five_and_six_containers_keep_alternating() {
        let five = bsp(5);
        assert_eq!(five[0], Rect::new(0, 0, 960, 1080));
        assert_eq!(five[1], Rect::new(960, 0, 1920, 540));
        assert_eq!(five[2], Rect::new(960, 540, 1440, 1080));
        assert_eq!(five[3], Rect::new(1440, 540, 1920, 810));
        assert_eq!(five[4], Rect::new(1440, 810, 1920, 1080));

        let six = bsp(6);
        assert_eq!(six[..4], five[..4]);
        assert_eq!(six[4], Rect::new(1440, 810, 1680, 1080));
        assert_eq!(six[5], Rect::new(1680, 810, 1920, 1080));
    }

    #[test]
    fn the_first_container_always_gets_half_the_width() {
        for len in 2..=12 {
            let rects = bsp(len);
            assert_eq!(
                rects[0],
                Rect::new(0, 0, 960, 1080),
                "with {len} containers"
            );
        }
    }

    #[test]
    fn every_cut_alternates_axis() {
        // Odd containers extend the previous one sideways, even ones downwards.
        let rects = bsp(4);
        assert_eq!(rects[0].height(), AREA.height(), "cut 0 runs top to bottom");
        assert_eq!(rects[1].width(), 960, "cut 1 runs left to right");
        assert_eq!(rects[2].height(), 540, "cut 2 runs top to bottom again");
    }

    #[test]
    fn a_resize_delta_moves_the_first_cut() {
        let resize = vec![Some(Rect::new(0, 0, 240, 0)), None, None];
        let rects = Layout::Bsp.calculate(AREA, 3, 0, Flip::NONE, &resize);
        assert_eq!(rects[0], Rect::new(0, 0, 1200, 1080));
        assert_eq!(rects[1], Rect::new(1200, 0, 1920, 540));
        assert_eq!(rects[2], Rect::new(1200, 540, 1920, 1080));
    }

    #[test]
    fn a_resize_delta_on_a_later_container_moves_its_own_cut() {
        // The cut between containers 1 and 2 is horizontal, so container 1's
        // bottom edge moves it.
        let resize = vec![None, Some(Rect::new(0, 0, 0, 100)), None];
        let rects = Layout::Bsp.calculate(AREA, 3, 0, Flip::NONE, &resize);
        assert_eq!(
            rects[0],
            Rect::new(0, 0, 960, 1080),
            "the first cut is untouched"
        );
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 640));
        assert_eq!(rects[2], Rect::new(960, 640, 1920, 1080));
    }

    #[test]
    fn the_last_container_can_push_back_on_its_own_cut() {
        let resize = vec![None, None, Some(Rect::new(0, -100, 0, 0))];
        let rects = Layout::Bsp.calculate(AREA, 3, 0, Flip::NONE, &resize);
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 440));
        assert_eq!(rects[2], Rect::new(960, 440, 1920, 1080));
    }

    // -- portrait areas -----------------------------------------------------

    const PORTRAIT: Rect = Rect::new(0, 0, 1080, 1920);

    #[test]
    fn two_containers_on_a_portrait_area_are_stacked_in_equal_halves() {
        let rects = Layout::Bsp.calculate(PORTRAIT, 2, 0, Flip::NONE, &[]);
        assert_eq!(
            rects,
            vec![Rect::new(0, 0, 1080, 960), Rect::new(0, 960, 1080, 1920)]
        );
    }

    #[test]
    fn a_portrait_area_alternates_starting_with_a_left_right_cut() {
        let rects = Layout::Bsp.calculate(PORTRAIT, 3, 0, Flip::NONE, &[]);
        assert_eq!(rects[0], Rect::new(0, 0, 1080, 960));
        assert_eq!(rects[1], Rect::new(0, 960, 540, 1920));
        assert_eq!(rects[2], Rect::new(540, 960, 1080, 1920));
    }

    #[test]
    fn a_square_area_is_cut_top_to_bottom_first_as_before() {
        let square = Rect::new(0, 0, 1000, 1000);
        let rects = Layout::Bsp.calculate(square, 2, 0, Flip::NONE, &[]);
        assert_eq!(
            rects,
            vec![Rect::new(0, 0, 500, 1000), Rect::new(500, 0, 1000, 1000)]
        );
    }

    #[test]
    fn a_wide_window_on_a_portrait_area_no_longer_squeezes_its_neighbour() {
        // A 1080 by 1920 panel at 100 percent: 14 pixels of workspace padding
        // and a 48 pixel taskbar leave 1052 by 1844. A music player that
        // accepts nothing under 800 by 600 used to take 820 of the 1052
        // pixels of a left and right cut, leaving 232 for the mail client
        // beside it. Stacked, both get the full width and half the height.
        let area = Rect::new(14, 14, 1066, 1858);
        let mins = [
            MinSize {
                width: 820,
                height: 620,
            },
            MinSize {
                width: 140,
                height: 83,
            },
        ];
        let rects = calculate(area, 2, &[], &mins);
        assert_eq!(rects[0], Rect::new(14, 14, 1066, 936));
        assert_eq!(rects[1], Rect::new(14, 936, 1066, 1858));
    }

    #[test]
    fn a_resize_delta_on_a_portrait_area_moves_the_stacked_cut() {
        let resize = vec![Some(Rect::new(0, 0, 0, 240)), None];
        let rects = Layout::Bsp.calculate(PORTRAIT, 2, 0, Flip::NONE, &resize);
        assert_eq!(rects[0], Rect::new(0, 0, 1080, 1200));
        assert_eq!(rects[1], Rect::new(0, 1200, 1080, 1920));
    }

    #[test]
    fn many_containers_still_tile_a_portrait_area() {
        for len in 1..=20 {
            let rects = Layout::Bsp.calculate(PORTRAIT, len, 0, Flip::NONE, &[]);
            let covered: i64 = rects.iter().map(Rect::area).sum();
            assert_eq!(covered, PORTRAIT.area(), "with {len} containers");
        }
    }

    #[test]
    fn many_containers_still_tile_the_area() {
        for len in 1..=20 {
            let rects = bsp(len);
            assert_eq!(rects.len(), len);
            let covered: i64 = rects.iter().map(Rect::area).sum();
            assert_eq!(covered, AREA.area(), "with {len} containers");
        }
    }

    // -- window minimums ----------------------------------------------------

    /// Every container at the default floor, except the ones given.
    fn mins_with(len: usize, demanding: &[(usize, MinSize)]) -> Vec<MinSize> {
        let mut mins = vec![MinSize::default(); len];
        for (idx, min) in demanding {
            mins[*idx] = *min;
        }
        mins
    }

    const fn wide(width: i32) -> MinSize {
        MinSize { width, height: 64 }
    }

    const fn tall(height: i32) -> MinSize {
        MinSize { width: 64, height }
    }

    fn assert_tiles(rects: &[Rect]) {
        let covered: i64 = rects.iter().map(Rect::area).sum();
        assert_eq!(covered, AREA.area(), "{rects:?}");
        for (i, a) in rects.iter().enumerate() {
            assert!(AREA.contains_rect(a), "rect {i} escapes: {rects:?}");
            for (j, b) in rects.iter().enumerate().skip(i + 1) {
                assert!(!a.intersects(b), "rects {i} and {j} overlap: {rects:?}");
            }
        }
    }

    #[test]
    fn the_floor_of_a_branch_adds_along_its_cuts_and_takes_the_larger_across() {
        let mins = [wide(300), tall(200), wide(400), tall(500)];
        let need = branch_minimums(Axis::Horizontal, &mins);
        assert_eq!(need[3], tall(500));
        // Cut 2 runs top to bottom: widths add, the taller height wins.
        assert_eq!(
            need[2],
            MinSize {
                width: 464,
                height: 500
            }
        );
        // Cut 1 runs left to right: heights add, the wider width wins.
        assert_eq!(
            need[1],
            MinSize {
                width: 464,
                height: 700
            }
        );
        assert_eq!(
            need[0],
            MinSize {
                width: 764,
                height: 700
            }
        );
    }

    #[test]
    fn branch_floors_saturate_instead_of_overflowing() {
        let mins = vec![MinSize::square(i32::MAX); 6];
        for need in branch_minimums(Axis::Horizontal, &mins) {
            assert_eq!(need, MinSize::square(i32::MAX));
        }
        assert!(branch_minimums(Axis::Horizontal, &[]).is_empty());
    }

    #[test]
    fn a_window_minimum_that_fits_its_half_moves_nothing() {
        let mins = mins_with(4, &[(2, wide(400))]);
        assert_eq!(calculate(AREA, 4, &[], &mins), bsp(4));
    }

    #[test]
    fn a_wide_window_in_the_second_container_moves_the_first_cut() {
        let mins = mins_with(2, &[(1, wide(1200))]);
        let rects = calculate(AREA, 2, &[], &mins);
        assert_eq!(rects[0], Rect::new(0, 0, 720, 1080));
        assert_eq!(rects[1], Rect::new(720, 0, 1920, 1080));
    }

    #[test]
    fn a_wide_window_deep_in_the_tree_moves_the_first_cut() {
        // Container 3 of 4 sits in the bottom right quarter. Its sibling alone
        // cannot give it 1200 pixels, so the first cut has to give way too.
        let mins = mins_with(4, &[(2, wide(1200))]);
        let rects = calculate(AREA, 4, &[], &mins);
        assert!(
            rects[0].right < 960,
            "the first cut did not move: {rects:?}"
        );
        assert!(
            rects[2].width() >= 1200,
            "container 3 got {} pixels: {rects:?}",
            rects[2].width()
        );
        assert_tiles(&rects);
    }

    #[test]
    fn a_wide_window_in_the_fifth_of_six_containers_moves_the_first_cut() {
        let mins = mins_with(6, &[(4, wide(1200))]);
        let rects = calculate(AREA, 6, &[], &mins);
        assert!(
            rects[0].right < 960,
            "the first cut did not move: {rects:?}"
        );
        assert!(
            rects[4].width() >= 1200,
            "container 5 got {} pixels: {rects:?}",
            rects[4].width()
        );
        assert_tiles(&rects);
    }

    #[test]
    fn a_deep_window_with_a_sibling_that_also_needs_room_moves_the_first_cut() {
        // Both sides of the last cut ask for more than its fair-share floor,
        // so nothing but the window minimums decides where the cuts go.
        let mins = mins_with(4, &[(2, wide(1100)), (3, wide(400))]);
        let rects = calculate(AREA, 4, &[], &mins);
        assert_eq!(rects[0], Rect::new(0, 0, 420, 1080));
        assert_eq!(rects[2].width(), 1100);
        assert_eq!(rects[3].width(), 400);
        assert_tiles(&rects);
    }

    #[test]
    fn a_tall_window_across_a_left_right_cut_still_takes_height_from_the_cut_above() {
        // Container 4 of 4 shares the bottom region with container 3 across
        // a cut that runs top to bottom, so its height is the whole region's.
        let mins = mins_with(4, &[(3, tall(900))]);
        let rects = calculate(AREA, 4, &[], &mins);
        assert_eq!(rects[0], Rect::new(0, 0, 960, 1080), "width is not needed");
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 180));
        assert_eq!(rects[2].height(), 900);
        assert_eq!(rects[3].height(), 900);
        assert_tiles(&rects);
    }

    #[test]
    fn two_tall_windows_side_by_side_need_the_taller_height_not_the_sum() {
        let mins = mins_with(4, &[(2, tall(500)), (3, tall(520))]);
        let rects = calculate(AREA, 4, &[], &mins);
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 540), "520 fits under 540");
        assert_tiles(&rects);
    }

    #[test]
    fn no_resize_delta_pushes_a_cut_into_a_deep_windows_minimum() {
        let mins = mins_with(4, &[(2, wide(500)), (3, tall(700))]);
        for delta in [i32::MIN, -100_000, -500, 500, 100_000, i32::MAX] {
            let resize = vec![
                Some(Rect::new(0, 0, delta, 0)),
                Some(Rect::new(0, 0, 0, delta)),
                Some(Rect::new(0, 0, delta, 0)),
                None,
            ];
            let rects = calculate(AREA, 4, &resize, &mins);
            assert!(rects[2].width() >= 500, "{delta}: {rects:?}");
            assert!(rects[3].height() >= 700, "{delta}: {rects:?}");
            assert_tiles(&rects);
        }
    }

    #[test]
    fn absurd_branch_minimums_still_tile_the_area() {
        for len in 1..=8 {
            for demanding in 0..len {
                for min in [MinSize::square(i32::MAX), wide(i32::MAX), tall(5_000)] {
                    let mins = mins_with(len, &[(demanding, min)]);
                    let rects = calculate(AREA, len, &[], &mins);
                    assert_eq!(rects.len(), len);
                    let covered: i64 = rects.iter().map(Rect::area).sum();
                    assert_eq!(covered, AREA.area(), "{len} {demanding} {min:?}");
                }
            }
        }
    }
}
