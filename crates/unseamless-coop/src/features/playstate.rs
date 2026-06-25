//! Publishes whether the local player is in active gameplay, for the overlay watermark gating.
//!
//! Each frame it checks `WorldChrMan.main_player`: `Some` once a character is loaded in the world
//! (gameplay, or a pause/inventory/map menu), `None` at the title/main menu, character select, and
//! loading screens. We only test the `Option`'s presence — never dereferencing the `PlayerIns` — so
//! it stays safe across the load/teardown transitions that can leave `ChrIns` pointers half-wired
//! (see CLAUDE.md > safety invariants). The overlay draws its branded corner stamp only while this
//! reads `false`, so the stamp is a title/menu element, never an in-play banner. Writes nothing to
//! the game.

use eldenring::cs::WorldChrMan;

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

    // Default phase (FrameBegin) ticks in menus and at the title screen too, so the flag is correct
    // before any save loads — same as the session observer.

    fn on_frame(&mut self, _tick: Tick) {
        // No WorldChrMan singleton yet (very early boot) reads as "not in gameplay", so the watermark
        // shows until a character is actually loaded.
        let in_gameplay = crate::sdk::with_instance::<WorldChrMan, _>(|w| w.main_player.is_some())
            .unwrap_or(false);
        crate::playstate::set_in_gameplay(in_gameplay);
    }
}
