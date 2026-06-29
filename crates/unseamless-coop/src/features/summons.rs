//! Allow summons: let the player call their Spirit Ash ("buddy") summons during a co-op session,
//! which vanilla Elden Ring blocks while another player (a phantom) is present in the world.
//! Host-enforced like [`crit_coop`](super::crit_coop) / [`death_debuffs`](super::death_debuffs) so the
//! whole party agrees on the rule; on by default (`gameplay.allow_summons`). FEATURES.md: "Spirit
//! summons allowed in MP", difficulty E.
//!
//! ## Status: SCAFFOLD — the apply mechanism is RE-pending (the gate is not yet located)
//! The config field, the host-authoritative sync ([`unseamless_core::protocol::SharedSettings`]), the
//! settings registry entry, and the overlay toggle are all already done. What this feature is *for* —
//! actually neutralizing vanilla's "a phantom is in your world ⇒ no Spirit Ash" block — needs the gate
//! to be located first, and that gate is **not** a charted SDK field (see "Why a scaffold" below). So
//! this feature currently only:
//!   1. reads the host-enforced toggle each frame (`crate::state`, so a `ConfigSync` re-applies here
//!      without a rebuild — same live-config contract as every other gameplay feature), and
//!   2. logs the enable/disable edge on the debug channel (silent unless `[debug]` verbosity is on).
//!
//! It performs **no game-state write yet** — deliberately. Flipping the wrong byte in the summon path
//! is a crash or a corrupt session, and the correct lever is unknown, so an inert-but-wired feature is
//! the safe, honest scaffold. [`apply_gate`] is the single seam the confirmed mechanism drops into.
//!
//! ## Why a scaffold (the SDK has no typed gate for this) — RE note, so it's not re-derived blind
//! Investigated the pinned `fromsoftware-rs` SDK (commit `8c67a84`) and `eldenring.exe` statically:
//! - **No charted gate flag.** Unlike `crit_coop` (which clears a typed `CSChrActionFlagModule` bit),
//!   there is no SDK field that reads "Spirit Ash summon allowed in multiplayer". The only
//!   `spirit_ashes_allowed` in the SDK is [`QuickMatchSettings::spirit_ashes_allowed`] — **PvP arena /
//!   quickmatch only** (`self.0 >= 10`), not the open-world co-op gate.
//! - **`SummonBuddyManager`** (`cs/world_chr_man.rs`, reached via `WorldChrMan.summon_buddy_manager`)
//!   models the summon *mechanic* — `request_summon_speffect_id` ("Written by TAE goods consume"),
//!   `active_summon_speffect_id`, `player_has_alive_summon`, `is_within_activation_range`,
//!   `item_use_cooldown_timer` — but exposes **no "allowed in MP" lever**. The block lives *upstream*
//!   of this manager, in the goods-use / EzState condition that decides whether the Spirit Calling Bell
//!   can be rung at all while a co-op partner is present.
//! - **Static landmarks** (via `scripts/re/static.py` over `eldenring.exe`, base `0x140000000`): the
//!   Spirit Ash system is internally "**Buddy**". Net-log RTTI `SummonBuddyLogParams@FromNet@@@CS@@`
//!   and `BuddyLogParams@FromNet@@@CS@@`; EzState event names `BuddyGenerator` / `BuddyGenerate2` /
//!   `BuddyUnsummon` / `BuddyStoneEliminateTargetCalc`; `MultiplayState@CS@@`. None of these is the
//!   gate *condition* itself, and the gate has no distinctive string of its own to xref to — so
//!   locating it needs **dynamic** RE (the project's documented default for unknown game state: the
//!   diagnostic rising-edge observer / a `watch-write.py` HW watchpoint), which is a rig action.
//!
//! ## Double blocker (why this can't be finished here, and what the orchestrator probe must do)
//! Both implementation-RE *and* verification of this gate need a live **"a phantom is in your world"**
//! state — i.e. a working co-op session (rung 3, currently the project frontier, not yet landing
//! in-world). Solo, the bell rings fine, so there is nothing to neutralize and nothing to observe. So:
//! - **Before rung 3 lands**, this feature stays an inert scaffold; there is no solo rig probe that can
//!   exercise the gate.
//! - **Once a phantom can be present**, the orchestrator probe is: with `allow_summons` ON and a
//!   phantom in the world, watch what blocks the bell. Concretely — `watch-write.py` a HW watchpoint on
//!   `SummonBuddyManager.request_summon_speffect_id` (the second field, offset `0x20` from the manager:
//!   it follows `trigger_speffect_to_buddy_map: ChainingMap<i32,i32>` = `DLMap` `0x18` [ZST comparator +
//!   `&'static` allocator 8 + head 8 + size 8] + a `buckets` ptr 8 = `0x20`; re-derive from
//!   `size_of::<ChainingMap<i32,i32>>()` if the SDK layout shifts) and ring the bell
//!   solo (write fires) vs. with a phantom present (write suppressed / different path); the suppressing
//!   instruction names the gate. Then the confirmed lever (hold a manager field, clear a SpEffect, or an
//!   AOB code patch via `coop/app::apply_boot_patches`) drops into [`apply_gate`].
//!
//! Clean-room: everything above is behavioral observation in our own words + public-SDK field names —
//! no upstream `ersc.dll` bytes, no decompiler output (CLAUDE.md > Clean-room hygiene).
//!
//! [`QuickMatchSettings::spirit_ashes_allowed`]: eldenring::cs::QuickMatchSettings::spirit_ashes_allowed

use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct Summons {
    /// Announce the enable/disable edge once (debug log), not every frame.
    latch: Latch<bool>,
}

impl Summons {
    pub fn new() -> Self {
        Self::default()
    }

    /// The single seam the confirmed gate mechanism drops into, once the rig probe (see module docs)
    /// locates it. Called every frame while the feature is enabled; must be cheap and self-healing
    /// (the summon manager re-initializes when a session forms / a map loads), mirroring how
    /// [`seamless`](super::seamless) holds `disable_multiplay_restriction`.
    ///
    /// **Currently inert by design** — the lever is unknown and a blind write to the summon path risks
    /// a crash or a corrupt session. No-op until RE lands; returns nothing so the call site stays a
    /// stable one-liner when it's filled in.
    fn apply_gate(&mut self) {
        // RE-PENDING: hold the located gate to "summons allowed" here. See module docs for the probe
        // recipe and why it's blocked on rung-3 co-op being live.
    }
}

impl Feature for Summons {
    fn name(&self) -> &'static str {
        "summons"
    }

    // Default phase (`FrameBegin`): this is session-config-shaped (hold a lever so the party agrees),
    // not frame-order-sensitive world state — the same reasoning (and the same "omit the override")
    // as `seamless`/`session_limit`. If the confirmed mechanism turns out to need ordering against a
    // specific game write (e.g. clearing a per-frame SpEffect like `crit_coop` does in
    // `WorldChrMan_PostPhysics`), override `phase()` then.

    fn on_frame(&mut self, _tick: Tick) {
        // Host-enforced toggle (synced via SharedSettings). Read the live config each frame so a
        // ConfigSync from the host re-applies without rebuilding the feature. Announce only on the
        // enable/disable edge (debug, silent unless `[debug]` verbosity is on — never an info!/per-frame
        // log on the hot path, per CLAUDE.md > Logging rule).
        let enabled = crate::state::with(|c| c.gameplay.allow_summons);
        if self.latch.changed(&enabled) {
            log::debug!(
                "summons {} (apply RE-pending — see features::summons docs)",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        if !enabled {
            return;
        }
        self.apply_gate();
    }
}
