#include "frt/platform.hpp"

#include <atomic>
#include <cstddef>
#include <cstdint>

#include "frt/frt_platform.h"

namespace frt {

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

} // namespace frt

// The C ABI seam: thin forwarders to the active backend. These are the stable symbols the C++
// operator new/delete and (on device) the Rust #[global_allocator] call.
extern "C" {

void* frt_platform_alloc(std::size_t size, std::size_t align) {
    return frt::platform().alloc(size, align);
}

void frt_platform_free(void* ptr, std::size_t size, std::size_t align) {
    frt::platform().free(ptr, size, align);
}

void frt_platform_panic(const char* msg) {
    frt::platform().panic(msg);
    // The contract says a backend's panic never returns; if one erroneously does, trap rather
    // than fall off the end of this [[noreturn]] seam function (which would be UB).
    __builtin_trap();
}

void frt_platform_log(const char* msg, std::size_t len) {
    frt::platform().log(msg, len);
}

std::uint64_t frt_platform_ticks(void) {
    return frt::platform().ticks();
}

std::uint64_t frt_platform_ticks_per_second(void) {
    return frt::platform().ticks_per_second();
}

} // extern "C"
