//! Process-global **live config**, shared between the game-thread features and the dev bridge.
//!
//! Features read this each frame instead of holding a construction-time snapshot, so a config change
//! from any source takes effect without rebuilding them. The writers are the co-op side-channel
//! ([`crate::coop`]) and the dev bridge, each applying a received host `ConfigSync`; the overlay menu
//! joins them as a second concurrent writer. A single `Mutex` guards it — contention is negligible
//! (the main thread reads a field briefly each frame; a writer writes only when it receives a sync or
//! the user changes a setting).
//!
//! **Concurrent writers narrow their writes.** With more than one writer, each touches only the
//! fields it owns via [`update`] — the menu writes the toggled field, the co-op client writes the
//! host's *shared* subset — so they don't clobber each other's disjoint changes. Whole-config
//! replacement ([`set`]) is reserved for the lone dev-bridge writer; see those functions' docs.

use std::sync::{Mutex, OnceLock, TryLockError};

use unseamless_core::config::Config;

static LIVE_CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();

/// Initialize the live config from the loaded file config. Called once at install, before any
/// feature ticks; later calls are ignored (first write wins).
pub fn init(config: Config) {
    let _ = LIVE_CONFIG.set(Mutex::new(config));
}

/// Run `f` against the current live config without cloning it — the read path features use each
/// frame. `f` must be **trivial and must not re-enter** this module (the `Mutex` is non-reentrant):
/// the existing callers just copy out a `Copy` field, holding the lock only for that.
pub fn with<R>(f: impl FnOnce(&Config) -> R) -> R {
    match LIVE_CONFIG.get() {
        Some(m) => f(&m.lock().unwrap_or_else(|p| p.into_inner())),
        None => {
            // `init` runs in `app::install` before any feature ticks or the bridge starts, so this
            // is unreachable in practice. Assert it in dev so an ordering regression is loud rather
            // than a silent read of default config (which the "fail loudly" posture wants).
            debug_assert!(false, "state::with called before init()");
            f(&Config::default())
        }
    }
}

/// A full clone of the current live config (for seeding a `Session`, where ownership is needed).
/// Used by the co-op side-channel ([`crate::coop`]) and the dev bridge to seed their `Session`.
pub fn snapshot() -> Config {
    with(Clone::clone)
}

/// A **non-blocking** clone of the live config, for the overlay's Present thread (which must never
/// block on the game thread). `None` if uninitialized or momentarily contended — the caller keeps
/// its last snapshot and redraws from that, so a contended frame doesn't flicker.
pub fn try_snapshot() -> Option<Config> {
    let m = LIVE_CONFIG.get()?;
    match m.try_lock() {
        Ok(c) => Some(c.clone()),
        // Poisoned: recover (a `Config` is a plain value, structurally intact) rather than never
        // drawing the live values again.
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner().clone()),
        // Contended: the game thread holds it briefly — keep the last snapshot this frame.
        Err(TryLockError::WouldBlock) => None,
    }
}

/// Mutate the live config in place under the lock, applying only the fields the caller touches.
/// **This is the write primitive to prefer** once more than one writer exists: each writer narrows
/// its `f` to the subset it owns, so two writers touching *disjoint* fields compose without losing
/// each other's update (the failure mode [`set`] has).
///
/// Concretely, when both the overlay menu and the co-op `ConfigSync` path write concurrently:
/// - the menu does `update(|c| c.gameplay.crit_coop = on)` — only the toggled field;
/// - the co-op client does `update(|c| host_shared.apply_to(c))` — only the host-authoritative
///   *shared* subset ([`SharedSettings::apply_to`](unseamless_core::protocol::SharedSettings::apply_to)),
///   leaving every machine-local field the user set in the menu intact.
///
/// Read-modify-write over the whole struct is no costlier than [`set`] here (the lock is held the
/// same instant), and the read sees the other writer's latest fields. No-op before [`init`].
///
/// `f` runs **while the lock is held**, so it must be infallible and must not re-enter `state`
/// (a [`snapshot`]/`update` call from inside `f` deadlocks this non-reentrant `Mutex`). Every caller
/// today is a simple field write or [`SharedSettings::apply_to`], all infallible — keep it that way:
/// a closure that panics mid-mutation would poison the mutex *and* leave a half-written `Config` that
/// the next reader recovers via `into_inner`, an observable partial write. (Unlike [`set`]'s single
/// move-assign, which can't tear.)
pub fn update(f: impl FnOnce(&mut Config)) {
    if let Some(m) = LIVE_CONFIG.get() {
        f(&mut m.lock().unwrap_or_else(|p| p.into_inner()));
    }
}

/// Replace the **whole** live config. This is last-writer-wins: it overwrites *every* field, so a
/// concurrent writer's update to a disjoint field is lost. Safe only for a **sole** writer pushing a
/// complete config — currently just the dev bridge ([`crate::bridge`]), which applies a whole config
/// it received over loopback and is the only writer in that build. The co-op client and the overlay
/// menu instead narrow via [`update`] so they don't clobber each other. No-op before [`init`].
//
// The dev bridge is the only caller and is `#[cfg(feature = "bridge")]`, so `set` is dead in a
// release (no-bridge) build — keep it defined regardless as the documented whole-config primitive.
#[cfg_attr(not(feature = "bridge"), allow(dead_code))]
pub fn set(config: Config) {
    update(|c| *c = config);
}
