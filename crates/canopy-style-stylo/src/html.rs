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

use std::rc::Rc;

use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, ParseOpts};
use markup5ever::local_name;
use markup5ever_rcdom::{Handle, NodeData, RcDom};

use crate::Document;

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
    // RcDom is the reference TreeSink: it runs html5ever's full insertion
    // algorithm and hands back a finished, owned tree. `drop_doctype` keeps the
    // doctype node out of the tree entirely (we'd skip it anyway).
    let opts = ParseOpts::default();
    let dom: RcDom = parse_document(RcDom::default(), opts)
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .expect("RcDom parsing is infallible for in-memory &str");

    let mut doc = Document::new();
    // The RcDom root is a `Document` node; its element children are the
    // `<html>` root (browsers synthesize exactly one). Mirror each top-level
    // element under the arena root (node 0).
    for child in dom.document.children.borrow().iter() {
        walk(&mut doc, 0, child);
    }
    doc
}

/// Recursively mirror one RcDom node (and its subtree) into the arena under
/// `parent` (an arena node id). Elements become arena elements; text becomes
/// arena text; everything else (head subtree, script/style, comments, doctype,
/// PIs) is skipped.
fn walk(doc: &mut Document, parent: usize, node: &Handle) {
    match &node.data {
        NodeData::Element { name, attrs, .. } => {
            let tag = &name.local;

            // Skip non-rendered subtrees entirely (head is not in the render
            // tree; script/style carry no boxes).
            if *tag == local_name!("head")
                || *tag == local_name!("script")
                || *tag == local_name!("style")
            {
                return;
            }

            // Read id / class / style attributes. `class` splits on ASCII
            // whitespace (the HTML class-token rule). Tendril values deref to
            // `&str`.
            let attrs = attrs.borrow();
            let mut id: Option<&str> = None;
            let mut class_str: Option<&str> = None;
            let mut style_str: Option<&str> = None;
            for attr in attrs.iter() {
                let an = &attr.name.local;
                if *an == local_name!("id") {
                    id = Some(&attr.value);
                } else if *an == local_name!("class") {
                    class_str = Some(&attr.value);
                } else if *an == local_name!("style") {
                    style_str = Some(&attr.value);
                }
            }
            let classes: Vec<&str> = class_str
                .map(|c| c.split_ascii_whitespace().collect())
                .unwrap_or_default();

            let new_id = doc.add_element(parent, tag, id, &classes);
            if let Some(style) = style_str {
                doc.set_inline_style(new_id, style);
            }

            // Recurse into children under this new element.
            //
            // Borrow children into an owned Vec of `Handle` clones first so we
            // don't hold the `RefCell` borrow across the recursive `walk`
            // (which itself borrows other nodes' children). `Handle = Rc<Node>`,
            // so the clone is a cheap refcount bump.
            let children: Vec<Handle> = node.children.borrow().iter().map(Rc::clone).collect();
            for child in &children {
                walk(doc, new_id, child);
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
