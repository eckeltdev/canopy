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
//! 2. `tick` advances every active tween's elapsed time, computes its eased value,
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
//! # The three knobs that make rich entrances easy
//!
//! A landing page does two things constantly: it *staggers* a set of elements in (each
//! starts a beat after the previous) and it runs *ambient* loops (a glow that breathes,
//! a shimmer that never stops). Both fall out of three per-tween parameters layered on
//! top of the basic interpolation:
//!
//! * **`delay`** — seconds to hold at `from` *before* interpolation begins. Staggering N
//!   elements is just `delay = index * step`: element 0 starts now, element 1 a beat
//!   later, and so on, all driven by the same single [`tick`](Timeline::tick).
//! * **`repeat`** — what happens at the end (see [`Repeat`]). [`Once`](Repeat::Once)
//!   completes and is dropped (the original behavior). [`Loop`](Repeat::Loop) jumps back
//!   to `from` and runs again; [`PingPong`](Repeat::PingPong) reverses and runs back. A
//!   repeating tween *never completes on its own*, so [`tick`](Timeline::tick) keeps
//!   returning `true` while one is active — that is the cue a host uses to keep an
//!   ambient redraw loop alive.
//! * **`easing`** — the curve (see [`Easing`]); unchanged from before.
//!
//! # Usage
//!
//! The original one-line form is unchanged — `Once`, no delay:
//!
//! ```
//! use canopy_anim::{Easing, Timeline};
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
//! For a staggered entrance or an ambient loop, build a [`Tween`] and `start` it. The
//! builder is additive — every knob has a sensible default ([`Once`](Repeat::Once), no
//! delay, [`Linear`](Easing::Linear)) — so you only name what you change:
//!
//! ```
//! use canopy_anim::{Easing, Repeat, Timeline, Tween};
//! use canopy_signals::Runtime;
//!
//! let rt = Runtime::new();
//! let mut timeline = Timeline::new();
//!
//! // Card #2 in a staggered list: fade-in starts 0.2s after the others.
//! let opacity = Tween::new(0.0, 1.0, 0.4)
//!     .delay(0.2)
//!     .easing(Easing::EaseOutCubic)
//!     .start(&mut timeline, &rt);
//!
//! // An ambient "breathing" glow that runs forever, back and forth.
//! let glow = Tween::new(0.6, 1.0, 1.5)
//!     .repeat(Repeat::PingPong)
//!     .easing(Easing::EaseInOutQuad)
//!     .start(&mut timeline, &rt);
//!
//! // The glow keeps the timeline busy on every frame — the host's redraw loop stays alive.
//! assert!(timeline.tick(0.016));
//! # let _ = (opacity, glow);
//! ```
//!
//! A repeating tween runs until the host stops it. The simplest, allocation-free way is
//! [`Timeline::clear`] (drop *every* running tween) — or just drop/replace the whole
//! [`Timeline`], since it owns the only retained clone that writes to each signal. The
//! signals themselves keep their last value; nothing further mutates them.
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

/// What an animation does when it reaches the end of its `duration`.
///
/// This is the single knob that turns a one-shot transition into either a finite entrance
/// or an endless ambient motion. It is `Copy` so a tween holds one inline, and it is
/// `#[non_exhaustive]` so a future mode (a fixed repeat count, say) can be added without a
/// breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Repeat {
    /// Run once `from → to`, then complete and be dropped from the [`Timeline`]. This is
    /// the original (and default) behavior: a finite transition that lets the timeline go
    /// idle so the host's redraw loop can stop.
    #[default]
    Once,
    /// On reaching `to`, jump back to `from` and run again, forever. The value
    /// discontinuously snaps back to the start each cycle — right for things that read as
    /// a *restart* (a progress sweep, a marquee). Never completes on its own, so
    /// [`tick`](Timeline::tick) keeps reporting busy until the host stops it.
    Loop,
    /// On reaching an end, reverse direction and run back, forever. The value moves
    /// `from → to → from → to …` smoothly with no snap — right for *breathing* motion (a
    /// glow that swells and fades, a float that bobs). Never completes on its own.
    PingPong,
}

/// A reusable description of an animation, *separate* from any [`Timeline`] or [`Signal`].
///
/// This is the data half of the builder: it captures every parameter ([`Tween`] is the
/// fluent surface that produces one and starts it). Holding the spec on its own is handy
/// for a host that wants to define a motion once and apply it to many elements — e.g. one
/// `AnimSpec` for "fade-and-rise" reused for every card in a stagger, with only the
/// `delay` varied per card via [`Tween::delay`].
///
/// Fields are public so a host can build one literally; [`Timeline::animate_with`] turns
/// it into a running tween and returns the driven [`Signal`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimSpec {
    /// Value at the start of a `from → to` pass.
    pub from: f32,
    /// Value at the end of a `from → to` pass (where the signal lands when a
    /// [`Once`](Repeat::Once) tween completes).
    pub to: f32,
    /// Seconds to interpolate across one pass. Non-positive means "instant" (snap to the
    /// end on the next tick); a non-positive duration with a repeating mode therefore
    /// degenerates to holding at `to`, which the engine treats as a completed `Once`.
    pub duration: f32,
    /// Seconds to hold at `from` before interpolation begins. `0.0` starts immediately.
    pub delay: f32,
    /// The curve mapping normalized progress to eased fraction.
    pub easing: Easing,
    /// End-of-pass behavior: one-shot, loop, or ping-pong.
    pub repeat: Repeat,
}

/// Fluent builder for an animation: pick the endpoints and duration, optionally add a
/// `delay`, an `easing`, and a `repeat` mode, then `start` it on a timeline.
///
/// `Tween` carries an [`AnimSpec`] and nothing else; every method is a cheap field set
/// returning `self`, so the whole thing is `Copy` and allocation-free. Defaults match the
/// original one-shot behavior — [`Repeat::Once`], `delay = 0.0`, [`Easing::Linear`] — so a
/// caller only spells out what differs from a plain linear transition.
///
/// ```
/// # use canopy_anim::{Easing, Repeat, Timeline, Tween};
/// # use canopy_signals::Runtime;
/// # let rt = Runtime::new();
/// # let mut tl = Timeline::new();
/// let s = Tween::new(0.0, 1.0, 0.5)
///     .delay(0.1)
///     .easing(Easing::EaseInOutCubic)
///     .repeat(Repeat::PingPong)
///     .start(&mut tl, &rt);
/// # let _ = s;
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    spec: AnimSpec,
}

impl Tween {
    /// Begin describing a tween from `from` to `to` over `duration` seconds.
    ///
    /// All other parameters default: no delay, [`Easing::Linear`], [`Repeat::Once`]. Chain
    /// [`delay`](Self::delay), [`easing`](Self::easing), and [`repeat`](Self::repeat) to
    /// change them, then [`start`](Self::start).
    #[must_use]
    pub fn new(from: f32, to: f32, duration: f32) -> Self {
        Tween {
            spec: AnimSpec {
                from,
                to,
                duration,
                delay: 0.0,
                easing: Easing::Linear,
                repeat: Repeat::Once,
            },
        }
    }

    /// Wrap an existing [`AnimSpec`] in the builder, e.g. to tweak one field of a reused
    /// spec before starting it.
    #[must_use]
    pub fn from_spec(spec: AnimSpec) -> Self {
        Tween { spec }
    }

    /// Hold at `from` for `secs` seconds before interpolating. Negative values are clamped
    /// to `0.0`. This is the staggering knob: element `n` passes `delay = n * step`.
    #[must_use]
    pub fn delay(mut self, secs: f32) -> Self {
        self.spec.delay = if secs > 0.0 { secs } else { 0.0 };
        self
    }

    /// Set the easing curve (default [`Easing::Linear`]).
    #[must_use]
    pub fn easing(mut self, easing: Easing) -> Self {
        self.spec.easing = easing;
        self
    }

    /// Set the end-of-pass behavior (default [`Repeat::Once`]). Choosing
    /// [`Loop`](Repeat::Loop) or [`PingPong`](Repeat::PingPong) makes the tween run until
    /// the host stops it (see [`Timeline::clear`]).
    #[must_use]
    pub fn repeat(mut self, repeat: Repeat) -> Self {
        self.spec.repeat = repeat;
        self
    }

    /// The [`AnimSpec`] built so far, for inspection or reuse.
    #[must_use]
    pub fn spec(self) -> AnimSpec {
        self.spec
    }

    /// Mint the backing signal on `rt`, register the tween on `timeline`, and return the
    /// [`Signal`] it drives. The signal starts holding `from`; see
    /// [`Timeline::animate_with`] for the full contract.
    #[must_use]
    pub fn start(self, timeline: &mut Timeline, rt: &Runtime) -> Signal<f32> {
        timeline.animate_with(rt, self.spec)
    }
}

/// One in-flight interpolation: it owns its parameters and the [`Signal`] it writes to.
///
/// An `Active` record is created by [`Timeline::animate`] / [`Timeline::animate_with`] and
/// lives inside the [`Timeline`] until it completes (or forever, if it repeats). Its public
/// surface is just the signal it drives — the caller reads *that*, not the record — so this
/// is purely internal bookkeeping.
struct Active {
    /// The animated output. `tick` `set`s the eased value here every frame; everything
    /// downstream sees an ordinary signal.
    signal: Signal<f32>,
    /// Start value at the beginning of a pass.
    from: f32,
    /// End value at the end of a pass (the value a completed [`Once`](Repeat::Once) tween
    /// holds).
    to: f32,
    /// Seconds of `delay` still remaining before interpolation begins. `tick` drains this
    /// first, holding the signal at `from`; once it hits zero, normal progress starts.
    delay_left: f32,
    /// Seconds elapsed within the current pass, advanced by `dt` each
    /// [`Timeline::tick`] once `delay_left` is exhausted. Wrapped on each loop/ping-pong
    /// cycle so it always names a position inside `[0, duration]`.
    elapsed: f32,
    /// Total duration of one pass in seconds. A non-positive duration is treated as
    /// "instant": the tween completes on its first post-delay tick with the signal at `to`.
    duration: f32,
    /// The curve mapping normalized progress to eased fraction. Stored by value (it is
    /// `Copy`) — no allocation, no dispatch on the per-frame path.
    easing: Easing,
    /// What to do at the end of a pass.
    repeat: Repeat,
    /// Direction of the current pass under [`PingPong`](Repeat::PingPong): `false` runs
    /// `from → to`, `true` runs `to → from`. Always `false` for [`Once`](Repeat::Once) and
    /// [`Loop`](Repeat::Loop), which only ever move forward.
    reversed: bool,
}

impl Active {
    /// Advance this tween by `dt` seconds, write the eased value into its signal, and
    /// report whether it is now **finished and should be dropped** (`true`).
    ///
    /// The flow per call:
    ///
    /// 1. **Delay.** If any `delay_left` remains, drain it by `dt`. While delayed the
    ///    signal is pinned to `from` (so a staggered element sits at its start pose) and
    ///    the tween reports *not finished*. Any `dt` left over after the delay ends spills
    ///    into the same call's interpolation, so a long frame never loses time.
    /// 2. **Interpolate.** Advance `elapsed`. While inside the pass, write the eased value.
    /// 3. **End of pass.** What happens depends on [`repeat`](Active::repeat):
    ///    * [`Once`](Repeat::Once): pin exactly to `to` and report finished. (`to` is set
    ///      literally rather than `from + (to-from)*easing(1)` so float drift in the curve
    ///      can never leave the signal a hair off target.)
    ///    * [`Loop`](Repeat::Loop): wrap `elapsed` back into the pass (`elapsed -=
    ///      duration`), write the value at the wrapped position, and report *not finished*
    ///      — the value snaps toward `from` and the motion continues.
    ///    * [`PingPong`](Repeat::PingPong): flip [`reversed`](Active::reversed), wrap
    ///      `elapsed`, write the value (which now runs back the other way), and report
    ///      *not finished*.
    ///
    /// A repeating tween thus never returns `true`; the [`Timeline`] keeps it (and keeps
    /// reporting busy) until the host clears it.
    fn advance(&mut self, dt: f32) -> bool {
        // --- 1. Delay. Hold at `from`, draining the delay first. ---
        let mut step = dt;
        if self.delay_left > 0.0 {
            if step < self.delay_left {
                // Still inside the delay window: hold at the start pose and stop.
                self.delay_left -= step;
                self.signal.set(self.from);
                return false;
            }
            // Delay ends this frame; the remainder spills into interpolation below.
            step -= self.delay_left;
            self.delay_left = 0.0;
        }

        // An instant (or already-overrun, non-positive-duration) tween snaps to the end.
        // This also guards the divide in the progress fraction. A repeating tween with a
        // non-positive duration has no meaningful cycle, so it degenerates to a completed
        // `Once` rather than spinning forever at zero cost-per-pass.
        if self.duration <= 0.0 {
            self.signal.set(self.to);
            return true;
        }

        // --- 2. Interpolate. ---
        self.elapsed += step;

        if self.elapsed < self.duration {
            self.signal.set(self.value_at(self.elapsed));
            return false;
        }

        // --- 3. End of pass. ---
        match self.repeat {
            Repeat::Once => {
                // Pin exactly to the pass's end value and finish.
                self.signal.set(self.end_value());
                true
            }
            Repeat::Loop => {
                // Wrap any number of whole passes back into `[0, duration)`. Using a
                // modulo-by-subtraction keeps this pure arithmetic (no `f32::rem_euclid`
                // is needed, and it stays exact for the small frame deltas in practice).
                self.elapsed = wrap(self.elapsed, self.duration);
                self.signal.set(self.value_at(self.elapsed));
                false
            }
            Repeat::PingPong => {
                // Each whole pass crossed flips direction once. Wrapping the overflow and
                // toggling `reversed` per crossed boundary keeps a big `dt` correct (e.g.
                // a 2.5× duration step lands mid-pass in the *original* direction).
                let overshoot = self.elapsed - self.duration;
                let crossings = 1 + (overshoot / self.duration) as u32;
                if crossings % 2 == 1 {
                    self.reversed = !self.reversed;
                }
                self.elapsed = wrap(self.elapsed, self.duration);
                self.signal.set(self.value_at(self.elapsed));
                false
            }
        }
    }

    /// Eased value at `elapsed` seconds into the current pass, honoring direction.
    ///
    /// Forward passes run `from → to`; a reversed (ping-pong return) pass runs `to → from`
    /// by easing the *complementary* progress. Easing is applied to time, then the result
    /// linearly blends the endpoints — so the curve's shape is preserved in both directions.
    fn value_at(&self, elapsed: f32) -> f32 {
        let t = elapsed / self.duration; // in [0,1) by the callers' guards.
        let eased = self.easing.apply(t);
        if self.reversed {
            // Returning: at `to` when the return pass starts, `from` when it ends.
            self.to + (self.from - self.to) * eased
        } else {
            self.from + (self.to - self.from) * eased
        }
    }

    /// The exact endpoint value for a completing pass: `to` going forward, `from` on a
    /// reversed pass. Set literally (not via the curve) so completion lands pixel-exact.
    fn end_value(&self) -> f32 {
        if self.reversed {
            self.from
        } else {
            self.to
        }
    }
}

/// Reduce `x` into `[0, period)` by subtracting whole `period`s.
///
/// A tiny, branch-light replacement for `f32::rem_euclid` (which is fine here too, but this
/// keeps the intent obvious and the math elementary). `period` is always `> 0.0` at every
/// call site (the non-positive-duration case is handled before any wrap). For the small
/// per-frame overshoots a host produces, the subtraction loop runs at most a handful of
/// times.
fn wrap(mut x: f32, period: f32) -> f32 {
    while x >= period {
        x -= period;
    }
    x
}

/// The host-owned clock that drives every active animation.
///
/// A `Timeline` holds the set of running tweens. It does not own a `Runtime` (a signal
/// already carries its runtime), and it has no concept of wall-clock time: the host
/// advances it with [`tick`](Timeline::tick), passing the real elapsed seconds it measured
/// however it measures time. That keeps the whole crate `no_std` and free of any ambient
/// clock — exactly one explicit `dt` flows in per frame.
///
/// Typical lifecycle, per frame, on the host:
///
/// ```text
/// let dt = now - last;                 // host's own time source
/// let busy = timeline.tick(dt);        // advance + write signals
/// rt.flush();                          // re-run bound effects -> emit ops
/// if !busy { /* stop the redraw loop until the next input */ }
/// ```
///
/// Note that a [`Loop`](Repeat::Loop) or [`PingPong`](Repeat::PingPong) tween keeps `busy`
/// `true` forever; that is intended (it keeps an ambient redraw alive). To stop such a
/// tween, call [`clear`](Timeline::clear) or drop the timeline.
#[derive(Default)]
pub struct Timeline {
    /// Active tweens. Completed [`Once`](Repeat::Once) tweens are removed in-place during
    /// [`tick`](Timeline::tick) so the vector shrinks to empty when the timeline goes idle
    /// (and reports [`is_idle`](Timeline::is_idle)). Repeating tweens stay until cleared.
    tweens: Vec<Active>,
}

impl Timeline {
    /// Create an empty timeline. It is idle until the first [`animate`](Timeline::animate).
    #[must_use]
    pub fn new() -> Self {
        Timeline { tweens: Vec::new() }
    }

    /// Start a one-shot tween from `from` to `to` over `duration` seconds under `easing`,
    /// and return the [`Signal`] it drives.
    ///
    /// This is the original, unchanged entry point — [`Repeat::Once`], no delay. For
    /// delays and repeats, use the [`Tween`] builder (or [`animate_with`](Self::animate_with)).
    ///
    /// The signal is minted on `rt` and starts holding `from`, so a bound effect reads a
    /// sensible value *before* the first tick. Each [`tick`](Timeline::tick) then `set`s
    /// the eased value, and the signal lands exactly on `to` when the tween completes.
    ///
    /// `duration <= 0.0` is allowed and means "snap to `to` on the next tick" (an instant
    /// transition); it is occasionally handy to disable an animation without a separate
    /// code path.
    ///
    /// The returned `Signal<f32>` is an ordinary signal — clone it, read it in effects and
    /// memos, bind text to it. The timeline retains its own clone to write into.
    pub fn animate(
        &mut self,
        rt: &Runtime,
        from: f32,
        to: f32,
        duration: f32,
        easing: Easing,
    ) -> Signal<f32> {
        self.animate_with(
            rt,
            AnimSpec {
                from,
                to,
                duration,
                delay: 0.0,
                easing,
                repeat: Repeat::Once,
            },
        )
    }

    /// Start a tween described by a full [`AnimSpec`] (endpoints, duration, delay, easing,
    /// repeat) and return the [`Signal`] it drives.
    ///
    /// This is the general constructor [`animate`](Self::animate) and the [`Tween`] builder
    /// both funnel through. The same start-at-`from` and exact-landing guarantees apply;
    /// the extra parameters add the delay/repeat behavior documented on [`AnimSpec`] and
    /// [`Repeat`]. A negative `delay` in the spec is clamped to `0.0` here so a host can
    /// pass a computed `index * step` without guarding the sign.
    pub fn animate_with(&mut self, rt: &Runtime, spec: AnimSpec) -> Signal<f32> {
        let signal = rt.signal(spec.from);
        self.tweens.push(Active {
            signal: signal.clone(),
            from: spec.from,
            to: spec.to,
            delay_left: if spec.delay > 0.0 { spec.delay } else { 0.0 },
            elapsed: 0.0,
            duration: spec.duration,
            easing: spec.easing,
            repeat: spec.repeat,
            reversed: false,
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
    /// timeline is idle. Note a [`Loop`](Repeat::Loop)/[`PingPong`](Repeat::PingPong) tween
    /// never finishes on its own, so `tick` stays `true` until the host
    /// [`clear`](Self::clear)s it.
    pub fn tick(&mut self, dt: f32) -> bool {
        let dt = if dt > 0.0 { dt } else { 0.0 };

        // Advance each tween, retaining only the ones still running. `retain_mut` keeps
        // this a single pass with no extra allocation: a tween that reports finished is
        // dropped here, after its final `set` has been written. Repeating tweens always
        // report not-finished, so they survive.
        self.tweens.retain_mut(|tween| !tween.advance(dt));

        !self.tweens.is_empty()
    }

    /// Drop *every* running tween immediately. This is the host's "stop" for ambient
    /// [`Loop`](Repeat::Loop)/[`PingPong`](Repeat::PingPong) animations that never end on
    /// their own.
    ///
    /// Each driven [`Signal`] keeps whatever value it last held — clearing only stops
    /// further writes, it does not reset anything — so a glow frozen mid-breath stays put
    /// until something else sets it. After this the timeline is [`is_idle`](Self::is_idle)
    /// and [`tick`](Self::tick) returns `false` until a new animation is started.
    pub fn clear(&mut self) {
        self.tweens.clear();
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
/// Identical behavior to the method — it exists only so that code which holds the timeline
/// as a separate value reads as `animate(&mut tl, &rt, 0.0, 1.0, ..)`, matching the prose
/// in the crate docs. See [`Timeline::animate`] for the full contract.
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
    fn new_quad_and_cubic_easings_have_sensible_midpoints() {
        // Each new curve hits its endpoints (covered above) and a hand-checked midpoint.
        // ease-in: f(0.5) = 0.5² = 0.25, below linear.
        approx(Easing::EaseInQuad.apply(0.5), 0.25);
        // ease-in-out quad: symmetric, crosses 0.5 at its center.
        approx(Easing::EaseInOutQuad.apply(0.5), 0.5);
        // ease-in cubic: f(0.5) = 0.5³ = 0.125, lower still.
        approx(Easing::EaseInCubic.apply(0.5), 0.125);
        // ease-in-out cubic: symmetric center.
        approx(Easing::EaseInOutCubic.apply(0.5), 0.5);

        // Direction sanity: ease-in curves lag linear early on, sharper for cubic.
        assert!(Easing::EaseInQuad.apply(0.25) < 0.25);
        assert!(Easing::EaseInCubic.apply(0.25) < Easing::EaseInQuad.apply(0.25));
    }

    #[test]
    fn spring_overshoots_before_settling() {
        // The spring curve is meant to pass above 1.0 near the end (the bouncy arrival)
        // and then settle exactly to 1.0.
        let peak = Easing::Spring.apply(0.8);
        assert!(peak > 1.0, "spring should overshoot mid-flight; got {peak}");
        approx(Easing::Spring.apply(1.0), 1.0);
    }

    #[test]
    fn delay_holds_at_from_then_interpolates() {
        // A delayed tween must sit at `from` for the whole delay, then run normally — this
        // is exactly what staggers element N of an entrance (delay = N * step).
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 10.0, 1.0)
            .delay(0.5)
            .easing(Easing::Linear)
            .start(&mut tl, &rt);

        approx(v.get(), 0.0); // pre-tick: at `from`.

        // Partway into the delay: still pinned at `from`, still busy.
        let busy = tl.tick(0.25);
        rt.flush();
        assert!(busy, "delayed tween keeps the timeline busy");
        approx(v.get(), 0.0);

        // Finish the delay exactly: still at `from`, interpolation has not advanced.
        let busy = tl.tick(0.25);
        rt.flush();
        assert!(busy);
        approx(v.get(), 0.0);

        // Now half the duration: linear ⇒ ~5.0.
        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 5.0);

        // Lands exactly on `to` and goes idle.
        let busy = tl.tick(0.5);
        rt.flush();
        approx(v.get(), 10.0);
        assert!(!busy);
        assert!(tl.is_idle());
    }

    #[test]
    fn delay_overflow_spills_into_interpolation() {
        // A single big frame that covers the whole delay *and* some interpolation must not
        // lose the leftover time — the tween should already be moving after one tick.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 1.0, 1.0).delay(0.5).start(&mut tl, &rt);

        // 0.5 delay + 0.25 into a 1.0s linear pass ⇒ ~0.25.
        tl.tick(0.75);
        rt.flush();
        approx(v.get(), 0.25);
    }

    #[test]
    fn loop_wraps_back_toward_from_and_stays_active() {
        // A Loop tween reaches `to`, snaps back toward `from`, and never lets the timeline
        // go idle — the cue a host uses to keep an ambient redraw alive.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 10.0, 1.0)
            .repeat(Repeat::Loop)
            .easing(Easing::Linear)
            .start(&mut tl, &rt);

        // Mid-pass.
        let busy = tl.tick(0.5);
        rt.flush();
        assert!(busy);
        approx(v.get(), 5.0);

        // Cross the end by 0.2: wraps to 0.2 into the next pass ⇒ ~2.0, NOT 10.0, and the
        // tween is still active.
        let busy = tl.tick(0.7);
        rt.flush();
        assert!(busy, "a Loop tween never completes on its own");
        approx(v.get(), 2.0);
        assert!(!tl.is_idle());
        assert_eq!(tl.active_count(), 1);

        // It keeps going indefinitely.
        let busy = tl.tick(1.0);
        rt.flush();
        assert!(busy);
        approx(v.get(), 2.0); // another full pass later, same phase.
    }

    #[test]
    fn pingpong_reverses_direction_and_stays_active() {
        // A PingPong tween runs up to `to`, then comes back *down* toward `from`, smoothly
        // (no snap), forever.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 10.0, 1.0)
            .repeat(Repeat::PingPong)
            .easing(Easing::Linear)
            .start(&mut tl, &rt);

        // Up to ~8.0 on the forward pass.
        let busy = tl.tick(0.8);
        rt.flush();
        assert!(busy);
        approx(v.get(), 8.0);

        // Cross the top by 0.3 into the return pass: value comes back DOWN. On the reverse
        // pass, 0.3 in ⇒ 10 - 0.3*10 = 7.0.
        let busy = tl.tick(0.5);
        rt.flush();
        assert!(busy, "a PingPong tween never completes on its own");
        let after = v.get();
        approx(after, 7.0);
        assert!(after < 8.0, "value must come back down on the return pass");
        assert!(!tl.is_idle());

        // Continue the return pass toward `from`: another 0.4 ⇒ 0.7 in ⇒ 3.0.
        tl.tick(0.4);
        rt.flush();
        approx(v.get(), 3.0);
    }

    #[test]
    fn clear_stops_a_repeating_tween_and_freezes_its_value() {
        // The documented host "stop": clear() drops the loop, the signal keeps its last
        // value, and the timeline goes idle.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 10.0, 1.0)
            .repeat(Repeat::Loop)
            .start(&mut tl, &rt);

        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 5.0);
        assert!(!tl.is_idle());

        tl.clear();
        assert!(tl.is_idle(), "clear drops every tween");
        assert_eq!(tl.active_count(), 0);

        // Further ticks change nothing; the value is frozen where it was.
        let busy = tl.tick(1.0);
        rt.flush();
        assert!(!busy);
        approx(v.get(), 5.0);
    }

    #[test]
    fn builder_defaults_match_the_once_animate_path() {
        // Tween::new(..).start(..) with no other knobs must behave exactly like animate():
        // Once, no delay, runs to `to`, then goes idle.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let v = Tween::new(0.0, 4.0, 1.0).start(&mut tl, &rt);

        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 2.0); // linear default.

        let busy = tl.tick(0.5);
        rt.flush();
        approx(v.get(), 4.0);
        assert!(!busy, "default builder is Once and completes");
        assert!(tl.is_idle());
    }

    #[test]
    fn animate_with_spec_round_trips_through_the_builder_spec() {
        // The spec built by the fluent API can be fed straight to animate_with.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let spec = Tween::new(0.0, 1.0, 1.0)
            .delay(0.25)
            .easing(Easing::Linear)
            .spec();
        let v = tl.animate_with(&rt, spec);

        // Through the delay it stays at `from`, then interpolates.
        tl.tick(0.25);
        rt.flush();
        approx(v.get(), 0.0);
        tl.tick(0.5);
        rt.flush();
        approx(v.get(), 0.5);
    }

    #[test]
    fn staggered_entrance_starts_each_element_a_beat_later() {
        // The headline use-case: three elements, delay = index * step, one shared tick.
        let rt = Runtime::new();
        let mut tl = Timeline::new();
        let step = 0.2;
        let v: alloc::vec::Vec<Signal<f32>> = (0..3)
            .map(|i| {
                Tween::new(0.0, 1.0, 0.4)
                    .delay(i as f32 * step)
                    .easing(Easing::Linear)
                    .start(&mut tl, &rt)
            })
            .collect();

        // After 0.2s: element 0 is halfway (0.2/0.4), element 1 just leaving `from`,
        // element 2 still fully at `from`.
        tl.tick(0.2);
        rt.flush();
        approx(v[0].get(), 0.5);
        approx(v[1].get(), 0.0);
        approx(v[2].get(), 0.0);

        // After another 0.2s (t=0.4): element 0 done, element 1 halfway, element 2 leaving.
        tl.tick(0.2);
        rt.flush();
        approx(v[0].get(), 1.0);
        approx(v[1].get(), 0.5);
        approx(v[2].get(), 0.0);
    }
}
