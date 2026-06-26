//! Crit co-op: let co-op partners damage an enemy during a critical (riposte / backstab / guard
//! counter) animation, instead of the enemy being invulnerable to everyone but the player who
//! landed the crit. Ported from the sibling `er-crit-coop` mod (same author/SDK) into this project's
//! [`Feature`] model. Host-enforced like [`death_debuffs`](super::death_debuffs) so the whole party
//! agrees on the rule; on by default.
//!
//! ## Mechanism (RE note, so it can be re-derived after a game update)
//! Vanilla makes the crit victim invulnerable for the crit window via TAE "Event Type 0, action 67
//! (Invincible excluding Throw Attacks)" — a runtime flag on the enemy's `ChrIns`, not reachable
//! from `regulation.bin` params. The `fromsoftware-rs` SDK exposes it as a typed field
//! (`CSChrActionFlagModule` `action_modifiers_flags::invincible_excluding_throw_attacks_defender`),
//! so the feature just clears it on every open-field enemy each frame; the enemy then stays
//! damageable during the crit.
//!
//! ## Why this phase / why it's safe
//! Registers in `WorldChrMan_PostPhysics` (the project's worked example). Safety here is
//! **frame-ordering, not thread exclusivity**: this phase runs *after* the character behavior update
//! has (re)set the flag for the frame and *before* `DmgMan` reads it later in the same frame, so
//! clearing it makes the enemy damageable for that frame's damage pass. Running inside the game's own
//! scheduled phase (vs a free-running background thread) means we touch `ChrIns` in step with the
//! frame instead of racing the behavior/damage phases that own those writes.
//!
//! Caveat: the flag shares a `u64` word with other action-modifier bits, and the SDK setter is a
//! read-modify-write of that word. That's fine as long as nothing else writes the word during this
//! phase (the behavior phase that set these bits has already run for the frame).
//!
//! Load-status guard: `characters()` yields every entry whose `chr_ins` is `Some` regardless of
//! `ChrSetEntry::chr_load_status`, so across a loading/fast-travel transition it can hand back a
//! mid-init/teardown `ChrIns` whose module pointers aren't wired up — dereferencing
//! `modules.action_flag` on one is a UAF (a segfault, not a catchable panic, so the per-feature
//! `catch_unwind` firewall won't save it). We skip any whose `chr_flags1c8.is_active()` is false
//! before touching `modules`, mirroring `sdk::with_active_main_player`. Rig-TODO: confirm this gate
//! doesn't also skip live enemies you'd want cleared (active combatants should read `Active`).

use eldenring::cs::{CSTaskGroupIndex, WorldChrMan};
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct CritCoop {
    /// Announce enable/disable once (debug log), not every frame.
    latch: Latch<bool>,
    /// Total flags cleared since install, for an occasional debug heartbeat.
    clears: u64,
}

impl CritCoop {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Feature for CritCoop {
    fn name(&self) -> &'static str {
        "crit-coop"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // After the behavior update (which sets the flag) and before `DmgMan` reads it — see module docs.
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        // Host-enforced toggle (synced via SharedSettings). Off ⇒ do nothing this frame, so the flag
        // resumes its vanilla behavior. Announce only on enable/disable (debug, silent by default).
        let enabled = crate::state::with(|c| c.gameplay.crit_coop);
        if self.latch.changed(&enabled) {
            log::debug!("crit co-op {}", if enabled { "enabled" } else { "disabled" });
        }
        if !enabled {
            return;
        }

        // `None` = no WorldChrMan singleton yet (menu / loading); nothing to clear, retry next frame.
        // Mutable: we write the action-modifier flag on each enemy `ChrIns`.
        let _ = crate::sdk::with_instance_mut::<WorldChrMan, _>(|wcm| {
            for chr in wcm.open_field_chr_set.base.characters() {
                // Skip a mid-load/teardown, half-wired `ChrIns` before dereferencing its module
                // pointers: `characters()` yields entries regardless of load status (the CLAUDE.md
                // UAF caveat), and `modules.action_flag` is a double pointer-chase. Mirrors the
                // `chr_flags1c8.is_active()` gate in `sdk::with_active_main_player`.
                if !chr.chr_flags1c8.is_active() {
                    continue;
                }
                let flags = &mut chr.modules.action_flag.action_modifiers_flags;
                if flags.invincible_excluding_throw_attacks_defender() {
                    flags.set_invincible_excluding_throw_attacks_defender(false);
                    self.clears += 1;
                    // Occasional heartbeat only (debug, so silent unless [debug] is on) — never an
                    // info!/per-frame log on the hot path.
                    if self.clears <= 5 || self.clears.is_multiple_of(500) {
                        log::debug!(
                            "crit co-op: cleared crit-invuln on cid={} (total {})",
                            chr.character_id,
                            self.clears,
                        );
                    }
                }
            }
        });
    }
}
