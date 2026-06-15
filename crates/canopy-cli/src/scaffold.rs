//! `canopy new <name>` — scaffold a new Canopy app project.
//!
//! Generation is deliberately split into two layers:
//!
//! - [`cmd_new`] parses the subcommand's args, resolves the target directory against
//!   the current working directory, and is what `main` calls.
//! - [`scaffold`] does the actual file writing into an explicit directory and is pure
//!   with respect to the process cwd, so tests drive it against a temp dir.
//!
//! What we emit: a project `Cargo.toml`, a `src/main.rs` holding a **windowed welcome
//! starter** (the Canopy answer to `npm create vite` — a centered logo, heading,
//! tagline, a click-to-increment counter card, and footer links), an editable
//! `styles.css`, and a `README.md`. We refuse to clobber a non-empty existing
//! directory.
//!
//! The welcome app is authored exactly like the in-repo `canopy-welcome` example: one
//! JSX [`rsx!`] tree over the batteries-included [`canopy_ui::Ui`] context, plus a thin
//! winit + softbuffer host (gated behind a default-on `window` feature) whose
//! click/hover logic is one-liners on `Ui` and which **hot-reloads** `styles.css` — a
//! `canopy-hotreload` watcher restyles the live window on save, no restart. The UI logic
//! depends on no windowing or hot-reload crate, so the scaffold-then-`cargo check
//! --no-default-features` test stays fast (and `canopy-hotreload` is dropped) while
//! `cargo run` still opens a real, live-reloading window. See [`main_rs`] for the
//! structure.
//!
//! [`Ui`]: https://docs.rs/canopy-ui
//! [`rsx!`]: https://docs.rs/canopy-ui

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Environment variable that, when set, makes the generated `Cargo.toml` use **path**
/// dependencies on the Canopy crates rooted at its value (an absolute path to a
/// checkout root, i.e. the directory holding `crates/`). When unset, the generated
/// manifest uses version placeholders with an explanatory comment so the project is
/// honest about needing the dependency wired up.
const CRATES_ROOT_ENV: &str = "CANOPY_CRATES_PATH";

/// The always-on Canopy crates the welcome starter depends on, in
/// dependency-graph-ish order.
///
/// This is the single source of truth for the *unconditional* dependency list:
/// [`cargo_toml`] turns it into either path deps (rooted at a checkout) or version
/// placeholders, so the path-vs-version split stays in lockstep for every crate.
/// `canopy-ui` is the one authoring crate the UI code touches (it re-exports the
/// view/signals/style/dom/layout core); the rest are the render-side crates the
/// windowed binary paints with. These all compile under `--no-default-features`, so
/// they are never feature-gated.
///
/// `winit`/`softbuffer` and the dev-only [`CANOPY_HOTRELOAD_DEP`] are emitted separately
/// as **optional** `window`-feature deps.
const CANOPY_DEPS: &[&str] = &[
    // The batteries-included authoring layer: the `Ui` context + the `rsx!` macro.
    "canopy-ui",
    // Render-side types + the sharp-text renderer the windowed binary draws with.
    "canopy-traits",
    "canopy-render-soft",
    "canopy-render-text",
    "canopy-text-parley",
];

/// The dev-time hot-reload crate, emitted as an **optional** dependency the `window`
/// feature activates.
///
/// It is a `std`/`notify` crate that only the windowed `main` uses to watch
/// `styles.css` and restyle the live window. Keeping it optional (and out of
/// [`CANOPY_DEPS`]) is what lets `cargo check --no-default-features` skip it entirely —
/// the reactive `welcome` module never touches it. It still honours the
/// path-vs-version split: under a checkout it is a path dep, otherwise a version
/// placeholder, in both cases tagged `optional = true`.
const CANOPY_HOTRELOAD_DEP: &str = "canopy-hotreload";

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
    fs::write(dir.join("styles.css"), styles_css())?;
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

/// Render one Canopy dependency line, honoring the path-vs-version split and an
/// `optional` flag.
///
/// Under a checkout this is a path dep rooted at `crates_root`; otherwise a `0.0.0`
/// version placeholder. When `optional` is set, the line is emitted in inline-table
/// form with `optional = true` so a `window`-feature `dep:` activation can gate it
/// (Cargo requires the *table* form for that). Centralizing this keeps the path-dep
/// behavior identical for the always-on crates and the optional `canopy-hotreload`.
fn dep_line(krate: &str, crates_root: Option<&Path>, optional: bool) -> String {
    match crates_root {
        Some(root) => {
            let path = format!("{}/crates/{krate}", root.display());
            if optional {
                format!("{krate} = {{ path = \"{path}\", optional = true }}\n")
            } else {
                format!("{krate} = {{ path = \"{path}\" }}\n")
            }
        }
        None => {
            if optional {
                format!("{krate} = {{ version = \"0.0.0\", optional = true }}\n")
            } else {
                format!("{krate} = \"0.0.0\"\n")
            }
        }
    }
}

/// Render the project `Cargo.toml`.
///
/// The always-on Canopy dependency block is generated from [`CANOPY_DEPS`] so the
/// path-vs-version split is identical for every crate. `winit`/`softbuffer` and the
/// dev-only [`CANOPY_HOTRELOAD_DEP`] are appended as **optional** deps activated by the
/// default-on `window` feature: that lets `cargo check --no-default-features` skip the
/// whole windowing + hot-reload stack (keeping the scaffold test fast and the reactive
/// UI display-free) while `cargo run` still opens a live, hot-reloading window by
/// default.
fn cargo_toml(name: &str, crates_root: Option<&Path>) -> String {
    // The always-on Canopy crates: path deps under a checkout, else version
    // placeholders. The optional `canopy-hotreload` line is rendered separately below.
    let canopy_deps: String = match crates_root {
        Some(_) => CANOPY_DEPS
            .iter()
            .map(|krate| dep_line(krate, crates_root, false))
            .collect(),
        None => {
            // Version placeholders. Canopy is pre-release (0.0.0) and unpublished, so
            // these are placeholders the developer points at a real source — a path to a
            // local checkout or a git dependency — before building.
            let mut s = String::from(
                "# Canopy is not yet published to crates.io. Point these at a local checkout\n\
                 # (e.g. `canopy-ui = { path = \"../canopy/crates/canopy-ui\" }`) or a git\n\
                 # dependency before building. Re-run `canopy new` with the CANOPY_CRATES_PATH\n\
                 # environment variable set to a checkout root to generate path deps for you.\n",
            );
            for krate in CANOPY_DEPS {
                s.push_str(&dep_line(krate, crates_root, false));
            }
            s
        }
    };

    // `canopy-hotreload`, optional and gated behind `window` (it is std/notify and only
    // the windowed `main` watches `styles.css`). Same path-vs-version split as above.
    let hotreload_dep = dep_line(CANOPY_HOTRELOAD_DEP, crates_root, true);

    format!(
        "[package]\n\
         name = \"{name}\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         # `cargo run` opens the windowed welcome screen (the `window` feature is on by\n\
         # default) and live-reloads `styles.css` on save. Build the UI logic alone — no\n\
         # winit/softbuffer/hot-reload — with `cargo check --no-default-features`.\n\
         default-run = \"{name}\"\n\
         \n\
         [dependencies]\n\
         {canopy_deps}\
         \n\
         # The windowing backend plus dev-time hot reload. All optional so the reactive UI\n\
         # (the `welcome` module) type-checks without a display: `--no-default-features`\n\
         # drops them entirely. `canopy-hotreload` watches `styles.css` and restyles the\n\
         # live window on save; only the windowed `main` uses it.\n\
         winit = {{ version = \"0.30\", optional = true }}\n\
         softbuffer = {{ version = \"0.4\", optional = true }}\n\
         {hotreload_dep}\
         \n\
         [features]\n\
         default = [\"window\"]\n\
         # Pull in winit + softbuffer + hot reload and compile the windowed `main`.\n\
         window = [\"dep:winit\", \"dep:softbuffer\", \"dep:canopy-hotreload\"]\n\
         \n\
         # Pin the workspace boundary here. A scaffolded app is standalone, but if it is\n\
         # created *inside* another Cargo workspace's tree, this empty table stops Cargo\n\
         # from walking up and binding the app to a workspace it isn't a member of.\n\
         [workspace]\n"
    )
}

/// Render the welcome-starter `src/main.rs`.
///
/// The doc header interpolates `{name}`; the body is assembled from raw-string `const`
/// fragments so we do not have to escape every brace in real Rust source. The body is
/// structured as the in-repo example is:
///
/// - the `welcome` module's `build()` assembles the UI as one `rsx!` tree on the
///   [`canopy_ui::Ui`] context and depends on no windowing crate, so it (and the whole
///   crate) type-checks under `--no-default-features`.
/// - a `#[cfg(feature = "window")]` `main` opens a winit window, presents frames via
///   softbuffer, drives clicks + the `:hover` cascade through `Ui` one-liners, and
///   hot-reloads `styles.css` via a `canopy-hotreload` watcher (waking the loop through
///   an `EventLoopProxy` + channel and restyling with `Ui::reload_css`).
/// - a fallback `#[cfg(not(feature = "window"))]` `main` builds the same tree headless
///   and prints the op-batch size, so the crate still *runs* without a display.
fn main_rs(name: &str) -> String {
    let header = format!(
        "//! `{name}` — a Canopy welcome starter.\n\
         //!\n\
         //! Scaffolded by `canopy new`, this is Canopy's answer to `npm create vite`: a\n\
         //! centered welcome screen — the Canopy logo (rounded leaf shapes), a heading and\n\
         //! tagline, a click-to-increment counter card, and footer links — written as a\n\
         //! single **JSX** tree with [`canopy_ui`]'s `rsx!` macro. Styling is a real CSS\n\
         //! stylesheet (`styles.css`), layout is Taffy flexbox, and text is sharp\n\
         //! antialiased glyphs — the `Ui` context bundles it all.\n\
         //!\n\
         //! `cargo run` opens a window (winit + softbuffer) and drives clicks and the\n\
         //! `:hover` cascade — both one-liners on `Ui`. It also **hot-reloads**\n\
         //! `styles.css`: edit the stylesheet, save, and the live window restyles without\n\
         //! a restart (a `canopy-hotreload` watcher feeds reloads back into the loop). The\n\
         //! UI itself lives in the `welcome` module's `build()`, which depends on no\n\
         //! windowing or hot-reload crate, so `cargo check --no-default-features`\n\
         //! type-checks the reactive UI without a display backend, and a headless fallback\n\
         //! `main` still runs (printing the op-batch size) when the `window` feature is\n\
         //! off.\n\n"
    );

    // The body. The `welcome` module is always compiled; the two `main`s are cfg-selected
    // on the `window` feature.
    format!("{header}{WELCOME_MODULE}\n{WINDOW_MAIN}\n{HEADLESS_MAIN}")
}

/// The UI logic, with no dependency on `winit`/`softbuffer`. Always compiled, so the
/// crate type-checks with `--no-default-features`.
const WELCOME_MODULE: &str = r##"/// The welcome UI: one `rsx!` tree on the `Ui` context, free of any windowing crate.
mod welcome {
    use canopy_ui::prelude::*;

    /// Logical window size. A tall, narrow canvas so the centered card reads like a
    /// landing screen.
    pub const VIEW_W: f32 = 560.0;
    /// Logical window height.
    pub const VIEW_H: f32 = 600.0;

    /// The editable stylesheet, located next to the source via `CARGO_MANIFEST_DIR` so
    /// it is found regardless of the working directory `cargo run` was invoked from.
    pub const STYLES_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/styles.css");

    /// Read `styles.css` from disk (empty string if it cannot be read, so the app still
    /// runs). Called once at startup, then again on every hot reload: the windowed
    /// `main` watches `styles.css` with `canopy-hotreload` and re-reads it here to
    /// restyle the live window on save — no restart.
    pub fn load_styles() -> String {
        std::fs::read_to_string(STYLES_PATH).unwrap_or_default()
    }

    /// The built welcome screen: the `Ui` context (app + stylesheet + styled/hover
    /// registries + hit-testing) plus the counter signal.
    pub struct Welcome {
        /// The authoring + host context. Drives batches, hover, clicks.
        pub ui: Ui,
        /// The counter value, bound to the button label.
        pub count: Signal<i64>,
    }

    /// Assemble the welcome UI and return the live `Ui` plus the counter handle. The
    /// whole screen is one `rsx!` expression; the logo is a reusable component spliced
    /// in with `{ logo(&ui) }`.
    pub fn build() -> Welcome {
        let ui = Ui::with_css(&load_styles());
        let count = ui.signal(0i64);

        let root = rsx!(ui =>
            <div class="screen">
                { logo(&ui) }
                <span class="title">"Canopy"</span>
                <span class="tagline">"web-like native UI \u{2014} no JavaScript runtime"</span>
                <div class="card">
                    <button class="counter"
                        on:click={ let c = count.clone(); move |_| c.update(|n| *n += 1) }>
                        { let c = count.clone(); move || format!("count is {}", c.get()) }
                    </button>
                    <span class="hint">"Edit styles.css \u{2014} the live window restyles on save"</span>
                </div>
                <div class="footer">
                    <div class="pill"><span class="pilltext">"docs"</span></div>
                    <div class="pill"><span class="pilltext">"github"</span></div>
                </div>
            </div>
        );
        ui.mount_root(root);

        Welcome { ui, count }
    }

    /// The Canopy logo: rows of rounded "leaf" tiles over a short trunk. A `<div>` is a
    /// flex container whose row/column direction is set by its CSS class (`.leafrow`
    /// carries `direction: row`), exactly like real flexbox. A component is just a
    /// function that builds a subtree on the shared `Ui` and returns its root.
    fn logo(ui: &Ui) -> NodeId {
        rsx!(ui =>
            <div class="logo">
                <div class="leafrow"><div class="leaf-teal"/></div>
                <div class="leafrow"><div class="leaf-green"/><div class="leaf-teal"/></div>
                <div class="leafrow">
                    <div class="leaf-green"/>
                    <div class="leaf-blue"/>
                    <div class="leaf-green"/>
                </div>
                <div class="leafrow"><div class="trunk"/></div>
            </div>
        )
    }
}
"##;

/// The windowed `main` (winit + softbuffer + hot reload). Compiled only with the
/// default `window` feature. Almost all of its app logic is a one-liner on `Ui`, and it
/// **hot-reloads** `styles.css`: a `canopy-hotreload` watcher wakes the loop on save and
/// restyles the live tree without a restart.
const WINDOW_MAIN: &str = r##"#[cfg(feature = "window")]
mod windowed {
    use std::num::NonZeroU32;
    use std::rc::Rc;
    use std::sync::mpsc::{self, Receiver};

    use canopy_hotreload::{reapply, ReloadEvent, Watcher};
    use canopy_text_parley::TextEngine;
    // `OpSink` brings `Dom::apply` into scope (it is the trait that defines it).
    use canopy_traits::{Color, OpSink, Point, Size};
    use canopy_ui::prelude::{Dom, EventPayload, NodeId};

    use winit::application::ApplicationHandler;
    use winit::dpi::{LogicalSize, PhysicalPosition};
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
    use winit::window::{Window, WindowId};

    use crate::welcome::{self, Welcome, STYLES_PATH, VIEW_H, VIEW_W};

    /// The canvas clear color (`#1e1e2e`), matching the `.screen` background so the
    /// window has no seam around the laid-out tree.
    const CLEAR: Color = Color {
        r: 0x1e,
        g: 0x1e,
        b: 0x2e,
        a: 255,
    };

    /// The custom winit user-event: "a `styles.css` reload is pending; drain the
    /// channel." The watcher thread cannot touch the `Dom`, so it sends the
    /// `ReloadEvent` down a channel we own and wakes the loop with this `'static`
    /// marker (an `EventLoopProxy` payload must be `'static`); the marker just says
    /// "go look".
    #[derive(Debug)]
    struct ReloadPending;

    /// The platform glue: a winit `ApplicationHandler` that presents the welcome tree,
    /// routes pointer input back into the reactive app via `Ui`, and restyles the live
    /// tree when `styles.css` is saved.
    struct WelcomeApp {
        welcome: Welcome,
        dom: Dom,
        seq: u32,
        cursor: PhysicalPosition<f64>,
        /// The hoverable node under the cursor, so we only restyle when it changes (and
        /// so a reload can keep a live hover lit).
        hovered: Option<NodeId>,
        /// Held across redraws so the bundled font is loaded/rasterized once.
        text: TextEngine,
        /// One `ReloadEvent` per debounced `styles.css` save from the watcher thread.
        reloads: Receiver<ReloadEvent>,
        /// Kept alive so the OS watch stays armed; dropping it stops hot reload.
        _watcher: Watcher,
        window: Option<Rc<Window>>,
        surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    }

    impl WelcomeApp {
        /// Build the app and arm the `styles.css` watcher. The watcher callback runs on
        /// its own debounce thread, so it cannot touch the `Dom`; it forwards the event
        /// through a channel and wakes the loop via the `EventLoopProxy`.
        fn new(proxy: EventLoopProxy<ReloadPending>) -> Self {
            let welcome = welcome::build();
            // `welcome.count` is the live counter handle (the on:click closure holds its
            // own clone); the screen opens at zero. A host could seed it for a lively
            // start, e.g. `welcome.count.set(3); welcome.ui.runtime().flush();`.
            debug_assert_eq!(welcome.count.get(), 0, "counter starts at zero");
            let mut dom = Dom::new();
            dom.apply(&welcome.ui.take_batch(0)).expect("initial batch");

            let (reload_tx, reloads) = mpsc::channel::<ReloadEvent>();
            let watcher = Watcher::new(STYLES_PATH, move |event| {
                // Off the main thread: forward the event and wake the loop. A failed
                // send means the app is shutting down, so dropping the reload is correct.
                if reload_tx.send(event).is_ok() {
                    let _ = proxy.send_event(ReloadPending);
                }
            })
            .expect("watch styles.css");

            Self {
                welcome,
                dom,
                seq: 1,
                cursor: PhysicalPosition::new(0.0, 0.0),
                hovered: None,
                text: TextEngine::new(),
                reloads,
                _watcher: watcher,
                window: None,
                surface: None,
            }
        }

        /// Drain everything the `Ui` emitted into the retained `Dom` and request a redraw.
        fn apply_and_redraw(&mut self) {
            let batch = self.welcome.ui.take_batch(self.seq);
            self.seq += 1;
            let _ = self.dom.apply(&batch);
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }

        fn viewport(&self) -> Size {
            match &self.window {
                Some(w) => {
                    let s = w.inner_size();
                    Size {
                        w: s.width.max(1) as f32,
                        h: s.height.max(1) as f32,
                    }
                }
                None => Size {
                    w: VIEW_W,
                    h: VIEW_H,
                },
            }
        }

        fn redraw(&mut self) {
            let viewport = self.viewport();
            let (w, h) = (viewport.w as usize, viewport.h as usize);

            // Lay out + rasterize with real antialiased glyphs, reusing the held engine.
            let buf =
                canopy_render_text::render_dom_with(&self.dom, viewport, CLEAR, &mut self.text);

            let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
                return;
            };
            surface
                .resize(
                    NonZeroU32::new(w as u32).unwrap(),
                    NonZeroU32::new(h as u32).unwrap(),
                )
                .expect("resize surface");
            let mut frame = surface.buffer_mut().expect("surface buffer");
            let rgba = buf.data();
            for (dst, src) in frame.iter_mut().zip(rgba.chunks_exact(4)) {
                // softbuffer wants 0RGB packed in a u32.
                *dst = ((src[0] as u32) << 16) | ((src[1] as u32) << 8) | (src[2] as u32);
            }
            window.pre_present_notify();
            frame.present().expect("present");
        }

        fn on_click(&mut self) {
            let point = Point {
                x: self.cursor.x as f32,
                y: self.cursor.y as f32,
            };
            if let Some(handler) = self.welcome.ui.click_handler(&self.dom, self.viewport(), point) {
                self.welcome.ui.dispatch(handler, EventPayload::None);
                self.apply_and_redraw();
            }
        }

        /// Update `:hover` on cursor move: find the hoverable under the cursor and, if it
        /// changed, un-hover the old node and hover the new one, then apply + redraw.
        fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
            self.cursor = position;
            let point = Point {
                x: position.x as f32,
                y: position.y as f32,
            };
            let target = self.welcome.ui.hover_target(&self.dom, self.viewport(), point);
            if target == self.hovered {
                return; // Same node (or still nothing): no style change needed.
            }
            if let Some(old) = self.hovered {
                self.welcome.ui.set_hover(old, false);
            }
            if let Some(new) = target {
                self.welcome.ui.set_hover(new, true);
            }
            self.hovered = target;
            self.apply_and_redraw();
        }

        /// Drain every pending `styles.css` reload and restyle the live tree. Runs on the
        /// main thread when the loop is woken by `ReloadPending`, so the `Dom` stays
        /// single-owner and lock-free. Re-reads the edited sheet, re-resolves every
        /// styled node with `Ui::reload_css` (keeping a live hover lit so it doesn't
        /// flicker back to base), and pushes the restyle batch on with `reapply` — a
        /// malformed reload is rejected at the capability boundary and the old tree kept.
        fn drain_reloads(&mut self) {
            let mut changed = false;
            while let Ok(event) = self.reloads.try_recv() {
                let n = self
                    .welcome
                    .ui
                    .reload_css(&welcome::load_styles(), self.hovered);
                let batch = self.welcome.ui.take_batch(self.seq);
                self.seq += 1;
                match reapply(&mut self.dom, &batch) {
                    Ok(()) => {
                        println!(
                            "hot reload: restyled {n} nodes from {STYLES_PATH} (changed: {:?})",
                            event.path()
                        );
                        changed = true;
                    }
                    Err(e) => eprintln!("hot reload: batch rejected, keeping old styles: {e}"),
                }
            }
            if changed {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
        }
    }

    impl ApplicationHandler<ReloadPending> for WelcomeApp {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let attrs = Window::default_attributes()
                .with_title("Canopy")
                .with_inner_size(LogicalSize::new(VIEW_W, VIEW_H));
            let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
            let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
            let surface =
                softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
            self.window = Some(window);
            self.surface = Some(surface);
        }

        /// The watcher woke us: a `styles.css` save is pending. Drain + restyle.
        fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ReloadPending) {
            self.drain_reloads();
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::RedrawRequested => self.redraw(),
                WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => self.on_click(),
                _ => {}
            }
        }
    }

    /// Open the window and run the event loop until the user closes it.
    ///
    /// Built `with_user_event` so the `styles.css` watcher thread can wake us via an
    /// `EventLoopProxy` (the only thread-safe handle into winit) when the sheet is saved.
    pub fn run() {
        let event_loop = EventLoop::<ReloadPending>::with_user_event()
            .build()
            .expect("event loop");
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        let mut app = WelcomeApp::new(proxy);
        event_loop.run_app(&mut app).expect("run app");
    }
}

#[cfg(feature = "window")]
fn main() {
    windowed::run();
}
"##;

/// The headless fallback `main`, compiled when the `window` feature is *off*. It builds
/// the same tree and prints the op-batch size so the crate still runs without a display
/// (and so `cargo run --no-default-features` does something visible).
const HEADLESS_MAIN: &str = r##"#[cfg(not(feature = "window"))]
fn main() {
    use welcome::{VIEW_H, VIEW_W};

    let w = welcome::build();
    println!(
        "welcome built for a {VIEW_W}x{VIEW_H} screen: {} hoverable node(s)",
        w.ui.hoverables().len(),
    );

    // The initial mount, then one simulated increment so the headless run exercises a
    // signal update + a targeted re-render.
    let mounted = w.ui.take_batch(0).len();
    println!("initial mount: {mounted} bytes of ops");

    w.count.update(|n| *n += 1);
    w.ui.runtime().flush();
    let after = w.ui.take_batch(1).len();
    println!(
        "after one increment: {after} bytes of ops (count is now {})",
        w.count.get()
    );
}
"##;

/// Render the editable `styles.css` — the stylesheet the app reads at startup.
fn styles_css() -> String {
    "/* Canopy welcome screen — Catppuccin-ish palette, rounded corners everywhere.\n\
     The windowed app watches this file: edit it and save, and the live window restyles\n\
     with no restart (hot reload). A `<div>`'s row/column direction is the `direction`\n\
     property here, like real flexbox. */\n\
     \n\
     /* Dark canvas, everything centered. */\n\
     .screen   { background: #1e1e2e; direction: column; padding: 40px; gap: 20px }\n\
     \n\
     /* --- Logo: rounded 'leaf' tiles stacked into a little canopy/tree. --- */\n\
     .logo     { direction: column; gap: 6px }\n\
     .leafrow  { direction: row; gap: 6px }\n\
     .leaf-teal  { background: #94e2d5; width: 26px; height: 26px; border-radius: 9px }\n\
     .leaf-green { background: #a6e3a1; width: 26px; height: 26px; border-radius: 9px }\n\
     .leaf-blue  { background: #89b4fa; width: 26px; height: 26px; border-radius: 9px }\n\
     .trunk    { background: #6c7086; width: 14px; height: 18px; border-radius: 5px }\n\
     \n\
     /* --- Wordmark. --- */\n\
     .title    { color: #cdd6f4; height: 40px }\n\
     .tagline  { color: #9399b2; height: 22px }\n\
     \n\
     /* --- Counter card. The counter's 'count is N' text is the button's default light\n\
        ink; the card and button carry the rounded surfaces. --- */\n\
     .card     { background: #313244; direction: column; padding: 24px; gap: 14px; border-radius: 12px; width: 320px }\n\
     .counter  { background: #45475a; padding: 12px; border-radius: 8px }\n\
     .counter:hover { background: #585b70 }\n\
     .hint     { color: #6c7086; height: 18px }\n\
     \n\
     /* --- Footer pills. --- */\n\
     .footer   { direction: row; gap: 12px }\n\
     .pill     { background: #313244; padding: 10px; border-radius: 8px }\n\
     .pill:hover { background: #585b70 }\n\
     .pilltext { color: #89b4fa; height: 18px }\n"
        .to_string()
}

/// Render the project `README.md`.
fn readme(name: &str) -> String {
    format!(
        "# {name}\n\
         \n\
         A [Canopy](https://github.com/iivillian/canopy) app, scaffolded by `canopy new`.\n\
         \n\
         Canopy is a JS-runtime-free, web-like native UI runtime. This starter is a\n\
         windowed **welcome screen** — the Canopy logo (rounded leaf shapes), a heading\n\
         and tagline, a click-to-increment counter card, and footer links — written as a\n\
         single JSX tree with `canopy-ui`'s `rsx!` macro.\n\
         \n\
         ## Run\n\
         \n\
         ```sh\n\
         cargo run            # open the welcome window (winit + softbuffer)\n\
         cargo run --release  # optimized\n\
         ```\n\
         \n\
         `canopy build` wraps `cargo build`, so it works too. Click the counter button\n\
         and watch it increment; hover the button and footer pills to see them lighten\n\
         (the `:hover` cascade).\n\
         \n\
         **Hot reload:** while the window is open, edit `styles.css` and save — the live\n\
         window restyles instantly, no restart. A `canopy-hotreload` watcher feeds each\n\
         save back into the event loop, which re-reads the sheet and re-resolves every\n\
         styled node.\n\
         \n\
         The reactive UI is independent of the window, so you can type-check it without a\n\
         display backend (this drops winit/softbuffer **and** `canopy-hotreload`):\n\
         \n\
         ```sh\n\
         cargo check --no-default-features   # UI logic only, no window/hot-reload\n\
         ```\n\
         \n\
         ## Edit the UI\n\
         \n\
         - **`src/main.rs`** builds the whole screen as one `rsx!` tree in the `welcome`\n\
           module. Add elements with JSX tags — `<div>` (a flex container; its row/column\n\
           direction comes from CSS), `<span>` (text), `<button>` — style them with\n\
           `class=\"..\"`, make text reactive with a `{{ move || .. }}` child, and wire\n\
           clicks with `on:click={{ .. }}`. A component is a function returning a `NodeId`,\n\
           spliced in with `{{ my_component(&ui) }}`.\n\
         - **`styles.css`** is the stylesheet the app reads (palette, spacing,\n\
           `border-radius`, `:hover`, and each `<div>`'s `direction`). The windowed app\n\
           watches it with `canopy-hotreload`, so saving an edit restyles the **live**\n\
           window with no restart.\n\
         \n\
         ## Dependencies\n\
         \n\
         The `Cargo.toml` depends on the Canopy crates (just `canopy-ui` for authoring,\n\
         plus the renderer). They are version placeholders until you point them at a\n\
         Canopy checkout (path dep) or a published version. Re-running\n\
         `CANOPY_CRATES_PATH=/path/to/canopy canopy new {name}` generates path deps for\n\
         you.\n"
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

        // Cargo.toml + src/main.rs + styles.css + README.md exist.
        let cargo = project.join("Cargo.toml");
        let main = project.join("src").join("main.rs");
        let styles = project.join("styles.css");
        let readme = project.join("README.md");
        assert!(cargo.is_file(), "Cargo.toml should be created");
        assert!(main.is_file(), "src/main.rs should be created");
        assert!(styles.is_file(), "styles.css should be created");
        assert!(readme.is_file(), "README.md should be created");

        // Cargo.toml names the package, pulls in canopy-ui + the sharp-text renderer, and
        // wires the window feature (winit/softbuffer) on by default.
        let cargo_txt = fs::read_to_string(&cargo).unwrap();
        assert!(
            cargo_txt.contains("name = \"my-app\""),
            "package name present"
        );
        assert!(cargo_txt.contains("canopy-ui"), "depends on canopy-ui");
        assert!(
            cargo_txt.contains("canopy-render-text"),
            "depends on the sharp-text renderer"
        );
        assert!(cargo_txt.contains("winit"), "wires the winit window dep");
        assert!(cargo_txt.contains("softbuffer"), "wires softbuffer");
        assert!(
            cargo_txt.contains("default = [\"window\"]"),
            "window feature on by default"
        );
        // Hot reload is wired, but **only** behind the `window` feature: the dep is
        // optional and the `window` feature activates it (so `--no-default-features`
        // drops it). Both halves must be present.
        assert!(
            cargo_txt.contains("canopy-hotreload"),
            "wires the hot-reload dep"
        );
        assert!(
            cargo_txt.contains("optional = true"),
            "canopy-hotreload is optional so --no-default-features drops it"
        );
        assert!(
            cargo_txt
                .contains("window = [\"dep:winit\", \"dep:softbuffer\", \"dep:canopy-hotreload\"]"),
            "the window feature activates winit + softbuffer + hot reload, got:\n{cargo_txt}"
        );

        // main.rs is the JSX welcome app: a counter signal, a "count is" reactive label,
        // a click handler, JSX tags, and the tree mounted under the root.
        let main_txt = fs::read_to_string(&main).unwrap();
        assert!(main_txt.contains("rsx!"), "main.rs uses the rsx! macro");
        assert!(main_txt.contains("<div"), "main.rs uses JSX tags");
        assert!(main_txt.contains("signal"), "main.rs creates a signal");
        assert!(
            main_txt.contains("on:click"),
            "main.rs wires a button click"
        );
        assert!(
            main_txt.contains("count is"),
            "main.rs has the 'count is {{n}}' label"
        );
        assert!(
            main_txt.contains("mount_root"),
            "main.rs mounts under the root"
        );
        assert!(
            main_txt.contains("Canopy"),
            "main.rs has the Canopy heading"
        );
        assert!(
            main_txt.contains("no JavaScript runtime"),
            "main.rs has the tagline"
        );

        // The windowed main does live hot reload: it arms a `canopy-hotreload` Watcher
        // and restyles the live tree with `Ui::reload_css` + `reapply`.
        assert!(
            main_txt.contains("Watcher"),
            "windowed main arms a hot-reload Watcher"
        );
        assert!(
            main_txt.contains("reload_css"),
            "windowed main restyles via Ui::reload_css"
        );
        assert!(
            main_txt.contains("reapply"),
            "windowed main pushes the restyle batch with reapply"
        );
        assert!(
            main_txt.contains("EventLoopProxy"),
            "windowed main wakes the loop from the watcher thread via an EventLoopProxy"
        );

        // styles.css carries the design language: rounded corners + the palette + hover.
        let styles_txt = fs::read_to_string(&styles).unwrap();
        assert!(
            styles_txt.contains("border-radius"),
            "styles.css uses rounded corners"
        );
        assert!(
            styles_txt.contains("#1e1e2e"),
            "styles.css uses the dark canvas color"
        );
        assert!(
            styles_txt.contains(":hover"),
            "styles.css has the hover cascade"
        );
    }

    #[test]
    fn scaffold_with_crates_root_emits_path_deps_for_every_crate() {
        let base = unique_temp_dir("paths");
        let _guard = TempDir(base.clone());
        let project = base.join("pathy");

        let root = Path::new("/tmp/canopy-checkout");
        scaffold(&project, "pathy", Some(root)).unwrap();

        let cargo_txt = fs::read_to_string(project.join("Cargo.toml")).unwrap();
        // Every always-on Canopy dep is wired as a path dep rooted at the provided
        // checkout.
        for krate in CANOPY_DEPS {
            let expected = format!("path = \"/tmp/canopy-checkout/crates/{krate}\"");
            assert!(
                cargo_txt.contains(&expected),
                "expected path dep for `{krate}`, got:\n{cargo_txt}"
            );
        }
        // The optional `canopy-hotreload` is also a path dep under the checkout — and,
        // because it is `window`-gated, carries `optional = true`.
        assert!(
            cargo_txt.contains(&format!(
                "{CANOPY_HOTRELOAD_DEP} = {{ path = \"/tmp/canopy-checkout/crates/{CANOPY_HOTRELOAD_DEP}\", optional = true }}"
            )),
            "expected optional path dep for `{CANOPY_HOTRELOAD_DEP}`, got:\n{cargo_txt}"
        );
    }

    #[test]
    fn scaffold_without_crates_root_emits_version_placeholders_for_every_crate() {
        let base = unique_temp_dir("versions");
        let _guard = TempDir(base.clone());
        let project = base.join("verp");

        scaffold(&project, "verp", None).unwrap();
        let cargo_txt = fs::read_to_string(project.join("Cargo.toml")).unwrap();
        for krate in CANOPY_DEPS {
            assert!(
                cargo_txt.contains(&format!("{krate} = \"0.0.0\"")),
                "expected version placeholder for `{krate}`, got:\n{cargo_txt}"
            );
        }
        // The optional `canopy-hotreload` gets a placeholder too, but in inline-table
        // form so it can carry `optional = true` (Cargo requires the table form for a
        // `dep:`-activated optional dependency).
        assert!(
            cargo_txt.contains(&format!(
                "{CANOPY_HOTRELOAD_DEP} = {{ version = \"0.0.0\", optional = true }}"
            )),
            "expected optional version placeholder for `{CANOPY_HOTRELOAD_DEP}`, got:\n{cargo_txt}"
        );
        // And it explains how to wire real deps.
        assert!(cargo_txt.contains("CANOPY_CRATES_PATH"));
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

    /// Prove the emitted template actually compiles against the real Canopy crates.
    ///
    /// Scaffolds into a temp dir with `crates_root` pointed at this checkout (so the
    /// generated `Cargo.toml` uses path deps) and runs `cargo +nightly check` on the
    /// generated project. We check `--no-default-features` (UI logic only) so the test
    /// does not pull in the winit/softbuffer/`canopy-hotreload` windowing + hot-reload
    /// stack — keeping it fast while still proving the welcome UI type-checks against the
    /// crates' real APIs. The full windowed build is verified manually.
    ///
    /// `#[ignore]` by default because it shells out to `cargo` and compiles a real
    /// dependency graph (minutes of build time). Run it explicitly with:
    ///
    /// ```sh
    /// CANOPY_CRATES_PATH=/abs/path/to/canopy \
    ///   cargo +nightly test -p canopy-cli -- --ignored scaffolded_template_compiles
    /// ```
    ///
    /// The checkout root comes from `CANOPY_CRATES_PATH`; if it is unset the test skips,
    /// so a harness can run it unconditionally.
    #[test]
    #[ignore = "shells out to cargo and compiles the real crate graph; run with --ignored"]
    fn scaffolded_template_compiles() {
        let Some(root) = std::env::var_os(CRATES_ROOT_ENV) else {
            eprintln!("skipping: set {CRATES_ROOT_ENV}=/abs/path/to/canopy to run this test");
            return;
        };
        let root = PathBuf::from(root);
        assert!(
            root.join("crates/canopy-ui").is_dir(),
            "{CRATES_ROOT_ENV} must point at a Canopy checkout root; \
             `{}/crates/canopy-ui` not found",
            root.display()
        );

        let base = unique_temp_dir("compile");
        let _guard = TempDir(base.clone());
        let project = base.join("welcome-app");
        scaffold(&project, "welcome-app", Some(&root)).expect("scaffold");

        let status = std::process::Command::new("cargo")
            .arg("+nightly")
            .arg("check")
            .arg("--no-default-features")
            .current_dir(&project)
            .status()
            .expect("spawn cargo check");
        assert!(
            status.success(),
            "generated project failed `cargo +nightly check --no-default-features`"
        );
    }
}
