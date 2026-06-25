//! Death-debuff stacking model: the pure decision logic for ERSC's "Rot Essence" penalty —
//! each death hangs a stacking debuff on the player, cured only by resting at a Site of Grace.
//! Host-tested here; the cdylib ([`coop/features/death_debuffs.rs`]) drives it from the live death
//! and grace signals and applies the matching SpEffect rows. Full design: `docs/DEATH-DEBUFFS.md`.
//!
//! The model is deliberately just a **death counter → active tiers** map. The cdylib treats this as
//! the source of truth for *which* tiers should be on the player and reconciles the live SpEffect
//! list against it each frame (re-applying any the game dropped across a load), so this stays a pure,
//! game-free state machine.

/// The five named debuff tiers, in escalation order. The first death applies [`Emaciation`], the
/// fifth [`Despair`]; the discriminant is the **stack level** (1-based) at which each becomes active.
///
/// [`Emaciation`]: DebuffTier::Emaciation
/// [`Despair`]: DebuffTier::Despair
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
    /// to a SpEffectParam row, the menu/log uses [`label`](DebuffTier::label)).
    pub const ALL: [DebuffTier; 5] = [
        Self::Emaciation,
        Self::Hopelessness,
        Self::Decay,
        Self::Vulnerability,
        Self::Despair,
    ];

    /// The tier that becomes active at 1-based stack `level` (`1..=5`), or `None` outside that range
    /// (`0`, or beyond the deepest tier).
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
}

/// The number of distinct debuff tiers (caps the stack's *visible* effect; deaths past this still
/// count but add no new tier in the current model — see [`DeathDebuffs::on_death`]).
pub const MAX_TIERS: u8 = DebuffTier::ALL.len() as u8;

/// A player's death-debuff stack: a count of deaths since the last grace rest. The active tiers are
/// always the prefix `1..=min(stack, MAX_TIERS)` of [`DebuffTier::ALL`] (cumulative — each death adds
/// the next tier, and the cure removes them all at once).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeathDebuffs {
    stack: u8,
}

impl DeathDebuffs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deaths recorded since the last cure (the raw stack depth, which can exceed [`MAX_TIERS`]).
    pub fn deaths(&self) -> u8 {
        self.stack
    }

    /// How many tiers are currently active: `min(deaths, MAX_TIERS)`.
    pub fn active_tier_count(&self) -> u8 {
        self.stack.min(MAX_TIERS)
    }

    /// The tiers currently active, weakest-first (`1..=active_tier_count`). This is what the cdylib
    /// reconciles the live SpEffect list against each frame.
    pub fn active_tiers(&self) -> impl Iterator<Item = DebuffTier> {
        DebuffTier::ALL.into_iter().take(usize::from(self.active_tier_count()))
    }

    /// Record a death. Returns the tier that *newly* became active, or `None` if this death added no
    /// new tier — either because the stack is already at [`MAX_TIERS`] (deeper deaths still count via
    /// [`deaths`](DeathDebuffs::deaths) but apply no new row in this model) or, defensively, on
    /// saturation. The cdylib still reconciles every active tier each frame, so a `None` return just
    /// means "nothing new to apply", not "nothing should be on the player".
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
    fn first_five_deaths_each_unlock_the_next_tier() {
        let mut d = DeathDebuffs::new();
        assert_eq!(d.on_death(), Some(DebuffTier::Emaciation));
        assert_eq!(d.on_death(), Some(DebuffTier::Hopelessness));
        assert_eq!(d.on_death(), Some(DebuffTier::Decay));
        assert_eq!(d.on_death(), Some(DebuffTier::Vulnerability));
        assert_eq!(d.on_death(), Some(DebuffTier::Despair));
        assert_eq!(d.active_tier_count(), 5);
    }

    #[test]
    fn deaths_past_the_cap_count_but_add_no_new_tier() {
        let mut d = DeathDebuffs::new();
        for _ in 0..MAX_TIERS {
            d.on_death();
        }
        assert_eq!(d.on_death(), None); // 6th death: no new tier
        assert_eq!(d.on_death(), None); // 7th too
        assert_eq!(d.deaths(), 7);
        assert_eq!(d.active_tier_count(), MAX_TIERS);
    }

    #[test]
    fn active_tiers_is_the_cumulative_prefix() {
        let mut d = DeathDebuffs::new();
        d.on_death();
        d.on_death();
        d.on_death();
        let active: Vec<_> = d.active_tiers().collect();
        assert_eq!(active, [DebuffTier::Emaciation, DebuffTier::Hopelessness, DebuffTier::Decay]);
    }

    #[test]
    fn clear_resets_and_reports_whether_anything_was_cured() {
        let mut d = DeathDebuffs::new();
        assert!(!d.clear(), "nothing to cure at zero");
        d.on_death();
        d.on_death();
        assert!(d.clear(), "had a stack to cure");
        assert_eq!(d.deaths(), 0);
        assert_eq!(d.active_tiers().count(), 0);
        // Dying again starts from tier 1.
        assert_eq!(d.on_death(), Some(DebuffTier::Emaciation));
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
    fn saturation_never_panics() {
        let mut d = DeathDebuffs::new();
        for _ in 0..300 {
            d.on_death(); // u8 stack saturates at 255 instead of wrapping
        }
        assert_eq!(d.deaths(), u8::MAX);
        assert_eq!(d.active_tier_count(), MAX_TIERS);
    }
}
