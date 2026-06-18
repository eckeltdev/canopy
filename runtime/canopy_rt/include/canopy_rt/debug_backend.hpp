#pragma once

#include <cstddef>

#include "canopy_rt/platform.hpp"

// A host-only instrumenting backend that wraps host_ops and records each allocation's
// (size, align), then verifies them at free. It makes the device-fragile advisory-free
// contract (a backend must self-describe its block sizes) catchable on the host: a sized or
// aligned delete whose hint disagrees with the original alloc bumps `mismatch_count`.
namespace canopy::rt {

    struct debug_stats {
        std::size_t alloc_count;    // intercepted allocations observed
        std::size_t free_count;     // intercepted frees observed
        std::size_t live_count;     // currently-live intercepted allocations
        std::size_t mismatch_count; // sized/aligned frees whose hint disagreed with the alloc
    };

    // Install the debug-tracking backend; returns the previously-active backend.
    auto install_debug_backend() -> const platform_ops*;

    // Snapshot the counters.
    [[nodiscard]] auto debug_backend_stats() -> debug_stats;

    // Clear the counters and the live set (call between test phases).
    void debug_backend_reset();

} // namespace canopy::rt
