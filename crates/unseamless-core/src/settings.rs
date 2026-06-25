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

use crate::config::{
    Config, MAX_SCALING_PERCENT, MAX_SESSION_PLAYERS, MIN_SESSION_PLAYERS, OverheadDisplay,
};

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
    MaxPlayers = 14,
    RoamAnywhere = 15,
    WorldTimeLock = 16,
    WorldTimeHour = 17,
    WorldTimeMinute = 18,
}

impl SettingId {
    /// Whether this setting is part of the host-enforced **shared** subset — the fields the host
    /// syncs across the whole party in co-op (mirrored in [`crate::protocol::SharedSettings`]) — as
    /// opposed to a machine-local preference. The overlay uses this to colour "synced vs local". The
    /// actual projection lives in `SharedSettings::from`; a test pins the two in agreement, so adding
    /// a field there without updating this is caught.
    pub fn is_shared(self) -> bool {
        use SettingId::*;
        matches!(
            self,
            EnemyHealth
                | EnemyDamage
                | EnemyPosture
                | BossHealth
                | BossDamage
                | BossPosture
                | AllowInvaders
                | DeathDebuffs
                | AllowSummons
                | RoamAnywhere
                | MaxPlayers
        )
    }
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
            id: RoamAnywhere,
            label: "Roam anywhere",
            kind: Toggle {
                get: |c| c.gameplay.roam_anywhere,
                set: |c, v| c.gameplay.roam_anywhere = v,
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
        Setting {
            id: MaxPlayers,
            label: "Max players",
            kind: Range {
                min: MIN_SESSION_PLAYERS,
                max: MAX_SESSION_PLAYERS,
                step: 1,
                get: |c| c.session.max_players,
                set: |c, v| c.session.max_players = v,
            },
        },
        Setting {
            id: WorldTimeLock,
            label: "Lock time of day",
            kind: Toggle {
                get: |c| c.world_time.lock,
                set: |c, v| c.world_time.lock = v,
            },
        },
        Setting {
            id: WorldTimeHour,
            label: "Time of day: hour",
            kind: Range {
                min: 0,
                max: 23,
                step: 1,
                get: |c| c.world_time.hour,
                set: |c, v| c.world_time.hour = v.min(23),
            },
        },
        Setting {
            id: WorldTimeMinute,
            label: "Time of day: minute",
            kind: Range {
                min: 0,
                max: 59,
                step: 5,
                get: |c| c.world_time.minute,
                set: |c, v| c.world_time.minute = v.min(59),
            },
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
        assert_eq!(n, 19, "registry size changed — update this if you added a setting");
    }

    #[test]
    fn shared_settings_match_the_wire_subset() {
        use SettingId::*;
        // Exhaustively destructure SharedSettings (no `..`): ADDING a field there is a compile error
        // here until this test and `SettingId::is_shared` are updated — that's what actually ties
        // `is_shared` to the wire subset rather than to a parallel literal.
        let crate::protocol::SharedSettings {
            scaling: _, // expands to the 6 percent settings (enemy/boss × health/damage/posture)
            allow_invaders: _,
            death_debuffs: _,
            allow_summons: _,
            roam_anywhere: _,
            max_players: _,
        } = crate::protocol::SharedSettings::from(&Config::default());

        let expected = [
            EnemyHealth, EnemyDamage, EnemyPosture, BossHealth, BossDamage, BossPosture,
            AllowInvaders, DeathDebuffs, AllowSummons, RoamAnywhere, MaxPlayers,
        ];
        let shared: Vec<SettingId> = registry().iter().map(|s| s.id).filter(|id| id.is_shared()).collect();
        assert_eq!(shared.len(), expected.len(), "shared-setting count drifted from SharedSettings");
        for s in registry() {
            assert_eq!(s.id.is_shared(), expected.contains(&s.id), "{:?} shared flag wrong", s.id);
        }
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
    fn roam_anywhere_toggle_binds_to_its_own_config_field() {
        // Guards the newly-added RoamAnywhere get/set against a copy-paste pointing at a sibling field.
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::RoamAnywhere).unwrap();
        let mut cfg = Config::default();
        cfg.gameplay.roam_anywhere = true;
        cfg.gameplay.allow_summons = false; // a neighbour, set opposite to catch a mis-wired closure
        s.adjust(&mut cfg, true);
        assert!(!cfg.gameplay.roam_anywhere, "must write gameplay.roam_anywhere");
        assert!(!cfg.gameplay.allow_summons, "must not touch a neighbouring field");
        assert_eq!(s.display_value(&cfg), "Off");
    }

    #[test]
    fn world_time_settings_bind_to_their_own_config_fields() {
        // Guard the 3 new settings' get/set against a copy-paste pointing at a sibling field — the
        // count bump alone wouldn't catch a hour/minute/lock mix-up.
        let reg = registry();
        let mut cfg = Config::default();

        let lock = reg.iter().find(|s| s.id == SettingId::WorldTimeLock).unwrap();
        cfg.world_time.lock = false;
        cfg.world_time.hour = 7; // neighbour sentinel
        lock.adjust(&mut cfg, true);
        assert!(cfg.world_time.lock, "lock toggle must write world_time.lock");
        assert_eq!(cfg.world_time.hour, 7, "lock must not touch hour");

        let hour = reg.iter().find(|s| s.id == SettingId::WorldTimeHour).unwrap();
        cfg.world_time.hour = 22;
        cfg.world_time.minute = 30; // neighbour sentinel
        hour.adjust(&mut cfg, true); // +1 -> 23
        assert_eq!(cfg.world_time.hour, 23, "hour must write world_time.hour");
        assert_eq!(cfg.world_time.minute, 30, "hour must not touch minute");
        hour.adjust(&mut cfg, true); // saturates at max 23
        assert_eq!(cfg.world_time.hour, 23, "hour saturates at 23");

        let minute = reg.iter().find(|s| s.id == SettingId::WorldTimeMinute).unwrap();
        cfg.world_time.minute = 50;
        cfg.world_time.hour = 9; // neighbour sentinel
        minute.adjust(&mut cfg, true); // +5 -> 55
        assert_eq!(cfg.world_time.minute, 55, "minute must write world_time.minute");
        assert_eq!(cfg.world_time.hour, 9, "minute must not touch hour");
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

        // Max players is the only Range with a non-zero floor (the SDK sentinel makes 1 invalid),
        // so it must saturate at MIN_SESSION_PLAYERS downward, not walk to 0/1.
        let mp = reg.iter().find(|s| s.id == SettingId::MaxPlayers).unwrap();
        for _ in 0..10 {
            mp.adjust(&mut cfg, false);
        }
        assert_eq!(cfg.session.max_players, crate::config::MIN_SESSION_PLAYERS);
        for _ in 0..10 {
            mp.adjust(&mut cfg, true);
        }
        assert_eq!(cfg.session.max_players, crate::config::MAX_SESSION_PLAYERS);
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
