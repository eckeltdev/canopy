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
use canopy_traits::{
    BoxShadow, Color, ComputedStyle, Display, GradientAxis, HostError, LinearGradient, StyleEngine,
};

/// Build a backend-neutral [`canopy_traits::DisplayList`] from the cascaded +
/// laid-out tree (the GPU/retained-scene sibling of [`paint`]).
pub mod display_list;
/// Parse real HTML into the arena [`Document`] (html5ever -> arena).
pub mod html;
/// L3 paint: rasterize the cascaded + laid-out tree to pixels.
pub mod paint;
/// Text measurement: shape a text leaf's content (via cosmic-text + Ahem) so an
/// auto-sized box leaves the size of its text during [`StyloEngine::layout`].
pub mod text_measure;

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

/// A plain-`String` view of one arena element's identity, for selector matching.
///
/// Returned by [`Document::element_infos`]. Carries the element's slab id, its
/// parent slab id (if any), tag name, optional `id`, and class list — everything a
/// small CSS selector matcher (tag / `.class` / `#id` / descendant combinator)
/// needs, without depending on the interned-`Atom` types the arena stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementInfo {
    /// Arena slab id of the element.
    pub slab: usize,
    /// Parent slab id, if any.
    pub parent: Option<usize>,
    /// Local tag name (lowercased HTML), e.g. `"div"`.
    pub tag: String,
    /// The `id` attribute, if present.
    pub id: Option<String>,
    /// The class tokens, in document order.
    pub classes: Vec<String>,
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

    /// Per-element descriptor (tag / id / classes / parent), as plain `String`s.
    ///
    /// The arena stores tag/id/classes as interned `markup5ever`/`style` `Atom`s,
    /// which a downstream crate (e.g. the WPT runner) can't name without depending
    /// on those crates. This accessor surfaces the same data as `String`s so a
    /// caller can run a small CSS-selector matcher over the tree (tag / `.class` /
    /// `#id` / descendant combinator) without re-implementing the arena. Returned
    /// in slab-id order; non-element nodes (the document root, text) are skipped.
    pub fn element_infos(&self) -> Vec<ElementInfo> {
        self.nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Element {
                    name,
                    id_attr,
                    classes,
                    ..
                } => Some(ElementInfo {
                    slab: n.id,
                    parent: n.parent,
                    tag: name.local.to_string(),
                    id: id_attr.as_ref().map(|a| a.to_string()),
                    classes: classes.iter().map(|c| c.to_string()).collect(),
                }),
                _ => None,
            })
            .collect()
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
    /// For the [`from_dom`](StyloEngine::from_dom) (capable-tier) path: maps a real
    /// `canopy_dom` node handle (`NodeId.raw()`) to its overlay slab id, so
    /// [`StyleEngine::resolve`] can answer for the real tree's nodes. Empty for the
    /// HTML/manual paths (which build the overlay with slab == handle).
    node_map: std::collections::HashMap<u64, usize>,
}

impl StyloEngine {
    /// Build an engine with the given author CSS. The DOM is then built via
    /// [`Document`] mutators on [`StyloEngine::document_mut`].
    pub fn new(css: &str) -> Self {
        // A minimal **user-agent stylesheet** — browsers ship one, and matching their
        // layout requires it. Block-level `display` defaults (a bare `<div>`'s CSS
        // *initial* `display` is `inline`, not `block`), `body { margin: 8px }` (the
        // offset every HTML page inherits), and `display: none` for non-rendered
        // elements. Appended at `Origin::UserAgent`, so any author rule still wins.
        const UA_STYLESHEET: &str = "\
html, body, div, p, section, article, header, footer, nav, main, aside, \
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dd, dt, table, figure, figcaption, \
blockquote, address, pre, form, fieldset, hr { display: block }
head, script, style, title, meta, link, base, noscript, template { display: none }
body { margin: 8px }
";

        // Stylo (in `servo` mode) gates `display: grid` parsing AND the
        // `grid-template-*` / `grid-auto-*` longhands behind the runtime pref
        // `layout.grid.enabled`, which defaults to false. Flip it on (process-
        // global atomic; idempotent) so the cascade keeps grid declarations.
        static_prefs::set_pref!("layout.grid.enabled", true);

        let device = Self::make_device();
        let mut stylist = Stylist::new(device, QuirksMode::NoQuirks);

        // The user-agent sheet first (lowest cascade origin), then the author sheet.
        let ua = Stylesheet::from_str(
            UA_STYLESHEET,
            dummy_url_data(),
            Origin::UserAgent,
            ServoArc::new(GLOBAL_GUARD.wrap(MediaList::empty())),
            GLOBAL_GUARD.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        );
        stylist.append_stylesheet(DocumentStyleSheet(ServoArc::new(ua)), &GLOBAL_GUARD.read());

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
            node_map: std::collections::HashMap::new(),
        }
    }

    /// Build an engine that cascades the **real** `canopy_dom` retained tree — the
    /// capable-tier path. Each element's CSS identity (tag name, id, classes) is read
    /// straight off the Dom (a capable-tier `Ui` carried it there via the op-stream),
    /// and `css` is the author stylesheet (e.g. `Ui::css_source()`).
    ///
    /// This is the production seam: a desktop/SBC host builds its `canopy_dom::Dom` from
    /// the op-stream, then `StyloEngine::from_dom(&dom, css)` gives it a real CSS cascade
    /// over that exact tree. [`StyleEngine::resolve`] answers for a `canopy_dom` node by
    /// mapping its handle to the overlay built here. Elements with no declared tag name
    /// default to `"div"` (class/id selectors carry the selectivity for app CSS).
    pub fn from_dom(dom: &canopy_dom::Dom, css: &str) -> Self {
        let mut engine = Self::new(css);
        let mut node_map = std::collections::HashMap::new();
        build_overlay(dom, canopy_dom::ROOT, 0, &mut engine.doc, &mut node_map);
        engine.node_map = node_map;
        engine
    }

    /// Build an engine from an already-parsed [`Document`] plus its author CSS.
    ///
    /// [`new`](StyloEngine::new) starts from an *empty* arena that the caller
    /// fills via [`document_mut`](StyloEngine::document_mut); this constructor
    /// instead injects a tree the caller already built (e.g. from
    /// [`html::parse_html_with_css`](crate::html::parse_html_with_css)) so the
    /// cascade/layout run over real parsed HTML. The CSS is parsed and appended
    /// as the author stylesheet exactly as in `new`.
    pub fn with_document(doc: Document, css: &str) -> Self {
        let mut engine = Self::new(css);
        engine.doc = doc;
        engine.resolved = false;
        engine
    }

    /// Convenience: parse `html` (harvesting its `<style>` CSS) and build an
    /// engine over the resulting tree in one step.
    ///
    /// Equivalent to calling [`html::parse_html_with_css`] and
    /// [`with_document`](StyloEngine::with_document); the `data-*` attributes are
    /// discarded. For attribute ("checkLayout") tests, call
    /// [`html::parse_html_with_css`] directly so you keep the `data-*` map.
    ///
    /// [`html::parse_html_with_css`]: crate::html::parse_html_with_css
    pub fn from_html(html: &str) -> Self {
        let (doc, css, _data) = crate::html::parse_html_with_css(html);
        Self::with_document(doc, &css)
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

    /// Hit-test a viewport-space point against the laid-out tree: return the
    /// **deepest** element whose absolute border-box contains `point`, as its
    /// arena slab id (`None` if the point is outside every box).
    ///
    /// Runs [`element_layout`](StyloEngine::element_layout) (which resolves +
    /// lays out the tree), then scans every element box. Because that vec is in
    /// pre-order DFS — parent **before** child — the last containing box found in
    /// the scan is the deepest one under the cursor, so a hover lands on the most
    /// specific element (a button rather than the page behind it). This is the
    /// hit-test that drives pointer `:hover` in the windowed browser.
    pub fn hit_test(
        &mut self,
        point: canopy_traits::Point,
        viewport: canopy_traits::Size,
    ) -> Option<usize> {
        let boxes = self.element_layout(viewport);
        let mut hit = None;
        for (slab, rect) in boxes {
            let x0 = rect.origin.x;
            let y0 = rect.origin.y;
            let x1 = x0 + rect.size.w;
            let y1 = y0 + rect.size.h;
            if point.x >= x0 && point.x < x1 && point.y >= y0 && point.y < y1 {
                // DFS is parent-before-child, so a later match is strictly deeper.
                hit = Some(slab);
            }
        }
        hit
    }

    /// Move the `:hover` element state to `slab` (or clear it entirely when
    /// `None`), then **force a full restyle** so the cascade re-runs and any
    /// `:hover` rules re-apply on the next resolve / layout / render.
    ///
    /// The cascade is one-shot and idempotent ([`resolve_styles`] early-returns
    /// once `resolved` is set, and every element caches its `ComputedValues` in
    /// `stylo_element_data`). Simply flipping the [`ElementState::HOVER`] bit
    /// would therefore change nothing visible: the cached styles are stale and
    /// nothing recomputes them. This DOM also carries no Stylo *snapshots*
    /// (`has_snapshot` is always false), so Stylo's normal state-change
    /// invalidation can't notice the `:hover` flip on its own.
    ///
    /// So we force the recascade by hand: clear `resolved`, and **clear every
    /// element's cached Stylo data**. With all data gone, the next
    /// [`resolve_styles`] re-runs the whole-tree traversal exactly as the very
    /// first resolve did (`element_needs_traversal` returns true for any element
    /// whose data has no styles, so the root and every descendant are visited
    /// and re-cascaded against the new element state).
    pub fn set_hover(&mut self, slab: Option<usize>) {
        // Clear HOVER wherever it currently sits, then set it on the target.
        for node in &mut self.doc.nodes {
            if node.is_element() {
                node.element_state.remove(ElementState::HOVER);
            }
        }
        if let Some(id) = slab {
            if let Some(node) = self.doc.nodes.get_mut(id) {
                if node.is_element() {
                    node.element_state.insert(ElementState::HOVER);
                }
            }
        }

        // Force the next resolve to re-run the full cascade: drop the resolved
        // flag AND every element's cached Stylo `ElementData`, so the traversal
        // re-cascades the whole tree against the updated element state.
        //
        // SAFETY: we hold `&mut self`, so there are no outstanding borrows of any
        // node's `stylo_element_data` (no traversal is in flight), satisfying
        // `StyloData::clear`'s contract.
        for node in &self.doc.nodes {
            if node.is_element() {
                unsafe { node.stylo_element_data.clear() };
            }
            node.selector_flags.set(ElementSelectorFlags::empty());
            node.unset_dirty_descendants();
            node.snapshot_handled.store(false, Ordering::SeqCst);
        }
        self.resolved = false;
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

    // border: uniform width / color / radius taken from the TOP edge + top-left
    // corner (the seam is uniform-only). The width longhand computes to a
    // `BorderSideWidth(Au)`; `.0.to_f32_px()` is the same accessor `taffy_convert`
    // uses. The color is a `computed::Color` (the same `GenericColor<Percentage>`
    // as `background-color`), resolved against the foreground `currentColor`.
    //
    // CRITICAL: `border-top-width` computes to the `medium` keyword (3px) even when
    // `border-style` is `none`/`hidden` — every element (incl. a bare `<div>` or
    // `<html>`) would then report a 3px frame. Gate the width on the style exactly
    // as `taffy_convert::border` does, so "no border-style" => width 0.
    let border = style.get_border();
    let border_width = if border.border_top_style.none_or_hidden() {
        0.0
    } else {
        border.border_top_width.0.to_f32_px()
    };
    let border_color = absolute_to_color(
        border
            .border_top_color
            .clone()
            .resolve_to_absolute(&style.clone_color()),
    );
    // `border-top-left-radius` computes to a `BorderCornerRadius<LengthPercentage>`
    // = `Size2D<LengthPercentage>`; take the horizontal (`.0.width`) component and
    // resolve a bare length to px (percentages have no box to resolve against here,
    // so they fall back to 0.0 like `padding` does above).
    let border_radius = match border.border_top_left_radius.0.width.0.unpack() {
        Unpacked::Length(l) => l.px(),
        Unpacked::Percentage(_) => 0.0,
        Unpacked::Calc(_) => 0.0,
    };

    // opacity: `effects` style struct, computed `Opacity = CSSFloat` (already a
    // straight f32 in [0,1]). Clamp defensively.
    let opacity = style.get_effects().opacity.clamp(0.0, 1.0);

    // font-family: is the FIRST family "Ahem" (case-insensitive)? Ahem is the
    // metrics-perfect WPT test font (every glyph is a solid 1em square). A renderer
    // without a real Ahem face draws each char as a filled `font_size` square in the
    // foreground color; this flag lets it do so without re-consulting Stylo. We read
    // the same first `FamilyName` the layout's text-measure context uses, so paint
    // and measurement agree on which elements are Ahem.
    let is_ahem = style
        .get_font()
        .font_family
        .families
        .iter()
        .find_map(|f| match f {
            style::values::computed::font::SingleFontFamily::FamilyName(name) => {
                Some(name.name.to_string())
            }
            _ => None,
        })
        .is_some_and(|name| name.eq_ignore_ascii_case(text_measure::AHEM_FAMILY));

    // gradient: a two-stop linear-gradient background, if the first background
    // layer is one. Maps the FIRST `background-image` layer's `linear-gradient`
    // (the topmost paint) to the seam's reduced two-stop form. Returns `None` for
    // `none`/url()/radial/conic images.
    let gradient = map_gradient(style);

    // box-shadow: the FIRST outset (non-inset) shadow, reduced to offset+blur+color.
    let box_shadow = map_box_shadow(style);

    ComputedStyle {
        display,
        color,
        background,
        font_size,
        padding,
        border_width,
        border_color,
        border_radius,
        opacity,
        is_ahem,
        gradient,
        box_shadow,
    }
}

/// Map the first `background-image` layer to a reduced two-stop [`LinearGradient`],
/// if it is a `linear-gradient`. Returns `None` for `none`, `url()`, image-set,
/// radial/conic gradients, or a gradient with no color stops.
///
/// The reduction: take the gradient's first and last color stops (resolved against
/// `currentColor`) as the two seam stops, and snap the line direction to the nearer
/// of vertical/horizontal — a `to bottom` (the CSS default) stays vertical, `to
/// right`/`to left` is horizontal, an angle picks the axis its sin/cos leans toward,
/// and a corner snaps to whichever of width/height it spans more of. The endpoint
/// colors are an exact match; only multi-stop interpolation detail and diagonal
/// angle are approximated.
fn map_gradient(style: &ComputedValues) -> Option<LinearGradient> {
    use style::values::computed::image::{Image, LineDirection};
    use style::values::computed::{Color as ComputedColor, LengthPercentage};
    use style::values::generics::image::{GenericGradient, GenericGradientItem};
    use style::values::specified::position::{HorizontalPositionKeyword, VerticalPositionKeyword};

    let images = &style.get_background().background_image.0;
    let first = images.first()?;
    let Image::Gradient(gradient) = first else {
        return None;
    };

    // Only linear gradients are reduced; radial/conic fall back to the flat bg.
    let GenericGradient::Linear {
        direction, items, ..
    } = gradient.as_ref()
    else {
        return None;
    };

    // First and last *color* stops (skip bare interpolation hints).
    let cur = style.clone_color();
    let stop_color =
        |item: &GenericGradientItem<ComputedColor, LengthPercentage>| -> Option<Color> {
            match item {
                GenericGradientItem::SimpleColorStop(c) => {
                    Some(absolute_to_color(c.clone().resolve_to_absolute(&cur)))
                }
                GenericGradientItem::ComplexColorStop { color, .. } => {
                    Some(absolute_to_color(color.clone().resolve_to_absolute(&cur)))
                }
                GenericGradientItem::InterpolationHint(_) => None,
            }
        };
    let start = items.iter().find_map(stop_color)?;
    let end = items.iter().rev().find_map(stop_color).unwrap_or(start);

    // Snap the line direction to the nearer orthogonal axis.
    let axis = match direction {
        LineDirection::Vertical(_) => GradientAxis::Vertical,
        LineDirection::Horizontal(_) => GradientAxis::Horizontal,
        LineDirection::Corner(_h, _v) => {
            // A corner spans both axes equally for a square box; default to vertical
            // (the common "to bottom right" reads top→bottom on a typical box).
            GradientAxis::Vertical
        }
        LineDirection::Angle(angle) => {
            // CSS angle: 0deg = to top, 90deg = to right. Pick the axis the line
            // leans toward more (|sin| vs |cos|).
            let rad = angle.radians();
            if rad.sin().abs() > rad.cos().abs() {
                GradientAxis::Horizontal
            } else {
                GradientAxis::Vertical
            }
        }
    };

    // Flip start/end so `start` is always the top (vertical) / left (horizontal)
    // edge, matching the seam's axis contract.
    let (start, end) = match direction {
        LineDirection::Horizontal(HorizontalPositionKeyword::Left) => (end, start),
        LineDirection::Vertical(VerticalPositionKeyword::Top) => (end, start),
        _ => (start, end),
    };

    Some(LinearGradient { start, end, axis })
}

/// Map the first **outset** (non-`inset`) `box-shadow` to the seam's reduced
/// [`BoxShadow`] (offset + blur + color). Returns `None` if the list is empty or
/// holds only inset shadows. Spread and any shadow past the first outset are
/// dropped (the seam carries one soft drop shadow).
fn map_box_shadow(style: &ComputedValues) -> Option<BoxShadow> {
    let shadows = &style.get_effects().box_shadow.0;
    let shadow = shadows.iter().find(|s| !s.inset)?;
    let cur = style.clone_color();
    let color = absolute_to_color(shadow.base.color.clone().resolve_to_absolute(&cur));
    Some(BoxShadow {
        dx: shadow.base.horizontal.px(),
        dy: shadow.base.vertical.px(),
        blur: shadow.base.blur.px().max(0.0),
        color,
    })
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
        // Capable-tier (`from_dom`): map the real Dom handle to its overlay slab.
        // HTML/manual paths leave `node_map` empty and use slab == handle.
        let id = self
            .node_map
            .get(&node.raw())
            .copied()
            .unwrap_or(node.raw() as usize);
        self.computed_style_for(id).ok_or(HostError::BadHandle)
    }
}

/// Recursively mirror the `canopy_dom` subtree under `parent_canopy` into the Stylo
/// overlay [`Document`] under `parent_slab`, reading each element's CSS identity
/// (tag-name / id / classes) off the real Dom and recording the handle->slab mapping.
fn build_overlay(
    dom: &canopy_dom::Dom,
    parent_canopy: NodeId,
    parent_slab: usize,
    doc: &mut Document,
    node_map: &mut std::collections::HashMap<u64, usize>,
) {
    for &child in dom.children(parent_canopy) {
        let Some(n) = dom.node(child) else { continue };
        if n.tag.is_some() {
            // Element: tag-name (default "div"), id, and classes from the real Dom.
            let tag = dom.tag_name(child).unwrap_or("div");
            let id = dom.id(child);
            let classes: Vec<&str> = dom.classes(child).iter().map(String::as_str).collect();
            let slab = doc.add_element(parent_slab, tag, id, &classes);
            node_map.insert(child.raw(), slab);
            build_overlay(dom, child, slab, doc, node_map);
        } else if n.text.is_some() {
            let slab = doc.add_text(parent_slab, dom.text_of(child).unwrap_or(""));
            node_map.insert(child.raw(), slab);
        }
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
//   * FLOAT + CLEAR are mapped (taffy's `float_layout` feature is enabled in
//     Cargo.toml). FLEX + BLOCK + GRID + the box-model subset (incl. `calc()`)
//     are ported.
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
            Clear, Display, DisplayInside, DisplayOutside, Float, Overflow,
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

    /// A CSS intrinsic-sizing *keyword* requested on the inline (width) axis.
    ///
    /// Taffy 0.11's `Dimension` has no `min-/max-/fit-content` variant, so
    /// [`dimension`] maps these keywords to `AUTO` — and a block child whose width
    /// is `AUTO` is *stretched* to fill its container, never sized to content. To
    /// honor the keyword we detect it here and let the layout pass pre-resolve the
    /// box's content width (via the text measure-fn) into a fixed length, so Taffy
    /// sees a definite width instead of stretching the leaf.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WidthKeyword {
        /// Not an intrinsic keyword (a length/percentage/auto/stretch).
        None,
        /// `width: min-content` — size to the widest unbreakable run.
        MinContent,
        /// `width: max-content` — size to the single unwrapped line.
        MaxContent,
        /// `width: fit-content` — `max-content` clamped to the available width.
        FitContent,
    }

    /// Classify the inline-axis (width) sizing keyword on a computed style.
    #[inline]
    pub fn width_keyword(style: &stylo::ComputedValues) -> WidthKeyword {
        match style.get_position().width {
            stylo::Size::MinContent => WidthKeyword::MinContent,
            stylo::Size::MaxContent => WidthKeyword::MaxContent,
            stylo::Size::FitContent | stylo::Size::FitContentFunction(_) => {
                WidthKeyword::FitContent
            }
            _ => WidthKeyword::None,
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

    /// Map stylo's computed `float` keyword to taffy's `Float`.
    ///
    /// `float` only applies to children of a block formatting context; taffy
    /// positions a `Float::Left`/`Right` box at the start/end of the current line
    /// and flows following in-flow content beside it (the `float_layout` feature).
    /// The CSS-logical `inline-start`/`inline-end` keywords are physicalised to
    /// `left`/`right` for our LTR-only test surface (stylo's adjuster does NOT
    /// resolve them by writing-mode for `float`, so they can survive into computed
    /// values — see the float gotcha in the layout pass).
    #[inline]
    pub fn float(input: stylo::Float) -> taffy::Float {
        match input {
            stylo::Float::None => taffy::Float::None,
            stylo::Float::Left => taffy::Float::Left,
            stylo::Float::Right => taffy::Float::Right,
            // Logical -> physical (LTR). RTL would swap, but our tests are LTR.
            stylo::Float::InlineStart => taffy::Float::Left,
            stylo::Float::InlineEnd => taffy::Float::Right,
        }
    }

    /// Map stylo's computed `clear` keyword to taffy's `Clear`.
    ///
    /// `clear` moves the box below any preceding floats on the named side(s).
    /// Logical `inline-start`/`inline-end` physicalise to `left`/`right` (LTR).
    #[inline]
    pub fn clear(input: stylo::Clear) -> taffy::Clear {
        match input {
            stylo::Clear::None => taffy::Clear::None,
            stylo::Clear::Left => taffy::Clear::Left,
            stylo::Clear::Right => taffy::Clear::Right,
            stylo::Clear::Both => taffy::Clear::Both,
            stylo::Clear::InlineStart => taffy::Clear::Left,
            stylo::Clear::InlineEnd => taffy::Clear::Right,
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
    /// (flex + block + grid + float/clear + box-model subset).
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

            // Floats (gated behind taffy's `float_layout` feature, enabled in
            // Cargo.toml). A `float:left` box is taken out of flow and placed at
            // the line start; following in-flow content wraps beside it. `clear`
            // pushes a box below preceding floats. Both come from the `box` struct.
            float: self::float(style.get_box().clone_float()),
            clear: self::clear(style.get_box().clone_clear()),

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

/// The four edge widths of a box (padding, border, or margin), in logical px.
///
/// A small `Copy` value type returned by [`StyloEngine::element_layout_detail`]
/// alongside each element's absolute border-box rect, so a caller can recover the
/// padding/border/margin a Taffy layout resolved for the element without
/// re-running the engine. Read straight off taffy's [`taffy::Layout`].
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Edges {
    /// Top edge width.
    pub top: f32,
    /// Right edge width.
    pub right: f32,
    /// Bottom edge width.
    pub bottom: f32,
    /// Left edge width.
    pub left: f32,
}

impl Edges {
    /// Build from a taffy `Rect<f32>` (taffy's edge container).
    fn from_taffy(r: taffy::Rect<f32>) -> Self {
        Edges {
            top: r.top,
            right: r.right,
            bottom: r.bottom,
            left: r.left,
        }
    }
}

/// One element's resolved geometry: its arena slab id, its absolute border-box
/// rect, and the padding / border / margin edge widths Taffy resolved for it.
///
/// Returned (in DFS element order) by [`StyloEngine::element_layout_detail`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ElementLayoutDetail {
    /// Arena slab id of the element.
    pub slab: usize,
    /// Absolute border-box rectangle (same coordinate space as
    /// [`StyloEngine::layout`]'s rects).
    pub rect: canopy_traits::Rect,
    /// Padding edge widths.
    pub padding: Edges,
    /// Border edge widths.
    pub border: Edges,
    /// Margin edge widths.
    pub margin: Edges,
    /// Whether the element's computed `position` is anything other than `static`
    /// (`relative` / `absolute` / `fixed` / `sticky`). An element is a CSS
    /// **offset parent** for its descendants iff this is true (or it's the body).
    /// Lets a caller resolve `offsetParent`-relative offsets without re-cascading.
    pub is_positioned: bool,
}

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

        // Node-context type carries text + font for auto-sized text leaves; a
        // non-text (or explicitly-sized) leaf gets `None` and never measures.
        let mut tree: taffy::TaffyTree<Option<text_measure::MeasureContext>> =
            taffy::TaffyTree::new();
        // taffy node handle per element index.
        let mut taffy_nodes: Vec<taffy::NodeId> = Vec::with_capacity(order.len());

        // First pass: create a leaf taffy node for each element with its style.
        // A LEAF element with a direct Text child AND no explicit width/height
        // also carries a measure context, so it sizes from its shaped text.
        for &slab in &order {
            let cv = self.computed_values_for(slab);
            let mut style = cv
                .as_ref()
                .map(|cv| taffy_convert::to_taffy_style(cv))
                .unwrap_or_default();

            // The inline-axis intrinsic-sizing keyword (`min-/max-/fit-content`),
            // if any. `to_taffy_style` maps these to `auto` (Taffy 0.11 has no
            // keyword variant), so we recover the author's intent from the
            // computed style to size the leaf to content below.
            let width_kw = cv
                .as_ref()
                .map(|cv| taffy_convert::width_keyword(cv))
                .unwrap_or(taffy_convert::WidthKeyword::None);

            let ctx = cv.as_ref().and_then(|cv| {
                // Only auto-sized leaves measure text: if width or height is set
                // explicitly, the box geometry is fixed and text never resizes it.
                // An intrinsic width keyword (`min-/max-/fit-content`) maps to
                // `auto` here yet still wants content sizing, so it does NOT
                // disqualify the leaf.
                if !style.size.width.is_auto() || !style.size.height.is_auto() {
                    return None;
                }
                let text = self.direct_text_of(slab)?;
                let font = cv.get_font();
                let font_size = font.font_size.used_size().px();
                let family = font
                    .font_family
                    .families
                    .iter()
                    .find_map(|f| match f {
                        style::values::computed::font::SingleFontFamily::FamilyName(name) => {
                            Some(name.name.to_string())
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                Some(text_measure::MeasureContext {
                    text,
                    font_size,
                    family,
                })
            });

            // Honor a `width: min-/max-/fit-content` keyword on a text leaf. Taffy
            // would otherwise STRETCH this auto-width block child to fill its
            // container (its `known_dimensions.width` wins over the measured size),
            // so we pre-resolve the intrinsic content width here and pin it as a
            // fixed length. `fit-content` == `max-content` clamped to the available
            // width; with no definite constraint here it collapses to `max-content`.
            if let (Some(ctx), true) = (
                ctx.as_ref(),
                matches!(
                    width_kw,
                    taffy_convert::WidthKeyword::MinContent
                        | taffy_convert::WidthKeyword::MaxContent
                        | taffy_convert::WidthKeyword::FitContent
                ),
            ) {
                let min_content = width_kw == taffy_convert::WidthKeyword::MinContent;
                let w = text_measure::intrinsic_width(ctx, min_content);
                style.size.width = taffy::Dimension::length(w);
            }

            let node = tree
                .new_leaf_with_context(style, ctx)
                .expect("taffy new_leaf_with_context");
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
        tree.compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(viewport.w),
                height: taffy::AvailableSpace::Definite(viewport.h),
            },
            // Measure closure: only auto-sized text leaves carry a context; every
            // other leaf falls back to its zero/style-driven size.
            |known_dimensions, available_space, _node_id, node_context, _style| {
                // `node_context` is `Option<&mut Option<MeasureContext>>`: outer
                // Some for every leaf, inner Some only for auto-sized text leaves.
                match node_context.and_then(|c| c.as_ref()) {
                    Some(ctx) => text_measure::measure_text(known_dimensions, available_space, ctx),
                    None => taffy::Size {
                        width: known_dimensions.width.unwrap_or(0.0),
                        height: known_dimensions.height.unwrap_or(0.0),
                    },
                }
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

    /// Compute layout and return each element's ABSOLUTE border-box paired with
    /// its arena **slab id**, in DFS element order.
    ///
    /// [`layout`](StyloEngine::layout) returns just the `Rect`s positionally (in
    /// the same DFS order as the cascade), which is enough to zip against styles
    /// but loses the slab id. A conformance runner needs to look up the box for a
    /// *specific* element (the one carrying a `data-expected-*` attribute), so
    /// this pairs each rect with its slab id. Build a `HashMap<usize, Rect>` from
    /// the result and index it by the slab ids returned from
    /// [`html::parse_html_with_css`](crate::html::parse_html_with_css).
    pub fn element_layout(
        &mut self,
        viewport: canopy_traits::Size,
    ) -> Vec<(usize, canopy_traits::Rect)> {
        let rects = self.layout(viewport);
        let order = self.element_dfs_order();
        order.into_iter().zip(rects).collect()
    }

    /// Compute layout and return, per element (in DFS element order), an
    /// [`ElementLayoutDetail`]: its slab id, absolute border-box rect, and the
    /// padding / border / margin [`Edges`] Taffy resolved.
    ///
    /// A sibling to [`layout`](StyloEngine::layout) /
    /// [`element_layout`](StyloEngine::element_layout): those return only the
    /// border-box rect, which hides the box-model breakdown (how much of the box is
    /// padding vs. border vs. how far the margin pushed it). This method reads
    /// taffy's [`Layout::padding`](taffy::Layout), `border`, and `margin` edge
    /// rects directly, so a caller can, e.g., assert that a `padding:10px` box
    /// resolved a 10px padding edge. It rebuilds and re-runs the taffy tree exactly
    /// as `layout` does (it does **not** touch `layout`'s codepath).
    pub fn element_layout_detail(
        &mut self,
        viewport: canopy_traits::Size,
    ) -> Vec<ElementLayoutDetail> {
        use canopy_traits::{Point, Rect, Size};

        self.resolve_styles();

        let order = self.element_dfs_order();
        if order.is_empty() {
            return Vec::new();
        }

        // slab id -> index into `order`.
        let mut slab_to_idx = std::collections::HashMap::new();
        for (i, &slab) in order.iter().enumerate() {
            slab_to_idx.insert(slab, i);
        }

        let mut tree: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let mut taffy_nodes: Vec<taffy::NodeId> = Vec::with_capacity(order.len());

        // Leaf per element with its converted style.
        for &slab in &order {
            let style = self
                .computed_values_for(slab)
                .map(|cv| taffy_convert::to_taffy_style(&cv))
                .unwrap_or_default();
            let node = tree.new_leaf(style).expect("taffy new_leaf");
            taffy_nodes.push(node);
        }

        // Wire element children in document order.
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

        // Walk accumulating absolute origins (same accumulation as `layout`), and
        // capture each node's padding/border/margin edges off the taffy `Layout`.
        let mut details = vec![
            ElementLayoutDetail {
                slab: 0,
                rect: Rect::default(),
                padding: Edges::default(),
                border: Edges::default(),
                margin: Edges::default(),
                is_positioned: false,
            };
            order.len()
        ];
        let mut stack = vec![(0usize, 0.0f32, 0.0f32)];
        while let Some((idx, px, py)) = stack.pop() {
            let l = tree.layout(taffy_nodes[idx]).expect("taffy layout");
            let ax = px + l.location.x;
            let ay = py + l.location.y;
            // `position != static` (computed) marks this element as an offset
            // parent for its descendants. Read the cascaded `position` directly.
            let is_positioned = self
                .computed_values_for(order[idx])
                .map(|cv| {
                    cv.clone_position()
                        != style::properties::longhands::position::computed_value::T::Static
                })
                .unwrap_or(false);
            details[idx] = ElementLayoutDetail {
                slab: order[idx],
                rect: Rect {
                    origin: Point { x: ax, y: ay },
                    size: Size {
                        w: l.size.width,
                        h: l.size.height,
                    },
                },
                padding: Edges::from_taffy(l.padding),
                border: Edges::from_taffy(l.border),
                margin: Edges::from_taffy(l.margin),
                is_positioned,
            };
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

        details
    }

    /// The concatenated text of an element's **direct** Text children, if any,
    /// and only when the element has no *element* children (a leaf). Returns
    /// `None` for non-leaves or elements with no direct text — those don't get a
    /// text measure context. Used by [`layout`](StyloEngine::layout) to size an
    /// auto-width/height text box from its content.
    fn direct_text_of(&self, node_id: usize) -> Option<String> {
        let node = self.doc.nodes.get(node_id)?;
        if !node.is_element() {
            return None;
        }
        // A text-bearing leaf has only Text children (no element children).
        let mut text = String::new();
        for &c in &node.children {
            match &self.doc.nodes[c].kind {
                NodeKind::Text(s) => text.push_str(s),
                NodeKind::Element { .. } => return None,
                NodeKind::Document => return None,
            }
        }
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
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
    fn from_dom_cascades_the_real_canopy_tree() {
        // THE productization proof: a REAL canopy_dom tree (built from the op-stream, as
        // a capable-tier Ui produces) is cascaded by Stylo — including a descendant
        // selector and inherited color — over that exact retained tree.
        use canopy_core::Emitter;
        use canopy_dom::{Dom, ROOT};
        use canopy_protocol::ElementTag;
        use canopy_traits::OpSink;

        // A `.card` containing a `.title`, with the identity a capable-tier Ui carries.
        let mut e = Emitter::new();
        let card = e.create_element(ElementTag::new(1));
        e.append(ROOT, card);
        e.set_tag_name(card, "div");
        e.set_class(card, "card");
        let title = e.create_element(ElementTag::new(1));
        e.append(card, title);
        e.set_tag_name(title, "div");
        e.set_class(title, "title");
        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();

        // Cascade the real Dom with real CSS: a class rule + a descendant selector.
        let css = ".card { background:#112233 } .card .title { color:#00ff00 }";
        let mut engine = StyloEngine::from_dom(&dom, css);

        let card_style = engine.resolve(card, None).unwrap();
        let title_style = engine.resolve(title, None).unwrap();

        assert_eq!(
            card_style.background,
            Color {
                r: 0x11,
                g: 0x22,
                b: 0x33,
                a: 255
            },
            ".card background applied over the real Dom"
        );
        assert_eq!(
            title_style.color,
            Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            },
            "`.card .title` descendant selector resolved over the real Dom"
        );
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

    #[test]
    fn hover_restyle_toggles_background() {
        // `.btn { background:#000000 } .btn:hover { background:#ff0000 }` on a
        // single `.btn` element. With no hover the cascade resolves black; setting
        // hover on the element and re-resolving must re-run the cascade so the
        // `:hover` rule wins and background flips to red; clearing hover restores
        // black. This is the whole interactivity seam: pointer hover -> element
        // state -> forced restyle -> visible change.
        let black = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let red = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };

        let mut engine =
            StyloEngine::new(".btn { background:#000000 } .btn:hover { background:#ff0000 }");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let btn = doc.add_element(html, "div", None, &["btn"]);

        // No hover: black.
        let style = resolve(&mut engine, btn);
        assert_eq!(
            style.background, black,
            "without hover the .btn background should be black"
        );

        // Hover on: the forced restyle must re-cascade so `:hover` wins -> red.
        engine.set_hover(Some(btn));
        let style = resolve(&mut engine, btn);
        assert_eq!(
            style.background, red,
            "hovering the .btn should restyle its background to red"
        );

        // Hover off: back to black.
        engine.set_hover(None);
        let style = resolve(&mut engine, btn);
        assert_eq!(
            style.background, black,
            "clearing hover should restyle the .btn background back to black"
        );
    }

    #[test]
    fn hit_test_picks_deepest_element() {
        // A page box with a nested button. A point inside the button must hit the
        // button (the deepest element), not the page behind it; a point inside the
        // page but outside the button hits the page; a point outside both misses.
        let mut engine = StyloEngine::new(
            ".page { width:200px; height:200px } \
             .btn { width:50px; height:20px }",
        );
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let page = doc.add_element(html, "div", None, &["page"]);
        let btn = doc.add_element(page, "div", None, &["btn"]);

        let vp = TSize { w: 300.0, h: 300.0 };
        // The button sits at the page's content origin (top-left). A point well
        // inside it lands on the button.
        let inside_btn = canopy_traits::Point { x: 12.0, y: 8.0 };
        assert_eq!(
            engine.hit_test(inside_btn, vp),
            Some(btn),
            "a point inside the nested button should hit the button (deepest)"
        );

        // A point inside the page but below the 20px-tall button hits the page.
        let inside_page = canopy_traits::Point { x: 100.0, y: 150.0 };
        assert_eq!(
            engine.hit_test(inside_page, vp),
            Some(page),
            "a point inside the page but outside the button should hit the page"
        );

        // A point outside the whole page misses everything.
        let outside = canopy_traits::Point { x: 999.0, y: 999.0 };
        assert_eq!(
            engine.hit_test(outside, vp),
            None,
            "a point outside the page should hit nothing"
        );
    }

    #[test]
    fn element_layout_detail_padding() {
        // A box with `padding:10px` must report a 10px padding edge in its
        // ElementLayoutDetail (read straight off taffy's Layout).
        let mut engine = StyloEngine::new("");
        let doc = engine.document_mut();
        let html = doc.add_element(0, "html", None, &[]);
        let boxed = doc.add_element(html, "div", None, &[]);
        engine
            .document_mut()
            .set_inline_style(boxed, "width:100px; height:50px; padding:10px");

        let details = engine.element_layout_detail(TSize { w: 200.0, h: 200.0 });
        // index 1 is the styled div (index 0 is the root <html>).
        let detail = details
            .iter()
            .find(|d| d.slab == boxed)
            .expect("styled box present in layout detail");
        assert!(
            near(detail.padding.top, 10.0)
                && near(detail.padding.right, 10.0)
                && near(detail.padding.bottom, 10.0)
                && near(detail.padding.left, 10.0),
            "padding edges should all be ~10px, got {:?}",
            detail.padding
        );
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
    fn layout_text_leaf_measures_from_content() {
        // A leaf <div font-family:Ahem; font-size:20px> containing "XXXXX",
        // inside an AUTO-width parent (a column flex that does NOT stretch its
        // child: `align-items:flex-start`, so the child shrinks to its content
        // rather than filling the container — the CSS way a box "leaves the size
        // of its text"). Ahem is metrics-perfect (every glyph a 1em square), so
        // the leaf sizes to EXACTLY 100px x 20px from its text — the measure
        // closure runs because the leaf has a Text child and no explicit
        // width/height. (Requires /tmp/wpt/fonts/Ahem.ttf.)
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            let parent = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(
                parent,
                "display:flex; flex-direction:column; align-items:flex-start",
            );
            let leaf = doc.add_element(parent, "div", None, &[]);
            doc.set_inline_style(leaf, "font-family:Ahem; font-size:20px");
            doc.add_text(leaf, "XXXXX");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + parent + leaf div");
        let leaf = rects[2];
        println!("text leaf box = {:?}", leaf.size);
        assert!(
            (leaf.size.w - 100.0).abs() <= 2.0,
            "leaf width should be ~100 (5 Ahem glyphs @ 20px), got {}",
            leaf.size.w
        );
        assert!(
            (leaf.size.h - 20.0).abs() <= 2.0,
            "leaf height should be ~20 (one 20px Ahem line), got {}",
            leaf.size.h
        );
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

    #[test]
    fn layout_absolute_position_top_left() {
        // A positioned parent (relative, 200x200) containing an absolutely
        // positioned child `top:10px; left:20px; width:30px; height:30px`.
        //
        // Taffy lays out abspos children against the DIRECT parent's padding box
        // (the area offset is the parent's border width; here zero). With LTR and
        // a definite left/top, the child's border-box location relative to the
        // parent's border-box is (left, top) = (20, 10), size 30x30. The parent is
        // at (0,0), so the child's ABSOLUTE origin is (20, 10).
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:200px; height:200px");
            let parent = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(parent, "position:relative; width:200px; height:200px");
            let child = doc.add_element(parent, "div", None, &[]);
            doc.set_inline_style(
                child,
                "position:absolute; top:10px; left:20px; width:30px; height:30px",
            );
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + relative parent + abspos child");

        let child = rects[2];
        println!("abspos child box = {child:?}");
        assert!(
            near(child.origin.x, 20.0),
            "abspos child x should be ~20 (left), got {}",
            child.origin.x
        );
        assert!(
            near(child.origin.y, 10.0),
            "abspos child y should be ~10 (top), got {}",
            child.origin.y
        );
        assert!(
            near(child.size.w, 30.0),
            "abspos child w should be ~30, got {}",
            child.size.w
        );
        assert!(
            near(child.size.h, 30.0),
            "abspos child h should be ~30, got {}",
            child.size.h
        );
    }

    #[test]
    fn layout_max_content_sizes_to_single_line() {
        // A div `font-family:Ahem; font-size:20px; width:max-content` with text
        // "XX YY". max-content sizes the box to its single unwrapped line: 5 glyphs
        // (X X space Y Y) at 20px = 100px wide, 20px tall. The width:max-content
        // keyword leaves the leaf's width AUTO in the taffy style, so the measure
        // closure is invoked with AvailableSpace::MaxContent and must return the
        // unwrapped single-line width.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            let leaf = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(leaf, "font-family:Ahem; font-size:20px; width:max-content");
            doc.add_text(leaf, "XX YY");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 2, "html + leaf div");
        let leaf = rects[1];
        println!("max-content leaf box = {:?}", leaf.size);
        assert!(
            (leaf.size.w - 100.0).abs() <= 2.0,
            "max-content width should be ~100 (5 Ahem glyphs @ 20px, one line), got {}",
            leaf.size.w
        );
        assert!(
            (leaf.size.h - 20.0).abs() <= 2.0,
            "max-content height should be ~20 (one 20px Ahem line), got {}",
            leaf.size.h
        );
    }

    #[test]
    fn layout_min_content_sizes_to_widest_word() {
        // A div `font-family:Ahem; font-size:20px; width:min-content` with text
        // "XX YY". min-content sizes the box to the widest unbreakable run: each
        // word is 2 glyphs = 40px wide; the box wraps to two lines so it's 40px
        // wide and 40px tall. The measure closure is invoked with
        // AvailableSpace::MinContent and must return the longest-word width.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            let leaf = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(leaf, "font-family:Ahem; font-size:20px; width:min-content");
            doc.add_text(leaf, "XX YY");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 2, "html + leaf div");
        let leaf = rects[1];
        println!("min-content leaf box = {:?}", leaf.size);
        assert!(
            (leaf.size.w - 40.0).abs() <= 2.0,
            "min-content width should be ~40 (widest word, 2 Ahem glyphs @ 20px), got {}",
            leaf.size.w
        );
    }

    #[test]
    fn layout_float_left_content_flows_beside() {
        // A `float:left` box (50x50) followed by a sibling that establishes its own
        // block formatting context (`overflow:hidden`). With taffy's `float_layout`
        // feature ON and `taffy_convert` mapping `float`/`clear`, the float is taken
        // out of flow at the line start (origin 0,0) and the BFC sibling flows
        // BESIDE it into the content slot to the float's right (x >= ~50), on the
        // same line (y ~ 0). Without float support the sibling would start at the
        // container origin (x == 0), overlapping the float — this is exactly the
        // before/after of enabling the float feature + mapping.
        //
        // NB: a *plain* block sibling's border box is NOT shifted by a float in CSS
        // (only its inline/line content wraps); a box must establish a new BFC
        // (overflow!=visible / display:flow-root / inline-block) to sit beside the
        // float as a block. Taffy models this via `is_in_same_bfc` — see the gotcha.
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:400px; height:200px");
            let floated = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(floated, "float:left; width:50px; height:50px");
            let beside = doc.add_element(html, "div", None, &[]);
            // A new-BFC in-flow sibling (overflow:hidden) sits beside the float.
            doc.set_inline_style(beside, "overflow:hidden; width:50px; height:50px");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + float + sibling");
        let floated = rects[1];
        let beside = rects[2];
        println!("float box = {floated:?}; sibling box = {beside:?}");

        // The float sits at the top-left of its container.
        assert!(
            near(floated.origin.x, 0.0) && near(floated.origin.y, 0.0),
            "float should be at (0,0), got {:?}",
            floated.origin
        );
        // The sibling flows BESIDE the float: its left edge is at/after the float's
        // right edge (x >= ~50), and it stays on the same line (top, y ~ 0).
        assert!(
            beside.origin.x >= 50.0 - 1.0,
            "sibling should flow to the RIGHT of the float (x >= ~50), got x={}",
            beside.origin.x
        );
        assert!(
            near(beside.origin.y, 0.0),
            "sibling should stay on the same line as the float (y ~ 0), got y={}",
            beside.origin.y
        );
    }

    #[test]
    fn layout_clear_left_drops_below_float() {
        // A `float:left` box (50x50) followed by a `clear:left` sibling. `clear:left`
        // moves the box BELOW any preceding left-floated box, so the sibling's top
        // edge is at/after the float's bottom (y >= ~50), flowing at x ~ 0. This
        // exercises the `clear` mapping (the complement of the float-flow test).
        let mut engine = StyloEngine::new("");
        {
            let doc = engine.document_mut();
            let html = doc.add_element(0, "html", None, &[]);
            doc.set_inline_style(html, "display:block; width:400px; height:200px");
            let floated = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(floated, "float:left; width:50px; height:50px");
            let cleared = doc.add_element(html, "div", None, &[]);
            doc.set_inline_style(cleared, "clear:left; width:50px; height:50px");
        }

        let rects = engine.layout(TSize { w: 800.0, h: 600.0 });
        assert_eq!(rects.len(), 3, "html + float + cleared sibling");
        let cleared = rects[2];
        println!("cleared sibling box = {cleared:?}");
        assert!(
            cleared.origin.y >= 50.0 - 1.0,
            "clear:left sibling should drop BELOW the float (y >= ~50), got y={}",
            cleared.origin.y
        );
    }
}
