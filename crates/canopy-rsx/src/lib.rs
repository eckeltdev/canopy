//! `canopy-rsx` — Canopy's first-party Rust authoring macro.
//!
//! A locked project decision is that Canopy ships "a first-party Rust macro similar
//! to JSX" so that hand-authoring a view in native Rust reads like the tree it builds,
//! while still lowering to nothing more than `canopy_view::App` builder calls. This
//! crate is that macro: [`rsx!`].
//!
//! (Cross-crate names like `canopy_view::App` are written as plain code below rather
//! than as doc links: this is a `proc-macro = true` crate, so `canopy-view` and
//! `canopy-protocol` are *dev*-dependencies — they are not in the macro library's own
//! dependency graph to link against.)
//!
//! ```ignore
//! let root = rsx!(app => Column(class = "root") {
//!     Label("Canopy demo");
//!     Row(class = "row") {
//!         Button("-") on_click(move |_| count.update(|n| *n -= 1));
//!         Label()     bind_text(move || format!("Count: {}", count.get()));
//!         Button("+") on_click(move |_| count.update(|n| *n += 1));
//!     }
//! });
//! ```
//!
//! That expands to the equivalent `app.el(ROW)` / `app.button("-")` / `app.mount(..)`
//! / `app.on_click(..)` / `app.bind_text(..)` calls and evaluates to the root
//! `canopy_protocol::NodeId`. There is **no second code path**: the macro emits the
//! exact ops a hand-written tree emits, in the same order — which is what lets
//! `canopy-rsx`'s own tests assert byte-for-byte equality with a hand-built tree.
//!
//! # Why a proc-macro (not `macro_rules!`)
//!
//! `canopy-view` already ships a small `macro_rules!` `rsx!` for the simplest cases.
//! This crate is the richer, JSX-shaped surface: a real parser gives precise,
//! span-pointed compile errors for malformed input (an unknown element name underlines
//! *that* identifier), full hygiene (a user closure's `count` is always the caller's
//! `count`, never a macro temporary), and room to grow the grammar without fighting
//! `macro_rules!` token-tree ambiguities.
//!
//! # Grammar
//!
//! ```text
//! rsx!( APP => NODE )
//!
//! NODE      := HEAD ARGS? MODIFIER* CHILDREN?
//! HEAD      := Column | Row | Button | Label | Text | Input | El
//! ARGS      := "(" ARG ("," ARG)* ","? ")"          // optional; `Label()` is allowed
//! ARG       := EXPR                                  // the one positional text/tag arg
//!            | class "=" "STRING"                    // a class name (a string literal)
//!            | style "(" PROP_EXPR "," VALUE_EXPR ")"// an inline style
//! MODIFIER  := on_click "(" CLOSURE ")"
//!            | bind_text "(" CLOSURE ")"
//! CHILDREN  := "{" ( NODE ";" )* "}"                 // each child is `;`-terminated
//! ```
//!
//! * `APP` is any expression that derefs to a `&canopy_view::App` — pass an `App`,
//!   a `&App`, or e.g. an `Rc<App>` deref; it is evaluated exactly once.
//! * **Element heads** map to builders:
//!   `Column`/`Row` → `App::el` with `canopy_view::COLUMN`/`ROW`;
//!   `Button(text)` → `App::button`;
//!   `Label(text)`/`Label()`/`Text(text)` → `App::label` (an absent text is `""`);
//!   `Input(initial)`/`Input()` → `App::text_input`;
//!   `El(tag_expr)` is the escape hatch for any host-defined `ElementTag` the macro
//!   does not name.
//! * **`class = "name"`** is recorded as a single, well-known inline-style write
//!   (`app.style(node, PropId::new(0), "name")` — id `0` is the reserved class slot).
//!   `App` has no built-in class concept — styling is resolved by an external
//!   stylesheet against `App::style` — so the macro carries the class on the node in
//!   the op-stream where a host's resolver (and a test) can read it back. The mapping
//!   is honest: it round-trips through the same `style` path a hand-written tree uses,
//!   dropping nothing.
//! * **`style(PROP, "value")`** lowers directly to `App::style`; `PROP` is any
//!   `canopy_protocol::PropId` expression, leaving the macro decoupled from any
//!   particular host's property registry.
//! * **Modifiers** attach to the node they follow: `on_click(closure)` →
//!   `App::on_click`, `bind_text(closure)` → `App::bind_text`. The closure is passed
//!   through verbatim, so it captures the caller's variables with normal Rust
//!   semantics.
//! * **Children** are built and `mount`ed left-to-right, so the emitted op order
//!   matches source order.
//!
//! # Dependencies a consumer needs
//!
//! Emitted code references `::canopy_view` (always) and, **only if you use
//! `class = ".."`**, `::canopy_protocol` (to build the class `PropId`). A crate that
//! uses `rsx!` therefore depends on `canopy-view`, and additionally on
//! `canopy-protocol` if it uses class attributes — both of which a real Canopy
//! consumer (e.g. the demo) already has.

mod ast;
mod codegen;

use proc_macro::TokenStream;
use syn::parse_macro_input;

use ast::Rsx;

/// The well-known `canopy_protocol::PropId` raw value that a `class = ".."`
/// attribute writes the class name under.
///
/// `App` exposes styling only as `style(node, PropId, &str)`, so the macro needs a
/// stable property id to carry a class on a node in the op-stream. Raw id `0` is
/// reserved for this "class list" slot by convention; a host's stylesheet resolver
/// reads the value back to look up its class rules.
///
/// It cannot be a `pub` item — a `proc-macro = true` crate may export only macros —
/// so consumers that want to decode the class slot match on `PropId::new(0)` directly;
/// the number is documented here and in the crate-level docs, and exercised in the
/// macro's own integration tests. The type matches `PropId`'s `u16` repr so the
/// emitted `PropId::new(..)` literal type-checks without a cast.
pub(crate) const CLASS_PROP_ID: u16 = 0;

/// Build a Canopy view subtree from a JSX-like description, lowering it to
/// `canopy_view::App` builder calls and evaluating to the root node's
/// `canopy_protocol::NodeId`.
///
/// See the [crate-level docs](crate) for the full grammar and the element/attribute/
/// modifier mappings. In brief:
///
/// ```ignore
/// let root = rsx!(&app => Column(class = "root") {
///     Label("Canopy demo");
///     Row(class = "row") {
///         Button("-") on_click(move |_| count.update(|n| *n -= 1));
///         Label()     bind_text(move || format!("Count: {}", count.get()));
///         Button("+") on_click(move |_| count.update(|n| *n += 1));
///     }
/// });
/// // `root` is the column's NodeId; mount it wherever the host wants it.
/// ```
///
/// The macro does **not** mount the returned root anywhere — the caller decides where
/// the subtree lives (usually under the host root).
#[proc_macro]
pub fn rsx(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as Rsx);
    codegen::expand(parsed).into()
}
