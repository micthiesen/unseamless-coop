//! Per-player enemy/boss scaling — the binding for the host-tested [`unseamless_core::scaling`] math.
//!
//! Vanilla scales co-op difficulty through a small param chain (full derivation in `docs/SCALING.md`):
//! each NPC names a `MultiPlayCorrectionParam` row; that row selects a `SpEffectParam` by the
//! additional-player count (`client1/2/3_sp_effect_id`); the SpEffect carries the multiplicative
//! *rate* fields (`max_hp_rate`, the `*_attack_power_rate` set, a posture rate). The boss-vs-normal
//! split is encoded purely in *which correction row* an NPC points at — bosses reference rows whose
//! SpEffects multiply harder.
//!
//! So the lever is **overwrite the referenced SpEffect rate rows, once at load** (and self-heal):
//! - idempotent — we write the **absolute** target rate from core for each column's player count,
//!   never read-modify-write, so re-running with the same config/party is a no-op;
//! - reuses the game's own pipeline — we don't pick the column, the game still does that from the
//!   live player count, so populating all three columns scales correctly at every party size;
//! - the enemy/boss split falls out for free — boss rows and enemy rows reference different
//!   SpEffects, so we write boss multipliers into the boss-row SpEffects and enemy into the others.
//!
//! ## Status: live — wired from the rig dump and rig-verified (boss +1 → max_hp 2.0, posture inverted)
//! Both rig unknowns are resolved from the 38-row param dump:
//! 1. **The row map.** [`CORRECTION_ROW_CLASSES`] holds all 38 `MultiPlayCorrectionParam` rows, split
//!    boss vs normal by the SpEffect family each references (vanilla tags it for us). If a row is
//!    missing/unclassified it's simply not touched.
//! 2. **The rate fields.** `max_hp_rate` (HP) and the `*_attack_power_rate` set (damage) write
//!    directly; the posture field is `sa_receive_damage_rate`, **inverted** (vanilla lowers it below
//!    1.0 to make enemies harder to stagger) — see [`apply_rates`]. `1.0` == vanilla, confirmed.
//!
//! Still TODO: a live rig pass to confirm an actual in-world HP/posture change (the dump only proves
//! the static values). The debug-only [`ScalingFeature::probe_param_map`] re-dumps the rows on demand.

use std::collections::HashSet;

use eldenring::cs::{
    CSSessionManager, MultiPlayCorrectionParam, SoloParam, SoloParamRepository, SpEffectParam,
};
use eldenring::param::SP_EFFECT_PARAM_ST;
use unseamless_core::config::Scaling;
use unseamless_core::scaling::{Category, StatMultipliers};
use unseamless_core::util::{Applied, Latch, Timer};

use crate::feature::{Feature, Tick};

/// Classification of each `MultiPlayCorrectionParam` row we scale: `(row_id, Category)`.
///
/// Filled from the rig param dump (38 rows; `~/.local/share/unseamless-fleet/payloads/`). The split is
/// by which SpEffect *family* a row references — vanilla tags it for us:
/// - **Normal**: families 7800/7810 (HP-only) and 7850/7860 on the `10000100`/`10000200` rows.
/// - **Boss**: families 7820/7830 and 7870/7880 on `10000300`/`10000400` (HP up, `atk_power` > 1,
///   `sa_receive` < 1). Vanilla's two boss tiers (7820 vs the stronger 7870) collapse into our single
///   `boss_*` knob set — matching ERSC's one-boss-knob model (a deliberate divergence, not a bug).
///
/// `b_override` (0/1/2) correlates (normal = 1, boss = 0/2) but is **not** the classifier — we key off
/// the rate family and leave `b_override` untouched, so the game's application semantics stay vanilla.
/// Many rows share one family; [`stamp`] dedups by SpEffect id, so a family is written once per stamp.
#[rustfmt::skip]
const CORRECTION_ROW_CLASSES: &[(u32, Category)] = &[
    // Normal (enemy) rows — families 7800 / 7810 / 7850 / 7860.
    (0, Category::Enemy), (100, Category::Enemy), (200, Category::Enemy),
    (10000100, Category::Enemy), (10000200, Category::Enemy),
    // Boss rows — families 7820 / 7830 / 7870 / 7880.
    (300, Category::Boss), (400, Category::Boss),
    (203000, Category::Boss), (203100, Category::Boss), (211000, Category::Boss),
    (212000, Category::Boss), (213000, Category::Boss), (219000, Category::Boss),
    (219200, Category::Boss), (305000, Category::Boss), (325000, Category::Boss),
    (325100, Category::Boss), (325200, Category::Boss), (356000, Category::Boss),
    (357000, Category::Boss), (451000, Category::Boss), (452000, Category::Boss),
    (465000, Category::Boss), (467000, Category::Boss), (468000, Category::Boss),
    (471000, Category::Boss), (472000, Category::Boss), (472100, Category::Boss),
    (473000, Category::Boss), (475000, Category::Boss), (476000, Category::Boss),
    (480000, Category::Boss), (491000, Category::Boss), (495000, Category::Boss),
    (496000, Category::Boss), (498000, Category::Boss),
    (10000300, Category::Boss), (10000400, Category::Boss),
];

/// How often to re-stamp the rows as a self-heal, in case the game reloads regulation params on a
/// map/regulation event (the rates are static data, so this only matters if something resets them).
const SELF_HEAL_SECS: f32 = 10.0;

/// Whether any correction rows are classified — gates all writes (and the real-effect logging). The
/// map is populated and rig-verified; this is now a defensive fallback (if it were ever emptied the
/// feature announces inert once rather than misbehaving).
fn rows_defined() -> bool {
    !CORRECTION_ROW_CLASSES.is_empty()
}

/// `true` when param `P`'s holder is wired (its res cap exists). **Load guard for every
/// `SoloParamRepository` access.** The SDK's `get`/`get_mut`/`rows` funnel through `get_param_file`,
/// which `.expect()`s the holder's res cap and so **panics** before regulation params finish loading
/// (e.g. at the title screen). The shipped profile is now `panic = "unwind"`, so the per-feature
/// `catch_unwind` firewall would *catch* such a panic — but it would also disable scaling for the rest
/// of the session, so we still must never reach that path: a caught panic means no scaling at all,
/// whereas this guard just skips the one frame and resumes once params load. Check the holder is
/// populated first and skip the frame otherwise. The check itself is panic-free (`solo_param_holders.get`
/// / `get_res_cap` are fully fallible). `P::INDEX` is the [`SoloParam`] repository index (77 for
/// MultiPlayCorrectionParam).
fn param_loaded<P: SoloParam>(repo: &SoloParamRepository) -> bool {
    repo.solo_param_holders.get(P::INDEX as usize).and_then(|h| h.get_res_cap(0)).is_some()
}

/// Both params this feature reads/writes are loaded — the single gate `stamp` and the probe check
/// before any (panicking) repository access. See [`param_loaded`].
fn params_ready(repo: &SoloParamRepository) -> bool {
    param_loaded::<MultiPlayCorrectionParam>(repo) && param_loaded::<SpEffectParam>(repo)
}

pub struct ScalingFeature {
    /// Last scaling config we stamped, so a host `ConfigSync` (or a hand edit) re-stamps and announces.
    last_config: Latch<Scaling>,
    /// Last connected-player count, so a join/leave re-stamps as a self-heal edge. The writes are
    /// count-independent (we populate all three client columns), so this is a *trigger*, not an input
    /// to the rates — which is also what keeps us robust against the player-count off-by-one below.
    last_count: Latch<usize>,
    /// Periodic self-heal so a param reload doesn't leave vanilla rates in place between config changes.
    heal: Timer,
    /// One-time inert note, so a shipping log shows the feature is waiting on the rig dump (not broken).
    inert_announced: bool,
    /// Debug-only one-shot guard for [`probe_param_map`](Self::probe_param_map): set once the dump lands.
    #[cfg(debug_assertions)]
    probed: bool,
}

impl ScalingFeature {
    pub fn new() -> Self {
        Self {
            last_config: Latch::new(),
            last_count: Latch::new(),
            heal: Timer::every_secs(SELF_HEAL_SECS),
            inert_announced: false,
            #[cfg(debug_assertions)]
            probed: false,
        }
    }
}

impl Feature for ScalingFeature {
    fn name(&self) -> &'static str {
        "scaling"
    }

    // Default phase (`FrameBegin`): this is static param data, not frame-ordered live state. Writing
    // before a session forms is harmless — the rates are read when a SpEffect is applied to a spawned
    // enemy, and the game picks the client column from its own live player count.

    fn on_frame(&mut self, tick: Tick) {
        // The rig dump tool: enumerate the correction rows + candidate rate fields once the param repo
        // is live. Debug builds only (the diag/rig build), so it never costs a shipping player anything.
        #[cfg(debug_assertions)]
        self.probe_param_map();

        if !rows_defined() {
            // Inert until the orchestrator fills CORRECTION_ROW_CLASSES from the rig dump above. Say so
            // once so a shared log reads as "waiting on rig", not "silently doing nothing".
            if !self.inert_announced {
                self.inert_announced = true;
                log::debug!(
                    "scaling: inert — no MultiPlayCorrectionParam rows classified yet (rig dump pending)"
                );
            }
            return;
        }

        // Live config (so a synced/edited change re-applies without rebuilding) + the connected count.
        let scaling = crate::state::with(|c| c.scaling);
        let players = crate::sdk::with_instance::<CSSessionManager, _>(|s| s.players.len());

        // Decide whether to (re-)stamp *without* mutating the latches yet — we only commit them once a
        // write actually lands (the param repo can be absent pre-init, in which case we retry).
        let config_changed = self.last_config.last() != Some(&scaling);
        let count_changed = matches!(players, Some(p) if self.last_count.last() != Some(&p));
        let heal = self.heal.tick(tick.delta);
        if !(config_changed || count_changed || heal) {
            return;
        }

        let Some(written) = stamp(scaling) else {
            return; // SoloParamRepository not live yet — retry next frame, latches untouched.
        };

        // Announce the config value via the shared hold-a-config policy (info on First/Changed, toast
        // only on a real Changed). Recording happens here, after the write landed.
        let applied = crate::features::announce_held(
            &mut self.last_config,
            scaling,
            || format!("scaling applied to {written} SpEffect row(s): {}", describe(&scaling)),
            || "Co-op scaling updated".to_string(),
        );
        if applied == Applied::Reasserted {
            log::debug!("scaling re-asserted ({written} row(s); party={players:?})");
        }
        if let Some(p) = players {
            self.last_count.classify(&p);
        }
    }
}

/// Stamp every classified correction row's referenced SpEffect rows with the **absolute** rates for
/// each client column's player count. Returns the number of SpEffect rows written, or `None` if the
/// param repository isn't live yet (so the caller retries). Idempotent: re-running with the same
/// config writes the same values.
fn stamp(scaling: Scaling) -> Option<usize> {
    crate::sdk::with_instance_mut::<SoloParamRepository, _>(|repo| {
        // Load guard: skip until regulation params are wired, or the SDK's get/get_mut panic (see
        // `param_loaded`). `None` ⇒ the caller retries next frame, latches untouched.
        if !params_ready(repo) {
            return None;
        }

        // 1. Resolve (sp_effect_id, absolute multipliers) from the classified correction rows. Read
        //    the rows immutably first and collect owned ids/rates, so the immutable borrow is released
        //    before we take the mutable SpEffect borrows below.
        let mut targets: Vec<(i32, StatMultipliers)> = Vec::new();
        for &(row_id, category) in CORRECTION_ROW_CLASSES {
            let Some(row) = repo.get::<MultiPlayCorrectionParam>(row_id) else {
                log::debug!("scaling: MultiPlayCorrectionParam row {row_id} not found; skipping");
                continue;
            };
            // clientN ⇒ N additional players ⇒ N+1 total (docs/SCALING.md). Populate all three columns
            // so the game's own column pick (by live count) lands on the right rate at any party size.
            // A `<= 0` id is the "no SpEffect" sentinel (-1) or row 0 — never write into it.
            for (extra_players, sp_id) in [
                (1u32, row.client1_sp_effect_id()),
                (2, row.client2_sp_effect_id()),
                (3, row.client3_sp_effect_id()),
            ] {
                if sp_id > 0 {
                    let total_players = extra_players + 1;
                    targets.push((sp_id, scaling.category_multipliers(category, total_players)));
                }
            }
        }

        // 2. Write the absolute rates into each referenced SpEffect row. Dedup by SpEffect id: the 38
        //    correction rows share a handful of families (e.g. 31 boss rows all point to 7820/1/2), and
        //    a given id always carries one column ⇒ one player count ⇒ one category, so writing it once
        //    is sufficient (and keeps `written` = the count of distinct SpEffect rows touched).
        let mut written = 0usize;
        let mut seen: HashSet<i32> = HashSet::new();
        for (sp_id, multipliers) in &targets {
            if !seen.insert(*sp_id) {
                continue;
            }
            match repo.get_mut::<SpEffectParam>(*sp_id as u32) {
                Some(row) => {
                    apply_rates(row, *multipliers);
                    written += 1;
                }
                None => log::debug!("scaling: SpEffectParam {sp_id} (referenced by a correction row) not found; skipping"),
            }
        }
        Some(written)
    })
    .flatten()
}

/// Write the absolute scaling rates onto one SpEffect row. **Sets** each field (no read-modify-write),
/// so the identity multipliers (solo / 0%) write `1.0` = vanilla-equivalent and re-stamping is a no-op.
///
/// Field map confirmed on the rig (`1.0` == vanilla for all):
/// - `max_hp_rate` — HP. Higher = tougher, so our health multiplier writes **directly**.
/// - the elemental `*_attack_power_rate` set — the damage the enemy *deals*. Higher = harder, **direct**.
/// - `sa_receive_damage_rate` — stance/poise damage *received*. Vanilla makes bosses harder to stagger
///   by **lowering** this below 1.0 (rig: boss 0.60/0.35/0.25 at +1/+2/+3; `toughness_damage_cut_rate`
///   stays 1.0 everywhere — we don't touch it). So our posture multiplier (≥ 1.0, "× tougher") is
///   **inverted** into the field: `sa_receive = 1 / posture` (more posture ⇒ less stance damage taken).
fn apply_rates(row: &mut SP_EFFECT_PARAM_ST, m: StatMultipliers) {
    row.set_max_hp_rate(m.health);
    // Damage = the full elemental attack-power-rate set, so every damage type scales together.
    row.set_physics_attack_power_rate(m.damage);
    row.set_magic_attack_power_rate(m.damage);
    row.set_fire_attack_power_rate(m.damage);
    row.set_thunder_attack_power_rate(m.damage);
    row.set_dark_attack_power_rate(m.damage);
    // Posture is inverted (see above). `multiplier()` is always ≥ 1.0, so the guard is defensive only;
    // identity (1.0) ⇒ 1.0 = vanilla.
    let sa_receive = if m.posture > 0.0 { 1.0 / m.posture } else { 1.0 };
    row.set_sa_receive_damage_rate(sa_receive);
}

/// One-line summary of the scaling config for the apply log (so a shared log shows what was applied).
fn describe(s: &Scaling) -> String {
    format!(
        "enemy hp+{}%/dmg+{}%/pos+{}% boss hp+{}%/dmg+{}%/pos+{}% per extra player",
        s.enemy_health, s.enemy_damage, s.enemy_posture, s.boss_health, s.boss_damage, s.boss_posture,
    )
}

#[cfg(debug_assertions)]
impl ScalingFeature {
    /// Debug-only rig dump: enumerate every `MultiPlayCorrectionParam` row with its `client1/2/3`
    /// SpEffect ids, then for each referenced SpEffect log the candidate rate fields' current values.
    /// This is the data the orchestrator needs to fill [`CORRECTION_ROW_CLASSES`] and confirm which
    /// rate fields drive HP/attack/posture (and that `1.0` == vanilla). Runs once, the first in-world
    /// frame with regulation params loaded (gated on [`crate::playstate::in_gameplay`] + [`params_ready`]
    /// so it can't hit the SDK's panicking accessor at the title screen). Gated to `debug_assertions`,
    /// so it's absent from shipping builds.
    fn probe_param_map(&mut self) {
        if self.probed {
            return;
        }
        // Only once confirmed in-world with params loaded: the orchestrator's rig run showed this
        // fired (and panicked) at the title screen, where regulation params aren't wired yet.
        // `in_gameplay` = a player is loaded; `params_ready` (below) is the panic-proof belt to the
        // in-world suspenders, since the SDK accessors panic on an unwired holder regardless of state.
        if !crate::playstate::in_gameplay() {
            return;
        }
        // Collect the correction rows first (immutable borrow), then look up each SpEffect — keeps the
        // borrows non-overlapping and the dump ordered by row id.
        let ran = crate::sdk::with_instance::<SoloParamRepository, _>(|repo| {
            if !params_ready(repo) {
                return false; // params not wired yet — retry next frame.
            }
            let rows: Vec<(u32, i32, i32, i32, u8)> = repo
                .rows::<MultiPlayCorrectionParam>()
                .map(|(id, r)| {
                    (
                        id,
                        r.client1_sp_effect_id(),
                        r.client2_sp_effect_id(),
                        r.client3_sp_effect_id(),
                        r.b_override_sp_effect(),
                    )
                })
                .collect();

            log::info!("scaling-probe: {} MultiPlayCorrectionParam row(s)", rows.len());
            for (id, c1, c2, c3, ovr) in &rows {
                log::info!("scaling-probe: correction row {id}: client1={c1} client2={c2} client3={c3} b_override={ovr}");
                for sp_id in [c1, c2, c3] {
                    if *sp_id > 0
                        && let Some(sp) = repo.get::<SpEffectParam>(*sp_id as u32)
                    {
                        log::info!(
                            "scaling-probe:   speffect {sp_id}: max_hp_rate={:.3} phys_atk_pow_rate={:.3} mag={:.3} fire={:.3} thunder={:.3} dark={:.3} toughness_cut={:.3} sa_receive={:.3}",
                            sp.max_hp_rate(),
                            sp.physics_attack_power_rate(),
                            sp.magic_attack_power_rate(),
                            sp.fire_attack_power_rate(),
                            sp.thunder_attack_power_rate(),
                            sp.dark_attack_power_rate(),
                            sp.toughness_damage_cut_rate(),
                            sp.sa_receive_damage_rate(),
                        );
                    }
                }
            }
            true
        });
        // Only latch the one-shot once it actually ran (repo live AND params wired); else retry.
        if ran == Some(true) {
            self.probed = true;
        }
    }
}
