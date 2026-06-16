//! Parse real HTML into the crate's Stylo arena [`Document`].
//!
//! This is the bridge that lets the full-tier engine consume **actual HTML**
//! (e.g. Web Platform Tests) rather than a hand-built arena. We parse with
//! [`html5ever`] — the same parser a browser/Blitz uses — into an owned
//! [`RcDom`] tree, then walk that tree **once** and mirror it into a
//! [`crate::Document`] via its public mutators (`add_element`/`add_text`/
//! `set_inline_style`).
//!
//! ## Why RcDom instead of a custom `TreeSink`
//!
//! html5ever drives a [`TreeSink`](markup5ever::interface::tree_builder::TreeSink)
//! incrementally: during parsing it *reparents*, *inserts-before*, and
//! *merges adjacent text* as the tree builder runs the HTML insertion
//! algorithm. Blitz implements that sink directly because its `DocumentMutator`
//! exposes those operations. Our arena `Document` is append-only
//! (`add_element`/`add_text`), so implementing the sink directly would be
//! awkward and error-prone. Instead we let `markup5ever_rcdom::RcDom` (the
//! reference sink, version-locked to html5ever 0.39) build the finished tree,
//! then do a single clean pre-order walk into the arena. `markup5ever_rcdom`
//! pins the same web_atoms/`QualName` types as stylo's `markup5ever 0.39`, so
//! the `QualName`s flow straight through.
//!
//! ## What we mirror
//!
//! The browser wraps any fragment in `<html><head>…</head><body>…</body></html>`.
//! We build that real structure so the document's styling root is `<html>`,
//! exactly like [`crate::StyloEngine`] expects (it takes the first element
//! child of node 0 as the cascade root). For each **element** we read its local
//! name (tag), `id`, `class` (split on ASCII whitespace), and inline `style`
//! attribute; for each **text** node we copy its contents. We skip `<head>` and
//! its contents, `<script>`, `<style>`, comments, doctype, and processing
//! instructions — none of which carry render-tree boxes.

use std::collections::BTreeMap;
use std::rc::Rc;

use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, ParseOpts};
use markup5ever::local_name;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use crate::Document;

/// Per-element `data-*` attributes retained from the source HTML, keyed by the
/// element's arena slab id.
///
/// WPT `checkLayout` ("attr") tests carry expectations as `data-expected-*` /
/// `data-offset-*` attributes on the elements they assert against. The base
/// arena `Document` only keeps tag/id/class/inline-style (the things the cascade
/// needs), so [`parse_html_with_css`] surfaces those `data-*` attributes
/// separately as a `(slab_id, map)` list. The slab id is stable for the lifetime
/// of the returned `Document` (the arena is append-only), so a runner can lay the
/// document out and look up each asserted element's box by slab id.
pub type DataAttrs = Vec<(usize, BTreeMap<String, String>)>;

/// Parse an HTML string into the crate's arena [`Document`].
///
/// The returned document's node 0 is the implicit root; its first element child
/// is `<html>` (the styling root), with the real `<body>` subtree mirrored
/// underneath. `<head>` (and `<script>`/`<style>`/comments/doctype) are skipped.
///
/// ```ignore
/// let doc = canopy_style_stylo::html::parse_html("<div class='a b' id='x'>hi</div>");
/// // doc now contains <html><body><div class="a b" id="x">"hi"</div>…
/// ```
pub fn parse_html(html: &str) -> Document {
    parse_html_with_css(html).0
}

/// Parse an HTML string, additionally returning the author CSS found in
/// `<style>` blocks and the per-element `data-*` attributes.
///
/// This is the entry point a conformance runner (e.g. Web Platform Tests) uses:
///
/// * The returned **CSS string** is the concatenation of every `<style>`
///   element's text content, in document order. WPT's `checkLayout`/reftest
///   pages put their author rules in inline `<style>` blocks; feed this string
///   to [`StyloEngine::with_document`](crate::StyloEngine::with_document) (or use
///   [`StyloEngine::from_html`](crate::StyloEngine::from_html)) so the cascade
///   sees them. External `<link rel="stylesheet">` sheets are **not** inlined —
///   the caller must resolve those itself if needed.
/// * The returned [`DataAttrs`] maps each element's slab id to its `data-*`
///   attributes (e.g. `data-expected-width`, `data-offset-x`), which the base
///   `Document` does not retain.
///
/// Inline `style=` attributes are applied to the arena as before (via
/// `set_inline_style`), so they need no separate handling.
///
/// ```ignore
/// let (doc, css, data) = parse_html_with_css(
///     "<style>.a{width:10px}</style><div class='a' data-expected-width='10'></div>",
/// );
/// let mut engine = StyloEngine::with_document(doc, &css);
/// ```
pub fn parse_html_with_css(html: &str) -> (Document, String, DataAttrs) {
    // RcDom is the reference TreeSink: it runs html5ever's full insertion
    // algorithm and hands back a finished, owned tree. `drop_doctype` keeps the
    // doctype node out of the tree entirely (we'd skip it anyway).
    let opts = ParseOpts::default();
    let dom: RcDom = parse_document(RcDom::default(), opts)
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .expect("RcDom parsing is infallible for in-memory &str");

    let mut doc = Document::new();
    let mut css = String::new();
    let mut data_attrs: DataAttrs = Vec::new();
    // The RcDom root is a `Document` node; its element children are the
    // `<html>` root (browsers synthesize exactly one). Mirror each top-level
    // element under the arena root (node 0).
    for child in dom.document.children.borrow().iter() {
        walk(&mut doc, 0, child, &mut css, &mut data_attrs);
    }
    (doc, css, data_attrs)
}

/// Recursively mirror one RcDom node (and its subtree) into the arena under
/// `parent` (an arena node id). Elements become arena elements; text becomes
/// arena text; everything else (head subtree, script/style, comments, doctype,
/// PIs) is skipped — except that a `<style>` element's text is appended to
/// `css`, and any `data-*` attributes are collected into `data_attrs`.
fn walk(
    doc: &mut Document,
    parent: usize,
    node: &Handle,
    css: &mut String,
    data_attrs: &mut DataAttrs,
) {
    match &node.data {
        NodeData::Element { name, attrs, .. } => {
            let tag = &name.local;

            // `<style>`: not rendered, but its text content IS the author CSS we
            // want. Concatenate every `<style>` block's text (in document order)
            // so the runner can hand it to the cascade. We still skip the subtree
            // (no boxes), but harvest its text first.
            if *tag == local_name!("style") {
                let mut style_text = String::new();
                collect_text(node, &mut style_text);
                css.push_str(&style_text);
                css.push('\n');
                return;
            }

            // Skip other non-rendered subtrees entirely (head is not in the
            // render tree; script carries no boxes). NOTE: `<head>` is skipped,
            // so a `<style>` nested in `<head>` is NOT harvested here — but the
            // HTML5 tree builder hoists/keeps `<style>` such that our recursion
            // never descends into head. To be safe, harvest `<style>` from head
            // too: handle head by recursing for `<style>` only.
            if *tag == local_name!("head") {
                let children: Vec<Handle> = node.children.borrow().iter().map(Rc::clone).collect();
                for child in &children {
                    if let NodeData::Element { name, .. } = &child.data {
                        if name.local == local_name!("style") {
                            let mut style_text = String::new();
                            collect_text(child, &mut style_text);
                            css.push_str(&style_text);
                            css.push('\n');
                        }
                    }
                }
                return;
            }
            if *tag == local_name!("script") {
                return;
            }

            // Read id / class / style attributes plus any `data-*`. `class`
            // splits on ASCII whitespace (the HTML class-token rule). Tendril
            // values deref to `&str`.
            let attrs = attrs.borrow();
            let mut id: Option<&str> = None;
            let mut class_str: Option<&str> = None;
            let mut style_str: Option<&str> = None;
            let mut data: BTreeMap<String, String> = BTreeMap::new();
            for attr in attrs.iter() {
                let an = &attr.name.local;
                if *an == local_name!("id") {
                    id = Some(&attr.value);
                } else if *an == local_name!("class") {
                    class_str = Some(&attr.value);
                } else if *an == local_name!("style") {
                    style_str = Some(&attr.value);
                } else if an.starts_with("data-") {
                    data.insert(an.to_string(), attr.value.to_string());
                }
            }
            let classes: Vec<&str> = class_str
                .map(|c| c.split_ascii_whitespace().collect())
                .unwrap_or_default();

            let new_id = doc.add_element(parent, tag, id, &classes);
            if let Some(style) = style_str {
                doc.set_inline_style(new_id, style);
            }
            if !data.is_empty() {
                data_attrs.push((new_id, data));
            }

            // Recurse into children under this new element.
            //
            // Borrow children into an owned Vec of `Handle` clones first so we
            // don't hold the `RefCell` borrow across the recursive `walk`
            // (which itself borrows other nodes' children). `Handle = Rc<Node>`,
            // so the clone is a cheap refcount bump.
            let children: Vec<Handle> = node.children.borrow().iter().map(Rc::clone).collect();
            for child in &children {
                walk(doc, new_id, child, css, data_attrs);
            }
        }
        NodeData::Text { contents } => {
            let text = contents.borrow();
            doc.add_text(parent, &text);
        }
        // Document / Comment / Doctype / ProcessingInstruction: not rendered.
        // (Doctype is also dropped by RcDom defaults; we ignore it defensively.)
        _ => {}
    }
}

/// Append the concatenated text content of `node`'s subtree into `out`.
/// Used to harvest a `<style>` element's CSS text.
fn collect_text(node: &Handle, out: &mut String) {
    if let NodeData::Text { contents } = &node.data {
        out.push_str(&contents.borrow());
    }
    let children: Vec<Handle> = node.children.borrow().iter().map(Rc::clone).collect();
    for child in &children {
        collect_text(child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKind;

    /// Find the first element in the arena with the given local tag name.
    /// Returns `(node_id, &NodeKind)`.
    fn find_element<'a>(doc: &'a Document, tag: &str) -> Option<(usize, &'a NodeKind)> {
        doc.nodes
            .iter()
            .enumerate()
            .find_map(|(i, n)| match &n.kind {
                NodeKind::Element { name, .. } if name.local.as_ref() == tag => Some((i, &n.kind)),
                _ => None,
            })
    }

    #[test]
    fn parse_html_with_css_harvests_style_and_data_attrs() {
        let (doc, css, data) = parse_html_with_css(
            "<style>.box{width:10px}</style>\
             <div class='box' data-expected-width='10' data-offset-x='8'>hi</div>",
        );

        // The <style> text was harvested into the author CSS string.
        assert!(
            css.contains(".box") && css.contains("width:10px"),
            "css should contain the <style> rule, got: {css:?}"
        );

        // The <div>'s data-* attributes were collected, keyed by its slab id.
        let (div_id, _) = find_element(&doc, "div").expect("a <div> should exist");
        let (slab, map) = data
            .iter()
            .find(|(id, _)| *id == div_id)
            .expect("div should have data-* attrs recorded");
        assert_eq!(*slab, div_id);
        assert_eq!(
            map.get("data-expected-width").map(String::as_str),
            Some("10")
        );
        assert_eq!(map.get("data-offset-x").map(String::as_str), Some("8"));
        // The class still flows through normally.
        match &doc.nodes[div_id].kind {
            NodeKind::Element { classes, .. } => {
                let cs: Vec<&str> = classes.iter().map(|c| c.as_ref()).collect();
                assert_eq!(cs, vec!["box"]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parses_html_into_arena() {
        // html5ever wraps this in <html><head></head><body>…</body></html>.
        let doc = parse_html("<div class='a b' id='x' style='color:#ff0000'><span>hi</span></div>");

        // ---- the <html> styling root exists as the first element child of node 0
        let html_id = doc.nodes[0]
            .children
            .iter()
            .copied()
            .find(|cid| matches!(doc.nodes[*cid].kind, NodeKind::Element { .. }))
            .expect("document root should have an <html> element child");
        match &doc.nodes[html_id].kind {
            NodeKind::Element { name, .. } => {
                assert_eq!(name.local.as_ref(), "html", "styling root must be <html>");
            }
            _ => unreachable!(),
        }

        // ---- <head> must NOT have been mirrored.
        assert!(
            find_element(&doc, "head").is_none(),
            "<head> should be skipped"
        );

        // ---- the <div> with id=x, classes a/b.
        let (div_id, div_kind) = find_element(&doc, "div").expect("a <div> should exist");
        match div_kind {
            NodeKind::Element {
                name,
                id_attr,
                classes,
                ..
            } => {
                assert_eq!(name.local.as_ref(), "div");
                assert_eq!(
                    id_attr.as_ref().map(|a| a.as_ref()),
                    Some("x"),
                    "div id should be x"
                );
                let class_strs: Vec<&str> = classes.iter().map(|c| c.as_ref()).collect();
                assert_eq!(class_strs, vec!["a", "b"], "div classes should be [a, b]");
            }
            _ => unreachable!(),
        }

        // ---- the div is a descendant of <body>, which is a descendant of <html>.
        let (body_id, _) = find_element(&doc, "body").expect("a <body> should exist");
        assert_eq!(
            doc.nodes[body_id].parent,
            Some(html_id),
            "<body> should be a child of <html>"
        );
        // div's ancestry climbs through body.
        let mut cur = doc.nodes[div_id].parent;
        let mut saw_body = false;
        while let Some(p) = cur {
            if p == body_id {
                saw_body = true;
            }
            cur = doc.nodes[p].parent;
        }
        assert!(saw_body, "<div> should be inside <body>");

        // ---- the <span> with text "hi", nested under the div.
        let (span_id, span_kind) = find_element(&doc, "span").expect("a <span> should exist");
        match span_kind {
            NodeKind::Element { name, .. } => assert_eq!(name.local.as_ref(), "span"),
            _ => unreachable!(),
        }
        // span is a descendant of the div.
        let mut cur = doc.nodes[span_id].parent;
        let mut saw_div = false;
        while let Some(p) = cur {
            if p == div_id {
                saw_div = true;
            }
            cur = doc.nodes[p].parent;
        }
        assert!(saw_div, "<span> should be a descendant of the <div>");

        // text "hi" is a child of the span.
        let text_child = doc.nodes[span_id]
            .children
            .iter()
            .copied()
            .find_map(|cid| match &doc.nodes[cid].kind {
                NodeKind::Text(t) => Some(t.clone()),
                _ => None,
            })
            .expect("span should have a text child");
        assert_eq!(text_child, "hi", "span text should be 'hi'");
    }
}
