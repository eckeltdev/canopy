//! The parsed shape of an `rsx!` invocation.
//!
//! This module turns the raw token stream into a typed tree; lowering it to
//! [`crate::Ui`]-shaped method calls is [`crate::codegen`]'s job. Keeping parsing
//! separate means malformed input fails here, in one place, with a span pointing at
//! the offending token.
//!
//! # Grammar
//!
//! ```text
//! rsx     := UI "=>" node
//! node    := head tag? attr* children?
//! head    := Column | Row | Button | Label | Text | Input | El
//! tag     := "(" EXPR ")"                 // ONLY for `El(tag)`; the host element kind
//! attr    := "class" "=" STRING           // space-separated class names
//!          | "on_click" "(" EXPR ")"       // a click-handler closure
//!          | "bind_text" "(" EXPR ")"      // bind this node's text to a closure
//! children:= "{" child* "}"               // no separators; source order
//! child   := STRING                        // a static text leaf (or this node's text)
//!          | "{" EXPR "}"                   // splice an already-built NodeId
//!          | node                           // a nested element
//! ```
//!
//! `UI` is any expression that derefs to a `&canopy_ui::Ui`. Text content lives in the
//! `{ .. }` body as a string literal (`Label { "Canopy" }`), or is made reactive with
//! `bind_text(..)`; there are no positional text arguments. `El(tag)` is the escape
//! hatch for a host element kind the macro does not name.

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{braced, parenthesized, token, Expr, Ident, LitStr, Token};

/// A whole `rsx!(ui => node)` invocation.
pub struct Rsx {
    /// The `&Ui` (or deref-to-`&Ui`) expression the tree is built on, evaluated once.
    pub ui: Expr,
    /// The single root node; the macro returns this node's `NodeId`.
    pub root: Node,
}

impl Parse for Rsx {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ui: Expr = input.parse()?;
        input.parse::<Token![=>]>()?;
        let root: Node = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the rsx! root element"));
        }
        Ok(Rsx { ui, root })
    }
}

/// One element in the tree: its kind, attributes, and children.
pub struct Node {
    /// What kind of node the head identifier named.
    pub kind: Kind,
    /// Span of the head identifier, so errors point at the element name.
    pub head_span: Span,
    /// The tag expression for an `El(tag)` head; `None` for the named heads.
    pub tag: Option<Expr>,
    /// `class = "a b"` attributes; each may name several space-separated classes, and
    /// several `class=` attrs accumulate.
    pub classes: Vec<LitStr>,
    /// An `on_click(closure)` handler, if present.
    pub on_click: Option<Expr>,
    /// A `bind_text(closure)` reactive-text binding, if present (valid on text-bearing
    /// heads: `Label`/`Text`/`Button`).
    pub bind_text: Option<Expr>,
    /// Child nodes, in source order.
    pub children: Vec<Child>,
}

/// Which `Ui` builder a node's head maps to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `Column` -> `ui.column()`.
    Column,
    /// `Row` -> `ui.row()`.
    Row,
    /// `Button` -> `ui.button(text)` / `ui.button_bound(f)`.
    Button,
    /// `Label`/`Text` -> `ui.label(text)` / `ui.label_bound(f)`.
    Label,
    /// `Input` -> `ui.input(initial)`.
    Input,
    /// `El(tag)` -> `ui.el(tag)`.
    El,
}

/// A child of a node.
pub enum Child {
    /// A string literal: a static text leaf, or (the single child of a text-bearing
    /// head) that node's own text.
    Text(LitStr),
    /// `{ expr }`: an already-built `NodeId` to mount (e.g. a component call).
    Splice(Expr),
    /// A nested element (boxed: a `Node` is much larger than the other variants).
    Node(Box<Node>),
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let head: Ident = input.parse()?;
        let head_span = head.span();
        let kind = Kind::from_ident(&head)?;

        // `El(tag)` carries its host element kind in parens; the named heads must not.
        let tag = if kind == Kind::El {
            if !input.peek(token::Paren) {
                return Err(syn::Error::new(
                    head_span,
                    "`El` requires a tag expression: `El(MY_TAG)`",
                ));
            }
            let content;
            parenthesized!(content in input);
            let tag: Expr = content.parse()?;
            if !content.is_empty() {
                return Err(content.error("`El(tag)` takes exactly one tag expression"));
            }
            Some(tag)
        } else {
            if input.peek(token::Paren) {
                return Err(syn::Error::new(
                    head_span,
                    "only `El(tag)` takes a parenthesized argument; put text in the body \
                     (e.g. `Label { \"hi\" }`) and use `on_click(..)`/`bind_text(..)` for behaviour",
                ));
            }
            None
        };

        // ---- attributes: class / on_click / bind_text, in any order --------------
        let mut classes = Vec::new();
        let mut on_click = None;
        let mut bind_text = None;
        while input.peek(Ident) {
            let name: Ident = input.fork().parse()?;
            match name.to_string().as_str() {
                "class" => {
                    input.parse::<Ident>()?;
                    input.parse::<Token![=]>()?;
                    classes.push(input.parse::<LitStr>()?);
                }
                "on_click" if input.peek2(token::Paren) => {
                    input.parse::<Ident>()?;
                    let expr = paren_closure(input)?;
                    if on_click.replace(expr).is_some() {
                        return Err(syn::Error::new_spanned(
                            &name,
                            "duplicate `on_click(..)` on one element",
                        ));
                    }
                }
                "bind_text" if input.peek2(token::Paren) => {
                    input.parse::<Ident>()?;
                    let expr = paren_closure(input)?;
                    if bind_text.replace(expr).is_some() {
                        return Err(syn::Error::new_spanned(
                            &name,
                            "duplicate `bind_text(..)` on one element",
                        ));
                    }
                }
                // Not an attribute keyword: only `{ children }` is valid next, so stop
                // scanning attributes and let the children/`Parse` logic take over.
                _ => break,
            }
        }

        // ---- children: an optional `{ child* }` block ----------------------------
        let mut children = Vec::new();
        if input.peek(token::Brace) {
            let content;
            braced!(content in input);
            while !content.is_empty() {
                children.push(content.parse::<Child>()?);
            }
        }

        Ok(Node {
            kind,
            head_span,
            tag,
            classes,
            on_click,
            bind_text,
            children,
        })
    }
}

impl Parse for Child {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(LitStr) {
            Ok(Child::Text(input.parse()?))
        } else if input.peek(token::Brace) {
            let content;
            braced!(content in input);
            let expr: Expr = content.parse()?;
            if !content.is_empty() {
                return Err(
                    content.error("a `{ expr }` child splices exactly one NodeId expression")
                );
            }
            Ok(Child::Splice(expr))
        } else if input.peek(Ident) {
            Ok(Child::Node(Box::new(input.parse()?)))
        } else {
            Err(input.error("expected a string literal, a `{ expr }` splice, or a nested element"))
        }
    }
}

/// Parse `( closure )` and return the single inner expression, erroring on extras.
fn paren_closure(input: ParseStream) -> syn::Result<Expr> {
    let content;
    parenthesized!(content in input);
    let expr: Expr = content.parse()?;
    if !content.is_empty() {
        return Err(content.error("expected exactly one closure argument"));
    }
    Ok(expr)
}

impl Kind {
    /// Map a head identifier to its [`Kind`], or error with a span on an unknown name.
    fn from_ident(ident: &Ident) -> syn::Result<Self> {
        Ok(match ident.to_string().as_str() {
            "Column" => Kind::Column,
            "Row" => Kind::Row,
            "Button" => Kind::Button,
            "Label" | "Text" => Kind::Label,
            "Input" => Kind::Input,
            "El" => Kind::El,
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "unknown rsx! element; expected one of \
                     `Column`, `Row`, `Button`, `Label`/`Text`, `Input`, or `El(tag)`",
                ));
            }
        })
    }
}
