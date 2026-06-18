#pragma once

#include <cstdint>
#include <utility>

// The fine-grained reactivity SURFACE plus its subscription SEAM. `signal<T>` is the state cell
// a component holds; the DSL reads it inside a reactive closure (e.g. `text([&]{ return label();
// })`). The get / set / version contract is frozen so components author against a stable API.
//
// The runtime BODY (subscription tracking on get, dirty propagation on set, a flush that re-runs
// dirty effects and emits one targeted op each) lives in `reactive.hpp` / the reactive runtime
// unit, NOT here. This header must stay POD-only and FREESTANDING-SAFE: it is pulled into
// tests/freestanding_smoke.cpp under -fno-exceptions -fno-rtti, so it deliberately does NOT
// include <vector> (or any other container). The seam carries only an `effect_id`, a bound-fn
// typedef, and two free-function HOOKS the runtime defines; the effect arena / subscription
// registry (which uses std::vector) sits entirely behind CANOPY_CPP_USE_STD over in the runtime.
namespace canopy {

    // A registered effect's identity in the active runtime. 0 is a valid id; "no effect running"
    // is signalled out-of-band by the runtime, never by a sentinel here.
    using effect_id = std::uint64_t;

    // The callable shape a binding registers (run once on mount, re-run on flush). It takes the
    // effect's own id so its body can re-establish `running` for the duration of the run. Defined
    // as a raw function-pointer + opaque context pair so the seam stays POD and allocation-free;
    // the runtime owns whatever the context points at.
    using bound_fn = void (*)(void* ctx, effect_id self);

    namespace detail {

        // ---- subscription HOOKS ---------------------------------------------------------------
        // These are DECLARED here (POD, no container) and DEFINED by the reactive runtime unit.
        // A `signal_key` is the address of a signal cell, used as its stable identity in the
        // runtime's subscription registry — the runtime never dereferences it as a T.

        // Record that the currently-running effect (if any) depends on the signal at `signal_key`.
        // A no-op when no runtime is active or no effect is running, which is what keeps a static
        // tree's bytes byte-identical: `get()` outside a build/flush pass does nothing extra.
        void reactive_record_subscription(const void* signal_key) noexcept;

        // Mark every effect subscribed to the signal at `signal_key` DIRTY (to be re-run on the
        // next flush). A no-op when no runtime is active. This does NOT re-run anything inline.
        void reactive_mark_dirty(const void* signal_key) noexcept;

    } // namespace detail

    template <class T> class signal {
    public:
        explicit signal(T value) : value_(std::move(value)) {}

        // Read the current value. If an effect is running in the active runtime, this also
        // records a subscription so the effect re-runs when the value changes. With no active
        // runtime (the static authoring path) it is a plain read — resolving a reactive slot
        // ONCE during a non-reactive mount is byte-identical to a static value.
        [[nodiscard]] auto get() const -> const T& {
            detail::reactive_record_subscription(this);
            return value_;
        }

        // Replace the value, bump the version, and mark subscribed effects dirty. The dirty
        // effects are NOT re-run inline; a `reactive_runtime::flush` re-runs them once each,
        // emitting one targeted diff op per binding. With no active runtime it just updates state.
        void set(T value) {
            value_ = std::move(value);
            ++version_;
            detail::reactive_mark_dirty(this);
        }

        // Monotonic change counter — lets a reactive slot detect staleness directly.
        [[nodiscard]] auto version() const noexcept -> std::uint64_t {
            return version_;
        }

    private:
        T value_;
        std::uint64_t version_ = 0;
    };

} // namespace canopy
