//! The **full-tier** Canopy [`StyleEngine`]: a real Servo-**Stylo** CSS cascade.
//!
//! Canopy's [`StyleEngine`](canopy_traits::StyleEngine) trait reserves a full CSS
//! cascade for capable tiers. This crate is that implementation: it runs the genuine
//! Stylo cascade — **inheritance**, **specificity**, **selector combinators**, and
//! computed-value resolution — and flattens the result into Canopy's small, flat
//! [`ComputedStyle`](canopy_traits::ComputedStyle). It is the capable-tier counterpart
//! to `canopy-style-css`, the constrained-tier resolver (bare class names, no
//! specificity, no tree inheritance); both satisfy the same trait, which is exactly the
//! tiered `StyleEngine` design the core documents.
//!
//! It also runs real **layout**: [`StyloEngine::layout`] maps each element's resolved
//! `ComputedValues` to a [`taffy::Style`] (the [`taffy_convert`] module, vendored from
//! Blitz's `stylo_taffy`) and runs Taffy's flex/block engine, so box geometry matches a
//! browser for what Taffy supports. (The crate's tests check both the cascade and the
//! layout against a real browser — `getComputedStyle` and `getBoundingClientRect`.)
//!
//! ## How it works
//!
//! Stylo's cascade matches selectors against a DOM that implements Stylo's own traits
//! (`selectors::Element`, `TElement`/`TNode`/`TDocument`). Canopy's retained tree
//! (`canopy-dom`) carries only resolved inline styles (classes are expanded author-side)
//! and a numeric element tag — not the CSS element name / class list a selector engine
//! needs. So, exactly as **Blitz** (`blitz-dom`) does for its renderer, this crate owns
//! a *small, HTML-like arena DOM* purpose-built for styling: a [`Document`] of [`Node`]s
//! carrying a local name, id, and class list.
//!
//! In **servo mode** (Stylo's default feature) Stylo ships the entire `SelectorImpl`
//! (`PseudoElement`/`NonTSPseudoClass`/atoms), so this crate writes none of it — it only
//! implements the DOM-side traits over `&Node` and drives the cascade. The trait glue,
//! the interior-mutable [`StyloData`] element-data slot, and the [`RecalcStyle`]
//! traversal driver are modeled on Blitz's `blitz-dom`, heavily stripped of animations,
//! shadow DOM, snapshots, presentational hints, and layout. The cascade runs
//! **single-threaded** (no rayon pool) under `thread_state::LAYOUT`.
//!
//! [`StyloEngine`] owns the [`Document`] + a Stylo `Stylist` (the parsed author CSS);
//! [`StyleEngine::resolve`](canopy_traits::StyleEngine::resolve) runs the whole-tree
//! cascade once (idempotent) and reads each node's resolved `ComputedValues` back out as
//! a flat [`ComputedStyle`](canopy_traits::ComputedStyle).
//!
//! ## Safety
//!
//! Each [`Node`] stores a raw `*const Vec<Node>` back-pointer so a borrowed `&Node`
//! handle can navigate the tree (Blitz's model). It is wired in [`Document::finalize`]
//! and **only dereferenced during the single-threaded style traversal**, immediately
//! after `finalize` re-points it — so it cannot dangle across a `Vec` reallocation or a
//! move of the owning `Document` (any structural mutation flips `resolved` off, forcing
//! a re-`finalize` before the next traversal). `unsafe impl Send/Sync for Node` is sound
//! only under that single-threaded-during-traversal invariant.

use std::cell::{Cell, UnsafeCell};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use markup5ever::{
    local_name, ns, LocalName, LocalNameStaticSet, Namespace, NamespaceStaticSet, QualName,
};
use selectors::{
    attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint},
    matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode},
    sink::Push,
    Element, OpaqueElement,
};
use servo_arc::Arc as ServoArc;

use style::applicable_declarations::ApplicableDeclarationBlock;
use style::color::AbsoluteColor;
use style::context::{
    QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
    StyleContext,
};
use style::data::{ElementDataMut, ElementDataRef, ElementDataWrapper};
use style::dom::{
    AttributeProvider, LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode,
    TShadowRoot,
};
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::media_queries::{MediaList, MediaType};
use style::properties::style_structs::Font;
use style::properties::{ComputedValues, PropertyDeclarationBlock};
use style::queries::values::PrefersColorScheme;
use style::selector_parser::{
    NonTSPseudoClass, PseudoElement, RestyleDamage, SelectorImpl, SnapshotMap,
};
use style::servo_arc::{Arc as StyleArc, ArcBorrow};
use style::shared_lock::{Locked, SharedRwLock, StylesheetGuards};
use style::stylesheets::{
    scope_rule::ImplicitScopeRoot, AllowImportRules, DocumentStyleSheet, Origin, Stylesheet,
    UrlExtraData,
};
use style::stylist::Stylist;
use style::thread_state::{self, ThreadState};
use style::traversal::{recalc_style_at, DomTraversal, PerLevelTraversalData};
use style::traversal_flags::TraversalFlags;
use style::values::{AtomIdent, AtomString, GenericAtomIdent};
use style::Atom;
use style::CaseSensitivityExt;

use style_dom::ElementState;

use canopy_protocol::NodeId;
use canopy_traits::{Color, ComputedStyle, Display, HostError, StyleEngine};

/// Parse real HTML into the arena [`Document`] (html5ever -> arena).
pub mod html;
/// L3 paint: rasterize the cascaded + laid-out tree to pixels.
pub mod paint;

// ===========================================================================
// StyloData: interior-mutable slot for Stylo's ElementData.
// (Copied from /tmp/blitz/.../node/stylo_data.rs, with ALL_DAMAGE replaced by
//  RestyleDamage::reconstruct() — a full one-shot damage.)
// ===========================================================================

/// Interior-mutable wrapper around `Option<StyloElementData>`.
///
/// Safety relies on Stylo having exclusive access to nodes during style
/// traversals: `init`/`clear` only happen during exclusive-access phases.
pub struct StyloData {
    inner: UnsafeCell<Option<ElementDataWrapper>>,
}

impl Default for StyloData {
    fn default() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }
}

impl fmt::Debug for StyloData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StyloData").finish_non_exhaustive()
    }
}

impl Deref for StyloData {
    type Target = Option<ElementDataWrapper>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.get() }
    }
}

impl DerefMut for StyloData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.get_mut()
    }
}

impl StyloData {
    /// Whether element data has been initialized.
    pub fn has_data(&self) -> bool {
        unsafe { &*self.inner.get() }.is_some()
    }

    /// Borrow the element data immutably, if present.
    pub fn get(&self) -> Option<ElementDataRef<'_>> {
        self.as_ref().map(|w| w.borrow())
    }

    /// Get a mutable reference to the data.
    ///
    /// # Safety
    /// There must be no aliasing access to the cell.
    pub unsafe fn unsafe_stylo_only_mut(&self) -> Option<ElementDataMut<'_>> {
        let opt = unsafe { &mut *self.inner.get() };
        opt.as_mut().map(|w| w.borrow_mut())
    }

    /// Initialize the element data ready for use (if not already initialized).
    ///
    /// # Safety
    /// There must be no outstanding borrows to this container or anything
    /// contained within it when this method is called.
    pub unsafe fn ensure_init(&self) -> ElementDataMut<'_> {
        if !self.has_data() {
            unsafe { *self.inner.get() = Some(ElementDataWrapper::default()) };
            let mut data_mut = unsafe { self.unsafe_stylo_only_mut() }.unwrap();
            data_mut.damage = RestyleDamage::reconstruct();
            data_mut
        } else {
            unsafe { self.unsafe_stylo_only_mut() }.unwrap()
        }
    }

    /// Clear the element data, returning to the uninitialized state.
    ///
    /// # Safety
    /// There must be no outstanding borrows when this is called.
    pub unsafe fn clear(&self) {
        unsafe { *self.inner.get() = None };
    }
}

// ===========================================================================
// The arena DOM.
// ===========================================================================

/// What a node is.
pub enum NodeKind {
    /// The implicit document root (node 0).
    Document,
    /// An HTML-like element.
    Element {
        /// Qualified name (local + html namespace).
        name: QualName,
        /// `id` attribute, interned.
        id_attr: Option<Atom>,
        /// `class` attribute tokens, interned.
        classes: Vec<Atom>,
        /// Parsed inline `style` attribute, if any.
        style_attribute: Option<StyleArc<Locked<PropertyDeclarationBlock>>>,
    },
    /// A text node.
    Text(String),
}

/// A single arena node. The handle type implementing the `style` traits is
/// `&Node`, exactly like Blitz's `BlitzNode<'a> = &'a Node`.
pub struct Node {
    /// Slab index.
    pub id: usize,
    /// Parent index.
    pub parent: Option<usize>,
    /// Child indices, in order.
    pub children: Vec<usize>,
    /// Node kind/payload.
    pub kind: NodeKind,

    // --- Stylo bookkeeping (mirrors Blitz's Node) ---
    /// Stylo's per-element computed-data slot.
    pub stylo_element_data: StyloData,
    /// Selector matching flags set during traversal.
    pub selector_flags: Cell<ElementSelectorFlags>,
    /// `:hover`/`:active`/etc. element state.
    pub element_state: ElementState,
    /// Whether a snapshot exists for this node (always false here).
    pub has_snapshot: bool,
    /// Whether a snapshot has been handled by the traversal.
    pub snapshot_handled: AtomicBool,
    /// Whether this node has dirty descendants needing restyle.
    pub dirty_descendants: AtomicBool,

    /// Raw pointer back to the slab so a `&Node` can navigate the tree.
    /// Set after the whole tree is built (the Vec must not move afterwards).
    pub tree: *const Vec<Node>,
}

// SAFETY: The raw `tree` pointer is only dereferenced during the single-threaded
// style traversal, while the owning `Document` is borrowed and pinned in place.
unsafe impl Send for Node {}
unsafe impl Sync for Node {}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("id", &self.id)
            .field("parent", &self.parent)
            .finish_non_exhaustive()
    }
}

// `selectors::Element` requires the element type (`&Node`) to be `PartialEq`.
// Two handles refer to the same DOM node iff their slab ids match.
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Node {}

impl Node {
    fn new(id: usize, parent: Option<usize>, kind: NodeKind) -> Self {
        Node {
            id,
            parent,
            children: Vec::new(),
            kind,
            stylo_element_data: StyloData::default(),
            selector_flags: Cell::new(ElementSelectorFlags::empty()),
            element_state: ElementState::empty(),
            has_snapshot: false,
            snapshot_handled: AtomicBool::new(false),
            dirty_descendants: AtomicBool::new(false),
            tree: std::ptr::null(),
        }
    }

    /// Borrow the whole slab via the stored raw pointer.
    fn tree(&self) -> &Vec<Node> {
        // SAFETY: set once after tree construction; the Vec outlives all `&Node`
        // handles used during traversal.
        unsafe { &*self.tree }
    }

    /// Re-borrow another node by id (Blitz's `with`).
    fn with(&self, id: usize) -> &Node {
        &self.tree()[id]
    }

    fn is_element(&self) -> bool {
        matches!(self.kind, NodeKind::Element { .. })
    }

    fn is_text_node(&self) -> bool {
        matches!(self.kind, NodeKind::Text(_))
    }

    /// Element name accessor.
    fn name(&self) -> Option<&QualName> {
        match &self.kind {
            NodeKind::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// nth following sibling (by absolute child-list offset).
    fn forward(&self, n: usize) -> Option<&Node> {
        let parent = self.with(self.parent?);
        let idx = parent.children.iter().position(|id| *id == self.id)?;
        parent.children.get(idx + n).map(|id| self.with(*id))
    }

    /// nth preceding sibling.
    fn backward(&self, n: usize) -> Option<&Node> {
        let parent = self.with(self.parent?);
        let idx = parent.children.iter().position(|id| *id == self.id)?;
        if idx < n {
            return None;
        }
        parent.children.get(idx - n).map(|id| self.with(*id))
    }

    fn has_dirty_descendants(&self) -> bool {
        self.dirty_descendants.load(Ordering::Relaxed)
    }

    fn set_dirty_descendants(&self) {
        self.dirty_descendants.store(true, Ordering::Relaxed);
    }

    fn unset_dirty_descendants(&self) {
        self.dirty_descendants.store(false, Ordering::Relaxed);
    }

    fn mark_ancestors_dirty(&self) {
        let mut current = self.parent;
        while let Some(pid) = current {
            let parent = self.with(pid);
            if parent.dirty_descendants.swap(true, Ordering::Relaxed) {
                break;
            }
            current = parent.parent;
        }
    }
}

// ===========================================================================
// `AttributeProvider` for `&Node` (used by selector attribute matching).
// ===========================================================================

impl AttributeProvider for &Node {
    fn get_attr(
        &self,
        attr: &GenericAtomIdent<LocalNameStaticSet>,
        _ns: &GenericAtomIdent<NamespaceStaticSet>,
    ) -> Option<String> {
        match &self.kind {
            NodeKind::Element {
                id_attr, classes, ..
            } => {
                if attr.0 == local_name!("id") {
                    id_attr.as_ref().map(|a| a.to_string())
                } else if attr.0 == local_name!("class") {
                    if classes.is_empty() {
                        None
                    } else {
                        Some(
                            classes
                                .iter()
                                .map(|c| c.to_string())
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// ===========================================================================
// `selectors::Element` for `&Node`.
// ===========================================================================

impl selectors::Element for &Node {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        let non_null = NonNull::new((self.id + 1) as *mut ()).unwrap();
        OpaqueElement::from_non_null_ptr(non_null)
    }

    fn parent_element(&self) -> Option<Self> {
        TElement::traversal_parent(self)
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let mut n = 1;
        while let Some(node) = self.backward(n) {
            if node.is_element() {
                return Some(node);
            }
            n += 1;
        }
        None
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let mut n = 1;
        while let Some(node) = self.forward(n) {
            if node.is_element() {
                return Some(node);
            }
            n += 1;
        }
        None
    }

    fn first_element_child(&self) -> Option<Self> {
        self.dom_children().find(|child| child.is_element())
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true
    }

    fn has_local_name(&self, local_name: &LocalName) -> bool {
        self.name().map(|n| &n.local == local_name).unwrap_or(false)
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        self.name().map(|n| &n.ns == ns).unwrap_or(false)
    }

    fn is_same_type(&self, other: &Self) -> bool {
        match (self.name(), other.name()) {
            (Some(a), Some(b)) => a.local == b.local && a.ns == b.ns,
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        _ns: &NamespaceConstraint<&GenericAtomIdent<NamespaceStaticSet>>,
        local_name: &GenericAtomIdent<LocalNameStaticSet>,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        // Minimal: only id/class are stored explicitly. We reconstruct their
        // string forms; other attributes are not supported.
        match &self.kind {
            NodeKind::Element {
                id_attr, classes, ..
            } => {
                if local_name.0 == local_name!("id") {
                    if let Some(id) = id_attr {
                        return operation.eval_str(id);
                    }
                    false
                } else if local_name.0 == local_name!("class") {
                    if classes.is_empty() {
                        return false;
                    }
                    let joined = classes
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(" ");
                    operation.eval_str(&joined)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo_class: &NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        match *pseudo_class {
            NonTSPseudoClass::Hover => self.element_state.contains(ElementState::HOVER),
            NonTSPseudoClass::Active => self.element_state.contains(ElementState::ACTIVE),
            NonTSPseudoClass::Focus => self.element_state.contains(ElementState::FOCUS),
            _ => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        let self_flags = flags.for_self();
        if !self_flags.is_empty() {
            self.selector_flags
                .set(self.selector_flags.get() | self_flags);
        }
        let parent_flags = flags.for_parent();
        if !parent_flags.is_empty() {
            if let Some(pid) = self.parent {
                let parent = self.with(pid);
                parent
                    .selector_flags
                    .set(parent.selector_flags.get() | parent_flags);
            }
        }
    }

    fn is_link(&self) -> bool {
        self.has_local_name(&local_name!("a"))
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        match &self.kind {
            NodeKind::Element { id_attr, .. } => id_attr
                .as_ref()
                .map(|id_attr| case_sensitivity.eq_atom(id_attr, id))
                .unwrap_or(false),
            _ => false,
        }
    }

    fn has_class(&self, search_name: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        match &self.kind {
            NodeKind::Element { classes, .. } => classes
                .iter()
                .any(|c| case_sensitivity.eq_atom(c, search_name)),
            _ => false,
        }
    }

    fn imported_part(&self, _name: &AtomIdent) -> Option<AtomIdent> {
        None
    }

    fn is_part(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.dom_children().next().is_none()
    }

    fn is_root(&self) -> bool {
        self.parent
            .map(|pid| self.with(pid).parent.is_none())
            .unwrap_or(false)
    }

    fn has_custom_state(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        // Returning false signals "no hashes added"; Stylo then skips the
        // bloom-filter fast-reject for this element, which is always correct
        // (just slightly slower). Avoids depending on stylo's bloom internals.
        false
    }
}

// ===========================================================================
// `TDocument` / `NodeInfo` / `TShadowRoot` / `TNode` / `TElement` for `&Node`.
// ===========================================================================

impl<'a> TDocument for &'a Node {
    type ConcreteNode = &'a Node;

    fn as_node(&self) -> Self::ConcreteNode {
        self
    }

    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> QuirksMode {
        QuirksMode::NoQuirks
    }

    fn shared_lock(&self) -> &SharedRwLock {
        &GLOBAL_GUARD
    }
}

impl NodeInfo for &Node {
    fn is_element(&self) -> bool {
        Node::is_element(self)
    }

    fn is_text_node(&self) -> bool {
        Node::is_text_node(self)
    }
}

impl<'a> TShadowRoot for &'a Node {
    type ConcreteNode = &'a Node;

    fn as_node(&self) -> Self::ConcreteNode {
        self
    }

    fn host(&self) -> <Self::ConcreteNode as TNode>::ConcreteElement {
        unreachable!("Shadow roots not implemented")
    }

    fn style_data<'b>(&self) -> Option<&'b style::stylist::CascadeData>
    where
        Self: 'b,
    {
        None
    }
}

impl<'a> TNode for &'a Node {
    type ConcreteElement = &'a Node;
    type ConcreteDocument = &'a Node;
    type ConcreteShadowRoot = &'a Node;

    fn parent_node(&self) -> Option<Self> {
        self.parent.map(|id| self.with(id))
    }

    fn first_child(&self) -> Option<Self> {
        self.children.first().map(|id| self.with(*id))
    }

    fn last_child(&self) -> Option<Self> {
        self.children.last().map(|id| self.with(*id))
    }

    fn prev_sibling(&self) -> Option<Self> {
        self.backward(1)
    }

    fn next_sibling(&self) -> Option<Self> {
        self.forward(1)
    }

    fn owner_doc(&self) -> Self::ConcreteDocument {
        self.with(0)
    }

    fn is_in_document(&self) -> bool {
        true
    }

    fn traversal_parent(&self) -> Option<Self::ConcreteElement> {
        self.parent_node().and_then(|node| node.as_element())
    }

    fn opaque(&self) -> OpaqueNode {
        OpaqueNode(self.id)
    }

    fn debug_id(self) -> usize {
        self.id
    }

    fn as_element(&self) -> Option<Self::ConcreteElement> {
        if self.is_element() {
            Some(self)
        } else {
            None
        }
    }

    fn as_document(&self) -> Option<Self::ConcreteDocument> {
        match self.kind {
            NodeKind::Document => Some(self),
            _ => None,
        }
    }

    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot> {
        None
    }
}

impl<'a> TElement for &'a Node {
    type ConcreteNode = &'a Node;
    type TraversalChildrenIterator = Traverser<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        self
    }

    fn implicit_scope_for_sheet_in_shadow_root(
        _opaque_host: OpaqueElement,
        _sheet_index: usize,
    ) -> Option<ImplicitScopeRoot> {
        None
    }

    fn traversal_children(&self) -> LayoutIterator<Self::TraversalChildrenIterator> {
        LayoutIterator(Traverser {
            parent: self,
            child_index: 0,
        })
    }

    fn is_html_element(&self) -> bool {
        self.is_element()
    }

    fn is_mathml_element(&self) -> bool {
        false
    }

    fn is_svg_element(&self) -> bool {
        false
    }

    fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        match &self.kind {
            NodeKind::Element {
                style_attribute, ..
            } => style_attribute.as_ref().map(|a| a.borrow_arc()),
            _ => None,
        }
    }

    fn state(&self) -> ElementState {
        self.element_state
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    fn id(&self) -> Option<&Atom> {
        match &self.kind {
            NodeKind::Element { id_attr, .. } => id_attr.as_ref(),
            _ => None,
        }
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&AtomIdent),
    {
        if let NodeKind::Element { classes, .. } = &self.kind {
            for class in classes {
                callback(AtomIdent::cast(class));
            }
        }
    }

    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&GenericAtomIdent<LocalNameStaticSet>),
    {
        if let NodeKind::Element {
            id_attr, classes, ..
        } = &self.kind
        {
            if id_attr.is_some() {
                callback(&GenericAtomIdent(local_name!("id")));
            }
            if !classes.is_empty() {
                callback(&GenericAtomIdent(local_name!("class")));
            }
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        Node::has_dirty_descendants(self)
    }

    fn has_snapshot(&self) -> bool {
        self.has_snapshot
    }

    fn handled_snapshot(&self) -> bool {
        self.snapshot_handled.load(Ordering::SeqCst)
    }

    unsafe fn set_handled_snapshot(&self) {
        self.snapshot_handled.store(true, Ordering::SeqCst);
    }

    unsafe fn set_dirty_descendants(&self) {
        Node::set_dirty_descendants(self);
        Node::mark_ancestors_dirty(self);
    }

    unsafe fn unset_dirty_descendants(&self) {
        Node::unset_dirty_descendants(self);
    }

    fn store_children_to_process(&self, _n: isize) {
        unreachable!()
    }

    fn did_process_child(&self) -> isize {
        unreachable!()
    }

    unsafe fn ensure_data(&self) -> ElementDataMut<'_> {
        unsafe { self.stylo_element_data.ensure_init() }
    }

    unsafe fn clear_data(&self) {
        unsafe { self.stylo_element_data.clear() }
    }

    fn has_data(&self) -> bool {
        self.stylo_element_data.has_data()
    }

    fn borrow_data(&self) -> Option<ElementDataRef<'_>> {
        self.stylo_element_data.get()
    }

    fn mutate_data(&self) -> Option<ElementDataMut<'_>> {
        unsafe { self.stylo_element_data.unsafe_stylo_only_mut() }
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        false
    }

    fn has_animations(&self, _context: &SharedStyleContext) -> bool {
        false
    }

    fn has_css_animations(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        false
    }

    fn has_css_transitions(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        false
    }

    fn animation_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<StyleArc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn transition_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<StyleArc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn shadow_root(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn containing_shadow(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn lang_attr(&self) -> Option<style::selector_parser::AttrValue> {
        None
    }

    fn match_element_lang(
        &self,
        _override_lang: Option<Option<style::selector_parser::AttrValue>>,
        _value: &style::selector_parser::Lang,
    ) -> bool {
        false
    }

    fn is_html_document_body_element(&self) -> bool {
        self.has_local_name(&local_name!("body"))
    }

    fn synthesize_presentational_hints_for_legacy_attributes<V>(
        &self,
        _visited_handling: VisitedHandlingMode,
        _hints: &mut V,
    ) where
        V: Push<ApplicableDeclarationBlock>,
    {
        // No legacy presentational hints in this minimal DOM.
    }

    fn local_name(&self) -> &LocalName {
        &self.name().expect("Not an element").local
    }

    fn namespace(&self) -> &Namespace {
        &self.name().expect("Not an element").ns
    }

    fn query_container_size(
        &self,
        _display: &style::values::specified::Display,
    ) -> euclid::default::Size2D<Option<app_units::Au>> {
        Default::default()
    }

    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }

    fn has_selector_flags(&self, flags: ElementSelectorFlags) -> bool {
        self.selector_flags.get().contains(flags)
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        let flags = self.selector_flags.get();
        if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR)
        {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR
        } else if flags.contains(ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING) {
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING
        } else {
            ElementSelectorFlags::empty()
        }
    }

    fn compute_layout_damage(_old: &ComputedValues, _new: &ComputedValues) -> RestyleDamage {
        RestyleDamage::reconstruct()
    }
}

/// Child iterator used by `traversal_children`.
pub struct Traverser<'a> {
    parent: &'a Node,
    child_index: usize,
}

impl<'a> Iterator for Traverser<'a> {
    type Item = &'a Node;

    fn next(&mut self) -> Option<Self::Item> {
        let node_id = self.parent.children.get(self.child_index)?;
        let node = self.parent.with(*node_id);
        self.child_index += 1;
        Some(node)
    }
}

impl std::hash::Hash for &Node {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_usize(self.id)
    }
}

// ===========================================================================
// Traversal driver (copied from Blitz stylo.rs).
// ===========================================================================

/// Stub painter registry — no custom painters.
pub struct RegisteredPaintersImpl;
impl RegisteredSpeculativePainters for RegisteredPaintersImpl {
    fn get(&self, _name: &Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

/// The pre-order recalc-style traversal.
pub struct RecalcStyle<'a> {
    context: SharedStyleContext<'a>,
}

impl<'a> RecalcStyle<'a> {
    pub fn new(context: SharedStyleContext<'a>) -> Self {
        RecalcStyle { context }
    }
}

#[allow(unsafe_code)]
impl<E> DomTraversal<E> for RecalcStyle<'_>
where
    E: TElement,
{
    fn process_preorder<F: FnMut(E::ConcreteNode)>(
        &self,
        traversal_data: &PerLevelTraversalData,
        context: &mut StyleContext<E>,
        node: E::ConcreteNode,
        note_child: F,
    ) {
        if let Some(el) = node.as_element() {
            let mut data = unsafe { el.ensure_data() };
            recalc_style_at(self, traversal_data, context, el, &mut data, note_child);
            unsafe { el.unset_dirty_descendants() }
        }
    }

    #[inline]
    fn needs_postorder_traversal() -> bool {
        false
    }

    fn process_postorder(&self, _style_context: &mut StyleContext<E>, _node: E::ConcreteNode) {
        panic!("this should never be called")
    }

    #[inline]
    fn shared_context(&self) -> &SharedStyleContext<'_> {
        &self.context
    }
}

// ===========================================================================
// A process-global SharedRwLock + URL data.
// Stylo's `TDocument::shared_lock` must return a `&SharedRwLock`, and the same
// lock must be used to parse stylesheets and inline styles. We keep one global.
// ===========================================================================

static GLOBAL_GUARD: LazyLock<SharedRwLock> = LazyLock::new(SharedRwLock::new);

fn dummy_url_data() -> UrlExtraData {
    // In servo mode, `UrlExtraData(pub servo_arc::Arc<url::Url>)`, with a
    // `From<url::Url>` impl. A trivial always-valid base URL is fine here.
    UrlExtraData::from(url::Url::parse("about:blank").unwrap())
}

// ===========================================================================
// FontMetricsProvider stub.
// ===========================================================================

#[derive(Debug)]
struct StubFontMetricsProvider;

impl style::device::servo::FontMetricsProvider for StubFontMetricsProvider {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        _font: &Font,
        _base_size: style::values::computed::CSSPixelLength,
        _flags: style::values::computed::font::QueryFontMetricsFlags,
    ) -> style::font_metrics::FontMetrics {
        Default::default()
    }

    fn base_size_for_generic(
        &self,
        _generic: style::values::computed::font::GenericFontFamily,
    ) -> style::values::computed::Length {
        style::values::computed::Length::from(app_units::Au::from_f32_px(16.0))
    }
}

// ===========================================================================
// Document: owns the arena.
// ===========================================================================

/// The arena DOM document.
pub struct Document {
    /// Slab of nodes. Node 0 is the implicit document root.
    pub nodes: Vec<Node>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Create an empty document with just the root (node 0).
    pub fn new() -> Self {
        let root = Node::new(0, None, NodeKind::Document);
        Document { nodes: vec![root] }
    }

    /// Append an element under `parent`, returning its slab id.
    pub fn add_element(
        &mut self,
        parent: usize,
        tag: &str,
        id_attr: Option<&str>,
        classes: &[&str],
    ) -> usize {
        let id = self.nodes.len();
        let name = QualName::new(None, ns!(html), LocalName::from(tag));
        let kind = NodeKind::Element {
            name,
            id_attr: id_attr.map(Atom::from),
            classes: classes.iter().map(|c| Atom::from(*c)).collect(),
            style_attribute: None,
        };
        self.nodes.push(Node::new(id, Some(parent), kind));
        self.nodes[parent].children.push(id);
        id
    }

    /// Append a text node under `parent`, returning its slab id.
    pub fn add_text(&mut self, parent: usize, text: &str) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node::new(
            id,
            Some(parent),
            NodeKind::Text(text.to_string()),
        ));
        self.nodes[parent].children.push(id);
        id
    }

    /// Set an inline `style` attribute on an element node.
    pub fn set_inline_style(&mut self, node_id: usize, css: &str) {
        let decls = style::properties::parse_style_attribute(
            css,
            &dummy_url_data(),
            None,
            QuirksMode::NoQuirks,
            style::stylesheets::CssRuleType::Style,
        );
        let locked = GLOBAL_GUARD.wrap(decls);
        if let NodeKind::Element {
            style_attribute, ..
        } = &mut self.nodes[node_id].kind
        {
            *style_attribute = Some(StyleArc::new(locked));
        }
    }

    /// Finalize the tree: wire every node's `tree` raw pointer to the slab.
    /// MUST be called after all nodes are added and before styling; the `nodes`
    /// Vec must not be mutated (reallocated) afterwards.
    fn finalize(&mut self) {
        let ptr: *const Vec<Node> = &self.nodes;
        for node in &mut self.nodes {
            node.tree = ptr;
        }
    }
}

// ===========================================================================
// The Stylo engine: owns the Document + Stylist + parsed stylesheet, runs the
// cascade once, and maps computed values into canopy `ComputedStyle`.
// ===========================================================================

/// A real Stylo-backed [`StyleEngine`].
pub struct StyloEngine {
    doc: Document,
    stylist: Stylist,
    snapshots: SnapshotMap,
    resolved: bool,
}

impl StyloEngine {
    /// Build an engine with the given author CSS. The DOM is then built via
    /// [`Document`] mutators on [`StyloEngine::document_mut`].
    pub fn new(css: &str) -> Self {
        // Stylo (in `servo` mode) gates `display: grid` parsing AND the
        // `grid-template-*` / `grid-auto-*` longhands behind the runtime pref
        // `layout.grid.enabled`, which defaults to false. Flip it on (process-
        // global atomic; idempotent) so the cascade keeps grid declarations.
        static_prefs::set_pref!("layout.grid.enabled", true);

        let device = Self::make_device();
        let mut stylist = Stylist::new(device, QuirksMode::NoQuirks);

        // Parse the author stylesheet and append it.
        let sheet = Stylesheet::from_str(
            css,
            dummy_url_data(),
            Origin::Author,
            ServoArc::new(GLOBAL_GUARD.wrap(MediaList::empty())),
            GLOBAL_GUARD.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        );
        let doc_sheet = DocumentStyleSheet(ServoArc::new(sheet));
        stylist.append_stylesheet(doc_sheet, &GLOBAL_GUARD.read());

        StyloEngine {
            doc: Document::new(),
            stylist,
            snapshots: SnapshotMap::new(),
            resolved: false,
        }
    }

    fn make_device() -> style::device::Device {
        let viewport_size = euclid::Size2D::new(800.0, 600.0);
        let device_pixel_ratio = euclid::Scale::new(1.0);
        style::device::Device::new(
            MediaType::screen(),
            QuirksMode::NoQuirks,
            viewport_size,
            device_pixel_ratio,
            Box::new(StubFontMetricsProvider),
            ComputedValues::initial_values_with_font_override(Font::initial_values()),
            PrefersColorScheme::Light,
        )
    }

    /// Mutable access to the underlying document for building the DOM.
    pub fn document_mut(&mut self) -> &mut Document {
        // Any structural mutation invalidates a prior resolve.
        self.resolved = false;
        &mut self.doc
    }

    /// Run the real Stylo cascade over the whole tree (idempotent: runs once).
    pub fn resolve_styles(&mut self) {
        if self.resolved {
            return;
        }
        self.doc.finalize();

        thread_state::enter(ThreadState::LAYOUT);

        let guard = GLOBAL_GUARD.read();
        let guards = StylesheetGuards {
            author: &guard,
            ua_or_user: &guard,
        };

        // root = first element child of node 0.
        let root_node: &Node = &self.doc.nodes[0];
        let root = TDocument::as_node(&root_node)
            .first_element_child()
            .expect("document has no root element")
            .as_element()
            .expect("root is not an element");

        self.stylist
            .flush(&guards)
            .process_style(root, Some(&self.snapshots));

        let context = SharedStyleContext {
            traversal_flags: TraversalFlags::empty(),
            stylist: &self.stylist,
            options: GLOBAL_STYLE_DATA.options.clone(),
            guards: StylesheetGuards {
                author: &guard,
                ua_or_user: &guard,
            },
            visited_styles_enabled: false,
            animations: Default::default(),
            current_time_for_animations: 0.0,
            snapshot_map: &self.snapshots,
            registered_speculative_painters: &RegisteredPaintersImpl,
        };

        let token = RecalcStyle::pre_traverse(root, &context);
        if token.should_traverse() {
            let traverser = RecalcStyle::new(context);
            // None pool = single-threaded.
            style::driver::traverse_dom(&traverser, token, None);
        }

        thread_state::exit(ThreadState::LAYOUT);
        self.resolved = true;
    }

    /// Read the computed style for a node by slab id (after resolving).
    fn computed_style_for(&self, node_id: usize) -> Option<ComputedStyle> {
        let node = self.doc.nodes.get(node_id)?;
        // Text nodes have no styles of their own; resolve them to their parent
        // element's style (which is what they'd inherit).
        let elem_id = if node.is_element() {
            node_id
        } else {
            node.parent?
        };
        let data = self.doc.nodes[elem_id].stylo_element_data.get()?;
        let styles = data.styles.get_primary()?;
        Some(map_computed_style(styles))
    }
}

/// Map a Stylo `&ComputedValues` into a canopy `ComputedStyle`.
fn map_computed_style(style: &ComputedValues) -> ComputedStyle {
    use style::values::computed::length_percentage::Unpacked;

    // color (foreground)
    let color = absolute_to_color(style.clone_color());

    // background color (transparent -> a=0)
    let bg = style.get_background().background_color.clone();
    let background = absolute_to_color(bg.resolve_to_absolute(&style.clone_color()));

    // font-size in px
    let font_size = style.get_font().font_size.used_size().px();

    // padding-top -> px
    let padding = match style.get_padding().padding_top.0.unpack() {
        Unpacked::Length(l) => l.px(),
        Unpacked::Percentage(_) => 0.0,
        Unpacked::Calc(_) => 0.0,
    };

    // display
    let display_val = style.get_box().clone_display();
    let display = if display_val == style::values::computed::Display::None {
        Display::None
    } else if display_val.inside() == style::values::specified::box_::DisplayInside::Flex {
        Display::Flex
    } else {
        Display::Block
    };

    ComputedStyle {
        display,
        color,
        background,
        font_size,
        padding,
    }
}

/// Convert a Stylo `AbsoluteColor` into a canopy straight-alpha `Color`.
fn absolute_to_color(c: AbsoluteColor) -> Color {
    let srgb = c.to_color_space(style::color::ColorSpace::Srgb);
    let [r, g, b, a] = *srgb.raw_components();
    Color {
        r: (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        g: (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        b: (b.clamp(0.0, 1.0) * 255.0).round() as u8,
        a: (a.clamp(0.0, 1.0) * 255.0).round() as u8,
    }
}

impl StyleEngine for StyloEngine {
    fn resolve(
        &mut self,
        node: NodeId,
        _parent: Option<&ComputedStyle>,
    ) -> Result<ComputedStyle, HostError> {
        self.resolve_styles();
        let id = node.raw() as usize;
        self.computed_style_for(id).ok_or(HostError::BadHandle)
    }
}

// ===========================================================================
// taffy_convert: Stylo `ComputedValues` -> `taffy::Style`.
//
// Adapted from Blitz's `packages/stylo_taffy/src/convert.rs` (public entry
// `to_taffy_style`). Blitz targets taffy `0.11.0-experimental-cache-fix.3`;
// stable taffy 0.11.0 ships a structurally identical `Style<S>` so the port is
// nearly verbatim. Differences from Blitz:
//   * `stylo::` alias -> `style::` (the crate is renamed `style` here).
//   * We use the DEFAULT `taffy::Style` (custom-ident = String) instead of
//     Blitz's `Style<Atom>`. Blitz's `Atom` generic is only reachable via its
//     own `TaffyStyloStyle` trait wrapper; `StyloEngine::layout` instead uses
//     taffy's `TaffyTree`, whose node `Style` is hard-wired to the default
//     `String` custom-ident (its `LayoutGridContainer` impl declares
//     `type CustomIdent = DefaultCheapStr`). So named grid lines/areas are
//     interned to `String` here; fixed/fr/named grids are otherwise identical.
//   * FLOAT properties are omitted (that taffy feature is off). FLEX + BLOCK +
//     GRID + the box-model subset (incl. `calc()`) are ported.
//   * `text_align` is set to the taffy default (`Auto`); we don't read stylo's
//     `text_align` because none of our flex/block geometry tests depend on it
//     and the accessor mapping isn't needed for the subset under test.
//   * Grid sub-features NOT supported (match Blitz / taffy 0.11 limits):
//     subgrid and masonry convert to empty/None.
// ===========================================================================

mod taffy_convert {
    //! Conversion from Stylo computed style to `taffy::Style` (flex + block + grid + calc).

    /// Stylo type aliases (Blitz names these `stylo::*`; the crate is `style`).
    mod stylo {
        pub(crate) use style::properties::generated::longhands::box_sizing::computed_value::T as BoxSizing;
        pub(crate) use style::properties::longhands::aspect_ratio::computed_value::T as AspectRatio;
        pub(crate) use style::properties::longhands::position::computed_value::T as Position;
        pub(crate) use style::properties::ComputedValues;
        pub(crate) use style::values::computed::length_percentage::CalcLengthPercentage;
        pub(crate) use style::values::computed::length_percentage::Unpacked as UnpackedLengthPercentage;
        pub(crate) use style::values::computed::{BorderSideWidth, LengthPercentage, Percentage};
        pub(crate) use style::values::generics::length::{
            GenericLengthPercentageOrNormal, GenericMargin, GenericMaxSize, GenericSize,
        };
        pub(crate) use style::values::generics::position::{Inset as GenericInset, PreferredRatio};
        pub(crate) use style::values::generics::NonNegative;
        pub(crate) use style::values::specified::align::{AlignFlags, ContentDistribution};
        pub(crate) use style::values::specified::border::BorderStyle;
        pub(crate) use style::values::specified::box_::{
            Display, DisplayInside, DisplayOutside, Overflow,
        };

        pub(crate) type MarginVal = GenericMargin<LengthPercentage>;
        pub(crate) type InsetVal = GenericInset<Percentage, LengthPercentage>;
        pub(crate) type Size = GenericSize<NonNegative<LengthPercentage>>;
        pub(crate) type MaxSize = GenericMaxSize<NonNegative<LengthPercentage>>;
        pub(crate) type Gap = GenericLengthPercentageOrNormal<NonNegative<LengthPercentage>>;

        // direction longhand
        pub(crate) use style::properties::generated::longhands::direction::computed_value::T as Direction;

        // flexbox
        pub(crate) use style::computed_values::{
            flex_direction::T as FlexDirection, flex_wrap::T as FlexWrap,
        };
        pub(crate) use style::values::generics::flex::GenericFlexBasis;
        pub(crate) type FlexBasis = GenericFlexBasis<Size>;

        // grid
        pub(crate) use style::computed_values::grid_auto_flow::T as GridAutoFlow;
        pub(crate) use style::values::computed::{
            GridLine, GridTemplateComponent, ImplicitGridTracks,
        };
        pub(crate) use style::values::generics::grid::{
            RepeatCount, TrackBreadth, TrackListValue, TrackSize,
        };
        pub(crate) use style::values::specified::position::{GridTemplateAreas, NamedArea};
        pub(crate) use style::values::specified::GenericGridTemplateComponent;
    }

    use taffy::style_helpers::*;
    use taffy::CompactLength;

    #[inline]
    pub fn length_percentage(val: &stylo::LengthPercentage) -> taffy::LengthPercentage {
        match val.unpack() {
            // Forward stylo's `calc()` pointer into taffy (Blitz's path): build a
            // `CompactLength::calc` from the opaque stylo `CalcLengthPercentage`
            // pointer and wrap it as a taffy `LengthPercentage`. Requires taffy's
            // non-default `calc` feature (enabled in Cargo.toml). Taffy treats the
            // pointer as an opaque handle and resolves it via stylo's own calc
            // representation at layout time.
            stylo::UnpackedLengthPercentage::Calc(calc_ptr) => {
                let val = CompactLength::calc(
                    calc_ptr as *const stylo::CalcLengthPercentage as *const (),
                );
                // SAFETY: `calc` is a valid `CompactLength` for a `LengthPercentage`.
                unsafe { taffy::LengthPercentage::from_raw(val) }
            }
            stylo::UnpackedLengthPercentage::Length(len) => length(len.px()),
            stylo::UnpackedLengthPercentage::Percentage(percentage) => percent(percentage.0),
        }
    }

    #[inline]
    pub fn dimension(val: &stylo::Size) -> taffy::Dimension {
        match val {
            stylo::Size::LengthPercentage(val) => length_percentage(&val.0).into(),
            stylo::Size::Auto => taffy::Dimension::AUTO,

            // TODO: implement other values in Taffy
            stylo::Size::MaxContent => taffy::Dimension::AUTO,
            stylo::Size::MinContent => taffy::Dimension::AUTO,
            stylo::Size::FitContent => taffy::Dimension::AUTO,
            stylo::Size::FitContentFunction(_) => taffy::Dimension::AUTO,
            stylo::Size::Stretch => taffy::Dimension::AUTO,
            stylo::Size::WebkitFillAvailable => taffy::Dimension::AUTO,

            // Anchor positioning is flagged off.
            stylo::Size::AnchorSizeFunction(_) => unreachable!(),
            stylo::Size::AnchorContainingCalcFunction(_) => unreachable!(),
        }
    }

    #[inline]
    pub fn max_size_dimension(val: &stylo::MaxSize) -> taffy::Dimension {
        match val {
            stylo::MaxSize::LengthPercentage(val) => length_percentage(&val.0).into(),
            stylo::MaxSize::None => taffy::Dimension::AUTO,

            stylo::MaxSize::MaxContent => taffy::Dimension::AUTO,
            stylo::MaxSize::MinContent => taffy::Dimension::AUTO,
            stylo::MaxSize::FitContent => taffy::Dimension::AUTO,
            stylo::MaxSize::FitContentFunction(_) => taffy::Dimension::AUTO,
            stylo::MaxSize::Stretch => taffy::Dimension::AUTO,
            stylo::MaxSize::WebkitFillAvailable => taffy::Dimension::AUTO,

            stylo::MaxSize::AnchorSizeFunction(_) => unreachable!(),
            stylo::MaxSize::AnchorContainingCalcFunction(_) => unreachable!(),
        }
    }

    #[inline]
    pub fn margin(val: &stylo::MarginVal) -> taffy::LengthPercentageAuto {
        match val {
            stylo::MarginVal::Auto => taffy::LengthPercentageAuto::AUTO,
            stylo::MarginVal::LengthPercentage(val) => length_percentage(val).into(),

            stylo::MarginVal::AnchorSizeFunction(_) => unreachable!(),
            stylo::MarginVal::AnchorContainingCalcFunction(_) => unreachable!(),
        }
    }

    #[inline]
    pub fn border(
        width: &stylo::BorderSideWidth,
        style: stylo::BorderStyle,
    ) -> taffy::LengthPercentage {
        if style.none_or_hidden() {
            return taffy::style_helpers::zero();
        }
        taffy::style_helpers::length(width.0.to_f32_px())
    }

    #[inline]
    pub fn inset(val: &stylo::InsetVal) -> taffy::LengthPercentageAuto {
        match val {
            stylo::InsetVal::Auto => taffy::LengthPercentageAuto::AUTO,
            stylo::InsetVal::LengthPercentage(val) => length_percentage(val).into(),

            stylo::InsetVal::AnchorSizeFunction(_) => unreachable!(),
            stylo::InsetVal::AnchorFunction(_) => unreachable!(),
            stylo::InsetVal::AnchorContainingCalcFunction(_) => unreachable!(),
        }
    }

    #[inline]
    pub fn display(input: stylo::Display) -> taffy::Display {
        let mut display = match input.inside() {
            stylo::DisplayInside::None => taffy::Display::None,
            stylo::DisplayInside::Flex => taffy::Display::Flex,
            stylo::DisplayInside::Grid => taffy::Display::Grid,
            stylo::DisplayInside::Flow => taffy::Display::Block,
            stylo::DisplayInside::FlowRoot => taffy::Display::Block,
            stylo::DisplayInside::TableCell => taffy::Display::Block,
            // taffy has no table layout; approximate with grid (matches Blitz).
            stylo::DisplayInside::Table => taffy::Display::Grid,
            _ => taffy::Display::DEFAULT,
        };

        match input.outside() {
            stylo::DisplayOutside::None => display = taffy::Display::None,
            stylo::DisplayOutside::Inline => {}
            stylo::DisplayOutside::Block => {}
            stylo::DisplayOutside::TableCaption => {}
            stylo::DisplayOutside::InternalTable => {}
        };

        display
    }

    #[inline]
    pub fn box_sizing(input: stylo::BoxSizing) -> taffy::BoxSizing {
        match input {
            stylo::BoxSizing::BorderBox => taffy::BoxSizing::BorderBox,
            stylo::BoxSizing::ContentBox => taffy::BoxSizing::ContentBox,
        }
    }

    #[inline]
    pub fn position(input: stylo::Position) -> taffy::Position {
        match input {
            stylo::Position::Relative => taffy::Position::Relative,
            stylo::Position::Static => taffy::Position::Relative,
            stylo::Position::Absolute => taffy::Position::Absolute,
            stylo::Position::Fixed => taffy::Position::Absolute,
            stylo::Position::Sticky => taffy::Position::Relative,
        }
    }

    #[inline]
    pub fn overflow(input: stylo::Overflow) -> taffy::Overflow {
        match input {
            stylo::Overflow::Visible => taffy::Overflow::Visible,
            stylo::Overflow::Clip => taffy::Overflow::Clip,
            stylo::Overflow::Hidden => taffy::Overflow::Hidden,
            stylo::Overflow::Scroll => taffy::Overflow::Scroll,
            // TODO: Support Overflow::Auto in Taffy
            stylo::Overflow::Auto => taffy::Overflow::Scroll,
        }
    }

    #[inline]
    pub fn direction(input: stylo::Direction) -> taffy::Direction {
        match input {
            stylo::Direction::Ltr => taffy::Direction::Ltr,
            stylo::Direction::Rtl => taffy::Direction::Rtl,
        }
    }

    #[inline]
    pub fn aspect_ratio(input: stylo::AspectRatio) -> Option<f32> {
        match input.ratio {
            stylo::PreferredRatio::None => None,
            stylo::PreferredRatio::Ratio(val) => Some(val.0 .0 / val.1 .0),
        }
    }

    // NOTE: stable taffy 0.11.0 models `AlignItems`/`AlignContent` as STRUCTS
    // with associated SCREAMING_CASE consts (`AlignItems::FLEX_START`), not the
    // enum variants (`AlignItems::FlexStart`) of Blitz's experimental fork. The
    // mapping is mechanical; the variant set is identical.
    #[inline]
    pub fn content_alignment(input: stylo::ContentDistribution) -> Option<taffy::AlignContent> {
        match input.primary().value() {
            stylo::AlignFlags::NORMAL => None,
            stylo::AlignFlags::AUTO => None,
            stylo::AlignFlags::START => Some(taffy::AlignContent::START),
            stylo::AlignFlags::END => Some(taffy::AlignContent::END),
            stylo::AlignFlags::LEFT => Some(taffy::AlignContent::START),
            stylo::AlignFlags::RIGHT => Some(taffy::AlignContent::END),
            stylo::AlignFlags::FLEX_START => Some(taffy::AlignContent::FLEX_START),
            stylo::AlignFlags::STRETCH => Some(taffy::AlignContent::STRETCH),
            stylo::AlignFlags::FLEX_END => Some(taffy::AlignContent::FLEX_END),
            stylo::AlignFlags::CENTER => Some(taffy::AlignContent::CENTER),
            stylo::AlignFlags::SPACE_BETWEEN => Some(taffy::AlignContent::SPACE_BETWEEN),
            stylo::AlignFlags::SPACE_AROUND => Some(taffy::AlignContent::SPACE_AROUND),
            stylo::AlignFlags::SPACE_EVENLY => Some(taffy::AlignContent::SPACE_EVENLY),
            _ => None,
        }
    }

    #[inline]
    pub fn item_alignment(input: stylo::AlignFlags) -> Option<taffy::AlignItems> {
        match input.value() {
            stylo::AlignFlags::AUTO => None,
            stylo::AlignFlags::NORMAL => Some(taffy::AlignItems::STRETCH),
            stylo::AlignFlags::STRETCH => Some(taffy::AlignItems::STRETCH),
            stylo::AlignFlags::FLEX_START => Some(taffy::AlignItems::FLEX_START),
            stylo::AlignFlags::FLEX_END => Some(taffy::AlignItems::FLEX_END),
            stylo::AlignFlags::SELF_START => Some(taffy::AlignItems::START),
            stylo::AlignFlags::SELF_END => Some(taffy::AlignItems::END),
            stylo::AlignFlags::START => Some(taffy::AlignItems::START),
            stylo::AlignFlags::END => Some(taffy::AlignItems::END),
            stylo::AlignFlags::LEFT => Some(taffy::AlignItems::START),
            stylo::AlignFlags::RIGHT => Some(taffy::AlignItems::END),
            stylo::AlignFlags::CENTER => Some(taffy::AlignItems::CENTER),
            stylo::AlignFlags::BASELINE => Some(taffy::AlignItems::BASELINE),
            _ => None,
        }
    }

    #[inline]
    pub fn gap(input: &stylo::Gap) -> taffy::LengthPercentage {
        match input {
            stylo::Gap::Normal => taffy::LengthPercentage::ZERO,
            stylo::Gap::LengthPercentage(val) => length_percentage(&val.0),
        }
    }

    #[inline]
    pub fn flex_basis(input: &stylo::FlexBasis) -> taffy::Dimension {
        // TODO: Support flex-basis: content in Taffy
        match input {
            stylo::FlexBasis::Content => taffy::Dimension::AUTO,
            stylo::FlexBasis::Size(size) => dimension(size),
        }
    }

    #[inline]
    pub fn flex_direction(input: stylo::FlexDirection) -> taffy::FlexDirection {
        match input {
            stylo::FlexDirection::Row => taffy::FlexDirection::Row,
            stylo::FlexDirection::RowReverse => taffy::FlexDirection::RowReverse,
            stylo::FlexDirection::Column => taffy::FlexDirection::Column,
            stylo::FlexDirection::ColumnReverse => taffy::FlexDirection::ColumnReverse,
        }
    }

    #[inline]
    pub fn flex_wrap(input: stylo::FlexWrap) -> taffy::FlexWrap {
        match input {
            stylo::FlexWrap::Wrap => taffy::FlexWrap::Wrap,
            stylo::FlexWrap::WrapReverse => taffy::FlexWrap::WrapReverse,
            stylo::FlexWrap::Nowrap => taffy::FlexWrap::NoWrap,
        }
    }

    // CSS Grid styles
    // ===============
    //
    // Ported from Blitz's `convert.rs` (struct-literal `to_taffy_style`). Taffy's
    // grid types are generic over the custom-ident string type `S`; we thread
    // stylo's interned `Atom` through as `S` so named grid lines/areas are
    // preserved. Subgrid and masonry are not supported (taffy doesn't implement
    // them); they convert to empty/None as Blitz does.

    #[inline]
    pub fn grid_auto_flow(input: stylo::GridAutoFlow) -> taffy::GridAutoFlow {
        let is_row = input.contains(stylo::GridAutoFlow::ROW);
        let is_dense = input.contains(stylo::GridAutoFlow::DENSE);

        match (is_row, is_dense) {
            (true, false) => taffy::GridAutoFlow::Row,
            (true, true) => taffy::GridAutoFlow::RowDense,
            (false, false) => taffy::GridAutoFlow::Column,
            (false, true) => taffy::GridAutoFlow::ColumnDense,
        }
    }

    #[inline]
    pub fn grid_line(input: &stylo::GridLine) -> taffy::GridPlacement {
        // The empty atom marks "no named line" (stylo uses `atom!("")`).
        let empty = style::Atom::default();
        if input.is_auto() {
            taffy::GridPlacement::Auto
        } else if input.is_span {
            if input.ident.0 != empty {
                taffy::GridPlacement::NamedSpan(
                    input.ident.0.to_string(),
                    input.line_num.try_into().unwrap(),
                )
            } else {
                taffy::GridPlacement::Span(input.line_num as u16)
            }
        } else if input.ident.0 != empty {
            taffy::GridPlacement::NamedLine(input.ident.0.to_string(), input.line_num as i16)
        } else if input.line_num != 0 {
            taffy::style_helpers::line(input.line_num as i16)
        } else {
            taffy::GridPlacement::Auto
        }
    }

    #[inline]
    pub fn grid_template_tracks(
        input: &stylo::GridTemplateComponent,
    ) -> Vec<taffy::GridTemplateComponent<String>> {
        match input {
            stylo::GenericGridTemplateComponent::None => Vec::new(),
            stylo::GenericGridTemplateComponent::TrackList(list) => list
                .values
                .iter()
                .map(|track| match track {
                    stylo::TrackListValue::TrackSize(size) => {
                        taffy::GridTemplateComponent::Single(track_size(size))
                    }
                    stylo::TrackListValue::TrackRepeat(repeat) => {
                        taffy::GridTemplateComponent::Repeat(taffy::GridTemplateRepetition {
                            count: track_repeat(repeat.count),
                            tracks: repeat.track_sizes.iter().map(track_size).collect(),
                            line_names: repeat
                                .line_names
                                .iter()
                                .map(|line_name_set| {
                                    line_name_set
                                        .iter()
                                        .map(|ident| ident.0.to_string())
                                        .collect::<Vec<_>>()
                                })
                                .collect::<Vec<_>>(),
                        })
                    }
                })
                .collect(),

            // Subgrid and masonry are not supported by taffy.
            stylo::GenericGridTemplateComponent::Subgrid(_) => Vec::new(),
            stylo::GenericGridTemplateComponent::Masonry => Vec::new(),
        }
    }

    /// The per-track `<line-names>` of a template component, as
    /// `Vec<Vec<String>>` (one set per grid line). Empty when there are no names.
    #[inline]
    pub fn grid_template_line_names(input: &stylo::GridTemplateComponent) -> Vec<Vec<String>> {
        match input {
            stylo::GenericGridTemplateComponent::TrackList(list) => list
                .line_names
                .iter()
                .map(|set| {
                    set.iter()
                        .map(|ident| ident.0.to_string())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            // None / subgrid / masonry carry no taffy-representable line names.
            stylo::GenericGridTemplateComponent::None
            | stylo::GenericGridTemplateComponent::Subgrid(_)
            | stylo::GenericGridTemplateComponent::Masonry => Vec::new(),
        }
    }

    #[inline]
    pub fn grid_template_area(input: &stylo::NamedArea) -> taffy::GridTemplateArea<String> {
        taffy::GridTemplateArea {
            name: input.name.to_string(),
            row_start: input.rows.start as u16,
            row_end: input.rows.end as u16,
            column_start: input.columns.start as u16,
            column_end: input.columns.end as u16,
        }
    }

    #[inline]
    pub fn grid_template_areas(
        input: &stylo::GridTemplateAreas,
    ) -> Vec<taffy::GridTemplateArea<String>> {
        match input {
            stylo::GridTemplateAreas::None => Vec::new(),
            stylo::GridTemplateAreas::Areas(template_areas_arc) => template_areas_arc
                .0
                .areas
                .iter()
                .map(grid_template_area)
                .collect(),
        }
    }

    #[inline]
    pub fn grid_auto_tracks(input: &stylo::ImplicitGridTracks) -> Vec<taffy::TrackSizingFunction> {
        input.0.iter().map(track_size).collect()
    }

    #[inline]
    pub fn track_repeat(input: stylo::RepeatCount<i32>) -> taffy::RepetitionCount {
        match input {
            stylo::RepeatCount::Number(val) => {
                taffy::RepetitionCount::Count(val.try_into().unwrap())
            }
            stylo::RepeatCount::AutoFill => taffy::RepetitionCount::AutoFill,
            stylo::RepeatCount::AutoFit => taffy::RepetitionCount::AutoFit,
        }
    }

    #[inline]
    pub fn track_size(
        input: &stylo::TrackSize<stylo::LengthPercentage>,
    ) -> taffy::TrackSizingFunction {
        use taffy::MaxTrackSizingFunction;

        match input {
            stylo::TrackSize::Breadth(breadth) => taffy::MinMax {
                min: min_track(breadth),
                max: max_track(breadth),
            },
            stylo::TrackSize::Minmax(min, max) => taffy::MinMax {
                min: min_track(min),
                max: max_track(max),
            },
            stylo::TrackSize::FitContent(limit) => taffy::MinMax {
                min: taffy::MinTrackSizingFunction::AUTO,
                max: match limit {
                    stylo::TrackBreadth::Breadth(lp) => {
                        MaxTrackSizingFunction::fit_content(length_percentage(lp))
                    }

                    // These are not valid inside fit-content() and taffy
                    // wouldn't support them anyway.
                    stylo::TrackBreadth::Fr(_) => unreachable!(),
                    stylo::TrackBreadth::Auto => unreachable!(),
                    stylo::TrackBreadth::MinContent => unreachable!(),
                    stylo::TrackBreadth::MaxContent => unreachable!(),
                },
            },
        }
    }

    #[inline]
    pub fn min_track(
        input: &stylo::TrackBreadth<stylo::LengthPercentage>,
    ) -> taffy::MinTrackSizingFunction {
        match input {
            stylo::TrackBreadth::Breadth(lp) => {
                taffy::MinTrackSizingFunction::from(length_percentage(lp))
            }
            stylo::TrackBreadth::Fr(_) => taffy::MinTrackSizingFunction::AUTO,
            stylo::TrackBreadth::Auto => taffy::MinTrackSizingFunction::AUTO,
            stylo::TrackBreadth::MinContent => taffy::MinTrackSizingFunction::MIN_CONTENT,
            stylo::TrackBreadth::MaxContent => taffy::MinTrackSizingFunction::MAX_CONTENT,
        }
    }

    #[inline]
    pub fn max_track(
        input: &stylo::TrackBreadth<stylo::LengthPercentage>,
    ) -> taffy::MaxTrackSizingFunction {
        use taffy::prelude::FromFr;

        match input {
            stylo::TrackBreadth::Breadth(lp) => {
                taffy::MaxTrackSizingFunction::from(length_percentage(lp))
            }
            stylo::TrackBreadth::Fr(val) => taffy::MaxTrackSizingFunction::from_fr(*val),
            stylo::TrackBreadth::Auto => taffy::MaxTrackSizingFunction::AUTO,
            stylo::TrackBreadth::MinContent => taffy::MaxTrackSizingFunction::MIN_CONTENT,
            stylo::TrackBreadth::MaxContent => taffy::MaxTrackSizingFunction::MAX_CONTENT,
        }
    }

    /// Eagerly convert an entire `ComputedValues` into a `taffy::Style`
    /// (flex + block + grid + box-model subset; float omitted).
    ///
    /// Returns the DEFAULT `taffy::Style` (custom-ident = `String`) rather than
    /// Blitz's `Style<Atom>`: taffy's `TaffyTree` (which `StyloEngine::layout`
    /// uses) hard-wires its node `Style` to the default `String` custom-ident
    /// (its `LayoutGridContainer` impl is `type CustomIdent = DefaultCheapStr`),
    /// so named grid lines/areas are interned to `String` here. Blitz's `Atom`
    /// generic is only reachable via its own `TaffyStyloStyle` trait wrapper,
    /// not `TaffyTree`. Fixed/fr/named grids are unaffected.
    pub fn to_taffy_style(style: &stylo::ComputedValues) -> taffy::Style {
        let display = style.clone_display();
        let pos = style.get_position();
        let margin = style.get_margin();
        let padding = style.get_padding();
        let border = style.get_border();

        taffy::Style {
            dummy: core::marker::PhantomData,
            display: self::display(display),
            box_sizing: self::box_sizing(style.clone_box_sizing()),
            item_is_table: display.inside() == stylo::DisplayInside::Table,
            item_is_replaced: false,
            position: self::position(style.clone_position()),
            overflow: taffy::Point {
                x: self::overflow(style.clone_overflow_x()),
                y: self::overflow(style.clone_overflow_y()),
            },
            direction: self::direction(style.clone_direction()),
            scrollbar_width: 0.0,

            size: taffy::Size {
                width: self::dimension(&pos.width),
                height: self::dimension(&pos.height),
            },
            min_size: taffy::Size {
                width: self::dimension(&pos.min_width),
                height: self::dimension(&pos.min_height),
            },
            max_size: taffy::Size {
                width: self::max_size_dimension(&pos.max_width),
                height: self::max_size_dimension(&pos.max_height),
            },
            aspect_ratio: self::aspect_ratio(pos.aspect_ratio),

            inset: taffy::Rect {
                left: self::inset(&pos.left),
                right: self::inset(&pos.right),
                top: self::inset(&pos.top),
                bottom: self::inset(&pos.bottom),
            },
            margin: taffy::Rect {
                left: self::margin(&margin.margin_left),
                right: self::margin(&margin.margin_right),
                top: self::margin(&margin.margin_top),
                bottom: self::margin(&margin.margin_bottom),
            },
            padding: taffy::Rect {
                left: self::length_percentage(&padding.padding_left.0),
                right: self::length_percentage(&padding.padding_right.0),
                top: self::length_percentage(&padding.padding_top.0),
                bottom: self::length_percentage(&padding.padding_bottom.0),
            },
            border: taffy::Rect {
                left: self::border(&border.border_left_width, border.border_left_style),
                right: self::border(&border.border_right_width, border.border_right_style),
                top: self::border(&border.border_top_width, border.border_top_style),
                bottom: self::border(&border.border_bottom_width, border.border_bottom_style),
            },

            // Gap
            gap: taffy::Size {
                width: self::gap(&pos.column_gap),
                height: self::gap(&pos.row_gap),
            },

            // Alignment
            align_content: self::content_alignment(pos.align_content),
            justify_content: self::content_alignment(pos.justify_content),
            align_items: self::item_alignment(pos.align_items.0),
            align_self: self::item_alignment(pos.align_self.0),
            // Grid-only inline-axis alignment (gated by taffy's `grid` feature).
            justify_items: self::item_alignment((pos.justify_items.computed.0).0),
            justify_self: self::item_alignment(pos.justify_self.0),

            // Block container: keep taffy default (Auto). Not read from stylo
            // because no flex/block geometry test depends on text-align.
            text_align: taffy::TextAlign::Auto,

            // Flexbox
            flex_direction: self::flex_direction(pos.flex_direction),
            flex_wrap: self::flex_wrap(pos.flex_wrap),
            flex_grow: pos.flex_grow.0,
            flex_shrink: pos.flex_shrink.0,
            flex_basis: self::flex_basis(&pos.flex_basis),

            // Grid
            grid_auto_flow: self::grid_auto_flow(pos.grid_auto_flow),
            grid_template_rows: self::grid_template_tracks(&pos.grid_template_rows),
            grid_template_columns: self::grid_template_tracks(&pos.grid_template_columns),
            grid_template_row_names: self::grid_template_line_names(&pos.grid_template_rows),
            grid_template_column_names: self::grid_template_line_names(&pos.grid_template_columns),
            grid_template_areas: self::grid_template_areas(&pos.grid_template_areas),
            grid_auto_rows: self::grid_auto_tracks(&pos.grid_auto_rows),
            grid_auto_columns: self::grid_auto_tracks(&pos.grid_auto_columns),
            grid_row: taffy::Line {
                start: self::grid_line(&pos.grid_row_start),
                end: self::grid_line(&pos.grid_row_end),
            },
            grid_column: taffy::Line {
                start: self::grid_line(&pos.grid_column_start),
                end: self::grid_line(&pos.grid_column_end),
            },
        }
    }
}

// ===========================================================================
// Layout pass: build a taffy tree mirroring the arena's ELEMENT tree, run
// taffy, and return absolute border-box rects in cascade DFS order.
// ===========================================================================

impl StyloEngine {
    /// Return the slab ids of the document's elements in the SAME DFS order the
    /// cascade visits them (pre-order from the root element). Text nodes skipped.
    fn element_dfs_order(&self) -> Vec<usize> {
        let mut order = Vec::new();
        // root = first element child of node 0 (matches `resolve_styles`).
        let root = self.doc.nodes[0]
            .children
            .iter()
            .copied()
            .find(|&id| self.doc.nodes[id].is_element());
        if let Some(root) = root {
            let mut stack = vec![root];
            // We want pre-order DFS with children in document order, so push in
            // reverse onto the stack.
            while let Some(id) = stack.pop() {
                order.push(id);
                let kids: Vec<usize> = self.doc.nodes[id]
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| self.doc.nodes[c].is_element())
                    .collect();
                for &c in kids.iter().rev() {
                    stack.push(c);
                }
            }
        }
        order
    }

    /// Compute layout for the whole element tree.
    ///
    /// Builds a [`taffy::TaffyTree`] with one node per element (text nodes are
    /// skipped — leaves get their size purely from their `Style`, NO text
    /// measurement), runs flex/block layout against `viewport`, then walks the
    /// tree accumulating each node's parent-relative `location` into an ABSOLUTE
    /// border-box [`Rect`]. The returned vec is in the SAME DFS element order as
    /// the cascade's resolve order, so callers can zip cascade styles and layout
    /// boxes by index.
    pub fn layout(&mut self, viewport: canopy_traits::Size) -> Vec<canopy_traits::Rect> {
        use canopy_traits::{Point, Rect, Size};

        self.resolve_styles();

        let order = self.element_dfs_order();
        if order.is_empty() {
            return Vec::new();
        }

        // Map: slab id -> index into `order` (its position in DFS order).
        let mut slab_to_idx = std::collections::HashMap::new();
        for (i, &slab) in order.iter().enumerate() {
            slab_to_idx.insert(slab, i);
        }

        let mut tree: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        // taffy node handle per element index.
        let mut taffy_nodes: Vec<taffy::NodeId> = Vec::with_capacity(order.len());

        // First pass: create a leaf taffy node for each element with its style.
        for &slab in &order {
            let style = self
                .computed_values_for(slab)
                .map(|cv| taffy_convert::to_taffy_style(&cv))
                .unwrap_or_default();
            let node = tree.new_leaf(style).expect("taffy new_leaf");
            taffy_nodes.push(node);
        }

        // Second pass: wire children (element children only, in document order).
        for (i, &slab) in order.iter().enumerate() {
            let child_handles: Vec<taffy::NodeId> = self.doc.nodes[slab]
                .children
                .iter()
                .copied()
                .filter(|c| self.doc.nodes[*c].is_element())
                .map(|c| taffy_nodes[slab_to_idx[&c]])
                .collect();
            if !child_handles.is_empty() {
                tree.set_children(taffy_nodes[i], &child_handles)
                    .expect("taffy set_children");
            }
        }

        let root = taffy_nodes[0];
        tree.compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(viewport.w),
                height: taffy::AvailableSpace::Definite(viewport.h),
            },
        )
        .expect("taffy compute_layout");

        // Walk the taffy tree, accumulating absolute origins. Each node's
        // `location` is relative to its parent's content box origin... taffy's
        // `Layout::location` is relative to the parent's border-box origin, so
        // absolute = parent_absolute + location.
        let mut rects = vec![Rect::default(); order.len()];
        // Stack of (element index, parent absolute origin).
        let mut stack = vec![(0usize, 0.0f32, 0.0f32)];
        while let Some((idx, px, py)) = stack.pop() {
            let l = tree.layout(taffy_nodes[idx]).expect("taffy layout");
            let ax = px + l.location.x;
            let ay = py + l.location.y;
            rects[idx] = Rect {
                origin: Point { x: ax, y: ay },
                size: Size {
                    w: l.size.width,
                    h: l.size.height,
                },
            };
            // Push element children with this node's absolute origin as their base.
            let slab = order[idx];
            for c in self.doc.nodes[slab]
                .children
                .iter()
                .copied()
                .filter(|c| self.doc.nodes[*c].is_element())
            {
                stack.push((slab_to_idx[&c], ax, ay));
            }
        }

        rects
    }

    /// Borrow the primary `ComputedValues` for an element slab id (post-resolve).
    fn computed_values_for(&self, node_id: usize) -> Option<servo_arc::Arc<ComputedValues>> {
        let node = self.doc.nodes.get(node_id)?;
        if !node.is_element() {
            return None;
        }
        let data = node.stylo_element_data.get()?;
        let styles = data.styles.get_primary()?;
        Some(styles.clone())
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(engine: &mut StyloEngine, id: usize) -> ComputedStyle {
        engine
            .resolve(NodeId::new(id as u64), None)
            .expect("resolve failed")
    }

    #[test]
    fn inheritance() {
        // .page { color: #ff0000 } on an ancestor; a descendant with no color
        // of its own inherits red through the tree.
        let mut engine = StyloEngine::new(".page { color: #ff0000 }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let page = doc.add_element(html, "div", None, &["page"]);
        let child = doc.add_element(page, "span", None, &[]);
        let _txt = doc.add_text(child, "hi");

        let style = resolve(&mut engine, child);
        assert_eq!(
            style.color,
            Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            },
            "descendant should inherit red"
        );
    }

    #[test]
    fn specificity_id_class_type() {
        // div { #000 } .x { #00ff00 } #y { #0000ff } -> <div class=x id=y> is blue.
        let mut engine =
            StyloEngine::new("div { color:#000000 } .x { color:#00ff00 } #y { color:#0000ff }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let el = doc.add_element(html, "div", Some("y"), &["x"]);

        let style = resolve(&mut engine, el);
        assert_eq!(
            style.color,
            Color {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            },
            "id should beat class should beat type"
        );
    }

    #[test]
    fn specificity_two_classes() {
        // .a.b { green } .a { red } -> element with both classes is green.
        let mut engine = StyloEngine::new(".a.b { color:#00ff00 } .a { color:#ff0000 }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let el = doc.add_element(html, "div", None, &["a", "b"]);

        let style = resolve(&mut engine, el);
        assert_eq!(
            style.color,
            Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            },
            ".a.b (specificity 0,2,0) should beat .a (0,1,0)"
        );
    }

    #[test]
    fn descendant_combinator() {
        // .card .title { background:#112233 } applies only to a .title nested
        // under a .card, not to a .title outside.
        let mut engine = StyloEngine::new(".card .title { background:#112233 }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);

        // .card > .wrap > .title  (nested at depth >1 to prove "any depth")
        let card = doc.add_element(html, "div", None, &["card"]);
        let wrap = doc.add_element(card, "div", None, &[]);
        let inside = doc.add_element(wrap, "div", None, &["title"]);

        // a .title outside any .card
        let outside = doc.add_element(html, "div", None, &["title"]);

        let inside_style = resolve(&mut engine, inside);
        assert_eq!(
            inside_style.background,
            Color {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 255
            },
            ".title under .card should get the background"
        );

        let outside_style = resolve(&mut engine, outside);
        assert_eq!(
            outside_style.background.a, 0,
            ".title outside .card should be transparent (no background)"
        );
    }

    #[test]
    fn value_extraction() {
        // font-size: 24px; padding: 8px; display: flex
        let mut engine = StyloEngine::new(".box { font-size: 24px; padding: 8px; display: flex }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let el = doc.add_element(html, "div", None, &["box"]);

        let style = resolve(&mut engine, el);
        assert!(
            (style.font_size - 24.0).abs() < 0.5,
            "font_size should be ~24, got {}",
            style.font_size
        );
        assert!(
            (style.padding - 8.0).abs() < 0.5,
            "padding should be ~8, got {}",
            style.padding
        );
        assert_eq!(style.display, Display::Flex, "display should be Flex");
    }

    // -----------------------------------------------------------------------
    // Layout smoke tests (Stylo cascade -> taffy_convert -> taffy layout).
    //
    // All geometry is driven by EXPLICIT sizes / flex ratios, never text
    // content. Boxes are returned in cascade DFS element order, so index 0 is
    // always the root <html>, index 1 the first element child, etc. We assert
    // within +/-1px.
    // -----------------------------------------------------------------------

    use canopy_traits::Size as TSize;

    /// Approximate-equality helper for f32 geometry (+/-1px).
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1.0
    }

    #[test]
    fn layout_flex_row_two_children() {
        // display:flex; width:200; height:100 container, two `flex:1 1 0`
        // children -> each 100x100, at (0,0) and (100,0); container 200x100 @ (0,0).
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:flex; width:200px; height:100px");
            let a = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(a, "flex-grow:1; flex-shrink:1; flex-basis:0");
            let b = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(b, "flex-grow:1; flex-shrink:1; flex-basis:0");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + 2 children");

        // index 0 = html container
        let c = rects[0];
        assert!(
            near(c.origin.x, 0.0) && near(c.origin.y, 0.0),
            "container origin {:?}",
            c.origin
        );
        assert!(
            near(c.size.w, 200.0) && near(c.size.h, 100.0),
            "container size {:?}",
            c.size
        );

        // index 1 = first child
        let a = rects[1];
        assert!(
            near(a.origin.x, 0.0) && near(a.origin.y, 0.0),
            "child A origin {:?}",
            a.origin
        );
        assert!(
            near(a.size.w, 100.0) && near(a.size.h, 100.0),
            "child A size {:?}",
            a.size
        );

        // index 2 = second child
        let b = rects[2];
        assert!(
            near(b.origin.x, 100.0) && near(b.origin.y, 0.0),
            "child B origin {:?}",
            b.origin
        );
        assert!(
            near(b.size.w, 100.0) && near(b.size.h, 100.0),
            "child B size {:?}",
            b.size
        );
    }

    #[test]
    fn layout_block_padding_child() {
        // block width:300; padding:20 containing child width:100 height:40.
        // child border-box origin = (20,20), size 100x40 (absolute).
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:300px; padding:20px");
            let child = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(child, "display:block; width:100px; height:40px");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 2);

        let child = rects[1];
        assert!(
            near(child.origin.x, 20.0),
            "child x should be 20 (padding-left), got {}",
            child.origin.x
        );
        assert!(
            near(child.origin.y, 20.0),
            "child y should be 20 (padding-top), got {}",
            child.origin.y
        );
        assert!(near(child.size.w, 100.0), "child w {}", child.size.w);
        assert!(near(child.size.h, 40.0), "child h {}", child.size.h);
    }

    #[test]
    fn layout_justify_content_center() {
        // justify-content:center on a 200-wide row with one 40-wide child:
        // free space = 160, child x ~= 80.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(
                html,
                "display:flex; width:200px; height:100px; justify-content:center",
            );
            let child = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(child, "width:40px; height:20px; flex-shrink:0");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 2);

        let child = rects[1];
        assert!(
            near(child.origin.x, 80.0),
            "centered child x should be ~80, got {}",
            child.origin.x
        );
        assert!(near(child.size.w, 40.0), "child w {}", child.size.w);
    }

    #[test]
    fn layout_margin_left() {
        // child with margin-left:30 inside a block -> child x ~= 30.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:300px; height:200px");
            let child = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(
                child,
                "display:block; width:100px; height:40px; margin-left:30px",
            );
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 2);

        let child = rects[1];
        assert!(
            near(child.origin.x, 30.0),
            "child x should be ~30 (margin-left), got {}",
            child.origin.x
        );
        assert!(near(child.size.w, 100.0), "child w {}", child.size.w);
    }

    #[test]
    fn layout_calc_width() {
        // width:calc(50px + 10px) -> resolved width 60px. Exercises the restored
        // `calc()` pointer path in `length_percentage` (taffy `calc` feature).
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:calc(50px + 10px); height:40px");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 1, "just the html box");

        let c = rects[0];
        assert!(
            near(c.size.w, 60.0),
            "calc(50px + 10px) should resolve to ~60, got {}",
            c.size.w
        );
    }

    #[test]
    fn layout_grid_two_columns() {
        // display:grid; grid-template-columns:100px 100px; width:200px container
        // with two children -> child 0 at x~=0 w~=100, child 1 at x~=100 w~=100.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(
                html,
                "display:grid; grid-template-columns:100px 100px; width:200px; height:50px",
            );
            let _a = doc.add_element(html, "div", None, &[]);
            let _b = doc.add_element(html, "div", None, &[]);
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + 2 grid children");

        // index 0 = grid container
        let c = rects[0];
        assert!(
            near(c.size.w, 200.0),
            "grid container w should be ~200, got {}",
            c.size.w
        );

        // index 1 = first child: column 1 (0..100)
        let a = rects[1];
        assert!(
            near(a.origin.x, 0.0),
            "grid child 0 x should be ~0, got {}",
            a.origin.x
        );
        assert!(
            near(a.size.w, 100.0),
            "grid child 0 w should be ~100, got {}",
            a.size.w
        );

        // index 2 = second child: column 2 (100..200)
        let b = rects[2];
        assert!(
            near(b.origin.x, 100.0),
            "grid child 1 x should be ~100, got {}",
            b.origin.x
        );
        assert!(
            near(b.size.w, 100.0),
            "grid child 1 w should be ~100, got {}",
            b.size.w
        );
    }
}
