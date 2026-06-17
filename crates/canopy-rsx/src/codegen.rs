//! Lowering the parsed JSX [`crate::ast`] tree to [`canopy_ui::Ui`] builder calls.
//!
//! Every element becomes a `let` binding holding its `NodeId`, so a parent can `mount`
//! each child by handle. The emitted calls go through exactly the `column`/`label`/
//! `button`/`el`/`class`/`mount`/`on_click`/`bind_text` surface a hand-written `Ui`
//! tree uses — there is no second code path.
//!
//! ## Hygiene and paths
//!
//! - The output calls methods on a single `__rsx_ui` binding (the macro's `UI`
//!   expression, evaluated once). It references **no** crate paths: every effect is a
//!   method on the receiver, so a consumer needs only `canopy-ui` in scope. (An `<el>`
//!   `tag={..}` expression is the user's own; whatever path it needs is theirs.)
//! - Per-node bindings use a `__rsx`-prefixed, [`Span::mixed_site`]-spanned identifier
//!   so they cannot capture or be captured by user code: a closure that reads `count`
//!   resolves to the caller's `count`, never to a macro temporary.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Block, Ident, LitStr, Stmt};

use crate::ast::{Child, Element, Rsx, Tag};

/// Lower a whole invocation to a block expression that builds the tree and evaluates to
/// the root `NodeId`. Validation errors become a spanned `compile_error!`.
pub fn expand(rsx: Rsx) -> TokenStream {
    match try_expand(rsx) {
        Ok(ts) => ts,
        Err(e) => e.to_compile_error(),
    }
}

fn try_expand(rsx: Rsx) -> syn::Result<TokenStream> {
    let ui = rsx.ui;
    let ui_ident = Ident::new("__rsx_ui", Span::mixed_site());

    let mut body = TokenStream::new();
    let root_ident = lower(&rsx.root, &ui_ident, &mut body, &mut 0)?;

    Ok(quote! {{
        let #ui_ident = &#ui;
        #body
        #root_ident
    }})
}

/// Emit the statements that build `el` (and its subtree) against `ui`, appending to
/// `body`, and return the identifier bound to this element's id.
fn lower(
    el: &Element,
    ui: &Ident,
    body: &mut TokenStream,
    counter: &mut usize,
) -> syn::Result<Ident> {
    let id = *counter;
    *counter += 1;
    let node = Ident::new(&format!("__rsx_n{id}"), Span::mixed_site());

    // Attributes that don't belong on this tag are rejected up front for clear errors.
    if el.value.is_some() && el.tag != Tag::Input {
        return Err(syn::Error::new(
            el.name_span,
            "`value=\"..\"` is only valid on `<input>`",
        ));
    }
    if el.el_tag.is_some() && el.tag != Tag::El {
        return Err(syn::Error::new(
            el.name_span,
            "`tag={..}` is only valid on `<el>`",
        ));
    }

    // ---- 1. create the node (+ its own text / reactive binding) ------------------
    let mount_kids = match el.tag {
        Tag::Text => {
            let create = text_create(el, ui, quote! { label }, quote! { label_bound })?;
            body.extend(quote! { let #node = #create; });
            false
        }
        Tag::Button => {
            let create = text_create(el, ui, quote! { button }, quote! { button_bound })?;
            body.extend(quote! { let #node = #create; });
            false
        }
        Tag::Input => {
            if !el.children.is_empty() {
                return Err(syn::Error::new(
                    el.name_span,
                    "`<input>` takes no children; use `value=\"..\"` for its initial text",
                ));
            }
            let initial = el
                .value
                .as_ref()
                .map(|v| quote! { #v })
                .unwrap_or_else(|| quote! { "" });
            body.extend(quote! { let #node = #ui.input(#initial); });
            false
        }
        Tag::Div => {
            body.extend(quote! { let #node = #ui.column(); });
            true
        }
        Tag::El => {
            let tag = el
                .el_tag
                .as_ref()
                .ok_or_else(|| syn::Error::new(el.name_span, "`<el>` requires `tag={MY_TAG}`"))?;
            let tag = block_value(tag);
            body.extend(quote! { let #node = #ui.el(#tag); });
            true
        }
    };

    // ---- 1b. element identity (tag-name + id) for a host-side cascade -------------
    // The capable tier carries these so type/id selectors resolve; the lite tier no-ops
    // `Ui::tag`/`Ui::set_id`, so the constrained op-stream is byte-identical.
    if let Some(name) = &el.local_name {
        let name_lit = LitStr::new(name, el.name_span);
        body.extend(quote! { #ui.tag(#node, #name_lit); });
    }
    if let Some(id) = &el.id {
        body.extend(quote! { #ui.set_id(#node, #id); });
    }

    // ---- 2. classes -> one `ui.class(node, &[..])` (records for reload) ----------
    let words = class_words(el);
    if !words.is_empty() {
        body.extend(quote! { #ui.class(#node, &[ #(#words),* ]); });
    }

    // ---- 3. click handler --------------------------------------------------------
    if let Some(handler) = &el.on_click {
        let handler = block_value(handler);
        body.extend(quote! { #ui.on_click(#node, #handler); });
    }

    // ---- 4. children for container tags ------------------------------------------
    if mount_kids {
        for child in &el.children {
            match child {
                Child::Text(lit) => {
                    let leaf = fresh(counter);
                    body.extend(quote! {
                        let #leaf = #ui.label(#lit);
                        #ui.mount(#node, #leaf);
                    });
                }
                Child::Dyn(closure) => {
                    let closure = block_value(closure);
                    let leaf = fresh(counter);
                    body.extend(quote! {
                        let #leaf = #ui.label_bound(#closure);
                        #ui.mount(#node, #leaf);
                    });
                }
                Child::Splice(expr) => {
                    let expr = block_value(expr);
                    body.extend(quote! { #ui.mount(#node, #expr); });
                }
                Child::Element(child) => {
                    let child_ident = lower(child, ui, body, counter)?;
                    body.extend(quote! { #ui.mount(#node, #child_ident); });
                }
            }
        }
    }

    Ok(node)
}

/// The creating expression for a text-bearing tag (`<span>`/`<button>`): a reactive
/// `{closure}` child binds the text (`bound` builder), a single string child sets it
/// (`plain` builder), an empty body is `""`. Element/splice children are an error.
fn text_create(
    el: &Element,
    ui: &Ident,
    plain: TokenStream,
    bound: TokenStream,
) -> syn::Result<TokenStream> {
    match el.children.as_slice() {
        [] => Ok(quote! { #ui.#plain("") }),
        [Child::Text(lit)] => Ok(quote! { #ui.#plain(#lit) }),
        [Child::Dyn(closure)] => {
            let closure = block_value(closure);
            Ok(quote! { #ui.#bound(#closure) })
        }
        _ => Err(syn::Error::new(
            el.name_span,
            "a text element's body is a single string or `{ closure }`; \
             nest other elements inside a `<div>`",
        )),
    }
}

/// Split every `class="a b"` attribute into individual class-name string literals,
/// preserving the attribute's span.
fn class_words(el: &Element) -> Vec<LitStr> {
    let mut words = Vec::new();
    for class in &el.classes {
        let value = class.value();
        for word in value.split_whitespace() {
            words.push(LitStr::new(word, class.span()));
        }
    }
    words
}

/// Emit a `{ .. }` block as a value, unwrapping the braces when it is a single trailing
/// expression (so `{ logo(&ui) }` lowers to `logo(&ui)`, not `{ logo(&ui) }`, avoiding a
/// spurious `unused_braces` warning). Multi-statement blocks (the usual
/// `let c = count.clone(); move |_| ..` capture preamble) keep their braces.
fn block_value(block: &Block) -> TokenStream {
    if let [Stmt::Expr(expr, None)] = block.stmts.as_slice() {
        quote! { #expr }
    } else {
        quote! { #block }
    }
}

/// A fresh hygienic binding for an anonymous text leaf.
fn fresh(counter: &mut usize) -> Ident {
    let id = *counter;
    *counter += 1;
    Ident::new(&format!("__rsx_t{id}"), Span::mixed_site())
}
