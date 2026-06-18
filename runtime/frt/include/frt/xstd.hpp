#pragma once

// frt's entry point for the vendored xstd extended STL. INCLUDE THIS BEFORE any <xstd/*.hpp>
// header — it sets xstd's configuration to honour the frt freestanding policy and (as their
// dependency chains are unlocked) to route xstd's platform hooks through the frt platform seam.
// After including this, include the specific xstd headers you want.
//
// CURATED FREESTANDING-SAFE SUBSET — compiles under -fno-exceptions -fno-rtti today:
//   xstd/type_helpers.hpp, xstd/bitwise.hpp, xstd/fnv.hpp  (+ their transitive base
//   xstd/intrinsics.hpp, whose x86 paths are arch-guarded so they are skipped on AArch64).
//
// GATED (not yet supported freestanding): anything that pulls xstd/assert.hpp, which drags
// xstd/logger.hpp -> xstd/formatting.hpp. formatting.hpp has a deduced-return-type ordering
// issue (its single-argument `as_string` is called by the STL string_formatter<> specialisations
// before its own definition) that Apple clang 17 rejects. Unlocking that chain — a vendor patch
// to xstd/formatting.hpp — is what enables narrow_cast / result / assert and the
// XSTD_ASSERT -> frt_platform_panic and XSTD_DEFAULT_CLOCK_READ -> frt_platform_ticks bindings
// (deliberately NOT defined here yet, since the headers that consume them do not build, so
// frt/frt_platform.h is intentionally not yet included here — it returns with those bindings).

// xstd resolves a would-be `throw` into an error value instead of a C++ exception — required
// under -fno-exceptions and the correct policy for the runtime core.
#ifndef XSTD_NO_EXCEPTIONS
// NOLINTNEXTLINE(cppcoreguidelines-macro-usage) — an xstd config switch; must be a #define (#if'd)
#define XSTD_NO_EXCEPTIONS 1
#endif
