//! The `canopy` developer command.
//!
//! A small, dependency-light std binary that scaffolds and builds Canopy apps:
//!
//! - `canopy new <name>` — scaffold a new app project under `./<name>/`.
//! - `canopy build [--release]` — shell out to `cargo build`, passing `--release`.
//! - `canopy --help` / no args — print usage.
//!
//! Arg parsing is hand-rolled (no `clap`) to keep the dependency surface empty; the
//! subcommand surface is tiny and stable, so a few `match` arms read more honestly
//! than a derive macro here. The scaffolding logic lives in [`scaffold`] as pure
//! functions over a target directory so it is unit-testable without spawning a
//! process or touching the user's cwd.

mod build;
mod scaffold;
mod usage;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Skip argv[0] (the program name); everything else is ours to interpret.
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
}

/// Dispatch on the parsed argument list and map the outcome to a process exit code.
///
/// Split out from [`main`] so tests can drive the top-level dispatch with an explicit
/// argument vector instead of the real process args.
fn run(args: &[String]) -> ExitCode {
    let first = args.first().map(String::as_str);
    match first {
        // No subcommand, or an explicit help flag: print usage and succeed.
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{}", usage::USAGE);
            ExitCode::SUCCESS
        }
        Some("new") => match scaffold::cmd_new(&args[1..]) {
            Ok(path) => {
                println!("Created Canopy app at {}", path.display());
                println!("  cd {} && canopy build", path.display());
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
        Some("build") => match build::cmd_build(&args[1..]) {
            Ok(code) if code.success() => ExitCode::SUCCESS,
            Ok(_) => ExitCode::FAILURE,
            Err(err) => {
                eprintln!("error: {err}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("error: unknown command `{other}`\n");
            eprint!("{}", usage::USAGE);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_no_args_prints_usage_and_succeeds() {
        // `ExitCode` is opaque (no `PartialEq`), so we assert via the debug shape that
        // it is the success variant — enough to catch a regression to FAILURE.
        let code = run(&[]);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn run_with_help_flag_succeeds() {
        let code = run(&["--help".to_string()]);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    }

    #[test]
    fn run_with_unknown_command_fails() {
        let code = run(&["frobnicate".to_string()]);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }
}
