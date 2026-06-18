#include "frt/xstd.hpp" // configure + route xstd onto the frt seam — MUST precede any <xstd/*.hpp>

#include <csetjmp>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <iostream>

#include "frt/platform.hpp"
#include "xstd/assert.hpp" // xassert / fassert -> xstd::error -> XSTD_CON_ERROR_REDIRECT
#include "xstd/time.hpp"   // xstd::time::now() -> XSTD_DEFAULT_CLOCK_READ() -> frt_platform_ticks

// Proof that xstd's platform hooks are ROUTED through the frt seam (frt_platform.h), not merely
// that they compile. We install an instrumented frt::platform_ops backend (via
// frt::install_platform) that RECORDS what flows through the seam, then drive xstd and assert the
// recorded backend values come back out the other side.
//
//   CLOCK: backend.ticks returns a fixed sentinel. xstd::time::now()'s raw count must equal it,
//          proving xstd read frt_platform_ticks() (our backend) and not a real wall clock.
//   FATAL: backend.panic captures the message and escapes via longjmp (honouring the redirect's
//          [[noreturn]] contract without aborting the process). Driving an xstd fatal diagnostic
//          must land that message on the capture, proving xstd::error -> XSTD_CON_ERROR_REDIRECT
//          -> frt_platform_panic -> our installed backend.
namespace {

    // The fixed tick value the instrumented backend reports; xstd's clock must echo it back.
    constexpr std::uint64_t fixed_tick_value = 0xABCD'1234'5678'9001ULL;

    // Capture state for the panic escape. setjmp target + the recorded panic message.
    // cpp-doctor: allow-next-line dangerous_patterns — setjmp/longjmp is the standard way to
    // exercise a [[noreturn]] panic hook without aborting the test process.
    std::jmp_buf panic_escape;
    auto panic_message() -> char (&)[256] {
        static char buffer[256] = {};
        return buffer;
    }
    auto panic_was_seen() -> bool& {
        static bool seen = false;
        return seen;
    }

    // --- the instrumented frt backend ----------------------------------------------------------
    // alloc/free defer to the host so std::string formatting inside xstd::error still works.
    auto record_alloc(std::size_t size, std::size_t align) -> void* {
        return frt::host_ops().alloc(size, align);
    }
    void record_free(void* ptr, std::size_t size, std::size_t align) {
        frt::host_ops().free(ptr, size, align);
    }
    // The clock seam: always report the sentinel so the test can prove xstd read THIS value.
    auto record_ticks() -> std::uint64_t {
        return fixed_tick_value;
    }
    auto record_ticks_per_second() -> std::uint64_t {
        return frt::host_ops().ticks_per_second();
    }
    // The panic seam: capture the message, then escape. NEVER returns (longjmp), so the
    // [[noreturn]] guarantee the redirect promises xstd is honoured.
    // cpp-doctor: allow-next-line dangerous_patterns — see panic_escape note above.
    [[noreturn]] void record_panic(const char* msg) {
        panic_was_seen() = true;
        const std::size_t len = (msg != nullptr) ? std::strlen(msg) : 0;
        const std::size_t copy = (len < sizeof(panic_message()) - 1) ? len : sizeof(panic_message()) - 1;
        std::memcpy(&panic_message()[0], msg, copy);
        panic_message()[copy] = '\0';
        std::longjmp(panic_escape, 1);
    }
    void record_log(const char* msg, std::size_t len) {
        frt::host_ops().log(msg, len);
    }
    constexpr frt::platform_ops record_ops_table = {
        .alloc = record_alloc,
        .free = record_free,
        .panic = record_panic,
        .log = record_log,
        .ticks = record_ticks,
        .ticks_per_second = record_ticks_per_second,
    };

    // CLOCK proof: xstd::time::now() wraps XSTD_DEFAULT_CLOCK_READ() (which frt routes to
    // frt_platform_ticks) as timestamp(duration(<read>)). The raw count of that timestamp must
    // equal the sentinel our installed backend reports — i.e. the clock genuinely read the seam.
    bool xstd_clock_reads_frt_ticks() {
        const auto stamp = xstd::time::now();
        const auto raw = static_cast<std::uint64_t>(stamp.time_since_epoch().count());
        if (raw != fixed_tick_value) {
            std::cerr << "FAIL: xstd clock read " << raw << ", expected frt ticks "
                      << fixed_tick_value << '\n';
            return false;
        }
        return true;
    }

    // FATAL proof: drive an xstd fatal diagnostic and confirm it lands on the frt panic seam.
    // fassert(false) -> xstd::error(...) -> XSTD_CON_ERROR_REDIRECT (frt_xstd_error_redirect)
    // -> frt_platform_panic -> our installed record_panic, which captures + longjmps back here.
    bool xstd_fatal_routes_to_frt_panic() {
        panic_was_seen() = false;
        panic_message()[0] = '\0';
        // cpp-doctor: allow-next-line dangerous_patterns — setjmp pairs with the panic longjmp.
        if (setjmp(panic_escape) == 0) {
            fassert(1 == 2); // always-false: takes xstd's failure path into the redirect
            std::cerr << "FAIL: xstd fatal diagnostic returned instead of escaping via panic\n";
            return false;
        }
        // Back here only via record_panic's longjmp.
        if (!panic_was_seen()) {
            std::cerr << "FAIL: longjmp fired without the frt panic seam being hit\n";
            return false;
        }
        if (panic_message()[0] == '\0') {
            std::cerr << "FAIL: frt panic seam received an empty message from xstd\n";
            return false;
        }
        std::cerr << "ok: xstd fatal routed to frt panic seam: \"" << &panic_message()[0]
                  << "\"\n";
        return true;
    }

} // namespace

int main() {
    frt::install_platform(record_ops_table);

    const bool all_passed = xstd_clock_reads_frt_ticks() && xstd_fatal_routes_to_frt_panic();
    if (all_passed) {
        std::cerr << "ok: xstd platform hooks (clock + fatal) route through the frt seam\n";
        return 0;
    }
    return 1;
}
