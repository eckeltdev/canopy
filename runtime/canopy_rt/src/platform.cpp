#include "canopy_rt/platform.hpp"

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "canopy_rt/canopy_platform.h"

namespace canopy::rt {

    namespace {
        // A function-local static avoids a mutable namespace-scope global; release/acquire so a
        // backend installed on one thread is visible to the seam on another.
        auto active_slot() -> std::atomic<const platform_ops*>& {
            static std::atomic<const platform_ops*> slot{nullptr};
            return slot;
        }
    } // namespace

    auto install_platform(const platform_ops& ops) -> const platform_ops* {
        return active_slot().exchange(&ops, std::memory_order_acq_rel);
    }

    auto platform() -> const platform_ops& {
        const platform_ops* active = active_slot().load(std::memory_order_acquire);
        return active != nullptr ? *active : host_ops();
    }

} // namespace canopy::rt

// The C ABI seam: thin forwarders to the active backend. These are the stable symbols the C++
// operator new/delete and (on device) the Rust #[global_allocator] call.
extern "C" {

void* canopy_platform_alloc(std::size_t size, std::size_t align) {
    return canopy::rt::platform().alloc(size, align);
}

void canopy_platform_free(void* ptr, std::size_t size, std::size_t align) {
    canopy::rt::platform().free(ptr, size, align);
}

void canopy_platform_panic(const char* msg) {
    canopy::rt::platform().panic(msg);
    // The contract says a backend's panic never returns; if one erroneously does, trap rather
    // than fall off the end of this [[noreturn]] seam function (which would be UB).
    __builtin_trap();
}

void canopy_platform_log(const char* msg, std::size_t len) {
    canopy::rt::platform().log(msg, len);
}

std::uint64_t canopy_platform_ticks(void) {
    return canopy::rt::platform().ticks();
}

std::uint64_t canopy_platform_ticks_per_second(void) {
    return canopy::rt::platform().ticks_per_second();
}

} // extern "C"
