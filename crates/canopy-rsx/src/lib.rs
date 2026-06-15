//! `canopy-rsx` — Canopy's first-party Rust authoring macro.
//!
//! A locked project decision is that Canopy ships "a first-party Rust macro similar to
//! JSX" so that hand-authoring a view reads like the tree it builds. This crate is that
//! macro: [`rsx!`]. It lowers to method calls on a [`canopy_ui::Ui`] receiver — the
//! batteries-included authoring context (an `App`, a stylesheet, and a styled-node
//! registry) — so a view written in `rsx!` and the same tree written by hand emit a
//! byte-identical op-stream. There is no second runtime.
//!
//! ```ignore
//! use canopy_ui::prelude::*;
//!
//! let ui = Ui::with_css(STYLES);
//! let count = ui.signal(0i32);
//! let root = rsx!(ui =>
//!     Column class="card" {
//!         Label class="title" { "Canopy" }
//!         Button class="btn"
//!             on_click({ let c = count.clone(); move |_| c.update(|n| *n += 1) })
//!             bind_text({ let c = count.clone(); move || format!("count is {}", c.get()) })
//!     }
//! );
//! ui.mount_root(root);
//! ```
//!
//! # Grammar
//!
//! ```text
//! rsx!( UI => node )
//!
//! node     := head tag? attr* children?
//! head     := Column | Row | Button | Label | Text | Input | El
//! tag      := "(" EXPR ")"                 // ONLY `El(tag)`: a host ElementTag expr
//! attr     := class "=" "STRING"           // space-separated class names
//!           | on_click "(" CLOSURE ")"      // a click handler
//!           | bind_text "(" CLOSURE ")"     // reactive text (Label/Text/Button)
//! children := "{" child* "}"               // no separators; built in source order
//! child    := "STRING"                      // static text leaf / this node's text
//!           | "{" EXPR "}"                   // splice an already-built NodeId
//!           | node                           // a nested element
//! ```
//!
//! - **`UI`** is any expression that derefs to a `&canopy_ui::Ui`; it is evaluated
//!   once.
//! - **Heads** map to `Ui` builders: `Column`/`Row` → `ui.column()`/`ui.row()`;
//!   `Label`/`Text` → `ui.label(..)`; `Button` → `ui.button(..)`; `Input` →
//!   `ui.input(..)`; `El(tag)` → `ui.el(tag)`.
//! - **Text** lives in the body: `Label { "Canopy" }` sets the leaf's text;
//!   `Button { "docs" }` sets the button's label. A `bind_text(closure)` instead makes
//!   the text reactive (`ui.label_bound`/`ui.button_bound`), re-emitting one `SetText`
//!   per change. A node may have static text *or* a `bind_text`, not both.
//! - **`class = "a b"`** lowers to `ui.class(node, &["a", "b"])`, which resolves the
//!   classes through the stylesheet *and* records the node so hover and hot-reload can
//!   replay it.
//! - **`on_click(closure)`** lowers to `ui.on_click(node, closure)`; the closure is
//!   passed through verbatim, capturing the caller's variables with normal semantics.
//! - **Children** of a `Column`/`Row`/`El` are mounted in source order: a string
//!   becomes a text leaf, a `{ expr }` splices an already-built `NodeId` (e.g. a
//!   component call `{ logo(&ui) }`), and a nested head builds a subtree.
//!
//! # Dependencies a consumer needs
//!
//! The emitted code references no crate paths — only methods on the `Ui` receiver — so
//! a crate using `rsx!` needs `canopy-ui` in scope and nothing else. (An `El(tag)`
//! tag expression is the user's own; whatever path it needs is the user's to bring.)

mod ast;
mod codegen;

use proc_macro::TokenStream;
use syn::parse_macro_input;

use ast::Rsx;

/// Build a Canopy view subtree from a JSX-like description, lowering it to
/// [`canopy_ui::Ui`] builder calls and evaluating to the root node's `NodeId`.
///
/// See the [crate-level docs](crate) for the full grammar. The macro does **not**
/// mount the returned root anywhere — the caller decides where the subtree lives
/// (usually `ui.mount_root(root)`).
///
/// ```ignore
/// let root = rsx!(ui =>
///     Column class="root" {
///         { logo(&ui) }                       // a component splice
///         Label class="title" { "Canopy" }    // static text leaf
///         Row class="footer" {
///             Button class="pill" { "docs" }
///             Button class="pill pill-link" { "github" }
///         }
///     }
/// );
/// ```
#[proc_macro]
pub fn rsx(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as Rsx);
    codegen::expand(parsed).into()
}
