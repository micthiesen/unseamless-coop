//! Process-global **live config**, shared between the game-thread features and the dev bridge.
//!
//! Features read this each frame instead of holding a construction-time snapshot, so a config change
//! from any source takes effect without rebuilding them. The writers are the co-op side-channel
//! ([`crate::coop`]) and the dev bridge, each applying a received host `ConfigSync`; later the menu
//! joins them. A single `Mutex` guards it — contention is negligible (the main thread reads a field
//! briefly each frame; a writer writes only when it receives a sync).

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

/// Replace the **whole** live config — e.g. after the co-op client (or the dev bridge) applies a
/// received host `ConfigSync`. No-op before [`init`]. These writers are last-writer-wins with no
/// race (each runs on its own single thread); when a second concurrent writer lands (the menu), this
/// should narrow to the changed fields to avoid a lost update. Poison recovery is safe because the
/// critical section is a single move-assign — a panic can't leave `Config` half-written.
pub fn set(config: Config) {
    if let Some(m) = LIVE_CONFIG.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = config;
    }
}
