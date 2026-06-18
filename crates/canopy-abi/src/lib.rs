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
use canopy_protocol::{EventKind, EventPayload, NodeId, Op, OpEncoder};
use canopy_render_soft::SoftwareRenderer;
use canopy_traits::{Color, HostError, OpSink, Point, Renderer, Size};

/// Hard cap on a single [`canopy_host_apply`] batch, in bytes.
///
/// A caller-supplied `len` larger than this is rejected with
/// [`CANOPY_ERR_TOO_LARGE`] before any memory is touched. This mirrors
/// `canopy-transport-wasmtime`'s `MAX_BATCH_BYTES`: the host never sizes a buffer
/// from an untrusted length.
pub const MAX_BATCH_BYTES: usize = 1 << 20; // 1 MiB

/// Cap on a single [`canopy_host_poll_events`] drained batch, in bytes — the outbound
/// analog of [`MAX_BATCH_BYTES`]. The host never queues more events than encode within
/// this, so an `out` buffer of this size always drains the queue in one call.
pub const MAX_EVENT_BATCH_BYTES: usize = 64 * 1024; // 64 KiB

/// Internal cap on queued events between drains, chosen so the encoded batch never
/// exceeds [`MAX_EVENT_BATCH_BYTES`] (a Pointer DispatchEvent is ~23 bytes + an 8-byte
/// envelope). Past this, new events are dropped until the queue is drained — bounded
/// memory under a flood of input with no poll.
const MAX_PENDING_EVENTS: usize = 2048;

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
    /// The host's retained tree. It validates every handle and decodes inbound op
    /// bytes, so the C ABI holds no inbound protocol knowledge.
    dom: Dom,
    /// The viewport the tree is laid out within for hit-testing. Set via
    /// [`canopy_host_resize`]; `0×0` until then (so no node has area to hit).
    viewport: Size,
    /// Events produced by hit-testing pointers, waiting to be drained by
    /// [`canopy_host_poll_events`]. Each is a host→guest `DispatchEvent`.
    pending_events: Vec<Op>,
    /// Monotonic seq stamped into each drained event batch's `BeginBatch`.
    event_seq: u32,
}

impl CanopyHost {
    /// A fresh host wrapping an empty [`Dom`]. Exposed for Rust embedders that link
    /// this crate as an `rlib` and would rather use the handle directly than go
    /// through raw pointers.
    pub fn new() -> Self {
        Self {
            dom: Dom::new(),
            viewport: Size::default(),
            pending_events: Vec::new(),
            event_seq: 0,
        }
    }

    /// Set the viewport the tree is laid out within for hit-testing.
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = Size {
            w: width,
            h: height,
        };
    }

    /// Hit-test a pointer at `(x, y)` and, if it lands on (or within) a node carrying a
    /// listener for `event`, queue a `DispatchEvent` for that handler. Returns the
    /// number of events queued (`0` or `1`).
    ///
    /// Geometry comes from the **lite (inline-style) layout**: correct for a tree whose
    /// nodes carry inline styles. A *host-side-cascade* tree (class identity only, no
    /// inline styles) lays out with no geometry here until its cascade has run — wiring
    /// the lite host-side cascade → layout is the follow-up that makes hit-testing
    /// correct for that model (the same gap that makes `canopy-ui`'s capable-tier
    /// hit-test defer to the host engine).
    pub fn pointer_event(&mut self, x: f32, y: f32, button: u8, event: u16) -> i32 {
        if self.pending_events.len() >= MAX_PENDING_EVENTS {
            return 0; // back-pressure: drop until the queue is drained
        }
        let (_scene, layout) = canopy_layout_taffy::layout(&self.dom, self.viewport);
        let Some(mut node) = canopy_layout_taffy::hit_test(&layout, Point { x, y }) else {
            return 0;
        };
        let kind = EventKind::new(event);
        // The nearest ancestor (including the hit node) with a matching listener wins —
        // mirroring `canopy-ui::click_handler`.
        loop {
            let Some(n) = self.dom.node(node) else {
                return 0;
            };
            if let Some((_, handler)) = n.listeners.iter().find(|(ev, _)| *ev == kind) {
                self.pending_events.push(Op::DispatchEvent {
                    handler: *handler,
                    node,
                    payload: EventPayload::Pointer { x, y, button },
                });
                return 1;
            }
            match n.parent {
                Some(p) => node = p,
                None => return 0,
            }
        }
    }

    /// Lite-tier render of the current tree to an RGBA8 framebuffer (row-major, straight
    /// alpha, `width * height * 4` bytes). Lays the retained tree out with the SAME
    /// inline-style engine the hit-test uses (so what you see is what you can click), then
    /// software-rasterizes the resulting display list — the device-representative no_std
    /// path. The clear color is the desktop dark base; any node without a painted
    /// background shows it through.
    pub fn render_rgba(&self, width: u32, height: u32) -> Vec<u8> {
        let viewport = Size {
            w: width as f32,
            h: height as f32,
        };
        let (scene, _layout) = canopy_layout_taffy::layout(&self.dom, viewport);
        let clear = Color {
            r: 0x1e,
            g: 0x1e,
            b: 0x2e,
            a: 0xff,
        };
        let mut renderer = SoftwareRenderer::new(width as usize, height as usize, clear);
        // `render` only errors on a malformed scene, which our own layout never produces;
        // on the impossible error path keep the clear-filled frame rather than panic.
        let _ = renderer.render(&scene);
        renderer.buffer().data().to_vec()
    }

    /// Drain the queued events into `out` as one `BeginBatch … DispatchEvent* … EndBatch`
    /// batch (so the guest decodes it with the same reader it uses for any batch).
    /// Returns `(code, written)`: on success `written` is the byte length and the queue
    /// is cleared; if the encoded batch exceeds `out.len()` nothing is consumed and the
    /// returned `(CANOPY_ERR_TOO_LARGE, needed)` lets the caller retry with a bigger
    /// buffer (an `out` of [`MAX_EVENT_BATCH_BYTES`] always suffices).
    pub fn poll_events_into(&mut self, out: &mut [u8]) -> (i32, usize) {
        if self.pending_events.is_empty() {
            return (CANOPY_OK, 0);
        }
        let mut enc = OpEncoder::new();
        enc.begin_batch(self.event_seq);
        for op in &self.pending_events {
            enc.push(op);
        }
        enc.end_batch();
        let bytes = enc.into_bytes();
        if bytes.len() > out.len() {
            return (CANOPY_ERR_TOO_LARGE, bytes.len()); // needed size; not consumed
        }
        out[..bytes.len()].copy_from_slice(&bytes);
        self.pending_events.clear();
        self.event_seq = self.event_seq.wrapping_add(1);
        (CANOPY_OK, bytes.len())
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

    /// A deterministic, human-readable dump of the retained tree — the **round-trip
    /// oracle** a foreign host asserts its op bytes against (a node count alone can't
    /// tell a swapped parent/child, a dropped class, or a mis-attached listener apart).
    ///
    /// Pre-order DFS from the root; one line per node, indented two spaces per depth.
    /// A text node renders as `text=<content>`; an element as `el tag=<n>` followed by
    /// its `name=`, `class=`, `style=`, `attr=`, and `on=` (listener) fields when present.
    /// `BTreeMap`-backed styles/attrs render in id order and `Vec`-backed
    /// classes/listeners/children keep op order, so the same tree always renders byte-for-
    /// byte identically.
    pub fn debug_snapshot(&self) -> String {
        let mut out = String::new();
        for &child in self.dom.children(canopy_dom::ROOT) {
            write_node(&self.dom, &mut out, child, 0);
        }
        out
    }
}

/// Render one node and its subtree into `out` (see [`CanopyHost::debug_snapshot`]).
fn write_node(dom: &Dom, out: &mut String, node: NodeId, depth: usize) {
    let Some(n) = dom.node(node) else {
        return;
    };
    for _ in 0..depth {
        out.push_str("  ");
    }
    if let Some(text) = &n.text {
        out.push_str("text=");
        push_escaped(out, text);
        out.push('\n');
        return;
    }
    out.push_str("el tag=");
    match n.tag {
        Some(tag) => out.push_str(&tag.raw().to_string()),
        None => out.push('?'),
    }
    if let Some(name) = &n.tag_name {
        out.push_str(" name=");
        push_escaped(out, name);
    }
    if !n.classes.is_empty() {
        out.push_str(" class=");
        out.push_str(&n.classes.join(","));
    }
    if !n.styles.is_empty() {
        let parts: Vec<String> = n
            .styles
            .iter()
            .map(|(p, v)| format!("{}:{v}", p.raw()))
            .collect();
        out.push_str(" style=");
        out.push_str(&parts.join(";"));
    }
    if !n.attrs.is_empty() {
        let parts: Vec<String> = n
            .attrs
            .iter()
            .map(|(a, v)| format!("{}:{v}", a.raw()))
            .collect();
        out.push_str(" attr=");
        out.push_str(&parts.join(";"));
    }
    if !n.listeners.is_empty() {
        let parts: Vec<String> = n
            .listeners
            .iter()
            .map(|(e, h)| format!("{}:{}", e.raw(), h.raw()))
            .collect();
        out.push_str(" on=");
        out.push_str(&parts.join(","));
    }
    out.push('\n');
    for &child in &n.children {
        write_node(dom, out, child, depth + 1);
    }
}

/// Escape `\` and newlines so each node stays on exactly one line (keeps the dump
/// unambiguous even if a text node contains a newline).
fn push_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
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

/// Write a deterministic UTF-8 dump of `host`'s retained tree into `out` (capacity `cap`
/// bytes), setting `*out_len` to the dump's byte length. The text is **not** NUL-terminated;
/// `*out_len` is authoritative. See [`CanopyHost::debug_snapshot`] for the format.
///
/// This is the **round-trip oracle** seam: a foreign host applies its op bytes, then asserts
/// this dump equals the tree it intended — catching structural bugs (swapped parent/child,
/// dropped class, mis-attached listener) that [`canopy_host_node_count`] cannot.
///
/// Returns [`CANOPY_OK`] with `*out_len` set to the bytes written (0 for an empty tree);
/// [`CANOPY_ERR_TOO_LARGE`] with `*out_len` set to the **needed** size if the dump does not
/// fit in `cap` (nothing is written — retry with a buffer of that size); or
/// [`CANOPY_ERR_NULL_HOST`] / [`CANOPY_ERR_NULL_DATA`].
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`]; `out_len` must be a valid
/// writable `usize`; and if the dump fits and is non-empty, `out` must point to `cap` writable
/// bytes.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_debug_snapshot(
    host: *const CanopyHost,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    if out_len.is_null() {
        return CANOPY_ERR_NULL_DATA;
    }
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; shared ref for
    // this call only.
    let host = unsafe { &*host };
    let snapshot = host.debug_snapshot();
    let bytes = snapshot.as_bytes();
    // SAFETY: `out_len` checked non-null above; per contract it is a valid writable usize.
    unsafe { *out_len = bytes.len() };
    if bytes.len() > cap {
        return CANOPY_ERR_TOO_LARGE; // needed size reported in *out_len; nothing written
    }
    if !bytes.is_empty() {
        if out.is_null() {
            return CANOPY_ERR_NULL_DATA;
        }
        // SAFETY: `out` is non-null and points to `cap >= bytes.len()` writable bytes per
        // contract; source and destination do not overlap.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    }
    CANOPY_OK
}

/// Hard cap on a render dimension (pixels): an untrusted `width`/`height` can't request a
/// multi-gigabyte framebuffer. `MAX_RENDER_DIM²·4 = 256 MiB` bounds the internal buffer.
pub const MAX_RENDER_DIM: u32 = 8192;

/// Render the current tree to an RGBA8 framebuffer (lite layout + software raster).
///
/// `out` receives `width * height * 4` bytes of row-major, straight-alpha RGBA8 pixels.
/// `*out_len` always receives the needed byte count; the **needed-size contract** mirrors
/// [`canopy_host_poll_events`] / [`canopy_host_debug_snapshot`]: pass a `cap` too small (or
/// `out` null) to size the buffer first, then call again. Returns [`CANOPY_OK`], or
/// [`CANOPY_ERR_NULL_HOST`] (null `host`) / [`CANOPY_ERR_TOO_LARGE`] (`cap` short, or a
/// dimension is zero or exceeds [`MAX_RENDER_DIM`]) / [`CANOPY_ERR_NULL_DATA`] (null `out_len`,
/// or null `out` when the frame fits) — matching [`canopy_host_poll_events`] /
/// [`canopy_host_debug_snapshot`].
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`]; `out_len` must be a valid
/// writable `usize`; and when the frame fits, `out` must point to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_render_rgba(
    host: *const CanopyHost,
    width: u32,
    height: u32,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    if out_len.is_null() {
        return CANOPY_ERR_NULL_DATA;
    }
    if width == 0 || height == 0 || width > MAX_RENDER_DIM || height > MAX_RENDER_DIM {
        return CANOPY_ERR_TOO_LARGE; // zero or out-of-range dimension; nothing written
    }
    // Bounded by MAX_RENDER_DIM² · 4, so the multiply cannot overflow usize on any target.
    let needed = (width as usize) * (height as usize) * 4;
    // SAFETY: `out_len` checked non-null above; per contract it is a valid writable usize.
    unsafe { *out_len = needed };
    if needed > cap {
        return CANOPY_ERR_TOO_LARGE; // needed size reported in *out_len; nothing written
    }
    if out.is_null() {
        return CANOPY_ERR_NULL_DATA;
    }
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; shared ref only.
    let host = unsafe { &*host };
    let rgba = host.render_rgba(width, height);
    debug_assert_eq!(rgba.len(), needed);
    // SAFETY: `out` is non-null and points to `cap >= needed` writable bytes per contract;
    // `rgba` is a fresh owned buffer of exactly `needed` bytes, so the regions don't overlap.
    unsafe { core::ptr::copy_nonoverlapping(rgba.as_ptr(), out, needed) };
    CANOPY_OK
}

/// Set the viewport (logical pixels) the tree is laid out within for hit-testing.
///
/// Call on window create/resize. Until set, the viewport is `0×0` and no node has area
/// to hit. Returns [`CANOPY_OK`], or [`CANOPY_ERR_NULL_HOST`] if `host` is null.
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`] that is not freed.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_resize(host: *mut CanopyHost, width: f32, height: f32) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; unique ref
    // for this call only.
    let host = unsafe { &mut *host };
    host.set_viewport(width, height);
    CANOPY_OK
}

/// Deliver a pointer event at `(x, y)`: hit-test the laid-out tree and, if it lands on
/// (or within) a node carrying a listener for `event` (e.g. [`CANOPY_EVENT_CLICK`]),
/// queue a `DispatchEvent` for the guest to drain with [`canopy_host_poll_events`].
///
/// `button` is the pressed button (0 = primary). `event` is the `EventKind` to match.
/// Returns the number of events queued (`0` or `1`), or a negative [`CANOPY_ERR_*`].
/// Hit geometry is the lite (inline-style) layout — see [`CanopyHost::pointer_event`]
/// for the host-side-cascade caveat.
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`] that is not freed.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_pointer(
    host: *mut CanopyHost,
    x: f32,
    y: f32,
    button: u8,
    event: u16,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; unique ref
    // for this call only.
    let host = unsafe { &mut *host };
    host.pointer_event(x, y, button, event)
}

/// Drain queued host→guest events into `out` (capacity `cap` bytes), writing the byte
/// length to `*out_len`. The drained bytes are one `canopy-protocol` batch
/// (`BeginBatch … DispatchEvent* … EndBatch`) the guest decodes with its normal reader.
///
/// Returns [`CANOPY_OK`] with `*out_len` set (0 if the queue was empty, clearing the
/// queue otherwise); [`CANOPY_ERR_TOO_LARGE`] with `*out_len` set to the **needed**
/// size if the batch does not fit in `cap` (nothing is consumed — retry with a bigger
/// buffer; [`MAX_EVENT_BATCH_BYTES`] always suffices); or [`CANOPY_ERR_NULL_HOST`] /
/// [`CANOPY_ERR_NULL_DATA`].
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`]; `out_len` must be a
/// valid writable `usize`; and if `cap > 0`, `out` must point to `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_poll_events(
    host: *mut CanopyHost,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    if out_len.is_null() {
        return CANOPY_ERR_NULL_DATA;
    }
    // Form the writable slice; tolerate a null `out` only when `cap == 0`.
    let buf: &mut [u8] = if cap == 0 {
        &mut []
    } else if out.is_null() {
        return CANOPY_ERR_NULL_DATA;
    } else {
        // SAFETY: per contract `out` points to `cap` writable bytes valid for this call.
        unsafe { core::slice::from_raw_parts_mut(out, cap) }
    };
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`.
    let host = unsafe { &mut *host };
    let (code, written) = host.poll_events_into(buf);
    // SAFETY: `out_len` checked non-null above; per contract it is a valid writable usize.
    unsafe { *out_len = written };
    code
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
    use canopy_protocol::{ElementTag, HandlerId, NodeId};

    #[test]
    fn render_rgba_rasterizes_a_styled_tree() {
        use canopy_paint::{BG, HEIGHT, WIDTH};
        // A 80×40 red card at the top-left, inline-styled — the geometry the lite layout reads.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_inline_style(card, WIDTH, "80");
        e.set_inline_style(card, HEIGHT, "40");
        e.set_inline_style(card, BG, "#ff0000");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);

        let (w, h) = (100u32, 60u32);
        let rgba = host.render_rgba(w, h);
        assert_eq!(rgba.len(), (w as usize) * (h as usize) * 4, "RGBA8, w*h*4");

        // The dark clear shows where nothing painted; the card paints red somewhere.
        let px = |x: usize, y: usize| {
            let i = (y * w as usize + x) * 4;
            (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
        };
        let (cr, cg, cb, ca) = px(10, 10); // inside the card
        assert!(
            cr > 200 && cg < 80 && cb < 80 && ca == 255,
            "card pixel is red, got {:?}",
            (cr, cg, cb, ca)
        );
        let (br, bg, bb, _) = px(95, 55); // bottom-right, outside the card -> clear
        assert!(
            br < 0x40 && bg < 0x40 && bb < 0x60,
            "corner shows the clear color, got {:?}",
            (br, bg, bb)
        );
    }

    #[test]
    fn render_rgba_extern_honors_the_needed_size_contract() {
        let batch = mounted_batch();
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&batch), CANOPY_OK);
        let (w, h) = (32u32, 16u32);
        // Probe with a too-small buffer: TOO_LARGE + needed size, nothing written.
        let mut len = 0usize;
        let code = unsafe {
            canopy_host_render_rgba(
                &host as *const CanopyHost,
                w,
                h,
                core::ptr::null_mut(),
                0,
                &mut len,
            )
        };
        assert_eq!(code, CANOPY_ERR_TOO_LARGE);
        assert_eq!(len, (w as usize) * (h as usize) * 4);
        // Now provide exactly the needed buffer.
        let mut buf = vec![0u8; len];
        let code = unsafe {
            canopy_host_render_rgba(
                &host as *const CanopyHost,
                w,
                h,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(code, CANOPY_OK);
        assert_eq!(len, buf.len());
        // A zero dimension and an over-large dimension are both rejected.
        assert_eq!(
            unsafe {
                canopy_host_render_rgba(
                    &host as *const CanopyHost,
                    0,
                    h,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            },
            CANOPY_ERR_TOO_LARGE
        );
        assert_eq!(
            unsafe {
                canopy_host_render_rgba(
                    &host as *const CanopyHost,
                    MAX_RENDER_DIM + 1,
                    h,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            },
            CANOPY_ERR_TOO_LARGE
        );
    }

    #[test]
    fn render_rgba_null_out_len_matches_the_sibling_needed_size_fns() {
        // The render fn's doc says its needed-size contract "mirrors
        // canopy_host_poll_events / canopy_host_debug_snapshot", and canopy.h lists
        // CANOPY_ERR_NULL_DATA for the null out-pointer family. Both sibling fns return
        // CANOPY_ERR_NULL_DATA when the `out_len` out-param is null; the render fn must
        // agree so a C caller can branch on one code for a null output pointer.
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&mounted_batch()), CANOPY_OK);
        let host_ptr: *mut CanopyHost = &mut host;

        // Sibling 1: debug_snapshot with a null out_len.
        let snap_code = unsafe {
            canopy_host_debug_snapshot(host_ptr, core::ptr::null_mut(), 0, core::ptr::null_mut())
        };
        // Sibling 2: poll_events with a null out_len.
        let poll_code = unsafe {
            canopy_host_poll_events(host_ptr, core::ptr::null_mut(), 0, core::ptr::null_mut())
        };
        // The render fn with a null out_len.
        let render_code = unsafe {
            canopy_host_render_rgba(
                host_ptr,
                32,
                16,
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
            )
        };

        assert_eq!(
            snap_code, CANOPY_ERR_NULL_DATA,
            "debug_snapshot: null out_len"
        );
        assert_eq!(poll_code, CANOPY_ERR_NULL_DATA, "poll_events: null out_len");
        assert_eq!(
            render_code, snap_code,
            "render_rgba must report the same null-out-param code as its sibling needed-size fns"
        );
    }

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
    fn cyclic_batch_is_rejected_and_the_host_stays_renderable() {
        // A crafted op batch that tries to form a parent/child cycle (A->B, then B->A) must be
        // rejected by the Dom as BadHandle through the real C entry point — NOT crash the host
        // by sending layout/hit-test into infinite recursion. The host must stay usable after.
        let mut e = Emitter::new();
        let a = e.create_element(ElementTag::new(1));
        let b = e.create_element(ElementTag::new(1));
        e.append(ROOT, a);
        e.append(a, b);
        e.append(b, a); // the cycle op
        let batch = e.take_batch(0);

        let mut host = CanopyHost::new();
        assert_eq!(
            apply_via_c(&mut host as *mut CanopyHost, &batch),
            CANOPY_ERR_BAD_HANDLE,
            "the cycle-forming op is rejected, not applied"
        );
        // The host survived and is acyclic: both walkers terminate rather than overflow.
        let rgba = host.render_rgba(64, 48);
        assert_eq!(
            rgba.len(),
            64 * 48 * 4,
            "render terminates and returns a full frame"
        );
        host.set_viewport(64.0, 48.0);
        let _ = host.pointer_event(1.0, 1.0, 0, 1); // hit-test must return, not diverge
        assert_eq!(host.node_count(), 2, "the acyclic prefix (A, B) is intact");
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

        // snapshot on null returns the null-host code before any deref.
        let mut snap_len = 0usize;
        // SAFETY: a null host is explicitly handled before `out`/`out_len` are touched.
        let snap_rc = unsafe {
            canopy_host_debug_snapshot(core::ptr::null(), core::ptr::null_mut(), 0, &mut snap_len)
        };
        assert_eq!(snap_rc, CANOPY_ERR_NULL_HOST);

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

    #[test]
    fn debug_snapshot_renders_the_tree_deterministically() {
        use canopy_view::CLICK;
        // column.card  >  button(on click → handler 0)  >  text "Click"
        let mut e = Emitter::new();
        let col = e.create_element(ElementTag::new(1));
        e.append(ROOT, col);
        e.set_class(col, "card");
        let btn = e.create_element(ElementTag::new(3));
        e.append(col, btn);
        e.add_listener(btn, CLICK, HandlerId::new(0));
        let label = e.create_text("Click");
        e.append(btn, label);

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);

        let expected = "el tag=1 class=card\n  el tag=3 on=1:0\n    text=Click\n";
        assert_eq!(host.debug_snapshot(), expected, "the safe-path dump");

        // The C buffer-fill path agrees and reports the exact byte length.
        let mut buf = [0u8; 256];
        let mut len = 0usize;
        // SAFETY: `host` is a live local; `buf`/`len` are valid writable storage for the call.
        let code = unsafe {
            canopy_host_debug_snapshot(
                &host as *const CanopyHost,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(code, CANOPY_OK);
        assert_eq!(&buf[..len], expected.as_bytes(), "the C path matches");
    }

    #[test]
    fn debug_snapshot_reports_needed_size_without_writing() {
        let mut host = CanopyHost::new();
        host.apply_bytes(&mounted_batch()); // column + text "hello"
        let full = host.debug_snapshot();
        assert!(!full.is_empty());

        // A 1-byte buffer cannot hold it: report the needed size, write nothing.
        let mut tiny = [0u8; 1];
        let mut len = 0usize;
        // SAFETY: `host` is live; the function reports the needed size before any write.
        let code = unsafe {
            canopy_host_debug_snapshot(
                &host as *const CanopyHost,
                tiny.as_mut_ptr(),
                tiny.len(),
                &mut len,
            )
        };
        assert_eq!(code, CANOPY_ERR_TOO_LARGE);
        assert_eq!(len, full.len(), "needed size is the full dump length");
        assert_eq!(tiny, [0u8; 1], "nothing was written");
    }

    /// A 100×40 button at the top-left with a CLICK listener (handler 7), as inline-
    /// styled op bytes — the geometry the lite hit-test reads.
    fn button_with_click() -> (Vec<u8>, NodeId, HandlerId) {
        use canopy_paint::{HEIGHT, WIDTH};
        use canopy_view::CLICK;
        let handler = HandlerId::new(7);
        let mut e = Emitter::new();
        let btn = e.create_element(ElementTag::new(3));
        e.append(ROOT, btn);
        e.set_inline_style(btn, WIDTH, "100");
        e.set_inline_style(btn, HEIGHT, "40");
        e.add_listener(btn, CLICK, handler);
        (e.take_batch(0), btn, handler)
    }

    #[test]
    fn pointer_hit_test_queues_and_drains_a_dispatch_event() {
        use canopy_protocol::{EventPayload, Op, OpReader};
        use canopy_view::CLICK;

        let (batch, btn, handler) = button_with_click();
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&batch), CANOPY_OK);
        host.set_viewport(200.0, 200.0);

        // Inside the button → one event queued; outside → none.
        assert_eq!(host.pointer_event(10.0, 10.0, 0, CLICK.raw()), 1);
        assert_eq!(host.pointer_event(150.0, 150.0, 0, CLICK.raw()), 0);

        // Drain and decode the host→guest batch.
        let mut out = [0u8; 256];
        let (code, n) = host.poll_events_into(&mut out);
        assert_eq!(code, CANOPY_OK);
        assert!(n > 0, "a non-empty event batch was drained");

        let ops: Vec<Op> = OpReader::new(&out[..n]).map(|r| r.unwrap()).collect();
        let (h, node, payload) = ops
            .iter()
            .find_map(|op| match op {
                Op::DispatchEvent {
                    handler,
                    node,
                    payload,
                } => Some((*handler, *node, payload)),
                _ => None,
            })
            .expect("a DispatchEvent in the drained batch");
        assert_eq!(h, handler, "the button's click handler");
        assert_eq!(node, btn, "the hit node");
        assert!(
            matches!(payload, EventPayload::Pointer { button: 0, .. }),
            "a pointer payload with the primary button"
        );

        // The queue is now empty: a second poll yields nothing.
        assert_eq!(host.poll_events_into(&mut out), (CANOPY_OK, 0));
    }

    #[test]
    fn poll_events_reports_needed_size_without_consuming() {
        use canopy_view::CLICK;
        let (batch, _btn, _h) = button_with_click();
        let mut host = CanopyHost::new();
        host.apply_bytes(&batch);
        host.set_viewport(200.0, 200.0);
        assert_eq!(host.pointer_event(10.0, 10.0, 0, CLICK.raw()), 1);

        // A 4-byte buffer cannot hold the batch: report the needed size, consume nothing.
        let mut tiny = [0u8; 4];
        let (code, needed) = host.poll_events_into(&mut tiny);
        assert_eq!(code, CANOPY_ERR_TOO_LARGE);
        assert!(needed > 4, "the needed size is reported");

        // Still queued — a big enough buffer drains it.
        let mut out = [0u8; 256];
        let (code2, n) = host.poll_events_into(&mut out);
        assert_eq!(code2, CANOPY_OK);
        assert!(n > 0);
    }

    #[test]
    fn event_fns_tolerate_a_null_host() {
        // SAFETY: a null host is a documented, handled input for every event fn.
        unsafe {
            assert_eq!(
                canopy_host_resize(core::ptr::null_mut(), 1.0, 1.0),
                CANOPY_ERR_NULL_HOST
            );
            assert_eq!(
                canopy_host_pointer(core::ptr::null_mut(), 0.0, 0.0, 0, 1),
                CANOPY_ERR_NULL_HOST
            );
            let mut len = 0usize;
            assert_eq!(
                canopy_host_poll_events(core::ptr::null_mut(), core::ptr::null_mut(), 0, &mut len),
                CANOPY_ERR_NULL_HOST
            );
        }
    }
}
