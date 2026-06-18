#include "canopy_cpp/reactive.hpp"

#include <cstdint>

#include "canopy_cpp/build_context.hpp"
#include "canopy_cpp/signal.hpp"

#ifdef CANOPY_CPP_USE_STD
#include <vector>
#else
#include <array>
#include <cstddef>
#endif

// The reactive runtime body. Mirrors canopy-signals' dirty-set + flush: a get() under a running
// effect records a subscription edge; a set() pushes every subscribed effect onto the dirty queue
// WITHOUT re-running it; flush() drains the queue, re-running each dirty effect once. A reactive
// text binding's effect emits exactly one targeted SetText per run (see dsl.hpp's reactive
// overload), so an update is surgical — no structural ops.
namespace canopy {

    namespace {

        // The process-wide active runtime. Single-threaded by design (the M0 runtime is `Rc`/
        // `RefCell` on the Rust side); the C++ port matches that — one event loop, one runtime
        // installed at a time. `signal::get/set` and the DSL reactive overload read this. It is
        // mutable by nature (the active runtime is swapped by every build pass / flush scope), so
        // the non-const-global rule does not apply.
        // NOLINTNEXTLINE(cppcoreguidelines-avoid-non-const-global-variables)
        reactive_runtime* g_active_runtime = nullptr;

    } // namespace

    auto active_runtime() noexcept -> reactive_runtime* {
        return g_active_runtime;
    }

    active_runtime_scope::active_runtime_scope(reactive_runtime* runtime) noexcept
        : previous_(g_active_runtime) {
        g_active_runtime = runtime;
    }

    active_runtime_scope::~active_runtime_scope() {
        g_active_runtime = previous_;
    }

    // ---- the seam hooks declared in signal.hpp -------------------------------------------------
    // These are `noexcept` by the freestanding contract (the device builds -fno-exceptions, where a
    // would-be std::bad_alloc from the registry's vector growth aborts rather than unwinds). The
    // escape-analysis warning is therefore expected and intentional.

    // NOLINTNEXTLINE(bugprone-exception-escape)
    void detail::reactive_record_subscription(const void* signal_key) noexcept {
        if (reactive_runtime* runtime = g_active_runtime; runtime != nullptr) {
            runtime->record_subscription(signal_key);
        }
    }

    // NOLINTNEXTLINE(bugprone-exception-escape)
    void detail::reactive_mark_dirty(const void* signal_key) noexcept {
        if (reactive_runtime* runtime = g_active_runtime; runtime != nullptr) {
            runtime->mark_dirty(signal_key);
        }
    }

    // ---- reactive_runtime ----------------------------------------------------------------------

    reactive_runtime::~reactive_runtime() {
        for (const effect_record& record : effects_) {
            if (record.free_ctx != nullptr && record.ctx_data != nullptr) {
                record.free_ctx(record.ctx_data);
            }
        }
    }

    auto reactive_runtime::effect_count() const noexcept -> std::uint32_t {
#ifdef CANOPY_CPP_USE_STD
        return static_cast<std::uint32_t>(effects_.size());
#else
        return effect_len_;
#endif
    }

    auto reactive_runtime::register_effect(bound_fn run, void* ctx_data,
                                           void (*free_ctx)(void* ctx_data) noexcept) -> effect_id {
        const effect_id eid = effect_count();
        const effect_record record{.run = run, .ctx_data = ctx_data, .free_ctx = free_ctx};
#ifdef CANOPY_CPP_USE_STD
        effects_.push_back(record);
#else
        if (effect_len_ >= reactive_fixed_capacity) {
            // Over fixed capacity on the no-heap target: free the orphaned context and drop the
            // effect rather than overrun. (CANOPY_CPP_USE_STD is the default everywhere today.)
            if (free_ctx != nullptr && ctx_data != nullptr) {
                free_ctx(ctx_data);
            }
            return eid;
        }
        effects_.at(effect_len_) = record;
        ++effect_len_;
#endif
        run_effect(eid, record);
        return eid;
    }

    void reactive_runtime::run_effect(effect_id eid, const effect_record& record) {
        if (record.run == nullptr) {
            return;
        }
        // Install `running` for the duration of the run so the signals this effect reads subscribe
        // to it (mirrors Runtime::run_effect's `running.replace`). Restore the previous on exit.
        const effect_id previous = running_;
        const bool previous_valid = running_valid_;
        running_ = eid;
        running_valid_ = true;
        record.run(record.ctx_data, eid);
        running_ = previous;
        running_valid_ = previous_valid;
    }

    void reactive_runtime::record_subscription(const void* signal_key) {
        if (!running_valid_) {
            return; // a get() outside any effect run records nothing — the static read path
        }
        const effect_id current = running_;
#ifdef CANOPY_CPP_USE_STD
        for (const subscription_edge& edge : subscriptions_) {
            if (edge.signal_key == signal_key && edge.effect == current) {
                return; // already subscribed — keep the edge set deduplicated
            }
        }
        subscriptions_.push_back({.signal_key = signal_key, .effect = current});
#else
        for (std::uint32_t index = 0; index < subscription_len_; ++index) {
            const subscription_edge& edge = subscriptions_.at(index);
            if (edge.signal_key == signal_key && edge.effect == current) {
                return;
            }
        }
        if (subscription_len_ < reactive_fixed_capacity) {
            subscriptions_.at(subscription_len_) = {.signal_key = signal_key, .effect = current};
            ++subscription_len_;
        }
#endif
    }

    void reactive_runtime::queue_dirty(effect_id eid) {
#ifdef CANOPY_CPP_USE_STD
        for (const effect_id queued : dirty_) {
            if (queued == eid) {
                return; // already queued — the dirty set stays deduplicated
            }
        }
        dirty_.push_back(eid);
#else
        for (std::uint32_t index = 0; index < dirty_len_; ++index) {
            if (dirty_.at(index) == eid) {
                return;
            }
        }
        if (dirty_len_ < reactive_fixed_capacity) {
            dirty_.at(dirty_len_) = eid;
            ++dirty_len_;
        }
#endif
    }

    void reactive_runtime::mark_dirty(const void* signal_key) {
        // Push every effect subscribed to this signal onto the dirty queue. Does NOT re-run them —
        // that is flush()'s job. A snapshot loop so a re-entrant edge change can't invalidate it.
#ifdef CANOPY_CPP_USE_STD
        for (const subscription_edge& edge : subscriptions_) {
            if (edge.signal_key == signal_key) {
                queue_dirty(edge.effect);
            }
        }
#else
        for (std::uint32_t index = 0; index < subscription_len_; ++index) {
            const subscription_edge& edge = subscriptions_.at(index);
            if (edge.signal_key == signal_key) {
                queue_dirty(edge.effect);
            }
        }
#endif
    }

    void reactive_runtime::flush(build_context& ctx) {
        // Make the emitter reachable to effect bodies, and install ourselves as active so the
        // re-run's get() calls re-subscribe (matching Rust, where flush runs effects under the
        // same runtime). Both are restored on exit.
        flush_target_ = &ctx;
        const active_runtime_scope scope(this);

        // Drain the dirty queue until it empties (an effect may dirty further effects). Each pass
        // takes a snapshot of the current dirty set, mirroring Runtime::flush's `mem::take`.
        while (true) {
#ifdef CANOPY_CPP_USE_STD
            if (dirty_.empty()) {
                break;
            }
            std::vector<effect_id> batch;
            batch.swap(dirty_);
            for (const effect_id eid : batch) {
                if (eid < effect_count()) {
                    run_effect(eid, effects_.at(eid));
                }
            }
#else
            if (dirty_len_ == 0) {
                break;
            }
            std::array<effect_id, reactive_fixed_capacity> batch{};
            const std::uint32_t batch_len = dirty_len_;
            for (std::uint32_t index = 0; index < batch_len; ++index) {
                batch.at(index) = dirty_.at(index);
            }
            dirty_len_ = 0;
            for (std::uint32_t index = 0; index < batch_len; ++index) {
                const effect_id eid = batch.at(index);
                if (eid < effect_count()) {
                    run_effect(eid, effects_.at(static_cast<std::size_t>(eid)));
                }
            }
#endif
        }
        flush_target_ = nullptr;
    }

} // namespace canopy
