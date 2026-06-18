#ifndef FRT_PLATFORM_H
#define FRT_PLATFORM_H

/*
 * frt_platform.h — the Canopy platform seam (the C ABI FLOOR).
 *
 * This is the only contract everything above it routes through: every C++ heap allocation
 * (via the global operator new/delete in new_delete.cpp) and, on the bare-metal device, the
 * Rust #[global_allocator], call these symbols. Canopy OWNS this contract; a backend (the
 * host-POSIX backend now, the bare-metal HAL later, e.g. gfx-rt) supplies the bodies.
 *
 * It is deliberately pure C (no C++ in this header) so the same declarations serve a C/Rust
 * consumer. Keep it tiny and stable.
 *
 * FREE CONTRACT (load-bearing):
 *   The (size, align) passed to frt_platform_free are ADVISORY HINTS, never required.
 *   A plain C++ `operator delete(void*)` has NO size, so 0 may be passed for size; a Rust
 *   GlobalAlloc::dealloc passes a real Layout. The two are NOT symmetric. A backend that
 *   needs the size or alignment at free MUST store its own per-allocation header and MUST
 *   NOT rely on these arguments being accurate. The host backend ignores them entirely.
 */

#include <stddef.h> /* size_t  */
#include <stdint.h> /* uint64_t */

#if defined(__cplusplus)
#define FRT_NORETURN [[noreturn]]
extern "C" {
#else
#define FRT_NORETURN _Noreturn
#endif

/* Allocate `size` bytes aligned to `align` (a power of two). Returns NULL on failure; the
 * C++ throwing operator new turns NULL into a panic, the nothrow forms return NULL. */
void *frt_platform_alloc(size_t size, size_t align);

/* Free a block from frt_platform_alloc. `size`/`align` are advisory (see FREE CONTRACT).
 * Freeing NULL is a no-op. */
void frt_platform_free(void *ptr, size_t size, size_t align);

/* Abort the program with a diagnostic message; never returns. Called on OOM and on
 * unrecoverable invariant violations. */
FRT_NORETURN void frt_platform_panic(const char *msg);

/* Write `len` bytes of log text (not necessarily NUL-terminated). */
void frt_platform_log(const char *msg, size_t len);

/* A monotonic tick counter and its frequency (ticks per second), for timing. */
uint64_t frt_platform_ticks(void);
uint64_t frt_platform_ticks_per_second(void);

#if defined(__cplusplus)
} /* extern "C" */
#endif

#endif /* FRT_PLATFORM_H */
