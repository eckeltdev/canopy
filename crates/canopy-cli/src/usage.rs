//! The `canopy --help` / usage text, kept in one place so every error path can print
//! the same thing.

/// The full usage string printed for `--help`, no args, and unknown-command errors.
pub const USAGE: &str = "\
canopy — developer command for the Canopy native UI runtime

USAGE:
    canopy <COMMAND> [OPTIONS]

COMMANDS:
    new <name>        Scaffold a new Canopy app project in ./<name>/
    build [--release] Build the current project (wraps `cargo build`)
    help              Print this message

OPTIONS:
    -h, --help        Print this message

EXAMPLES:
    canopy new my-app       # create ./my-app with a counter skeleton
    cd my-app
    canopy build --release  # cargo build --release
";
