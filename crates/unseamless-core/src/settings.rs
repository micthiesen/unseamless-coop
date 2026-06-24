//! The settings registry: a declarative description of every user-tunable option, used to drive
//! the in-game menu (and anything else that needs to enumerate/label/edit settings) without
//! hand-writing UI per option.
//!
//! ## Divergence from ERSC (intentional)
//! ERSC exposes settings only through the `.ini` file and a fixed set of item/hotkey actions.
//! We keep the *data* in [`Config`] (serde/TOML) and describe how to *present and edit* each
//! option here, once. Adding an option is: add the [`Config`] field, then add one
//! [`Setting`] entry to [`registry`]. Both the config file and the menu pick it up. See
//! `docs/ARCHITECTURE.md` > Divergences.

use crate::config::{Config, MAX_SCALING_PERCENT, OverheadDisplay};

/// Stable identifier for a setting, used to address it from the menu / over the wire. Discriminant
/// stability matters (it can appear in saved UI state), so keep values fixed and append new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SettingId {
    AllowInvaders = 0,
    DeathDebuffs = 1,
    AllowSummons = 2,
    SkipSplashScreens = 3,
    AppendSteamId = 4,
    AlwaysSpectateOnDeath = 5,
    OverheadDisplay = 6,
    BootMasterVolume = 7,
    EnemyHealth = 8,
    EnemyDamage = 9,
    EnemyPosture = 10,
    BossHealth = 11,
    BossDamage = 12,
    BossPosture = 13,
}

/// How a setting is edited in the menu, plus the get/set glue over [`Config`]. Keeping the
/// accessors as function pointers means a [`Setting`] is plain data — trivially testable and
/// `const`-friendly — while still reading/writing real config fields.
pub enum SettingKind {
    /// On/off.
    Toggle { get: fn(&Config) -> bool, set: fn(&mut Config, bool) },
    /// Integer in `[min, max]` adjusted in `step` increments (e.g. a percentage or 0..=10 volume).
    Range {
        min: u32,
        max: u32,
        step: u32,
        get: fn(&Config) -> u32,
        set: fn(&mut Config, u32),
    },
    /// A cycle through named choices (e.g. the overhead-display modes).
    Choice {
        /// `(wire_value, label)` for each choice, in cycle order. Owned so the list can be
        /// derived from the source enum rather than hand-duplicated as a `'static` literal.
        choices: Vec<(u32, &'static str)>,
        get: fn(&Config) -> u32,
        set: fn(&mut Config, u32),
    },
}

/// One registry entry: identity, label, and how to read/edit it.
pub struct Setting {
    pub id: SettingId,
    pub label: &'static str,
    pub kind: SettingKind,
}

impl Setting {
    /// Current value rendered for display (e.g. `"On"`, `"35%"`, `"Soul level and ping"`).
    pub fn display_value(&self, cfg: &Config) -> String {
        match &self.kind {
            SettingKind::Toggle { get, .. } => if get(cfg) { "On" } else { "Off" }.to_string(),
            SettingKind::Range { get, .. } => get(cfg).to_string(),
            SettingKind::Choice { choices, get, .. } => {
                let v = get(cfg);
                choices
                    .iter()
                    .find(|(cv, _)| *cv == v)
                    .map(|(_, label)| (*label).to_string())
                    .unwrap_or_else(|| v.to_string())
            }
        }
    }

    /// Edit the setting: toggle a [`Toggle`], step a [`Range`], or advance a [`Choice`].
    /// `forward` chooses direction (right/left, or next/prev). Saturates at range bounds and
    /// wraps for choices.
    pub fn adjust(&self, cfg: &mut Config, forward: bool) {
        match &self.kind {
            SettingKind::Toggle { get, set } => {
                let v = get(cfg);
                set(cfg, !v);
            }
            SettingKind::Range { min, max, step, get, set } => {
                let cur = get(cfg);
                let next = if forward {
                    cur.saturating_add(*step).min(*max)
                } else {
                    cur.saturating_sub(*step).max(*min)
                };
                set(cfg, next.clamp(*min, *max));
            }
            SettingKind::Choice { choices, get, set } => {
                if choices.is_empty() {
                    return;
                }
                let cur = get(cfg);
                let idx = choices.iter().position(|(v, _)| *v == cur).unwrap_or(0);
                let len = choices.len();
                let next = if forward { (idx + 1) % len } else { (idx + len - 1) % len };
                set(cfg, choices[next].0);
            }
        }
    }
}

/// Overhead-display choices, genuinely derived from [`OverheadDisplay::ALL`] + its `label()`, so
/// the enum is the single source of truth and adding a variant updates the menu automatically.
fn overhead_choices() -> Vec<(u32, &'static str)> {
    OverheadDisplay::ALL
        .into_iter()
        .map(|d| (d as u32, d.label()))
        .collect()
}

/// The full, ordered registry of tunable settings. Add a new option here (and a `Config` field)
/// and it appears in the config file and the menu with no further wiring.
pub fn registry() -> Vec<Setting> {
    use SettingId::*;
    use SettingKind::*;

    let pct = |get: fn(&Config) -> u32, set: fn(&mut Config, u32)| Range {
        min: 0,
        max: MAX_SCALING_PERCENT,
        step: 5,
        get,
        set,
    };

    vec![
        Setting {
            id: AllowInvaders,
            label: "Allow invaders",
            kind: Toggle {
                get: |c| c.gameplay.allow_invaders,
                set: |c, v| c.gameplay.allow_invaders = v,
            },
        },
        Setting {
            id: DeathDebuffs,
            label: "Death debuffs",
            kind: Toggle {
                get: |c| c.gameplay.death_debuffs,
                set: |c, v| c.gameplay.death_debuffs = v,
            },
        },
        Setting {
            id: AllowSummons,
            label: "Allow summons",
            kind: Toggle {
                get: |c| c.gameplay.allow_summons,
                set: |c, v| c.gameplay.allow_summons = v,
            },
        },
        Setting {
            id: SkipSplashScreens,
            label: "Skip splash screens",
            kind: Toggle {
                get: |c| c.gameplay.skip_splash_screens,
                set: |c, v| c.gameplay.skip_splash_screens = v,
            },
        },
        Setting {
            id: AppendSteamId,
            label: "Append Steam ID to names",
            kind: Toggle {
                get: |c| c.gameplay.append_steam_id,
                set: |c, v| c.gameplay.append_steam_id = v,
            },
        },
        Setting {
            id: AlwaysSpectateOnDeath,
            label: "Always spectate on death",
            kind: Toggle {
                get: |c| c.gameplay.always_spectate_on_death,
                set: |c, v| c.gameplay.always_spectate_on_death = v,
            },
        },
        Setting {
            id: OverheadDisplay,
            label: "Overhead player display",
            kind: Choice {
                choices: overhead_choices(),
                get: |c| c.gameplay.overhead_display as u32,
                set: |c, v| {
                    if let Some(d) =
                        crate::config::OverheadDisplay::ALL.into_iter().find(|d| *d as u32 == v)
                    {
                        c.gameplay.overhead_display = d;
                    }
                },
            },
        },
        Setting {
            id: BootMasterVolume,
            label: "Boot master volume",
            kind: Range {
                min: 0,
                max: 10,
                step: 1,
                get: |c| c.gameplay.default_boot_master_volume as u32,
                set: |c, v| c.gameplay.default_boot_master_volume = v.min(10) as u8,
            },
        },
        Setting {
            id: EnemyHealth,
            label: "Enemy health scaling %",
            kind: pct(|c| c.scaling.enemy_health, |c, v| c.scaling.enemy_health = v),
        },
        Setting {
            id: EnemyDamage,
            label: "Enemy damage scaling %",
            kind: pct(|c| c.scaling.enemy_damage, |c, v| c.scaling.enemy_damage = v),
        },
        Setting {
            id: EnemyPosture,
            label: "Enemy posture scaling %",
            kind: pct(|c| c.scaling.enemy_posture, |c, v| c.scaling.enemy_posture = v),
        },
        Setting {
            id: BossHealth,
            label: "Boss health scaling %",
            kind: pct(|c| c.scaling.boss_health, |c, v| c.scaling.boss_health = v),
        },
        Setting {
            id: BossDamage,
            label: "Boss damage scaling %",
            kind: pct(|c| c.scaling.boss_damage, |c, v| c.scaling.boss_damage = v),
        },
        Setting {
            id: BossPosture,
            label: "Boss posture scaling %",
            kind: pct(|c| c.scaling.boss_posture, |c, v| c.scaling.boss_posture = v),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverheadDisplay;

    #[test]
    fn registry_ids_are_unique_and_complete() {
        let reg = registry();
        let mut ids: Vec<u16> = reg.iter().map(|s| s.id as u16).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate SettingId in registry");
        assert_eq!(n, 14, "registry size changed — update this if you added a setting");
    }

    #[test]
    fn toggle_flips() {
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::AllowInvaders).unwrap();
        let mut cfg = Config::default();
        assert!(cfg.gameplay.allow_invaders);
        s.adjust(&mut cfg, true);
        assert!(!cfg.gameplay.allow_invaders);
        assert_eq!(s.display_value(&cfg), "Off");
    }

    #[test]
    fn range_steps_and_saturates() {
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::EnemyHealth).unwrap();
        let mut cfg = Config::default();
        cfg.scaling.enemy_health = 35;
        s.adjust(&mut cfg, true); // +5
        assert_eq!(cfg.scaling.enemy_health, 40);
        s.adjust(&mut cfg, false); // -5
        assert_eq!(cfg.scaling.enemy_health, 35);

        // Volume range saturates at its own bounds, not the percent bounds.
        let vol = reg.iter().find(|s| s.id == SettingId::BootMasterVolume).unwrap();
        cfg.gameplay.default_boot_master_volume = 10;
        vol.adjust(&mut cfg, true);
        assert_eq!(cfg.gameplay.default_boot_master_volume, 10);
        cfg.gameplay.default_boot_master_volume = 0;
        vol.adjust(&mut cfg, false);
        assert_eq!(cfg.gameplay.default_boot_master_volume, 0);
    }

    #[test]
    fn choice_cycles_both_ways_and_wraps() {
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::OverheadDisplay).unwrap();
        let mut cfg = Config::default();
        assert_eq!(cfg.gameplay.overhead_display, OverheadDisplay::Normal);
        assert_eq!(s.display_value(&cfg), "Normal");
        s.adjust(&mut cfg, false); // wrap backwards from first -> last
        assert_eq!(cfg.gameplay.overhead_display, OverheadDisplay::SoulLevelAndPing);
        s.adjust(&mut cfg, true); // forward wraps back to first
        assert_eq!(cfg.gameplay.overhead_display, OverheadDisplay::Normal);
    }
}
