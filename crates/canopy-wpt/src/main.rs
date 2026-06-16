//! Canopy's Web Platform Tests (WPT) conformance runner.
//!
//! Ported from Blitz's `wpt/runner` (`main.rs`, `test_runners/*`), adapted to
//! Canopy's `canopy-style-stylo` engine (the SAME Stylo + Taffy stack Blitz
//! runs). It runs REAL WPT CSS tests and prints honest pass/fail/skip numbers.
//!
//! Two test kinds (Blitz's taxonomy):
//!   * **ATTR / checkLayout** — HTML whose elements carry `data-expected-width`,
//!     `data-expected-height`, `data-expected-padding-*`, `data-expected-margin-*`,
//!     `data-offset-x/y`, and a `checkLayout('selector')` script call. We resolve
//!     layout and check each such element's Taffy box against the `data-expected-*`
//!     values with ±1px tolerance. No rendering needed.
//!   * **REF** — a file with `<link rel="match" href="...-ref.html">`. We render
//!     BOTH test and ref to RGBA buffers (same renderer) and compare:
//!     reject-blank -> exact-equality -> per-pixel max-channel diff at a threshold.
//!
//! Honesty over green: many flexbox tests size boxes by their TEXT contents, and
//! our engine does NOT measure text (leaves get size purely from their `Style`).
//! Those would spuriously fail, so we feature-gate them into SKIP buckets (see
//! [`SkipReason`]) — exactly the spirit of Blitz's feature flags. The value is a
//! working harness + an accurate baseline with a clear skip taxonomy.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{env, fs};

mod test_runners;

use test_runners::{run_attr_test, run_ref_test, AttrOutcome, RefOutcome};

/// Viewport the engine lays out / renders at. Matches Blitz (800x600).
const WIDTH: f32 = 800.0;
const HEIGHT: f32 = 600.0;

/// Tests known to crash / hang / be pathological. Skipped up front (Blitz keeps
/// the same idea — a hand-maintained block list). Matched as a path suffix.
const BLOCKED_TESTS: &[&str] = &[
    // xhtml variants we don't special-case; harmless to skip.
    "css/css-flexbox/flexbox-paint-ordering-002.xhtml",
];

/// Why a test was skipped (not run). Kept granular so the summary reads as an
/// honest taxonomy rather than one opaque "skipped" bucket.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SkipReason {
    /// Neither `checkLayout(` nor `rel="match"` — no assertion we support.
    NoSupportedAssertion,
    /// On the hand-maintained block list.
    Blocked,
    /// Sizes boxes by TEXT content / Ahem-font metrics — we do not measure text,
    /// so the expected boxes are unreachable. Biggest single bucket.
    TextDependent,
    /// Uses `writing-mode` / vertical flow — Taffy/our mapping doesn't do it.
    WritingMode,
    /// Uses `position: absolute` / abspos flexbox children — not modeled here.
    AbsolutePosition,
    /// Uses script beyond a single `checkLayout(...)` call (dynamic DOM/JS).
    Script,
    /// A multi-call `checkLayout` test (we only handle a single selector).
    MultiCheckLayout,
}

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            SkipReason::NoSupportedAssertion => "no supported assertion",
            SkipReason::Blocked => "blocked (crash/hang list)",
            SkipReason::TextDependent => "text-dependent sizing (no text measurement)",
            SkipReason::WritingMode => "writing-mode / vertical flow",
            SkipReason::AbsolutePosition => "position:absolute",
            SkipReason::Script => "script beyond checkLayout",
            SkipReason::MultiCheckLayout => "multiple checkLayout() calls",
        }
    }
}

/// Final disposition of one test.
enum Outcome {
    Pass,
    Fail(String),
    Skip(SkipReason),
    Crash(String),
}

/// One test's name + outcome + kind tag, for the report file.
struct TestResult {
    name: String,
    kind: &'static str,
    outcome: Outcome,
}

/// `true` if this is a test we should attempt (not a -ref/reference/support file
/// and not on the block list). Mirrors Blitz's `filter_path`.
fn is_test_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let is_ref = s.ends_with("-ref.html")
        || s.ends_with("-ref.htm")
        || s.ends_with("-ref.xhtml")
        || s.ends_with("-ref.xht")
        || path_has_dir(p, "reference");
    let is_support = path_has_dir(p, "support");
    let is_dir = p.is_dir();
    // NOTE: blocked tests are NOT filtered here — they're kept in the test list
    // and classified as Skip(Blocked) so they show up in the taxonomy.
    !(is_ref || is_support || is_dir)
}

/// True if `p` is on the hand-maintained crash/hang block list (suffix match).
fn is_blocked(p: &Path) -> bool {
    let s = p.to_string_lossy();
    BLOCKED_TESTS.iter().any(|suffix| s.ends_with(suffix))
}

fn path_has_dir(path: &Path, dir: &str) -> bool {
    path.components().any(|c| c.as_os_str() == dir)
}

/// Glob a suite directory for test files (Blitz's `collect_tests`, single suite).
fn collect_tests(suite_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for pat in ["/**/*.htm", "/**/*.html", "/**/*.xht", "/**/*.xhtml"] {
        let pattern = format!("{}{}", suite_dir.display(), pat);
        let Ok(results) = glob::glob(&pattern) else {
            continue;
        };
        for entry in results.flatten() {
            if is_test_path(&entry) {
                paths.push(entry);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Detect feature-gate SKIPs from the raw file text (Blitz's regex idea, done
/// with plain substring scans to avoid a regex dep). Returns the FIRST matching
/// reason, or `None` if nothing forces a skip.
///
/// `is_attr` flips the script check: an ATTR test legitimately has a `<script>`
/// for `checkLayout`, so we only skip on script for OTHER scripts.
fn detect_skip(src: &str, is_attr: bool) -> Option<SkipReason> {
    let lower = src.to_ascii_lowercase();

    // writing-mode / vertical flow — Taffy mapping doesn't model it.
    if lower.contains("writing-mode")
        || lower.contains("vertical-rl")
        || lower.contains("vertical-lr")
    {
        return Some(SkipReason::WritingMode);
    }

    // Absolute positioning — not modeled in our Taffy wiring.
    if lower.contains("position:absolute") || lower.contains("position: absolute") {
        return Some(SkipReason::AbsolutePosition);
    }

    // TEXT-dependent sizing. Our engine does NOT measure text, so any box whose
    // size comes from its text content (or the Ahem font, which WPT uses for
    // deterministic glyph metrics) is unreachable. Heuristics:
    //   * the Ahem font is loaded/used, or
    //   * an intrinsic content keyword sizes a box from contents, or
    //   * a flex item explicitly has `height:auto`/`width:auto` "from contents".
    if lower.contains("ahem")
        || lower.contains("font-family: ahem")
        || lower.contains("max-content")
        || lower.contains("min-content")
        || lower.contains("fit-content")
    {
        return Some(SkipReason::TextDependent);
    }

    // Script beyond checkLayout. For ATTR tests we expect exactly the harness
    // scripts; flag tests that wire up extra behavior via inline event handlers
    // or non-harness `<script>` blocks containing logic.
    if !is_attr {
        // For REF tests, any script at all is suspect (dynamic rendering).
        if lower.contains("<script") && !lower.contains("check-layout") {
            return Some(SkipReason::Script);
        }
    }

    None
}

/// Classify and run a single test file. Never panics out (callers catch_unwind
/// around this for belt-and-suspenders), returns a fully-formed [`TestResult`].
fn process_test(rel_name: &str, abs_path: &Path) -> TestResult {
    if is_blocked(abs_path) {
        return TestResult {
            name: rel_name.to_string(),
            kind: "UNK",
            outcome: Outcome::Skip(SkipReason::Blocked),
        };
    }

    let src = match fs::read_to_string(abs_path) {
        Ok(s) => s,
        Err(e) => {
            return TestResult {
                name: rel_name.to_string(),
                kind: "UNK",
                outcome: Outcome::Skip(SkipReason::NoSupportedAssertion),
                // read errors are rare; treat as skip with a note via Crash text
            }
            .with_read_error(e);
        }
    };

    let has_check_layout = src.contains("checkLayout(");
    let has_match = src.contains("rel=\"match\"") || src.contains("rel='match'");

    // Classification: ATTR (checkLayout) takes precedence, then REF, else SKIP.
    if has_check_layout {
        // Multiple checkLayout calls: we only handle a single selector (Blitz too).
        let call_count = src.matches("checkLayout(").count();
        if call_count > 1 {
            return TestResult {
                name: rel_name.to_string(),
                kind: "ATT",
                outcome: Outcome::Skip(SkipReason::MultiCheckLayout),
            };
        }

        if let Some(reason) = detect_skip(&src, true) {
            return TestResult {
                name: rel_name.to_string(),
                kind: "ATT",
                outcome: Outcome::Skip(reason),
            };
        }

        let outcome = match catch_unwind(AssertUnwindSafe(|| run_attr_test(&src))) {
            Ok(AttrOutcome::Pass) => Outcome::Pass,
            Ok(AttrOutcome::Fail(msg)) => Outcome::Fail(msg),
            Ok(AttrOutcome::NoTarget) => Outcome::Skip(SkipReason::NoSupportedAssertion),
            Err(_) => Outcome::Crash("panic during attr layout".to_string()),
        };
        return TestResult {
            name: rel_name.to_string(),
            kind: "ATT",
            outcome,
        };
    }

    if has_match {
        if let Some(reason) = detect_skip(&src, false) {
            return TestResult {
                name: rel_name.to_string(),
                kind: "REF",
                outcome: Outcome::Skip(reason),
            };
        }

        let outcome = match catch_unwind(AssertUnwindSafe(|| {
            run_ref_test(&src, abs_path, WIDTH, HEIGHT)
        })) {
            Ok(RefOutcome::Pass) => Outcome::Pass,
            Ok(RefOutcome::Fail(msg)) => Outcome::Fail(msg),
            Ok(RefOutcome::Skip(msg)) => {
                // ref file missing/unreadable etc — count as skip with a note.
                Outcome::Fail(msg)
            }
            Err(_) => Outcome::Crash("panic during ref render".to_string()),
        };
        return TestResult {
            name: rel_name.to_string(),
            kind: "REF",
            outcome,
        };
    }

    // No supported assertion.
    TestResult {
        name: rel_name.to_string(),
        kind: "UNK",
        outcome: Outcome::Skip(SkipReason::NoSupportedAssertion),
    }
}

impl TestResult {
    fn with_read_error(mut self, e: std::io::Error) -> Self {
        self.outcome = Outcome::Crash(format!("read error: {e}"));
        self
    }
}

fn main() {
    // Args / env: suite dir (positional, default css-flexbox) + WPT root.
    let args: Vec<String> = env::args().skip(1).collect();
    let wpt_root = env::var("WPT_DIR").unwrap_or_else(|_| "/tmp/wpt".to_string());
    let suite_dir = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| format!("{wpt_root}/css/css-flexbox"));

    let suite_path = PathBuf::from(&suite_dir);
    let wpt_root_path = PathBuf::from(&wpt_root);
    if !suite_path.exists() {
        eprintln!("Suite dir does not exist: {suite_dir}");
        std::process::exit(1);
    }

    let tests = collect_tests(&suite_path);
    let total = tests.len();
    eprintln!(
        "canopy-wpt: {total} tests found under {}",
        suite_path.display()
    );

    let start = Instant::now();
    let mut results: Vec<TestResult> = Vec::with_capacity(total);

    for (i, path) in tests.iter().enumerate() {
        // Relative name for display/report (relative to wpt root if possible).
        let rel = path
            .strip_prefix(&wpt_root_path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        // Catch a panic even at the classify/read layer so one bad file can
        // never take down the whole run.
        let result =
            catch_unwind(AssertUnwindSafe(|| process_test(&rel, path))).unwrap_or_else(|_| {
                TestResult {
                    name: rel.clone(),
                    kind: "UNK",
                    outcome: Outcome::Crash("panic during classification".to_string()),
                }
            });

        // Progress line (stderr so the summary on stdout stays clean).
        let tag = match &result.outcome {
            Outcome::Pass => "PASS",
            Outcome::Fail(_) => "FAIL",
            Outcome::Skip(_) => "SKIP",
            Outcome::Crash(_) => "CRASH",
        };
        eprintln!(
            "[{:>4}/{total}] {tag} {} ({})",
            i + 1,
            result.name,
            result.kind
        );

        results.push(result);
    }

    let elapsed = start.elapsed();

    // ---- Tally ----
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut crash = 0u32;
    let mut skip = 0u32;
    let mut skip_by_reason: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut attr_pass = 0u32;
    let mut attr_run = 0u32;
    let mut ref_pass = 0u32;
    let mut ref_run = 0u32;

    for r in &results {
        match &r.outcome {
            Outcome::Pass => {
                pass += 1;
                if r.kind == "ATT" {
                    attr_pass += 1;
                    attr_run += 1;
                } else if r.kind == "REF" {
                    ref_pass += 1;
                    ref_run += 1;
                }
            }
            Outcome::Fail(_) => {
                fail += 1;
                if r.kind == "ATT" {
                    attr_run += 1;
                } else if r.kind == "REF" {
                    ref_run += 1;
                }
            }
            Outcome::Crash(_) => {
                crash += 1;
                if r.kind == "ATT" {
                    attr_run += 1;
                } else if r.kind == "REF" {
                    ref_run += 1;
                }
            }
            Outcome::Skip(reason) => {
                skip += 1;
                *skip_by_reason.entry(reason.label()).or_insert(0) += 1;
            }
        }
    }

    let run = pass + fail + crash;
    let pass_rate_run = if run > 0 {
        (pass as f64 / run as f64) * 100.0
    } else {
        0.0
    };
    let pass_rate_total = if total > 0 {
        (pass as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    // ---- Write per-test report ----
    let report = build_report(&results);
    let report_path = PathBuf::from("wpt-report.txt");
    if let Err(e) = fs::write(&report_path, &report) {
        eprintln!("warning: failed to write {}: {e}", report_path.display());
    }

    // ---- Print SUMMARY (stdout) ----
    println!();
    println!("================ canopy-wpt SUMMARY ================");
    println!("suite:        {}", suite_path.display());
    println!("duration:     {:.2}s", elapsed.as_secs_f64());
    println!("---------------------------------------------------");
    println!("{total:>5} tests FOUND");
    println!("{run:>5} tests RUN");
    println!("{skip:>5} tests SKIPPED");
    println!("---------------------------------------------------");
    println!("{pass:>5} PASSED");
    println!("{fail:>5} FAILED");
    println!("{crash:>5} CRASHED");
    println!("---------------------------------------------------");
    println!("pass rate:    {pass_rate_run:.2}% of run   ({pass_rate_total:.2}% of found)");
    println!("  ATTR:       {attr_pass}/{attr_run} passed");
    println!("  REF:        {ref_pass}/{ref_run} passed");
    println!("---------------------------------------------------");
    println!("skipped, by reason:");
    for (reason, n) in &skip_by_reason {
        println!("  {n:>5}  {reason}");
    }
    println!("---------------------------------------------------");
    println!("per-test results written to {}", report_path.display());
    println!("===================================================");
}

/// Build the `wpt-report.txt` body: one line per test.
fn build_report(results: &[TestResult]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# canopy-wpt per-test results");
    let _ = writeln!(out, "# STATUS KIND NAME [detail]");
    for r in results {
        match &r.outcome {
            Outcome::Pass => {
                let _ = writeln!(out, "PASS  {} {}", r.kind, r.name);
            }
            Outcome::Fail(msg) => {
                let _ = writeln!(out, "FAIL  {} {} :: {}", r.kind, r.name, msg);
            }
            Outcome::Crash(msg) => {
                let _ = writeln!(out, "CRASH {} {} :: {}", r.kind, r.name, msg);
            }
            Outcome::Skip(reason) => {
                let _ = writeln!(out, "SKIP  {} {} :: {}", r.kind, r.name, reason.label());
            }
        }
    }
    out
}
