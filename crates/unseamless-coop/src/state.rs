//! Process-global **live config**, shared between the game-thread features and the dev bridge.
//!
//! Features read this each frame instead of holding a construction-time snapshot, so a config change
//! from any source takes effect without rebuilding them. Today the only writer is the bridge
//! applying a received `ConfigSync`; later it'll be the menu and the game-P2P side-channel. A single
//! `Mutex` guards it — contention is negligible (the main thread reads a field briefly each frame;
//! the bridge writes only when it receives a sync).

use std::sync::{Mutex, OnceLock};

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
// The live-config write path's consumers are the bridge today and the menu / game-P2P sync next;
// only `bridge` builds exercise it so far, so allow it to be unused without that feature (real dead
// code is still caught in the bridge build, where it's used).
#[cfg_attr(not(feature = "bridge"), allow(dead_code))]
pub fn snapshot() -> Config {
    with(Clone::clone)
}

/// Replace the **whole** live config — e.g. after the bridge applies a received `ConfigSync`. No-op
/// before [`init`]. The bridge is the only writer today, so this is last-writer-wins with no race;
/// when a second writer lands (the menu), this should narrow to the changed fields to avoid a
/// lost update. Poison recovery is safe because the critical section is a single move-assign — a
/// panic can't leave `Config` half-written.
#[cfg_attr(not(feature = "bridge"), allow(dead_code))]
pub fn set(config: Config) {
    if let Some(m) = LIVE_CONFIG.get() {
        *m.lock().unwrap_or_else(|p| p.into_inner()) = config;
    }
}
