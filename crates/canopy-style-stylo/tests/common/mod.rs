//! Shared harness for the cascade tests: a declarative element-tree fixture that can be
//! resolved two ways — through our Stylo [`StyloEngine`] and through a real browser
//! (headless Chrome `getComputedStyle`) — so the two can be compared.
//!
//! The fixture is a tiny HTML-like tree (`El`); each *element* gets a stable depth-first
//! index used as its `data-testid` in the generated HTML, so a node's Stylo result and
//! its browser result line up by index.

#![allow(dead_code)]

use std::fmt::Write as _;

use canopy_protocol::NodeId;
use canopy_style_stylo::StyloEngine;
use canopy_traits::{Color, ComputedStyle, Display, StyleEngine};

/// A minimal UA stylesheet: the block-display defaults a browser applies that affect the
/// (small) set of properties we compare. Prepended to the author CSS on the Stylo side so
/// a bare `<div>` computes `display: block` (its CSS *initial* value is `inline`), matching
/// the browser's UA sheet. The author rules in each fixture still win (higher origin/
/// specificity), so this only fills in defaults.
pub const UA_CSS: &str =
    "div, p, section, header, footer, main, article, h1, h2, h3 { display: block }";

/// A fixture element: a tag, optional id, classes, optional inline `style`, optional
/// text, and element children.
#[derive(Clone)]
pub struct El {
    pub tag: &'static str,
    pub id: Option<&'static str>,
    pub classes: Vec<&'static str>,
    pub style: Option<&'static str>,
    pub text: Option<&'static str>,
    pub kids: Vec<El>,
}

/// A `<div>` container with classes and element children.
pub fn div(classes: &[&'static str], kids: Vec<El>) -> El {
    El {
        tag: "div",
        id: None,
        classes: classes.to_vec(),
        style: None,
        text: None,
        kids,
    }
}

/// A leaf `<div>` carrying text (no element children).
pub fn leaf(classes: &[&'static str], text: &'static str) -> El {
    El {
        tag: "div",
        id: None,
        classes: classes.to_vec(),
        style: None,
        text: Some(text),
        kids: vec![],
    }
}

/// A leaf `<div>` with an id + classes + text.
pub fn leaf_id(id: &'static str, classes: &[&'static str], text: &'static str) -> El {
    El {
        tag: "div",
        id: Some(id),
        classes: classes.to_vec(),
        style: None,
        text: Some(text),
        kids: vec![],
    }
}

/// A `<div>` with an id + classes + children.
pub fn div_id(id: &'static str, classes: &[&'static str], kids: Vec<El>) -> El {
    El {
        tag: "div",
        id: Some(id),
        classes: classes.to_vec(),
        style: None,
        text: None,
        kids,
    }
}

/// An inline-styled `<div>` container (for layout fixtures: explicit sizes/flex).
pub fn bx(style: &'static str, kids: Vec<El>) -> El {
    El {
        tag: "div",
        id: None,
        classes: vec![],
        style: Some(style),
        text: None,
        kids,
    }
}

/// An inline-styled leaf `<div>` (no children).
pub fn bx_leaf(style: &'static str) -> El {
    El {
        tag: "div",
        id: None,
        classes: vec![],
        style: Some(style),
        text: None,
        kids: vec![],
    }
}

/// Flatten the tree into depth-first **element** order (the `data-testid` order).
pub fn dfs<'a>(root: &'a El, out: &mut Vec<&'a El>) {
    out.push(root);
    for k in &root.kids {
        dfs(k, out);
    }
}

/// Build the fixture into a [`StyloEngine`] arena and resolve every element's
/// [`ComputedStyle`] through the real Stylo cascade, returned in depth-first order.
pub fn resolve_stylo(css: &str, root: &El) -> Vec<ComputedStyle> {
    let mut engine = StyloEngine::new(&format!("{UA_CSS}\n{css}"));
    let mut ids: Vec<usize> = Vec::new();
    build_arena(engine.document_mut(), 0, root, &mut ids);

    engine.resolve_styles();
    ids.iter()
        .map(|&id| {
            engine
                .resolve(NodeId::new(id as u64), None)
                .expect("resolve")
        })
        .collect()
}

/// Build the fixture into a [`StyloEngine`] arena, run real Stylo→Taffy **layout** at
/// `viewport`, and return each element's absolute border box in DFS order (the same order
/// [`resolve_stylo`] and the browser's `data-testid` use).
pub fn resolve_layout_stylo(css: &str, root: &El, viewport: (u32, u32)) -> Vec<LayoutBox> {
    let mut engine = StyloEngine::new(&format!("{UA_CSS}\n{css}"));
    let mut ids: Vec<usize> = Vec::new();
    build_arena(engine.document_mut(), 0, root, &mut ids);

    let rects = engine.layout(canopy_traits::Size {
        w: viewport.0 as f32,
        h: viewport.1 as f32,
    });
    rects
        .into_iter()
        .map(|r| LayoutBox {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.w,
            h: r.size.h,
        })
        .collect()
}

/// Recursively build the `El` tree into the arena, recording each element's slab id in
/// DFS (pre-order) order — the order both resolve + layout return values follow.
fn build_arena(
    doc: &mut canopy_style_stylo::Document,
    parent: usize,
    el: &El,
    ids: &mut Vec<usize>,
) {
    let id = doc.add_element(parent, el.tag, el.id, &el.classes);
    ids.push(id);
    if let Some(s) = el.style {
        doc.set_inline_style(id, s);
    }
    if let Some(t) = el.text {
        doc.add_text(id, t);
    }
    for k in &el.kids {
        build_arena(doc, id, k, ids);
    }
}

/// Render the fixture as a standalone HTML page that, on load, computes
/// `getComputedStyle` for every `data-testid` element and writes a JSON array (indexed by
/// testid) into `document.body`'s `data-result` attribute — extractable via Chrome's
/// `--dump-dom`.
pub fn fixture_html(css: &str, root: &El) -> String {
    let body = render_body(root);
    // The reporter: collect getComputedStyle for each testid into an array, stash it.
    let reporter = r#"
    var nodes = Array.from(document.querySelectorAll('[data-testid]'));
    var res = [];
    nodes.forEach(function(n) {
        var i = parseInt(n.getAttribute('data-testid'), 10);
        var cs = getComputedStyle(n);
        res[i] = { color: cs.color, background: cs.backgroundColor, fontSize: cs.fontSize, padding: cs.paddingTop, display: cs.display };
    });
    document.body.setAttribute('data-result', JSON.stringify(res));
    "#;

    format!(
        "<!doctype html><html><head><meta charset=\"utf8\"><style>{css}</style></head><body>{body}<script>{reporter}</script></body></html>"
    )
}

/// Like [`fixture_html`] but reports each element's **`getBoundingClientRect`** (border
/// box, viewport coords). A margin/padding reset zeroes the body offset so the root
/// content box starts at `(0, 0)`, matching our Taffy root's origin.
pub fn fixture_html_layout(css: &str, root: &El) -> String {
    let body = render_body(root);
    let reporter = r#"
    var nodes = Array.from(document.querySelectorAll('[data-testid]'));
    var res = [];
    nodes.forEach(function(n) {
        var i = parseInt(n.getAttribute('data-testid'), 10);
        var r = n.getBoundingClientRect();
        res[i] = { x: r.x, y: r.y, w: r.width, h: r.height };
    });
    document.body.setAttribute('data-result', JSON.stringify(res));
    "#;

    format!(
        "<!doctype html><html><head><meta charset=\"utf8\"><style>html,body{{margin:0;padding:0;border:0}} {css}</style></head><body>{body}<script>{reporter}</script></body></html>"
    )
}

/// Render the element tree to HTML with per-element `data-testid` (DFS order), used by
/// both the cascade and layout pages.
fn render_body(root: &El) -> String {
    let mut body = String::new();
    let mut idx = 0usize;
    fn emit(el: &El, idx: &mut usize, out: &mut String) {
        let id_attr = el.id.map(|i| format!(" id=\"{i}\"")).unwrap_or_default();
        let class_attr = if el.classes.is_empty() {
            String::new()
        } else {
            format!(" class=\"{}\"", el.classes.join(" "))
        };
        let style_attr = el
            .style
            .map(|s| format!(" style=\"{s}\""))
            .unwrap_or_default();
        let testid = *idx;
        *idx += 1;
        let _ = write!(
            out,
            "<{tag} data-testid=\"{testid}\"{id_attr}{class_attr}{style_attr}>",
            tag = el.tag
        );
        if let Some(t) = el.text {
            out.push_str(t);
        }
        for k in &el.kids {
            emit(k, idx, out);
        }
        let _ = write!(out, "</{}>", el.tag);
    }
    emit(root, &mut idx, &mut body);
    body
}

/// A browser-resolved style for one element (the subset we compare).
#[derive(Debug, Clone)]
pub struct BrowserStyle {
    pub color: Color,
    pub background: Color,
    pub font_size: f32,
    pub padding: f32,
    pub display: String,
}

/// Locate a usable Chrome binary (env override, then the macOS default). `None` if absent.
pub fn find_chrome() -> Option<String> {
    if let Ok(p) = std::env::var("CANOPY_CHROME") {
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    candidates
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|p| p.to_string())
}

/// Render the fixture in headless Chrome and return each element's computed style (in
/// `data-testid`/DFS order). `None` if Chrome isn't available or the run failed.
pub fn resolve_browser(chrome: &str, css: &str, root: &El) -> Option<Vec<BrowserStyle>> {
    let json = chrome_json(chrome, &fixture_html(css, root), None)?;

    // Hand-parse the small JSON array. The values (rgb()/px/keywords) never contain `"`,
    // `{`, or `}`, so splitting objects on `},{` and reading `"key":"value"` is exact and
    // avoids a serde_json dependency (whose newer releases pull a crate needing a newer
    // rustc than this repo's pinned toolchain).
    let mut styles = Vec::new();
    for obj in split_objects(&json) {
        styles.push(BrowserStyle {
            color: parse_css_color(field(obj, "color")?)?,
            background: parse_css_color(field(obj, "background")?)?,
            font_size: parse_px(field(obj, "fontSize")?)?,
            padding: parse_px(field(obj, "padding")?)?,
            display: field(obj, "display")?.to_string(),
        });
    }
    Some(styles)
}

/// Split a flat JSON array of objects into per-object substrings (no nesting; values
/// contain no braces).
fn split_objects(json: &str) -> Vec<&str> {
    let trimmed = json.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.trim().is_empty() {
        return Vec::new();
    }
    trimmed.split("},{").collect()
}

/// Read a string field `"key":"value"` out of one object substring (values have no `"`).
fn field<'a>(obj: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = obj.find(&pat)? + pat.len();
    let rest = &obj[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Decode the handful of HTML entities `--dump-dom` produces in an attribute value.
fn html_decode(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Parse a CSS `rgb(r, g, b)` / `rgba(r, g, b, a)` string into a straight-alpha [`Color`].
pub fn parse_css_color(s: &str) -> Option<Color> {
    let inner = s.trim().strip_prefix("rgb")?;
    let inner = inner.strip_prefix('a').unwrap_or(inner);
    let inner = inner.trim().strip_prefix('(')?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }
    let r = parts[0].parse::<f32>().ok()?;
    let g = parts[1].parse::<f32>().ok()?;
    let b = parts[2].parse::<f32>().ok()?;
    let a = if parts.len() >= 4 {
        parts[3].parse::<f32>().ok()?
    } else {
        1.0
    };
    Some(Color {
        r: r.round() as u8,
        g: g.round() as u8,
        b: b.round() as u8,
        a: (a * 255.0).round() as u8,
    })
}

/// Parse a CSS `"24px"` length into pixels.
pub fn parse_px(s: &str) -> Option<f32> {
    s.trim().strip_suffix("px")?.trim().parse::<f32>().ok()
}

/// Canopy [`Display`] as the CSS keyword a browser reports.
pub fn display_keyword(d: Display) -> &'static str {
    match d {
        Display::Block => "block",
        Display::Flex => "flex",
        Display::None => "none",
    }
}

/// Compare a Stylo result against a browser result; returns a list of human-readable
/// mismatches (empty = exact agreement within tolerance).
pub fn diff(stylo: &ComputedStyle, browser: &BrowserStyle) -> Vec<String> {
    let mut out = Vec::new();
    if stylo.color != browser.color {
        out.push(format!(
            "color: stylo {:?} vs browser {:?}",
            stylo.color, browser.color
        ));
    }
    if stylo.background != browser.background {
        out.push(format!(
            "background: stylo {:?} vs browser {:?}",
            stylo.background, browser.background
        ));
    }
    if (stylo.font_size - browser.font_size).abs() > 0.5 {
        out.push(format!(
            "font_size: stylo {} vs browser {}",
            stylo.font_size, browser.font_size
        ));
    }
    if (stylo.padding - browser.padding).abs() > 0.5 {
        out.push(format!(
            "padding: stylo {} vs browser {}",
            stylo.padding, browser.padding
        ));
    }
    if display_keyword(stylo.display) != browser.display {
        out.push(format!(
            "display: stylo {} vs browser {}",
            display_keyword(stylo.display),
            browser.display
        ));
    }
    out
}

/// The shared fixture set: (name, css, tree). Used by both the regression tests and the
/// browser-oracle conformance test.
pub fn fixtures() -> Vec<(&'static str, &'static str, El)> {
    vec![
        (
            "inheritance",
            ".page { color: #ff0000 }",
            div(&["page"], vec![div(&[], vec![leaf(&[], "hi")])]),
        ),
        (
            "specificity_id_class_type",
            "div { color:#000000 } .x { color:#00ff00 } #y { color:#0000ff }",
            div_id("y", &["x"], vec![leaf(&[], "z")]),
        ),
        (
            "specificity_two_classes",
            ".a.b { color:#00ff00 } .a { color:#ff0000 }",
            leaf(&["a", "b"], "z"),
        ),
        (
            "descendant_combinator",
            ".card .title { background:#112233 }",
            div(
                &[],
                vec![
                    div(&["card"], vec![div(&[], vec![leaf(&["title"], "in")])]),
                    leaf(&["title"], "out"),
                ],
            ),
        ),
        (
            "value_extraction",
            ".box { font-size: 24px; padding: 8px; display: flex }",
            leaf(&["box"], "x"),
        ),
        (
            "font_size_inherits",
            ".app { font-size: 20px } .app .child { color: #336699 }",
            div(&["app"], vec![leaf(&["child"], "c")]),
        ),
    ]
}

// ===========================================================================
// Layout oracle (L2): box geometry vs the browser's getBoundingClientRect.
// ===========================================================================

/// A laid-out border box (absolute, viewport coords) — how both our engine and the
/// browser report a node's geometry.
#[derive(Debug, Clone, Copy)]
pub struct LayoutBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Lay the fixture out in headless Chrome and read each element's `getBoundingClientRect`
/// (DFS order), at the given `viewport` window size. `None` if Chrome fails.
pub fn resolve_browser_layout(
    chrome: &str,
    css: &str,
    root: &El,
    viewport: (u32, u32),
) -> Option<Vec<LayoutBox>> {
    let json = chrome_json(chrome, &fixture_html_layout(css, root), Some(viewport))?;
    let mut boxes = Vec::new();
    for obj in split_objects(&json) {
        boxes.push(LayoutBox {
            x: field_num(obj, "x")?,
            y: field_num(obj, "y")?,
            w: field_num(obj, "w")?,
            h: field_num(obj, "h")?,
        });
    }
    Some(boxes)
}

/// Compare two boxes within `tol` px; returns human-readable mismatches.
pub fn diff_box(ours: LayoutBox, browser: LayoutBox, tol: f32) -> Vec<String> {
    let mut out = Vec::new();
    for (name, a, b) in [
        ("x", ours.x, browser.x),
        ("y", ours.y, browser.y),
        ("w", ours.w, browser.w),
        ("h", ours.h, browser.h),
    ] {
        if (a - b).abs() > tol {
            out.push(format!("{name}: ours {a} vs browser {b}"));
        }
    }
    out
}

/// Layout fixtures: explicit-size / flex trees (no text-content-dependent sizing, so the
/// geometry is font-independent and comparable to the browser). `(name, css, tree,
/// viewport)`.
pub fn layout_fixtures() -> Vec<(&'static str, &'static str, El, (u32, u32))> {
    vec![
        (
            "flex_row_grow",
            "",
            bx(
                "display:flex; width:200px; height:100px",
                vec![
                    bx_leaf("flex:1 1 0; height:100px"),
                    bx_leaf("flex:1 1 0; height:100px"),
                ],
            ),
            (400, 300),
        ),
        (
            "block_padding",
            "",
            bx(
                "width:300px; padding:20px",
                vec![bx_leaf("width:100px; height:40px")],
            ),
            (400, 300),
        ),
        (
            "justify_center",
            "",
            bx(
                "display:flex; justify-content:center; width:200px; height:50px",
                vec![bx_leaf("width:40px; height:50px")],
            ),
            (400, 300),
        ),
        (
            "margin_left",
            "",
            bx(
                "width:300px; height:60px",
                vec![bx_leaf("width:50px; height:30px; margin-left:30px")],
            ),
            (400, 300),
        ),
    ]
}

/// Run headless Chrome on `html`, returning the decoded JSON it stashed in
/// `body[data-result]`. Optional `window` size in CSS px.
fn chrome_json(chrome: &str, html: &str, window: Option<(u32, u32)>) -> Option<String> {
    let path = html_path(html);
    std::fs::write(&path, html).ok()?;
    let mut cmd = std::process::Command::new(chrome);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--no-sandbox",
        "--hide-scrollbars",
        "--force-device-scale-factor=1",
        "--virtual-time-budget=4000",
        "--dump-dom",
    ]);
    if let Some((w, h)) = window {
        cmd.arg(format!("--window-size={w},{h}"));
    }
    let out = cmd
        .arg(format!("file://{}", path.display()))
        .output()
        .ok()?;
    let dom = String::from_utf8_lossy(&out.stdout);
    // Extract data-result="..." (the value is HTML-attribute-encoded: " -> &quot;).
    let needle = "data-result=\"";
    let start = dom.find(needle)? + needle.len();
    let rest = &dom[start..];
    let end = rest.find('"')?;
    Some(html_decode(&rest[..end]))
}

/// Content-addressed temp path, so parallel test threads never collide on the same file.
fn html_path(html: &str) -> std::path::PathBuf {
    let mut h: u64 = 5381;
    for b in html.bytes() {
        h = h.wrapping_mul(33) ^ b as u64;
    }
    std::env::temp_dir().join(format!("canopy_oracle_{h:016x}.html"))
}

/// Read a numeric field `"key":<number>` out of one object substring.
fn field_num(obj: &str, key: &str) -> Option<f32> {
    let pat = format!("\"{key}\":");
    let start = obj.find(&pat)? + pat.len();
    let rest = &obj[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    rest[..end].trim().parse::<f32>().ok()
}
