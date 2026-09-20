//! Assertions an end-to-end tiling test can make about a set of window rects.
//!
//! Every helper comes in two forms: a `check_*` function that returns the first
//! [`Violation`] it finds, so a test can retry while the layout settles, and an
//! `assert_*` wrapper that panics with the same message once the test has
//! waited long enough.
//!
//! Feed these the perceived rectangles, `DWMWA_EXTENDED_FRAME_BOUNDS`, not
//! `GetWindowRect`: a Windows 11 window carries an invisible resize border of
//! roughly seven physical pixels per side at 100% scaling, so the raw window
//! rects of two perfectly tiled windows always overlap.

use crate::geometry::Rect;

/// What went wrong in a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Two windows share pixels.
    Overlap {
        /// Index of the first rectangle in the slice that was checked.
        a: usize,
        /// Index of the second rectangle.
        b: usize,
        /// The shared pixels.
        overlap: Rect,
    },
    /// A window is not fully inside the area it was supposed to tile.
    Outside {
        /// Index of the offending rectangle.
        index: usize,
        /// The rectangle itself.
        rect: Rect,
        /// The area it should have stayed in.
        area: Rect,
    },
    /// A window whose rectangle covers no pixel at all.
    Degenerate {
        /// Index of the offending rectangle in the slice that was checked.
        index: usize,
        /// The rectangle itself.
        rect: Rect,
    },
    /// A hole in the tiling that is larger than the tolerance in both directions.
    Gap {
        /// The uncovered block.
        gap: Rect,
        /// The area that should have been covered.
        area: Rect,
        /// The tolerance that was allowed.
        tolerance: i32,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overlap { a, b, overlap } => write!(
                f,
                "windows {a} and {b} overlap on {overlap}, {} px",
                overlap.area()
            ),
            Self::Outside { index, rect, area } => {
                write!(f, "window {index} at {rect} leaves the area {area}")
            }
            Self::Degenerate { index, rect } => {
                write!(f, "window {index} at {rect} covers no pixel")
            }
            Self::Gap {
                gap,
                area,
                tolerance,
            } => write!(
                f,
                "uncovered {gap} inside {area}, larger than the {tolerance} px tolerance"
            ),
        }
    }
}

impl std::error::Error for Violation {}

/// Checks that no two rectangles share a pixel, and that none of them is empty.
///
/// A rectangle that covers no pixel overlaps nothing, so skipping it would let
/// a layout that collapsed a window pass a check that claims it is correct. A
/// minimized or hidden window is the caller's to leave out of the slice.
///
/// # Errors
/// Returns the first empty rectangle, then the first overlapping pair.
pub fn check_no_overlap(rects: &[Rect]) -> Result<(), Violation> {
    check_none_degenerate(rects)?;
    for (a, ra) in rects.iter().enumerate() {
        for (b, rb) in rects.iter().enumerate().skip(a + 1) {
            if let Some(overlap) = ra.intersection(rb) {
                return Err(Violation::Overlap { a, b, overlap });
            }
        }
    }
    Ok(())
}

/// The first rectangle that covers no pixel, which every check rejects.
fn check_none_degenerate(rects: &[Rect]) -> Result<(), Violation> {
    match rects.iter().position(Rect::is_empty) {
        Some(index) => Err(Violation::Degenerate {
            index,
            rect: rects[index],
        }),
        None => Ok(()),
    }
}

/// Checks that every rectangle is fully inside `area`, and that none of them is
/// empty.
///
/// # Errors
/// Returns the first empty rectangle, then the first one that pokes out.
pub fn check_all_within(rects: &[Rect], area: Rect) -> Result<(), Violation> {
    check_none_degenerate(rects)?;
    for (index, rect) in rects.iter().enumerate() {
        if !area.contains(rect) {
            return Err(Violation::Outside {
                index,
                rect: *rect,
                area,
            });
        }
    }
    Ok(())
}

/// Checks that the rectangles together cover `area`, allowing gaps that are at
/// most `tolerance_px` thick in one direction.
///
/// The tolerance is what makes this usable against a real desktop: a configured
/// window gap, the invisible border and rounding when an odd number of pixels
/// is split all leave thin seams. A hole that is larger than the tolerance in
/// both directions is a real layout bug.
///
/// # Errors
/// Returns the first empty rectangle, then the largest hole that is too large.
pub fn check_covers(rects: &[Rect], area: Rect, tolerance_px: i32) -> Result<(), Violation> {
    check_none_degenerate(rects)?;
    if area.is_empty() {
        return Ok(());
    }

    let clipped: Vec<Rect> = rects.iter().filter_map(|r| r.intersection(&area)).collect();

    // Compress the coordinates: every hole is a union of cells of this grid.
    let mut xs = vec![area.left, area.right];
    let mut ys = vec![area.top, area.bottom];
    for r in &clipped {
        xs.push(r.left);
        xs.push(r.right);
        ys.push(r.top);
        ys.push(r.bottom);
    }
    xs.sort_unstable();
    xs.dedup();
    ys.sort_unstable();
    ys.dedup();

    let cols = xs.len() - 1;
    let rows = ys.len() - 1;
    let mut covered = vec![false; cols * rows];
    for (row, cell_row) in covered.chunks_mut(cols).enumerate() {
        for (col, cell) in cell_row.iter_mut().enumerate() {
            let cell_rect = Rect::new(xs[col], ys[row], xs[col + 1], ys[row + 1]);
            *cell = clipped.iter().any(|r| r.contains(&cell_rect));
        }
    }

    // The hole to look for is a whole uncovered block, not a run in a single
    // direction. Merging only across a row and only down a column misses a
    // hole that the edges of other frames slice both ways: every one of its
    // runs is then as thin as the distance between two of those edges, however
    // large the hole itself is. So every band of rows is taken in turn and the
    // longest run of columns that is uncovered over all of them is measured.
    let mut worst: Option<Rect> = None;
    for top in 0..rows {
        let mut free: Vec<bool> = covered[top * cols..(top + 1) * cols]
            .iter()
            .map(|cell| !cell)
            .collect();
        for bottom in top..rows {
            if bottom > top {
                for (col, open) in free.iter_mut().enumerate() {
                    *open = *open && !covered[bottom * cols + col];
                }
            }
            if ys[bottom + 1] - ys[top] <= tolerance_px {
                continue;
            }
            let mut start: Option<usize> = None;
            for col in 0..=cols {
                match (col < cols && free[col], start) {
                    (true, None) => start = Some(col),
                    (false, Some(s)) => {
                        let gap = Rect::new(xs[s], ys[top], xs[col], ys[bottom + 1]);
                        if gap.width() > tolerance_px
                            && worst.is_none_or(|worst| gap.area() > worst.area())
                        {
                            worst = Some(gap);
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
        }
    }
    match worst {
        Some(gap) => Err(Violation::Gap {
            gap,
            area,
            tolerance: tolerance_px,
        }),
        None => Ok(()),
    }
}

/// Panicking form of [`check_no_overlap`].
///
/// # Panics
/// When two rectangles overlap.
pub fn assert_no_overlap(rects: &[Rect]) {
    if let Err(violation) = check_no_overlap(rects) {
        panic!("layout: {violation}\nrects: {rects:?}");
    }
}

/// Panicking form of [`check_all_within`].
///
/// # Panics
/// When a rectangle leaves the area.
pub fn assert_all_within(rects: &[Rect], area: Rect) {
    if let Err(violation) = check_all_within(rects, area) {
        panic!("layout: {violation}\nrects: {rects:?}");
    }
}

/// Panicking form of [`check_covers`].
///
/// # Panics
/// When a hole larger than the tolerance is left uncovered.
pub fn assert_covers(rects: &[Rect], area: Rect, tolerance_px: i32) {
    if let Err(violation) = check_covers(rects, area, tolerance_px) {
        panic!("layout: {violation}\nrects: {rects:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1000, 800);

    fn columns(n: i32) -> Vec<Rect> {
        (0..n)
            .map(|i| Rect::new(i * 1000 / n, 0, (i + 1) * 1000 / n, 800))
            .collect()
    }

    #[test]
    fn a_clean_column_layout_passes_every_check() {
        let rects = columns(3);
        assert_no_overlap(&rects);
        assert_all_within(&rects, AREA);
        assert_covers(&rects, AREA, 0);
    }

    #[test]
    fn overlap_names_both_windows() {
        let rects = [Rect::new(0, 0, 500, 800), Rect::new(400, 0, 1000, 800)];
        let err = check_no_overlap(&rects).unwrap_err();
        assert_eq!(
            err,
            Violation::Overlap {
                a: 0,
                b: 1,
                overlap: Rect::new(400, 0, 500, 800),
            }
        );
        assert!(err.to_string().contains("overlap"));
    }

    #[test]
    fn an_empty_rect_is_a_violation_rather_than_a_rect_that_overlaps_nothing() {
        let rects = [Rect::new(0, 0, 500, 800), Rect::new(100, 100, 100, 100)];
        assert_eq!(
            check_no_overlap(&rects),
            Err(Violation::Degenerate {
                index: 1,
                rect: Rect::new(100, 100, 100, 100),
            })
        );
        assert!(
            check_no_overlap(&rects)
                .unwrap_err()
                .to_string()
                .contains("covers no pixel")
        );
    }

    #[test]
    fn a_window_poking_out_of_the_area_is_caught() {
        let rects = [Rect::new(0, 0, 500, 800), Rect::new(500, 0, 1001, 800)];
        assert_eq!(
            check_all_within(&rects, AREA),
            Err(Violation::Outside {
                index: 1,
                rect: Rect::new(500, 0, 1001, 800),
                area: AREA,
            })
        );
    }

    #[test]
    fn a_gap_within_the_tolerance_is_allowed() {
        // Two columns with an eight pixel seam, as a window gap would leave.
        let rects = [Rect::new(0, 0, 496, 800), Rect::new(504, 0, 1000, 800)];
        assert!(check_covers(&rects, AREA, 8).is_ok());
        assert!(check_covers(&rects, AREA, 4).is_err());
    }

    #[test]
    fn a_missing_window_leaves_a_hole_that_is_reported() {
        let rects = [Rect::new(0, 0, 500, 800)];
        let err = check_covers(&rects, AREA, 8).unwrap_err();
        assert_eq!(
            err,
            Violation::Gap {
                gap: Rect::new(500, 0, 1000, 800),
                area: AREA,
                tolerance: 8,
            }
        );
    }

    #[test]
    fn a_hole_sliced_by_other_edges_is_still_found() {
        // The hole in the bottom right is cut in two by the edge at x = 700, so
        // the row pass sees two thin pieces; the column pass still catches it.
        let rects = [
            Rect::new(0, 0, 1000, 400),
            Rect::new(0, 400, 500, 800),
            Rect::new(700, 400, 1000, 500),
        ];
        assert!(check_covers(&rects, AREA, 8).is_err());
    }

    #[test]
    fn covering_an_empty_area_is_vacuously_true() {
        assert!(check_covers(&[], Rect::default(), 0).is_ok());
    }

    #[test]
    fn a_collapsed_window_satisfies_nothing() {
        // One full screen window and three windows a layout squeezed to
        // nothing. Every check here claims the layout is correct, so none of
        // them may accept a rectangle that covers no pixel.
        let rects = [
            Rect::new(0, 0, 1000, 800),
            Rect::new(500, 400, 500, 400),
            Rect::default(),
            Rect::new(-32_000, -32_000, -32_000, -32_000),
        ];
        assert!(
            check_no_overlap(&rects).is_err(),
            "no overlap accepted them"
        );
        assert!(
            check_all_within(&rects, AREA).is_err(),
            "all within accepted them"
        );
        assert!(
            check_covers(&rects, AREA, 8).is_err(),
            "covers accepted them"
        );
    }

    #[test]
    fn a_hole_the_frame_edges_slice_in_both_directions_is_found() {
        // A 100x100 hole at 400,350 with a tolerance of 34, the tolerance a 4K
        // desktop at 150% produces. Frames above it put x edges at 425, 450 and
        // 475 inside its width, frames beside it put y edges at 375, 400 and
        // 425 inside its height, so no run in a single direction is ever
        // thicker than the 25 px between two of those edges.
        let rects = [
            Rect::new(0, 0, 425, 350),
            Rect::new(425, 0, 450, 350),
            Rect::new(450, 0, 475, 350),
            Rect::new(475, 0, 1000, 350),
            Rect::new(0, 350, 400, 375),
            Rect::new(0, 375, 400, 400),
            Rect::new(0, 400, 400, 425),
            Rect::new(0, 425, 400, 450),
            Rect::new(500, 350, 1000, 450),
            Rect::new(0, 450, 1000, 800),
        ];
        assert_no_overlap(&rects);
        assert_eq!(
            check_covers(&rects, AREA, 34),
            Err(Violation::Gap {
                gap: Rect::new(400, 350, 500, 450),
                area: AREA,
                tolerance: 34,
            })
        );
    }

    #[test]
    #[should_panic(expected = "overlap")]
    fn the_assert_form_panics_with_the_message() {
        assert_no_overlap(&[Rect::new(0, 0, 10, 10), Rect::new(5, 5, 20, 20)]);
    }
}
