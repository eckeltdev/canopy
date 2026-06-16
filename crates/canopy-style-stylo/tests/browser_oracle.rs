//! **Browser-oracle conformance**: prove our Stylo cascade resolves the same computed
//! values a real browser does. For each shared fixture, the resolved [`ComputedStyle`]
//! from our engine is compared, node-for-node, against `getComputedStyle` from a real
//! browser (headless Chrome) over the *same* element tree + CSS.
//!
//! This is the layer where Canopy can be genuinely browser-accurate today: the Stylo
//! cascade resolves true computed values before they are projected to the flat
//! `ComputedStyle`. We compare the five fields that survive that projection — `color`,
//! `background`, `font-size`, `padding`, `display`.
//!
//! Marked `#[ignore]` because it shells out to a local browser. Run it with:
//!
//! ```text
//! cargo +nightly test --test browser_oracle -- --ignored --nocapture
//! ```
//!
//! It auto-discovers Chrome (or honors `CANOPY_CHROME=/path/to/chrome`); if none is
//! found it prints a SKIP and passes, so it never breaks a browser-less machine.

mod common;

use common::{diff, find_chrome, fixtures, resolve_browser, resolve_stylo};

#[test]
#[ignore = "needs a local Chrome; run with `cargo test --test browser_oracle -- --ignored`"]
fn cascade_matches_browser() {
    let Some(chrome) = find_chrome() else {
        eprintln!(
            "SKIP cascade_matches_browser: no Chrome found (set CANOPY_CHROME=/path/to/chrome)"
        );
        return;
    };
    eprintln!("oracle browser: {chrome}");

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for (name, css, tree) in fixtures() {
        let stylo = resolve_stylo(css, &tree);
        let Some(browser) = resolve_browser(&chrome, css, &tree) else {
            failures.push(format!("{name}: browser run produced no result"));
            continue;
        };
        if stylo.len() != browser.len() {
            failures.push(format!(
                "{name}: node count mismatch (stylo {}, browser {})",
                stylo.len(),
                browser.len()
            ));
            continue;
        }
        for (i, (s, b)) in stylo.iter().zip(browser.iter()).enumerate() {
            for d in diff(s, b) {
                failures.push(format!("{name}[node {i}] {d}"));
            }
            checked += 1;
        }
        eprintln!("  {name}: {} nodes agree", stylo.len());
    }

    assert!(
        failures.is_empty(),
        "Stylo vs browser mismatches ({} nodes checked):\n  {}",
        checked,
        failures.join("\n  ")
    );
    eprintln!(
        "OK: {checked} nodes match the browser across {} fixtures",
        fixtures().len()
    );
}
