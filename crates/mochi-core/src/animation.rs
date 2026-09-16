//! Easing functions and rectangle animation.
//!
//! Everything here is pure arithmetic. The daemon asks an [`Animation`] for the
//! rectangle of frame `n` and hands that to `SetWindowPos`; this module never
//! sleeps and never knows what time it is.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;

/// The default animation length in milliseconds.
pub const DEFAULT_DURATION_MS: u64 = 250;
/// The default animation frame rate.
pub const DEFAULT_FPS: u32 = 60;

/// A standard easing curve.
///
/// The names are the ones from easings.net, which is also what the existing
/// config format uses, so `"style": "EaseOutQuad"` in a migrated config keeps
/// working.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[non_exhaustive]
pub enum AnimationStyle {
    /// No easing at all.
    #[default]
    Linear,
    /// Sine, slow start.
    EaseInSine,
    /// Sine, slow end.
    EaseOutSine,
    /// Sine, slow at both ends.
    EaseInOutSine,
    /// Quadratic, slow start.
    EaseInQuad,
    /// Quadratic, slow end. The usual choice for window movement.
    EaseOutQuad,
    /// Quadratic, slow at both ends.
    EaseInOutQuad,
    /// Cubic, slow start.
    EaseInCubic,
    /// Cubic, slow end.
    EaseOutCubic,
    /// Cubic, slow at both ends.
    EaseInOutCubic,
    /// Quartic, slow start.
    EaseInQuart,
    /// Quartic, slow end.
    EaseOutQuart,
    /// Quartic, slow at both ends.
    EaseInOutQuart,
    /// Quintic, slow start.
    EaseInQuint,
    /// Quintic, slow end.
    EaseOutQuint,
    /// Quintic, slow at both ends.
    EaseInOutQuint,
    /// Exponential, slow start.
    EaseInExpo,
    /// Exponential, slow end.
    EaseOutExpo,
    /// Exponential, slow at both ends.
    EaseInOutExpo,
    /// Circular, slow start.
    EaseInCirc,
    /// Circular, slow end.
    EaseOutCirc,
    /// Circular, slow at both ends.
    EaseInOutCirc,
    /// Overshoots backwards at the start.
    EaseInBack,
    /// Overshoots forwards at the end.
    EaseOutBack,
    /// Overshoots at both ends.
    EaseInOutBack,
    /// Springs at the start.
    EaseInElastic,
    /// Springs at the end.
    EaseOutElastic,
    /// Springs at both ends.
    EaseInOutElastic,
    /// Bounces at the start.
    EaseInBounce,
    /// Bounces at the end.
    EaseOutBounce,
    /// Bounces at both ends.
    EaseInOutBounce,
}

const BACK_C1: f64 = 1.701_58;
const BACK_C2: f64 = BACK_C1 * 1.525;
const BACK_C3: f64 = BACK_C1 + 1.0;
const ELASTIC_C4: f64 = std::f64::consts::TAU / 3.0;
const ELASTIC_C5: f64 = std::f64::consts::TAU / 4.5;

impl AnimationStyle {
    /// Every style, in declaration order.
    pub const ALL: [AnimationStyle; 31] = [
        AnimationStyle::Linear,
        AnimationStyle::EaseInSine,
        AnimationStyle::EaseOutSine,
        AnimationStyle::EaseInOutSine,
        AnimationStyle::EaseInQuad,
        AnimationStyle::EaseOutQuad,
        AnimationStyle::EaseInOutQuad,
        AnimationStyle::EaseInCubic,
        AnimationStyle::EaseOutCubic,
        AnimationStyle::EaseInOutCubic,
        AnimationStyle::EaseInQuart,
        AnimationStyle::EaseOutQuart,
        AnimationStyle::EaseInOutQuart,
        AnimationStyle::EaseInQuint,
        AnimationStyle::EaseOutQuint,
        AnimationStyle::EaseInOutQuint,
        AnimationStyle::EaseInExpo,
        AnimationStyle::EaseOutExpo,
        AnimationStyle::EaseInOutExpo,
        AnimationStyle::EaseInCirc,
        AnimationStyle::EaseOutCirc,
        AnimationStyle::EaseInOutCirc,
        AnimationStyle::EaseInBack,
        AnimationStyle::EaseOutBack,
        AnimationStyle::EaseInOutBack,
        AnimationStyle::EaseInElastic,
        AnimationStyle::EaseOutElastic,
        AnimationStyle::EaseInOutElastic,
        AnimationStyle::EaseInBounce,
        AnimationStyle::EaseOutBounce,
        AnimationStyle::EaseInOutBounce,
    ];

    /// Maps progress `t` in `0.0..=1.0` onto the eased value.
    ///
    /// `evaluate(0.0)` is always `0.0` and `evaluate(1.0)` is always `1.0`.
    /// The overshooting styles can leave that range in between, which is the
    /// point of them.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn evaluate(self, t: f64) -> f64 {
        use std::f64::consts::PI;
        let t = t.clamp(0.0, 1.0);

        match self {
            Self::Linear => t,

            Self::EaseInSine => 1.0 - ((t * PI) / 2.0).cos(),
            Self::EaseOutSine => ((t * PI) / 2.0).sin(),
            Self::EaseInOutSine => -((PI * t).cos() - 1.0) / 2.0,

            Self::EaseInQuad => t * t,
            Self::EaseOutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Self::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }

            Self::EaseInCubic => t.powi(3),
            Self::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Self::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t.powi(3)
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }

            Self::EaseInQuart => t.powi(4),
            Self::EaseOutQuart => 1.0 - (1.0 - t).powi(4),
            Self::EaseInOutQuart => {
                if t < 0.5 {
                    8.0 * t.powi(4)
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(4) / 2.0
                }
            }

            Self::EaseInQuint => t.powi(5),
            Self::EaseOutQuint => 1.0 - (1.0 - t).powi(5),
            Self::EaseInOutQuint => {
                if t < 0.5 {
                    16.0 * t.powi(5)
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(5) / 2.0
                }
            }

            Self::EaseInExpo => {
                if t == 0.0 {
                    0.0
                } else {
                    (2.0_f64).powf(10.0 * t - 10.0)
                }
            }
            Self::EaseOutExpo => {
                if t == 1.0 {
                    1.0
                } else {
                    1.0 - (2.0_f64).powf(-10.0 * t)
                }
            }
            Self::EaseInOutExpo => {
                if t == 0.0 {
                    0.0
                } else if t == 1.0 {
                    1.0
                } else if t < 0.5 {
                    (2.0_f64).powf(20.0 * t - 10.0) / 2.0
                } else {
                    (2.0 - (2.0_f64).powf(-20.0 * t + 10.0)) / 2.0
                }
            }

            Self::EaseInCirc => 1.0 - (1.0 - t.powi(2)).max(0.0).sqrt(),
            Self::EaseOutCirc => (1.0 - (t - 1.0).powi(2)).max(0.0).sqrt(),
            Self::EaseInOutCirc => {
                if t < 0.5 {
                    (1.0 - (1.0 - (2.0 * t).powi(2)).max(0.0).sqrt()) / 2.0
                } else {
                    ((1.0 - (-2.0 * t + 2.0).powi(2)).max(0.0).sqrt() + 1.0) / 2.0
                }
            }

            Self::EaseInBack => BACK_C3 * t.powi(3) - BACK_C1 * t.powi(2),
            Self::EaseOutBack => 1.0 + BACK_C3 * (t - 1.0).powi(3) + BACK_C1 * (t - 1.0).powi(2),
            Self::EaseInOutBack => {
                if t < 0.5 {
                    ((2.0 * t).powi(2) * ((BACK_C2 + 1.0) * 2.0 * t - BACK_C2)) / 2.0
                } else {
                    ((2.0 * t - 2.0).powi(2) * ((BACK_C2 + 1.0) * (t * 2.0 - 2.0) + BACK_C2) + 2.0)
                        / 2.0
                }
            }

            Self::EaseInElastic => {
                if t == 0.0 {
                    0.0
                } else if t == 1.0 {
                    1.0
                } else {
                    -(2.0_f64).powf(10.0 * t - 10.0) * ((t * 10.0 - 10.75) * ELASTIC_C4).sin()
                }
            }
            Self::EaseOutElastic => {
                if t == 0.0 {
                    0.0
                } else if t == 1.0 {
                    1.0
                } else {
                    (2.0_f64).powf(-10.0 * t) * ((t * 10.0 - 0.75) * ELASTIC_C4).sin() + 1.0
                }
            }
            Self::EaseInOutElastic => {
                if t == 0.0 {
                    0.0
                } else if t == 1.0 {
                    1.0
                } else if t < 0.5 {
                    -((2.0_f64).powf(20.0 * t - 10.0) * ((20.0 * t - 11.125) * ELASTIC_C5).sin())
                        / 2.0
                } else {
                    ((2.0_f64).powf(-20.0 * t + 10.0) * ((20.0 * t - 11.125) * ELASTIC_C5).sin())
                        / 2.0
                        + 1.0
                }
            }

            Self::EaseInBounce => 1.0 - bounce_out(1.0 - t),
            Self::EaseOutBounce => bounce_out(t),
            Self::EaseInOutBounce => {
                if t < 0.5 {
                    (1.0 - bounce_out(1.0 - 2.0 * t)) / 2.0
                } else {
                    (1.0 + bounce_out(2.0 * t - 1.0)) / 2.0
                }
            }
        }
    }
}

fn bounce_out(t: f64) -> f64 {
    const N1: f64 = 7.5625;
    const D1: f64 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.937_5
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984_375
    }
}

impl std::fmt::Display for AnimationStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::str::FromStr for AnimationStyle {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Self::ALL
            .into_iter()
            .find(|style| {
                let name: String = format!("{style:?}").to_ascii_lowercase();
                name == wanted
            })
            .ok_or_else(|| crate::Error::Parse {
                kind: "animation style",
                value: s.to_string(),
            })
    }
}

/// A rectangle moving from one place to another over a fixed number of frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animation {
    start: Rect,
    end: Rect,
    duration: Duration,
    fps: u32,
    style: AnimationStyle,
}

impl Animation {
    /// An animation from `start` to `end`.
    ///
    /// `duration_ms` and `fps` are both clamped to at least one, so there is
    /// always at least one frame and the frame interval is never zero.
    #[must_use]
    pub fn new(start: Rect, end: Rect, duration_ms: u64, fps: u32, style: AnimationStyle) -> Self {
        Self {
            start,
            end,
            duration: Duration::from_millis(duration_ms.max(1)),
            fps: fps.max(1),
            style,
        }
    }

    /// The rectangle the animation starts from.
    #[must_use]
    pub const fn start(&self) -> Rect {
        self.start
    }

    /// The rectangle the animation ends at.
    #[must_use]
    pub const fn end(&self) -> Rect {
        self.end
    }

    /// The configured style.
    #[must_use]
    pub const fn style(&self) -> AnimationStyle {
        self.style
    }

    /// How long the whole animation takes.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// How long to wait between two frames.
    #[must_use]
    pub fn frame_interval(&self) -> Duration {
        Duration::from_nanos(1_000_000_000 / u64::from(self.fps))
    }

    /// The number of frames, including the final one.
    ///
    /// Always at least two: the start and the end.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        let millis = self.duration.as_millis().max(1) as u64;
        let frames = (millis * u64::from(self.fps)).div_ceil(1000);
        (frames as usize).max(1) + 1
    }

    /// The rectangle of frame `idx`.
    ///
    /// Frame `0` is the start rectangle and the last frame is exactly the end
    /// rectangle, however the easing curve behaves in between. Asking for a
    /// frame past the end returns the end rectangle.
    #[must_use]
    pub fn frame(&self, idx: usize) -> Rect {
        let last = self.frame_count() - 1;
        if idx >= last {
            return self.end;
        }
        let progress = idx as f64 / last as f64;
        self.start.lerp(&self.end, self.style.evaluate(progress))
    }

    /// Every frame, from the start rectangle to the end rectangle.
    #[must_use]
    pub fn frames(&self) -> Vec<Rect> {
        (0..self.frame_count()).map(|idx| self.frame(idx)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rect = Rect::new(0, 0, 100, 100);
    const B: Rect = Rect::new(900, 500, 1900, 1100);

    #[test]
    fn every_style_is_anchored_at_both_ends() {
        for style in AnimationStyle::ALL {
            assert!(
                style.evaluate(0.0).abs() < 1e-9,
                "{style} does not start at zero: {}",
                style.evaluate(0.0)
            );
            assert!(
                (style.evaluate(1.0) - 1.0).abs() < 1e-9,
                "{style} does not end at one: {}",
                style.evaluate(1.0)
            );
        }
    }

    #[test]
    fn every_style_is_finite_across_the_whole_range() {
        for style in AnimationStyle::ALL {
            for step in 0..=100 {
                let value = style.evaluate(f64::from(step) / 100.0);
                assert!(value.is_finite(), "{style} produced {value}");
                assert!(value > -1.0 && value < 2.0, "{style} produced {value}");
            }
        }
    }

    #[test]
    fn progress_outside_the_range_is_clamped() {
        for style in AnimationStyle::ALL {
            assert_eq!(style.evaluate(-5.0), style.evaluate(0.0), "{style}");
            assert_eq!(style.evaluate(5.0), style.evaluate(1.0), "{style}");
        }
    }

    #[test]
    fn the_monotonic_styles_never_go_backwards() {
        let monotonic = [
            AnimationStyle::Linear,
            AnimationStyle::EaseInSine,
            AnimationStyle::EaseOutSine,
            AnimationStyle::EaseInOutSine,
            AnimationStyle::EaseInQuad,
            AnimationStyle::EaseOutQuad,
            AnimationStyle::EaseInOutQuad,
            AnimationStyle::EaseInCubic,
            AnimationStyle::EaseOutCubic,
            AnimationStyle::EaseInOutCubic,
            AnimationStyle::EaseInQuart,
            AnimationStyle::EaseOutQuart,
            AnimationStyle::EaseInQuint,
            AnimationStyle::EaseOutQuint,
            AnimationStyle::EaseInCirc,
            AnimationStyle::EaseOutCirc,
            AnimationStyle::EaseInOutCirc,
        ];
        for style in monotonic {
            let mut previous = f64::NEG_INFINITY;
            for step in 0..=200 {
                let value = style.evaluate(f64::from(step) / 200.0);
                assert!(value >= previous - 1e-9, "{style} dipped at {step}");
                previous = value;
            }
        }
    }

    #[test]
    fn linear_is_the_identity() {
        assert!((AnimationStyle::Linear.evaluate(0.25) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn ease_out_quad_starts_fast() {
        let style = AnimationStyle::EaseOutQuad;
        assert!(
            style.evaluate(0.5) > 0.5,
            "half way through, more than half done"
        );
        assert!((style.evaluate(0.5) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn ease_in_quad_starts_slow() {
        assert!((AnimationStyle::EaseInQuad.evaluate(0.5) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn the_back_styles_overshoot() {
        assert!(AnimationStyle::EaseInBack.evaluate(0.3) < 0.0);
        assert!(AnimationStyle::EaseOutBack.evaluate(0.7) > 1.0);
    }

    #[test]
    fn frame_counts_follow_duration_and_fps() {
        assert_eq!(
            Animation::new(A, B, 250, 60, AnimationStyle::Linear).frame_count(),
            16
        );
        assert_eq!(
            Animation::new(A, B, 1000, 60, AnimationStyle::Linear).frame_count(),
            61
        );
        assert_eq!(
            Animation::new(A, B, 16, 60, AnimationStyle::Linear).frame_count(),
            2
        );
        assert_eq!(
            Animation::new(A, B, 0, 0, AnimationStyle::Linear).frame_count(),
            2,
            "a zero duration still has a start and an end"
        );
    }

    #[test]
    fn the_first_and_last_frames_are_exact() {
        for style in AnimationStyle::ALL {
            let animation = Animation::new(A, B, 250, 60, style);
            assert_eq!(animation.frame(0), A, "{style}");
            assert_eq!(animation.frame(animation.frame_count() - 1), B, "{style}");
            assert_eq!(animation.frame(9_999), B, "{style} past the end");
        }
    }

    #[test]
    fn frames_walk_from_start_to_end() {
        let animation = Animation::new(A, B, 250, 60, AnimationStyle::Linear);
        let frames = animation.frames();
        assert_eq!(frames.len(), animation.frame_count());
        assert_eq!(frames[0], A);
        assert_eq!(frames[frames.len() - 1], B);
        for pair in frames.windows(2) {
            assert!(pair[1].left >= pair[0].left, "linear never goes backwards");
        }
    }

    #[test]
    fn a_linear_animation_is_half_way_in_the_middle() {
        let animation = Animation::new(
            Rect::new(0, 0, 100, 100),
            Rect::new(100, 100, 200, 200),
            1000,
            2,
            AnimationStyle::Linear,
        );
        assert_eq!(animation.frame_count(), 3);
        assert_eq!(animation.frame(1), Rect::new(50, 50, 150, 150));
    }

    #[test]
    fn accessors_report_what_was_configured() {
        let animation = Animation::new(A, B, 250, 60, AnimationStyle::EaseOutQuad);
        assert_eq!(animation.start(), A);
        assert_eq!(animation.end(), B);
        assert_eq!(animation.style(), AnimationStyle::EaseOutQuad);
        assert_eq!(animation.duration(), Duration::from_millis(250));
        assert_eq!(animation.frame_interval(), Duration::from_nanos(16_666_666));
    }

    #[test]
    fn an_animation_that_does_not_move_stays_put() {
        let animation = Animation::new(A, A, 250, 60, AnimationStyle::EaseInOutBack);
        assert!(animation.frames().iter().all(|r| *r == A));
    }

    #[test]
    fn styles_round_trip_through_json_with_the_documented_names() {
        assert_eq!(
            serde_json::to_string(&AnimationStyle::EaseOutQuad).unwrap(),
            "\"EaseOutQuad\""
        );
        assert_eq!(
            serde_json::from_str::<AnimationStyle>("\"EaseOutQuad\"").unwrap(),
            AnimationStyle::EaseOutQuad
        );
        assert_eq!(AnimationStyle::default(), AnimationStyle::Linear);
    }

    #[test]
    fn styles_parse_from_cli_words() {
        use std::str::FromStr;
        assert_eq!(
            AnimationStyle::from_str("ease-out-quad"),
            Ok(AnimationStyle::EaseOutQuad)
        );
        assert_eq!(
            AnimationStyle::from_str("EaseInOutBounce"),
            Ok(AnimationStyle::EaseInOutBounce)
        );
        assert!(AnimationStyle::from_str("wobble").is_err());
        assert_eq!(AnimationStyle::EaseOutQuad.to_string(), "EaseOutQuad");
    }
}
