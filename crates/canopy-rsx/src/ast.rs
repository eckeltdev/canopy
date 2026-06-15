//! The parsed shape of an `rsx!` invocation.
//!
//! This module is *only* concerned with turning the raw token stream into a typed
//! tree — it does not know how to emit `App` calls (that is [`crate::codegen`]'s job).
//! Keeping parsing separate means malformed input fails here, in one place, with a
//! span pointing at the offending token, instead of producing a confusing error deep
//! inside the generated code.
//!
//! # Grammar (the surface the macro accepts)
//!
//! ```text
//! rsx        := APP "=>" node
//! node       := head args? modifier* children?
//! head       := IDENT                       // Column | Row | Button | Label | Input | Text | El
//! args       := "(" arg ("," arg)* ")"      // present even when empty: `Label()`
//! arg        := EXPR                         //   first positional arg is the node's text/tag
//!             | IDENT "=" EXPR               //   a named attribute, e.g. `class = "root"`
//!             | "style" "(" EXPR "," EXPR ")"//   an inline style: `style(BG, "#101")`
//! modifier   := "on_click" "(" EXPR ")"      // attach a click handler closure
//!             | "bind_text" "(" EXPR ")"      // bind this node's text to a closure
//! children   := "{" ( node ";" )* "}"        // nested nodes, each terminated by `;`
//! ```
//!
//! `APP` is any expression evaluating to something that derefs to a `&canopy_view::App`.
//! The first *positional* argument of a node is its text (for `Button`/`Label`/`Input`)
//! or its tag expression (for the `El(tag)` escape hatch); named/`style(..)` arguments
//! may follow it in any order.

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{braced, parenthesized, token, Expr, Ident, LitStr, Token};

/// A whole `rsx!(app => node)` invocation.
pub struct Rsx {
    /// The `&App` (or deref-to-`&App`) expression the tree is built on.
    pub app: Expr,
    /// The single root node; the macro returns this node's `canopy_view`-minted id.
    pub root: Node,
}

impl Parse for Rsx {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let app: Expr = input.parse()?;
        input.parse::<Token![=>]>()?;
        let root: Node = input.parse()?;
        // Reject trailing tokens so `rsx!(app => Column {} extra)` is an error here
        // rather than silently dropping `extra`.
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the rsx! root element"));
        }
        Ok(Rsx { app, root })
    }
}

/// One element in the tree: its kind, its arguments, attached modifiers, and children.
pub struct Node {
    /// What kind of node the head identifier named (e.g. `Column`, `Button`).
    pub kind: Kind,
    /// The span of the head identifier, used to point errors at the element name.
    pub head_span: Span,
    /// The first positional argument — the text (`Button`/`Label`/`Input`) or tag
    /// (`El`) — if one was supplied. `None` for e.g. `Label()` or `Column`.
    pub primary: Option<Expr>,
    /// `class = "..."` attributes, recorded in source order.
    pub classes: Vec<LitStr>,
    /// `style(PROP, "value")` inline-style attributes, in source order.
    pub styles: Vec<StyleAttr>,
    /// `on_click(closure)` / `bind_text(closure)` modifiers, in source order.
    pub modifiers: Vec<Modifier>,
    /// Nested child nodes, in source order.
    pub children: Vec<Node>,
}

/// Which builder a node's head maps to.
pub enum Kind {
    /// `Column` -> `app.el(COLUMN)`.
    Column,
    /// `Row` -> `app.el(ROW)`.
    Row,
    /// `Button(text)` -> `app.button(text)`.
    Button,
    /// `Label(text)` / `Label()` -> `app.label(text)` / `app.label("")`.
    /// `Text` is an accepted alias since "label" reads oddly for a bare string leaf.
    Label,
    /// `Input(initial)` -> `app.text_input(initial)`.
    Input,
    /// `El(tag_expr)` -> `app.el(tag_expr)`: the escape hatch for host-defined element
    /// kinds the macro does not name.
    El,
}

/// A `style(PROP, "value")` inline-style attribute: a `canopy_protocol::PropId`
/// expression and a string value, lowered to `app.style(node, prop, value)`.
pub struct StyleAttr {
    /// The property-id expression (any `PropId`-typed expression).
    pub prop: Expr,
    /// The value to set; lowered as a `&str` argument to `App::style`.
    pub value: Expr,
}

/// A behavioural modifier attached to the just-created node.
pub enum Modifier {
    /// `on_click(closure)`: lowered to `app.on_click(node, closure)`.
    OnClick(Expr),
    /// `bind_text(closure)`: lowered to `app.bind_text(node, closure)`.
    BindText(Expr),
}

/// One argument inside a node's `(...)` list, after splitting positional from named.
enum Arg {
    /// A bare expression: the node's text or tag.
    Positional(Expr),
    /// `name = value`.
    Named { name: Ident, value: Expr },
    /// `style(prop, value)`.
    Style(StyleAttr),
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let head: Ident = input.parse()?;
        let head_span = head.span();
        let kind = Kind::from_ident(&head)?;

        // ---- argument list: present iff the next token is `(` --------------------
        let mut primary = None;
        let mut classes = Vec::new();
        let mut styles = Vec::new();
        if input.peek(token::Paren) {
            let content;
            parenthesized!(content in input);
            let args = Punctuated::<Arg, Token![,]>::parse_terminated(&content)?;
            for arg in args {
                match arg {
                    Arg::Positional(expr) => {
                        if primary.is_some() {
                            return Err(syn::Error::new_spanned(
                                &expr,
                                "an rsx! element takes at most one positional (text/tag) \
                                 argument; use `class = ..` or `style(..)` for the rest",
                            ));
                        }
                        primary = Some(expr);
                    }
                    Arg::Named { name, value } => {
                        if name == "class" {
                            classes.push(as_str_lit(&name, value)?);
                        } else {
                            return Err(syn::Error::new_spanned(
                                &name,
                                "unknown rsx! attribute; the only named attribute is \
                                 `class = \"..\"` (use `style(PROP, \"..\")` for inline styles)",
                            ));
                        }
                    }
                    Arg::Style(style) => styles.push(style),
                }
            }
        }

        // ---- modifiers: zero or more `ident(expr)` after the args ----------------
        let mut modifiers = Vec::new();
        while input.peek(Ident) && input.peek2(token::Paren) {
            let name: Ident = input.parse()?;
            let content;
            parenthesized!(content in input);
            let expr: Expr = content.parse()?;
            if !content.is_empty() {
                return Err(content.error("a modifier takes exactly one closure argument"));
            }
            if name == "on_click" {
                modifiers.push(Modifier::OnClick(expr));
            } else if name == "bind_text" {
                modifiers.push(Modifier::BindText(expr));
            } else {
                return Err(syn::Error::new_spanned(
                    &name,
                    "unknown rsx! modifier; expected `on_click(..)` or `bind_text(..)`",
                ));
            }
        }

        // ---- children: an optional `{ node; node; .. }` block --------------------
        let mut children = Vec::new();
        if input.peek(token::Brace) {
            let content;
            braced!(content in input);
            while !content.is_empty() {
                children.push(content.parse::<Node>()?);
                // Each child statement is terminated by `;`; allow the last to omit it.
                if content.is_empty() {
                    break;
                }
                content.parse::<Token![;]>()?;
            }
        }

        Ok(Node {
            kind,
            head_span,
            primary,
            classes,
            styles,
            modifiers,
            children,
        })
    }
}

impl Parse for Arg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // `style(PROP, "val")` is recognised by its keyword head + paren, before we
        // try the `name = value` / bare-expression forms.
        if input.peek(Ident) && input.peek2(token::Paren) {
            let fork = input.fork();
            let name: Ident = fork.parse()?;
            if name == "style" {
                input.parse::<Ident>()?; // consume `style`
                let content;
                parenthesized!(content in input);
                let prop: Expr = content.parse()?;
                content.parse::<Token![,]>()?;
                let value: Expr = content.parse()?;
                // Tolerate a trailing comma inside `style(.., ..,)`.
                if content.peek(Token![,]) {
                    content.parse::<Token![,]>()?;
                }
                if !content.is_empty() {
                    return Err(content.error("style(PROP, \"value\") takes exactly two arguments"));
                }
                return Ok(Arg::Style(StyleAttr { prop, value }));
            }
        }

        // `name = value` (named attribute) vs a bare positional expression.
        if input.peek(Ident) && input.peek2(Token![=]) {
            let name: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: Expr = input.parse()?;
            return Ok(Arg::Named { name, value });
        }

        Ok(Arg::Positional(input.parse()?))
    }
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

/// Coerce a `class = <value>` value to a string literal, erroring otherwise.
///
/// Classes are *names*, not arbitrary expressions: keeping them literal lets the macro
/// (and a reader) see exactly what class a node carries, and matches the CSS-lite
/// stylesheet model where a class is a static selector.
fn as_str_lit(name: &Ident, value: Expr) -> syn::Result<LitStr> {
    match value {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(s),
        other => Err(syn::Error::new_spanned(
            other,
            format!("`{name} = ..` expects a string literal, e.g. `{name} = \"root\"`"),
        )),
    }
}
