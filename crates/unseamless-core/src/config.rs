//! Mod configuration: a typed [`Config`], a small lenient INI parser, and round-trip
//! serialization. Models the same surface as ERSC's `ersc_settings.ini` so existing users'
//! configs port over, but the types and defaults are our own.
//!
//! Parsing is deliberately lenient — a mod that refuses to launch because one line is malformed
//! is worse than one that warns and falls back to a default. [`Config::from_ini_str`] never
//! fails; it collects [`ConfigWarning`]s for the caller to log.

use std::collections::BTreeMap;
use std::fmt;

/// Full mod configuration. Construct from disk with [`Config::from_ini_str`], or take
/// [`Config::default`] (which equals a fresh ERSC install's defaults).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub gameplay: Gameplay,
    pub scaling: Scaling,
    /// Co-op session password. Empty = no password.
    pub password: String,
    /// Save-file extension for co-op saves (vanilla is `sl2`); keeps co-op saves separate.
    pub save_file_extension: String,
    /// Locale file to force; `None` follows the game language.
    pub language_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gameplay {
    pub allow_invaders: bool,
    pub death_debuffs: bool,
    pub allow_summons: bool,
    pub overhead_display: OverheadDisplay,
    pub skip_splash_screens: bool,
    pub append_steam_id: bool,
    pub always_spectate_on_death: bool,
    /// Boot master volume, 0 (mute) .. 10 (max).
    pub default_boot_master_volume: u8,
}

/// Per-player scaling percentages. Each value is "% added per extra player" applied by the
/// host; see [`crate::scaling`] for how they turn into multipliers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scaling {
    pub enemy_health: u32,
    pub enemy_damage: u32,
    pub enemy_posture: u32,
    pub boss_health: u32,
    pub boss_damage: u32,
    pub boss_posture: u32,
}

/// What to show above other players' heads. Discriminants match the ERSC ini values so legacy
/// configs map directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OverheadDisplay {
    Normal = 0,
    None = 1,
    Ping = 2,
    SoulLevel = 3,
    DeathCount = 4,
    SoulLevelAndPing = 5,
}

impl OverheadDisplay {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Normal,
            1 => Self::None,
            2 => Self::Ping,
            3 => Self::SoulLevel,
            4 => Self::DeathCount,
            5 => Self::SoulLevelAndPing,
            _ => return None,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gameplay: Gameplay {
                allow_invaders: true,
                death_debuffs: true,
                allow_summons: true,
                overhead_display: OverheadDisplay::Normal,
                skip_splash_screens: false,
                append_steam_id: false,
                always_spectate_on_death: false,
                default_boot_master_volume: 5,
            },
            scaling: Scaling {
                enemy_health: 35,
                enemy_damage: 0,
                enemy_posture: 15,
                boss_health: 100,
                boss_damage: 0,
                boss_posture: 20,
            },
            password: String::new(),
            save_file_extension: "co2".to_string(),
            language_override: None,
        }
    }
}

/// A non-fatal issue encountered while parsing config. The caller should log these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    /// A `key = value` whose value didn't parse as the expected type; the default was kept.
    BadValue { section: String, key: String, value: String },
    /// A key we don't recognize. Ignored.
    UnknownKey { section: String, key: String },
    /// A non-blank, non-comment line with no `=`.
    MalformedLine { line: usize, content: String },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadValue { section, key, value } => {
                write!(f, "bad value for [{section}] {key} = {value:?}; using default")
            }
            Self::UnknownKey { section, key } => write!(f, "unknown key [{section}] {key}; ignored"),
            Self::MalformedLine { line, content } => {
                write!(f, "malformed line {line}: {content:?}; ignored")
            }
        }
    }
}

impl Config {
    /// Parse a config from INI text. Never fails: unrecognized/invalid entries fall back to the
    /// default value and are reported as [`ConfigWarning`]s.
    pub fn from_ini_str(text: &str) -> (Config, Vec<ConfigWarning>) {
        let (ini, mut warnings) = Ini::parse(text);
        let mut cfg = Config::default();

        let mut take_bool = |section: &str, key: &str, slot: &mut bool| {
            if let Some(raw) = ini.get(section, key) {
                match parse_bool(raw) {
                    Some(v) => *slot = v,
                    None => warnings.push(ConfigWarning::BadValue {
                        section: section.into(),
                        key: key.into(),
                        value: raw.into(),
                    }),
                }
            }
        };

        take_bool("gameplay", "allow_invaders", &mut cfg.gameplay.allow_invaders);
        take_bool("gameplay", "death_debuffs", &mut cfg.gameplay.death_debuffs);
        take_bool("gameplay", "allow_summons", &mut cfg.gameplay.allow_summons);
        take_bool("gameplay", "skip_splash_screens", &mut cfg.gameplay.skip_splash_screens);
        take_bool("gameplay", "append_steam_id_to_players", &mut cfg.gameplay.append_steam_id);
        take_bool(
            "gameplay",
            "always_spectate_on_death",
            &mut cfg.gameplay.always_spectate_on_death,
        );

        if let Some(raw) = ini.get("gameplay", "overhead_player_display") {
            match raw.trim().parse::<u8>().ok().and_then(OverheadDisplay::from_u8) {
                Some(v) => cfg.gameplay.overhead_display = v,
                None => warnings.push(ConfigWarning::BadValue {
                    section: "gameplay".into(),
                    key: "overhead_player_display".into(),
                    value: raw.into(),
                }),
            }
        }

        take_u8_clamped(
            &ini,
            "gameplay",
            "default_boot_master_volume",
            0,
            10,
            &mut cfg.gameplay.default_boot_master_volume,
            &mut warnings,
        );

        for (key, slot) in [
            ("enemy_health_scaling", &mut cfg.scaling.enemy_health as *mut u32),
            ("enemy_damage_scaling", &mut cfg.scaling.enemy_damage),
            ("enemy_posture_scaling", &mut cfg.scaling.enemy_posture),
            ("boss_health_scaling", &mut cfg.scaling.boss_health),
            ("boss_damage_scaling", &mut cfg.scaling.boss_damage),
            ("boss_posture_scaling", &mut cfg.scaling.boss_posture),
        ] {
            if let Some(raw) = ini.get("scaling", key) {
                match raw.trim().parse::<u32>() {
                    // SAFETY: each `slot` is a unique &mut field of `cfg.scaling`, used once.
                    Ok(v) => unsafe { *slot = v },
                    Err(_) => warnings.push(ConfigWarning::BadValue {
                        section: "scaling".into(),
                        key: key.into(),
                        value: raw.into(),
                    }),
                }
            }
        }

        if let Some(raw) = ini.get("password", "cooppassword") {
            cfg.password = raw.trim().to_string();
        }
        if let Some(raw) = ini.get("save", "save_file_extension") {
            let ext = raw.trim();
            if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) || ext.len() > 120 {
                warnings.push(ConfigWarning::BadValue {
                    section: "save".into(),
                    key: "save_file_extension".into(),
                    value: raw.into(),
                });
            } else {
                cfg.save_file_extension = ext.to_string();
            }
        }
        if let Some(raw) = ini.get("language", "mod_language_override") {
            let v = raw.trim();
            cfg.language_override = if v.is_empty() { None } else { Some(v.to_string()) };
        }

        warnings.extend(unknown_keys(&ini));
        (cfg, warnings)
    }

    /// Serialize to commented INI text suitable for writing a default config file.
    pub fn to_ini_string(&self) -> String {
        let g = &self.gameplay;
        let s = &self.scaling;
        format!(
            "[GAMEPLAY]\n\
             allow_invaders = {}\n\
             death_debuffs = {}\n\
             allow_summons = {}\n\
             overhead_player_display = {}\n\
             skip_splash_screens = {}\n\
             append_steam_id_to_players = {}\n\
             always_spectate_on_death = {}\n\
             default_boot_master_volume = {}\n\
             \n\
             [SCALING]\n\
             enemy_health_scaling = {}\n\
             enemy_damage_scaling = {}\n\
             enemy_posture_scaling = {}\n\
             boss_health_scaling = {}\n\
             boss_damage_scaling = {}\n\
             boss_posture_scaling = {}\n\
             \n\
             [PASSWORD]\n\
             cooppassword = {}\n\
             \n\
             [SAVE]\n\
             save_file_extension = {}\n\
             \n\
             [LANGUAGE]\n\
             mod_language_override = {}\n",
            b2i(g.allow_invaders),
            b2i(g.death_debuffs),
            b2i(g.allow_summons),
            g.overhead_display as u8,
            b2i(g.skip_splash_screens),
            b2i(g.append_steam_id),
            b2i(g.always_spectate_on_death),
            g.default_boot_master_volume,
            s.enemy_health,
            s.enemy_damage,
            s.enemy_posture,
            s.boss_health,
            s.boss_damage,
            s.boss_posture,
            self.password,
            self.save_file_extension,
            self.language_override.as_deref().unwrap_or(""),
        )
    }
}

fn take_u8_clamped(
    ini: &Ini,
    section: &str,
    key: &str,
    lo: u8,
    hi: u8,
    slot: &mut u8,
    warnings: &mut Vec<ConfigWarning>,
) {
    if let Some(raw) = ini.get(section, key) {
        match raw.trim().parse::<u8>() {
            Ok(v) => *slot = v.clamp(lo, hi),
            Err(_) => warnings.push(ConfigWarning::BadValue {
                section: section.into(),
                key: key.into(),
                value: raw.into(),
            }),
        }
    }
}

fn b2i(b: bool) -> u8 {
    b as u8
}

/// Accepts `0`/`1` (ERSC style) plus `true`/`false`/`yes`/`no`, case-insensitive.
fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Recognized `section -> keys`, used to flag unknown keys.
const KNOWN: &[(&str, &[&str])] = &[
    (
        "gameplay",
        &[
            "allow_invaders",
            "death_debuffs",
            "allow_summons",
            "overhead_player_display",
            "skip_splash_screens",
            "append_steam_id_to_players",
            "always_spectate_on_death",
            "default_boot_master_volume",
        ],
    ),
    (
        "scaling",
        &[
            "enemy_health_scaling",
            "enemy_damage_scaling",
            "enemy_posture_scaling",
            "boss_health_scaling",
            "boss_damage_scaling",
            "boss_posture_scaling",
        ],
    ),
    ("password", &["cooppassword"]),
    ("save", &["save_file_extension"]),
    ("language", &["mod_language_override"]),
];

fn unknown_keys(ini: &Ini) -> Vec<ConfigWarning> {
    let mut out = Vec::new();
    for (section, keys) in &ini.sections {
        let known = KNOWN.iter().find(|(s, _)| s == section).map(|(_, k)| *k);
        for key in keys.keys() {
            let recognized = known.is_some_and(|k| k.contains(&key.as_str()));
            if !recognized {
                out.push(ConfigWarning::UnknownKey {
                    section: section.clone(),
                    key: key.clone(),
                });
            }
        }
    }
    out
}

/// A minimal, lenient INI representation: `section -> (key -> value)`, all keys/sections
/// lowercased. Comments start with `;` or `#`. This is the only parsing primitive; the typed
/// [`Config`] is layered on top so the parser stays trivially testable.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ini {
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Ini {
    pub fn parse(text: &str) -> (Ini, Vec<ConfigWarning>) {
        let mut ini = Ini::default();
        let mut warnings = Vec::new();
        let mut current = String::new(); // keys before any [section] live in ""

        for (i, raw_line) in text.lines().enumerate() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = name.trim().to_ascii_lowercase();
                ini.sections.entry(current.clone()).or_default();
                continue;
            }
            match line.split_once('=') {
                Some((k, v)) => {
                    ini.sections
                        .entry(current.clone())
                        .or_default()
                        .insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
                None => warnings.push(ConfigWarning::MalformedLine {
                    line: i + 1,
                    content: line.to_string(),
                }),
            }
        }
        (ini, warnings)
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }
}

/// Strip a trailing `;`/`#` comment. Note: ERSC values are plain (no quoting/escaping), and a
/// `;` or `#` never appears in a legitimate value here, so a simple split is correct.
fn strip_comment(line: &str) -> &str {
    let cut = line.find([';', '#']).unwrap_or(line.len());
    &line[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_ini() {
        let cfg = Config::default();
        let (reparsed, warnings) = Config::from_ini_str(&cfg.to_ini_string());
        assert_eq!(cfg, reparsed);
        assert!(warnings.is_empty(), "default config should produce no warnings: {warnings:?}");
    }

    #[test]
    fn parses_the_shipped_ersc_settings() {
        // Mirrors reference/.../ersc_settings.ini (the real upstream defaults).
        let text = "\
            [GAMEPLAY]\n\
            allow_invaders = 1\n\
            death_debuffs = 1\n\
            allow_summons = 1\n\
            overhead_player_display = 0\n\
            skip_splash_screens = 0\n\
            append_steam_id_to_players = 0\n\
            always_spectate_on_death = 0 \n\
            default_boot_master_volume = 5\n\
            [SCALING]\n\
            enemy_health_scaling = 35\n\
            enemy_damage_scaling = 0\n\
            enemy_posture_scaling = 15\n\
            boss_health_scaling = 100\n\
            boss_damage_scaling = 0\n\
            boss_posture_scaling = 20\n\
            [PASSWORD]\n\
            cooppassword = \n\
            [SAVE]\n\
            save_file_extension = co2\n\
            [LANGUAGE]\n\
            mod_language_override = \n";
        let (cfg, warnings) = Config::from_ini_str(text);
        assert_eq!(cfg, Config::default());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "\
            ; a comment\n\
            \n\
            [GAMEPLAY]   ; inline comment after section\n\
            allow_invaders = 0  ; turn invaders off\n\
            # hash comment\n\
            death_debuffs = 0\n";
        let (cfg, warnings) = Config::from_ini_str(text);
        assert!(!cfg.gameplay.allow_invaders);
        assert!(!cfg.gameplay.death_debuffs);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn bad_value_keeps_default_and_warns() {
        let (cfg, warnings) = Config::from_ini_str("[SCALING]\nenemy_health_scaling = lots\n");
        assert_eq!(cfg.scaling.enemy_health, Config::default().scaling.enemy_health);
        assert_eq!(
            warnings,
            vec![ConfigWarning::BadValue {
                section: "scaling".into(),
                key: "enemy_health_scaling".into(),
                value: "lots".into(),
            }]
        );
    }

    #[test]
    fn bool_accepts_word_and_numeric_forms() {
        let (cfg, w) = Config::from_ini_str(
            "[GAMEPLAY]\nallow_invaders = true\nallow_summons = NO\nskip_splash_screens = on\n",
        );
        assert!(cfg.gameplay.allow_invaders);
        assert!(!cfg.gameplay.allow_summons);
        assert!(cfg.gameplay.skip_splash_screens);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn volume_is_clamped() {
        let (cfg, _) = Config::from_ini_str("[GAMEPLAY]\ndefault_boot_master_volume = 99\n");
        assert_eq!(cfg.gameplay.default_boot_master_volume, 10);
    }

    #[test]
    fn overhead_display_maps_from_int() {
        let (cfg, w) = Config::from_ini_str("[GAMEPLAY]\noverhead_player_display = 5\n");
        assert_eq!(cfg.gameplay.overhead_display, OverheadDisplay::SoulLevelAndPing);
        assert!(w.is_empty());
        let (cfg, w) = Config::from_ini_str("[GAMEPLAY]\noverhead_player_display = 9\n");
        assert_eq!(cfg.gameplay.overhead_display, OverheadDisplay::Normal); // default kept
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn invalid_save_extension_rejected() {
        let (cfg, w) = Config::from_ini_str("[SAVE]\nsave_file_extension = co.2\n");
        assert_eq!(cfg.save_file_extension, "co2"); // default kept (non-alphanumeric)
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn unknown_key_is_flagged_not_fatal() {
        let (_, warnings) = Config::from_ini_str("[GAMEPLAY]\nallow_teabagging = 1\n");
        assert!(warnings.contains(&ConfigWarning::UnknownKey {
            section: "gameplay".into(),
            key: "allow_teabagging".into(),
        }));
    }

    #[test]
    fn language_override_blank_is_none() {
        let (cfg, _) = Config::from_ini_str("[LANGUAGE]\nmod_language_override =   \n");
        assert_eq!(cfg.language_override, None);
        let (cfg, _) = Config::from_ini_str("[LANGUAGE]\nmod_language_override = french\n");
        assert_eq!(cfg.language_override.as_deref(), Some("french"));
    }
}
