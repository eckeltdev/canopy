//! `canopy new <name>` — scaffold a new Canopy app project.
//!
//! Generation is deliberately split into two layers:
//!
//! - [`cmd_new`] parses the subcommand's args, resolves the target directory against
//!   the current working directory, and is what `main` calls.
//! - [`scaffold`] does the actual file writing into an explicit directory and is pure
//!   with respect to the process cwd, so tests drive it against a temp dir.
//!
//! What we emit: a project `Cargo.toml`, a `src/main.rs` with a minimal counter-app
//! skeleton (an [`App`], a counter signal, a button that increments it, all mounted
//! under the host root), and a `README.md`. We refuse to clobber a non-empty existing
//! directory.
//!
//! [`App`]: https://docs.rs/canopy-view

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that, when set, makes the generated `Cargo.toml` use **path**
/// dependencies on the Canopy crates rooted at its value (an absolute path to a
/// checkout's `crates/` directory's parent, i.e. the workspace root). When unset, the
/// generated manifest uses version placeholders with an explanatory comment so the
/// project is honest about needing the dependency wired up.
const CRATES_ROOT_ENV: &str = "CANOPY_CRATES_PATH";

/// Parse the `new` subcommand args, resolve `<name>` against the cwd, and scaffold.
///
/// Returns the path to the created project directory on success. Errors if no name was
/// given, if an unknown option was passed, or if the target directory already exists
/// and is non-empty.
pub fn cmd_new(args: &[String]) -> io::Result<PathBuf> {
    let mut name: Option<&str> = None;
    for arg in args {
        if arg.starts_with('-') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown option `{arg}` for `canopy new`"),
            ));
        }
        if name.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected a single project name; got more than one argument",
            ));
        }
        name = Some(arg);
    }
    let name = name.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing project name (usage: canopy new <name>)",
        )
    })?;

    validate_name(name)?;

    let target = std::env::current_dir()?.join(name);
    let crates_root = std::env::var_os(CRATES_ROOT_ENV).map(PathBuf::from);
    scaffold(&target, name, crates_root.as_deref())?;
    Ok(target)
}

/// A package name must be non-empty and free of path separators so `./<name>/` is a
/// single directory and the value is a legal cargo package name.
fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project name must not be empty",
        ));
    }
    if name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid project name `{name}`: must be a plain directory name"),
        ));
    }
    Ok(())
}

/// Write the project skeleton into `dir`, naming the package `name`.
///
/// If `crates_root` is `Some`, the generated `Cargo.toml` depends on the Canopy crates
/// by path under that root; otherwise it uses version placeholders with a comment.
///
/// Refuses to write into a directory that already exists and is non-empty, returning an
/// [`io::ErrorKind::AlreadyExists`] error. Creates the directory (and `src/`) otherwise.
pub fn scaffold(dir: &Path, name: &str, crates_root: Option<&Path>) -> io::Result<()> {
    ensure_empty_dir(dir)?;
    fs::create_dir_all(dir.join("src"))?;

    fs::write(dir.join("Cargo.toml"), cargo_toml(name, crates_root))?;
    fs::write(dir.join("src").join("main.rs"), main_rs(name))?;
    fs::write(dir.join("README.md"), readme(name))?;
    Ok(())
}

/// Ensure `dir` is safe to scaffold into: it must either not exist, or exist and be an
/// empty directory. A non-empty directory (or a path that exists but is a file) is an
/// error so we never clobber the user's work.
fn ensure_empty_dir(dir: &Path) -> io::Result<()> {
    match fs::read_dir(dir) {
        // Exists and is a directory: it must be empty.
        Ok(mut entries) => {
            if entries.next().is_some() {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("`{}` already exists and is not empty", dir.display()),
                ))
            } else {
                Ok(())
            }
        }
        // Does not exist yet: fine, we'll create it.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        // Exists but isn't a directory (e.g. a regular file), or a real I/O error.
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("`{}` exists and is not a directory", dir.display()),
        )),
        Err(e) => Err(e),
    }
}

/// Render the project `Cargo.toml`.
fn cargo_toml(name: &str, crates_root: Option<&Path>) -> String {
    let deps = match crates_root {
        Some(root) => {
            // Path deps relative to the provided checkout root.
            let root = root.display();
            format!(
                "canopy-view = {{ path = \"{root}/crates/canopy-view\" }}\n\
                 canopy-protocol = {{ path = \"{root}/crates/canopy-protocol\" }}\n\
                 canopy-paint = {{ path = \"{root}/crates/canopy-paint\" }}\n"
            )
        }
        None => {
            // Version placeholders. Canopy is pre-release (0.0.0) and unpublished, so
            // these are placeholders the developer points at a real source — a path to
            // a local checkout or a git dependency — before building.
            "# Canopy is not yet published to crates.io. Point these at a local checkout\n\
             # (e.g. `canopy-view = { path = \"../canopy/crates/canopy-view\" }`) or a git\n\
             # dependency before building. Re-run `canopy new` with the CANOPY_CRATES_PATH\n\
             # environment variable set to a checkout root to generate path deps for you.\n\
             canopy-view = \"0.0.0\"\n\
             canopy-protocol = \"0.0.0\"\n\
             canopy-paint = \"0.0.0\"\n"
                .to_string()
        }
    };

    format!(
        "[package]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         \n\
         [dependencies]\n\
         {deps}"
    )
}

/// Render the counter-app `src/main.rs` skeleton.
///
/// The body is a `const`-friendly raw string with one `{name}` interpolation in the
/// doc header, so we build it by concatenation to avoid escaping every brace in the
/// Rust source.
fn main_rs(name: &str) -> String {
    let header = format!(
        "//! `{name}` — a minimal Canopy counter app.\n\
         //!\n\
         //! Scaffolded by `canopy new`. This builds a reactive [`App`], a counter\n\
         //! signal, and a button that increments it; a bound text node shows the\n\
         //! current count and re-renders via a single targeted op on each click. The\n\
         //! whole subtree is mounted under the host root (`NodeId::new(0)`).\n\
         //!\n\
         //! `App::take_batch` drains the op-stream the host applies; in a real program\n\
         //! you would hand those bytes to a Canopy host/renderer. Here `main` builds\n\
         //! the tree, simulates a click, and prints how many ops were produced so the\n\
         //! skeleton runs end-to-end out of the box.\n\n"
    );

    let body = r##"use canopy_protocol::{EventPayload, NodeId};
use canopy_view::{App, COLUMN};

/// The host root every Canopy subtree mounts under.
const ROOT: NodeId = NodeId::new(0);

fn main() {
    let app = App::new();
    let rt = app.runtime();

    // A counter signal. Reading it inside `bind_text` subscribes the binding, so a
    // later `set` re-runs only that binding and emits one targeted `SetText`.
    let count = rt.signal(0i64);

    // A column to hold the label and the button.
    let col = app.el(COLUMN);
    app.mount(ROOT, col);

    // A text node bound to the counter: it shows "Count: N" and updates on each change.
    let label = app.label("");
    app.mount(col, label);
    {
        let count = count.clone();
        app.bind_text(label, move || format!("Count: {}", count.get()));
    }

    // A button that increments the counter when clicked. `App::button` builds the
    // BUTTON element with its text child and returns the button node to mount.
    let button = app.button("Increment");
    app.mount(col, button);
    let handler = {
        let count = count.clone();
        app.on_click(button, move |_payload| {
            count.set(count.get() + 1);
        })
    };

    // --- Demo drive ------------------------------------------------------------
    // A real host would deliver events from the platform and stream `take_batch`
    // bytes to a renderer. We simulate one click and report the op count so the
    // skeleton does something visible when you `canopy build && cargo run`.
    let ops_before = app.take_batch(0).len();
    println!("initial mount: {ops_before} bytes of ops");

    // Pretend the host dispatched a click on our button.
    app.dispatch(handler, EventPayload::None);
    let ops_after = app.take_batch(1).len();
    println!("after one click: {ops_after} bytes of ops (count is now 1)");
}
"##;

    format!("{header}{body}")
}

/// Render the project `README.md`.
fn readme(name: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         A [Canopy](https://github.com/iivillian/canopy) app, scaffolded by `canopy new`.\n\
         \n\
         Canopy is a JS-runtime-free, web-like native UI runtime. This skeleton builds a\n\
         reactive `App` with a counter signal and an increment button, mounted under the\n\
         host root.\n\
         \n\
         ## Build\n\
         \n\
         ```sh\n\
         canopy build            # debug build\n\
         canopy build --release  # optimized build\n\
         ```\n\
         \n\
         `canopy build` wraps `cargo build`, so `cargo run` works too.\n\
         \n\
         ## Where to go next\n\
         \n\
         - `src/main.rs` builds the UI tree and a counter signal; edit `bind_text` and\n\
           `on_click` to grow the app.\n\
         - The dependencies in `Cargo.toml` are placeholders until you point them at a\n\
           Canopy checkout (path dep) or a published version. Re-running\n\
           `CANOPY_CRATES_PATH=/path/to/canopy canopy new {name}` generates path deps.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A unique temp directory under the OS temp dir, never reused within a process.
    fn unique_temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "canopy-cli-test-{tag}-{}-{nanos}-{n}",
            std::process::id()
        ))
    }

    /// RAII guard that removes a temp dir tree on drop, even if the test panics.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scaffold_creates_expected_files_with_sane_contents() {
        let base = unique_temp_dir("new");
        let _guard = TempDir(base.clone());
        let project = base.join("my-app");

        scaffold(&project, "my-app", None).expect("scaffold should succeed into a fresh dir");

        // Cargo.toml + src/main.rs + README.md exist.
        let cargo = project.join("Cargo.toml");
        let main = project.join("src").join("main.rs");
        let readme = project.join("README.md");
        assert!(cargo.is_file(), "Cargo.toml should be created");
        assert!(main.is_file(), "src/main.rs should be created");
        assert!(readme.is_file(), "README.md should be created");

        // Cargo.toml names the package and pulls in canopy-view.
        let cargo_txt = fs::read_to_string(&cargo).unwrap();
        assert!(
            cargo_txt.contains("name = \"my-app\""),
            "package name present"
        );
        assert!(cargo_txt.contains("canopy-view"), "depends on canopy-view");

        // main.rs mentions App and a signal — i.e. it is the reactive skeleton.
        let main_txt = fs::read_to_string(&main).unwrap();
        assert!(main_txt.contains("App"), "main.rs references App");
        assert!(main_txt.contains("signal"), "main.rs creates a signal");
        assert!(
            main_txt.contains("on_click"),
            "main.rs wires a button click"
        );
        assert!(main_txt.contains("ROOT"), "main.rs mounts under the root");
    }

    #[test]
    fn scaffold_with_crates_root_emits_path_deps() {
        let base = unique_temp_dir("paths");
        let _guard = TempDir(base.clone());
        let project = base.join("pathy");

        let root = Path::new("/tmp/canopy-checkout");
        scaffold(&project, "pathy", Some(root)).unwrap();

        let cargo_txt = fs::read_to_string(project.join("Cargo.toml")).unwrap();
        assert!(
            cargo_txt.contains("path = \"/tmp/canopy-checkout/crates/canopy-view\""),
            "uses a path dep rooted at the provided checkout, got:\n{cargo_txt}"
        );
    }

    #[test]
    fn scaffold_refuses_non_empty_dir() {
        let base = unique_temp_dir("nonempty");
        let _guard = TempDir(base.clone());
        let project = base.join("occupied");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("KEEP"), b"do not clobber me").unwrap();

        let err = scaffold(&project, "occupied", None)
            .expect_err("scaffolding into a non-empty dir must error");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);

        // The pre-existing file is untouched.
        assert_eq!(
            fs::read_to_string(project.join("KEEP")).unwrap(),
            "do not clobber me"
        );
    }

    #[test]
    fn scaffold_into_empty_existing_dir_is_allowed() {
        let base = unique_temp_dir("emptyexisting");
        let _guard = TempDir(base.clone());
        let project = base.join("blank");
        fs::create_dir_all(&project).unwrap();

        scaffold(&project, "blank", None).expect("an empty existing dir is fine");
        assert!(project.join("Cargo.toml").is_file());
    }

    #[test]
    fn validate_name_rejects_path_like_names() {
        assert!(validate_name("ok-name").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
    }
}
