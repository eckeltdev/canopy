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

/// A fixture element: a tag, optional id, classes, optional text, and element children.
#[derive(Clone)]
pub struct El {
    pub tag: &'static str,
    pub id: Option<&'static str>,
    pub classes: Vec<&'static str>,
    pub text: Option<&'static str>,
    pub kids: Vec<El>,
}

/// A `<div>` container with classes and element children.
pub fn div(classes: &[&'static str], kids: Vec<El>) -> El {
    El {
        tag: "div",
        id: None,
        classes: classes.to_vec(),
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
        text: None,
        kids,
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
    let full_css = format!("{UA_CSS}\n{css}");
    let mut engine = StyloEngine::new(&full_css);

    // Build the arena, recording each element's slab id in DFS order.
    let mut ids: Vec<usize> = Vec::new();
    fn build(doc: &mut canopy_style_stylo::Document, parent: usize, el: &El, ids: &mut Vec<usize>) {
        let id = doc.add_element(parent, el.tag, el.id, &el.classes);
        ids.push(id);
        if let Some(t) = el.text {
            doc.add_text(id, t);
        }
        for k in &el.kids {
            build(doc, id, k, ids);
        }
    }
    build(engine.document_mut(), 0, root, &mut ids);

    engine.resolve_styles();
    ids.iter()
        .map(|&id| {
            engine
                .resolve(NodeId::new(id as u64), None)
                .expect("resolve")
        })
        .collect()
}

/// Render the fixture as a standalone HTML page that, on load, computes
/// `getComputedStyle` for every `data-testid` element and writes a JSON array (indexed by
/// testid) into `document.body`'s `data-result` attribute — extractable via Chrome's
/// `--dump-dom`.
pub fn fixture_html(css: &str, root: &El) -> String {
    let mut body = String::new();
    let mut idx = 0usize;
    fn emit(el: &El, idx: &mut usize, out: &mut String) {
        let id_attr = el.id.map(|i| format!(" id=\"{i}\"")).unwrap_or_default();
        let class_attr = if el.classes.is_empty() {
            String::new()
        } else {
            format!(" class=\"{}\"", el.classes.join(" "))
        };
        let testid = *idx;
        *idx += 1;
        let _ = write!(
            out,
            "<{tag} data-testid=\"{testid}\"{id_attr}{class_attr}>",
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
    let html = fixture_html(css, root);
    // Write to a unique-ish temp file (no Date/rand in tests — use the html length + a salt).
    let path = std::env::temp_dir().join(format!("canopy_oracle_{}.html", html.len()));
    std::fs::write(&path, &html).ok()?;

    let out = std::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--hide-scrollbars",
            "--force-device-scale-factor=1",
            "--virtual-time-budget=4000",
            "--dump-dom",
        ])
        .arg(format!("file://{}", path.display()))
        .output()
        .ok()?;
    let dom = String::from_utf8_lossy(&out.stdout);

    // Extract data-result="..." (the value is HTML-attribute-encoded: " -> &quot;).
    let needle = "data-result=\"";
    let start = dom.find(needle)? + needle.len();
    let rest = &dom[start..];
    let end = rest.find('"')?;
    let encoded = &rest[..end];
    let json = html_decode(encoded);

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
