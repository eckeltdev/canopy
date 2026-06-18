#pragma once

// frt's entry point for the vendored xstd extended STL. INCLUDE THIS BEFORE any <xstd/*.hpp>
// header — it sets xstd's configuration to honour the frt freestanding policy and (as their
// dependency chains are unlocked) to route xstd's platform hooks through the frt platform seam.
// After including this, include the specific xstd headers you want.
//
// CURATED FREESTANDING-SAFE SUBSET — compiles under -fno-exceptions -fno-rtti today:
//   type_helpers, bitwise, fnv, math, narrow_cast, result, assert, formatting, logger (+ their
//   transitive base xstd/intrinsics.hpp, whose x86 paths are arch-guarded, skipped on AArch64).
//   The assert/logger/formatting chain was unblocked by one frt vendor patch to
//   xstd/formatting.hpp (a deduced-return `as_string` used before its definition, which Apple
//   clang 17 rejects — see the "frt vendor patch" comment there). Genuinely hosted xstd
//   (http / coro / sockets / gzip / file / thread_pool / time) stays out by not being included.
//
// STILL DEFERRED: the platform-hook BINDINGS (XSTD_ASSERT failure -> frt_platform_panic via
// xstd's logger/error config; XSTD_DEFAULT_CLOCK_READ -> frt_platform_ticks). Those headers
// COMPILE now but aren't yet ROUTED to the frt seam — that wiring + its test is the next step,
// which is why frt/frt_platform.h is not yet included here.

// xstd resolves a would-be `throw` into an error value instead of a C++ exception — required
// under -fno-exceptions and the correct policy for the runtime core.
#ifndef XSTD_NO_EXCEPTIONS
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage) — an xstd config switch; must be a #define (#if'd)
#define XSTD_NO_EXCEPTIONS 1
#endif
