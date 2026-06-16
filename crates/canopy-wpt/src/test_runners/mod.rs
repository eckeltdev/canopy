//! The two WPT test-kind runners, ported from Blitz's `test_runners/`.
//!
//! * [`run_attr_test`] — the `checkLayout` / `data-expected-*` path
//!   (`attr_test.rs`).
//! * [`run_ref_test`] — the `<link rel=match>` render-and-compare path
//!   (`ref_test.rs`).

mod attr_test;
mod ref_test;

pub use attr_test::{run_attr_test, AttrOutcome};
pub use ref_test::{run_ref_test, RefOutcome};
