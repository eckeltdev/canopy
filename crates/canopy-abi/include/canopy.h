/*
 * canopy.h — the stable C ABI for embedding the Canopy host.
 *
 * This header is the cross-language surface for driving a Canopy host UI from a
 * non-Rust host (C, C++, Python via ctypes/cffi, Swift, Kotlin/JNA, Node N-API, …).
 * Link against `libcanopy_abi` (the `staticlib` `.a` or the `cdylib`
 * `.so`/`.dylib`/`.dll` produced by the `canopy-abi` crate).
 *
 * Model
 * -----
 * A `CanopyHost` is an opaque handle wrapping the host's retained node tree. Your
 * UI logic — written in any language, using its own binding over `canopy-core`'s
 * op-builder — produces batches of op bytes. You hand those bytes to
 * `canopy_host_apply`, which validates and applies them, then read back simple
 * facts (e.g. the live node count). The host owns and validates every node handle,
 * so a malformed or forged batch is a returned error code, never a crash.
 *
 * The op bytes are the entire protocol; this header is deliberately tiny. It mirrors
 * the single `canopy_apply` capability granted to a sandboxed wasm guest by
 * `canopy-transport-wasmtime` — same trust model, in-process.
 *
 * Threading
 * ---------
 * A single `CanopyHost*` is NOT thread-safe: never call into the same host from two
 * threads at once. Distinct hosts are fully independent and a host may be moved
 * between threads.
 *
 * Memory / ownership
 * ------------------
 * `canopy_host_new` returns an owning pointer; pass it to `canopy_host_free` exactly
 * once. The op-byte buffer you pass to `canopy_host_apply` is borrowed only for the
 * duration of the call — Canopy copies what it needs and never retains your pointer.
 *
 * This header is hand-maintained to match the crate's `extern "C"` exports; keep the
 * two in sync. (No cbindgen step is required.)
 */

#ifndef CANOPY_H
#define CANOPY_H

#include <stddef.h> /* size_t */
#include <stdint.h> /* int32_t */

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Return codes for canopy_host_apply.
 *
 * 0 means success; every failure is a distinct negative code. These mirror the
 * `CANOPY_*` constants exported by the Rust crate.
 */
#define CANOPY_OK 0              /* batch decoded, validated, and applied      */
#define CANOPY_ERR_NULL_HOST -1  /* the host pointer was NULL                  */
#define CANOPY_ERR_NULL_DATA -2  /* ptr was NULL while len > 0                 */
#define CANOPY_ERR_TOO_LARGE -3  /* len exceeded CANOPY_MAX_BATCH_BYTES        */
#define CANOPY_ERR_DECODE -4     /* bytes were not a valid op-stream           */
#define CANOPY_ERR_BAD_HANDLE -5 /* an op named a node the guest never created */
#define CANOPY_ERR_UNSUPPORTED -6 /* op unsupported on this host/tier          */

/*
 * Hard cap on a single canopy_host_apply batch, in bytes (1 MiB). A larger `len`
 * is rejected with CANOPY_ERR_TOO_LARGE before any memory is read. Keep this in
 * sync with `canopy_abi::MAX_BATCH_BYTES`.
 */
#define CANOPY_MAX_BATCH_BYTES (1u << 20)

/*
 * Opaque host handle. The layout is private; only hold and pass the pointer.
 */
typedef struct CanopyHost CanopyHost;

/*
 * Create a new Canopy host (an empty retained tree).
 *
 * Returns a non-NULL owning pointer. Pass it to canopy_host_free exactly once when
 * done. (Allocation failure aborts, as is standard for the Rust allocator, so the
 * result is never NULL.)
 */
CanopyHost *canopy_host_new(void);

/*
 * Decode, validate, and apply one op batch to `host`.
 *
 * `ptr`/`len` describe a buffer of canopy-protocol op bytes (produced by a guest's
 * op-builder). The bytes are treated as untrusted: `len` is capped at
 * CANOPY_MAX_BATCH_BYTES, `host` and (for len > 0) `ptr` are NULL-checked, and the
 * host validates every node handle while decoding.
 *
 * A `len` of 0 is a valid no-op batch (ptr may be NULL) and returns CANOPY_OK.
 *
 * Returns CANOPY_OK (0) on success, or one of the negative CANOPY_ERR_* codes. This
 * call never panics and never triggers undefined behavior on bad input: a NULL host,
 * NULL data, oversized length, undecodable bytes, or a forged handle each return the
 * corresponding error code.
 *
 * Precondition: if len > 0, `ptr` must point to at least `len` readable bytes that
 * remain valid for the duration of the call.
 */
int32_t canopy_host_apply(CanopyHost *host, const uint8_t *ptr, size_t len);

/*
 * The number of live nodes in `host`'s retained tree (excluding the implicit root).
 *
 * Returns 0 if `host` is NULL, so it is safe to call defensively.
 */
size_t canopy_host_node_count(const CanopyHost *host);

/*
 * Events (host -> guest)
 * ----------------------
 * Set the viewport, deliver pointer input, and drain the resulting DispatchEvent
 * batch. See canopy_protocol.h for the DispatchEvent wire layout and the well-known
 * EventKind ids (e.g. CANOPY_EVENT_CLICK).
 */

/*
 * Set the viewport (logical pixels) the tree is laid out within for hit-testing. Call
 * on window create/resize. Until set, the viewport is 0x0 (nothing has area to hit).
 * Returns CANOPY_OK, or CANOPY_ERR_NULL_HOST.
 */
int32_t canopy_host_resize(CanopyHost *host, float width, float height);

/*
 * Deliver a pointer event at (x, y): hit-test the laid-out tree and, if it lands on (or
 * within) a node carrying a listener for `event` (e.g. CANOPY_EVENT_CLICK), queue a
 * DispatchEvent for the guest to drain with canopy_host_poll_events. `button` is the
 * pressed button (0 = primary); `event` is the EventKind to match.
 *
 * Returns the number of events queued (0 or 1), or a negative CANOPY_ERR_* code.
 *
 * Hit geometry is the lite (inline-style) layout. A host-side-cascade tree (class
 * identity only, no inline styles) has no geometry here until its cascade runs.
 */
int32_t canopy_host_pointer(CanopyHost *host, float x, float y, uint8_t button,
                            uint16_t event);

/*
 * Drain queued host -> guest events into `out` (capacity `cap` bytes), writing the byte
 * length to *out_len. The drained bytes are one canopy-protocol batch
 * (BeginBatch ... DispatchEvent* ... EndBatch) the guest decodes with its normal reader.
 *
 * Returns CANOPY_OK with *out_len set (0 if the queue was empty; otherwise the queue is
 * cleared); CANOPY_ERR_TOO_LARGE with *out_len set to the NEEDED size if the batch does
 * not fit in `cap` (nothing is consumed — retry with a bigger buffer;
 * CANOPY_MAX_EVENT_BATCH_BYTES always suffices); or CANOPY_ERR_NULL_HOST /
 * CANOPY_ERR_NULL_DATA.
 *
 * Precondition: `out_len` is a writable size_t, and if cap > 0, `out` points to `cap`
 * writable bytes valid for the call.
 */
int32_t canopy_host_poll_events(CanopyHost *host, uint8_t *out, size_t cap,
                                size_t *out_len);

/*
 * Cap on a single drained event batch, in bytes (64 KiB) — the outbound analog of
 * CANOPY_MAX_BATCH_BYTES. An `out` of this size always drains the queue in one call.
 * Keep in sync with `canopy_abi::MAX_EVENT_BATCH_BYTES`.
 */
#define CANOPY_MAX_EVENT_BATCH_BYTES (64u << 10)

/*
 * Destroy a host created by canopy_host_new, freeing its retained tree.
 *
 * Passing NULL is a no-op. Passing the same non-NULL pointer twice, or any pointer
 * not returned by canopy_host_new, is undefined behavior (the usual free-once rule).
 * After this call the pointer is dangling and must not be used again.
 */
void canopy_host_free(CanopyHost *host);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* CANOPY_H */
