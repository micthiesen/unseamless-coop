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
    Config, MAX_MASTER_VOLUME, MAX_NAMEPLATE_DISTANCE, MAX_SCALING_PERCENT, MAX_SESSION_PLAYERS,
    MIN_NAMEPLATE_DISTANCE, MIN_SESSION_PLAYERS, OverheadDisplay,
};

/// Stable identifier for a setting, used to address it from the menu / over the wire. Discriminant
/// stability matters (it can appear in saved UI state), so keep values fixed and append new ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SettingId {
    // 0 was AllowInvaders (removed). Discriminants are stable/append-only (see doc above), so the
    // value is retired rather than reused.
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
    CritCoop = 19,
    BootMasterVolumeEnabled = 20,
    Nameplates = 21,
    NameplateDistance = 22,
    EnableOfflineMultiplayer = 23,
    ForceOnlineMenuMode = 24,
    BypassSessionCreateGate = 25,
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
                | CritCoop
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
            id: CritCoop,
            label: "Crit co-op",
            kind: Toggle {
                get: |c| c.gameplay.crit_coop,
                set: |c, v| c.gameplay.crit_coop = v,
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
            id: EnableOfflineMultiplayer,
            label: "Enable offline multiplayer items",
            kind: Toggle {
                get: |c| c.gameplay.enable_offline_multiplayer,
                set: |c, v| c.gameplay.enable_offline_multiplayer = v,
            },
        },
        Setting {
            id: ForceOnlineMenuMode,
            label: "Force online menu mode (experimental)",
            kind: Toggle {
                get: |c| c.gameplay.force_online_menu_mode,
                set: |c, v| c.gameplay.force_online_menu_mode = v,
            },
        },
        Setting {
            id: BypassSessionCreateGate,
            label: "Bypass session create gate (experimental)",
            kind: Toggle {
                get: |c| c.gameplay.bypass_session_create_gate,
                set: |c, v| c.gameplay.bypass_session_create_gate = v,
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
            id: Nameplates,
            label: "Overhead nameplates",
            kind: Toggle {
                get: |c| c.nameplates.enabled,
                set: |c, v| c.nameplates.enabled = v,
            },
        },
        Setting {
            id: NameplateDistance,
            label: "Nameplate distance (m)",
            kind: Range {
                min: MIN_NAMEPLATE_DISTANCE,
                max: MAX_NAMEPLATE_DISTANCE,
                step: 5,
                get: |c| c.nameplates.max_distance_m,
                set: |c, v| c.nameplates.max_distance_m = v,
            },
        },
        Setting {
            id: BootMasterVolumeEnabled,
            label: "Set master volume on boot",
            kind: Toggle {
                get: |c| c.gameplay.boot_master_volume_enabled,
                set: |c, v| c.gameplay.boot_master_volume_enabled = v,
            },
        },
        Setting {
            id: BootMasterVolume,
            label: "Boot master volume",
            kind: Range {
                min: 0,
                max: MAX_MASTER_VOLUME as u32,
                step: 1,
                get: |c| c.gameplay.boot_master_volume as u32,
                set: |c, v| c.gameplay.boot_master_volume = v.min(MAX_MASTER_VOLUME as u32) as u8,
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
    use std::collections::HashMap;

    /// Mutate `s` so its displayed value is *guaranteed* to change: flip a toggle, advance a choice,
    /// or step a range one increment (up, or down if already at max). Mirrors the production `adjust`
    /// semantics but never no-ops, so the binding audits below can rely on "this moved exactly one
    /// field". Used by the distinctness / projection sweeps.
    fn mutate_for_change(s: &Setting, cfg: &mut Config) {
        match &s.kind {
            SettingKind::Toggle { get, set } => {
                let v = get(cfg);
                set(cfg, !v);
            }
            SettingKind::Choice { .. } => s.adjust(cfg, true),
            SettingKind::Range { min, max, step, get, set } => {
                let cur = get(cfg);
                let up = cur.saturating_add(*step).min(*max);
                let target = if up != cur { up } else { cur.saturating_sub(*step).max(*min) };
                set(cfg, target);
            }
        }
    }

    /// The set of differing leaf paths (e.g. `"scaling.enemy_health"`) between two configs, found by
    /// serializing both to TOML and recursively diffing tables. Lets the audit name the *actual*
    /// config field a setting writes without hand-maintaining a SettingId→field map.
    fn changed_leaf_keys(a: &Config, b: &Config) -> Vec<String> {
        let va = toml::Value::try_from(a).expect("config serializes");
        let vb = toml::Value::try_from(b).expect("config serializes");
        let mut out = Vec::new();
        diff_leaves("", &va, &vb, &mut out);
        out.sort();
        out
    }

    fn diff_leaves(prefix: &str, a: &toml::Value, b: &toml::Value, out: &mut Vec<String>) {
        match (a, b) {
            (toml::Value::Table(ta), toml::Value::Table(tb)) => {
                let mut keys: Vec<&String> = ta.keys().chain(tb.keys()).collect();
                keys.sort();
                keys.dedup();
                for k in keys {
                    let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                    match (ta.get(k), tb.get(k)) {
                        (Some(x), Some(y)) => diff_leaves(&p, x, y, out),
                        _ => out.push(p), // key present on only one side
                    }
                }
            }
            _ => {
                if a != b {
                    out.push(prefix.to_string());
                }
            }
        }
    }

    #[test]
    fn registry_ids_are_unique_and_complete() {
        let reg = registry();
        let mut ids: Vec<u16> = reg.iter().map(|s| s.id as u16).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate SettingId in registry");
        assert_eq!(n, 25, "registry size changed — update this if you added a setting");
    }

    #[test]
    fn nameplate_settings_bind_to_their_own_config_fields() {
        // Guard the two nameplate settings' get/set against a copy-paste pointing at a sibling field.
        let reg = registry();
        let mut cfg = Config::default();

        let toggle = reg.iter().find(|s| s.id == SettingId::Nameplates).unwrap();
        cfg.nameplates.enabled = false;
        cfg.nameplates.max_distance_m = 42; // neighbour sentinel
        toggle.adjust(&mut cfg, true);
        assert!(cfg.nameplates.enabled, "toggle must write nameplates.enabled");
        assert_eq!(cfg.nameplates.max_distance_m, 42, "toggle must not touch the distance");
        assert_eq!(toggle.display_value(&cfg), "On");

        let dist = reg.iter().find(|s| s.id == SettingId::NameplateDistance).unwrap();
        cfg.nameplates.max_distance_m = 60;
        cfg.nameplates.show_self = true; // neighbour sentinel
        dist.adjust(&mut cfg, true); // +5
        assert_eq!(cfg.nameplates.max_distance_m, 65, "distance must write nameplates.max_distance_m");
        assert!(cfg.nameplates.show_self, "distance must not touch show_self");
    }

    #[test]
    fn shared_settings_match_the_wire_subset() {
        use SettingId::*;
        // Exhaustively destructure SharedSettings (no `..`): ADDING a field there is a compile error
        // here until this test and `SettingId::is_shared` are updated — that's what actually ties
        // `is_shared` to the wire subset rather than to a parallel literal.
        let crate::protocol::SharedSettings {
            scaling: _, // expands to the 6 percent settings (enemy/boss × health/damage/posture)
            crit_coop: _,
            death_debuffs: _,
            allow_summons: _,
            roam_anywhere: _,
            max_players: _,
        } = crate::protocol::SharedSettings::from(&Config::default());

        let expected = [
            EnemyHealth, EnemyDamage, EnemyPosture, BossHealth, BossDamage, BossPosture,
            CritCoop, DeathDebuffs, AllowSummons, RoamAnywhere, MaxPlayers,
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
        let s = reg.iter().find(|s| s.id == SettingId::CritCoop).unwrap();
        let mut cfg = Config::default();
        assert!(cfg.gameplay.crit_coop);
        s.adjust(&mut cfg, true);
        assert!(!cfg.gameplay.crit_coop);
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
    fn enable_offline_multiplayer_toggle_binds_to_its_own_config_field() {
        // Guards the EnableOfflineMultiplayer get/set against a copy-paste pointing at a sibling
        // (especially the skip_splash_screens / append_steam_id neighbours it sits between).
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::EnableOfflineMultiplayer).unwrap();
        let mut cfg = Config::default();
        assert!(cfg.gameplay.enable_offline_multiplayer, "must default on");
        cfg.gameplay.skip_splash_screens = true; // neighbour sentinel
        cfg.gameplay.append_steam_id = true; // neighbour sentinel
        s.adjust(&mut cfg, true);
        assert!(!cfg.gameplay.enable_offline_multiplayer, "must write gameplay.enable_offline_multiplayer");
        assert!(cfg.gameplay.skip_splash_screens, "must not touch a neighbouring field");
        assert!(cfg.gameplay.append_steam_id, "must not touch a neighbouring field");
        assert_eq!(s.display_value(&cfg), "Off");
    }

    #[test]
    fn boot_master_volume_enabled_toggle_binds_to_its_own_config_field() {
        // Guards the BootMasterVolumeEnabled get/set against a copy-paste pointing at a sibling — especially the
        // u8 it gates (boot_master_volume) or the neighbour above it (always_spectate_on_death).
        let reg = registry();
        let s = reg.iter().find(|s| s.id == SettingId::BootMasterVolumeEnabled).unwrap();
        let mut cfg = Config::default();
        cfg.gameplay.boot_master_volume_enabled = false;
        cfg.gameplay.always_spectate_on_death = true; // neighbour sentinel, set opposite
        cfg.gameplay.boot_master_volume = 7; // the gated value must be untouched
        s.adjust(&mut cfg, true);
        assert!(cfg.gameplay.boot_master_volume_enabled, "must write gameplay.boot_master_volume_enabled");
        assert!(cfg.gameplay.always_spectate_on_death, "must not touch a neighbouring field");
        assert_eq!(cfg.gameplay.boot_master_volume, 7, "must not touch the gated level");
        assert_eq!(s.display_value(&cfg), "On");
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
        cfg.gameplay.boot_master_volume = 10;
        vol.adjust(&mut cfg, true);
        assert_eq!(cfg.gameplay.boot_master_volume, 10);
        cfg.gameplay.boot_master_volume = 0;
        vol.adjust(&mut cfg, false);
        assert_eq!(cfg.gameplay.boot_master_volume, 0);

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
    fn every_setting_survives_a_mutate_save_reload_round_trip() {
        // Drive a non-default value through EVERY registered setting (via the registry's own get/set
        // glue), persist to TOML, reload, and assert the whole Config round-trips. This ties the
        // registry to serde for the entire surface at once: a setting whose set() writes a field that
        // is `#[serde(skip)]`'d, mis-renamed, or simply absent from the serialized form is caught here
        // — without a hand-written per-field test. Complements the per-field binding tests (which
        // guard get/set wiring) by guarding *persistence* of every mutated value.
        let reg = registry();
        let mut cfg = Config::default();

        for s in &reg {
            let before = s.display_value(&cfg);
            match &s.kind {
                SettingKind::Toggle { get, set } => {
                    let v = get(&cfg);
                    set(&mut cfg, !v); // flip off its default
                }
                SettingKind::Choice { .. } => s.adjust(&mut cfg, true), // advance one choice
                SettingKind::Range { min, max, step, get, set } => {
                    // Land on an in-range, non-default value: step one up (saturating, like the
                    // production `adjust`), or down if already at max.
                    let cur = get(&cfg);
                    let up = cur.saturating_add(*step).min(*max);
                    let target = if up != cur { up } else { cur.saturating_sub(*step).max(*min) };
                    set(&mut cfg, target);
                }
            }
            // The mutation must be observable through the same accessor — for *every* kind. This also
            // keeps the test honest as settings are added: a zero-width Range or a single-variant Choice
            // (whose adjust is a no-op) trips here instead of silently testing nothing.
            assert_ne!(s.display_value(&cfg), before, "{:?}: mutation must change the value", s.id);
        }

        let (reloaded, warnings) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert!(warnings.is_empty(), "mutated-but-in-range config should not warn: {warnings:?}");
        // Total `Config: PartialEq` makes this one assert cover every mutated field at once.
        assert_eq!(reloaded, cfg, "every mutated setting must survive save -> reload unchanged");
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

    // ----- binding-integrity audit (w3-settings-audit) -----

    #[test]
    fn every_setting_binds_to_a_distinct_config_field() {
        // Mutating setting `j` must change ONLY setting `j`'s displayed value — never another's. This
        // is a complete copy-paste guard in one double sweep: a *set* that writes a sibling's field, or
        // a *get* that reads one, both make some `i != j` move in lockstep with `j` and trip the
        // `changed == (i == j)` rule. The diagonal (a setting must move its own value) also catches a
        // get/set wired to different fields. Together: every registry entry reads and writes one
        // distinct real Config field.
        let reg = registry();
        let base = Config::default();
        let baseline: Vec<String> = reg.iter().map(|s| s.display_value(&base)).collect();
        for (j, sj) in reg.iter().enumerate() {
            let mut cfg = base.clone();
            mutate_for_change(sj, &mut cfg);
            for (i, si) in reg.iter().enumerate() {
                let changed = si.display_value(&cfg) != baseline[i];
                assert_eq!(
                    changed,
                    i == j,
                    "mutating {:?} changed {:?}'s value (each setting must bind to its own field)",
                    sj.id,
                    si.id,
                );
            }
        }
    }

    #[test]
    fn each_setting_writes_exactly_one_distinct_config_leaf() {
        // The same distinctness, asserted at the serialized-field level so a failure names the exact
        // colliding key. Each setting's mutation must touch exactly one Config leaf, and no two
        // settings may share a leaf — a precise, diagnosable copy-paste guard for the `set` closures.
        let reg = registry();
        let base = Config::default();
        // Assumes every bound field is serde-visible: a setting binding a `#[serde(skip)]` field would
        // surface here as "wrote []" rather than naming the field — true of the whole registry today.
        let mut owner: HashMap<String, SettingId> = HashMap::new();
        for s in &reg {
            let mut cfg = base.clone();
            mutate_for_change(s, &mut cfg);
            let keys = changed_leaf_keys(&base, &cfg);
            assert_eq!(keys.len(), 1, "{:?} must write exactly one config leaf, wrote {keys:?}", s.id);
            let key = keys.into_iter().next().unwrap();
            if let Some(prev) = owner.insert(key.clone(), s.id) {
                panic!("{prev:?} and {:?} both write config leaf `{key}` (copy-paste bug)", s.id);
            }
        }
    }

    #[test]
    fn range_settings_min_max_default_validate_idempotently() {
        // Every Range's registry bounds must live *inside* the config's accepted range: driving the
        // field to the registry min, max, or default and validating must clamp nothing (no warning,
        // value untouched) — otherwise the menu would let the user pick a value `Config::validate`
        // immediately rejects. And validate must be idempotent on an in-range value (a second pass is
        // a no-op), so re-validating a saved config never drifts it.
        let reg = registry();
        for s in &reg {
            let SettingKind::Range { min, max, get, set, .. } = &s.kind else { continue };
            let default_v = get(&Config::default());
            for &v in &[*min, *max, default_v] {
                let mut cfg = Config::default();
                set(&mut cfg, v);
                // The registry bound must be representable end to end. Asserting `set` round-trips it
                // is load-bearing: several `set` closures clamp on write (boot volume, world-time
                // hour/minute), so without this a registry bound *above* the config-accepted range
                // would be silently absorbed by `set` before `validate` ever saw it and the
                // empty-warnings check below would pass anyway.
                assert_eq!(get(&cfg), v, "{:?}: set did not round-trip registry bound {v}", s.id);
                let w1 = cfg.validate();
                assert!(
                    w1.is_empty(),
                    "{:?} at {v} warned {w1:?} — registry bound escapes the config-accepted range",
                    s.id,
                );
                assert_eq!(get(&cfg), v, "{:?} validate mutated an in-range value", s.id);
                let w2 = cfg.validate();
                assert!(w2.is_empty(), "{:?} validate not idempotent: {w2:?}", s.id);
                assert_eq!(get(&cfg), v, "{:?} second validate drifted the value", s.id);
            }
        }
    }

    #[test]
    fn is_shared_matches_the_actual_sharedsettings_projection() {
        // Tie `SettingId::is_shared` to the *behavior* of `SharedSettings::from`, not to a parallel
        // list: a setting is shared iff mutating it actually changes the projected wire subset. This
        // catches a field added to (or dropped from) the projection without updating `is_shared`,
        // complementing the destructure-based `shared_settings_match_the_wire_subset` above.
        use crate::protocol::SharedSettings;
        let reg = registry();
        let base = Config::default();
        let base_shared = SharedSettings::from(&base);
        for s in &reg {
            let mut cfg = base.clone();
            mutate_for_change(s, &mut cfg);
            let affects_projection = SharedSettings::from(&cfg) != base_shared;
            assert_eq!(
                affects_projection,
                s.id.is_shared(),
                "{:?}: is_shared()={} but mutating it {} the wire projection",
                s.id,
                s.id.is_shared(),
                if affects_projection { "changed" } else { "left unchanged" },
            );
        }
    }

    #[test]
    fn display_value_is_total_across_all_settings_and_kinds() {
        // `display_value` must never panic for any reachable value of any setting. Sweep each one with
        // many adjusts in both directions (covering both toggle states and every choice variant + wrap)
        // and render at each step. The non-trivial branch is the `Choice` `find(...).unwrap_or`
        // fallback, which no real registry choice can hit — exercised explicitly by the synthetic
        // `Setting` below.
        let reg = registry();
        for s in &reg {
            let mut cfg = Config::default();
            let _ = s.display_value(&cfg);
            for _ in 0..40 {
                s.adjust(&mut cfg, true);
                let _ = s.display_value(&cfg);
            }
            for _ in 0..40 {
                s.adjust(&mut cfg, false);
                let _ = s.display_value(&cfg);
            }
        }

        // The Choice fallback branch: a `get` returning a value absent from `choices` must render the
        // raw number rather than panic (guards the `find(...).unwrap_or` in `display_value`).
        let synthetic = Setting {
            id: SettingId::OverheadDisplay,
            label: "synthetic",
            kind: SettingKind::Choice { choices: vec![(0, "zero")], get: |_| 999, set: |_, _| {} },
        };
        assert_eq!(synthetic.display_value(&Config::default()), "999");
    }
}
