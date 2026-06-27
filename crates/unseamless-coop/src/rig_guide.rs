//! Process-global bridge between the game-thread rig-guide feature and the Present-thread overlay
//! draw — the pinned step-banner channel.
//!
//! Mirrors [`crate::nameplates`]: the overlay must never read game singletons or run engine logic, so
//! the game-thread feature ([`crate::features::rig_guide`]) ticks the host-tested
//! [`GuideRunner`](unseamless_core::guide::GuideRunner) and **publishes** the current pinned banner
//! here; the overlay reads it non-blocking on the Present thread and draws it. When no guide is active
//! (or it's finished), the published banner is `None` and the overlay draws nothing.
//!
//! Debug-only, like the rest of the guide subsystem (`#[cfg(debug_assertions)]` in `crate::lib`).

use std::sync::{Mutex, OnceLock, TryLockError};

/// The current pinned banner to draw: the text (instruction + auto-appended control hints, possibly a
/// `[PENDING …]` stub marker) and its auto-assigned RGB colour. Built by the game-thread feature from
/// a [`TickResult`](unseamless_core::guide::TickResult); colours are never chosen in a guide.
#[derive(Clone, Debug)]
pub struct RigBanner {
    pub text: String,
    pub color: [f32; 3],
}

/// Latest published banner (`None` = no guide active / finished), like [`crate::nameplates`]'s label
/// cell — a `Mutex<Option<_>>` read non-blocking from the Present thread.
static BANNER: OnceLock<Mutex<Option<RigBanner>>> = OnceLock::new();

/// Initialize the banner cell. Called once at install (in `app::install`), before any feature ticks
/// or the overlay renders.
pub fn init() {
    let _ = BANNER.set(Mutex::new(None));
}

/// Publish the current pinned banner (game thread). No-op before [`init`]. The lock is held only for
/// the move-assign, so the Present thread's [`snapshot`] (a `try_lock`) almost never contends. Publish
/// `None` to clear a stale banner (guide finished / not running).
pub fn publish(banner: Option<RigBanner>) {
    if let Some(m) = BANNER.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = banner;
    }
}

/// A **non-blocking** clone of the latest banner, for the overlay's Present thread (which must never
/// block on the game thread). The outer `Option` is `None` if uninitialized or momentarily contended
/// (skip drawing this frame); the inner `Option` is `None` when no guide banner is active.
pub fn snapshot() -> Option<Option<RigBanner>> {
    let m = BANNER.get()?;
    match m.try_lock() {
        Ok(guard) => Some(guard.clone()),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner().clone()),
        Err(TryLockError::WouldBlock) => None,
    }
}
