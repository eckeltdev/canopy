//! The parsed shape of an `rsx!` invocation — a JSX/HTML-style element tree.
//!
//! This module turns the raw token stream into a typed tree; lowering it to
//! [`crate::Ui`]-shaped method calls is [`crate::codegen`]'s job. Parsing fails here,
//! in one place, with a span pointing at the offending token.
//!
//! # Grammar (JSX-shaped)
//!
//! ```text
//! rsx      := UI "=>" element
//! element  := "<" name attr* ( "/>" | ">" child* "</" name ">" )
//! name     := div | button | span | label | p | input | el
//! attr     := IDENT "=" STRING            // class="a b", value="seed"
//!           | "on" ":" IDENT "=" "{" EXPR "}"   // on:click={closure}
//!           | "tag" "=" "{" EXPR "}"       // <el tag={MY_TAG}> escape hatch
//! child    := STRING                        // static text
//!           | "{" CLOSURE "}"               // reactive text (a `Fn() -> String`)
//!           | "{" EXPR "}"                   // splice an already-built NodeId
//!           | element                        // a nested element
//! ```
//!
//! `UI` is any expression that derefs to a `&canopy_ui::Ui`. Tags are HTML-flavored:
//! `<div>` is a flex container (its row/column direction comes from CSS, like real
//! flexbox), `<span>`/`<label>`/`<p>` are text leaves, `<button>` is a button,
//! `<input/>` a text input, and `<el tag={..}>` is the escape hatch for a host element
//! kind the macro does not name. A `{ .. }` child is reactive text if it is a closure,
//! otherwise an already-built `NodeId` to splice in (e.g. `{ logo(&ui) }`).

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{token, Block, Expr, Ident, LitStr, Stmt, Token};

/// A whole `rsx!(ui => <..>)` invocation.
pub struct Rsx {
    /// The `&Ui` (or deref-to-`&Ui`) expression the tree is built on, evaluated once.
    pub ui: Expr,
    /// The single root element; the macro returns its `NodeId`.
    pub root: Element,
}

impl Parse for Rsx {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ui: Expr = input.parse()?;
        input.parse::<Token![=>]>()?;
        let root: Element = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the rsx! root element"));
        }
        Ok(Rsx { ui, root })
    }
}

/// One element: its tag, attributes, and children.
pub struct Element {
    /// What the tag name mapped to.
    pub tag: Tag,
    /// Span of the tag name, so errors point at it.
    pub name_span: Span,
    /// `class="a b"` attributes (each may name several space-separated classes).
    pub classes: Vec<LitStr>,
    /// An `on:click={ .. }` handler block, if present (a `{ .. }` so it can carry the
    /// usual `let c = count.clone(); move |_| ..` capture preamble).
    pub on_click: Option<Block>,
    /// A `value="seed"` attribute (a text input's initial value), if present.
    pub value: Option<LitStr>,
    /// The `tag={ .. }` element-kind block for an `<el>` head.
    pub el_tag: Option<Block>,
    /// Child nodes, in source order.
    pub children: Vec<Child>,
}

/// Which `Ui` builder a tag name maps to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// `<div>` -> `ui.column()`: a flex container (direction comes from CSS).
    Div,
    /// `<button>` -> `ui.button(..)` / `ui.button_bound(..)`.
    Button,
    /// `<span>`/`<label>`/`<p>` -> `ui.label(..)` / `ui.label_bound(..)`: a text leaf.
    Text,
    /// `<input>` -> `ui.input(..)`: a single-line text input.
    Input,
    /// `<el tag={..}>` -> `ui.el(tag)`: an arbitrary host element kind.
    El,
}

/// A child of an element.
pub enum Child {
    /// A string literal: static text.
    Text(LitStr),
    /// `{ closure }`: reactive text — a block whose trailing expression is a
    /// `Fn() -> String`, re-run on signal change.
    Dyn(Block),
    /// `{ expr }`: an already-built `NodeId` to mount (e.g. a component call).
    Splice(Block),
    /// A nested element (boxed: an `Element` is much larger than the other variants).
    Element(Box<Element>),
}

impl Parse for Element {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<Token![<]>()?;
        let name: Ident = input.parse()?;
        let name_span = name.span();
        let tag = Tag::from_name(&name)?;

        // ---- attributes until `>` or `/>` ---------------------------------------
        let mut classes = Vec::new();
        let mut on_click = None;
        let mut value = None;
        let mut el_tag = None;
        while !input.peek(Token![>]) && !input.peek(Token![/]) {
            let attr_name: Ident = input.parse()?;
            match attr_name.to_string().as_str() {
                "on" => {
                    input.parse::<Token![:]>()?;
                    let event: Ident = input.parse()?;
                    input.parse::<Token![=]>()?;
                    let block: Block = input.parse()?;
                    match event.to_string().as_str() {
                        "click" => {
                            if on_click.replace(block).is_some() {
                                return Err(syn::Error::new_spanned(
                                    &event,
                                    "duplicate `on:click`",
                                ));
                            }
                        }
                        _ => {
                            return Err(syn::Error::new_spanned(
                                &event,
                                "unknown event; only `on:click={..}` is supported",
                            ));
                        }
                    }
                }
                "class" => {
                    input.parse::<Token![=]>()?;
                    classes.push(input.parse::<LitStr>()?);
                }
                "value" => {
                    input.parse::<Token![=]>()?;
                    value = Some(input.parse::<LitStr>()?);
                }
                "tag" => {
                    input.parse::<Token![=]>()?;
                    el_tag = Some(input.parse::<Block>()?);
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        &attr_name,
                        "unknown attribute; expected `class=\"..\"`, `value=\"..\"`, \
                         `on:click={..}`, or (on `<el>`) `tag={..}`",
                    ));
                }
            }
        }

        // ---- self-closing `/>` vs `>` children `</name>` ------------------------
        let mut children = Vec::new();
        if input.peek(Token![/]) {
            input.parse::<Token![/]>()?;
            input.parse::<Token![>]>()?;
        } else {
            input.parse::<Token![>]>()?;
            // Children until the closing `</`.
            while !(input.peek(Token![<]) && input.peek2(Token![/])) {
                if input.is_empty() {
                    return Err(syn::Error::new(
                        name_span,
                        "unclosed element: missing `</..>`",
                    ));
                }
                children.push(input.parse::<Child>()?);
            }
            // Closing tag `</name>`, with a matching name.
            input.parse::<Token![<]>()?;
            input.parse::<Token![/]>()?;
            let close: Ident = input.parse()?;
            if close != name {
                return Err(syn::Error::new_spanned(
                    &close,
                    format!("closing tag `</{close}>` does not match opening tag `<{name}>`"),
                ));
            }
            input.parse::<Token![>]>()?;
        }

        Ok(Element {
            tag,
            name_span,
            classes,
            on_click,
            value,
            el_tag,
            children,
        })
    }
}

impl Parse for Child {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(LitStr) {
            Ok(Child::Text(input.parse()?))
        } else if input.peek(token::Brace) {
            // A `{ .. }` block whose trailing expression is a closure is reactive text;
            // anything else is a NodeId splice (e.g. `{ logo(&ui) }`).
            let block: Block = input.parse()?;
            if trailing_is_closure(&block) {
                Ok(Child::Dyn(block))
            } else {
                Ok(Child::Splice(block))
            }
        } else if input.peek(Token![<]) {
            Ok(Child::Element(Box::new(input.parse()?)))
        } else {
            Err(input.error("expected text \"..\", a `{ expr }`, or a nested `<element>`"))
        }
    }
}

/// Whether a `{ .. }` block's trailing expression is a closure — the signal that a
/// `{ .. }` child is *reactive text* (`Fn() -> String`) rather than a `NodeId` splice.
fn trailing_is_closure(block: &Block) -> bool {
    matches!(block.stmts.last(), Some(Stmt::Expr(Expr::Closure(_), None)))
}

impl Tag {
    /// Map a tag name to its [`Tag`], or error with a span on an unknown name.
    fn from_name(ident: &Ident) -> syn::Result<Self> {
        Ok(match ident.to_string().as_str() {
            "div" => Tag::Div,
            "button" => Tag::Button,
            "span" | "label" | "p" => Tag::Text,
            "input" => Tag::Input,
            "el" => Tag::El,
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "unknown tag; expected `div`, `button`, `span`/`label`/`p`, `input`, \
                     or `el` (with `tag={..}`)",
                ));
            }
        })
    }
}
