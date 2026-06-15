//! Lowering the parsed [`crate::ast`] tree to [`canopy_ui::Ui`] builder calls.
//!
//! Every node becomes a `let` binding holding its `NodeId`, so a parent can `mount`
//! each child by handle and a modifier can attach to the node it follows. The emitted
//! calls go through exactly the `column`/`row`/`label`/`button`/`el`/`class`/`mount`/
//! `on_click`/`bind_text` surface a hand-written `Ui` tree uses — there is no second
//! code path.
//!
//! ## Hygiene and paths
//!
//! - The output calls methods on a single `__rsx_ui` binding (the macro's `UI`
//!   expression, evaluated once). It references **no** crate paths: every effect is a
//!   method on the receiver, so a consumer needs only `canopy-ui` in scope, nothing
//!   else — not even `canopy-view`/`canopy-protocol` (the `El(tag)` tag expression is
//!   the user's own, so any path *it* needs is the user's concern).
//! - Per-node bindings use a `__rsx`-prefixed, [`Span::mixed_site`]-spanned identifier
//!   so they cannot capture or be captured by user code: a user closure that says
//!   `count` still resolves to the caller's `count`, never to a macro temporary.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Ident, LitStr};

use crate::ast::{Child, Kind, Node, Rsx};

/// Lower a whole invocation to a block expression that builds the tree and evaluates
/// to the root `NodeId`. Parse/validation errors become a spanned `compile_error!`.
pub fn expand(rsx: Rsx) -> TokenStream {
    match try_expand(rsx) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error(),
    }
}

fn try_expand(rsx: Rsx) -> syn::Result<TokenStream> {
    let ui = rsx.ui;
    // One binding for the Ui, shared by the whole subtree, spanned at the call site so
    // a type error on the expression points at the user's code. `&(expr)` accepts an
    // owned `Ui` or a `&Ui` and method auto-ref resolves `&self` methods through either.
    let ui_ident = Ident::new("__rsx_ui", Span::mixed_site());

    let mut body = TokenStream::new();
    let root_ident = lower_node(&rsx.root, &ui_ident, &mut body, &mut 0)?;

    Ok(quote! {{
        let #ui_ident = &#ui;
        #body
        #root_ident
    }})
}

/// Emit the statements that build `node` (and its subtree) against `ui`, appending to
/// `body`, and return the identifier bound to this node's id.
fn lower_node(
    node: &Node,
    ui: &Ident,
    body: &mut TokenStream,
    counter: &mut usize,
) -> syn::Result<Ident> {
    let id = *counter;
    *counter += 1;
    let node_ident = Ident::new(&format!("__rsx_n{id}"), Span::mixed_site());

    // ---- 1. create the node (+ its own text / reactive binding) ------------------
    //
    // Text-bearing heads consume a single string child as their text or a `bind_text`
    // closure as a reactive binding (so step 4 must not also mount their children).
    // Container heads create the element and mount their children below. The match
    // evaluates to whether step 4 still needs to run.
    let mount_kids = match node.kind {
        Kind::Label => {
            let create = if let Some(f) = &node.bind_text {
                quote! { #ui.label_bound(#f) }
            } else if let Some(text) = self_text(node)? {
                quote! { #ui.label(#text) }
            } else {
                quote! { #ui.label("") }
            };
            body.extend(quote! { let #node_ident = #create; });
            false
        }
        Kind::Button => {
            let create = if let Some(f) = &node.bind_text {
                reject_text_children(node, "a `bind_text(..)` button")?;
                quote! { #ui.button_bound(#f) }
            } else if let Some(text) = self_text(node)? {
                quote! { #ui.button(#text) }
            } else {
                quote! { #ui.button("") }
            };
            body.extend(quote! { let #node_ident = #create; });
            false
        }
        Kind::Input => {
            reject_bind_text(node, "Input")?;
            let create = if let Some(text) = self_text(node)? {
                quote! { #ui.input(#text) }
            } else {
                quote! { #ui.input("") }
            };
            body.extend(quote! { let #node_ident = #create; });
            false
        }
        Kind::Column => {
            reject_bind_text(node, "Column")?;
            body.extend(quote! { let #node_ident = #ui.column(); });
            true
        }
        Kind::Row => {
            reject_bind_text(node, "Row")?;
            body.extend(quote! { let #node_ident = #ui.row(); });
            true
        }
        Kind::El => {
            reject_bind_text(node, "El")?;
            let tag = node.tag.as_ref().expect("El always parses a tag");
            body.extend(quote! { let #node_ident = #ui.el(#tag); });
            true
        }
    };

    // ---- 2. classes -> one `ui.class(node, &[..])` (records for reload) ----------
    let words = class_words(node);
    if !words.is_empty() {
        body.extend(quote! { #ui.class(#node_ident, &[ #(#words),* ]); });
    }

    // ---- 3. click handler --------------------------------------------------------
    if let Some(closure) = &node.on_click {
        body.extend(quote! { #ui.on_click(#node_ident, #closure); });
    }

    // ---- 4. children for container heads -----------------------------------------
    if mount_kids {
        for child in &node.children {
            match child {
                Child::Text(lit) => {
                    let leaf = fresh(counter);
                    body.extend(quote! {
                        let #leaf = #ui.label(#lit);
                        #ui.mount(#node_ident, #leaf);
                    });
                }
                Child::Splice(expr) => {
                    body.extend(quote! { #ui.mount(#node_ident, #expr); });
                }
                Child::Node(child) => {
                    let child_ident = lower_node(child, ui, body, counter)?;
                    body.extend(quote! { #ui.mount(#node_ident, #child_ident); });
                }
            }
        }
    }

    Ok(node_ident)
}

/// The text of a text-bearing head: `Some(lit)` for exactly one string child, `None`
/// for no children. Errors if it has element/splice children (a text leaf has no
/// element content) or more than one string child.
fn self_text(node: &Node) -> syn::Result<Option<LitStr>> {
    match node.children.as_slice() {
        [] => Ok(None),
        [Child::Text(lit)] => Ok(Some(lit.clone())),
        [Child::Text(_), extra, ..] => Err(child_error(
            extra,
            node,
            "a text element takes a single string of text, not multiple children",
        )),
        [other, ..] => Err(child_error(
            other,
            node,
            "a text element's body is a single string literal (e.g. `Label { \"hi\" }`); \
             nest elements inside a `Column`/`Row` instead",
        )),
    }
}

/// Reject any children on a `bind_text(..)` text element (its text is the closure).
fn reject_text_children(node: &Node, what: &str) -> syn::Result<()> {
    match node.children.first() {
        None => Ok(()),
        Some(child) => Err(child_error(
            child,
            node,
            &alloc_msg(
                what,
                "already gets its text from the closure; remove the body",
            ),
        )),
    }
}

/// Reject `bind_text(..)` on a head that has no text to bind.
fn reject_bind_text(node: &Node, head: &str) -> syn::Result<()> {
    match &node.bind_text {
        None => Ok(()),
        Some(expr) => Err(syn::Error::new_spanned(
            expr,
            alloc_msg(
                head,
                "has no text to bind; `bind_text(..)` is for Label/Text/Button",
            ),
        )),
    }
}

/// Build a `compile_error!` span pointing at a child (or the head if the child has no
/// good span).
fn child_error(child: &Child, node: &Node, msg: &str) -> syn::Error {
    match child {
        Child::Text(lit) => syn::Error::new_spanned(lit, msg),
        Child::Splice(expr) => syn::Error::new_spanned(expr, msg),
        Child::Node(_) => syn::Error::new(node.head_span, msg),
    }
}

/// Split every `class = "a b"` attribute into individual class-name string literals,
/// preserving the attribute's span so an error points at the right place.
fn class_words(node: &Node) -> Vec<LitStr> {
    let mut words = Vec::new();
    for class in &node.classes {
        let value = class.value();
        for word in value.split_whitespace() {
            words.push(LitStr::new(word, class.span()));
        }
    }
    words
}

/// A fresh hygienic binding for an anonymous text leaf.
fn fresh(counter: &mut usize) -> Ident {
    let id = *counter;
    *counter += 1;
    Ident::new(&format!("__rsx_t{id}"), Span::mixed_site())
}

/// Compose `"<head> <tail>"` without pulling `format!` formatting machinery in oddly.
fn alloc_msg(head: &str, tail: &str) -> String {
    let mut s = String::from(head);
    s.push(' ');
    s.push_str(tail);
    s
}
