//! Death-debuff stacking model: the pure decision logic for ERSC's "Rot Essence" penalty —
//! each death hangs a stacking debuff on the player, cured only by resting at a Site of Grace.
//! Host-tested here; the cdylib ([`coop/features/death_debuffs.rs`]) drives it from the live death
//! and grace signals and applies the matching SpEffect rows. Full design: `docs/DEATH-DEBUFFS.md`.
//!
//! The model maps a **death counter** to two outputs the cdylib realizes on the player:
//! - [`active_tiers`](DeathDebuffs::active_tiers): which of the five named tiers are on (cumulative,
//!   one new tier per death up to the configured cap), and
//! - [`intensity`](DeathDebuffs::intensity): a potency multiplier the cdylib scales each tier's
//!   SpEffectParam fields by, so deaths *past* the tier cap keep getting worse instead of plateauing.
//!
//! It stays a pure, game-free state machine: the cdylib treats it as the source of truth for what
//! should be on the player and reconciles the live SpEffect list against it each frame (re-applying
//! anything the game dropped across a load). All behavior is config-driven ([`DeathDebuffTuning`])
//! with reasonable defaults.

use serde::{Deserialize, Serialize};

/// The five named debuff tiers, in escalation order. The first death applies [`Emaciation`], later
/// deaths add the next tier up to the configured cap; the discriminant is the **stack level**
/// (1-based) at which each becomes active.
///
/// [`Emaciation`]: DebuffTier::Emaciation
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum DebuffTier {
    /// Reduced stamina recovery.
    Emaciation = 1,
    /// Reduced max HP / FP / stamina.
    Hopelessness = 2,
    /// Reduced rune acquisition + item discovery.
    Decay = 3,
    /// Reduced defence.
    Vulnerability = 4,
    /// Reduced attack.
    Despair = 5,
}

impl DebuffTier {
    /// All tiers, weakest-first — the single source of truth for the tier set (the cdylib maps each
    /// to a SpEffectParam row; the menu/log uses [`label`](DebuffTier::label)).
    pub const ALL: [DebuffTier; 5] = [
        Self::Emaciation,
        Self::Hopelessness,
        Self::Decay,
        Self::Vulnerability,
        Self::Despair,
    ];

    /// The tier that becomes active at 1-based stack `level` (`1..=5`), or `None` outside that range.
    pub fn from_level(level: u8) -> Option<DebuffTier> {
        Self::ALL.get(usize::from(level.checked_sub(1)?)).copied()
    }

    /// 1-based stack level at which this tier becomes active (`Emaciation` = 1 … `Despair` = 5).
    pub fn level(self) -> u8 {
        self as u8
    }

    /// Human-readable name (ERSC's locale tier names). Single source for menu/log text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Emaciation => "Emaciation",
            Self::Hopelessness => "Hopelessness",
            Self::Decay => "Decay",
            Self::Vulnerability => "Vulnerability",
            Self::Despair => "Despair",
        }
    }

    /// The concrete `SpEffectParam` rate-field values this tier writes, scaled by `intensity`
    /// (`1.0` = base potency; the death-stack's [`intensity`](DeathDebuffs::intensity) grows it past
    /// the tier cap). Each tier touches only its own fields; the rest stay neutral
    /// ([`SpEffectRates::NEUTRAL`]). The cdylib writes these onto the tier's SpEffectParam row.
    ///
    /// Base magnitudes are reasonable starting points to **confirm/tune on the rig** (the exact feel,
    /// and the `stamina_recover_change_speed` units, are gameplay choices); the *shape* — which fields,
    /// scaled how — is what's pinned and host-tested here. Penalty = `BASE * intensity`, with
    /// multiplicative rates floored so a high configured intensity can't invert a stat.
    pub fn rates(self, intensity: f32) -> SpEffectRates {
        // Per-tier base penalties at intensity 1.0 (rig-tunable). Multiplicative ones are expressed as
        // the fraction subtracted from 1.0; the stamina one is the game's additive recovery delta.
        const HP_DROP: f32 = 0.10; // Hopelessness: -10% max HP/FP/stamina
        const STAMINA_RECOVER_DELTA: f32 = -30.0; // Emaciation: slower stamina regen (units: rig-confirm)
        const GAIN_DROP: f32 = 0.10; // Decay: -10% rune gain + item discovery
        const DEFENCE_DROP: f32 = 0.10; // Vulnerability: -10% defence (take more damage)
        const ATTACK_DROP: f32 = 0.10; // Despair: -10% outgoing damage

        let i = intensity.max(0.0);
        // A multiplicative rate of `1.0 - base*i`, floored so it can't reach 0/negative at high intensity.
        let down = |base: f32| (1.0 - base * i).max(0.1);

        let mut r = SpEffectRates::NEUTRAL;
        match self {
            Self::Emaciation => r.stamina_recover_change_speed = STAMINA_RECOVER_DELTA * i,
            Self::Hopelessness => {
                r.max_hp_rate = down(HP_DROP);
                r.max_fp_rate = down(HP_DROP);
                r.max_stamina_rate = down(HP_DROP);
            }
            Self::Decay => {
                r.have_soul_rate = down(GAIN_DROP);
                r.item_drop_rate = down(GAIN_DROP);
            }
            Self::Vulnerability => r.defence_rate = down(DEFENCE_DROP),
            Self::Despair => r.attack_rate = down(ATTACK_DROP),
        }
        r
    }
}

/// The subset of `SP_EFFECT_PARAM_ST` rate fields the death debuffs use, as a host-tested plain value
/// the cdylib writes onto a tier's SpEffectParam row (fields grounded in DEATH-DEBUFFS.md §B and the
/// rate-field briefs in SCALING.md). Multiplicative rates default to `1.0` (no change);
/// `stamina_recover_change_speed` is an additive delta defaulting to `0.0`. `defence_rate`/`attack_rate`
/// are single knobs the binding fans out across the per-element `*_diffence_rate` / `*_attack_rate`
/// families (which the death tiers move together).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpEffectRates {
    pub max_hp_rate: f32,
    pub max_fp_rate: f32,
    pub max_stamina_rate: f32,
    pub stamina_recover_change_speed: f32,
    pub have_soul_rate: f32,
    pub item_drop_rate: f32,
    pub defence_rate: f32,
    pub attack_rate: f32,
}

impl SpEffectRates {
    /// All fields at their no-op values: every multiplicative rate `1.0`, the additive recovery `0.0`.
    pub const NEUTRAL: SpEffectRates = SpEffectRates {
        max_hp_rate: 1.0,
        max_fp_rate: 1.0,
        max_stamina_rate: 1.0,
        stamina_recover_change_speed: 0.0,
        have_soul_rate: 1.0,
        item_drop_rate: 1.0,
        defence_rate: 1.0,
        attack_rate: 1.0,
    };
}

/// The absolute number of distinct tiers (the cap on [`DeathDebuffTuning::max_tiers`]).
pub const MAX_TIERS: u8 = DebuffTier::ALL.len() as u8;

/// Tunable knobs for the stacking algorithm. All have reasonable defaults; the config file's
/// `[death_debuffs]` section is this struct (the on/off toggle is the separate, synced
/// `gameplay.death_debuffs`). Potency is expressed as integer **percent** — matching the `[scaling]`
/// convention, and keeping `Config` `Eq`. Validated/clamped on load (see `config::Config::validate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeathDebuffTuning {
    /// How many distinct tiers can stack (`1..=`[`MAX_TIERS`]). Each death adds the next tier until
    /// this many are active. Default 5 (all named tiers).
    pub max_tiers: u8,
    /// After the tier cap is reached, do further deaths keep intensifying the debuffs (vs. plateau)?
    /// Default `true` — a death streak should keep mattering.
    pub intensify_past_cap: bool,
    /// Potency added per death *beyond* the cap, in percent (e.g. `50` ⇒ +50% per extra death). The
    /// cdylib multiplies each tier's effect magnitude by [`intensity`](DeathDebuffs::intensity).
    /// Default `50`.
    pub intensity_step_percent: u32,
    /// Upper bound on the intensity multiplier, in percent (`100` = 1.0×, the floor). Caps a long
    /// death streak. Default `300` (3.0×). Clamped to at least `100` on load.
    pub max_intensity_percent: u32,
}

impl Default for DeathDebuffTuning {
    fn default() -> Self {
        Self { max_tiers: MAX_TIERS, intensify_past_cap: true, intensity_step_percent: 50, max_intensity_percent: 300 }
    }
}

impl DeathDebuffTuning {
    /// Clamp out-of-range fields into the valid envelope, returning a `(field, message)` for each one
    /// changed (the caller logs them as config warnings). Mirrors the other config clamps.
    pub fn clamp(&mut self) -> Vec<(&'static str, String)> {
        let mut warnings = Vec::new();
        if self.max_tiers < 1 || self.max_tiers > MAX_TIERS {
            let clamped = self.max_tiers.clamp(1, MAX_TIERS);
            warnings.push(("death_debuffs.max_tiers", format!("{} out of range 1..={MAX_TIERS}; clamped to {clamped}", self.max_tiers)));
            self.max_tiers = clamped;
        }
        if self.max_intensity_percent < 100 {
            warnings.push(("death_debuffs.max_intensity_percent", format!("{} below the 100% floor; clamped to 100", self.max_intensity_percent)));
            self.max_intensity_percent = 100;
        }
        warnings
    }
}

/// A player's death-debuff stack: deaths since the last grace rest, plus the tuning that shapes how
/// the count becomes tiers + intensity. The active tiers are always the prefix
/// `1..=min(deaths, max_tiers)` of [`DebuffTier::ALL`] (cumulative — the cure removes them all).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeathDebuffs {
    stack: u8,
    tuning: DeathDebuffTuning,
}

impl Default for DeathDebuffs {
    fn default() -> Self {
        Self::new(DeathDebuffTuning::default())
    }
}

impl DeathDebuffs {
    pub fn new(tuning: DeathDebuffTuning) -> Self {
        Self { stack: 0, tuning }
    }

    /// Replace the tuning (e.g. after a live `ConfigSync`) without disturbing the current stack, so a
    /// changed cap/intensity takes effect on the next death without losing the player's death count.
    pub fn set_tuning(&mut self, tuning: DeathDebuffTuning) {
        self.tuning = tuning;
    }

    /// Deaths recorded since the last cure (the raw stack depth, which can exceed `max_tiers`).
    pub fn deaths(&self) -> u8 {
        self.stack
    }

    /// How many tiers are currently active: `min(deaths, max_tiers)`.
    pub fn active_tier_count(&self) -> u8 {
        self.stack.min(self.tuning.max_tiers)
    }

    /// The tiers currently active, weakest-first (`1..=active_tier_count`). What the cdylib reconciles
    /// the live SpEffect list against each frame.
    pub fn active_tiers(&self) -> impl Iterator<Item = DebuffTier> {
        DebuffTier::ALL.into_iter().take(usize::from(self.active_tier_count()))
    }

    /// The potency multiplier the cdylib scales each active tier's effect magnitude by. `1.0` until
    /// the tier cap is reached; then `1.0 + (deaths - max_tiers) * intensity_step`, capped at
    /// `max_intensity`. Always `1.0` when `intensify_past_cap` is off.
    pub fn intensity(&self) -> f32 {
        let extra = self.stack.saturating_sub(self.tuning.max_tiers);
        if !self.tuning.intensify_past_cap || extra == 0 {
            return 1.0;
        }
        let step = self.tuning.intensity_step_percent as f32 / 100.0;
        let cap = self.tuning.max_intensity_percent.max(100) as f32 / 100.0;
        (1.0 + f32::from(extra) * step).min(cap)
    }

    /// Record a death. Returns the tier that *newly* became active, or `None` if this death added no
    /// new tier (already at the cap — deeper deaths still raise [`intensity`](DeathDebuffs::intensity)
    /// and count via [`deaths`](DeathDebuffs::deaths), but unlock no new row). The caller reconciles
    /// all active tiers + the current intensity afterward regardless.
    pub fn on_death(&mut self) -> Option<DebuffTier> {
        let before = self.active_tier_count();
        self.stack = self.stack.saturating_add(1);
        let after = self.active_tier_count();
        (after > before).then(|| DebuffTier::from_level(after)).flatten()
    }

    /// Cure the stack (rest at a Site of Grace): reset to zero. Returns whether anything was cleared,
    /// so the caller can decide whether to `remove_speffect` and toast a milestone.
    pub fn clear(&mut self) -> bool {
        let had = self.stack > 0;
        self.stack = 0;
        had
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deaths_each_unlock_the_next_tier_up_to_the_cap() {
        let mut d = DeathDebuffs::default();
        assert_eq!(d.on_death(), Some(DebuffTier::Emaciation));
        assert_eq!(d.on_death(), Some(DebuffTier::Hopelessness));
        assert_eq!(d.on_death(), Some(DebuffTier::Decay));
        assert_eq!(d.on_death(), Some(DebuffTier::Vulnerability));
        assert_eq!(d.on_death(), Some(DebuffTier::Despair));
        assert_eq!(d.active_tier_count(), 5);
    }

    #[test]
    fn a_lower_tier_cap_stops_unlocking_early() {
        let tuning = DeathDebuffTuning { max_tiers: 3, ..Default::default() };
        let mut d = DeathDebuffs::new(tuning);
        assert_eq!(d.on_death(), Some(DebuffTier::Emaciation));
        assert_eq!(d.on_death(), Some(DebuffTier::Hopelessness));
        assert_eq!(d.on_death(), Some(DebuffTier::Decay));
        assert_eq!(d.on_death(), None); // capped at 3 tiers
        let active: Vec<_> = d.active_tiers().collect();
        assert_eq!(active, [DebuffTier::Emaciation, DebuffTier::Hopelessness, DebuffTier::Decay]);
    }

    #[test]
    fn intensity_is_flat_until_the_cap_then_grows_and_caps() {
        // Defaults: max_tiers 5, step 0.5, max_intensity 3.0.
        let mut d = DeathDebuffs::default();
        for _ in 0..5 {
            d.on_death();
        }
        assert_eq!(d.intensity(), 1.0, "flat while still unlocking tiers");
        d.on_death(); // 6th: 1 over cap
        assert_eq!(d.intensity(), 1.5);
        d.on_death(); // 7th
        assert_eq!(d.intensity(), 2.0);
        for _ in 0..100 {
            d.on_death();
        }
        assert_eq!(d.intensity(), 3.0, "capped at max_intensity");
    }

    #[test]
    fn intensify_off_keeps_intensity_flat() {
        let tuning = DeathDebuffTuning { intensify_past_cap: false, ..Default::default() };
        let mut d = DeathDebuffs::new(tuning);
        for _ in 0..20 {
            d.on_death();
        }
        assert_eq!(d.intensity(), 1.0);
        assert_eq!(d.active_tier_count(), MAX_TIERS);
    }

    #[test]
    fn clear_resets_and_reports_whether_anything_was_cured() {
        let mut d = DeathDebuffs::default();
        assert!(!d.clear(), "nothing to cure at zero");
        d.on_death();
        d.on_death();
        assert!(d.clear(), "had a stack to cure");
        assert_eq!(d.deaths(), 0);
        assert_eq!(d.active_tiers().count(), 0);
        assert_eq!(d.intensity(), 1.0);
        assert_eq!(d.on_death(), Some(DebuffTier::Emaciation)); // starts from tier 1 again
    }

    #[test]
    fn saturation_never_panics() {
        let mut d = DeathDebuffs::default();
        for _ in 0..300 {
            d.on_death(); // u8 stack saturates at 255 instead of wrapping
        }
        assert_eq!(d.deaths(), u8::MAX);
        assert_eq!(d.active_tier_count(), MAX_TIERS);
    }

    #[test]
    fn tier_level_round_trips() {
        for t in DebuffTier::ALL {
            assert_eq!(DebuffTier::from_level(t.level()), Some(t));
        }
        assert_eq!(DebuffTier::from_level(0), None);
        assert_eq!(DebuffTier::from_level(MAX_TIERS + 1), None);
    }

    #[test]
    fn clamp_pulls_fields_into_range() {
        let mut t = DeathDebuffTuning {
            max_tiers: 9,
            max_intensity_percent: 20,
            intensity_step_percent: 50,
            intensify_past_cap: true,
        };
        let warnings = t.clamp();
        assert_eq!(t.max_tiers, MAX_TIERS);
        assert_eq!(t.max_intensity_percent, 100);
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn clamp_accepts_defaults_without_warnings() {
        assert!(DeathDebuffTuning::default().clamp().is_empty());
    }

    #[test]
    fn each_tier_touches_only_its_own_fields_at_base_intensity() {
        let n = SpEffectRates::NEUTRAL;
        // Hopelessness moves the three max-stat rates and nothing else.
        let h = DebuffTier::Hopelessness.rates(1.0);
        assert_eq!(h.max_hp_rate, 0.9);
        assert_eq!(h.max_fp_rate, 0.9);
        assert_eq!(h.max_stamina_rate, 0.9);
        assert_eq!(h.attack_rate, n.attack_rate, "must not touch attack");
        assert_eq!(h.have_soul_rate, n.have_soul_rate, "must not touch rune gain");

        // Each other tier moves its field(s) and leaves max_hp_rate neutral.
        assert_eq!(DebuffTier::Despair.rates(1.0).attack_rate, 0.9);
        assert_eq!(DebuffTier::Vulnerability.rates(1.0).defence_rate, 0.9);
        assert_eq!(DebuffTier::Decay.rates(1.0).item_drop_rate, 0.9);
        assert!(DebuffTier::Emaciation.rates(1.0).stamina_recover_change_speed < 0.0);
        for t in [DebuffTier::Despair, DebuffTier::Vulnerability, DebuffTier::Decay, DebuffTier::Emaciation] {
            assert_eq!(t.rates(1.0).max_hp_rate, n.max_hp_rate, "{t:?} must not touch max HP");
        }
    }

    #[test]
    fn intensity_deepens_the_penalty_and_floors_multiplicative_rates() {
        // At 2x intensity the −10% HP penalty doubles to −20%.
        assert!((DebuffTier::Hopelessness.rates(2.0).max_hp_rate - 0.8).abs() < 1e-6);
        // The additive stamina penalty scales linearly and stays negative.
        let e1 = DebuffTier::Emaciation.rates(1.0).stamina_recover_change_speed;
        let e2 = DebuffTier::Emaciation.rates(2.0).stamina_recover_change_speed;
        assert!((e2 - 2.0 * e1).abs() < 1e-3);
        // A wild intensity can't invert a stat — multiplicative rates floor at 0.1.
        assert_eq!(DebuffTier::Hopelessness.rates(100.0).max_hp_rate, 0.1);
        assert_eq!(DebuffTier::Despair.rates(100.0).attack_rate, 0.1);
    }
}
