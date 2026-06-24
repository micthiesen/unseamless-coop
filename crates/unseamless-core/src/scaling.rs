//! Per-player enemy/boss scaling math.
//!
//! Co-op makes fights easier by adding players, so the host scales enemy stats up with party
//! size. Each configured value is a percentage *added per extra player* beyond the host: with
//! `enemy_health = 35`, a 2-player party gives enemies ×1.35 health, 3 players ×1.70, etc. A
//! solo host (1 player) always gets ×1.0 — vanilla.
//!
//! This is pure arithmetic on [`Scaling`] so it's fully unit-tested on the host; the cdylib
//! just multiplies the relevant param rows by these factors on the host's machine.

use crate::config::Scaling;

/// Multipliers for one enemy category (enemy or boss).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatMultipliers {
    pub health: f32,
    pub damage: f32,
    pub posture: f32,
}

impl StatMultipliers {
    /// The identity (vanilla) multipliers.
    pub const IDENTITY: Self = Self { health: 1.0, damage: 1.0, posture: 1.0 };
}

/// Multiplier for one `per_player_percent` value at a given party size.
///
/// `players` is the total connected count; the host alone (`<= 1`) yields `1.0`.
pub fn multiplier(per_player_percent: u32, players: u32) -> f32 {
    let extra = players.saturating_sub(1);
    1.0 + (per_player_percent as f32 / 100.0) * extra as f32
}

/// Apply a multiplier to an integer stat (e.g. a param's HP field), rounding to nearest.
/// Saturates into `i32` and never goes below zero.
pub fn scale_i32(base: i32, mult: f32) -> i32 {
    let scaled = (base as f32 * mult).round();
    scaled.clamp(0.0, i32::MAX as f32) as i32
}

impl Scaling {
    /// Multipliers for regular (non-boss) enemies at the given party size.
    pub fn enemy_multipliers(&self, players: u32) -> StatMultipliers {
        StatMultipliers {
            health: multiplier(self.enemy_health, players),
            damage: multiplier(self.enemy_damage, players),
            posture: multiplier(self.enemy_posture, players),
        }
    }

    /// Multipliers for bosses at the given party size.
    pub fn boss_multipliers(&self, players: u32) -> StatMultipliers {
        StatMultipliers {
            health: multiplier(self.boss_health, players),
            damage: multiplier(self.boss_damage, players),
            posture: multiplier(self.boss_posture, players),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaling() -> Scaling {
        // ERSC defaults.
        Scaling {
            enemy_health: 35,
            enemy_damage: 0,
            enemy_posture: 15,
            boss_health: 100,
            boss_damage: 0,
            boss_posture: 20,
        }
    }

    #[test]
    fn solo_host_is_vanilla() {
        assert_eq!(scaling().enemy_multipliers(1), StatMultipliers::IDENTITY);
        assert_eq!(scaling().boss_multipliers(1), StatMultipliers::IDENTITY);
        // 0 players (degenerate) also clamps to identity, not negative.
        assert_eq!(multiplier(35, 0), 1.0);
    }

    #[test]
    fn adds_per_extra_player() {
        // enemy health 35%: 2 players -> 1.35, 3 -> 1.70, 4 -> 2.05
        assert_eq!(scaling().enemy_multipliers(2).health, 1.35);
        assert!((scaling().enemy_multipliers(3).health - 1.70).abs() < 1e-6);
        assert!((scaling().enemy_multipliers(4).health - 2.05).abs() < 1e-6);
        // boss health 100%: 3 players -> 3.0
        assert_eq!(scaling().boss_multipliers(3).health, 3.0);
    }

    #[test]
    fn zero_percent_stays_vanilla_at_any_size() {
        assert_eq!(scaling().enemy_multipliers(4).damage, 1.0);
        assert_eq!(scaling().boss_multipliers(6).damage, 1.0);
    }

    #[test]
    fn scale_i32_rounds_and_floors_at_zero() {
        assert_eq!(scale_i32(1000, 1.35), 1350);
        assert_eq!(scale_i32(1000, 1.704), 1704);
        assert_eq!(scale_i32(3, 1.35), 4); // 4.05 -> 4
        assert_eq!(scale_i32(0, 3.0), 0);
        assert_eq!(scale_i32(100, 0.0), 0);
    }

    #[test]
    fn scale_i32_saturates_and_never_goes_negative() {
        // Large product saturates to i32::MAX rather than wrapping/panicking.
        assert_eq!(scale_i32(i32::MAX, 2.0), i32::MAX);
        assert_eq!(scale_i32(10_000_000, 1000.0), i32::MAX); // 1e10 > i32::MAX -> saturates
        // Negative base clamps to 0 (the doc contract), not a negative stat.
        assert_eq!(scale_i32(-100, 1.0), 0);
        // Worst-case config: max percent (1000) at a large party still saturates cleanly.
        let m = multiplier(1000, 64);
        assert_eq!(scale_i32(i32::MAX, m), i32::MAX);
    }
}
