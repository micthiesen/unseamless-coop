//! Death debuffs: ERSC's stacking death penalty, driven by the live death + grace signals and
//! applied as SpEffects on the local player. The stacking *decision* is the host-tested
//! [`unseamless_core::death_debuffs`] model; this feature is the binding — it detects the signals,
//! advances the model, and reconciles the player's SpEffects against it. Design: `docs/DEATH-DEBUFFS.md`.
//!
//! ## Status: detection scaffold (two rig-RE blanks)
//! The death edge (HP≤0) and the model wiring are real and rig-observable today. Two values still
//! need a (solo) rig pass before the debuff actually *applies*, and are marked `RIG-TODO` below:
//! 1. **`GRACE_REST_FLAG`** — the event flag that rises when the player rests at a Site of Grace
//!    (discoverable with the ER Debug Tool's flag tab). `0` ⇒ grace-cure disabled.
//! 2. **`TIER_ROW_IDS`** — our own `SpEffectParam` row ids per tier, written at install (the one open
//!    question is whether `SoloParamRepository` supports *adding* rows vs only mutating). `0` ⇒ that
//!    tier applies nothing, so an undefined/colliding id is never sent to the game.
//!
//! Until those are filled in the feature is **inert and silent in a shipping build** (it only
//! `debug!`-logs the detected signals, which the rig diag build surfaces). Once they're set it lights
//! up: applies/removes the rows and toasts the milestones.

use eldenring::cs::{CSEventFlagMan, CSTaskGroupIndex};
use fromsoftware_shared::Subclass;
use unseamless_core::death_debuffs::DeathDebuffs;
use unseamless_core::util::{Edge, Transition};

use crate::feature::{Feature, Tick};

/// RIG-TODO: event flag that rises on a grace rest (the cure trigger). `0` = unknown ⇒ cure disabled.
const GRACE_REST_FLAG: u32 = 0;

/// RIG-TODO: our `SpEffectParam` row id per tier (index 0 = Emaciation … 4 = Despair). `0` = not yet
/// defined ⇒ that tier applies nothing (so we never send a bogus/colliding id to the game).
const TIER_ROW_IDS: [i32; 5] = [0; 5];

/// Whether the SpEffect rows have been defined — gates the user-facing toasts and real application so
/// the half-built feature stays silent until the rig pass lands.
fn rows_defined() -> bool {
    TIER_ROW_IDS.iter().any(|&id| id != 0)
}

pub struct DeathDebuffsFeature {
    stack: DeathDebuffs,
    /// Rising edge of "the player is dead" (HP≤0), so we count one death, not one per dead frame.
    dead_edge: Edge,
    /// Rising edge of the grace-rest flag (the cure).
    grace_edge: Edge,
}

impl DeathDebuffsFeature {
    pub fn new() -> Self {
        Self { stack: DeathDebuffs::default(), dead_edge: Edge::new(), grace_edge: Edge::new() }
    }
}

impl Feature for DeathDebuffsFeature {
    fn name(&self) -> &'static str {
        "death-debuffs"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Read HP after the game has written it this frame (the project's PostPhysics worked example).
        CSTaskGroupIndex::WorldChrMan_PostPhysics
    }

    fn on_frame(&mut self, _tick: Tick) {
        // Live config: the toggle (synced) and the tuning. Off ⇒ do nothing this frame.
        let (enabled, tuning) = crate::state::with(|c| (c.gameplay.death_debuffs, c.death_debuffs));
        if !enabled {
            return;
        }
        self.stack.set_tuning(tuning);

        // Death edge — rising edge of HP≤0 on the active main player.
        if let Some(dead) = player_is_dead()
            && self.dead_edge.update(dead) == Transition::Rising
        {
            self.on_death();
        }

        // Grace edge — rising edge of the rest flag (global, readable without a player).
        if GRACE_REST_FLAG != 0 {
            let rested =
                crate::sdk::with_instance::<CSEventFlagMan, _>(|m| m.virtual_memory_flag.get_flag(GRACE_REST_FLAG))
                    .unwrap_or(false);
            if self.grace_edge.update(rested) == Transition::Rising {
                self.on_grace();
            }
        }
    }
}

impl DeathDebuffsFeature {
    fn on_death(&mut self) {
        let new_tier = self.stack.on_death();
        log::debug!(
            "death debuffs: death #{} -> {} tier(s) active, intensity {:.2}",
            self.stack.deaths(),
            self.stack.active_tier_count(),
            self.stack.intensity(),
        );
        self.reconcile();
        // Only announce a real effect (rows defined); otherwise stay silent (scaffold).
        if rows_defined()
            && let Some(tier) = new_tier
        {
            crate::notify::with_mut(|n| n.info(format!("Death debuff: {}", tier.label())));
        }
    }

    fn on_grace(&mut self) {
        if !self.stack.clear() {
            return;
        }
        log::debug!("death debuffs: cured at grace");
        for id in TIER_ROW_IDS {
            if id != 0 {
                crate::sdk::remove_speffect_from_main_player(id);
            }
        }
        if rows_defined() {
            crate::notify::with_mut(|n| n.info("Death debuffs cured"));
        }
    }

    /// Ensure every active tier's row is on the player. Idempotent — re-applying is how we self-heal
    /// after a load drops an effect (the live SpEffect list is the source of truth; our counter is a
    /// cache). The per-tier `SpEffectParam` rate values come from the host-tested
    /// [`DebuffTier::rates`](unseamless_core::death_debuffs::DebuffTier::rates), scaled by the current
    /// intensity, so deaths past the tier cap keep biting.
    ///
    /// RIG-TODO: write `rates` onto each tier's row via `SoloParamRepository` before applying it (the
    /// rows + their ids are the rig blank). Until then this logs the intended values so a rig run can
    /// see exactly what each tier would write.
    fn reconcile(&mut self) {
        let intensity = self.stack.intensity();
        for tier in self.stack.active_tiers() {
            let rates = tier.rates(intensity);
            let id = TIER_ROW_IDS[usize::from(tier.level() - 1)];
            log::debug!("death debuffs: tier {} @intensity {intensity:.2} -> {rates:?} (row {id})", tier.label());
            if id != 0 {
                crate::sdk::apply_speffect_to_main_player(id, true);
            }
        }
    }
}

/// `Some(true)` if the active main player's HP is ≤ 0 (dead), `Some(false)` if alive, `None` if there's
/// no active player to read. The `max_hp > 0` guard avoids a false death on a pre-init frame where both
/// are 0. (Rig caveat: HP can also hit ≤0 in scripted/cutscene cases — confirm/debounce on the rig.)
fn player_is_dead() -> Option<bool> {
    crate::sdk::with_active_main_player(|p| {
        let data = &p.superclass().modules.data;
        data.max_hp > 0 && data.hp <= 0
    })
}
