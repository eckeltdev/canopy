//! Lowering the parsed [`crate::ast`] tree to `canopy_view::App` builder calls.
//!
//! Every node becomes a `let` binding holding its `canopy_protocol::NodeId`, so a
//! parent can `mount` each child by handle and a modifier can attach to the node it
//! follows. The emitted calls go through exactly the same `el`/`label`/`button`/
//! `text_input`/`mount`/`style`/`on_click`/`bind_text` surface a hand-written tree
//! uses — there is no second code path, which is what lets the integration tests
//! assert byte-for-byte equality with a hand-built tree.
//!
//! ## Why these particular tokens
//!
//! * The output references `::canopy_view` by its absolute path so the macro works in
//!   any consumer crate regardless of its `use`s, and never collides with a local item
//!   named `canopy_view`.
//! * Per-node bindings use a `__rsx`-prefixed, [`Span::mixed_site`]-spanned identifier
//!   so they cannot capture or be captured by user code (hygiene): a user closure that
//!   says `count` still resolves to the caller's `count`, never to a macro temporary.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

use crate::ast::{Kind, Modifier, Node, Rsx};

/// Lower a whole invocation to a block expression that builds the tree and evaluates
/// to the root `canopy_protocol::NodeId`.
///
/// The block binds the app once (so `$app` is evaluated a single time even if it is a
/// non-trivial expression) and then builds the root subtree against that binding.
pub fn expand(rsx: Rsx) -> TokenStream {
    let app = rsx.app;
    // One binding for the app, shared by the whole subtree. Spanned at the call site
    // so a type error on the app expression points at the user's code.
    let app_ident = Ident::new("__rsx_app", Span::mixed_site());

    let mut body = TokenStream::new();
    let root_ident = lower_node(&rsx.root, &app_ident, &mut body, &mut 0);

    quote! {{
        // Bind the app once and take a reference to it. `&(expr)` accepts an owned
        // `App` *or* a `&App` (yielding `&App` or `&&App`); method-call auto-ref then
        // resolves `App`'s `&self` methods through either, so the caller may pass
        // whichever they have without us demanding a particular form.
        let #app_ident = &#app;
        #body
        #root_ident
    }}
}

/// Emit the statements that build `node` (and its whole subtree) against `app_ident`,
/// appending them to `body`, and return the identifier bound to this node's id.
///
/// `counter` makes each generated binding unique within the block; it is threaded
/// through the recursion so sibling and descendant temporaries never clash.
fn lower_node(
    node: &Node,
    app_ident: &Ident,
    body: &mut TokenStream,
    counter: &mut usize,
) -> Ident {
    let id = *counter;
    *counter += 1;
    // Hygienic, unique binding for this node's handle. `mixed_site` keeps it invisible
    // to user code so it can never shadow a caller variable of the same spelling.
    let node_ident = Ident::new(&format!("__rsx_n{id}"), Span::mixed_site());

    // ---- 1. create the node, binding its handle ----------------------------------
    //
    // The type (`canopy_protocol::NodeId`) is left to inference so the generated code
    // needs no path to the protocol crate just to name a `let` — the builder return
    // type carries it.
    let create = create_expr(node, app_ident);
    body.extend(quote! {
        let #node_ident = #create;
    });

    // ---- 2. class attributes -> one inline-style write each ----------------------
    //
    // `App` has no first-class "class" concept (styling is resolved by an external
    // stylesheet against `App::style`). So a `class = "name"` is lowered to a single,
    // well-known inline-style write — `app.style(node, CLASS_PROP, "name")` — which
    // carries the class on the node in the op-stream where a host's stylesheet
    // resolver (and a test) can read it back. This is the honest mapping: nothing is
    // silently dropped, and it round-trips through the exact same `style` path a
    // hand-written tree uses.
    //
    // The property id is the documented well-known [`crate::CLASS_PROP_ID`]
    // (`PropId::new(0)`). We construct it inline via `::canopy_protocol` rather than
    // a path into this proc-macro crate, because a `proc-macro = true` crate exports
    // only macros — a consumer cannot name `::canopy_rsx::SOME_CONST`. Consumers that
    // use `class = ".."` therefore depend on `canopy-protocol` directly (as the demo
    // already does); this is stated in the crate docs.
    let class_prop_id = crate::CLASS_PROP_ID;
    for class in &node.classes {
        body.extend(quote! {
            #app_ident.style(
                #node_ident,
                ::canopy_protocol::PropId::new(#class_prop_id),
                #class,
            );
        });
    }

    // ---- 3. explicit inline styles -> `App::style` -------------------------------
    for style in &node.styles {
        let prop = &style.prop;
        let value = &style.value;
        body.extend(quote! {
            #app_ident.style(#node_ident, #prop, #value);
        });
    }

    // ---- 4. modifiers attach to the just-created node ----------------------------
    for modifier in &node.modifiers {
        match modifier {
            Modifier::OnClick(closure) => body.extend(quote! {
                #app_ident.on_click(#node_ident, #closure);
            }),
            Modifier::BindText(closure) => body.extend(quote! {
                #app_ident.bind_text(#node_ident, #closure);
            }),
        }
    }

    // ---- 5. children: build each, then mount it under this node -------------------
    //
    // Children are emitted left-to-right so the op order matches source order (and
    // therefore a hand-written tree built in the same order).
    for child in &node.children {
        let child_ident = lower_node(child, app_ident, body, counter);
        body.extend(quote! {
            #app_ident.mount(#node_ident, #child_ident);
        });
    }

    node_ident
}

/// The expression that creates `node` and yields its `canopy_protocol::NodeId`,
/// chosen by the node's [`Kind`].
fn create_expr(node: &Node, app_ident: &Ident) -> TokenStream {
    // The text/tag the head was given, if any. For text-bearing kinds a missing
    // primary means the empty string; for `El` it is required (checked below).
    let primary = node.primary.as_ref();

    match node.kind {
        Kind::Column => quote! { #app_ident.el(::canopy_view::COLUMN) },
        Kind::Row => quote! { #app_ident.el(::canopy_view::ROW) },
        Kind::Button => {
            // `Button("-")` -> `app.button("-")`. A button with no text is `button("")`.
            let text = primary
                .map(|e| quote! { #e })
                .unwrap_or_else(|| quote! { "" });
            quote! { #app_ident.button(#text) }
        }
        Kind::Label => {
            // `Label("x")` / `Label()` -> `app.label("x")` / `app.label("")`.
            let text = primary
                .map(|e| quote! { #e })
                .unwrap_or_else(|| quote! { "" });
            quote! { #app_ident.label(#text) }
        }
        Kind::Input => {
            // `Input("seed")` / `Input()` -> `app.text_input("seed")` / `text_input("")`.
            let initial = primary
                .map(|e| quote! { #e })
                .unwrap_or_else(|| quote! { "" });
            quote! { #app_ident.text_input(#initial) }
        }
        Kind::El => match primary {
            Some(tag) => quote! { #app_ident.el(#tag) },
            None => {
                // `El` needs a tag expression. Emit a `compile_error!` *spanned at the
                // head* so the message lands on the offending element, not the whole
                // macro, and so codegen stays infallible (no Result threading).
                syn::Error::new(
                    node.head_span,
                    "`El` requires a tag expression: `El(MY_TAG)`",
                )
                .to_compile_error()
            }
        },
    }
}
