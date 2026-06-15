//! `canopy build` — a thin wrapper over `cargo build`.
//!
//! The only flag we interpret is `--release`, which we forward verbatim; any other
//! arguments are rejected so a typo (`--relese`) fails loudly here rather than being
//! silently swallowed by cargo. The child inherits our stdio so cargo's progress and
//! diagnostics stream straight through to the user's terminal.

use std::io;
use std::process::{Command, ExitStatus};

/// Run `cargo build`, forwarding `--release` when requested, and return the child's
/// [`ExitStatus`].
///
/// Returns an [`io::Error`] if `cargo` could not be spawned (e.g. not on `PATH`) or if
/// an unrecognized flag was passed. A non-zero build is *not* an error here — it is a
/// successful run with a failing status, which the caller maps to a failing exit code.
pub fn cmd_build(args: &[String]) -> io::Result<ExitStatus> {
    let mut release = false;
    for arg in args {
        match arg.as_str() {
            "--release" | "-r" => release = true,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown option `{other}` for `canopy build` (try --release)"),
                ));
            }
        }
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    // Inherit stdio so cargo's output reaches the user directly.
    cmd.status()
}
