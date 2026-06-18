#include <cstddef>
#include <cstdint>
#include <iostream>

#include "frt/frt_platform.h"
#include "frt/platform.hpp"

namespace {

    // A probe backend (wrapping the host backend) that counts alloc calls, installed at runtime
    // to prove the C seam routes to whatever ops are active.
    auto probe_calls() -> int& {
        static int calls = 0;
        return calls;
    }
    auto probe_alloc(std::size_t size, std::size_t align) -> void* {
        probe_calls() += 1;
        return frt::host_ops().alloc(size, align);
    }
    void probe_free(void* ptr, std::size_t size, std::size_t align) {
        frt::host_ops().free(ptr, size, align);
    }
    void probe_panic(const char* msg) {
        frt::host_ops().panic(msg);
    }
    void probe_log(const char* msg, std::size_t len) {
        frt::host_ops().log(msg, len);
    }
    auto probe_ticks() -> std::uint64_t {
        return frt::host_ops().ticks();
    }
    auto probe_ticks_per_second() -> std::uint64_t {
        return frt::host_ops().ticks_per_second();
    }
    constexpr frt::platform_ops probe_ops_table = {
        .alloc = probe_alloc,
        .free = probe_free,
        .panic = probe_panic,
        .log = probe_log,
        .ticks = probe_ticks,
        .ticks_per_second = probe_ticks_per_second,
    };

    // With no backend installed, the C seam uses the host default.
    bool default_backend_round_trips() {
        void* ptr = frt_platform_alloc(64, 16);
        if (ptr == nullptr) {
            std::cerr << "FAIL: default alloc returned null\n";
            return false;
        }
        frt_platform_free(ptr, 64, 16);
        return true;
    }

    bool ticks_are_monotonic_with_a_frequency() {
        const std::uint64_t start = frt_platform_ticks();
        const std::uint64_t later = frt_platform_ticks();
        if (later < start) {
            std::cerr << "FAIL: ticks went backwards\n";
            return false;
        }
        if (frt_platform_ticks_per_second() == 0) {
            std::cerr << "FAIL: zero tick frequency\n";
            return false;
        }
        return true;
    }

    // Installing a backend reroutes the C seam without a relink.
    bool installing_a_backend_reroutes_the_seam() {
        frt::install_platform(probe_ops_table);
        const int before = probe_calls();
        void* ptr = frt_platform_alloc(32, 8);
        const int after = probe_calls();
        frt_platform_free(ptr, 32, 8);
        if (after != before + 1) {
            std::cerr << "FAIL: install did not reroute the seam\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const bool all_passed = default_backend_round_trips() &&
                            ticks_are_monotonic_with_a_frequency() &&
                            installing_a_backend_reroutes_the_seam();
    if (all_passed) {
        std::cerr << "ok: platform host tests passed\n";
        return 0;
    }
    return 1;
}
