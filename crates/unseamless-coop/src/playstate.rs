//! Process-global "is the local player in active gameplay" flag.
//!
//! A game-thread feature ([`crate::features::playstate`]) publishes this once per frame; the overlay
//! (Present thread) reads it non-blocking to gate the corner watermark. We want the branded
//! version/hint stamp only when **not** controlling the character (title/main menu, character
//! select, loading) — never as a persistent banner while you're playing.
//!
//! The whole shared state is one [`AtomicBool`], so `Relaxed` ordering is correct (no other memory is
//! published through it — same reasoning as `crate::input`'s block flag). Defaults to `false` (not in
//! gameplay) so the watermark shows at the title screen before the first probe tick.

use std::sync::atomic::{AtomicBool, Ordering};

static IN_GAMEPLAY: AtomicBool = AtomicBool::new(false);

/// Publish whether the local player is currently in active gameplay (a character is loaded in the
/// world). Called each frame by the game-thread probe.
pub fn set_in_gameplay(in_gameplay: bool) {
    IN_GAMEPLAY.store(in_gameplay, Ordering::Relaxed);
}

/// Whether the local player is in active gameplay right now. Read by the overlay to gate the
/// watermark (drawn only when this is `false`).
pub fn in_gameplay() -> bool {
    IN_GAMEPLAY.load(Ordering::Relaxed)
}
