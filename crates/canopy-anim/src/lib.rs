//! `canopy-anim` — a tiny, host-driven tween/clock engine wired into Canopy's signals.
//!
//! # Why this exists, and why it looks the way it does
//!
//! Canopy reactivity is pull-based and *event*-driven: a [`Signal`] changes, a bound
//! effect re-runs on [`Runtime::flush`], and exactly one targeted op (e.g. a `SetText`)
//! is emitted. There is no notion of "time" anywhere in that model — and on purpose,
//! because Canopy targets everything from a desktop host down to a bare-metal target,
//! and a no_std core cannot assume an ambient clock (`std::time::Instant`), a timer
//! thread, or `requestAnimationFrame`. **Time is the host's job.**
//!
//! An animation, though, is precisely "a value that changes as a function of time".
//! This crate bridges the two without breaking the model: it lets a value *animate*
//! while staying an ordinary [`Signal`], so everything downstream (bound text, memos,
//! the op-stream) is unchanged. The trick is to make time explicit:
//!
//! 1. The host owns a [`Timeline`]. Each frame — driven by whatever the host *does*
//!    have for time (`Instant` deltas on desktop, a vsync callback, an RTC tick on an
//!    MCU) — it calls [`Timeline::tick`] with the elapsed seconds `dt`.
//! 2. `tick` advances every active [`Tween`]'s elapsed time, computes its eased value,
//!    and **`set`s** that value into the tween's backing [`Signal`]. Because it goes
//!    through `set`, the change participates in the normal flush model: the host then
//!    calls [`Runtime::flush`] and the bound effect re-runs, emitting one op.
//! 3. `tick` returns whether anything is still animating, so a host can stop redrawing
//!    when the timeline goes idle (no busy-loop when nothing moves).
//!
//! This is the same shape as a browser's `requestAnimationFrame`: the platform drives
//! the clock, the engine advances state, the framework re-renders. We just keep the
//! "platform" outside the no_std seam.
//!
//! # Usage
//!
//! ```
//! use canopy_anim::{animate, Easing, Timeline};
//! use canopy_signals::Runtime;
//!
//! let rt = Runtime::new();
//! let mut timeline = Timeline::new();
//!
//! // A value that goes 0 → 100 over 0.25s, decelerating.
//! let x = timeline.animate(&rt, 0.0, 100.0, 0.25, Easing::EaseOutCubic);
//!
//! // The host's frame loop (here, one 16ms-ish frame):
//! let still_animating = timeline.tick(0.016);
//! rt.flush(); // re-runs any effect bound to `x`
//! assert!(still_animating);
//! # let _ = x;
//! ```
//!
//! The free [`animate`] function is the same constructor against the default-style API
//! when you already have a `&mut Timeline` in hand via the method; both exist so calling
//! code reads naturally (`timeline.animate(..)` inside a host, `animate(&mut tl, ..)`
//! when threading the timeline explicitly).
//!
//! # Honest boundary (read this before reaching for it)
//!
//! Today this drives a `Signal<f32>` — which is *exactly* what reactive **text** wants:
//! bind a label to a closure that reads the signal (`App::bind_text` / a `label_bound`
//! in `canopy-ui`) and the number animates on screen, one `SetText` per frame. It is
//! also directly usable in host-side logic (scroll offsets, hit-test thresholds,
//! anything the host computes from a value).
//!
//! What it does **not** yet do is animate a node's *style* (an opacity, a translate, a
//! color). That is not a limitation of this crate — it is that `canopy-view` has no
//! reactive *style* binding yet (only `bind_text`). The clean next step is a
//! `bind_style`/`bind_attr` in `canopy-view` that, like `bind_text`, re-runs on flush
//! and emits one targeted style op; an animated `Signal<f32>` from this crate then plugs
//! straight into it with no change here. That seam is intentionally left to the view
//! crate so this stays pure math + signals.
//!
//! # `no_std`
//!
//! Pure `core`/`alloc` math plus `canopy-signals`. No I/O, no ambient time, no float
//! library: every easing is a polynomial (see [`easing`]). The crate builds for a
//! bare-metal target unchanged.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

use canopy_signals::{Runtime, Signal};

mod easing;

pub use easing::Easing;

/// One in-flight interpolation: it owns its parameters and the [`Signal`] it writes to.
///
/// A tween is created by [`Timeline::animate`] (or the free [`animate`]) and lives
/// inside the [`Timeline`] until it completes. Its public surface is just the signal it
/// drives — the caller reads *that*, not the tween — so the struct itself is an internal
/// bookkeeping record, exposed only enough to be inspectable in tests and docs.
struct Tween {
    /// The animated output. `tick` `set`s the eased value here every frame; everything
    /// downstream sees an ordinary signal.
    signal: Signal<f32>,
    /// Start value at `t = 0`.
    from: f32,
    /// End value at `t = 1` (the value the signal holds once the tween completes).
    to: f32,
    /// Seconds elapsed so far, advanced by `dt` each [`Timeline::tick`]. Clamped at
    /// `duration` on completion so the final `set` lands exactly on `to`.
    elapsed: f32,
    /// Total duration in seconds. A non-positive duration is treated as "instant": the
    /// tween completes on its first tick with the signal at `to`.
    duration: f32,
    /// The curve mapping normalized progress to eased fraction. Stored by value (it is
    /// `Copy`) — no allocation, no dispatch on the per-frame path.
    easing: Easing,
}

impl Tween {
    /// Advance this tween by `dt` seconds, write the eased value into its signal, and
    /// report whether it is now complete (`true` = finished this tick or earlier).
    ///
    /// The eased value is always written — including on the completing tick, where the
    /// value is pinned exactly to `to` (rather than `from + (to-from)*easing(1.0)`) so
    /// floating-point drift in the curve can never leave the signal a hair off its
    /// target. A completed tween is removed by the [`Timeline`]; calling this again
    /// would simply keep reporting `to`, but the timeline never does.
    fn advance(&mut self, dt: f32) -> bool {
        self.elapsed += dt;

        // Instant (or already-overrun) tweens snap to the end. Guarding `duration <= 0`
        // also avoids a divide-by-zero in the progress fraction below.
        if self.duration <= 0.0 || self.elapsed >= self.duration {
            self.elapsed = self.duration.max(0.0);
            self.signal.set(self.to);
            return true;
        }

        let t = self.elapsed / self.duration; // in (0,1) here, by the guard above.
        let eased = self.easing.apply(t);
        self.signal.set(self.from + (self.to - self.from) * eased);
        false
    }
}

/// The host-owned clock that drives every active animation.
///
/// A `Timeline` holds the set of running [`Tween`]s. It does not own a `Runtime` (a
/// signal already carries its runtime), and it has no concept of wall-clock time: the
/// host advances it with [`tick`](Timeline::tick), passing the real elapsed seconds it
/// measured however it measures time. That keeps the whole crate `no_std` and free of
/// any ambient clock — exactly one explicit `dt` flows in per frame.
///
/// Typical lifecycle, per frame, on the host:
///
/// ```text
/// let dt = now - last;                 // host's own time source
/// let busy = timeline.tick(dt);        // advance + write signals
/// rt.flush();                          // re-run bound effects -> emit ops
/// if !busy { /* stop the redraw loop until the next input */ }
/// ```
#[derive(Default)]
pub struct Timeline {
    /// Active tweens. Completed ones are removed in-place during [`tick`](Timeline::tick)
    /// so the vector shrinks to empty when the timeline goes idle (and reports
    /// [`is_idle`](Timeline::is_idle)).
    tweens: Vec<Tween>,
}

impl Timeline {
    /// Create an empty timeline. It is idle until the first [`animate`](Timeline::animate).
    #[must_use]
    pub fn new() -> Self {
        Timeline { tweens: Vec::new() }
    }

    /// Start a tween from `from` to `to` over `duration` seconds under `easing`, and
    /// return the [`Signal`] it drives.
    ///
    /// The signal is minted on `rt` and starts holding `from`, so a bound effect reads a
    /// sensible value *before* the first tick. Each [`tick`](Timeline::tick) then `set`s
    /// the eased value, and the signal lands exactly on `to` when the tween completes.
    ///
    /// `duration <= 0.0` is allowed and means "snap to `to` on the next tick" (an
    /// instant transition); it is occasionally handy to disable an animation without a
    /// separate code path.
    ///
    /// The returned `Signal<f32>` is an ordinary signal — clone it, read it in effects
    /// and memos, bind text to it. The timeline retains its own clone to write into.
    pub fn animate(
        &mut self,
        rt: &Runtime,
        from: f32,
        to: f32,
        duration: f32,
        easing: Easing,
    ) -> Signal<f32> {
        let signal = rt.signal(from);
        self.tweens.push(Tween {
            signal: signal.clone(),
            from,
            to,
            elapsed: 0.0,
            duration,
            easing,
        });
        signal
    }

    /// Advance every active tween by `dt` seconds and write their new values into their
    /// signals; return `true` if anything is still animating after this tick.
    ///
    /// This is the single per-frame entry point. `dt` is *elapsed seconds since the last
    /// tick* — the host supplies it from its own time source; the crate measures nothing
    /// itself. A negative `dt` is clamped to `0.0` (a clock that ran backwards must not
    /// rewind an animation), so `tick` is robust to a host's time glitches.
    ///
    /// Writing happens via [`Signal::set`], so it only marks subscribers dirty — call
    /// [`Runtime::flush`] afterward to actually re-run bound effects and emit ops. The
    /// returned bool is the cue to keep (or stop) the redraw loop: `false` means the
    /// timeline is idle and nothing will change until a new [`animate`](Timeline::animate).
    pub fn tick(&mut self, dt: f32) -> bool {
        let dt = if dt > 0.0 { dt } else { 0.0 };

        // Advance each tween, retaining only the ones still running. `retain_mut` keeps
        // this a single pass with no extra allocation: a tween that reports complete is
        // dropped from the vector here, after its final `set(to)` has been written.
        self.tweens.retain_mut(|tween| !tween.advance(dt));

        !self.tweens.is_empty()
    }

    /// `true` when no tween is active — nothing will change on the next
    /// [`tick`](Timeline::tick) until something is animated. The inverse of the value the
    /// last `tick` returned; exposed so a host can query idleness without ticking.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.tweens.is_empty()
    }

    /// Number of tweens currently running. Mostly for tests and host diagnostics.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.tweens.len()
    }
}

/// Free-function form of [`Timeline::animate`], for call sites that thread a
/// `&mut Timeline` explicitly rather than calling the method on a field.
///
/// Identical behavior to the method — it exists only so that code which holds the
/// timeline as a separate value reads as `animate(&mut tl, &rt, 0.0, 1.0, ..)`, matching
/// the prose in the crate docs. See [`Timeline::animate`] for the full contract.
pub fn animate(
    timeline: &mut Timeline,
    rt: &Runtime,
    from: f32,
    to: f32,
    duration: f32,
    easing: Easing,
) -> Signal<f32> {
    timeline.animate(rt, from, to, duration, easing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use core::cell::Cell;

    /// Assert `a ≈ b` within a small epsilon. Animations are f32 polynomial math, so we
    /// compare with tolerance rather than `==`.
    fn approx(a: f32, b: f32) {
        const EPS: f32 = 1e-4;
        assert!((a - b).abs() < EPS, "expected ~{b}, got {a}");
    }

    #[test]
    fn linear_tween_hits_midpoint_then_endpoint_and_goes_idle() {
        let rt = Runtime::new();
        let mut tl = Timeline::new();

        // 0 → 10 over 1.0s, linear. Starts at `from` before any tick.
        let v = tl.animate(&rt, 0.0, 10.0, 1.0, Easing::Linear);
        approx(v.get(), 0.0);
        assert!(!tl.is_idle(), "an active tween means the timeline is busy");

        // Halfway through: linear ⇒ ~5.0. Still animating.
        let busy = tl.tick(0.5);
        rt.flush();
        assert!(busy, "still mid-flight at t=0.5");
        approx(v.get(), 5.0);

        // The remaining 0.5s lands exactly on `to` and the timeline reports idle.
        let busy = tl.tick(0.5);
        rt.flush();
        approx(v.get(), 10.0);
        assert!(!busy, "tween completed ⇒ tick reports idle");
        assert!(tl.is_idle());
        assert_eq!(tl.active_count(), 0, "completed tween was dropped");
    }

    #[test]
    fn ease_in_out_curve_endpoints_and_midpoint() {
        // EaseInOutCubic is symmetric: f(0)=0, f(0.5)=0.5, f(1)=1. Check the value the
        // tween produces at each (over a 0 → 1 range so value == eased fraction).
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 0.0, 1.0, 1.0, Easing::EaseInOutCubic);

        // Endpoint check at t=0 (pre-tick) is `from`.
        approx(v.get(), 0.0);

        // Midpoint: a symmetric in-out curve crosses exactly 0.5 at its center.
        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 0.5);

        // Endpoint: lands exactly on 1.0.
        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 1.0);
        assert!(tl.is_idle());
    }

    #[test]
    fn ease_in_out_is_slow_at_the_start() {
        // Sanity that the curve is actually eased and not linear: at t=0.25 an
        // ease-in-out cubic is well below the linear 0.25.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 0.0, 1.0, 1.0, Easing::EaseInOutCubic);
        tl.tick(0.25);
        rt.flush();
        let eased = v.get();
        assert!(
            eased < 0.25,
            "ease-in should lag linear early on; got {eased}"
        );
        assert!(eased > 0.0);
    }

    #[test]
    fn bound_effect_reruns_across_ticks_with_one_observation_per_flush() {
        // The whole point: a closure reading the tween signal re-runs as the value
        // animates, exactly like a `bind_text` effect would.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 0.0, 10.0, 1.0, Easing::Linear);

        let observed = Rc::new(Cell::new(-1.0f32));
        let runs = Rc::new(Cell::new(0u32));
        {
            let v = v.clone();
            let observed = observed.clone();
            let runs = runs.clone();
            rt.create_effect(move || {
                observed.set(v.get());
                runs.set(runs.get() + 1);
            });
        }
        // Effect ran once on registration, seeing the start value.
        assert_eq!(runs.get(), 1);
        approx(observed.get(), 0.0);

        // Each ticked frame + flush re-runs the bound effect with the new value.
        tl.tick(0.5);
        rt.flush();
        assert_eq!(runs.get(), 2, "tick+flush re-ran the bound effect");
        approx(observed.get(), 5.0);

        tl.tick(0.5);
        rt.flush();
        assert_eq!(runs.get(), 3);
        approx(observed.get(), 10.0);

        // Timeline is idle now; a further tick changes nothing and does not re-run the
        // effect (no signal was written, so nothing is dirtied).
        let busy = tl.tick(1.0);
        rt.flush();
        assert!(!busy);
        assert_eq!(runs.get(), 3, "idle timeline does not re-run effects");
    }

    #[test]
    fn negative_dt_does_not_rewind() {
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 0.0, 10.0, 1.0, Easing::Linear);

        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 5.0);

        // A backwards clock tick is clamped to 0 — the animation holds, never rewinds.
        tl.tick(-0.4);
        rt.flush();
        approx(v.get(), 5.0);
    }

    #[test]
    fn zero_duration_snaps_to_target_on_first_tick() {
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 2.0, 9.0, 0.0, Easing::Linear);
        approx(v.get(), 2.0); // starts at `from`

        let busy = tl.tick(0.016);
        rt.flush();
        approx(v.get(), 9.0); // snapped to `to`
        assert!(!busy, "instant tween completes immediately");
        assert!(tl.is_idle());
    }

    #[test]
    fn overshooting_tick_clamps_to_target() {
        // A dt larger than the remaining duration must not overshoot `to`.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = tl.animate(&rt, 0.0, 100.0, 0.1, Easing::Linear);

        let busy = tl.tick(10.0); // way past the end
        rt.flush();
        approx(v.get(), 100.0);
        assert!(!busy);
    }

    #[test]
    fn multiple_tweens_complete_independently() {
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let fast = tl.animate(&rt, 0.0, 1.0, 0.5, Easing::Linear);
        let slow = tl.animate(&rt, 0.0, 1.0, 2.0, Easing::Linear);
        assert_eq!(tl.active_count(), 2);

        // After 0.5s the fast one is done; the slow one is a quarter through.
        let busy = tl.tick(0.5);
        rt.flush();
        assert!(busy, "the slow tween is still running");
        approx(fast.get(), 1.0);
        approx(slow.get(), 0.25);
        assert_eq!(tl.active_count(), 1, "fast tween was dropped, slow remains");
    }

    #[test]
    fn free_function_matches_method() {
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = animate(&mut tl, &rt, 0.0, 4.0, 1.0, Easing::Linear);
        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 2.0);
    }

    #[test]
    fn easing_endpoints_are_exact() {
        // Every curve must pin f(0)=0 and f(1)=1 so a tween lands on `from`/`to`.
        for e in [
            Easing::Linear,
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::EaseInCubic,
            Easing::EaseOutCubic,
            Easing::EaseInOutCubic,
            Easing::Smoothstep,
            Easing::Spring,
        ] {
            approx(e.apply(0.0), 0.0);
            approx(e.apply(1.0), 1.0);
        }
    }

    #[test]
    fn spring_overshoots_before_settling() {
        // The spring curve is meant to pass above 1.0 near the end (the bouncy arrival)
        // and then settle exactly to 1.0.
        let peak = Easing::Spring.apply(0.8);
        assert!(peak > 1.0, "spring should overshoot mid-flight; got {peak}");
        approx(Easing::Spring.apply(1.0), 1.0);
    }
}
