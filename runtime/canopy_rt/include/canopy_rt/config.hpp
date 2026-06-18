#pragma once

#include "canopy_rt/canopy_platform.h"

// config.hpp — the canopy-rt freestanding POLICY header.
//
// This is an EXPLICIT-INCLUDE policy header: it is never force-included by the build. A
// translation unit that wants the freestanding contract enforced includes it directly (the
// rest of the surface — containers.hpp / stl.hpp — pulls it in for you). Define
// CANOPY_RT_FREESTANDING when you build the freestanding-safe configuration (the device, and
// the smoke test): the static_asserts below then turn the language-mode requirements into
// compile errors instead of runtime surprises.
//
// What "freestanding-safe" means here, and how each rule is enforced:
//
//   * No exceptions. The whole runtime must compile under -fno-exceptions; nothing in the
//     surface ever throws (fixed_vector signals overflow with a bool, the arena with nullptr).
//     Under -fno-exceptions clang/gcc leave the __cpp_exceptions feature-test macro UNDEFINED,
//     so the static_assert below keys off `#ifndef __cpp_exceptions`.
//
//   * No RTTI. The runtime must compile under -fno-rtti; there is no dynamic_cast / typeid in
//     the hot path. Under -fno-rtti the __cpp_rtti feature-test macro is UNDEFINED, so the
//     static_assert keys off `#ifndef __cpp_rtti`.
//
//   * No thread-safe static-local guards (-fno-threadsafe-statics) and no thread_local. The
//     device target is single-core / interrupt-driven and links no pthread; the per-image
//     __cxa_guard_acquire / TLS machinery libstdc++ emits for function-local statics and
//     thread_local would pull in a runtime we do not have. This is a build-flag + coding
//     policy (there is no feature-test macro to assert on), documented here as the contract:
//     do not introduce a non-trivially-constructed function-local `static`, and never use
//     `thread_local`, in freestanding-safe code. (M1's host backend uses a couple of
//     trivially-zero-initialized statics, which need no guard, and lives outside this
//     freestanding TU anyway.)
//
// CANOPY_RT_PANIC(msg) is the one escape hatch: it routes to canopy_platform_panic (the C ABI
// floor, [[noreturn]]). Use it where a precondition that "cannot" fail is worth trapping; it is
// the freestanding stand-in for what would otherwise be a throw/abort.

#if defined(CANOPY_RT_FREESTANDING)

#if defined(__cpp_exceptions)
static_assert(false, "canopy-rt freestanding build requires -fno-exceptions "
                     "(__cpp_exceptions must be undefined)");
#endif

#if defined(__cpp_rtti)
static_assert(false, "canopy-rt freestanding build requires -fno-rtti "
                     "(__cpp_rtti must be undefined)");
#endif

#endif // CANOPY_RT_FREESTANDING

// Trap with a diagnostic and never return. Routes through the C ABI floor so a backend (host
// abort now, the device fault handler later) supplies the body. Wrapping the message in a
// do/while keeps it a single statement usable as the body of an `if`.
#define CANOPY_RT_PANIC(msg)                                                                       \
    do {                                                                                           \
        ::canopy_platform_panic(msg);                                                              \
    } while (false)
