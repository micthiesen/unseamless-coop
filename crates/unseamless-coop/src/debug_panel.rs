//! Process-global bridge between the game-thread diagnostic publisher and the Present-thread debug
//! panel ([`crate::overlay`]'s bottom-left surface).
//!
//! The overlay must never read game singletons; it only reads *published* shared state non-blocking
//! (same rule as [`crate::playstate`] / [`crate::state`]). So the live debug panel works in two
//! halves: the overlay publishes whether a report is *wanted* — i.e. the summary panel or any detail
//! pane is showing — (an atomic the game thread reads), and a game-thread probe ([`crate::diag`]'s
//! `debug-panel` feature) — only when one is wanted — publishes a [`DiagnosticReport`] snapshot here
//! that the overlay reads non-blocking and renders.
//!
//! When nothing wants the report it's a single atomic load on the game thread (the publisher
//! early-returns), so the panel costs nothing when not shown.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::diagnostics::DiagnosticReport;

/// Whether a diagnostic report is currently wanted — the overlay's summary panel or any detail pane
/// is showing. Written by the overlay (Present thread), read by the game-thread publisher. `Relaxed`
/// is correct: it only gates work and publishes no other memory through itself (same reasoning as
/// [`crate::input`]'s block flag).
static REPORT_WANTED: AtomicBool = AtomicBool::new(false);

/// Latest published snapshot, or `None` before the first publish. A `Mutex` (like [`crate::state`]'s
/// live config) read non-blocking from the Present thread.
static SNAPSHOT: OnceLock<Mutex<Option<DiagnosticReport>>> = OnceLock::new();

/// Monotonic publish counter, bumped once per [`publish`] (so it advances at the publisher's ~10 Hz,
/// not the Present hook's frame rate). The overlay reads it cheaply each frame via [`version`] and only
/// deep-clones a new [`snapshot`] when it advances — turning a per-frame clone of the whole report into
/// a per-publish one. Starts at 0 (before any publish); the report is fresh whenever this differs from
/// the value the overlay last cloned at.
static VERSION: AtomicU64 = AtomicU64::new(0);

/// Initialize the snapshot cell. Called once at install, before any feature ticks or the overlay
/// renders.
pub fn init() {
    let _ = SNAPSHOT.set(Mutex::new(None));
}

/// Publish whether a diagnostic report is wanted — summary panel or any detail pane showing
/// (overlay → game thread).
pub fn set_report_wanted(wanted: bool) {
    REPORT_WANTED.store(wanted, Ordering::Relaxed);
}

/// Whether a diagnostic report is wanted — the gate the game-thread publisher checks before doing any
/// work.
pub fn report_wanted() -> bool {
    REPORT_WANTED.load(Ordering::Relaxed)
}

/// Publish the latest diagnostic snapshot (game thread). No-op before [`init`]. The lock is held only
/// for the move-assign, so the Present thread's [`snapshot`] (a `try_lock`) almost never contends.
pub fn publish(report: DiagnosticReport) {
    if let Some(m) = SNAPSHOT.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = Some(report);
        // The synchronizing edge for the report itself is the *mutex*, not this counter: the overlay
        // always reads the report by re-locking in `snapshot`, and that lock-acquire synchronizes-with
        // this lock-release, so the clone it gets is never older than the version it then observes. The
        // counter only needs to signal "changed", so `Relaxed` suffices (no ordering is leaned on here).
        VERSION.fetch_add(1, Ordering::Relaxed);
    }
}

/// The current publish version (see [`VERSION`]). A cheap, non-blocking atomic load the overlay checks
/// each frame to decide whether the report changed since its last clone; if unchanged it reuses its
/// cached clone and skips [`snapshot`] entirely.
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// A **non-blocking** clone of the latest snapshot, for the overlay's Present thread (which must
/// never block on the game thread). `None` if uninitialized, momentarily contended, or nothing's been
/// published yet — the overlay then skips drawing the panel this frame. Cloned out (rather than
/// rendered under the lock) so an imgui draw never holds the lock the game-thread publisher blocks on.
pub fn snapshot() -> Option<DiagnosticReport> {
    let m = SNAPSHOT.get()?;
    match m.try_lock() {
        Ok(guard) => guard.clone(),
        Err(TryLockError::Poisoned(p)) => p.into_inner().clone(),
        Err(TryLockError::WouldBlock) => None,
    }
}
