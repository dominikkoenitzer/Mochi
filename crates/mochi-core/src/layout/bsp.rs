//! Binary space partition, the daily driver layout.
//!
//! The first container takes half the area with a vertical cut, the second
//! takes half of what is left with a horizontal cut, and so on, alternating.
//! The last two containers share the final region evenly.

use crate::geometry::{Axis, Rect};

use super::split::{boundary_delta, rect_from_slice, split_two};

/// The rectangle for each of `len` containers inside `area`.
///
/// `min_tile` is the smallest a tile may get. Every cut keeps that much for
/// the container it splits off, and one more for each later cut along the same
/// axis, so the minimum holds all the way down the tree.
pub(crate) fn calculate(
    area: Rect,
    len: usize,
    resize: &[Option<Rect>],
    min_tile: super::MinSize,
) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(len);
    let mut remaining = area;
    let mut axis = Axis::Horizontal;

    for idx in 0..len {
        if idx + 1 == len {
            rects.push(remaining);
            break;
        }

        let delta = boundary_delta(resize, idx, idx + 1, axis);
        // What is left over is cut along this axis again every second step, so
        // it has to keep room for one tile per later cut plus the last one.
        // Every second cut from here divides this same axis, so the room to
        // keep is this axis's floor once per later cut plus the last tile.
        let floor = min_tile.along(axis);
        let later_tiles = (len - idx - 2) / 2 + 1;
        let min_rest = (i64::from(floor) * later_tiles as i64).min(i64::from(i32::MAX)) as i32;
        let start = remaining.start(axis);
        let end = remaining.end(axis);
        let at = split_two(start, end, delta, floor, min_rest);

        rects.push(rect_from_slice(remaining, axis, start, at));
        remaining = rect_from_slice(remaining, axis, at, end);
        axis = axis.other();
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Flip, Layout};

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);

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

    #[test]
    fn many_containers_still_tile_the_area() {
        for len in 1..=20 {
            let rects = bsp(len);
            assert_eq!(rects.len(), len);
            let covered: i64 = rects.iter().map(Rect::area).sum();
            assert_eq!(covered, AREA.area(), "with {len} containers");
        }
    }
}
