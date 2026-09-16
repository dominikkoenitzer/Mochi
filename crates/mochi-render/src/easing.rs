//! Easing curves for the animation driver.
//!
//! The names and the maths are the standard ones from the penner family, so a
//! config that says `"EaseOutQuad"` means what every other window manager means
//! by it. [`Easing::apply`] maps a progress `t` in `0.0..=1.0` to an eased
//! progress; the back and elastic curves deliberately leave that range in the
//! middle, which is what gives them their overshoot, so callers must not clamp
//! the interpolation factor.
//!
//! The lead will fold this module into `mochi-core` once the daemon needs the
//! same curves; it is deliberately one self-contained file.

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

/// A named easing curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Easing {
    /// No easing at all, constant speed.
    #[default]
    Linear,
    /// Sine, slow start.
    EaseInSine,
    /// Sine, slow stop.
    EaseOutSine,
    /// Sine, slow at both ends.
    EaseInOutSine,
    /// Quadratic, slow start.
    EaseInQuad,
    /// Quadratic, slow stop. The style the rice uses.
    EaseOutQuad,
    /// Quadratic, slow at both ends.
    EaseInOutQuad,
    /// Cubic, slow start.
    EaseInCubic,
    /// Cubic, slow stop.
    EaseOutCubic,
    /// Cubic, slow at both ends.
    EaseInOutCubic,
    /// Quartic, slow start.
    EaseInQuart,
    /// Quartic, slow stop.
    EaseOutQuart,
    /// Quartic, slow at both ends.
    EaseInOutQuart,
    /// Quintic, slow start.
    EaseInQuint,
    /// Quintic, slow stop.
    EaseOutQuint,
    /// Quintic, slow at both ends.
    EaseInOutQuint,
    /// Exponential, very slow start.
    EaseInExpo,
    /// Exponential, very slow stop.
    EaseOutExpo,
    /// Exponential, very slow at both ends.
    EaseInOutExpo,
    /// Circular, slow start.
    EaseInCirc,
    /// Circular, slow stop.
    EaseOutCirc,
    /// Circular, slow at both ends.
    EaseInOutCirc,
    /// Pulls back before moving.
    EaseInBack,
    /// Overshoots and settles.
    EaseOutBack,
    /// Pulls back, then overshoots.
    EaseInOutBack,
    /// Winds up like a spring.
    EaseInElastic,
    /// Springs past the target and wobbles in.
    EaseOutElastic,
    /// Springs at both ends.
    EaseInOutElastic,
    /// Bounces into the start.
    EaseInBounce,
    /// Bounces onto the target.
    EaseOutBounce,
    /// Bounces at both ends.
    EaseInOutBounce,
}

impl Easing {
    /// Every curve, in declaration order. Handy for CLI completion and for the
    /// property tests below.
    pub const ALL: [Easing; 31] = [
        Easing::Linear,
        Easing::EaseInSine,
        Easing::EaseOutSine,
        Easing::EaseInOutSine,
        Easing::EaseInQuad,
        Easing::EaseOutQuad,
        Easing::EaseInOutQuad,
        Easing::EaseInCubic,
        Easing::EaseOutCubic,
        Easing::EaseInOutCubic,
        Easing::EaseInQuart,
        Easing::EaseOutQuart,
        Easing::EaseInOutQuart,
        Easing::EaseInQuint,
        Easing::EaseOutQuint,
        Easing::EaseInOutQuint,
        Easing::EaseInExpo,
        Easing::EaseOutExpo,
        Easing::EaseInOutExpo,
        Easing::EaseInCirc,
        Easing::EaseOutCirc,
        Easing::EaseInOutCirc,
        Easing::EaseInBack,
        Easing::EaseOutBack,
        Easing::EaseInOutBack,
        Easing::EaseInElastic,
        Easing::EaseOutElastic,
        Easing::EaseInOutElastic,
        Easing::EaseInBounce,
        Easing::EaseOutBounce,
        Easing::EaseInOutBounce,
    ];

    /// `true` for the curves that leave `0.0..=1.0` on the way, so a caller that
    /// wants to clamp knows which ones it would ruin.
    #[must_use]
    pub const fn overshoots(self) -> bool {
        matches!(
            self,
            Easing::EaseInBack
                | Easing::EaseOutBack
                | Easing::EaseInOutBack
                | Easing::EaseInElastic
                | Easing::EaseOutElastic
                | Easing::EaseInOutElastic
        )
    }

    /// Maps linear progress to eased progress.
    ///
    /// `t` is clamped to `0.0..=1.0` on the way in; the result is not clamped.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn apply(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,

            Easing::EaseInSine => 1.0 - ((t * PI) / 2.0).cos(),
            Easing::EaseOutSine => ((t * PI) / 2.0).sin(),
            Easing::EaseInOutSine => -((PI * t).cos() - 1.0) / 2.0,

            Easing::EaseInQuad => t * t,
            Easing::EaseOutQuad => 1.0 - (1.0 - t).powi(2),
            Easing::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (2.0 - 2.0 * t).powi(2) / 2.0
                }
            }

            Easing::EaseInCubic => t * t * t,
            Easing::EaseOutCubic => 1.0 - (1.0 - t).powi(3),
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (2.0 - 2.0 * t).powi(3) / 2.0
                }
            }

            Easing::EaseInQuart => t.powi(4),
            Easing::EaseOutQuart => 1.0 - (1.0 - t).powi(4),
            Easing::EaseInOutQuart => {
                if t < 0.5 {
                    8.0 * t.powi(4)
                } else {
                    1.0 - (2.0 - 2.0 * t).powi(4) / 2.0
                }
            }

            Easing::EaseInQuint => t.powi(5),
            Easing::EaseOutQuint => 1.0 - (1.0 - t).powi(5),
            Easing::EaseInOutQuint => {
                if t < 0.5 {
                    16.0 * t.powi(5)
                } else {
                    1.0 - (2.0 - 2.0 * t).powi(5) / 2.0
                }
            }

            Easing::EaseInExpo => {
                if t <= 0.0 {
                    0.0
                } else {
                    2.0f64.powf(10.0 * t - 10.0)
                }
            }
            Easing::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2.0f64.powf(-10.0 * t)
                }
            }
            Easing::EaseInOutExpo => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else if t < 0.5 {
                    2.0f64.powf(20.0 * t - 10.0) / 2.0
                } else {
                    (2.0 - 2.0f64.powf(10.0 - 20.0 * t)) / 2.0
                }
            }

            Easing::EaseInCirc => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
            Easing::EaseOutCirc => (1.0 - (t - 1.0).powi(2)).max(0.0).sqrt(),
            Easing::EaseInOutCirc => {
                if t < 0.5 {
                    (1.0 - (1.0 - (2.0 * t).powi(2)).max(0.0).sqrt()) / 2.0
                } else {
                    ((1.0 - (2.0 - 2.0 * t).powi(2)).max(0.0).sqrt() + 1.0) / 2.0
                }
            }

            Easing::EaseInBack => C3 * t * t * t - C1 * t * t,
            Easing::EaseOutBack => {
                let u = t - 1.0;
                1.0 + C3 * u * u * u + C1 * u * u
            }
            Easing::EaseInOutBack => {
                if t < 0.5 {
                    let u = 2.0 * t;
                    (u * u * ((C2 + 1.0) * u - C2)) / 2.0
                } else {
                    let u = 2.0 * t - 2.0;
                    (u * u * ((C2 + 1.0) * u + C2) + 2.0) / 2.0
                }
            }

            Easing::EaseInElastic => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else {
                    -(2.0f64.powf(10.0 * t - 10.0)) * ((10.0 * t - 10.75) * C4).sin()
                }
            }
            Easing::EaseOutElastic => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else {
                    2.0f64.powf(-10.0 * t) * ((10.0 * t - 0.75) * C4).sin() + 1.0
                }
            }
            Easing::EaseInOutElastic => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else if t < 0.5 {
                    -(2.0f64.powf(20.0 * t - 10.0) * ((20.0 * t - 11.125) * C5).sin()) / 2.0
                } else {
                    (2.0f64.powf(10.0 - 20.0 * t) * ((20.0 * t - 11.125) * C5).sin()) / 2.0 + 1.0
                }
            }

            Easing::EaseInBounce => 1.0 - bounce_out(1.0 - t),
            Easing::EaseOutBounce => bounce_out(t),
            Easing::EaseInOutBounce => {
                if t < 0.5 {
                    (1.0 - bounce_out(1.0 - 2.0 * t)) / 2.0
                } else {
                    (1.0 + bounce_out(2.0 * t - 1.0)) / 2.0
                }
            }
        }
    }
}

/// Back overshoot constant: about 10% past the target.
const C1: f64 = 1.701_58;
/// Back overshoot constant for the in-out variant.
const C2: f64 = C1 * 1.525;
/// Back overshoot constant, cubed term.
const C3: f64 = C1 + 1.0;
/// Elastic period for the single-sided curves.
const C4: f64 = (2.0 * PI) / 3.0;
/// Elastic period for the in-out curve.
const C5: f64 = (2.0 * PI) / 4.5;

/// The curve every bounce easing is built from.
fn bounce_out(t: f64) -> f64 {
    const N1: f64 = 7.5625;
    const D1: f64 = 2.75;

    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let u = t - 1.5 / D1;
        N1 * u * u + 0.75
    } else if t < 2.5 / D1 {
        let u = t - 2.25 / D1;
        N1 * u * u + 0.9375
    } else {
        let u = t - 2.625 / D1;
        N1 * u * u + 0.984_375
    }
}

impl std::fmt::Display for Easing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::str::FromStr for Easing {
    type Err = crate::RenderError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Easing::ALL
            .into_iter()
            .find(|candidate| format!("{candidate:?}").eq_ignore_ascii_case(s))
            .ok_or_else(|| crate::RenderError::Easing(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn every_curve_starts_at_zero_and_ends_at_one() {
        for easing in Easing::ALL {
            assert!(
                easing.apply(0.0).abs() < 1e-6,
                "{easing:?} does not start at 0: {}",
                easing.apply(0.0)
            );
            assert!(
                (easing.apply(1.0) - 1.0).abs() < 1e-6,
                "{easing:?} does not end at 1: {}",
                easing.apply(1.0)
            );
        }
    }

    #[test]
    fn progress_is_clamped_on_the_way_in() {
        for easing in Easing::ALL {
            assert!(
                (easing.apply(-5.0) - easing.apply(0.0)).abs() < EPS,
                "{easing:?}"
            );
            assert!(
                (easing.apply(5.0) - easing.apply(1.0)).abs() < EPS,
                "{easing:?}"
            );
        }
    }

    #[test]
    fn no_curve_returns_a_non_finite_number() {
        for easing in Easing::ALL {
            for step in 0..=100 {
                let t = f64::from(step) / 100.0;
                assert!(easing.apply(t).is_finite(), "{easing:?} at {t}");
            }
        }
    }

    #[test]
    fn non_overshooting_curves_stay_in_range() {
        for easing in Easing::ALL.into_iter().filter(|e| !e.overshoots()) {
            for step in 0..=100 {
                let t = f64::from(step) / 100.0;
                let v = easing.apply(t);
                assert!(
                    (-1e-9..=1.0 + 1e-9).contains(&v),
                    "{easing:?} at {t} gave {v}"
                );
            }
        }
    }

    #[test]
    fn overshooting_curves_really_overshoot() {
        for easing in Easing::ALL.into_iter().filter(|e| e.overshoots()) {
            let out_of_range = (0..=100)
                .map(|step| easing.apply(f64::from(step) / 100.0))
                .any(|v| !(-1e-9..=1.0 + 1e-9).contains(&v));
            assert!(out_of_range, "{easing:?} never leaves 0..=1");
        }
    }

    #[test]
    fn ease_out_quad_matches_the_known_curve() {
        // The style the rice uses: fast out of the gate, gentle arrival.
        assert!((Easing::EaseOutQuad.apply(0.25) - 0.437_5).abs() < EPS);
        assert!((Easing::EaseOutQuad.apply(0.5) - 0.75).abs() < EPS);
        assert!((Easing::EaseOutQuad.apply(0.75) - 0.937_5).abs() < EPS);
        assert!(Easing::EaseOutQuad.apply(0.5) > Easing::Linear.apply(0.5));
    }

    #[test]
    fn ease_in_curves_lag_behind_linear() {
        for easing in [
            Easing::EaseInSine,
            Easing::EaseInQuad,
            Easing::EaseInCubic,
            Easing::EaseInQuart,
            Easing::EaseInQuint,
            Easing::EaseInExpo,
            Easing::EaseInCirc,
        ] {
            assert!(easing.apply(0.5) < 0.5, "{easing:?}");
        }
    }

    #[test]
    fn ease_out_curves_run_ahead_of_linear() {
        for easing in [
            Easing::EaseOutSine,
            Easing::EaseOutQuad,
            Easing::EaseOutCubic,
            Easing::EaseOutQuart,
            Easing::EaseOutQuint,
            Easing::EaseOutExpo,
            Easing::EaseOutCirc,
        ] {
            assert!(easing.apply(0.5) > 0.5, "{easing:?}");
        }
    }

    #[test]
    fn ease_in_out_curves_are_symmetric_around_the_middle() {
        for easing in [
            Easing::EaseInOutSine,
            Easing::EaseInOutQuad,
            Easing::EaseInOutCubic,
            Easing::EaseInOutQuart,
            Easing::EaseInOutQuint,
            Easing::EaseInOutCirc,
            Easing::EaseInOutExpo,
        ] {
            assert!((easing.apply(0.5) - 0.5).abs() < 1e-6, "{easing:?}");
            for step in 0..=50 {
                let t = f64::from(step) / 100.0;
                let left = easing.apply(t);
                let right = 1.0 - easing.apply(1.0 - t);
                assert!((left - right).abs() < 1e-6, "{easing:?} at {t}");
            }
        }
    }

    #[test]
    fn monotonic_curves_never_go_backwards() {
        let bouncy = |e: Easing| {
            matches!(
                e,
                Easing::EaseInBounce | Easing::EaseOutBounce | Easing::EaseInOutBounce
            )
        };
        for easing in Easing::ALL
            .into_iter()
            .filter(|e| !e.overshoots() && !bouncy(*e))
        {
            let mut previous = f64::NEG_INFINITY;
            for step in 0..=1000 {
                let v = easing.apply(f64::from(step) / 1000.0);
                assert!(v >= previous - 1e-9, "{easing:?} went backwards at {step}");
                previous = v;
            }
        }
    }

    #[test]
    fn parses_and_prints_config_names() {
        use std::str::FromStr;
        assert_eq!(
            Easing::from_str("EaseOutQuad").unwrap(),
            Easing::EaseOutQuad
        );
        assert_eq!(
            Easing::from_str("easeoutquad").unwrap(),
            Easing::EaseOutQuad
        );
        assert!(Easing::from_str("EaseOutSquiggle").is_err());
        assert_eq!(Easing::EaseOutQuad.to_string(), "EaseOutQuad");
        assert_eq!(
            serde_json::from_str::<Easing>("\"EaseOutQuad\"").unwrap(),
            Easing::EaseOutQuad
        );
        assert_eq!(Easing::default(), Easing::Linear);
    }
}
