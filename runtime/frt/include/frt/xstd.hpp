#pragma once

// frt's entry point for the vendored xstd extended STL. INCLUDE THIS BEFORE any <xstd/*.hpp>
// header — it sets xstd's configuration to honour the frt freestanding policy AND routes xstd's
// platform hooks through the frt platform seam (frt/frt_platform.h). After including this,
// include the specific xstd headers you want.
//
// CURATED FREESTANDING-SAFE SUBSET — compiles under -fno-exceptions -fno-rtti today:
//   type_helpers, bitwise, fnv, math, narrow_cast, result, assert, formatting, logger, time
//   (+ their transitive base xstd/intrinsics.hpp, whose x86 paths are arch-guarded, skipped on
//   AArch64). The assert/logger/formatting chain was unblocked by one frt vendor patch to
//   xstd/formatting.hpp (a deduced-return `as_string` used before its definition, which Apple
//   clang 17 rejects — see the "frt vendor patch" comment there). Genuinely hosted xstd
//   (http / coro / sockets / gzip / file / thread_pool) stays out by not being included.
//
// PLATFORM-HOOK ROUTING (this is what wires xstd onto frt's seam):
//
//   CLOCK — xstd::time::now() reads XSTD_DEFAULT_CLOCK_READ() and wraps it as
//     timestamp( duration( <read>() ) ). The default reads std::chrono::high_resolution_clock.
//     We keep the default clock type (so xstd::duration / timestamp arithmetic is unchanged) but
//     force the READ to frt_platform_ticks(), so every xstd timestamp's raw count is frt's tick
//     counter — whatever backend frt::install_platform() made active. ROUTED + TESTED.
//
//   FATAL / ERROR PATH — xstd's assert (xassert/fassert) and throw_fmt route a failure into
//     xstd::error(...) under XSTD_NO_EXCEPTIONS. logger.hpp exposes XSTD_CON_ERROR_REDIRECT: when
//     defined to the name of an `extern "C" void NAME [[noreturn]](const char*)`, xstd::error
//     formats the message and tail-calls that function instead of touching stdio/exit. We point
//     it at frt_xstd_error_redirect (below), which forwards to frt_platform_panic — so an xstd
//     fatal diagnostic lands on frt's panic seam. This is xstd's ONE clean override for the fatal
//     path; there is no per-call function hook for the *non-fatal* log/warning sink (those write
//     to FILE* via XSTD_CON_*_DST and cannot be pointed at frt_platform_log without a FILE
//     shim), so the genuinely-routable error/panic path is what we bind. ROUTED + TESTED (the
//     test drives an xstd error through the redirect and captures it on an installed frt backend
//     whose panic escapes via longjmp, honouring [[noreturn]] without aborting the process).

#include "frt/frt_platform.h"

// xstd resolves a would-be `throw` into an error value instead of a C++ exception — required
// under -fno-exceptions and the correct policy for the runtime core.
#ifndef XSTD_NO_EXCEPTIONS
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage) — an xstd config switch; must be a #define (#if'd)
#define XSTD_NO_EXCEPTIONS 1
#endif

// CLOCK BINDING: make xstd's monotonic clock read frt's platform ticks. xstd wraps the result of
// XSTD_DEFAULT_CLOCK_READ() in duration(...) (an integer rep count), so returning the uint64_t
// tick directly is exactly the shape xstd wants. We leave XSTD_DEFAULT_CLOCK at its default so
// the duration/time_point *types* are unchanged.
#ifndef XSTD_DEFAULT_CLOCK_READ
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage) — an xstd config switch; must be a function-macro
#define XSTD_DEFAULT_CLOCK_READ() (::frt_platform_ticks())
#endif

// FATAL-PATH BINDING: name the extern "C" [[noreturn]] redirect xstd::error will call on a fatal
// diagnostic. xstd declares this symbol itself (logger.hpp) from the macro; we define it below.
#ifndef XSTD_CON_ERROR_REDIRECT
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage) — an xstd config switch; must be a #define
#define XSTD_CON_ERROR_REDIRECT frt_xstd_error_redirect
#endif

// The redirect target: forward an xstd fatal diagnostic onto frt's panic seam. `inline` gives it
// a single definition across every TU that includes this header (a function may carry C language
// linkage and still be inline). frt_platform_panic is itself [[noreturn]], satisfying the
// contract xstd's logger.hpp declares for the redirect.
extern "C" {
// cpp-doctor: allow-next-line naming — fixed C-ABI hook name dictated by xstd's redirect macro.
[[noreturn]] inline void frt_xstd_error_redirect(const char* msg) {
    ::frt_platform_panic(msg);
}
}
