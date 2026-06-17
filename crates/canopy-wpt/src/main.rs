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
//! Honesty over green: only features the engine genuinely cannot model are
//! feature-gated into SKIP buckets (see [`SkipReason`]) — exactly the spirit of
//! Blitz's feature flags. Today that is `writing-mode` / vertical flow, which
//! Taffy 0.11 has no property for (it cannot swap the inline/block axes — see
//! [`detect_skip`]). Text *is* measured (via the Ahem font in
//! `canopy-style-stylo`'s `text_measure`), `position:absolute` lays out via
//! Taffy's abspos engine, and the `min-/max-/fit-content` inline-sizing keyword is
//! honored for text leaves — so all three are now RUN, not skipped. The value is a
//! working harness + an accurate baseline with a clear, honest skip taxonomy.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::{env, fs};

mod expectations;
mod test_runners;

use expectations::{Baseline, Status};
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
    /// Uses `writing-mode` / vertical flow — Taffy 0.11 has no writing-mode
    /// property, so the inline/block axes cannot be swapped (see [`detect_skip`]).
    WritingMode,
    /// Uses script beyond a single `checkLayout(...)` call (dynamic DOM/JS).
    Script,
    /// A multi-call `checkLayout` test (we only handle a single selector).
    MultiCheckLayout,
    /// A REF test whose reference file could not be read (missing/unresolvable).
    /// This is an INFRASTRUCTURE skip, NOT an engine result: the engine never got
    /// to render-compare, so counting it as a Fail would dishonestly depress the
    /// REF accuracy denominator. The css-flexbox reference files live under
    /// `css/reference`, which CI now sparse-checks out (see stylo.yml); once that
    /// path is present these become real PASS/FAIL render-compares instead of
    /// infra-skips. NOTE: the committed baselines are regenerated in CI via the
    /// `--write-baseline` flag AFTER the css/reference fetch — do not regenerate
    /// them in a checkout that lacks the reference tree (it would bake the
    /// infra-skips into the baseline).
    RefFileUnreadable,
}
// NOTE: the former `AbsolutePosition` and `TextDependent` skip buckets were
// RETIRED: `position:absolute` now lays out via Taffy, and the
// `min-/max-/fit-content` keyword is honored for text-leaf inline sizing. Both
// categories are RUN now (they pass/fail on their merits) rather than skipped.

impl SkipReason {
    fn label(self) -> &'static str {
        match self {
            SkipReason::NoSupportedAssertion => "no supported assertion",
            SkipReason::Blocked => "blocked (crash/hang list)",
            SkipReason::WritingMode => "writing-mode / vertical flow",
            SkipReason::Script => "script beyond checkLayout",
            SkipReason::MultiCheckLayout => "multiple checkLayout() calls",
            SkipReason::RefFileUnreadable => "INFRA: reference file unreadable",
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

impl Outcome {
    /// Project this outcome onto the coarse [`Status`] used by the expectation
    /// baseline (the per-test PASS/FAIL/SKIP/CRASH bucket, dropping detail text).
    fn status(&self) -> Status {
        match self {
            Outcome::Pass => Status::Pass,
            Outcome::Fail(_) => Status::Fail,
            Outcome::Skip(_) => Status::Skip,
            Outcome::Crash(_) => Status::Crash,
        }
    }
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

    // writing-mode / vertical flow — STILL SKIPPED, and deliberately so.
    //
    // Taffy 0.11 has NO `writing_mode` style property: `taffy::Style` only carries
    // `direction` (LTR/RTL bidi on the inline axis), never a block-axis rotation.
    // Taffy's own source confirms this is unimplemented — `geometry.rs` hard-codes
    // `AbstractAxis::is_horizontal()` to always-true with the comment "will change
    // if Taffy ever implements the writing_mode property", and the flexbox engine
    // carries several "TODO if/when vertical writing modes are supported" markers.
    // The `WritingMode` enum that exists only lives in Taffy's internal test
    // harness, not the public API. So a `vertical-rl`/`vertical-lr` document cannot
    // have its inline/block axes swapped: forcing a mapping would silently lay the
    // content out HORIZONTALLY and report wrong geometry (a false pass or a noisy
    // false fail). Honesty over green — we leave these skipped until the layout
    // engine gains real writing-mode support.
    if lower.contains("writing-mode")
        || lower.contains("vertical-rl")
        || lower.contains("vertical-lr")
    {
        return Some(SkipReason::WritingMode);
    }

    // Absolute positioning is now RUN, not skipped. `taffy_convert` maps
    // `position` (absolute/relative/static) and `inset` (top/left/right/bottom),
    // and Taffy DOES implement abspos layout (`perform_absolute_layout_on_absolute_children`
    // in both its block and flexbox engines). Verified by the
    // `layout_absolute_position_top_left` unit test in canopy-style-stylo: a child
    // `position:absolute; top:10px; left:20px; width:30px; height:30px` lands at
    // (20,10) sized 30x30 against its parent's padding box. NOTE: Taffy resolves
    // abspos against the DIRECT parent's padding box, not the nearest *positioned*
    // ancestor (a known Taffy limitation, shared with Blitz), so deeply-nested
    // containing-block tests may still FAIL on their merits — but they FAIL
    // honestly now rather than hiding in a skip bucket.

    // INTRINSIC-content sizing is now RUN for the common case. The CSS keywords
    // `min-content` / `max-content` / `fit-content` on the inline (width) axis of a
    // TEXT leaf are honored: canopy-style-stylo's layout pass pre-resolves the
    // content width via the Ahem text measure-fn (max-content = single unwrapped
    // line; min-content = widest word) and pins it as a fixed length, because
    // Taffy 0.11 has no intrinsic-keyword `Dimension` and would otherwise stretch
    // the auto-width block child to fill its container. Verified by the
    // `layout_max_content_sizes_to_single_line` / `layout_min_content_sizes_to_widest_word`
    // unit tests. We no longer blanket-skip these: tests that use the keyword in a
    // way we don't model yet (on `height`, in grid track sizing, on a non-text
    // box, …) simply FAIL on their merits — which can only ADD passes/fails, never
    // a regression — instead of being hidden.

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
            Ok(RefOutcome::Skip(_msg)) => {
                // The reference file was missing/unreadable, so the engine never
                // got to render-compare. That is an INFRASTRUCTURE skip, NOT an
                // engine Fail: classifying it as Fail would dishonestly depress the
                // REF accuracy denominator (a missing reference is a fetch/setup
                // gap, not a rendering bug). The css-flexbox reference tree lives
                // under `css/reference`, which CI sparse-checks out (see
                // stylo.yml); with it present these run as real PASS/FAIL compares.
                Outcome::Skip(SkipReason::RefFileUnreadable)
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

/// Parsed command-line options.
///
/// Usage:
///   canopy-wpt [SUITE_DIR] [--check BASELINE] [--write-baseline PATH]
///
/// * `--write-baseline PATH` — run the suite, then write a fresh expectation
///   baseline (per-test PASS/FAIL/SKIP/CRASH) to `PATH`. Used to (re)generate the
///   committed `expectations/<suite>.txt` files.
/// * `--check BASELINE` — run the suite, then compare against `BASELINE` and exit
///   non-zero if any test REGRESSED (was PASS, now not PASS). This is the CI gate.
struct Opts {
    suite_dir: Option<String>,
    check: Option<String>,
    write_baseline: Option<String>,
}

/// Parse argv into [`Opts`]. Flags accept either `--flag VALUE` or `--flag=VALUE`.
/// On a malformed flag, prints usage and exits 2.
fn parse_opts(args: &[String]) -> Opts {
    let mut opts = Opts {
        suite_dir: None,
        check: None,
        write_baseline: None,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // Helper: pull the value for `--flag`, supporting both `--flag=v` and
        // `--flag v`. Advances `i` past the consumed value when separate.
        let mut take_value = |inline: Option<&str>| -> String {
            if let Some(v) = inline {
                v.to_string()
            } else if i + 1 < args.len() {
                i += 1;
                args[i].clone()
            } else {
                eprintln!("error: {arg} requires a value");
                std::process::exit(2);
            }
        };
        if let Some(rest) = arg.strip_prefix("--check") {
            let inline = rest.strip_prefix('=');
            opts.check = Some(take_value(inline));
        } else if let Some(rest) = arg.strip_prefix("--write-baseline") {
            let inline = rest.strip_prefix('=');
            opts.write_baseline = Some(take_value(inline));
        } else if arg == "-h" || arg == "--help" {
            println!("usage: canopy-wpt [SUITE_DIR] [--check BASELINE] [--write-baseline PATH]");
            std::process::exit(0);
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag {arg}");
            std::process::exit(2);
        } else if opts.suite_dir.is_none() {
            opts.suite_dir = Some(arg.clone());
        } else {
            eprintln!("error: unexpected positional argument {arg}");
            std::process::exit(2);
        }
        i += 1;
    }
    opts
}

/// Derive a short suite name (e.g. `css-grid`) from the suite directory, for the
/// baseline header. Falls back to the full path's last component.
fn suite_name(suite_path: &Path) -> String {
    suite_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| suite_path.to_string_lossy().into_owned())
}

fn main() {
    // Args / env: suite dir (positional, default css-flexbox) + WPT root + flags.
    let args: Vec<String> = env::args().skip(1).collect();
    let opts = parse_opts(&args);
    let wpt_root = env::var("WPT_DIR").unwrap_or_else(|_| "/tmp/wpt".to_string());
    let suite_dir = opts
        .suite_dir
        .clone()
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

    // ---- Expectation tracking (--write-baseline / --check) ----
    // The coarse per-test status list shared by both modes.
    let current: Vec<(String, Status)> = results
        .iter()
        .map(|r| (r.name.clone(), r.outcome.status()))
        .collect();
    let suite = suite_name(&suite_path);

    if let Some(path) = &opts.write_baseline {
        let baseline = Baseline::from_results(&suite, current.iter().cloned());
        let path = PathBuf::from(path);
        match baseline.write(&path) {
            Ok(()) => println!(
                "wrote baseline ({} tests) to {}",
                current.len(),
                path.display()
            ),
            Err(e) => {
                eprintln!("error: failed to write baseline {}: {e}", path.display());
                std::process::exit(1);
            }
        }
    }

    if let Some(path) = &opts.check {
        let path = PathBuf::from(path);
        let baseline = match Baseline::load(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: failed to read baseline {}: {e}", path.display());
                std::process::exit(2);
            }
        };
        let report = expectations::check(&baseline, &current);

        println!();
        println!("================ --check vs baseline ===============");
        println!(
            "baseline:     {} ({} tests)",
            path.display(),
            baseline.statuses.len()
        );
        if !report.improvements.is_empty() {
            println!(
                "improvements: {} test(s) now PASS that were not PASS in the baseline",
                report.improvements.len()
            );
            for imp in report.improvements.iter().take(10) {
                let was = imp.was.map(|s| s.token()).unwrap_or("NEW");
                println!("  + {} ({was} -> {})", imp.name, imp.now.token());
            }
            if report.improvements.len() > 10 {
                println!("  + (+{} more)", report.improvements.len() - 10);
            }
            println!("  (refresh the committed baseline with --write-baseline to lock these in)");
        }
        if !report.missing.is_empty() {
            println!(
                "missing:      {} baseline test(s) not present in this run",
                report.missing.len()
            );
        }
        if report.has_regressions() {
            println!("---------------------------------------------------");
            println!(
                "REGRESSIONS:  {} test(s) were PASS, now FAIL/CRASH/SKIP",
                report.regressions.len()
            );
            for reg in &report.regressions {
                println!(
                    "  - {} ({} -> {})",
                    reg.name,
                    reg.was.token(),
                    reg.now.token()
                );
            }
            println!("===================================================");
            eprintln!(
                "canopy-wpt --check FAILED: {} regression(s) against {}",
                report.regressions.len(),
                path.display()
            );
            std::process::exit(1);
        }
        println!("no regressions. OK.");
        println!("===================================================");
    }
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
