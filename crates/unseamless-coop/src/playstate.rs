//! Process-global current [`GameState`] — the lifecycle state the rest of the mod gates on.
//!
//! A game-thread probe ([`crate::features::playstate`]) classifies the state once per frame and
//! publishes it here; readers pull it non-blocking from other threads: the overlay (Present thread) to
//! gate its corner watermark *and* to know when the frontend is up so it's safe to install, plus any
//! feature that needs "are we actually in the game" (e.g. host/join gating).
//!
//! The whole shared state is one [`AtomicU8`] holding the [`GameState`] tag, so `Relaxed` ordering is
//! correct (no other memory is published through it — same reasoning as `crate::input`'s block flag).
//! Defaults to [`GameState::Booting`], so before the first probe tick we read "nothing's up yet" rather
//! than a fabricated in-game state.

use std::sync::atomic::{AtomicU8, Ordering};

use unseamless_core::game_state::GameState;

static STATE: AtomicU8 = AtomicU8::new(GameState::Booting as u8);

/// Publish the current lifecycle state. Called each frame by the game-thread probe.
pub fn set(state: GameState) {
    STATE.store(state.as_u8(), Ordering::Relaxed);
}

/// The current lifecycle state, read non-blocking from any thread.
pub fn current() -> GameState {
    GameState::from_u8(STATE.load(Ordering::Relaxed))
}

/// Whether the local player is in active gameplay right now. The overlay's watermark gate (the stamp
/// shows only when this is `false` — title / menu / loading). Thin wrapper over [`current`].
pub fn in_gameplay() -> bool {
    current().in_game()
}
