//! Death debuffs: ERSC's stacking death penalty, driven by the live death + grace signals and
//! applied as SpEffects on the local player. The stacking *decision* is the host-tested
//! [`unseamless_core::death_debuffs`] model; this feature is the binding — it detects the signals,
//! advances the model, and writes/applies the player's SpEffects to match. Design: `docs/DEATH-DEBUFFS.md`.
//!
//! ## Status: complete, pending the final live-apply rig verify
//! Everything is wired and host-green: the debounced death edge advances the model and
//! [`apply_active_tiers`] stamps + applies each tier's row; a throttled [`self_heal`] re-applies rows
//! the game strips across a load; the grace edge (event flag [`GRACE_REST_FLAG`]) clears the stack and
//! removes every owned row. Both rig blanks are now resolved:
//! 1. **`GRACE_REST_FLAG` = 9000** — rig-confirmed the "resting at a Site of Grace" flag.
//! 2. **`TIER_ROW_IDS` = 7210..7250** — verified-unused existing `SpEffectParam` rows we repurpose
//!    (`get_mut` can only mutate existing rows, not mint new ids — see [`write_tier_row`] /
//!    [`TIER_ROW_IDS`]).
//!
//! Remaining: one short rig pass to confirm the debuff actually *lands* on death and *clears* on
//! resting at grace 9000 (detection + rates were confirmed in earlier passes). A `debug_assertions`
//! build still runs [the probe], which logs the exact rows + rates written and the param census.
//!
//! [`apply_active_tiers`]: DeathDebuffsFeature::apply_active_tiers
//! [`self_heal`]: DeathDebuffsFeature::self_heal
//! [the probe]: DeathDebuffsFeature::probe_log_intended_writes

use eldenring::cs::{CSEventFlagMan, CSTaskGroupIndex, SoloParamRepository, SpEffectParam};
use eldenring::param::SP_EFFECT_PARAM_ST;
use fromsoftware_shared::Subclass;
use unseamless_core::death_debuffs::{
    APPLY_DONT_SYNC, DeathDebounce, DeathDebuffs, DebuffTier, ITEM_DROP_STATE_INFO, PERMANENT_ENDURANCE, SpEffectRates,
};
use unseamless_core::util::{Edge, FrameThrottle, Transition};

use crate::feature::{Feature, Tick};

/// Event flag that rises while the player rests at a Site of Grace (the cure trigger). Rig-confirmed:
/// flag 9000 ("篝火休息中フラグ" / "resting at a Site of Grace" in the public ER event-flag reference)
/// was the only flag in an 8990..9019 scan to flip ON the instant the player rested and off on standing
/// up (9001/9002, its menu/clock-suppression siblings, stayed put). Re-derive after a game update with
/// the `[debug.probes] event_flag_scan_start=8990 count=30` window across a grace rest.
const GRACE_REST_FLAG: u32 = 9000;

/// Our `SpEffectParam` row id per tier (index 0 = Emaciation … 4 = Despair). `0` = not defined ⇒ that
/// tier applies nothing (so we never send a bogus/colliding id to the game).
///
/// We **repurpose existing unused rows** rather than mint new ids: the rig confirmed
/// `SoloParamRepository::get_mut` is a by-id lookup (returns `None` for an absent id), so it can only
/// mutate rows already in the regulation — it cannot add a brand-new id. `7210..=7280` are the
/// "Area Scaling - (Unused)" rows in the public Paramdex names (clean source): they exist in the
/// regulation, are referenced by nothing in vanilla (so overwriting their fields is invisible), and sit
/// in the gap between the **used** area-scaling tiers (`7000..=7200`) and the **used** NG+ rows
/// (`7400+`) — i.e. outside the ranges the scaling feature writes (`docs/SCALING.md`). We take the
/// first five (7260/7270/7280 are spare). `write_tier_row` overwrites all fields we use plus
/// `effect_endurance`, so whatever scaling-param leftovers these rows held don't matter.
///
/// Re-derive after a game update: dump the SpEffectParam census via the feature's `debug_assertions`
/// write-back probe and re-pick verified-unused rows from Paramdex/Smithbox.
const TIER_ROW_IDS: [i32; 5] = [7210, 7220, 7230, 7240, 7250];

/// Vanilla `SpEffectParam` row we copy the buff-bar `icon_id` from (one shared debuff icon for all
/// tiers). `6860` = "Destined Death - (Max HP Debuff)" in the public Paramdex names — its icon is the
/// death/black-blade debuff glyph, on-theme for a *death*-debuff feature. We copy just its `icon_id`
/// (icon ids are `20500 + sheet#`; copying live avoids guessing a number and survives game updates).
/// `-1` (no icon) if the source row is gone after a patch.
///
/// (We do **not** clone the whole row: a rig pass showed cloning 6860's full field set brought its
/// attack-applied behavioral flags, which made the self-applied effect not take / self-evict. So we
/// keep the repurposed row's own working application config and only scrub its baggage — see
/// [`neutralize_side_effects`] — plus copy this icon.)
const DEBUFF_ICON_SOURCE_ID: u32 = 6860;

/// Whether any SpEffect row id is defined (non-zero) — gates the user-facing toasts / self-heal.
/// Defensive: if a game update ever forced all of [`TIER_ROW_IDS`] back to `0` (every tier disabled),
/// the feature would degrade to quiet detection rather than toast a cure that applied nothing.
fn rows_defined() -> bool {
    TIER_ROW_IDS.iter().any(|&id| id != 0)
}

pub struct DeathDebuffsFeature {
    stack: DeathDebuffs,
    /// Debounced "the player really died" detector (HP≤0 sustained), so one death counts once — not
    /// once per dead frame, and not on a transient scripted/cutscene HP dip.
    death: DeathDebounce,
    /// Rising edge of the grace-rest flag (the cure).
    grace_edge: Edge,
    /// Tracks the `gameplay.death_debuffs` toggle so we can clean up on the on→off edge (our debuffs
    /// are permanent, so without this they'd stay stuck on the player after the feature is disabled).
    enabled_edge: Edge,
    /// Set on the disable edge, cleared once cleanup actually reaches a live player. The toggle is
    /// synced, so a disable can land mid-load with no active player to remove from; we retry each frame
    /// until one appears, so the permanent rows can't outlive the toggle.
    pending_disable_cleanup: bool,
    /// Throttles the per-frame self-heal (re-applying rows the game dropped across a load) — a couple
    /// times a second is ample to restore a stripped debuff without per-frame `entries()` reads.
    heal_throttle: FrameThrottle,
    /// `debug_assertions` only: gate the one-time startup probe dump (the static rows+rates table).
    #[cfg(debug_assertions)]
    probe_announced: bool,
    /// `debug_assertions` only: cleared until the param write-back census succeeds (needs a live
    /// `SoloParamRepository`, i.e. in-game — not the title screen), so it runs exactly once.
    #[cfg(debug_assertions)]
    probe_writeback_done: bool,
}

impl DeathDebuffsFeature {
    /// Self-heal cadence: ~2 Hz at 60fps. Catching a load-strip a few hundred ms late is invisible.
    const HEAL_PERIOD: u64 = 30;

    pub fn new() -> Self {
        Self {
            stack: DeathDebuffs::default(),
            death: DeathDebounce::default(),
            grace_edge: Edge::new(),
            enabled_edge: Edge::new(),
            pending_disable_cleanup: false,
            heal_throttle: FrameThrottle::every(Self::HEAL_PERIOD),
            #[cfg(debug_assertions)]
            probe_announced: false,
            #[cfg(debug_assertions)]
            probe_writeback_done: false,
        }
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
        // On the on→off edge, schedule cleanup: our debuffs are permanent (endurance=-1), so disabling
        // the feature must remove any already-applied rows (else they'd stay stuck). Fed every frame so
        // the edge is real.
        if self.enabled_edge.update(enabled) == Transition::Falling {
            self.stack.clear();
            self.pending_disable_cleanup = true;
            log::debug!("death debuffs: disabled — cleanup pending");
        }
        // Run pending disable-cleanup until it actually reaches a live player. A synced disable can land
        // mid-load (no active player to remove from, so the strip would silently no-op and the permanent
        // rows would survive), so retry each frame until a player appears. Runs even while `!enabled`.
        if self.pending_disable_cleanup && self.clear_owned_rows() {
            self.pending_disable_cleanup = false;
            log::debug!("death debuffs: disabled — owned rows removed");
        }
        if !enabled {
            return;
        }
        self.stack.set_tuning(tuning);

        // Diag (debug build only): dump the exact rows + rates we'd write, once, so a rig log reveals
        // the values even before any death and even while the row ids are still blank.
        #[cfg(debug_assertions)]
        {
            if !self.probe_announced {
                self.probe_announced = true;
                self.probe_log_intended_writes("startup");
            }
            // The param census needs a live SoloParamRepository (in-game, not the title screen), so
            // retry while in gameplay until it succeeds, then never again.
            if !self.probe_writeback_done && crate::playstate::in_gameplay() {
                self.probe_writeback_done = self.probe_param_writeback();
            }
        }

        // Death edge — debounced rising edge of HP≤0 on the active main player. Only fed when there's
        // a live player to read; a load/teardown gap leaves the debounce untouched (no false death).
        let mut died_this_frame = false;
        if let Some(dead) = player_is_dead()
            && self.death.update(dead)
        {
            self.on_death();
            died_this_frame = true;
        }

        // Self-heal — throttled: re-apply any active tier's row the game stripped across a load
        // (fast-travel / area change / quitout clear player SpEffects). No-op until rows are defined.
        // Skipped on a death frame: `apply_active_tiers` just (re)applied every row, and self-heal
        // re-applies any it reads as missing — so running both the same frame would double-apply if the
        // engine doesn't surface a just-applied SpEffect in `entries()` synchronously. Waiting one
        // throttle period to heal a freshly-applied row is invisible, and makes correctness independent
        // of that apply-timing assumption.
        if !died_this_frame && rows_defined() && self.heal_throttle.tick() {
            self.self_heal();
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
        let before_intensity = self.stack.intensity();
        let new_tier = self.stack.on_death();
        let after_intensity = self.stack.intensity();
        log::debug!(
            "death debuffs: death #{} -> {} tier(s) active, intensity {after_intensity:.2}",
            self.stack.deaths(),
            self.stack.active_tier_count(),
        );
        #[cfg(debug_assertions)]
        self.probe_log_intended_writes("death");
        self.apply_active_tiers();
        // Toast the milestone (rows-defined so we never announce a no-op): a newly-unlocked tier, or —
        // for deaths past the tier cap (new_tier None) — the intensify step, so the feedback doesn't
        // just go silent on a death streak. No toast when intensity is plateaued (at max, or
        // intensify-off): nothing changed worth announcing.
        if rows_defined() {
            if let Some(tier) = new_tier {
                crate::notify::with_mut(|n| n.info(format!("Afflicted by {}", tier.label())));
            } else if after_intensity > before_intensity {
                crate::notify::with_mut(|n| n.info("Your afflictions deepen"));
            }
        }
    }

    fn on_grace(&mut self) {
        let had = self.stack.clear();
        // ALWAYS strip owned rows (idempotent + cheap), not just when there was a stack: this is the
        // backstop that cleans up an orphan left permanent by a disable that landed mid-load (stack
        // already 0, yet rows still on the player). Only the toast is gated on whether a real cure
        // happened, so a no-op grace rest stays silent.
        self.clear_owned_rows();
        if had {
            log::debug!("death debuffs: cured at grace");
            if rows_defined() {
                crate::notify::with_mut(|n| n.info("Afflictions cleansed"));
            }
        }
    }

    /// Remove every owned (non-zero) tier row from the local player. Returns whether the cleanup is
    /// **settled**: `true` if there are no owned rows to remove, or a live active player was reached and
    /// stripped; `false` if there was no active player this frame (the disable retry then tries again
    /// next frame). Shared by the grace cure and the disable path.
    fn clear_owned_rows(&self) -> bool {
        if !rows_defined() {
            return true; // nothing owned to remove
        }
        // remove_speffect_from_main_player reports whether a live active player was present; all ids
        // share that state within a frame, so the last non-zero id's result reflects "reached a player".
        let mut reached_player = false;
        for id in TIER_ROW_IDS {
            if id != 0 {
                reached_player = crate::sdk::remove_speffect_from_main_player(id);
            }
        }
        reached_player
    }

    /// Authoritative re-stamp on a death: rewrite every active tier's row at the current intensity and
    /// **remove-then-apply** it, so a changed magnitude (intensity growth past the tier cap) replaces
    /// the prior instance deterministically — correct whether the game's apply-by-id refreshes an
    /// existing SpEffect or stacks a second one (the remove makes both paths equivalent). The per-tier
    /// values come from the host-tested
    /// [`DebuffTier::rates`](unseamless_core::death_debuffs::DebuffTier::rates), scaled by intensity.
    ///
    /// The write + apply are **coupled**: we never `apply_speffect` a row we couldn't stamp, so a row
    /// id wired without a live `SoloParamRepository` can't apply flat vanilla-default fields.
    fn apply_active_tiers(&self) {
        let intensity = self.stack.intensity();
        for tier in self.stack.active_tiers() {
            let rates = tier.rates(intensity);
            let id = TIER_ROW_IDS[usize::from(tier.level() - 1)];
            log::debug!("death debuffs: tier {} @intensity {intensity:.2} -> {rates:?} (row {id})", tier.label());
            if id == 0 {
                continue; // this tier's row id is disabled (0) — apply nothing for it
            }
            if write_tier_row(id, &rates) {
                // Remove first so re-applying at a new magnitude can't compound (see method doc).
                crate::sdk::remove_speffect_from_main_player(id);
                crate::sdk::apply_speffect_to_main_player(id, APPLY_DONT_SYNC);
            } else {
                log::debug!("death debuffs: SpEffectParam row {id} not writable (param repo not live?); skipped apply");
            }
        }
    }

    /// Re-apply any active tier whose row the game has **dropped** — fast-travel, area change, and
    /// quitout clear the player's SpEffects even though our rows are `effect_endurance = -1` permanent,
    /// so without this the debuffs would silently vanish after a load until the next death. We read the
    /// player's live SpEffect list (the source of truth; our counter is a cache) and re-apply **only
    /// the rows that are missing** — so a present row is never re-touched (no stacking, no VFX churn).
    /// Throttled by the caller. No-op when no tiers are active or the row ids aren't defined yet.
    fn self_heal(&self) {
        let intensity = self.stack.intensity();
        // The (tier, row id) pairs we expect on the player, skipping not-yet-defined rows.
        let expected: Vec<(DebuffTier, i32)> = self
            .stack
            .active_tiers()
            .map(|t| (t, TIER_ROW_IDS[usize::from(t.level() - 1)]))
            .filter(|&(_, id)| id != 0)
            .collect();
        if expected.is_empty() {
            return;
        }
        // Read which of our rows are currently live (separate borrow from the apply below — `entries()`
        // borrows `&`, `apply_speffect` needs `&mut`, so we collect ids first, then act).
        let present: Vec<i32> =
            match crate::sdk::with_active_main_player(|p| p.superclass().special_effect.entries().map(|e| e.param_id).collect()) {
                Some(ids) => ids,
                None => return, // no active player to read/heal this frame
            };
        for (tier, id) in expected {
            if present.contains(&id) {
                continue; // still on the player — leave it alone
            }
            log::debug!("death debuffs: self-heal re-applying {} (row {id}) — game dropped it across a load", tier.label());
            if write_tier_row(id, &tier.rates(intensity)) {
                // Remove-then-apply (like apply_active_tiers): if this row was actually still present
                // but not yet visible in entries() (e.g. the frame after a death, before the engine
                // surfaces a just-applied SpEffect), a plain apply would stack a 2nd instance. The
                // remove collapses both the genuinely-missing and the not-yet-visible cases to one.
                crate::sdk::remove_speffect_from_main_player(id);
                crate::sdk::apply_speffect_to_main_player(id, APPLY_DONT_SYNC);
            }
        }
    }

    /// Diag (debug builds only): log the exact `SpEffectParam` rows + rates this feature *would* write
    /// for **every** tier at the current intensity, plus the persistence fields and the current row
    /// ids / grace flag. Runs regardless of whether the blanks are filled, so a single rig run reveals
    /// the concrete values to confirm on the rows — and points at the event-flag scanner for the grace
    /// cure flag. `info!` so it lands in the shared rig log; compiled out of shipping builds.
    #[cfg(debug_assertions)]
    fn probe_log_intended_writes(&self, reason: &str) {
        let intensity = self.stack.intensity();
        log::info!(
            "death-debuffs probe ({reason}): deaths={} active_tiers={} intensity={:.2} rows_defined={}",
            self.stack.deaths(),
            self.stack.active_tier_count(),
            intensity,
            rows_defined(),
        );
        for tier in DebuffTier::ALL {
            let rates = tier.rates(intensity);
            let id = TIER_ROW_IDS[usize::from(tier.level() - 1)];
            let id_str = if id == 0 { "UNDEFINED".to_string() } else { id.to_string() };
            let state_info = if rates.affects_item_drop() { ITEM_DROP_STATE_INFO } else { 0 };
            log::info!(
                "death-debuffs probe:   {} -> row {id_str}: effect_endurance={PERMANENT_ENDURANCE} state_info={state_info} {rates:?}",
                tier.label(),
            );
        }
        log::info!(
            "death-debuffs probe: grace cure flag = {} (0 = UNSET — prime candidate is flag 9000, the \
             public ER event-flag reference's \"resting at a Site of Grace\" flag [9001/9002 are its \
             menu/clock-suppression siblings]; confirm with a NARROW `[debug.probes] event_flag_scan` \
             window over ~8990..9020 while resting at a grace)",
            GRACE_REST_FLAG,
        );
    }

    /// Diag (debug builds only): answer the insert-vs-mutate question for `SoloParamRepository` in one
    /// rig pass. `get_mut::<SpEffectParam>(id)` is a **lookup** — it returns `None` for an absent id, so
    /// it can only mutate rows that already exist, never mint new ones. This probe makes that empirical
    /// and hands the orchestrator the data to choose `TIER_ROW_IDS`:
    /// - a census (count + id range + the highest existing ids) so repurpose-able high rows are visible
    ///   (cross-ref Paramdex for ones that are actually unused),
    /// - a non-destructive write-back to a guaranteed-existing row (write a sentinel, read it back,
    ///   restore in the same frame) proving in-place mutation sticks, and
    /// - `get_mut` on a few high "would-be-new" ids, expected `None` (confirming minting isn't available
    ///   via this API, so `TIER_ROW_IDS` must repurpose existing rows rather than invent fresh ones).
    ///
    /// Runs once in gameplay (needs a live repo). Returns whether the census succeeded, so the caller
    /// stops retrying. `info!` so it lands in the shared rig log; compiled out of shipping builds.
    #[cfg(debug_assertions)]
    fn probe_param_writeback(&self) -> bool {
        // High ids that "shouldn't" be vanilla rows — used to test whether get_mut can address a new id.
        const MINT_CANDIDATES: [u32; 3] = [7_400_000, 9_000_000, 9_999_999];
        crate::sdk::with_instance_mut::<SoloParamRepository, _>(|repo| {
            // Census: rows() yields (id, row) in ascending id order. Collect ids first (the borrow ends
            // before the get_mut calls below).
            let ids: Vec<u32> = repo.rows::<SpEffectParam>().map(|(id, _)| id).collect();
            if ids.is_empty() {
                return false; // repo present but param not populated yet — retry next frame
            }
            // ids is non-empty here, so min/max exist.
            let (min, max) = (ids[0], ids[ids.len() - 1]);
            let top: Vec<u32> = ids.iter().rev().take(12).copied().collect();
            log::info!(
                "death-debuffs probe: SpEffectParam = {} rows, id range {min}..={max}; highest ids {top:?} (pick a verified-unused one to repurpose)",
                ids.len(),
            );
            // Resolved debuff icon: the icon_id we copy onto each tier row. -1 = no icon (source gone).
            // Surfaced so the icon can be eyeballed/swapped on the rig.
            let icon = repo.get::<SpEffectParam>(DEBUFF_ICON_SOURCE_ID).map_or(-1, |src| src.icon_id());
            log::info!("death-debuffs probe: debuff icon_id = {icon} (from source row {DEBUFF_ICON_SOURCE_ID})");
            // Field-by-field diff: clean reference (6860) vs a still-dirty repurposed row (runs once at
            // startup, before any death rewrites the tier row, so it shows the row's original baggage).
            // The differing words pinpoint what to scrub; our in-place neutralize now clears
            // all of them, so this is the confirmation the orchestrator asked for.
            probe_field_diff(repo);
            // Mutate test on a guaranteed-existing row (the max id). Restore immediately — the change is
            // unobserved (same task callback, before any game code runs this frame).
            if let Some(row) = repo.get_mut::<SpEffectParam>(max) {
                let orig = row.effect_endurance();
                let sentinel = orig + 12_345.0;
                row.set_effect_endurance(sentinel);
                let stuck = row.effect_endurance() == sentinel;
                row.set_effect_endurance(orig); // restore
                log::info!("death-debuffs probe: mutate existing row {max}: write-back stuck = {stuck} (restored)");
            }
            // Mint test: get_mut on absent high ids. Some ⇒ the id already exists (collision — pick
            // another); None ⇒ get_mut can't create it, so TIER_ROW_IDS must repurpose an existing row.
            for id in MINT_CANDIDATES {
                let verdict = if repo.get_mut::<SpEffectParam>(id).is_some() {
                    "Some (id already EXISTS — collision, don't repurpose)"
                } else {
                    "None (absent — get_mut can't mint it; repurpose an existing row instead)"
                };
                log::info!("death-debuffs probe: mint candidate id {id}: get_mut -> {verdict}");
            }
            true
        })
        .unwrap_or(false) // repo singleton not live yet — retry next frame
    }
}

/// Scrub the **non-rate-debuff baggage** off a repurposed "Area Scaling - (Unused)" row in place,
/// before we overlay our intended rates. These rows carry leftover scaling/status fields; applying them
/// raw showed ~5 status-buildup meters (poison-like, center-screen). We do this in place (NOT by cloning
/// a clean template — a rig pass showed cloning row 6860 dragged in its attack-applied behavioral flags
/// and the self-applied effect stopped taking), so the row keeps its own working application config and
/// we just zero what shouldn't ride along. Does **not** touch `icon_id` (set after) or the rate fields
/// we overlay. Re-derive via the field-diff probe if new baggage appears after a game update.
fn neutralize_side_effects(row: &mut SP_EFFECT_PARAM_ST) {
    // STATUS-RESISTANCE rates → 1.0 (neutral). THE METER FIX: the unused area-scaling rows set these to
    // ~2.8, and a non-neutral status-resistance rate makes the game show that status's gauge — the
    // rig-observed ~5 buildup meters. The field-diff (probe) pinned these as the only status-family
    // fields that differed from the clean reference row (6860 has them all at 1.0).
    row.set_regist_poizon_change_rate(1.0);
    row.set_regist_disease_change_rate(1.0);
    row.set_regist_blood_change_rate(1.0);
    row.set_regist_curse_change_rate(1.0);
    row.set_regist_freeze_change_rate(1.0);
    row.set_regist_sleep_change_rate(1.0);
    row.set_regist_madness_change_rate(1.0);
    // Leftover area-scaling ATTACK-POWER rates → 1.0. We set the *defence*/*attack* rate families for
    // our debuff, but the scaling rows also carry the separate *_attack_power_rate family (~2.6) which
    // would otherwise buff the player's outgoing damage. (Distinct from our Despair *_attack_rate.)
    row.set_physics_attack_power_rate(1.0);
    row.set_magic_attack_power_rate(1.0);
    row.set_fire_attack_power_rate(1.0);
    row.set_thunder_attack_power_rate(1.0);
    row.set_dark_attack_power_rate(1.0);
    row.set_slash_attack_power_rate(1.0);
    row.set_blow_attack_power_rate(1.0);
    row.set_thrust_attack_power_rate(1.0);
    row.set_neutral_attack_power_rate(1.0);
    row.set_stamina_attack_rate(1.0);
    row.set_sa_attack_power_rate(1.0);
    // Status-buildup the row would inflict (poison/rot[disease]/bleed[blood]/death-blight[curse]/
    // frost[freeze]/sleep/madness) — zero so the debuff applies no status accumulation.
    row.set_poizon_attack_power(0);
    row.set_disease_attack_power(0);
    row.set_blood_attack_power(0);
    row.set_curse_attack_power(0);
    row.set_freeze_attack_power(0);
    row.set_sleep_attack_power(0);
    row.set_madness_attack_power(0);
    // VFX / SFX — `-1` is the "no effect" sentinel. vfx_id(+1..7) drive any leftover particles;
    // the footstep SFX is the other emitter a leftover row can carry.
    row.set_vfx_id(-1);
    row.set_vfx_id1(-1);
    row.set_vfx_id2(-1);
    row.set_vfx_id3(-1);
    row.set_vfx_id4(-1);
    row.set_vfx_id5(-1);
    row.set_vfx_id6(-1);
    row.set_vfx_id7(-1);
    row.set_add_foot_effect_sfx_id(-1);
    // Chained SpEffects / behavior the row could spawn — `-1` = none, so it can't re-introduce a status
    // or effect we just scrubbed.
    row.set_replace_sp_effect_id(-1);
    row.set_cycle_occurrence_sp_effect_id(-1);
    row.set_atk_occurrence_sp_effect_id(-1);
    row.set_spirit_death_sp_effect_id(-1);
    row.set_behavior_id(-1);
}

/// Build tier `id`'s `SpEffectParam` row in place: scrub the repurposed row's baggage
/// ([`neutralize_side_effects`]), copy a themed buff-bar icon, then overlay our `rates` (scaled per
/// intensity) + permanence, so a later `apply_speffect(id)` carries our magnitudes and nothing else.
/// Returns whether the row was written (repo live, id resolved). The single knobs
/// [`SpEffectRates::defence_rate`] / [`SpEffectRates::attack_rate`] fan out across the per-element
/// `*_diffence_rate` / `*_attack_rate` families (which the death tiers move together).
///
/// How the field set was derived: each name maps 1:1 to a `SP_EFFECT_PARAM_ST` `set_*` accessor in the
/// pinned SDK's `param/generated.rs` (struct at line ~53417), per DEATH-DEBUFFS.md §B. `effect_endurance
/// = -1` makes the row permanent; `state_info = 66` is the gate that makes `item_drop_rate` take effect.
///
/// Note: `get_mut::<SpEffectParam>` *only mutates an existing row* — it's a by-id lookup and returns
/// `None` for an absent id (rig-confirmed; the SDK has no row-insertion API at this pin). That's why
/// [`TIER_ROW_IDS`] repurposes already-present unused rows rather than minting new ids; this returning
/// `false` for an unknown id is the safety net (we then skip the apply, never sending a flat row).
fn write_tier_row(id: i32, rates: &SpEffectRates) -> bool {
    if id == 0 {
        return false;
    }
    crate::sdk::with_instance_mut::<SoloParamRepository, _>(|repo| {
        // Copy the buff-bar icon live from a themed vanilla row (immutable borrow ends before get_mut).
        let icon = repo.get::<SpEffectParam>(DEBUFF_ICON_SOURCE_ID).map_or(-1, |src| src.icon_id());
        let Some(row) = repo.get_mut::<SpEffectParam>(id as u32) else {
            return false;
        };
        // Scrub the repurposed row's baggage in place (keeps its own working application flags/category,
        // which a clone of 6860 broke). This is what removes the status-buildup meters + leftover scaling.
        neutralize_side_effects(row);
        // Icon set AFTER neutralize (which leaves icon_id alone) so the debuff shows in the buff bar.
        row.set_icon_id(icon);
        row.set_effect_endurance(PERMANENT_ENDURANCE);
        // Emaciation: additive stamina-recovery delta (game field is i32 — round our f32).
        row.set_stamina_recover_change_speed(rates.stamina_recover_change_speed.round() as i32);
        // Hopelessness: max HP/FP/stamina (FP == "MP" in FromSoft naming).
        row.set_max_hp_rate(rates.max_hp_rate);
        row.set_max_mp_rate(rates.max_fp_rate);
        row.set_max_stamina_rate(rates.max_stamina_rate);
        // Decay: rune gain + item discovery (the latter needs the state_info gate below).
        row.set_have_soul_rate(rates.have_soul_rate);
        row.set_item_drop_rate(rates.item_drop_rate);
        // Set state_info UNCONDITIONALLY: these are repurposed rows, so a pre-existing state_info (which
        // gates special behaviors) must be cleared on the 4 non-item-drop tiers, not left to ride along.
        // Only the item-drop (Decay) tier needs the gate value; everyone else gets 0.
        row.set_state_info(if rates.affects_item_drop() { ITEM_DROP_STATE_INFO } else { 0 });
        // Vulnerability: defence — one knob across the per-element diffence family (FromSoft's spelling).
        row.set_physics_diffence_rate(rates.defence_rate);
        row.set_magic_diffence_rate(rates.defence_rate);
        row.set_fire_diffence_rate(rates.defence_rate);
        row.set_thunder_diffence_rate(rates.defence_rate);
        row.set_dark_diffence_rate(rates.defence_rate);
        // Despair: outgoing attack — one knob across the per-element attack family.
        row.set_physics_attack_rate(rates.attack_rate);
        row.set_magic_attack_rate(rates.attack_rate);
        row.set_fire_attack_rate(rates.attack_rate);
        row.set_thunder_attack_rate(rates.attack_rate);
        row.set_dark_attack_rate(rates.attack_rate);
        true
    })
    .unwrap_or(false)
}

/// Diag (debug builds only): log a field-by-field (4-byte-word) diff between a clean reference debuff
/// row (the icon source, 6860) and a still-dirty repurposed tier row, so a rig log pinpoints exactly
/// which fields carry the baggage [`neutralize_side_effects`] must scrub. Offsets map to fields via the
/// SDK's `SP_EFFECT_PARAM_ST` in `param/generated.rs`. Called once at startup, before any death rewrites
/// the tier row, so the sample is still its original (dirty) self. (This is how the status-resistance
/// meter driver was found: the `regist_*_change_rate` family was the only status-family difference.)
#[cfg(debug_assertions)]
fn probe_field_diff(repo: &SoloParamRepository) {
    const DIRTY_SAMPLE: u32 = TIER_ROW_IDS[1] as u32; // a repurposed tier row (Hopelessness, 7220)
    let (Some(clean), Some(dirty)) =
        (repo.get::<SpEffectParam>(DEBUFF_ICON_SOURCE_ID), repo.get::<SpEffectParam>(DIRTY_SAMPLE))
    else {
        log::info!("death-debuffs probe: field-diff skipped (reference {DEBUFF_ICON_SOURCE_ID} or row {DIRTY_SAMPLE} absent)");
        return;
    };
    let n = std::mem::size_of::<SP_EFFECT_PARAM_ST>();
    // SAFETY: both are live, fully-initialized #[repr(C)] param rows; we only read their bytes.
    let cb = unsafe { std::slice::from_raw_parts((clean as *const SP_EFFECT_PARAM_ST).cast::<u8>(), n) };
    let db = unsafe { std::slice::from_raw_parts((dirty as *const SP_EFFECT_PARAM_ST).cast::<u8>(), n) };
    log::info!("death-debuffs probe: field-diff reference {DEBUFF_ICON_SOURCE_ID} vs dirty {DIRTY_SAMPLE} ({n} bytes) — differing 4-byte words:");
    let mut diffs = 0u32;
    for off in (0..n).step_by(4) {
        if off + 4 > n {
            break;
        }
        let c = [cb[off], cb[off + 1], cb[off + 2], cb[off + 3]];
        let d = [db[off], db[off + 1], db[off + 2], db[off + 3]];
        if c != d {
            diffs += 1;
            log::info!(
                "  @0x{off:03x} ({off}): template i32={} f32={:.3} | dirty i32={} f32={:.3}",
                i32::from_le_bytes(c),
                f32::from_le_bytes(c),
                i32::from_le_bytes(d),
                f32::from_le_bytes(d),
            );
        }
    }
    log::info!("death-debuffs probe: field-diff done — {diffs} differing words (map offsets via SP_EFFECT_PARAM_ST in param/generated.rs)");
}

/// `Some(true)` if the active main player's HP is ≤ 0 (dead), `Some(false)` if alive, `None` if there's
/// no active player to read. The `max_hp > 0` guard avoids a false death on a pre-init frame where both
/// are 0. (The [`DeathDebounce`] absorbs scripted/cutscene HP≤0 dips — confirm the window on the rig.)
fn player_is_dead() -> Option<bool> {
    crate::sdk::with_active_main_player(|p| {
        let data = &p.superclass().modules.data;
        data.max_hp > 0 && data.hp <= 0
    })
}
