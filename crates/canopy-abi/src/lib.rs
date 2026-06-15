//! Canopy's stable **C ABI** over the host op-stream — the cross-language
//! embedding surface.
//!
//! # Why this crate exists
//!
//! A core thesis of Canopy is that *each language builds its own React-like wrapper
//! over the core*. A Rust host can link [`canopy_dom::Dom`] directly, but a C++,
//! Swift, Kotlin, or Python (ctypes) host cannot — Rust has no stable ABI. This
//! crate is the **single stable seam** those hosts link against: an opaque handle
//! and a handful of `extern "C"` functions. The contract is intentionally tiny,
//! because the whole protocol already lives in the op bytes. A foreign host only
//! needs to:
//!
//! 1. create a host handle ([`canopy_host_new`]),
//! 2. hand it batches of op bytes to validate and apply ([`canopy_host_apply`]),
//! 3. read back simple facts (e.g. [`canopy_host_node_count`]),
//! 4. free the handle ([`canopy_host_free`]).
//!
//! The op bytes themselves are produced by a guest using `canopy-core`'s `Emitter`
//! (in whatever language, via its own binding) and are **validated host-side** by
//! [`canopy_dom::Dom`], so the foreign host never has to understand the wire
//! format — it just shuttles bytes.
//!
//! # Trust model (mirrors the wasmtime transport)
//!
//! The bytes crossing [`canopy_host_apply`] are treated as **untrusted**, exactly
//! like the bytes a sandboxed wasm guest hands `canopy-transport-wasmtime`:
//!
//! * **Null/bounds-checked.** Every raw pointer is checked for null before use, and
//!   `len` is rejected if it exceeds [`MAX_BATCH_BYTES`] — the host never sizes an
//!   allocation from an untrusted number, nor reads through a dangling pointer.
//! * **Capability-validated.** The bytes are applied through
//!   [`canopy_traits::OpSink`]; the `Dom` mints and validates every node handle, so
//!   a forged batch that names a node the guest never created is rejected with an
//!   error code, not silently aliased.
//! * **Never panics, never UB.** Bad input — null handle, oversized length,
//!   undecodable bytes, forged handle — is reported as a negative [error code](self#error-codes),
//!   never a panic that would unwind across the FFI boundary (which is itself UB)
//!   and never an out-of-bounds read.
//!
//! # Error codes
//!
//! [`canopy_host_apply`] returns `0` on success and one of these negative codes on
//! failure. They are also published as `CANOPY_*` constants in the hand-written
//! header `include/canopy.h` so a C caller can name them.
//!
//! | Code | Constant                  | Meaning                                            |
//! |-----:|---------------------------|----------------------------------------------------|
//! |  `0` | `CANOPY_OK`               | The batch was decoded, validated, and applied.     |
//! | `-1` | `CANOPY_ERR_NULL_HOST`    | The `host` pointer was null.                       |
//! | `-2` | `CANOPY_ERR_NULL_DATA`    | The `ptr` was null while `len > 0`.                |
//! | `-3` | `CANOPY_ERR_TOO_LARGE`    | `len` exceeded [`MAX_BATCH_BYTES`].                |
//! | `-4` | `CANOPY_ERR_DECODE`       | The bytes were not a valid op-stream.              |
//! | `-5` | `CANOPY_ERR_BAD_HANDLE`   | A mutating op named a node the guest never created.|
//! | `-6` | `CANOPY_ERR_UNSUPPORTED`  | The op is unsupported on this host/tier.           |
//!
//! # Safety / FFI seam
//!
//! This is the project's **explicit FFI boundary**, so it is the one crate that
//! opts out of the workspace-wide `unsafe_code = "deny"`. Reconstructing a
//! `Box<CanopyHost>` from a caller-supplied raw pointer is inherently `unsafe` —
//! there is no safe way to express "trust me, this pointer came from
//! `canopy_host_new`". Every `unsafe` block below is the smallest possible
//! pointer→reference reconstruction and carries a `// SAFETY:` note stating the
//! contract the caller must uphold. All *other* workspace lints still apply
//! (`[lints] workspace = true`), so only the FFI seam itself is unsafe.
#![allow(unsafe_code)]

use canopy_dom::Dom;
use canopy_traits::{HostError, OpSink};

/// Hard cap on a single [`canopy_host_apply`] batch, in bytes.
///
/// A caller-supplied `len` larger than this is rejected with
/// [`CANOPY_ERR_TOO_LARGE`] before any memory is touched. This mirrors
/// `canopy-transport-wasmtime`'s `MAX_BATCH_BYTES`: the host never sizes a buffer
/// from an untrusted length.
pub const MAX_BATCH_BYTES: usize = 1 << 20; // 1 MiB

/// Return code: the batch was decoded, validated, and applied.
pub const CANOPY_OK: i32 = 0;
/// Return code: the `host` pointer was null.
pub const CANOPY_ERR_NULL_HOST: i32 = -1;
/// Return code: the data pointer was null while `len > 0`.
pub const CANOPY_ERR_NULL_DATA: i32 = -2;
/// Return code: `len` exceeded [`MAX_BATCH_BYTES`].
pub const CANOPY_ERR_TOO_LARGE: i32 = -3;
/// Return code: the bytes were not a decodable op-stream.
pub const CANOPY_ERR_DECODE: i32 = -4;
/// Return code: a mutating op named a node the guest never created (forged handle).
pub const CANOPY_ERR_BAD_HANDLE: i32 = -5;
/// Return code: the op is unsupported on this host/tier.
pub const CANOPY_ERR_UNSUPPORTED: i32 = -6;

/// The opaque host handle exposed across the C ABI.
///
/// A foreign host only ever sees a `*mut CanopyHost`; the layout is deliberately
/// private so the wire-level [`Dom`] can evolve without breaking the ABI. It is
/// created by [`canopy_host_new`], driven by [`canopy_host_apply`], and destroyed by
/// [`canopy_host_free`].
pub struct CanopyHost {
    /// The host's retained tree. It validates every handle and decodes the op bytes,
    /// so the C ABI itself holds no protocol knowledge.
    dom: Dom,
}

impl CanopyHost {
    /// A fresh host wrapping an empty [`Dom`]. Exposed for Rust embedders that link
    /// this crate as an `rlib` and would rather use the handle directly than go
    /// through raw pointers.
    pub fn new() -> Self {
        Self { dom: Dom::new() }
    }

    /// Apply one op batch through the safe, capability-validating path, mapping the
    /// host result to a stable C error code.
    ///
    /// This is the single place the byte slice meets the `Dom`. Both the C entry
    /// point and the Rust tests funnel through here, so the happy path and every
    /// error path are exercised without going through raw pointers.
    pub fn apply_bytes(&mut self, bytes: &[u8]) -> i32 {
        match self.dom.apply(bytes) {
            Ok(()) => CANOPY_OK,
            Err(e) => error_code(e),
        }
    }

    /// The number of live nodes (excluding the implicit host root).
    pub fn node_count(&self) -> usize {
        self.dom.node_count()
    }

    /// Borrow the underlying retained tree (for Rust embedders that want richer
    /// reads than the C surface exposes).
    pub fn dom(&self) -> &Dom {
        &self.dom
    }
}

impl Default for CanopyHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a host-side [`HostError`] to its stable C error code. Kept exhaustive (no
/// wildcard arm) so adding a `HostError` variant forces a deliberate choice here
/// rather than silently collapsing to a generic code.
fn error_code(e: HostError) -> i32 {
    match e {
        HostError::BadHandle => CANOPY_ERR_BAD_HANDLE,
        HostError::Decode => CANOPY_ERR_DECODE,
        HostError::Unsupported => CANOPY_ERR_UNSUPPORTED,
    }
}

/// Create a new Canopy host and return an owning pointer to it.
///
/// The returned pointer is **owned by the caller** and must eventually be passed to
/// [`canopy_host_free`] exactly once; dropping it on the floor leaks the host. It is
/// never null (allocation failure aborts, as is standard for Rust's allocator).
///
/// # Safety
///
/// This function is safe to call from any thread, but the returned handle is **not**
/// `Sync`: a single host must not be driven from two threads concurrently. (It may be
/// moved between threads, and distinct hosts are independent.)
#[no_mangle]
pub extern "C" fn canopy_host_new() -> *mut CanopyHost {
    Box::into_raw(Box::new(CanopyHost::new()))
}

/// Decode, validate, and apply one op batch to `host`.
///
/// `ptr`/`len` describe a buffer of `canopy-protocol` op bytes (as produced by a
/// guest's `Emitter::take_batch`). The bytes are treated as untrusted: the length is
/// capped at [`MAX_BATCH_BYTES`], the pointer is null-checked, and the `Dom`
/// validates every handle while decoding. On success the host's retained tree
/// reflects the batch.
///
/// Returns [`CANOPY_OK`] (`0`) on success or one of the negative
/// [`CANOPY_ERR_*`](self#error-codes) codes. It **never panics and never triggers
/// UB on bad input** — a null host, null data, oversized length, undecodable bytes,
/// or a forged handle are all reported as error codes.
///
/// A `len` of `0` is a valid no-op batch and returns [`CANOPY_OK`]; in that case
/// `ptr` may be null.
///
/// # Safety
///
/// The caller must ensure that:
/// * `host` is either null or a pointer returned by [`canopy_host_new`] that has not
///   yet been freed, and
/// * if `len > 0`, then `ptr` points to at least `len` readable, initialized bytes
///   that stay valid for the duration of the call.
///
/// Passing a dangling or mis-sized `ptr` with `len > 0` is undefined behavior, as it
/// is for any C function that reads through a pointer+length. All *other* misuse
/// (null host, null data, oversized or garbage bytes) is handled and returns an
/// error code.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_apply(
    host: *mut CanopyHost,
    ptr: *const u8,
    len: usize,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    // Reject an oversized length before forming any slice — never trust the size.
    if len > MAX_BATCH_BYTES {
        return CANOPY_ERR_TOO_LARGE;
    }
    // An empty batch is a valid no-op; tolerate a null `ptr` only in that case.
    if len == 0 {
        // SAFETY: `host` was checked non-null above and, per the function contract,
        // is a live pointer from `canopy_host_new`. We form a unique reference for
        // the duration of this call only; the caller guarantees no aliasing
        // concurrent access to the same host.
        let host = unsafe { &mut *host };
        return host.apply_bytes(&[]);
    }
    if ptr.is_null() {
        return CANOPY_ERR_NULL_DATA;
    }

    // SAFETY: `ptr` is non-null and, per the function contract, points to at least
    // `len` readable, initialized bytes valid for this call; `len <= MAX_BATCH_BYTES`
    // so it fits an `isize`. We do not retain the slice past this call.
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };

    // SAFETY: `host` was checked non-null above and is a live pointer from
    // `canopy_host_new`; we form a unique reference for the duration of this call.
    let host = unsafe { &mut *host };
    host.apply_bytes(bytes)
}

/// The number of live nodes in `host`'s retained tree (excluding the implicit root).
///
/// Returns `0` if `host` is null, so a caller can read it defensively without a
/// separate null check.
///
/// # Safety
///
/// `host` must be either null or a live pointer returned by [`canopy_host_new`] that
/// has not been freed.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_node_count(host: *const CanopyHost) -> usize {
    if host.is_null() {
        return 0;
    }
    // SAFETY: `host` is non-null and, per contract, a live pointer from
    // `canopy_host_new`. We form a shared reference for this call only.
    let host = unsafe { &*host };
    host.node_count()
}

/// Destroy a host created by [`canopy_host_new`], freeing its retained tree.
///
/// Passing null is a no-op (so double-free guards in foreign code that null their
/// pointer are tolerated). Passing the same non-null pointer twice, or any pointer
/// not returned by [`canopy_host_new`], is undefined behavior — the usual C
/// free-once contract.
///
/// # Safety
///
/// `host` must be either null or a pointer returned by [`canopy_host_new`] that has
/// not already been freed. After this call the pointer is dangling and must not be
/// used again.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_free(host: *mut CanopyHost) {
    if host.is_null() {
        return;
    }
    // SAFETY: `host` is non-null and, per contract, was produced by `Box::into_raw`
    // in `canopy_host_new` and not yet freed. Reconstructing the `Box` takes back
    // ownership; dropping it runs `Dom`'s destructor and frees the allocation.
    drop(unsafe { Box::from_raw(host) });
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::ROOT;
    use canopy_protocol::ElementTag;

    /// Build a real op batch: a column element with a text child, both appended under
    /// the host root. Returns the encoded bytes — exactly what a guest would hand the
    /// host.
    fn mounted_batch() -> Vec<u8> {
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        let label = e.create_text("hello");
        e.append(col, label);
        e.take_batch(0)
    }

    /// Drive a batch through the real C entry point (pointer + length), the way a
    /// foreign host would.
    fn apply_via_c(host: *mut CanopyHost, batch: &[u8]) -> i32 {
        // SAFETY: `host` comes from `canopy_host_new` below, and `batch` is a live
        // Rust slice valid for the call.
        unsafe { canopy_host_apply(host, batch.as_ptr(), batch.len()) }
    }

    #[test]
    fn happy_path_applies_and_counts_through_the_c_abi() {
        let host = canopy_host_new();
        assert!(!host.is_null());

        let batch = mounted_batch();
        let rc = apply_via_c(host, &batch);
        assert_eq!(rc, CANOPY_OK, "a well-formed batch applies cleanly");

        // SAFETY: `host` is the live pointer from `canopy_host_new`.
        let count = unsafe { canopy_host_node_count(host) };
        assert_eq!(count, 2, "the column + text child are both live");

        // SAFETY: `host` was created here and is freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn null_host_returns_error_codes_not_panics() {
        let batch = mounted_batch();
        // SAFETY: a null host is explicitly handled; `batch` is a live slice.
        let rc = unsafe { canopy_host_apply(core::ptr::null_mut(), batch.as_ptr(), batch.len()) };
        assert_eq!(rc, CANOPY_ERR_NULL_HOST);

        // node_count on null is defined to be 0.
        // SAFETY: a null host is explicitly handled.
        let count = unsafe { canopy_host_node_count(core::ptr::null()) };
        assert_eq!(count, 0);

        // Freeing null is a no-op.
        // SAFETY: a null host is explicitly handled.
        unsafe { canopy_host_free(core::ptr::null_mut()) };
    }

    #[test]
    fn null_data_with_nonzero_len_is_rejected() {
        let host = canopy_host_new();
        // SAFETY: `host` is live; we pass a null data pointer with a non-zero length,
        // which the function rejects before dereferencing it.
        let rc = unsafe { canopy_host_apply(host, core::ptr::null(), 8) };
        assert_eq!(rc, CANOPY_ERR_NULL_DATA);
        // The tree is untouched.
        // SAFETY: `host` is live.
        assert_eq!(unsafe { canopy_host_node_count(host) }, 0);
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn empty_batch_is_a_valid_noop() {
        let host = canopy_host_new();
        // len == 0 with a null ptr is allowed and applies nothing.
        // SAFETY: `host` is live; len 0 means `ptr` is never dereferenced.
        let rc = unsafe { canopy_host_apply(host, core::ptr::null(), 0) };
        assert_eq!(rc, CANOPY_OK);
        // SAFETY: `host` is live.
        assert_eq!(unsafe { canopy_host_node_count(host) }, 0);
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn garbage_bytes_decode_to_an_error_not_a_crash() {
        let host = canopy_host_new();
        // Bytes that are not a valid op-stream must surface a decode error, never a
        // panic or UB.
        let garbage = [0xFFu8, 0x00, 0x13, 0x37, 0xAB, 0xCD];
        let rc = apply_via_c(host, &garbage);
        assert_eq!(rc, CANOPY_ERR_DECODE);
        // SAFETY: `host` is live.
        assert_eq!(unsafe { canopy_host_node_count(host) }, 0);
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn truncated_batch_decodes_to_an_error() {
        let host = canopy_host_new();
        let mut batch = mounted_batch();
        // Cut the batch in half so the op-stream ends mid-op.
        batch.truncate(batch.len() / 2);
        let rc = apply_via_c(host, &batch);
        assert_eq!(
            rc, CANOPY_ERR_DECODE,
            "a truncated stream is a decode error"
        );
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn forged_handle_is_rejected_as_bad_handle() {
        // The capability boundary: a batch that mutates a node the guest never created
        // must be refused with the bad-handle code, mirroring the wasmtime transport.
        let host = canopy_host_new();

        // First, a valid mount so the host has *some* live nodes.
        let real = mounted_batch();
        assert_eq!(apply_via_c(host, &real), CANOPY_OK);

        // Now hand-roll a batch that targets a fabricated handle far beyond anything
        // allocated above.
        let mut forged = Emitter::new();
        for _ in 0..1000 {
            forged.alloc_node();
        }
        let ghost = forged.alloc_node();
        forged.set_text(ghost, "haxx");
        let rc = apply_via_c(host, &forged.take_batch(1));
        assert_eq!(rc, CANOPY_ERR_BAD_HANDLE);

        // The valid nodes from the first batch are still intact.
        // SAFETY: `host` is live.
        assert_eq!(unsafe { canopy_host_node_count(host) }, 2);
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn oversized_length_is_rejected_before_any_read() {
        let host = canopy_host_new();
        // A length over the cap must be rejected without dereferencing `ptr` — pass a
        // dangling-but-unused pointer to prove the length check fires first.
        let dummy = [0u8; 4];
        // SAFETY: the function rejects `len > MAX_BATCH_BYTES` before reading `ptr`,
        // so the slice is never formed; the pointer is never dereferenced.
        let rc = unsafe { canopy_host_apply(host, dummy.as_ptr(), MAX_BATCH_BYTES + 1) };
        assert_eq!(rc, CANOPY_ERR_TOO_LARGE);
        // SAFETY: freed exactly once.
        unsafe { canopy_host_free(host) };
    }

    #[test]
    fn safe_rust_path_mirrors_the_c_path() {
        // Rust embedders linking the rlib can use the handle directly; verify it
        // agrees with the C entry points.
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&mounted_batch()), CANOPY_OK);
        assert_eq!(host.node_count(), 2);
        assert_eq!(host.dom().children(ROOT).len(), 1);
    }
}
