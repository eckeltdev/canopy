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

use std::collections::BTreeMap;

use canopy_dom::{Dom, ROOT};
use canopy_protocol::{AttrId, EventKind, EventPayload, NodeId, Op, OpEncoder, PropId};
use canopy_render_soft::SoftwareRenderer;
use canopy_style_css::{ElementIdentity, ElementStates, MatchTarget, Stylesheet};
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
    /// An optional CSS-lite class stylesheet (set via [`canopy_host_set_stylesheet`]). When
    /// present, layout/render/hit-test run against a cascaded *clone* of the tree (a node's
    /// classes resolve to inline styles, author inline winning) — so `class`-styled trees paint
    /// without the guest expanding any CSS. `None` = inline-only styling.
    stylesheet: Option<Stylesheet>,
    /// The node currently under the pointer (set via [`canopy_host_hover`]); its `:hover` rules
    /// (and its ancestors') apply during the cascade. `None` when the pointer is outside the tree.
    hovered: Option<NodeId>,
    /// The node that currently has keyboard focus (set via [`CanopyHost::set_focus`]); its `:focus`
    /// rules apply during the cascade. `None` when nothing is focused.
    focused: Option<NodeId>,
    /// The node currently being activated/pressed (set via [`CanopyHost::set_active`]); its
    /// `:active` rules apply during the cascade. `None` when nothing is active.
    active: Option<NodeId>,
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
            stylesheet: None,
            hovered: None,
            focused: None,
            active: None,
        }
    }

    /// Install a CSS-lite class stylesheet (`.class { prop: value }` rules). Parsed once and
    /// stored; subsequent render/hit-test runs cascade each node's classes through it. Passing
    /// an empty string clears any current stylesheet (back to inline-only styling).
    pub fn set_stylesheet(&mut self, css: &str) {
        self.stylesheet = if css.trim().is_empty() {
            None
        } else {
            Some(canopy_style_css::parse(css))
        };
    }

    /// The tree to lay out / paint / hit-test: the host's own `Dom` when there is no stylesheet,
    /// or a *clone* with each node's resolved CSS-class declarations folded in as inline styles
    /// (author inline wins, per CSS specificity). Non-destructive — the host's `Dom`, its node
    /// count, and the debug snapshot stay exactly what the guest authored.
    fn styled_dom(&self) -> Option<Dom> {
        let sheet = self.stylesheet.as_ref()?;
        // The `:hover` chain: a node matches `:hover` when the pointer is over it OR a descendant,
        // so the hovered leaf and every ancestor up to the root are "hovered" (CSS semantics).
        let hover_path = self.hover_path();
        let mut dom = self.dom.clone();
        // Collect (node, prop, value) first so the immutable walk doesn't overlap the mutation.
        let mut overlay: Vec<(NodeId, PropId, String)> = Vec::new();
        // Ordered top-down traversal (parent before its children) carrying the inherited-property
        // map down the tree, so a child can take its parent's resolved value for any inherited prop.
        // The root's children start from an empty map (no Dom-root defaults to seed). The root's
        // children are siblings of each other, so each carries its index + the sibling count so
        // structural pseudo-classes (`:first-child`, `:nth-child`, …) resolve on the host path.
        let root_children = dom.children(ROOT);
        let root_count = root_children.len() as u32;
        for (idx, &child) in root_children.iter().enumerate() {
            collect_cascade(
                &dom,
                sheet,
                &hover_path,
                self.focused,
                self.active,
                child,
                idx as u32,
                root_count,
                &BTreeMap::new(),
                &[],
                &mut overlay,
            );
        }
        for (node, prop, value) in overlay {
            dom.set_inline_style(node, prop, value);
        }
        Some(dom)
    }

    /// The chain of node ids from the hovered leaf ([`Self::hovered`]) up to the root, i.e. every
    /// node a `:hover` rule should match (the leaf under the pointer and all its ancestors). Empty
    /// when nothing is hovered. A short, shallow path, so a `Vec` + linear `contains` is plenty.
    fn hover_path(&self) -> Vec<NodeId> {
        let mut path = Vec::new();
        let mut cur = self.hovered;
        while let Some(node) = cur {
            path.push(node);
            cur = self.dom.node(node).and_then(|n| n.parent);
        }
        path
    }

    /// Update which node is under the pointer at `(x, y)` for `:hover` styling. Hit-tests the
    /// styled geometry (so hover tracks what is painted) and records the topmost node, or `None`
    /// outside the tree. Returns `true` if the hovered node changed — the caller should re-render
    /// (and may skip the render when it returns `false`).
    pub fn set_hover(&mut self, x: f32, y: f32) -> bool {
        let hit = {
            let styled = self.styled_dom();
            let dom = styled.as_ref().unwrap_or(&self.dom);
            let (_scene, layout) = canopy_layout_taffy::layout(dom, self.viewport);
            canopy_layout_taffy::hit_test(&layout, Point { x, y })
        };
        if hit == self.hovered {
            return false;
        }
        self.hovered = hit;
        true
    }

    /// Set (or clear, with `None`) the node that has keyboard focus, so a later render applies its
    /// `:focus` rules. Mirrors [`set_hover`](Self::set_hover): returns `true` if the focused node
    /// changed (the caller should re-render to reflect it) and `false` if it did not (the render can
    /// be skipped). Unlike hover, focus is not hit-tested off a coordinate — the caller decides
    /// which node is focused (e.g. on a tab/click) and names it here.
    pub fn set_focus(&mut self, node: Option<NodeId>) -> bool {
        if node == self.focused {
            return false;
        }
        self.focused = node;
        true
    }

    /// Set (or clear, with `None`) the node being activated (pressed), so a later render applies its
    /// `:active` rules. Mirrors [`set_hover`](Self::set_hover)/[`set_focus`](Self::set_focus):
    /// returns `true` if the active node changed (re-render) and `false` if it did not.
    pub fn set_active(&mut self, node: Option<NodeId>) -> bool {
        if node == self.active {
            return false;
        }
        self.active = node;
        true
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
        // Hit-test the cascaded tree's geometry (class styles can supply width/height), so what
        // is clicked matches what was painted. Listener lookup below uses the original tree —
        // same node ids, same structure — so the cascade only affects geometry, not handlers.
        let styled = self.styled_dom();
        let dom = styled.as_ref().unwrap_or(&self.dom);
        let (_scene, layout) = canopy_layout_taffy::layout(dom, self.viewport);
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
        // Lay out the cascaded tree (class styles folded in) when a stylesheet is set, else the
        // host's own inline-styled tree.
        let styled = self.styled_dom();
        let dom = styled.as_ref().unwrap_or(&self.dom);
        let (scene, _layout) = canopy_layout_taffy::layout(dom, viewport);
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

/// The CSS type/tag name a node matches against in the lite cascade. Prefers the
/// guest-declared [`canopy_dom::Node::tag_name`] (the capable tiers set it); otherwise
/// falls back to the canonical name of the well-known [`canopy_protocol::ElementTag`] the
/// reference host assigns, so a constrained author who only emitted `CreateElement(BUTTON)`
/// still matches a `button { … }` rule. Text nodes (no tag, no name) → `None`.
fn element_type_name(node: &canopy_dom::Node) -> Option<&str> {
    if let Some(name) = node.tag_name.as_deref() {
        return Some(name);
    }
    // The reference-host ElementTag ids (canopy-view): COLUMN=1, ROW=2, BUTTON=3, INPUT=4.
    // COLUMN is the generic flex/block container, so its CSS name is the familiar `div`.
    Some(match node.tag?.raw() {
        1 => "div",
        2 => "row",
        3 => "button",
        4 => "input",
        _ => return None,
    })
}

/// Walk `node`'s subtree top-down (parent before children), folding the lite cascade into
/// `overlay`, threading inherited properties down through `inherited`, and threading the
/// **ancestor stack** (`ancestors`, root-first) down so descendant/child combinators and the
/// node's attribute pairs join its [`MatchTarget`]. The interaction-state pseudo-classes are fed
/// from `hover_path` (every node on the hovered-leaf-to-root chain matches `:hover`) and the
/// `focused` / `active` node ids (the focused / active node itself matches `:focus` / `:active`);
/// `:disabled` / `:checked` resolve off the node's own attributes, needing no state here.
/// `sibling_index` (0-based) and `sibling_count`
/// give the node's position among its siblings; together with its own child count they are threaded
/// via [`MatchTarget::with_structure`] so the structural pseudo-classes (`:first-child`,
/// `:nth-child`, `:empty`, …) resolve on the host path.
///
/// For each node, in CSS source order of weakening strength:
/// 1. compute its **own resolved styles** = its author-inline styles ([`canopy_dom::Node::styles`])
///    plus the matched-rule decls from [`Stylesheet::resolve_for`] it doesn't already set
///    (author inline wins over matched rules);
/// 2. for every prop in the incoming `inherited` map the node does **not** resolve itself, push it
///    onto the overlay — the node inherits the parent's value (inheritance is the weakest source,
///    so it only fills what nothing else set);
/// 3. build the inherited map for **this** node's children: the incoming map overlaid with this
///    node's own resolved values for inherited props (per [`canopy_paint::is_inherited`]);
/// 4. recurse into the children with that map.
///
/// This runs over an immutable clone so the collected `overlay` can be applied mutably afterwards
/// without overlapping the walk (the caller's two-phase split).
#[allow(clippy::too_many_arguments)]
fn collect_cascade(
    dom: &Dom,
    sheet: &Stylesheet,
    hover_path: &[NodeId],
    focused: Option<NodeId>,
    active: Option<NodeId>,
    node: NodeId,
    sibling_index: u32,
    sibling_count: u32,
    inherited: &BTreeMap<PropId, String>,
    ancestors: &[NodeIdentity<'_>],
    overlay: &mut Vec<(NodeId, PropId, String)>,
) {
    let Some(n) = dom.node(node) else { return };

    // (1) The node's own resolved styles: author inline first, then matched-rule decls it didn't
    // set itself. Keyed by PropId so a child can look up "did I resolve this myself?" in step (2).
    let mut own: BTreeMap<PropId, String> = n.styles.clone();
    // This node's identity (type / id / class / attrs), borrowing strings from the cloned Dom that
    // lives for the whole walk. Built up-front so it can both drive this node's match AND be pushed
    // onto the ancestor stack handed to the children.
    let identity = NodeIdentity::from_node(n);
    // Match the full lite selector model (type / id / class / compound / attr / combinators, with
    // specificity) against this node's identity and its ancestor chain. Skip bare text nodes: they
    // aren't elements, so only an element-ish node (a tag, a declared name, classes, an id, or any
    // attr) participates in matching.
    if identity.type_name.is_some()
        || identity.id.is_some()
        || !identity.classes.is_empty()
        || !identity.attrs.is_empty()
    {
        // The matcher wants ancestors nearest-first (index 0 = parent); our `ancestors` stack is
        // root-first, so reverse it into borrowed `ElementIdentity`s for this resolve call.
        let chain: Vec<ElementIdentity> = ancestors.iter().rev().map(|a| a.as_element()).collect();
        // Thread this node's structural context so `:first-child`/`:nth-child`/`:empty`/… resolve:
        // its 0-based index among its siblings, the total sibling count, and its own child count.
        let target = identity
            .as_match_target()
            .with_attrs(&identity.attrs)
            .with_ancestors(&chain)
            .with_structure(sibling_index, sibling_count, n.children.len() as u32);
        // This node's current dynamic interaction state, fed to the state pseudos
        // (`:hover`/`:focus`/`:active`). `:hover` fires for the node and every ancestor of the
        // hovered leaf (the hover_path, CSS semantics); `:focus`/`:active` fire only on the focused
        // / active node itself (no ancestor walk this wave). `:disabled`/`:checked` are NOT here —
        // they match off this node's `disabled`/`checked` attribute via `target` instead.
        let states = ElementStates {
            hover: hover_path.contains(&node),
            focus: focused == Some(node),
            active: active == Some(node),
        };
        for (prop, value) in sheet.resolve_for(&target, states) {
            // Author inline styles win over class rules (CSS specificity: inline beats a class
            // selector), so only fold in a property the node didn't set itself. The matched-rule
            // value is overlaid onto the live tree below; record it in `own` too so it shadows any
            // inherited value of the same prop and propagates to this node's children.
            if !n.styles.contains_key(&prop) {
                overlay.push((node, prop, value.clone()));
                own.insert(prop, value);
            }
        }
    }

    // (2) Inherit: for every inherited prop the node does not resolve itself, take the parent's
    // value. Inheritance is the weakest source — it only fills what neither inline nor a matched
    // rule set (both already live in `own`).
    for (prop, value) in inherited {
        if !own.contains_key(prop) {
            overlay.push((node, *prop, value.clone()));
        }
    }

    // (3) The inherited map to pass to THIS node's children: the incoming map overlaid with this
    // node's own resolved values for the props that inherit (per `canopy_paint::is_inherited`).
    let mut child_inherited = inherited.clone();
    for (prop, value) in &own {
        if canopy_paint::is_inherited(*prop) {
            child_inherited.insert(*prop, value.clone());
        }
    }

    // (4) Recurse, processing each child after this parent (top-down order). Extend the ancestor
    // stack (root-first) with THIS node's identity, so each child sees its full chain. A bare text
    // node carries no identity worth matching, but pushing it keeps the chain structurally complete
    // (it simply never matches a compound).
    let mut child_ancestors: Vec<NodeIdentity> = ancestors.to_vec();
    child_ancestors.push(identity);
    // Each child knows its 0-based index among its siblings and the total sibling count, so its own
    // structural pseudo-classes resolve when it is visited.
    let child_count = n.children.len() as u32;
    for (idx, &child) in n.children.iter().enumerate() {
        collect_cascade(
            dom,
            sheet,
            hover_path,
            focused,
            active,
            child,
            idx as u32,
            child_count,
            &child_inherited,
            &child_ancestors,
            overlay,
        );
    }
}

/// The reference-host **attribute** ids whose CSS names the lite cascade understands, beyond the
/// well-known [`AttrId::ID`] (`1` → `"id"`). These mirror the hardcoded ElementTag→name table in
/// [`element_type_name`]: there is still no general attr-name registry, but the two
/// interaction-state attribute pseudos `:disabled` / `:checked` need their attributes exposed under
/// a CSS name, so a constrained author who emits `SetAttribute(DISABLED_ATTR, …)` matches a
/// `:disabled` rule. (`:disabled` / `:checked` are presence tests — the attribute's value is
/// ignored, exactly as in CSS.)
const DISABLED_ATTR: AttrId = AttrId::new(2);
/// See [`DISABLED_ATTR`]: the reference-host attribute id whose CSS name is `"checked"`.
const CHECKED_ATTR: AttrId = AttrId::new(3);

/// One node's borrowed identity for selector matching, owning the small `Vec`s that back the
/// `&[&str]` / `&[(&str, &str)]` slices [`ElementIdentity`] needs. Strings are borrowed from the
/// cloned `Dom`, which outlives the whole cascade walk, so an instance can sit on the ancestor
/// stack and be re-borrowed for each descendant's `resolve_for`.
struct NodeIdentity<'a> {
    type_name: Option<&'a str>,
    id: Option<&'a str>,
    classes: Vec<&'a str>,
    /// The CSS attribute `(name, value)` pairs. The well-known **id** attribute (`"id"`) plus the
    /// interaction-state attributes **disabled** / **checked** (the reference-host ids
    /// [`DISABLED_ATTR`] / [`CHECKED_ATTR`], surfaced for the `:disabled` / `:checked` pseudos) have
    /// known CSS names; other host-minted numeric attrs have no name mapping yet, so they are not
    /// exposed to attribute selectors (a documented limitation).
    attrs: Vec<(&'a str, &'a str)>,
}

impl<'a> NodeIdentity<'a> {
    /// Derive a node's identity from its retained [`canopy_dom::Node`].
    fn from_node(n: &'a canopy_dom::Node) -> Self {
        let id = n.attrs.get(&AttrId::ID).map(String::as_str);
        // Expose the id attribute under its CSS name so `[id="x"]` / `[id^="…"]` selectors work,
        // and the disabled/checked attributes under theirs so `:disabled` / `:checked` (which fold
        // to a `disabled` / `checked` attribute-presence test) match. Other attrs have no CSS-name
        // mapping (no registry), so they are intentionally omitted.
        let mut attrs: Vec<(&str, &str)> = id.map(|v| ("id", v)).into_iter().collect();
        if let Some(v) = n.attrs.get(&DISABLED_ATTR) {
            attrs.push(("disabled", v.as_str()));
        }
        if let Some(v) = n.attrs.get(&CHECKED_ATTR) {
            attrs.push(("checked", v.as_str()));
        }
        Self {
            type_name: element_type_name(n),
            id,
            classes: n.classes.iter().map(String::as_str).collect(),
            attrs,
        }
    }

    /// A [`MatchTarget`] for this node's own identity (type/id/classes), without attrs/ancestors —
    /// the caller layers those on with the builder methods.
    fn as_match_target(&self) -> MatchTarget<'_> {
        MatchTarget::new(self.type_name, self.id, &self.classes)
    }

    /// Borrow this identity as an [`ElementIdentity`] (for an ancestor in another node's chain). An
    /// ancestor's structural context is not threaded (structural pseudos resolve only on the
    /// subject node here), so it carries the default [`canopy_style_css::StructInfo::UNKNOWN`].
    fn as_element(&self) -> ElementIdentity<'_> {
        ElementIdentity {
            type_name: self.type_name,
            id: self.id,
            classes: &self.classes,
            attrs: &self.attrs,
            structure: canopy_style_css::StructInfo::UNKNOWN,
        }
    }
}

// `NodeIdentity` is cloneable so the ancestor stack can be extended per child (the inner `&str`s
// are cheap `Copy` borrows into the long-lived cloned Dom).
impl Clone for NodeIdentity<'_> {
    fn clone(&self) -> Self {
        Self {
            type_name: self.type_name,
            id: self.id,
            classes: self.classes.clone(),
            attrs: self.attrs.clone(),
        }
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

/// Install a CSS-lite class stylesheet on `host`: `len` UTF-8 bytes of `.class { prop: value }`
/// rules. Subsequent `canopy_host_render_rgba` / `canopy_host_pointer` cascade each node's
/// classes through it (the guest just emits `SetClass`; the host expands the CSS), with author
/// inline styles winning over class rules. A `len` of 0 clears any installed stylesheet.
///
/// Supported properties (the lite subset): background, color, width, height, gap, padding,
/// border-radius, direction, opacity, translate-x/y, align-items, justify-content, text-align.
/// The cascade is non-destructive — the retained tree and `canopy_host_debug_snapshot` are
/// unchanged; only layout/paint/hit-test see the resolved styles.
///
/// Returns [`CANOPY_OK`]; [`CANOPY_ERR_NULL_HOST`] (null `host`); [`CANOPY_ERR_NULL_DATA`]
/// (null `css` with `len > 0`); [`CANOPY_ERR_TOO_LARGE`] (`len` exceeds [`MAX_BATCH_BYTES`]);
/// or [`CANOPY_ERR_DECODE`] (the bytes are not valid UTF-8).
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`]; if `len > 0`, `css` must
/// point to `len` readable bytes valid for the call.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_set_stylesheet(
    host: *mut CanopyHost,
    css: *const u8,
    len: usize,
) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    if len > MAX_BATCH_BYTES {
        return CANOPY_ERR_TOO_LARGE; // the host never sizes a parse from an untrusted length
    }
    let text = if len == 0 {
        ""
    } else if css.is_null() {
        return CANOPY_ERR_NULL_DATA;
    } else {
        // SAFETY: `css` is non-null and points to `len` (<= MAX_BATCH_BYTES) readable bytes
        // per the documented contract; the slice is only read for this call.
        let bytes = unsafe { core::slice::from_raw_parts(css, len) };
        match core::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return CANOPY_ERR_DECODE,
        }
    };
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; unique ref for this
    // call. `set_stylesheet` parses `text` into an owned Stylesheet, so it need not outlive it.
    let host = unsafe { &mut *host };
    host.set_stylesheet(text);
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

/// Update which node is under the pointer at `(x, y)` for `:hover` styling, so a later
/// [`canopy_host_render_rgba`] applies the node's (and its ancestors') `:hover` rules. Feed this
/// on pointer move. Returns `1` if the hovered node changed (re-render to reflect it), `0` if it
/// did not (the caller can skip the render), or [`CANOPY_ERR_NULL_HOST`] if `host` is null.
///
/// # Safety
///
/// `host` must be null or a live pointer from [`canopy_host_new`] that has not been freed.
#[no_mangle]
pub unsafe extern "C" fn canopy_host_hover(host: *mut CanopyHost, x: f32, y: f32) -> i32 {
    if host.is_null() {
        return CANOPY_ERR_NULL_HOST;
    }
    // SAFETY: `host` is non-null and a live pointer from `canopy_host_new`; unique ref for this
    // call only.
    let host = unsafe { &mut *host };
    i32::from(host.set_hover(x, y))
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
    fn a_class_stylesheet_styles_the_render_without_mutating_the_tree() {
        // The node carries ONLY a class; the stylesheet supplies its geometry + color. The
        // host-side cascade folds the class rules in for layout/paint — but does NOT touch the
        // retained tree (the snapshot stays structural), so authoring + parity stay byte-exact.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_class(card, "card");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);

        let red = |buf: &[u8], at: (usize, usize)| {
            let i = (at.1 * 100 + at.0) * 4;
            buf[i] > 200 && buf[i + 1] < 80 && buf[i + 2] < 80
        };

        // No stylesheet yet: the class has no effect, nothing paints the card -> no red anywhere.
        let bare = host.render_rgba(100, 60);
        assert!(
            !bare
                .chunks_exact(4)
                .any(|p| p[0] > 200 && p[1] < 80 && p[2] < 80),
            "no red before"
        );

        host.set_stylesheet(".card { width: 80; height: 40; background: #ff0000 }");
        let styled = host.render_rgba(100, 60);
        assert!(
            red(&styled, (10, 10)),
            "the class-styled card paints red where it lays out"
        );
        assert!(
            !red(&styled, (95, 55)),
            "the clear shows outside the 80x40 card"
        );

        // The cascade is non-destructive: the tree is still `class=card` with NO inline style.
        assert_eq!(
            host.debug_snapshot(),
            "el tag=1 class=card\n",
            "tree unchanged by the cascade"
        );

        // Clearing the stylesheet goes back to inline-only (no red).
        host.set_stylesheet("");
        assert!(
            !host
                .render_rgba(100, 60)
                .chunks_exact(4)
                .any(|p| p[0] > 200 && p[1] < 80 && p[2] < 80),
            "clearing the stylesheet drops the class styling"
        );
    }

    #[test]
    fn end_to_end_overflow_hidden_masks_an_overflowing_child() {
        // A 40x40 `overflow: hidden` box with a 100x100 red child: the child paints inside the box
        // but is clipped to it. Proves the full chain — overflow parsed (style-css), a PushClip/
        // PopClip emitted around the child (layout-taffy), and the clip stack masks pixels outside
        // the box (render-soft).
        let mut e = Emitter::new();
        let clip = e.create_element(ElementTag::new(1));
        e.append(ROOT, clip);
        e.set_attribute(clip, AttrId::ID, "clip");
        let child = e.create_element(ElementTag::new(1));
        e.append(clip, child);
        e.set_class(child, "big");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_viewport(100.0, 100.0);
        host.set_stylesheet(
            "#clip { width: 40; height: 40; overflow: hidden } \
             .big  { width: 100; height: 100; background: #ff0000 }",
        );

        let buf = host.render_rgba(100, 100);
        let is_red = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            buf[i] > 200 && buf[i + 1] < 80 && buf[i + 2] < 80
        };
        assert!(is_red(20, 20), "child paints inside the 40x40 clip box");
        assert!(!is_red(60, 60), "child is masked outside the clip box");
    }

    #[test]
    fn hover_restyles_the_node_under_the_pointer() {
        // A button styled by class: `.btn` is blue, `.btn:hover` is red. Moving the pointer over
        // it must apply `:hover` (red); moving away reverts (blue).
        let mut e = Emitter::new();
        let btn = e.create_element(ElementTag::new(3)); // button
        e.append(ROOT, btn);
        e.set_class(btn, "btn");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_viewport(100.0, 60.0);
        host.set_stylesheet(
            ".btn { width: 100; height: 60; background: #0000ff } .btn:hover { background: #ff0000 }",
        );

        let center = (50usize, 30usize);
        let rgb = |buf: &[u8], at: (usize, usize)| {
            let i = (at.1 * 100 + at.0) * 4;
            (buf[i], buf[i + 1], buf[i + 2])
        };
        let is_blue = |c: (u8, u8, u8)| c.0 < 80 && c.1 < 80 && c.2 > 200;
        let is_red = |c: (u8, u8, u8)| c.0 > 200 && c.1 < 80 && c.2 < 80;

        // Pointer outside: base `.btn` is blue.
        assert!(
            is_blue(rgb(&host.render_rgba(100, 60), center)),
            "base .btn is blue"
        );

        // Move over the button: hover changed -> re-render shows `.btn:hover` red.
        assert!(
            host.set_hover(50.0, 30.0),
            "set_hover reports the hovered node changed"
        );
        assert!(
            is_red(rgb(&host.render_rgba(100, 60), center)),
            ".btn:hover paints red"
        );

        // Move off the button: hover changed back -> blue again.
        assert!(host.set_hover(500.0, 500.0), "hover left the button");
        assert!(
            is_blue(rgb(&host.render_rgba(100, 60), center)),
            "reverts to base blue"
        );

        // Staying off the button: no change, so the caller can skip the render.
        assert!(
            !host.set_hover(500.0, 500.0),
            "no change reported when hover stays out"
        );
    }

    // --- Wave 3c: interaction-state pseudos (:focus/:active/:disabled/:checked) -------------
    // (reuses the `styled_prop` helper defined later in this test module)

    /// Build a host with a single `.btn` button under ROOT and the given `css`, returning the host
    /// plus the button node id.
    fn host_with_btn(css: &str) -> (CanopyHost, NodeId) {
        let mut e = Emitter::new();
        let btn = e.create_element(ElementTag::new(3)); // button
        e.append(ROOT, btn);
        e.set_class(btn, "btn");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(css);
        (host, btn)
    }

    #[test]
    fn focus_rule_applies_after_set_focus_and_reverts_after_clear() {
        use canopy_paint::BG;
        // `.btn:focus` styles the button only while it is the focused node.
        let (mut host, btn) =
            host_with_btn(".btn { background: #313244 } .btn:focus { background: #89b4fa }");

        // No focus yet: base only.
        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(styled_prop(&styled, btn, BG).as_deref(), Some("#313244"));

        // Focus the button -> :focus applies (and the host reports the change).
        assert!(host.set_focus(Some(btn)), "focus changed -> re-render");
        let styled = host.styled_dom().unwrap();
        assert_eq!(
            styled_prop(&styled, btn, BG).as_deref(),
            Some("#89b4fa"),
            ".btn:focus restyles the focused node"
        );

        // Focusing the same node again is a no-op (no re-render needed).
        assert!(
            !host.set_focus(Some(btn)),
            "no change when focus is unchanged"
        );

        // Clear focus -> reverts to the base.
        assert!(host.set_focus(None), "focus left the button -> re-render");
        let styled = host.styled_dom().unwrap();
        assert_eq!(
            styled_prop(&styled, btn, BG).as_deref(),
            Some("#313244"),
            "clearing focus reverts to the base background"
        );
    }

    #[test]
    fn active_rule_applies_after_set_active_and_reverts() {
        use canopy_paint::BG;
        let (mut host, btn) =
            host_with_btn(".btn { background: #313244 } .btn:active { background: #f38ba8 }");

        assert!(host.set_active(Some(btn)));
        let styled = host.styled_dom().unwrap();
        assert_eq!(
            styled_prop(&styled, btn, BG).as_deref(),
            Some("#f38ba8"),
            ".btn:active restyles the active node"
        );

        assert!(host.set_active(None));
        let styled = host.styled_dom().unwrap();
        assert_eq!(
            styled_prop(&styled, btn, BG).as_deref(),
            Some("#313244"),
            "clearing active reverts to the base"
        );
    }

    #[test]
    fn disabled_pseudo_matches_a_node_with_a_disabled_attribute_no_host_state() {
        use canopy_paint::BG;
        // `:disabled` is attribute-driven: a node carrying the `disabled` attribute matches with NO
        // host state set, exactly like the `:disabled` CSS pseudo (the value is ignored).
        let mut e = Emitter::new();
        let on = e.create_element(ElementTag::new(4)); // input
        e.append(ROOT, on);
        let off = e.create_element(ElementTag::new(4)); // input
        e.append(ROOT, off);
        e.set_attribute(off, DISABLED_ATTR, ""); // carries the disabled attribute
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet("input:disabled { background: #585b70 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, off, BG).as_deref(),
            Some("#585b70"),
            "the input carrying a `disabled` attribute matches :disabled"
        );
        assert_eq!(
            styled_prop(&styled, on, BG),
            None,
            "the input without the attribute does not match :disabled"
        );
    }

    #[test]
    fn checked_pseudo_matches_a_node_with_a_checked_attribute() {
        use canopy_paint::FG;
        let mut e = Emitter::new();
        let checked = e.create_element(ElementTag::new(4)); // input
        e.append(ROOT, checked);
        e.set_attribute(checked, CHECKED_ATTR, "true");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet("input:checked { color: #a6e3a1 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, checked, FG).as_deref(),
            Some("#a6e3a1"),
            ":checked matches a node carrying the `checked` attribute"
        );
    }

    #[test]
    fn composed_hover_and_focus_requires_both_states() {
        use canopy_paint::BG;
        // `.btn:hover:focus` applies only when the button is BOTH hovered and focused.
        let (mut host, btn) = host_with_btn(
            ".btn { width: 100; height: 60; background: #313244 } \
             .btn:hover:focus { background: #a6e3a1 }",
        );
        host.set_viewport(100.0, 60.0);

        let bg = |host: &CanopyHost| {
            let styled = host.styled_dom().unwrap();
            styled_prop(&styled, btn, BG)
        };

        // Focus only: the composed rule does not fire.
        host.set_focus(Some(btn));
        assert_eq!(
            bg(&host).as_deref(),
            Some("#313244"),
            "focus without hover does not satisfy :hover:focus"
        );
        // Add hover (pointer over the button) -> both states set -> the composed rule fires.
        assert!(host.set_hover(50.0, 30.0), "the pointer entered the button");
        assert_eq!(
            bg(&host).as_deref(),
            Some("#a6e3a1"),
            ":hover:focus fires once the button is both hovered AND focused"
        );
        // Drop hover -> back to base even though focus remains.
        assert!(host.set_hover(500.0, 500.0), "the pointer left the button");
        assert_eq!(
            bg(&host).as_deref(),
            Some("#313244"),
            "losing hover drops the composed rule, even with focus still set"
        );
    }

    #[test]
    fn a_sheet_with_no_state_pseudos_is_unaffected_by_focus_active() {
        use canopy_paint::BG;
        // A plain class sheet (no state pseudos) must resolve identically regardless of the host's
        // focus/active state — the back-compat guarantee.
        let (mut host, btn) = host_with_btn(".btn { background: #313244 }");
        let base = host.styled_dom().and_then(|d| styled_prop(&d, btn, BG));
        host.set_focus(Some(btn));
        host.set_active(Some(btn));
        let after = host.styled_dom().and_then(|d| styled_prop(&d, btn, BG));
        assert_eq!(
            base.as_deref(),
            Some("#313244"),
            "the plain class rule resolves the base background"
        );
        assert_eq!(
            base, after,
            "focus/active leave a state-pseudo-free sheet unchanged"
        );
    }

    #[test]
    fn type_id_and_compound_selectors_resolve_through_styled_dom() {
        use canopy_paint::{BG, BORDER_COLOR, BORDER_WIDTH, MARGIN};
        // <div id="hero" class="card primary"> with a <button> child. Exercises every new
        // selector shape (type / id / compound) and all three color spellings (named, #rgb, rgb()).
        let mut e = Emitter::new();
        let div = e.create_element(ElementTag::new(1)); // COLUMN -> CSS type name "div"
        e.append(ROOT, div);
        e.set_attribute(div, AttrId::ID, "hero");
        e.set_class(div, "card");
        e.set_class(div, "primary");
        let btn = e.create_element(ElementTag::new(3)); // BUTTON -> CSS type name "button"
        e.append(div, btn);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(
            "div { background: navy } \
             #hero { border-width: 2; border-color: #f80 } \
             .card.primary { margin: 8 } \
             button { background: rgb(10, 20, 30) }",
        );

        let styled = host.styled_dom().expect("a stylesheet is set");
        fn prop(dom: &Dom, node: NodeId, p: PropId) -> Option<&str> {
            dom.node(node)
                .and_then(|n| n.styles.get(&p))
                .map(String::as_str)
        }

        // Type selector `div` and id selector `#hero` both target the container.
        assert_eq!(prop(&styled, div, BG), Some("#000080"), "div type -> navy");
        assert_eq!(
            prop(&styled, div, BORDER_WIDTH),
            Some("2"),
            "#hero id -> border-width"
        );
        assert_eq!(
            prop(&styled, div, BORDER_COLOR),
            Some("#ff8800"),
            "#hero id -> #rgb expands to #rrggbb"
        );
        // Compound `.card.primary` matches only because BOTH classes are present.
        assert_eq!(
            prop(&styled, div, MARGIN),
            Some("8"),
            "compound .card.primary -> margin"
        );
        // The `button` type selector targets the child, and rgb() normalizes to hex.
        assert_eq!(
            prop(&styled, btn, BG),
            Some("#0a141e"),
            "button type -> rgb() folds to #rrggbb"
        );
        // The div is NOT a button, so it never picked up the button rule.
        assert_eq!(prop(&styled, div, BG), Some("#000080"), "div bg unchanged");
    }

    #[test]
    fn descendant_selector_styles_a_nested_node_not_a_sibling_at_the_wrong_depth() {
        use canopy_paint::BG;
        // Tree:
        //   div.card                       (the card)
        //     div.inner                     (a wrapper)
        //       button.title  <- matches `.card .title`
        //   button.title      <- sibling of the card at ROOT depth: NO `.card` ancestor
        // `.card .title` must style the nested `.title` (any depth under a `.card`) but NOT the
        // outside `.title`, proving the ancestor chain is threaded into `resolve_for`.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1)); // div
        e.append(ROOT, card);
        e.set_class(card, "card");
        let inner = e.create_element(ElementTag::new(1)); // div
        e.append(card, inner);
        e.set_class(inner, "inner");
        let nested_title = e.create_element(ElementTag::new(3)); // button
        e.append(inner, nested_title);
        e.set_class(nested_title, "title");
        // A `.title` OUTSIDE any `.card`, at ROOT depth.
        let outside_title = e.create_element(ElementTag::new(3)); // button
        e.append(ROOT, outside_title);
        e.set_class(outside_title, "title");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".card .title { background: #abcdef }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        fn prop(dom: &Dom, node: NodeId, p: PropId) -> Option<&str> {
            dom.node(node)
                .and_then(|n| n.styles.get(&p))
                .map(String::as_str)
        }
        assert_eq!(
            prop(&styled, nested_title, BG),
            Some("#abcdef"),
            "the .title nested under a .card is styled by the descendant selector"
        );
        assert_eq!(
            prop(&styled, outside_title, BG),
            None,
            "the .title with no .card ancestor is NOT styled"
        );
    }

    #[test]
    fn child_selector_requires_the_immediate_parent() {
        use canopy_paint::BG;
        // `div > .item` matches only a `.item` whose IMMEDIATE parent is a div. A `.item` whose
        // immediate parent is a ROW element (CSS type "row", not "div") must NOT be styled — even
        // though a div (the outer container) sits higher up the chain.
        let mut e = Emitter::new();
        let outer = e.create_element(ElementTag::new(1)); // div
        e.append(ROOT, outer);
        let direct = e.create_element(ElementTag::new(1)); // div.item, immediate child of the div
        e.append(outer, direct);
        e.set_class(direct, "item");
        let row = e.create_element(ElementTag::new(2)); // row (CSS type name "row")
        e.append(outer, row);
        let deep = e.create_element(ElementTag::new(1)); // div.item, immediate parent is the row
        e.append(row, deep);
        e.set_class(deep, "item");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet("div > .item { background: #010203 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        fn prop(dom: &Dom, node: NodeId, p: PropId) -> Option<&str> {
            dom.node(node)
                .and_then(|n| n.styles.get(&p))
                .map(String::as_str)
        }
        assert_eq!(
            prop(&styled, direct, BG),
            Some("#010203"),
            "the .item that is a direct child of a div is styled"
        );
        assert_eq!(
            prop(&styled, deep, BG),
            None,
            "the .item whose immediate parent is a row is NOT styled by `div > .item`"
        );
    }

    #[test]
    fn attribute_selector_styles_the_node_by_its_id_attr() {
        use canopy_paint::BG;
        // The id attribute is exposed under its CSS name `id`, so `[id="hero"]` (an attribute
        // selector, distinct from the `#hero` id selector) styles exactly the node carrying it.
        let mut e = Emitter::new();
        let hero = e.create_element(ElementTag::new(1));
        e.append(ROOT, hero);
        e.set_attribute(hero, AttrId::ID, "hero");
        let other = e.create_element(ElementTag::new(1));
        e.append(ROOT, other);
        e.set_attribute(other, AttrId::ID, "footer");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(
            "[id=\"hero\"] { background: #111111 } [id^=\"foot\"] { background: #222222 }",
        );

        let styled = host.styled_dom().expect("a stylesheet is set");
        fn prop(dom: &Dom, node: NodeId, p: PropId) -> Option<&str> {
            dom.node(node)
                .and_then(|n| n.styles.get(&p))
                .map(String::as_str)
        }
        assert_eq!(
            prop(&styled, hero, BG),
            Some("#111111"),
            "[id=\"hero\"] exact attribute selector styles the hero node"
        );
        assert_eq!(
            prop(&styled, other, BG),
            Some("#222222"),
            "[id^=\"foot\"] prefix attribute selector styles the footer node"
        );
    }

    #[test]
    fn end_to_end_two_value_padding_shorthand_offsets_a_child() {
        // Full-stack proof that the streams compose: the CSS-lite parser expands the
        // `padding: 20 40` shorthand to per-side longhands (parser), the host folds them in
        // (cascade), Taffy lays the child out inside the asymmetric padding box (layout), and
        // the software rasterizer paints it (render). The child must land at (left=40, top=20).
        let mut e = Emitter::new();
        let outer = e.create_element(ElementTag::new(1)); // div#box
        e.append(ROOT, outer);
        e.set_attribute(outer, AttrId::ID, "box");
        let inner = e.create_element(ElementTag::new(1)); // div.inner
        e.append(outer, inner);
        e.set_class(inner, "inner");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_viewport(100.0, 100.0);
        host.set_stylesheet(
            "#box { width: 100; height: 100; background: #000080; padding: 20 40 } \
             .inner { width: 20; height: 20; background: #ff0000 }",
        );

        let buf = host.render_rgba(100, 100);
        let at = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            (buf[i], buf[i + 1], buf[i + 2])
        };
        let is_red = |c: (u8, u8, u8)| c.0 > 200 && c.1 < 80 && c.2 < 80;
        let is_navy = |c: (u8, u8, u8)| c.0 < 80 && c.1 < 80 && c.2 > 100;

        // Inner box occupies x in [40,60), y in [20,40) — inside the 40px-left / 20px-top padding.
        assert!(
            is_red(at(50, 30)),
            "child sits at the asymmetric padding offset"
        );
        // Left of the 40px left padding is still the navy container, not the child.
        assert!(is_navy(at(20, 30)), "left padding band is the container");
        // Above the 20px top padding is the container too.
        assert!(is_navy(at(50, 8)), "top padding band is the container");
    }

    #[test]
    fn end_to_end_linear_gradient_background_renders_a_ramp() {
        // Full Wave-2 chain: the parser normalizes `linear-gradient(to bottom, red, blue)` to a
        // canonical form (style-css), the layout emits a Gradient display item (layout-taffy), and
        // the software rasterizer interpolates the stops top-to-bottom (render-soft). A vertical
        // red->blue ramp must read red-dominant at the top and blue-dominant at the bottom.
        let mut e = Emitter::new();
        let bx = e.create_element(ElementTag::new(1));
        e.append(ROOT, bx);
        e.set_attribute(bx, AttrId::ID, "g");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_viewport(100.0, 100.0);
        host.set_stylesheet(
            "#g { width: 100; height: 100; background-image: linear-gradient(to bottom, red, blue) }",
        );

        let buf = host.render_rgba(100, 100);
        let at = |x: usize, y: usize| {
            let i = (y * 100 + x) * 4;
            (buf[i], buf[i + 2]) // (R, B)
        };
        let (rt, bt) = at(50, 6); // near the top
        let (rb, bb) = at(50, 94); // near the bottom
        assert!(rt > bt, "top of the ramp is red-dominant (R>B)");
        assert!(bb > rb, "bottom of the ramp is blue-dominant (B>R)");
        assert!(rt > rb, "red fades from top to bottom");
        assert!(bb > bt, "blue grows from top to bottom");
    }

    /// Read a node's resolved value for `p` out of a styled (cascaded) clone.
    fn styled_prop(dom: &Dom, node: NodeId, p: PropId) -> Option<String> {
        dom.node(node).and_then(|n| n.styles.get(&p)).cloned()
    }

    #[test]
    fn an_inherited_prop_flows_from_parent_to_a_child_with_no_value() {
        use canopy_paint::FG;
        // <div color:via-rule> with a text child that has NO color of its own. After the cascade
        // the child's resolved FG (color) equals the parent's — real CSS inheritance.
        let mut e = Emitter::new();
        let div = e.create_element(ElementTag::new(1));
        e.append(ROOT, div);
        e.set_class(div, "box");
        let label = e.create_text("hi");
        e.append(div, label);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".box { color: #112233 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, div, FG).as_deref(),
            Some("#112233"),
            "the div resolves color from its class rule"
        );
        assert_eq!(
            styled_prop(&styled, label, FG).as_deref(),
            Some("#112233"),
            "the text child inherits the parent's color (it set none of its own)"
        );
    }

    #[test]
    fn inheritance_works_from_an_inline_parent_color_too() {
        use canopy_paint::FG;
        // The parent sets color *inline* (not via a rule). The child with no color still inherits it,
        // proving inheritance threads a node's OWN resolved styles (inline included), not just rules.
        let mut e = Emitter::new();
        let div = e.create_element(ElementTag::new(1));
        e.append(ROOT, div);
        e.set_inline_style(div, FG, "#abcdef");
        let inner = e.create_element(ElementTag::new(1));
        e.append(div, inner);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        // A stylesheet must be set for styled_dom to run; an empty rule body is fine.
        host.set_stylesheet(".noop { width: 1 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, inner, FG).as_deref(),
            Some("#abcdef"),
            "the child inherits the parent's inline color"
        );
    }

    #[test]
    fn a_child_rule_overrides_the_inherited_value() {
        use canopy_paint::FG;
        // Inheritance is the WEAKEST source: a matched rule on the child wins over the inherited
        // parent color, and an author-inline color on the child wins over both.
        let mut e = Emitter::new();
        let div = e.create_element(ElementTag::new(1));
        e.append(ROOT, div);
        e.set_class(div, "parent");
        let ruled = e.create_element(ElementTag::new(1)); // child colored by a rule
        e.append(div, ruled);
        e.set_class(ruled, "child");
        let inlined = e.create_element(ElementTag::new(1)); // child colored inline
        e.append(div, inlined);
        e.set_inline_style(inlined, FG, "#00ff00");
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".parent { color: #111111 } .child { color: #222222 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, ruled, FG).as_deref(),
            Some("#222222"),
            "a matched rule on the child beats the inherited parent color"
        );
        assert_eq!(
            styled_prop(&styled, inlined, FG).as_deref(),
            Some("#00ff00"),
            "author inline on the child beats both the rule and inheritance"
        );
    }

    #[test]
    fn a_non_inherited_prop_does_not_leak_to_a_child() {
        use canopy_paint::{BG, FG};
        // background is NOT an inherited property: the child must not pick up the parent's bg, even
        // though color (an inherited prop set on the same parent) does flow down.
        let mut e = Emitter::new();
        let div = e.create_element(ElementTag::new(1));
        e.append(ROOT, div);
        e.set_class(div, "parent");
        let child = e.create_element(ElementTag::new(1));
        e.append(div, child);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".parent { background: #ff0000; color: #00ff00 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, child, BG),
            None,
            "background does not inherit — the child has no bg of its own"
        );
        assert_eq!(
            styled_prop(&styled, child, FG).as_deref(),
            Some("#00ff00"),
            "color does inherit, confirming the parent actually resolved styles"
        );
    }

    #[test]
    fn inheritance_passes_through_multiple_levels() {
        use canopy_paint::FG;
        // grandparent (color via rule) -> parent (no color) -> child (no color): the child inherits
        // the grandparent's color, threaded through the intermediate node that set none of its own.
        let mut e = Emitter::new();
        let gp = e.create_element(ElementTag::new(1));
        e.append(ROOT, gp);
        e.set_class(gp, "gp");
        let parent = e.create_element(ElementTag::new(1));
        e.append(gp, parent);
        let child = e.create_element(ElementTag::new(1));
        e.append(parent, child);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".gp { color: #0a0b0c }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, parent, FG).as_deref(),
            Some("#0a0b0c"),
            "the middle node inherits the grandparent's color"
        );
        assert_eq!(
            styled_prop(&styled, child, FG).as_deref(),
            Some("#0a0b0c"),
            "inheritance carries through the intermediate node to the grandchild"
        );
    }

    #[test]
    fn an_intermediate_override_reparents_inheritance_for_descendants() {
        use canopy_paint::FG;
        // grandparent color A -> parent overrides to color B -> child: the child must inherit B (the
        // parent's own resolved value shadows the grandparent's when computing the child's inherited
        // map), proving step (3) overlays a node's own values onto what it passes down.
        let mut e = Emitter::new();
        let gp = e.create_element(ElementTag::new(1));
        e.append(ROOT, gp);
        e.set_class(gp, "gp");
        let parent = e.create_element(ElementTag::new(1));
        e.append(gp, parent);
        e.set_class(parent, "mid");
        let child = e.create_element(ElementTag::new(1));
        e.append(parent, child);
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(".gp { color: #aaaaaa } .mid { color: #bbbbbb }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, child, FG).as_deref(),
            Some("#bbbbbb"),
            "the child inherits the parent's overriding color, not the grandparent's"
        );
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

    // --- Wave 3b: structural + functional pseudo-classes via styled_dom ----
    // (reuses the `styled_prop` helper defined earlier in this test module)

    /// Build a host whose root holds `n` `li` children inside a `ul`, returning the host plus the
    /// child node ids in order. Each `li` is a real sibling, so structural pseudos resolve.
    fn host_with_list(n: usize, css: &str) -> (CanopyHost, Vec<NodeId>) {
        let mut e = Emitter::new();
        let ul = e.create_element(ElementTag::new(1));
        e.append(ROOT, ul);
        e.set_tag_name(ul, "ul");
        let mut items = Vec::new();
        for _ in 0..n {
            let li = e.create_element(ElementTag::new(1));
            e.append(ul, li);
            e.set_tag_name(li, "li");
            items.push(li);
        }
        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet(css);
        (host, items)
    }

    #[test]
    fn first_child_styles_only_the_first_sibling() {
        use canopy_paint::BG;
        let (host, items) = host_with_list(3, "li:first-child { background: #abcdef }");
        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, items[0], BG).as_deref(),
            Some("#abcdef"),
            "the first li is styled by :first-child"
        );
        for &later in &items[1..] {
            assert_eq!(
                styled_prop(&styled, later, BG),
                None,
                ":first-child does not style a non-first sibling"
            );
        }
    }

    #[test]
    fn nth_child_2n_styles_the_even_siblings() {
        use canopy_paint::BG;
        // 5 items, 1-based positions 1..=5; 2n matches the 2nd and 4th (0-based index 1 and 3).
        let (host, items) = host_with_list(5, "li:nth-child(2n) { background: #020202 }");
        let styled = host.styled_dom().expect("a stylesheet is set");
        let styled_indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, &li)| styled_prop(&styled, li, BG).is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            styled_indices,
            vec![1, 3],
            ":nth-child(2n) styles the 2nd and 4th siblings"
        );
    }

    #[test]
    fn not_excludes_the_right_node() {
        use canopy_paint::BG;
        // `li:not(.skip)` styles every li except the one carrying `.skip`.
        let mut e = Emitter::new();
        let ul = e.create_element(ElementTag::new(1));
        e.append(ROOT, ul);
        e.set_tag_name(ul, "ul");
        let a = e.create_element(ElementTag::new(1));
        e.append(ul, a);
        e.set_tag_name(a, "li");
        let b = e.create_element(ElementTag::new(1));
        e.append(ul, b);
        e.set_tag_name(b, "li");
        e.set_class(b, "skip");
        let c = e.create_element(ElementTag::new(1));
        e.append(ul, c);
        e.set_tag_name(c, "li");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet("li:not(.skip) { background: #777777 }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, a, BG).as_deref(),
            Some("#777777"),
            "li without .skip is styled"
        );
        assert_eq!(
            styled_prop(&styled, b, BG),
            None,
            ":not(.skip) excludes the .skip li"
        );
        assert_eq!(
            styled_prop(&styled, c, BG).as_deref(),
            Some("#777777"),
            "the other plain li is styled"
        );
    }

    #[test]
    fn empty_styles_a_childless_node_end_to_end() {
        use canopy_paint::BG;
        // A `div:empty` rule styles the leaf li (no children) but not the ul (which has children).
        let mut e = Emitter::new();
        let ul = e.create_element(ElementTag::new(1));
        e.append(ROOT, ul);
        e.set_tag_name(ul, "ul");
        let leaf = e.create_element(ElementTag::new(1));
        e.append(ul, leaf);
        e.set_tag_name(leaf, "li");

        let mut host = CanopyHost::new();
        assert_eq!(host.apply_bytes(&e.take_batch(0)), CANOPY_OK);
        host.set_stylesheet("li:empty { background: #0c0c0c } ul:empty { background: #ffffff }");

        let styled = host.styled_dom().expect("a stylesheet is set");
        assert_eq!(
            styled_prop(&styled, leaf, BG).as_deref(),
            Some("#0c0c0c"),
            "the childless li matches :empty"
        );
        assert_eq!(
            styled_prop(&styled, ul, BG),
            None,
            "the ul has a child, so ul:empty does not match"
        );
    }
}
