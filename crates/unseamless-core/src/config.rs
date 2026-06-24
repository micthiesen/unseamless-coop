//! Mod configuration: a typed, serde-(de)serializable [`Config`] stored as TOML.
//!
//! ## Divergence from ERSC (intentional)
//! ERSC ships a hand-written `ersc_settings.ini`. We use **TOML + serde** instead: adding an
//! option is just a new struct field (serde handles load/save, unknown keys are ignored so old
//! and new configs interoperate), and the same fields are surfaced in the in-game menu by the
//! [`crate::settings`] registry. We do **not** read ERSC's `.ini` — every player runs our mod,
//! so there's no drop-in-compat requirement (see `docs/ARCHITECTURE.md` > Divergences).
//!
//! Parsing is lenient where it can be: missing fields fall back to defaults (`#[serde(default)]`)
//! and unknown fields are ignored. Values that parse but are out of range are clamped by
//! [`Config::validate`], which reports [`ConfigWarning`]s for the caller to log.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::diagnostics::LogLevel;

/// Upper bound for a per-player scaling percentage. Shared by [`Config::validate`] and the menu
/// (`crate::settings`) so the file and the UI agree on the range.
pub const MAX_SCALING_PERCENT: u32 = 1000;

/// Full mod configuration. Load with [`Config::from_toml_str`]; [`Config::default`] is a fresh
/// install's settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gameplay: Gameplay,
    pub scaling: Scaling,
    pub session: Session,
    pub save: Save,
    pub language: Language,
    pub debug: Debug,
}

/// Debugging / diagnostics. Off by default so normal play does no extra disk or network work
/// (see CLAUDE.md / ARCHITECTURE.md). When `enabled`, logging drops to `level` and, if
/// `forward_to_host`, this client also ships its records to the host for one-place inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Debug {
    pub enabled: bool,
    pub level: LogLevel,
    pub forward_to_host: bool,
}

impl Default for Debug {
    fn default() -> Self {
        Self { enabled: false, level: LogLevel::Info, forward_to_host: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gameplay {
    pub allow_invaders: bool,
    pub death_debuffs: bool,
    pub allow_summons: bool,
    pub overhead_display: OverheadDisplay,
    pub skip_splash_screens: bool,
    pub append_steam_id: bool,
    pub always_spectate_on_death: bool,
    /// Boot master volume, 0 (mute) .. 10 (max). Clamped by [`Config::validate`].
    pub default_boot_master_volume: u8,
}

/// Per-player scaling percentages ("% added per extra player"); see [`crate::scaling`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scaling {
    pub enemy_health: u32,
    pub enemy_damage: u32,
    pub enemy_posture: u32,
    pub boss_health: u32,
    pub boss_damage: u32,
    pub boss_posture: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Co-op session password. Empty = no password.
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Save {
    /// Save-file extension for co-op saves (vanilla is `sl2`); keeps co-op saves separate.
    pub file_extension: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    /// Locale file to force; empty follows the game language. (TOML key: `override`.)
    #[serde(rename = "override")]
    pub override_locale: String,
}

/// What to show above other players' heads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum OverheadDisplay {
    #[default]
    Normal = 0,
    None = 1,
    Ping = 2,
    SoulLevel = 3,
    DeathCount = 4,
    SoulLevelAndPing = 5,
}

impl OverheadDisplay {
    /// All variants in display order — for cycling the value in the menu.
    pub const ALL: [OverheadDisplay; 6] = [
        Self::Normal,
        Self::None,
        Self::Ping,
        Self::SoulLevel,
        Self::DeathCount,
        Self::SoulLevelAndPing,
    ];

    /// Human-readable label. Single source of truth for the menu choice list (see
    /// `crate::settings`), so adding a variant updates the menu automatically.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::None => "None",
            Self::Ping => "Ping",
            Self::SoulLevel => "Soul level",
            Self::DeathCount => "Death count",
            Self::SoulLevelAndPing => "Soul level and ping",
        }
    }
}

impl Default for Gameplay {
    fn default() -> Self {
        Self {
            allow_invaders: true,
            death_debuffs: true,
            allow_summons: true,
            overhead_display: OverheadDisplay::Normal,
            skip_splash_screens: false,
            append_steam_id: false,
            always_spectate_on_death: false,
            default_boot_master_volume: 5,
        }
    }
}

impl Default for Scaling {
    fn default() -> Self {
        Self {
            enemy_health: 35,
            enemy_damage: 0,
            enemy_posture: 15,
            boss_health: 100,
            boss_damage: 0,
            boss_posture: 20,
        }
    }
}

impl Default for Save {
    fn default() -> Self {
        Self { file_extension: "co2".to_string() }
    }
}

/// A non-fatal config issue (out-of-range value that was clamped/replaced). The caller logs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub field: String,
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl Config {
    /// Parse a config from TOML. Missing fields default; unknown fields are ignored (so adding
    /// options stays forward/backward compatible). Returns the validated config plus any
    /// range-clamp warnings. Errors only on genuinely malformed TOML.
    pub fn from_toml_str(text: &str) -> Result<(Config, Vec<ConfigWarning>), toml::de::Error> {
        let mut cfg: Config = toml::from_str(text)?;
        let warnings = cfg.validate();
        Ok((cfg, warnings))
    }

    /// Serialize to pretty TOML suitable for writing a default config file.
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).expect("Config always serializes")
    }

    /// Like [`to_toml_string`](Config::to_toml_string) but with secrets redacted — for the
    /// **shareable** diagnostics log header. The session password is the session's only access
    /// control, so it must never land in a log that gets handed to a host or an assistant.
    pub fn to_redacted_toml_string(&self) -> String {
        let mut redacted = self.clone();
        if !redacted.session.password.is_empty() {
            redacted.session.password = "<redacted>".to_string();
        }
        redacted.to_toml_string()
    }

    /// Clamp/repair out-of-range values in place, reporting what changed.
    pub fn validate(&mut self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();

        // Scaling percentages share their upper bound with the menu, so a hand-edited file can't
        // exceed what the UI allows (and downstream multiplier math stays in a sane range).
        for (name, field) in [
            ("enemy_health", &mut self.scaling.enemy_health),
            ("enemy_damage", &mut self.scaling.enemy_damage),
            ("enemy_posture", &mut self.scaling.enemy_posture),
            ("boss_health", &mut self.scaling.boss_health),
            ("boss_damage", &mut self.scaling.boss_damage),
            ("boss_posture", &mut self.scaling.boss_posture),
        ] {
            if *field > MAX_SCALING_PERCENT {
                warnings.push(ConfigWarning {
                    field: format!("scaling.{name}"),
                    message: format!("{field} exceeds {MAX_SCALING_PERCENT}%; clamped"),
                });
                *field = MAX_SCALING_PERCENT;
            }
        }

        if self.gameplay.default_boot_master_volume > 10 {
            warnings.push(ConfigWarning {
                field: "gameplay.default_boot_master_volume".into(),
                message: format!(
                    "{} out of range 0..=10; clamped to 10",
                    self.gameplay.default_boot_master_volume
                ),
            });
            self.gameplay.default_boot_master_volume = 10;
        }

        let ext = &self.save.file_extension;
        let valid = !ext.is_empty()
            && ext.len() <= 120
            && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if !valid {
            warnings.push(ConfigWarning {
                field: "save.file_extension".into(),
                message: format!("{ext:?} is not 1..=120 alphanumerics; reset to \"co2\""),
            });
            self.save.file_extension = "co2".into();
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_toml() {
        let cfg = Config::default();
        let (reparsed, warnings) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert_eq!(cfg, reparsed);
        assert!(warnings.is_empty(), "default should not warn: {warnings:?}");
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Only one section present; everything else must default.
        let (cfg, warnings) = Config::from_toml_str("[scaling]\nboss_health = 150\n").unwrap();
        assert_eq!(cfg.scaling.boss_health, 150);
        assert_eq!(cfg.scaling.enemy_health, Config::default().scaling.enemy_health);
        assert_eq!(cfg.gameplay, Gameplay::default());
        assert_eq!(cfg.save.file_extension, "co2");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn unknown_keys_are_ignored_for_extensibility() {
        // A config written by a newer build (extra key) still loads on an older one.
        let (cfg, warnings) =
            Config::from_toml_str("[gameplay]\nallow_invaders = false\nfuture_option = 42\n")
                .unwrap();
        assert!(!cfg.gameplay.allow_invaders);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn overhead_display_serializes_as_snake_case() {
        let mut cfg = Config::default();
        cfg.gameplay.overhead_display = OverheadDisplay::SoulLevelAndPing;
        assert!(cfg.to_toml_string().contains("overhead_display = \"soul_level_and_ping\""));
        let (round, _) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert_eq!(round.gameplay.overhead_display, OverheadDisplay::SoulLevelAndPing);
    }

    #[test]
    fn password_and_language_override_persist() {
        let mut cfg = Config::default();
        cfg.session.password = "hunter2".into();
        cfg.language.override_locale = "french".into();
        let (round, _) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert_eq!(round.session.password, "hunter2");
        assert_eq!(round.language.override_locale, "french");
        // TOML key is `override`, not the Rust field name.
        assert!(cfg.to_toml_string().contains("override = \"french\""));
    }

    #[test]
    fn volume_is_clamped_with_warning() {
        let (cfg, warnings) =
            Config::from_toml_str("[gameplay]\ndefault_boot_master_volume = 99\n").unwrap();
        assert_eq!(cfg.gameplay.default_boot_master_volume, 10);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "gameplay.default_boot_master_volume");
    }

    #[test]
    fn invalid_save_extension_reset_with_warning() {
        let (cfg, warnings) =
            Config::from_toml_str("[save]\nfile_extension = \"co.2\"\n").unwrap();
        assert_eq!(cfg.save.file_extension, "co2");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "save.file_extension");
    }

    #[test]
    fn malformed_toml_errors() {
        assert!(Config::from_toml_str("[gameplay\nbroken").is_err());
    }

    #[test]
    fn redacted_toml_hides_password_but_keeps_everything_else() {
        let mut cfg = Config::default();
        cfg.session.password = "hunter2".into();
        cfg.scaling.boss_health = 150;
        let redacted = cfg.to_redacted_toml_string();
        assert!(!redacted.contains("hunter2"), "password leaked: {redacted}");
        assert!(redacted.contains("<redacted>"));
        assert!(redacted.contains("boss_health = 150"));
        // Empty password is left empty (nothing to hide), not turned into "<redacted>".
        assert!(Config::default().to_redacted_toml_string().contains("password = \"\""));
    }

    #[test]
    fn validate_clamps_out_of_range_scaling() {
        let (cfg, warnings) =
            Config::from_toml_str("[scaling]\nboss_health = 5000\nenemy_health = 40\n").unwrap();
        assert_eq!(cfg.scaling.boss_health, super::MAX_SCALING_PERCENT);
        assert_eq!(cfg.scaling.enemy_health, 40); // in-range untouched
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "scaling.boss_health");
    }

    #[test]
    fn volume_boundary_is_exact() {
        // 10 is valid (no warning); 11 clamps.
        let (cfg, w) = Config::from_toml_str("[gameplay]\ndefault_boot_master_volume = 10\n").unwrap();
        assert_eq!(cfg.gameplay.default_boot_master_volume, 10);
        assert!(w.is_empty());
        let (cfg, w) = Config::from_toml_str("[gameplay]\ndefault_boot_master_volume = 11\n").unwrap();
        assert_eq!(cfg.gameplay.default_boot_master_volume, 10);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn save_extension_boundaries() {
        // empty -> reset; 120 chars -> ok; 121 -> reset.
        let (cfg, w) = Config::from_toml_str("[save]\nfile_extension = \"\"\n").unwrap();
        assert_eq!(cfg.save.file_extension, "co2");
        assert_eq!(w.len(), 1);

        let ok = "a".repeat(120);
        let (cfg, w) = Config::from_toml_str(&format!("[save]\nfile_extension = \"{ok}\"\n")).unwrap();
        assert_eq!(cfg.save.file_extension, ok);
        assert!(w.is_empty());

        let too_long = "a".repeat(121);
        let (cfg, w) =
            Config::from_toml_str(&format!("[save]\nfile_extension = \"{too_long}\"\n")).unwrap();
        assert_eq!(cfg.save.file_extension, "co2");
        assert_eq!(w.len(), 1);
    }
}
