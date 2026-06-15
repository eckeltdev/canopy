//! `canopy-rsx` — Canopy's first-party Rust authoring macro.
//!
//! A locked project decision is that Canopy ships "a first-party Rust macro similar to
//! JSX". This crate is that macro: [`rsx!`]. It accepts a **JSX/HTML-shaped** element
//! tree — angle-bracket tags, `class="..."`, `on:click={..}`, `{ .. }` expression
//! children — and lowers it to method calls on a [`canopy_ui::Ui`] receiver, the
//! batteries-included authoring context (an `App`, a stylesheet, a styled-node
//! registry). A view written in `rsx!` and the same tree written by hand emit a
//! byte-identical op-stream; there is no second runtime.
//!
//! ```ignore
//! use canopy_ui::prelude::*;
//!
//! let ui = Ui::with_css(STYLES);
//! let count = ui.signal(0i32);
//! let root = rsx!(ui =>
//!     <div class="card">
//!         <span class="title">"Canopy"</span>
//!         <button class="btn"
//!             on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
//!             { let c = count.clone(); move || format!("count is {}", c.get()) }
//!         </button>
//!     </div>
//! );
//! ui.mount_root(root);
//! ```
//!
//! # Grammar
//!
//! ```text
//! rsx!( UI => element )
//!
//! element := "<" name attr* ( "/>" | ">" child* "</" name ">" )
//! name    := div | button | span | label | p | input | el
//! attr    := class "=" "STRING"            // space-separated class names
//!          | value "=" "STRING"            // <input> initial text
//!          | on ":" click "=" "{" CLOSURE "}"   // a click handler
//!          | tag "=" "{" EXPR "}"           // <el> host element kind
//! child   := "STRING"                       // static text
//!          | "{" CLOSURE "}"                // reactive text (a `Fn() -> String`)
//!          | "{" EXPR "}"                    // splice an already-built NodeId
//!          | element                         // a nested element
//! ```
//!
//! - **`UI`** is any expression that derefs to a `&canopy_ui::Ui`; evaluated once.
//! - **Tags are HTML-flavored.** `<div>` is a flex container — its row/column direction
//!   comes from CSS (`direction: row`), exactly like real flexbox, so one tag covers
//!   both. `<span>`/`<label>`/`<p>` are text leaves, `<button>` a button, `<input/>` a
//!   text input, and `<el tag={K}>` the escape hatch for a host element kind the macro
//!   does not name.
//! - **Text** is a string child: `<span>"Canopy"</span>`. A `{ closure }` child instead
//!   makes the text reactive (re-emitting one `SetText` per change). For `<span>` and
//!   `<button>` the body is the element's own text; for `<div>` a string/closure child
//!   is mounted as a text leaf.
//! - **`{ expr }` children** that are not closures **splice** an already-built `NodeId`
//!   — this is how components compose: `{ logo(&ui) }`.
//! - **`class="a b"`** lowers to `ui.class(node, &["a", "b"])` (resolved through the
//!   stylesheet *and* recorded for hover/hot-reload). **`on:click={closure}`** lowers to
//!   `ui.on_click(node, closure)`, passing the closure through verbatim.
//!
//! # Dependencies a consumer needs
//!
//! The emitted code references no crate paths — only methods on the `Ui` receiver — so
//! a crate using `rsx!` needs `canopy-ui` in scope and nothing else.

mod ast;
mod codegen;

use proc_macro::TokenStream;
use syn::parse_macro_input;

use ast::Rsx;

/// Build a Canopy view subtree from a JSX-like description, lowering it to
/// [`canopy_ui::Ui`] builder calls and evaluating to the root node's `NodeId`.
///
/// See the [crate-level docs](crate) for the full grammar. The macro does **not** mount
/// the returned root anywhere — the caller decides where it lives (usually
/// `ui.mount_root(root)`).
///
/// ```ignore
/// let root = rsx!(ui =>
///     <div class="root">
///         { logo(&ui) }                              // a component splice
///         <span class="title">"Canopy"</span>        // a text leaf
///         <div class="footer">
///             <button class="pill">"docs"</button>
///             <button class="pill pill-link">"github"</button>
///         </div>
///     </div>
/// );
/// ```
#[proc_macro]
pub fn rsx(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as Rsx);
    codegen::expand(parsed).into()
}
