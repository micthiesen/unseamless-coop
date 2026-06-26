//! Process-global bridge between the game-thread diagnostic publisher and the Present-thread debug
//! panel ([`crate::overlay`]'s bottom-left surface).
//!
//! The overlay must never read game singletons; it only reads *published* shared state non-blocking
//! (same rule as [`crate::playstate`] / [`crate::state`]). So the live debug panel works in two
//! halves: the overlay publishes whether the panel is *shown* (an atomic the game thread reads), and
//! a game-thread probe ([`crate::diag`]'s `debug-panel` feature) — only when it's shown — publishes a
//! [`DiagnosticReport`] snapshot here that the overlay reads non-blocking and renders.
//!
//! When the panel is off it's a single atomic load on the game thread (the publisher early-returns),
//! so the panel costs nothing when not shown.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::diagnostics::DiagnosticReport;

/// Whether the debug panel is currently shown. Written by the overlay (Present thread), read by the
/// game-thread publisher. `Relaxed` is correct: it only gates work and publishes no other memory
/// through itself (same reasoning as [`crate::input`]'s block flag).
static VISIBLE: AtomicBool = AtomicBool::new(false);

/// Latest published snapshot, or `None` before the first publish. A `Mutex` (like [`crate::state`]'s
/// live config) read non-blocking from the Present thread.
static SNAPSHOT: OnceLock<Mutex<Option<DiagnosticReport>>> = OnceLock::new();

/// Initialize the snapshot cell. Called once at install, before any feature ticks or the overlay
/// renders.
pub fn init() {
    let _ = SNAPSHOT.set(Mutex::new(None));
}

/// Publish whether the debug panel is shown (overlay → game thread).
pub fn set_visible(visible: bool) {
    VISIBLE.store(visible, Ordering::Relaxed);
}

/// Whether the debug panel is shown — the gate the game-thread publisher checks before doing any work.
pub fn visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

/// Publish the latest diagnostic snapshot (game thread). No-op before [`init`]. The lock is held only
/// for the move-assign, so the Present thread's [`snapshot`] (a `try_lock`) almost never contends.
pub fn publish(report: DiagnosticReport) {
    if let Some(m) = SNAPSHOT.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = Some(report);
    }
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
