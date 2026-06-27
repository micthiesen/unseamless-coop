//! Process-global **notifications**, shared between the game thread and the overlay's Present thread.
//!
//! Mirrors [`crate::state`] (the live config) but for the host-tested
//! [`Notifications`](unseamless_core::notifications::Notifications) model: game-thread features push
//! toasts/banners and a dedicated feature `tick`s it once per frame; the overlay's Present-thread
//! render loop reads it to draw. The read path is **non-blocking** (`try_lock`) — the present thread
//! must never block on the game thread, so a momentarily-held lock just skips drawing that frame.
//!
//! Three nearby names, to disambiguate: `unseamless_core::notifications` is the *model*; this module
//! (`crate::notify`) is the *shared-state accessor*; `crate::features::notifications` is the *ager*
//! (ticks the model once per frame).

use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Notifications, Severity};

static NOTIFICATIONS: OnceLock<Mutex<Notifications>> = OnceLock::new();

/// Initialize the shared notifications. Called once at install, before any feature ticks or the
/// overlay draws.
pub fn init() {
    let _ = NOTIFICATIONS.set(Mutex::new(Notifications::new()));
}

/// Mutate the notifications (push a toast/banner, or `tick`) from the **game thread**. The lock is
/// held only for the brief mutation. No-op before [`init`]. Poison is recovered: a panic mid-push
/// leaves the `Vec`s structurally intact, so the value is still safe to use.
pub fn with_mut(f: impl FnOnce(&mut Notifications)) {
    match NOTIFICATIONS.get() {
        Some(m) => f(&mut m.lock().unwrap_or_else(|p| p.into_inner())),
        // `init` runs in `app::install` before any producer pushes, so this is unreachable. Assert
        // it in dev so an ordering regression is loud, not a silently-dropped notification (matches
        // `state::with`). `try_read` deliberately has no such guard — it's called pre-init from the
        // Present thread and returning None then is correct.
        None => debug_assert!(false, "notify::with_mut called before init()"),
    }
}

/// Read the notifications **without blocking** — for the overlay's Present-thread render loop. Runs
/// `f` and returns its result, or `None` if uninitialized or the lock is momentarily held by the
/// game thread (the caller skips drawing this frame).
pub fn try_read<R>(f: impl FnOnce(&Notifications) -> R) -> Option<R> {
    let m = NOTIFICATIONS.get()?;
    match m.try_lock() {
        Ok(n) => Some(f(&n)),
        // Poisoned: recover (the `Vec`s are intact) rather than never drawing again.
        Err(TryLockError::Poisoned(p)) => Some(f(&p.into_inner())),
        // Contended: the game thread holds it for a brief push/tick — skip this frame.
        Err(TryLockError::WouldBlock) => None,
    }
}

// ---- Game-thread producer convenience wrappers ----------------------------------------------------
//
// The canonical one-liners for the common toast/banner pushes, so the producer modules
// ([`crate::coop`], [`crate::steam_ready`], …) don't each re-spell the `with_mut(|n| n.…)` body (and
// the `DEFAULT_TOAST_SECS` default). All are thin [`with_mut`] calls — game-thread / driver-thread
// only, never the Present thread (which uses [`try_read`]).

/// Push a toast with the standard lifetime.
pub fn toast(severity: Severity, message: impl Into<String>) {
    with_mut(|n| n.toast(severity, message, DEFAULT_TOAST_SECS));
}

/// Set (or update in place) the persistent banner under `id`.
pub fn set_banner(id: &str, severity: Severity, message: impl Into<String>) {
    with_mut(|n| n.set_banner(id, severity, message));
}

/// Clear the persistent banner under `id` (no-op if it isn't set).
pub fn clear_banner(id: &str) {
    with_mut(|n| {
        n.clear_banner(id);
    });
}
