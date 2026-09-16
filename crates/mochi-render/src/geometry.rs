//! The maths behind a border frame, plus the unclamped rectangle lerp the
//! animator needs.
//!
//! All integer coordinates are physical pixels, the same as [`mochi_core::Rect`]
//! everywhere else in Mochi. The float rectangle is in device independent
//! pixels as Direct2D calls them, but every render target in this crate is
//! pinned to 96 DPI, so one of those is one physical pixel and the two never
//! drift apart.
//!
//! # How a frame is laid out
//!
//! The daemon hands over the rect the window occupies on screen (already
//! corrected for the invisible Windows 11 resize border). The border window
//! covers that rect grown by `width + offset` on every side, and the stroke is
//! drawn with its outer edge flush with the border window:
//!
//! ```text
//!  <- width + offset ->
//! +---------------------------------+  border window
//! |  ####  stroke, `width` thick    |
//! |  ##+-------------------------+  |  target rect
//! |  ##|                         |  |
//! ```
//!
//! With the default offset of `-1` the inner edge of the stroke laps one pixel
//! over the target, which hides the seam between the frame and the window.

use mochi_core::Rect;

/// The corner radius Windows 11 uses for a top level window, in device
/// independent pixels. There is no API that reports it, so it is a constant
/// here and scales with the monitor DPI.
pub const WINDOW_CORNER_RADIUS: f32 = 8.0;

/// A rectangle in Direct2D coordinates, relative to the top left of the border
/// window.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RectF {
    /// The left edge.
    pub left: f32,
    /// The top edge.
    pub top: f32,
    /// The right edge.
    pub right: f32,
    /// The bottom edge.
    pub bottom: f32,
}

impl RectF {
    /// A rectangle from its four edges.
    #[must_use]
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// The width.
    #[must_use]
    pub fn width(self) -> f32 {
        self.right - self.left
    }

    /// The height.
    #[must_use]
    pub fn height(self) -> f32 {
        self.bottom - self.top
    }

    /// The shorter side.
    #[must_use]
    pub fn shorter_side(self) -> f32 {
        self.width().min(self.height())
    }
}

#[cfg(windows)]
impl From<RectF> for windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
    fn from(rect: RectF) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

/// Everything the painter needs to draw one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameGeometry {
    /// Where the border window goes, in physical screen pixels.
    pub window: Rect,
    /// The stroke path, relative to the top left of the border window. Direct2D
    /// centres a stroke on its path, so this rectangle is inset by half the
    /// stroke width.
    pub stroke: RectF,
    /// How thick to draw the stroke.
    pub stroke_width: f32,
    /// The corner radius of the path. Zero means draw a plain rectangle.
    pub radius: f32,
}

impl FrameGeometry {
    /// `true` when there is nothing worth painting, because the target is empty
    /// or the border was configured away.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.window.is_empty() || self.stroke_width <= 0.0 || self.stroke.shorter_side() <= 0.0
    }
}

/// How far the border window reaches past the target rect on every side.
///
/// Can be negative, which draws the frame inside the window instead of around
/// it; that is what a `border_offset` below `-border_width` asks for.
#[must_use]
pub const fn expansion(width: i32, offset: i32) -> i32 {
    width + offset
}

/// The rectangle the border window occupies for a given target.
#[must_use]
pub fn border_window_rect(target: Rect, width: i32, offset: i32) -> Rect {
    let expand = expansion(width, offset);
    Rect::new(
        target.left - expand,
        target.top - expand,
        target.right + expand,
        target.bottom + expand,
    )
}

/// The radius of the stroke path for a frame around a window.
///
/// The window itself is rounded with [`WINDOW_CORNER_RADIUS`] scaled by the
/// monitor DPI. A frame that sits `expansion` pixels outside it is concentric
/// with it, so its outer radius is that much larger, and the path runs down the
/// middle of the stroke, half a stroke width further in.
#[must_use]
pub fn corner_radius(rounded: bool, width: i32, offset: i32, dpi: u32) -> f32 {
    if !rounded {
        return 0.0;
    }
    let scale = dpi as f32 / 96.0;
    let outer = (WINDOW_CORNER_RADIUS * scale) + expansion(width, offset) as f32;
    (outer - width as f32 / 2.0).max(0.0)
}

/// Works out where and how to draw one frame.
///
/// `rounded` picks between a rounded and a square frame; the caller resolves
/// [`crate::BorderStyle::System`] first. `dpi` is the DPI of the monitor the
/// target sits on, which only affects the corner radius: the stroke width is
/// given in physical pixels and stays there.
#[must_use]
pub fn frame_geometry(
    target: Rect,
    width: i32,
    offset: i32,
    rounded: bool,
    dpi: u32,
) -> FrameGeometry {
    let window = border_window_rect(target, width, offset);
    let stroke_width = width.max(0) as f32;
    let half = stroke_width / 2.0;

    let stroke = RectF::new(
        half,
        half,
        window.width().max(0) as f32 - half,
        window.height().max(0) as f32 - half,
    );

    // A radius larger than half the shorter side turns the rounded rectangle
    // into a stadium and then into nothing, so it is capped here.
    let radius = corner_radius(rounded, width, offset, dpi).min(stroke.shorter_side() / 2.0);

    FrameGeometry {
        window,
        stroke,
        stroke_width,
        radius: radius.max(0.0),
    }
}

/// Interpolates between two rectangles **without** clamping `t`.
///
/// [`mochi_core::Rect::lerp`] clamps, which is right for a layout blend and
/// wrong for an animation: the back and elastic curves need to travel outside
/// `0.0..=1.0` for their overshoot.
#[must_use]
pub fn lerp_rect(from: Rect, to: Rect, t: f64) -> Rect {
    if t == 1.0 {
        return to;
    }
    let mix = |a: i32, b: i32| {
        let value = f64::from(a) + (f64::from(b) - f64::from(a)) * t;
        // Screen coordinates are always well inside i32, but an elastic curve
        // on a huge rect could in principle push a rounded value out of range.
        value
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    };
    Rect::new(
        mix(from.left, to.left),
        mix(from.top, to.top),
        mix(from.right, to.right),
        mix(from.bottom, to.bottom),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rice: 6 px wide, pulled one pixel into the window.
    const WIDTH: i32 = 6;
    const OFFSET: i32 = -1;

    #[test]
    fn the_border_window_grows_by_width_plus_offset() {
        let target = Rect::new(100, 100, 500, 400);
        let window = border_window_rect(target, WIDTH, OFFSET);
        assert_eq!(window, Rect::new(95, 95, 505, 405));
        assert_eq!(window.width(), target.width() + 10);
        assert_eq!(window.height(), target.height() + 10);
    }

    #[test]
    fn a_zero_offset_puts_the_whole_stroke_outside_the_target() {
        let target = Rect::new(0, 0, 100, 100);
        let window = border_window_rect(target, 8, 0);
        assert_eq!(window, Rect::new(-8, -8, 108, 108));
    }

    #[test]
    fn a_very_negative_offset_draws_the_frame_inside_the_window() {
        let target = Rect::new(0, 0, 100, 100);
        let window = border_window_rect(target, 6, -10);
        assert_eq!(window, Rect::new(4, 4, 96, 96));
        assert_eq!(expansion(6, -10), -4);
    }

    #[test]
    fn the_stroke_path_is_inset_by_half_the_width() {
        let g = frame_geometry(Rect::new(100, 100, 500, 400), WIDTH, OFFSET, true, 96);
        assert_eq!(g.stroke_width, 6.0);
        assert_eq!(g.stroke.left, 3.0);
        assert_eq!(g.stroke.top, 3.0);
        assert_eq!(g.stroke.right, g.window.width() as f32 - 3.0);
        assert_eq!(g.stroke.bottom, g.window.height() as f32 - 3.0);
    }

    #[test]
    fn the_stroke_covers_exactly_the_configured_thickness() {
        let target = Rect::new(200, 200, 600, 600);
        let g = frame_geometry(target, WIDTH, OFFSET, true, 96);
        // Outer edge of the stroke, in screen coordinates.
        let outer_left = g.window.left as f32 + g.stroke.left - g.stroke_width / 2.0;
        let inner_left = g.window.left as f32 + g.stroke.left + g.stroke_width / 2.0;
        assert_eq!(outer_left, 195.0, "the frame starts at the window edge");
        assert_eq!(
            inner_left, 201.0,
            "and laps one pixel over the target, because the offset is -1"
        );
    }

    #[test]
    fn the_corner_radius_scales_with_dpi() {
        // At 100% the frame is concentric with an 8 px window corner.
        assert_eq!(corner_radius(true, WIDTH, OFFSET, 96), 8.0 + 5.0 - 3.0);
        // 150%, the 4K monitor.
        assert_eq!(corner_radius(true, WIDTH, OFFSET, 144), 12.0 + 5.0 - 3.0);
        // 200%.
        assert_eq!(corner_radius(true, WIDTH, OFFSET, 192), 16.0 + 5.0 - 3.0);
    }

    #[test]
    fn a_square_border_has_no_radius() {
        assert_eq!(corner_radius(false, WIDTH, OFFSET, 144), 0.0);
        let g = frame_geometry(Rect::new(0, 0, 400, 400), WIDTH, OFFSET, false, 144);
        assert_eq!(g.radius, 0.0);
    }

    #[test]
    fn the_radius_never_exceeds_half_the_shorter_side() {
        let g = frame_geometry(Rect::new(0, 0, 20, 400), 6, -1, true, 384);
        assert!(
            g.radius <= g.stroke.shorter_side() / 2.0,
            "radius {} vs side {}",
            g.radius,
            g.stroke.shorter_side()
        );
    }

    #[test]
    fn the_radius_is_never_negative() {
        // A fat border on a tiny window: the path radius would go below zero.
        assert_eq!(corner_radius(true, 40, -40, 96), 0.0);
        let g = frame_geometry(Rect::new(0, 0, 100, 100), 40, -40, true, 96);
        assert!(g.radius >= 0.0);
    }

    #[test]
    fn an_empty_target_yields_an_empty_frame() {
        let g = frame_geometry(Rect::new(10, 10, 10, 10), 0, 0, true, 96);
        assert!(g.is_empty());
        let ok = frame_geometry(Rect::new(0, 0, 200, 200), WIDTH, OFFSET, true, 96);
        assert!(!ok.is_empty());
    }

    #[test]
    fn lerp_hits_both_ends_exactly() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(400, 300, 900, 800);
        assert_eq!(lerp_rect(a, b, 0.0), a);
        assert_eq!(lerp_rect(a, b, 1.0), b);
        assert_eq!(lerp_rect(a, b, 0.5), Rect::new(200, 150, 500, 450));
    }

    #[test]
    fn lerp_does_not_clamp_so_overshoot_survives() {
        let a = Rect::new(0, 0, 100, 100);
        let b = Rect::new(100, 100, 200, 200);
        let past = lerp_rect(a, b, 1.1);
        assert_eq!(past, Rect::new(110, 110, 210, 210));
        let before = lerp_rect(a, b, -0.1);
        assert_eq!(before, Rect::new(-10, -10, 90, 90));
        // The clamping version in mochi-core would have given the end points.
        assert_eq!(a.lerp(&b, 1.1), b);
    }

    #[test]
    fn lerp_keeps_the_size_while_moving() {
        let a = Rect::new(0, 0, 640, 480);
        let b = Rect::new(1000, 200, 1640, 680);
        for step in 0..=10 {
            let r = lerp_rect(a, b, f64::from(step) / 10.0);
            assert_eq!(r.width(), 640);
            assert_eq!(r.height(), 480);
        }
    }
}
