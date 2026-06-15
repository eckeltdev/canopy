//! Integration tests for `canopy-hotreload`.
//!
//! Two behaviours are covered, deterministically:
//!
//! 1. The [`Watcher`] actually fires its debounced callback when a watched file is
//!    written. We assert this with a *bounded* channel wait — a generous timeout
//!    that fails loudly if the callback never fires — rather than a fixed sleep
//!    that would either flake (too short) or slow the suite (too long). Filesystem
//!    notification is inherently asynchronous, so there is no wall-clock-free way to
//!    observe it; the discipline is to wait on a *signal* with an upper bound, not
//!    to sleep for a guessed duration.
//! 2. A second `Emitter` build's op-batch applies cleanly via [`reapply`] onto a
//!    `Dom` that already holds an initial tree, mutating it as expected (a live text
//!    update). This part is fully synchronous and deterministic.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use canopy_core::Emitter;
use canopy_dom::{Dom, ROOT};
use canopy_hotreload::{reapply, ReloadEvent, Watcher};
use canopy_protocol::ElementTag;
use canopy_traits::OpSink;

/// Upper bound for "the watcher should have fired by now". Far longer than any real
/// FS-notify + 80ms debounce latency, so a healthy machine passes instantly and
/// only a genuinely broken watcher times out. Generous on purpose for slow CI.
const FIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// A self-cleaning unique temp directory, so tests don't collide and leave litter.
/// Avoids pulling a `tempfile` dev-dependency for two tests.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        // Uniqueness from pid + a per-process atomic counter: stable without a
        // clock dependency, distinct across concurrent test binaries/threads.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("canopy-hotreload-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Build a tiny tree: a column element with a single text child, mounted under
/// [`ROOT`]. Returns the batch bytes plus the text node's handle so a follow-up
/// build can target it.
fn initial_tree() -> (Vec<u8>, canopy_protocol::NodeId) {
    let mut e = Emitter::new();
    let col = e.create_element(ElementTag::new(1));
    e.append(ROOT, col);
    let label = e.create_text("before");
    e.append(col, label);
    (e.take_batch(0), label)
}

#[test]
fn watcher_fires_on_file_change() {
    let dir = TempDir::new("fire");
    let file = dir.child("styles.css");
    std::fs::write(&file, b"/* v1 */\n").expect("seed file");

    // The callback signals every fired burst over a channel; the test thread waits
    // on it with a bounded timeout (no fixed sleep, so this can't flake on timing).
    let (tx, rx) = mpsc::channel::<ReloadEvent>();
    let watcher = Watcher::new(&file, move |event| {
        // If the receiver is gone the test already finished — ignore the error.
        let _ = tx.send(event);
    })
    .expect("start watcher");

    // Mutate the watched file. Some platforms need the watch to be fully armed; if
    // the first write races arming, we retry writes until the callback fires or the
    // overall timeout elapses — robust on slow CI without trusting any single sleep.
    let deadline = std::time::Instant::now() + FIRE_TIMEOUT;
    let mut fired: Option<ReloadEvent> = None;
    let mut version = 0u32;
    while std::time::Instant::now() < deadline {
        version += 1;
        std::fs::write(&file, format!("/* v{version} */\n")).expect("write change");
        // Wait a slice of the budget for a fire; if none, loop and write again.
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => {
                fired = Some(ev);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => unreachable!("watcher kept alive"),
        }
    }

    let event = fired.expect("watcher callback never fired within timeout");
    // The burst must name our file.
    let canon = std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone());
    assert!(
        event
            .paths
            .iter()
            .any(|p| paths_equal(p, &file) || paths_equal(p, &canon)),
        "fired event {:?} did not mention {:?}",
        event.paths,
        file
    );
    // `path()` accessor is consistent with `paths[0]` and never panics.
    assert_eq!(event.path(), event.paths[0].as_path());

    // Dropping the guard must join cleanly and stop further callbacks.
    drop(watcher);
}

/// Compare paths tolerantly: on macOS the temp dir is often `/var/...` while
/// notify reports the `/private/var/...` canonical form (or vice versa).
fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a.file_name() == b.file_name(),
    }
}

#[test]
fn dropping_watcher_stops_callbacks() {
    // A dropped watcher must not invoke the callback afterwards. We watch, drop,
    // then write, and assert nothing arrives within a bounded grace window.
    let dir = TempDir::new("drop");
    let file = dir.child("ui.txt");
    std::fs::write(&file, b"v0").expect("seed");

    let (tx, rx) = mpsc::channel::<ReloadEvent>();
    let watcher = Watcher::with_debounce(&file, Duration::from_millis(20), move |event| {
        let _ = tx.send(event);
    })
    .expect("start watcher");

    drop(watcher); // joins the debounce thread; no callback can fire after this.

    std::fs::write(&file, b"v1-after-drop").expect("write after drop");
    // Nothing should arrive. A short bounded wait is enough: drop already joined, so
    // any in-flight callback would have completed before drop returned.
    match rx.recv_timeout(Duration::from_millis(300)) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {} // expected: sender dropped.
        Err(mpsc::RecvTimeoutError::Timeout) => {}      // also fine: never fired.
        Ok(ev) => panic!("callback fired after watcher was dropped: {ev:?}"),
    }
}

#[test]
fn rebuilt_batch_reapplies_onto_live_tree() {
    // Mount an initial tree onto a fresh Dom, exactly as a host would at startup.
    let (batch0, label) = initial_tree();
    let mut dom = Dom::new();
    dom.apply(&batch0).expect("initial mount");
    assert_eq!(dom.node_count(), 2);
    assert_eq!(dom.text_of(label), Some("before"));

    // Simulate a hot reload: a *second* Emitter build re-runs the same deterministic
    // construction, so handles line up, then emits the changed value as a targeted
    // op. This is precisely what a host produces inside the watch callback.
    let mut rebuild = Emitter::new();
    let col = rebuild.create_element(ElementTag::new(1)); // handle 1, as before
    rebuild.append(ROOT, col);
    let label2 = rebuild.create_text("before"); // handle 2 == `label`
    rebuild.append(col, label2);
    assert_eq!(
        label2, label,
        "deterministic rebuild must reproduce handles"
    );

    // The edit: the text changes. The rebuild lowers it to a `SetText` on the
    // existing handle (the signal-reactive hot path).
    rebuild.set_text(label2, "after");
    let reload_batch = rebuild.take_batch(1);

    // Apply via the crate's reload glue onto the *existing* tree.
    reapply(&mut dom, &reload_batch).expect("reapply reload batch");

    // The live tree updated in place: same structure, new text.
    assert_eq!(dom.node_count(), 2, "structure unchanged");
    assert_eq!(dom.children(ROOT), &[col]);
    assert_eq!(dom.children(col), &[label]);
    assert_eq!(
        dom.text_of(label),
        Some("after"),
        "text node updated by the reload batch"
    );
}

#[test]
fn reapply_rejects_a_forged_handle() {
    // The reload glue must inherit the Dom's capability check: a batch that targets
    // a node the live tree never created is rejected, not silently aliased.
    let (batch0, _label) = initial_tree();
    let mut dom = Dom::new();
    dom.apply(&batch0).expect("initial mount");

    let mut forged = Emitter::new();
    for _ in 0..256 {
        forged.alloc_node(); // burn handles so none collide with the real tree.
    }
    let ghost = forged.alloc_node();
    forged.set_text(ghost, "haxx");

    assert_eq!(
        reapply(&mut dom, &forged.take_batch(1)),
        Err(canopy_traits::HostError::BadHandle)
    );
    // The live tree is untouched.
    assert_eq!(dom.node_count(), 2);
}
