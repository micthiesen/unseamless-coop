//! Publishes the current [`GameState`] each frame.
//!
//! Samples two reliable signals — `CSMenuMan` (the UI system is up ⇒ the frontend has come up) and
//! `WorldChrMan.main_player` (a character is loaded in the world) — and hands them to the host-tested
//! [`GameState::classify`]. The result goes to [`crate::playstate`] for the overlay (install timing +
//! watermark gating, Present thread) and any feature that gates on "actually in the game". We only test
//! pointer/`Option` presence — never dereference `main_player` — so it stays safe across the load/
//! teardown transitions that leave `ChrIns` pointers half-wired (CLAUDE.md > safety invariants). Writes
//! nothing to the game.

use eldenring::cs::{CSMenuManImp, WorldChrMan};
use unseamless_core::game_state::{GameSignals, GameState};

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct PlayStateProbe;

impl PlayStateProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Feature for PlayStateProbe {
    fn name(&self) -> &'static str {
        "playstate-probe"
    }

    // Default phase (FrameBegin) ticks in menus and at the title screen too, so the state is correct
    // before any save loads — same as the session observer.

    fn on_frame(&mut self, _tick: Tick) {
        let signals = GameSignals {
            // The UI manager singleton being live ⇒ the frontend is up. `with_instance` returns `None`
            // until then (earliest boot). We only need its existence, so the closure is a no-op.
            menu_system_up: crate::sdk::with_instance::<CSMenuManImp, _>(|_| ()).is_some(),
            // `Some` once a character is loaded in the world; `None` at title / menu / loading. Presence
            // only — never dereferenced (the pointer can be half-wired mid-transition).
            player_in_world: crate::sdk::with_instance::<WorldChrMan, _>(|w| w.main_player.is_some())
                .unwrap_or(false),
        };
        crate::playstate::set(GameState::classify(signals));
    }
}
