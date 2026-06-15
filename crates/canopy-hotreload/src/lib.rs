//! Canopy dev-time **hot reload**: edit a file, save, and watch the running tree
//! update — the "Vite/React dev loop" for a Canopy app, without a restart.
//!
//! Canopy apps are structurally a *guest* that emits an op-stream and a *host* that
//! applies it to a retained [`Dom`](canopy_dom::Dom) (see `canopy-core` /
//! `canopy-dom`). That split is exactly what makes a live reload tractable: the
//! host's tree is just the accumulated result of op-batches, so "reload" means
//! "rebuild a fresh batch from the edited source and apply it onto the tree that's
//! already on screen." No process restart, no lost window — the next batch mutates
//! the live arena in place.
//!
//! This crate ships the two dev-only pieces a host needs for that loop:
//!
//! 1. [`Watcher`] — a debounced filesystem watcher. Point it at a file or
//!    directory; on save it coalesces the OS event burst and invokes your callback
//!    once. Dropping the returned [`Watcher`] stops watching and joins its thread.
//! 2. [`reapply`] — the thin, well-documented glue that applies a freshly rebuilt
//!    op-batch onto an existing [`Dom`](canopy_dom::Dom). Because the `Dom` already
//!    validates and applies batches (and enforces the capability boundary), this is
//!    deliberately a one-liner with a precise contract, not a re-implementation.
//!
//! # The intended host loop
//!
//! This crate is the *plumbing*; the host owns the policy. The canonical dev loop —
//! the one the showcase will use for live style editing — is:
//!
//! ```no_run
//! # use std::sync::{Arc, Mutex};
//! # use canopy_dom::Dom;
//! # use canopy_hotreload::{Watcher, reapply};
//! # fn rebuild_op_batch_from(_src: &str) -> Vec<u8> { Vec::new() }
//! # fn request_redraw() {}
//! // The live tree the renderer paints. Shared with the watcher callback, which
//! // runs on the watcher's own thread — hence the lock.
//! let dom = Arc::new(Mutex::new(Dom::new()));
//!
//! let dom_for_cb = Arc::clone(&dom);
//! let _watcher = Watcher::new("app/styles.css", move |event| {
//!     // 1. Read the edited source/asset that changed.
//!     let Ok(src) = std::fs::read_to_string(&event.paths[0]) else { return };
//!     // 2. Re-parse / rebuild the op-stream (your App/Emitter rebuild).
//!     let batch = rebuild_op_batch_from(&src);
//!     // 3. Apply it onto the *existing* tree, mutating it in place.
//!     let mut dom = dom_for_cb.lock().unwrap();
//!     if let Err(e) = reapply(&mut dom, &batch) {
//!         eprintln!("hot reload: batch rejected: {e}"); // keep the old tree
//!         return;
//!     }
//!     // 4. Ask the platform to repaint with the updated tree.
//!     request_redraw();
//! })
//! .expect("failed to start watcher");
//!
//! // ... run your event loop; `_watcher` lives as long as you want reloads ...
//! ```
//!
//! # The reload contract
//!
//! Step 2 above is the part *you* own, and it has one rule that follows directly
//! from how `canopy-dom` works: **a reload batch is applied on top of the existing
//! arena, not a fresh one.** A node handle is only valid if the `Dom` already knows
//! it (or the same batch creates it first); a batch that mutates a node the tree
//! has never seen is rejected with [`HostError::BadHandle`]. In practice a rebuild
//! comes from re-running the *same* deterministic `App`/`Emitter` build, so handles
//! line up and a changed value (a restyle, a text edit) becomes a targeted
//! `SetText` / `SetInlineStyle` against a handle the tree already holds. Live *data*
//! reload — the realistic 80/20 — is fully supported this way.
//!
//! # Scope: data reload, not code reload
//!
//! This is hot reload of the UI's *data* (styles, text, the declarative tree),
//! driven by re-running native/WASM build logic that is already linked in. It does
//! **not** reload native code: swapping a recompiled `dylib` (so edits to Rust
//! *logic* take effect without a restart) is a much larger problem — symbol
//! relocation, dropping in-flight state, ABI stability — and is intentionally left
//! as future work, not attempted here. The honest, useful win is: a designer edits
//! a stylesheet, hits save, and the live window restyles.
//!
//! # Threading model
//!
//! [`Watcher`] is multi-threaded and your callback runs **off the main thread**:
//!
//! - `notify` spawns its own OS-watch thread and pushes raw events into a channel.
//! - This crate spawns **one** *debounce* thread that drains that channel,
//!   coalesces a burst of events within the debounce window (default
//!   [`DEFAULT_DEBOUNCE`]), and then calls your closure once per quiet burst, on
//!   that debounce thread.
//!
//! So your callback must be `Send + 'static` and is **never** re-entered
//! concurrently (the single debounce thread calls it serially). To touch state the
//! main/render thread also uses — the [`Dom`](canopy_dom::Dom), a redraw flag —
//! share it behind a `Mutex`/channel, as the example above does. Dropping the
//! [`Watcher`] signals the debounce thread to stop and joins it, so no callback can
//! fire after drop returns.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use canopy_dom::Dom;
use canopy_traits::{HostError, OpSink};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// Default debounce window: coalesce a burst of filesystem events that land within
/// this span into a single callback.
///
/// Editors routinely emit several events per save (truncate, write, rename, chmod,
/// fsync), and on some platforms a single save fans out further. 80ms is long
/// enough to absorb that burst yet short enough to feel instant. Tune via
/// [`Watcher::with_debounce`].
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(80);

/// A coalesced "something you watch changed, reload now" notification.
///
/// One [`ReloadEvent`] represents a *debounced burst*: the set of paths that
/// changed during one quiet-settling window, deduplicated and order-stable. For a
/// single watched file this is just that file; for a watched directory it is every
/// distinct path touched in the burst.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReloadEvent {
    /// The distinct filesystem paths that changed in this burst, in first-seen
    /// order. Always non-empty. For a single watched file, exactly that file.
    pub paths: Vec<PathBuf>,
}

impl ReloadEvent {
    /// A convenience accessor for the common single-file case: the first changed
    /// path. Never panics — [`ReloadEvent::paths`] is guaranteed non-empty.
    #[must_use]
    pub fn path(&self) -> &Path {
        // INVARIANT: the debounce thread only ever constructs a `ReloadEvent` once
        // it has at least one path (see `Watcher::spawn`), so [0] is always valid.
        &self.paths[0]
    }
}

/// Errors that can occur starting a [`Watcher`].
///
/// Once a watcher is running, transient filesystem errors are surfaced through the
/// callback's own logic (you `read` the changed file and decide what to do); the
/// only thing that can fail *here* is bootstrapping the OS watch.
#[derive(Debug)]
pub enum WatchError {
    /// The OS-level watch could not be created or armed (e.g. the path does not
    /// exist, or the platform ran out of watch descriptors).
    Notify(notify::Error),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::Notify(e) => write!(f, "filesystem watch failed: {e}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Notify(e) => Some(e),
        }
    }
}

impl From<notify::Error> for WatchError {
    fn from(e: notify::Error) -> Self {
        WatchError::Notify(e)
    }
}

/// A guard that watches a path and invokes a debounced callback on change.
///
/// Construct one with [`Watcher::new`] (or [`Watcher::with_debounce`] to tune the
/// coalescing window). The watcher runs until the guard is dropped: drop stops the
/// OS watch, signals the debounce thread to finish, and joins it, guaranteeing no
/// callback fires after `drop` returns.
///
/// See the [crate docs](crate#threading-model) for the threading model — the
/// callback runs on the debounce thread, never the thread that holds this guard.
pub struct Watcher {
    /// Held only to keep the OS watch alive. Wrapped in `Option` so [`Drop`] can
    /// drop it *first* — dropping it stops `notify`'s thread and closes the
    /// raw-event channel, which is what unblocks the debounce thread's `recv` so
    /// the subsequent `join` returns promptly instead of deadlocking.
    inner: Option<RecommendedWatcher>,
    /// `Some` until `drop`; taken so we can `join` the debounce thread.
    debounce: Option<JoinHandle<()>>,
}

impl Watcher {
    /// Start watching `path`, calling `on_change` once per debounced burst of
    /// changes, using the [`DEFAULT_DEBOUNCE`] window.
    ///
    /// `path` may be a file (watched directly) or a directory (watched
    /// recursively). The callback runs on a background thread and must therefore be
    /// `Send + 'static`; it is called serially, never re-entered (see the
    /// [crate docs](crate#threading-model)).
    ///
    /// # Errors
    ///
    /// Returns [`WatchError`] if the OS watch cannot be created or armed — most
    /// commonly because `path` does not exist.
    pub fn new<P, F>(path: P, on_change: F) -> Result<Self, WatchError>
    where
        P: AsRef<Path>,
        F: FnMut(ReloadEvent) + Send + 'static,
    {
        Self::with_debounce(path, DEFAULT_DEBOUNCE, on_change)
    }

    /// Like [`Watcher::new`], but with an explicit debounce window.
    ///
    /// A larger `debounce` coalesces more aggressively (fewer, later callbacks); a
    /// smaller one is snappier but may fire several times for one logical save on
    /// chatty platforms. A zero window still coalesces whatever events arrived in
    /// the same scheduling instant, but effectively fires per-event.
    ///
    /// # Errors
    ///
    /// Returns [`WatchError`] if the OS watch cannot be created or armed.
    pub fn with_debounce<P, F>(
        path: P,
        debounce: Duration,
        on_change: F,
    ) -> Result<Self, WatchError>
    where
        P: AsRef<Path>,
        F: FnMut(ReloadEvent) + Send + 'static,
    {
        let path = path.as_ref();

        // A file is watched non-recursively; a directory recursively so edits to
        // any asset under it are caught. (`notify` ignores the mode for files.)
        let recursive = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        // Raw events flow notify-thread -> `raw_tx`/`raw_rx` -> debounce thread.
        let (raw_tx, raw_rx) = mpsc::channel::<notify::Event>();
        let mut inner: RecommendedWatcher = notify::recommended_watcher(move |res| {
            // `notify` hands us `Result<Event, Error>`. We forward only successful
            // events; a `SendError` here just means the debounce thread already
            // shut down (guard dropped), so dropping the event is correct.
            if let Ok(event) = res {
                let _ = raw_tx.send(event);
            }
        })?;
        inner.watch(path, recursive)?;

        let debounce = Self::spawn(raw_rx, debounce, on_change);

        Ok(Self {
            inner: Some(inner),
            debounce: Some(debounce),
        })
    }

    /// Spawn the single debounce thread.
    ///
    /// It blocks on the raw-event channel; on the first event it opens a coalescing
    /// window of `debounce`, keeps draining (resetting the window on each fresh
    /// event so a steady stream of writes settles before we fire), then emits one
    /// [`ReloadEvent`] with the deduplicated paths. The thread exits when the
    /// channel closes (the [`Watcher`] guard — and thus `notify`'s sender — was
    /// dropped).
    fn spawn<F>(
        raw_rx: mpsc::Receiver<notify::Event>,
        debounce: Duration,
        mut on_change: F,
    ) -> JoinHandle<()>
    where
        F: FnMut(ReloadEvent) + Send + 'static,
    {
        std::thread::Builder::new()
            .name("canopy-hotreload-debounce".into())
            .spawn(move || {
                loop {
                    // Block until the first event of a new burst (or shutdown).
                    let first = match raw_rx.recv() {
                        Ok(ev) => ev,
                        Err(_) => return, // channel closed -> guard dropped.
                    };

                    // Collect paths first-seen-ordered, deduplicated.
                    let mut paths: Vec<PathBuf> = Vec::new();
                    let push = |evt: notify::Event, paths: &mut Vec<PathBuf>| {
                        for p in evt.paths {
                            if !paths.contains(&p) {
                                paths.push(p);
                            }
                        }
                    };
                    push(first, &mut paths);

                    // Drain the burst: keep accepting events until the window goes
                    // quiet for `debounce`. Each new event refreshes the deadline.
                    let mut deadline = Instant::now() + debounce;
                    let closed = loop {
                        let now = Instant::now();
                        let wait = deadline.saturating_duration_since(now);
                        match raw_rx.recv_timeout(wait) {
                            Ok(ev) => {
                                push(ev, &mut paths);
                                deadline = Instant::now() + debounce;
                            }
                            Err(RecvTimeoutError::Timeout) => break false,
                            Err(RecvTimeoutError::Disconnected) => break true,
                        }
                    };

                    // Some backends emit only metadata-bearing events with no
                    // paths; skip an empty burst rather than fire a useless reload.
                    if !paths.is_empty() {
                        on_change(ReloadEvent { paths });
                    }

                    if closed {
                        return;
                    }
                }
            })
            .expect("failed to spawn hot-reload debounce thread")
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        // Order matters. Drop the OS watcher *first*: that stops `notify`'s thread
        // and, crucially, drops the raw-event `Sender`, closing the channel. The
        // debounce thread's blocking `recv`/`recv_timeout` then returns
        // `Disconnected`, so it falls out of its loop. Only after that do we join —
        // otherwise the join could block forever waiting on a thread parked in
        // `recv` with a still-open sender.
        drop(self.inner.take());
        if let Some(handle) = self.debounce.take() {
            // If the debounce thread panicked inside the user callback, propagating
            // here would abort during unwind; swallow it (the watcher is going away
            // regardless) so drop is infallible.
            let _ = handle.join();
        }
    }
}

/// Apply a freshly rebuilt op-batch onto an existing live [`Dom`](canopy_dom::Dom).
///
/// This is the host-side glue for the hot-reload loop: in your watch callback you
/// read the edited source, rebuild a batch (`Emitter`/`App` → `take_batch`), and
/// call this to push the batch onto the tree the renderer is already showing.
///
/// It is a thin, intentional wrapper over [`OpSink::apply`]: the `Dom` decodes,
/// validates, and applies the batch, enforcing the capability boundary (a node the
/// tree never created is rejected). Keeping it a named function — rather than
/// inlining `dom.apply(..)` at every call site — gives the reload path one
/// documented seam and one obvious place for the contract below.
///
/// # Atomicity
///
/// `apply` is **not** transactional: ops are applied in order, and a malformed or
/// rejected op stops processing, leaving the ops *before* it already applied. For a
/// hot reload that means a bad batch can leave the tree partially updated. Build
/// reload batches from a deterministic, known-good rebuild (the same `App` you
/// mounted with) so this stays a "shouldn't happen" path; on an `Err`, treat the
/// tree as suspect and prefer a full remount over trusting a half-applied reload.
///
/// # Errors
///
/// Returns [`HostError::Decode`] if the bytes are not a valid op-stream, or
/// [`HostError::BadHandle`] if the batch references a node the `Dom` does not own —
/// which, during reload, usually means the rebuild diverged from the mounted tree.
pub fn reapply(dom: &mut Dom, batch: &[u8]) -> Result<(), HostError> {
    dom.apply(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;

    /// A unique scratch directory for a test, removed on drop. Kept here (rather
    /// than depending on `tempfile`) so the unit tests stay dependency-free.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let mut p = std::env::temp_dir();
            p.push(format!(
                "canopy-hotreload-unit-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reload_event_path_is_first_path() {
        let ev = ReloadEvent {
            paths: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        };
        assert_eq!(ev.path(), Path::new("/a"));
    }

    /// A burst of rapid writes inside one debounce window must collapse into a
    /// single callback whose path set is deduplicated — the whole point of the
    /// debounce thread. We use a deliberately long window so all writes land in one
    /// burst, then assert exactly one callback arrived (bounded wait, no fixed
    /// sleep to gate correctness).
    #[test]
    fn rapid_writes_coalesce_into_one_callback() {
        let dir = Scratch::new("coalesce");
        let file = dir.0.join("styles.css");
        std::fs::write(&file, b"v0").unwrap();

        let (tx, rx) = mpsc::channel::<ReloadEvent>();
        // 400ms window: long enough to swallow a tight write loop into one burst.
        let watcher = Watcher::with_debounce(&file, Duration::from_millis(400), move |ev| {
            let _ = tx.send(ev);
        })
        .expect("start watcher");

        // Hammer the file. These all fall inside the single debounce window.
        for i in 0..8 {
            std::fs::write(&file, format!("v{i}")).unwrap();
        }

        // Exactly one coalesced callback should arrive. Bounded wait for the first.
        let first = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("debounced callback should fire");
        assert!(!first.paths.is_empty());
        // Paths are deduplicated within a burst.
        let mut seen = first.paths.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), first.paths.len(), "burst paths must be unique");

        // No *second* callback for the same burst within a further grace window.
        match rx.recv_timeout(Duration::from_millis(600)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(extra) => panic!("burst was not coalesced; got a 2nd callback: {extra:?}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        drop(watcher);
    }
}
