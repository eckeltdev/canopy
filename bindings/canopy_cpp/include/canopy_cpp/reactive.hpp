#pragma once

#include <cstdint>

#include "canopy_cpp/signal.hpp"

// The reactive runtime BODY: the dirty-set + flush engine that backs `signal<T>` and the DSL's
// reactive `text(λ)` overload. It mirrors Rust's `canopy-signals` / `canopy-view`:
//
//   * a signal's `get()` records a subscription IF an effect is running (the `running` mechanism);
//   * a signal's `set()` marks subscribed effects DIRTY — it does NOT re-run them inline;
//   * `flush(ctx)` re-runs each dirty effect ONCE, and a text binding's effect emits exactly ONE
//     targeted `SetText(node, new_text)` per run rather than rebuilding the subtree.
//
// AUTO-DISCOVERY: a process-wide active-runtime pointer is installed by the runtime for the
// duration of a build pass and each flush; `signal::get/set` read it through the seam hooks in
// signal.hpp to find the runtime — no component ever wires a runtime in by hand.
//
// ZERO-OVERHEAD STATIC PATH: with no active runtime the seam hooks are no-ops, so a tree with no
// reactivity emits byte-IDENTICAL bytes to the non-reactive path (the dsl_test parity gates hold).
//
// FREESTANDING SEAM: the subscription registry / effect arena uses std::vector behind
// CANOPY_CPP_USE_STD, with a fixed-capacity fallback for the no-heap target. signal.hpp stays
// POD-only and never pulls this in; only an authoring/flush translation unit includes it.
#if !defined(CANOPY_CPP_USE_STD) && !defined(CANOPY_CPP_NO_STD)
// A build-time platform toggle (parallels gfx-rt's GFX_RT_USE_STD): selects the std::vector-backed
// registries vs the fixed-capacity no-heap fallback. A macro is the right tool here — it gates both
// includes and member layout — so the macro-usage guidance does not apply.
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage)
#define CANOPY_CPP_USE_STD 1
#endif

#ifdef CANOPY_CPP_USE_STD
#include <vector>
#else
#include <array>
#include <cstddef>
#endif

namespace canopy {

    class build_context;

    // Capacity of the fixed-size fallback arenas when CANOPY_CPP_USE_STD is off (the no-heap
    // target). Generous for a single screen of bindings; overflow is dropped, never UB.
    inline constexpr std::uint32_t reactive_fixed_capacity = 256;

    // One registered effect: the callable to run plus its owned context pointer. `ctx_data` is
    // whatever the binding parked (a heap-owned closure box for text bindings); the runtime frees
    // it on teardown via `free_ctx`.
    struct effect_record {
        bound_fn run = nullptr;
        void* ctx_data = nullptr;
        void (*free_ctx)(void* ctx_data) noexcept = nullptr;
    };

    // A subscription edge: a signal identity (its cell address) paired with the effect that read
    // it. Stored as a flat edge list — small N, and it keeps the registry POD and arena-friendly.
    struct subscription_edge {
        const void* signal_key = nullptr;
        effect_id effect = 0;
    };

    // The single-threaded reactive runtime: effect registry, subscription edges, dirty queue, and
    // the currently-running effect. `build_context` owns one and installs it as active for its
    // build/flush passes.
    class reactive_runtime {
    public:
        reactive_runtime() = default;
        reactive_runtime(const reactive_runtime&) = delete;
        auto operator=(const reactive_runtime&) -> reactive_runtime& = delete;
        reactive_runtime(reactive_runtime&&) = delete;
        // The `&&` move-assignment trips the c-style-cast heuristic; this is a deleted special
        // member, not a cast.
        // cpp-doctor: allow-next-line dangerous.no-c-style-cast
        auto operator=(reactive_runtime&&) -> reactive_runtime& = delete;
        ~reactive_runtime();

        // Register `run` (with owned `ctx_data`, freed by `free_ctx`) as an effect and run it ONCE
        // immediately under dependency tracking, so its reads subscribe it. Returns the new id.
        auto register_effect(bound_fn run, void* ctx_data,
                             void (*free_ctx)(void* ctx_data) noexcept) -> effect_id;

        // Re-run every dirty effect once, draining the queue (a run may dirty further effects).
        // The effects' targeted ops land in `ctx`; the caller `take_batch`es them afterward.
        void flush(build_context& ctx);

        // The build_context this runtime is currently flushing into (so an effect body can reach
        // the emitter). Valid only during `flush`; nullptr otherwise.
        [[nodiscard]] auto flush_target() const noexcept -> build_context* {
            return flush_target_;
        }

        // ---- seam, called by the signal hooks --------------------------------------------------
        void record_subscription(const void* signal_key);
        void mark_dirty(const void* signal_key);

    private:
        void run_effect(effect_id eid, const effect_record& record);
        [[nodiscard]] auto effect_count() const noexcept -> std::uint32_t;
        void queue_dirty(effect_id eid);

        // The effect currently running (so a signal read knows whom to subscribe). `running_valid_`
        // gates it because effect_id has no spare sentinel.
        effect_id running_ = 0;
        bool running_valid_ = false;
        build_context* flush_target_ = nullptr;

#ifdef CANOPY_CPP_USE_STD
        std::vector<effect_record> effects_;
        std::vector<subscription_edge> subscriptions_;
        std::vector<effect_id> dirty_;
#else
        std::array<effect_record, reactive_fixed_capacity> effects_{};
        std::uint32_t effect_len_ = 0;
        std::array<subscription_edge, reactive_fixed_capacity> subscriptions_{};
        std::uint32_t subscription_len_ = 0;
        std::array<effect_id, reactive_fixed_capacity> dirty_{};
        std::uint32_t dirty_len_ = 0;
#endif
    };

    // Install `runtime` as the process-wide active runtime for the lifetime of this guard, then
    // restore the previous one — an RAII scope the build pass and `flush` open so `signal::get/set`
    // can discover the runtime through the seam. Restores on any exit.
    class active_runtime_scope {
    public:
        explicit active_runtime_scope(reactive_runtime* runtime) noexcept;
        active_runtime_scope(const active_runtime_scope&) = delete;
        auto operator=(const active_runtime_scope&) -> active_runtime_scope& = delete;
        active_runtime_scope(active_runtime_scope&&) = delete;
        // The `&&` move-assignment trips the c-style-cast heuristic; deleted special member.
        // cpp-doctor: allow-next-line dangerous.no-c-style-cast
        auto operator=(active_runtime_scope&&) -> active_runtime_scope& = delete;
        ~active_runtime_scope();

    private:
        reactive_runtime* previous_;
    };

    // The active runtime, or nullptr if none is installed. The signal seam reads this; the DSL's
    // reactive `text` overload reads it to decide whether to register a binding (active) or resolve
    // once (nullptr — the static path).
    [[nodiscard]] auto active_runtime() noexcept -> reactive_runtime*;

} // namespace canopy
