//! Easing functions: pure `fn(f32) -> f32` over the *normalized* progress `t ∈ [0,1]`.
//!
//! An easing maps "how far through the duration are we" (`t`) to "how far through the
//! value range are we" (the return). Keeping them as plain `fn` pointers — not a trait
//! or a closure — is deliberate: a [`Tween`](crate::Tween) stores one `Easing` inline
//! (no allocation, no dynamic dispatch on the per-frame hot path), and authoring code
//! can name a curve by value (`Easing::EaseInOutCubic`) without constructing anything.
//!
//! All curves here are defined so that `f(0.0) == 0.0` and `f(1.0) == 1.0` (a tween
//! that starts at `from` lands exactly on `to`), and the engine only ever calls them
//! with a clamped `t`. They are written in pure `core` math — no `std`, no `libm` — so
//! the crate stays `no_std` with zero extra dependencies. That rules out transcendental
//! curves (true `sin`/`exp` springs need `libm`); the included "spring" is a polynomial
//! overshoot approximation, documented as such.

/// A normalized easing curve: given progress `t ∈ [0,1]`, return the eased fraction of
/// the value range to apply. See the [module docs](self) for the contract every variant
/// upholds (`f(0)=0`, `f(1)=1`) and why this is a value enum rather than a closure.
///
/// `Easing` is `Copy` so a [`Tween`](crate::Tween) can hold one by value and a host can
/// pass `Easing::Linear` around freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Easing {
    /// Constant rate: `f(t) = t`. The value moves at a fixed speed for the whole
    /// duration. Useful for continuous motion (a marquee, a spinner) where any
    /// acceleration would read as a stutter.
    Linear,
    /// Quadratic ease-in: `f(t) = t²`. Starts slow, accelerates. Good for an element
    /// leaving the screen (it picks up speed as it goes).
    EaseInQuad,
    /// Quadratic ease-out: `f(t) = 1 - (1-t)²`. Starts fast, decelerates into the end.
    /// Good for an element arriving (it settles).
    EaseOutQuad,
    /// Quadratic ease-in-out: accelerate then decelerate, symmetric about the midpoint.
    /// The default "feels natural" curve for most UI transitions.
    EaseInOutQuad,
    /// Cubic ease-in: `f(t) = t³`. A sharper start than [`EaseInQuad`](Self::EaseInQuad).
    EaseInCubic,
    /// Cubic ease-out: `f(t) = 1 - (1-t)³`. A sharper settle than the quad variant.
    EaseOutCubic,
    /// Cubic ease-in-out: the most common "material"-style transition curve.
    EaseInOutCubic,
    /// Hermite smoothstep: `f(t) = t²(3 - 2t)`. Like [`EaseInOutQuad`](Self::EaseInOutQuad)
    /// but with zero *first derivative* at both ends (it eases in and out with no abrupt
    /// velocity change), which is why it is the standard interpolation in graphics.
    Smoothstep,
    /// A polynomial overshoot ("back"/spring-ish) curve: eases out but overshoots `1.0`
    /// before settling, giving a springy arrival. This is **not** a physical spring
    /// (a damped-oscillator spring needs `exp`/`sin`, hence `libm`, which this `no_std`
    /// crate avoids); it is the classic `back` polynomial with a fixed overshoot. The
    /// returned value can exceed `1.0` mid-flight, so the interpolated value can briefly
    /// pass `to` — intended for that bouncy feel, not for values that must stay in range.
    Spring,
}

impl Easing {
    /// Evaluate the curve at progress `t`.
    ///
    /// The engine always passes a `t` already clamped to `[0,1]`; this function does not
    /// re-clamp (it is the hot path, called once per active tween per frame) and instead
    /// relies on that invariant. Calling it directly with `t` outside `[0,1]` is allowed
    /// but only the polynomial is evaluated — no extrapolation guarantees are made.
    #[must_use]
    pub fn apply(self, t: f32) -> f32 {
        match self {
            Easing::Linear => t,
            Easing::EaseInQuad => t * t,
            Easing::EaseOutQuad => {
                let inv = 1.0 - t;
                1.0 - inv * inv
            }
            Easing::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - (inv * inv) / 2.0
                }
            }
            Easing::EaseInCubic => t * t * t,
            Easing::EaseOutCubic => {
                let inv = 1.0 - t;
                1.0 - inv * inv * inv
            }
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let inv = -2.0 * t + 2.0;
                    1.0 - (inv * inv * inv) / 2.0
                }
            }
            Easing::Smoothstep => t * t * (3.0 - 2.0 * t),
            Easing::Spring => {
                // The "back" overshoot polynomial: f(t) = 1 + c3·(t-1)³ + c1·(t-1)²,
                // with the conventional c1 = 1.70158 (≈10% overshoot). f(0)=0, f(1)=1,
                // and f peaks above 1 near the end — the springy part. Pure polynomial,
                // so no transcendental dependency.
                const C1: f32 = 1.701_58;
                const C3: f32 = C1 + 1.0;
                let p = t - 1.0;
                1.0 + C3 * p * p * p + C1 * p * p
            }
        }
    }
}
