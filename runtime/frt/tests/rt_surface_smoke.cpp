#include <cstddef>
#include <cstdio>

#include "frt/config.hpp"
#include "frt/containers.hpp"
#include "frt/stl.hpp"

// rt_surface_smoke.cpp — M2 STL-surface smoke test.
//
// Built with -fno-exceptions -fno-rtti -DFRT_FREESTANDING=1 (see cpp-doctor.toml), so
// merely including config.hpp fires its static_asserts: this TU compiling at all is the proof
// that exceptions and RTTI are off. The runtime checks below then prove the two behavioural
// contracts that must hold without exceptions — fixed_vector signals overflow with `false`
// (never throws) and the arena bumps then resets — and exercise the std-vocabulary aliases.
//
// No exceptions are used; failures are reported by returning false and printing to stderr, and
// main() turns the aggregate into a 0/1 exit code.
namespace {

    // fixed_vector fills to capacity, then refuses further pushes with false (no throw, no grow).
    auto fixed_vector_overflow_returns_false() -> bool {
        frt::fixed_vector<int, 4> values;
        for (int idx = 0; idx < 4; ++idx) {
            if (!values.push_back(idx)) {
                std::fprintf(stderr, "FAIL: push_back rejected a value within capacity\n");
                return false;
            }
        }
        if (values.size() != values.capacity()) {
            std::fprintf(stderr, "FAIL: fixed_vector did not reach capacity\n");
            return false;
        }
        // The fifth push must fail cleanly (this is the no-throw overflow contract).
        if (values.push_back(99)) {
            std::fprintf(stderr, "FAIL: overflowing push_back returned true\n");
            return false;
        }
        if (values.size() != 4 || values.back() != 3) {
            std::fprintf(stderr, "FAIL: overflow mutated the container\n");
            return false;
        }
        values.clear();
        if (!values.empty()) {
            std::fprintf(stderr, "FAIL: clear() did not empty the container\n");
            return false;
        }
        return true;
    }

    // The arena bumps an aligned cursor, refuses an over-large request with nullptr, then reset()
    // rewinds so the buffer is reusable.
    auto arena_bumps_and_resets() -> bool {
        alignas(std::max_align_t) std::array<frt::byte, 128> backing{};
        frt::arena scratch{frt::span<frt::byte>{backing}};

        void* first = scratch.allocate(16, alignof(std::max_align_t));
        void* second = scratch.allocate(16, alignof(std::max_align_t));
        if (first == nullptr || second == nullptr) {
            std::fprintf(stderr, "FAIL: arena could not satisfy two in-bounds allocations\n");
            return false;
        }
        if (first == second || scratch.used() < 32) {
            std::fprintf(stderr, "FAIL: arena did not bump its cursor\n");
            return false;
        }
        // A request larger than what remains must return nullptr, not throw or wrap.
        if (scratch.allocate(scratch.capacity(), 1) != nullptr) {
            std::fprintf(stderr, "FAIL: over-large allocation did not return nullptr\n");
            return false;
        }
        scratch.reset();
        if (scratch.used() != 0) {
            std::fprintf(stderr, "FAIL: reset() did not rewind the arena\n");
            return false;
        }
        void* after_reset = scratch.allocate(16, alignof(std::max_align_t));
        if (after_reset == nullptr) {
            std::fprintf(stderr, "FAIL: arena did not reuse the buffer after reset\n");
            return false;
        }
        return true;
    }

    // Exercise the std-vocabulary aliases so the alias header is part of the compiled surface.
    auto aliases_are_usable() -> bool {
        constexpr frt::array<int, 3> source{10, 20, 30};
        const frt::span<const int> view{source};
        const frt::pair<int, int> ends{view.front(), view.back()};
        const frt::optional<int> present{ends.second};
        const frt::string_view label{"canopy"};

        const frt::expected<int, int> ok =
            present.has_value() ? frt::expected<int, int>{present.value()}
                                : frt::expected<int, int>{frt::unexpected<int>{-1}};

        auto owned = frt::unique_ptr<int>{std::make_unique<int>(ends.first)};

        const bool good = view.size() == source.size() && ends.first == 10 && ends.second == 30 &&
                          ok.has_value() && ok.value() == 30 && *owned == 10 && label.size() == 6 &&
                          sizeof(frt::byte) == 1;
        if (!good) {
            std::fprintf(stderr, "FAIL: std-vocabulary aliases did not behave as expected\n");
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const bool all_passed = fixed_vector_overflow_returns_false() && arena_bumps_and_resets() &&
                            aliases_are_usable();
    if (all_passed) {
        std::fprintf(stderr, "ok: frt STL surface smoke passed\n");
        return 0;
    }
    return 1;
}
