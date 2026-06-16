//! **Layout browser-oracle conformance** (L2): prove our Stylo→Taffy layout produces the
//! same box geometry a real browser does. For each layout fixture, every element's
//! absolute border box from [`resolve_layout_stylo`] is compared, node-for-node, against
//! the browser's `getBoundingClientRect` over the same tree, within 1px (matching Blitz's
//! WPT `checkLayout` tolerance).
//!
//! The fixtures use explicit sizes / flex ratios (no text-content-dependent sizing), so
//! the geometry is font-independent and the comparison is apples-to-apples. A margin reset
//! in the browser page zeroes the body offset so both place the root box at the origin.
//!
//! `#[ignore]` (shells out to a local browser). Run with:
//!
//! ```text
//! cargo +nightly test --test layout_oracle -- --ignored --nocapture
//! ```

mod common;

use common::{
    diff_box, find_chrome, layout_fixtures, resolve_browser_layout, resolve_layout_stylo,
};

#[test]
#[ignore = "needs a local Chrome; run with `cargo test --test layout_oracle -- --ignored`"]
fn layout_matches_browser() {
    let Some(chrome) = find_chrome() else {
        eprintln!(
            "SKIP layout_matches_browser: no Chrome found (set CANOPY_CHROME=/path/to/chrome)"
        );
        return;
    };
    eprintln!("oracle browser: {chrome}");

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (name, css, tree, viewport) in layout_fixtures() {
        let ours = resolve_layout_stylo(css, &tree, viewport);
        let Some(browser) = resolve_browser_layout(&chrome, css, &tree, viewport) else {
            failures.push(format!("{name}: browser run produced no result"));
            continue;
        };
        if ours.len() != browser.len() {
            failures.push(format!(
                "{name}: node count mismatch (ours {}, browser {})",
                ours.len(),
                browser.len()
            ));
            continue;
        }
        for (i, (o, b)) in ours.iter().zip(browser.iter()).enumerate() {
            for d in diff_box(*o, *b, 1.0) {
                failures.push(format!("{name}[node {i}] {d}"));
            }
            checked += 1;
        }
        eprintln!("  {name}: {} boxes agree", ours.len());
    }

    assert!(
        failures.is_empty(),
        "Layout vs browser mismatches ({} boxes checked):\n  {}",
        checked,
        failures.join("\n  ")
    );
    eprintln!(
        "OK: {checked} boxes match the browser across {} fixtures",
        layout_fixtures().len()
    );
}
