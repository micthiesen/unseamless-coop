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

/// Max master volume. The engine's volume sliders run 0..=10 (the SDK charts
/// `GameSettings::master_volume` with that range). Single-sourced so [`Config::validate`], the menu
/// (`crate::settings`), and the cdylib's boot-volume write all agree, like [`MAX_SCALING_PERCENT`].
pub const MAX_MASTER_VOLUME: u8 = 10;

/// Bounds + default for the co-op session player cap ([`Session::max_players`]), shared by
/// [`Config::validate`] and the settings registry. Vanilla caps a session at 4 (open world) / 6
/// (arena); the SDK documents 6 as the engine's limit, so we don't exceed it without rig evidence
/// that higher is stable. The mod applies this by writing the game's `session_player_limit_override`,
/// so the default of 6 raises the open-world cap (4) to the engine max.
///
/// The floor is **2, not 1**: the game treats `session_player_limit_override == 1` as "use the
/// per-context default", so a configured 1 would be a silent no-op masquerading as a setting.
pub const MIN_SESSION_PLAYERS: u32 = 2;
pub const MAX_SESSION_PLAYERS: u32 = 6;
pub const DEFAULT_SESSION_PLAYERS: u32 = 6;

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
    pub loader: Loader,
    pub debug: Debug,
    /// Tuning for the death-debuff stacking (the on/off toggle is `gameplay.death_debuffs`).
    pub death_debuffs: crate::death_debuffs::DeathDebuffTuning,
    pub world_time: WorldTime,
    pub nameplates: Nameplates,
}

/// Overhead peer **nameplates**: screen-space labels drawn over each co-op partner, projected from
/// their world position (see [`crate::projection`] and `docs/OVERLAY-RENDERING.md`). The *content*
/// shown is selected by [`Gameplay::overhead_display`]; this block holds the nameplate-specific
/// rendering knobs. Off by default — it's a Wave-2 surface that only has peers to draw in a real
/// co-op session, so a fresh install pays nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Nameplates {
    /// Master switch for drawing overhead nameplates. Default off.
    pub enabled: bool,
    /// Cull peers farther than this many meters from the camera (a label on a peer across the map is
    /// noise). Clamped to [`MIN_NAMEPLATE_DISTANCE`]..=[`MAX_NAMEPLATE_DISTANCE`] by
    /// [`Config::validate`]. Integer meters (so [`Config`] stays `Eq` and it maps to a menu `Range`).
    pub max_distance_m: u32,
    /// Debug/solo aid: also draw a nameplate over your *own* character. Off in normal play (you don't
    /// label yourself), but it makes the projection + draw verifiable solo on the rig before the real
    /// remote-peer feed lands. Default off.
    pub show_self: bool,
    /// **Native nameplate dots (debug/config-only):** draw a colored **disc** over each player via the
    /// game's own `CSEzDraw` renderer (world-space, depth-tested, no present-hook) instead of the imgui
    /// overlay's projected labels. Draws over your own head + phantoms so it's verifiable solo on the rig.
    /// Off by default; never in the settings menu (like [`show_self`]). The lone native UI surface kept
    /// after the native-UI exploration (toasts/banners/menu are the imgui overlay) — see
    /// `docs/NAMEPLATES.md` > Outcome.
    pub native_spike: bool,
}

impl Default for Nameplates {
    fn default() -> Self {
        Self {
            enabled: false,
            max_distance_m: DEFAULT_NAMEPLATE_DISTANCE,
            show_self: false,
            native_spike: false,
        }
    }
}

/// Bounds + default for [`Nameplates::max_distance_m`], shared by [`Config::validate`] and the
/// settings registry so the file and the menu agree on the range.
pub const MIN_NAMEPLATE_DISTANCE: u32 = 5;
pub const MAX_NAMEPLATE_DISTANCE: u32 = 300;
pub const DEFAULT_NAMEPLATE_DISTANCE: u32 = 60;

/// Lock the in-game time of day. Local for now (each player sets their own); host-enforced sync is a
/// follow-up, since time-of-day desync between co-op players is a known annoyance worth syncing later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldTime {
    /// Hold the time of day at `hour`:`minute` (a permanent day/night setting) instead of letting it
    /// progress. Off by default = vanilla. The mod re-asserts the game's time *target* each frame,
    /// intended to pin the clock there (the exact engine behavior is rig-confirmed in the feature).
    pub lock: bool,
    /// Hour to hold, `0..=23` (clamped by [`Config::validate`]).
    pub hour: u32,
    /// Minute to hold, `0..=59` (clamped by [`Config::validate`]).
    pub minute: u32,
}

impl Default for WorldTime {
    fn default() -> Self {
        Self { lock: false, hour: 12, minute: 0 }
    }
}

/// External DLL-mod loading. Our shipped `dinput8.dll` is the game's proxy, so this mod is the
/// parent loader: it loads other simple DLL mods dropped in `mods/` (see [`crate::loader`]). The
/// *ordering policy* lives in `crate::loader`; this just holds the user's preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Loader {
    /// Load other DLL mods found in `mods/`. Off → only this mod loads.
    pub enabled: bool,
    /// Mod filenames to load first, in this order; the rest load alphabetically after.
    pub order: Vec<String>,
}

impl Default for Loader {
    fn default() -> Self {
        Self { enabled: true, order: Vec::new() }
    }
}

/// What co-op session (if any) to start automatically once Steam is ready and we're in gameplay,
/// without the in-overlay Open/Join actions. A testing/stopgap aid — chiefly for a machine where the
/// overlay can't run (e.g. the hudhook DX12 hook crashing on some native-Windows GPUs), so it can still
/// connect headless. `off` (the default) keeps co-op fully on-demand.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoSession {
    /// Open a world (host) once ready.
    Host,
    /// Join a world (client) once ready.
    Join,
    /// No auto-trigger — co-op starts only from the overlay actions (the normal path; the default).
    /// Also the unknown-value fallback (`#[serde(other)]` must be the last variant), so a typo degrades
    /// to off rather than a surprise connect.
    #[default]
    #[serde(other)]
    Off,
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
    /// Loopback TCP port for the dev side-channel **bridge** (0 = off). Lets the harness drive the
    /// live mod's `Session` over a socket — no second game/Steam (the `/test-loop` skill's layer 3).
    /// Only honored in builds compiled with the cdylib's `bridge` cargo feature (rig/diag builds),
    /// never in release. A remote-input surface, so it binds `127.0.0.1` only and stays off by default.
    pub bridge_port: u16,
    /// Install the in-game **overlay** (hudhook DX12 → imgui). On by default; this is a recovery
    /// kill-switch — the DX12-over-vkd3d present-hook is the fragile part under Proton (a driver/
    /// Proton update could black-screen it), so set this `false` and relaunch to run without the
    /// overlay rather than being stuck.
    pub overlay: bool,
    /// On-demand diagnostic probes (`[debug.probes]`). All off by default — these are the levers we
    /// ask a tester to flip when chasing a specific issue ("set this, reproduce, send the log").
    pub probes: DebugProbes,
    /// Auto-start a co-op session (`off` / `host` / `join`) once Steam is ready and we're in gameplay,
    /// bypassing the overlay Open/Join actions — for a headless or overlay-broken machine. Off by
    /// default. See [`AutoSession`].
    #[serde(default)]
    pub auto_session: AutoSession,
    /// (**debug builds only**) Which committed rig-testing guide to run, by name (empty = off). The
    /// guide engine is gated behind `#[cfg(debug_assertions)]` (zero release cost), so these two
    /// fields are too — a `release` config simply has no such keys, and an old/foreign config that
    /// carries them is ignored. See [`crate::guide::guides`] for the names.
    #[cfg(debug_assertions)]
    #[serde(default)]
    pub guide: String,
    /// (**debug builds only**) Which role this machine plays for an active [`guide`](Debug::guide):
    /// `host` / `join` / `solo` (default `solo`). One shared guide runs on every machine; each shows
    /// only the steps tagged for its role.
    #[cfg(debug_assertions)]
    #[serde(default)]
    pub rig_role: crate::guide::Role,
}

impl Default for Debug {
    fn default() -> Self {
        Self {
            enabled: false,
            level: LogLevel::Info,
            forward_to_host: false,
            bridge_port: 0,
            overlay: true,
            probes: DebugProbes::default(),
            auto_session: AutoSession::default(),
            #[cfg(debug_assertions)]
            guide: String::new(),
            #[cfg(debug_assertions)]
            rig_role: crate::guide::Role::default(),
        }
    }
}

/// Requestable diagnostic probes. Each is inert unless explicitly enabled, so they can ship in any
/// build and be turned on per-tester on demand. The probes themselves live in the cdylib (`coop/diag`);
/// this is just the switchboard. See [`Config::validate`] for the bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugProbes {
    /// Log a full [`DiagnosticReport`](crate::diagnostics::DiagnosticReport) snapshot every N seconds
    /// (0 = off; the boot + feature-panic snapshots still fire). For watching state evolve on request.
    pub snapshot_secs: u32,
    /// Scan `event_flag_scan_count` event flags starting at this id, logging each that flips — for
    /// locating the flag behind an in-game action (e.g. resting at a grace). `count = 0` = off.
    pub event_flag_scan_start: u32,
    /// How many flags to scan from `event_flag_scan_start`. Clamped to [`MAX_FLAG_SCAN`] (a huge range
    /// scanned per frame is wasteful and pointless). `0` = scanner off.
    pub event_flag_scan_count: u32,
    /// Install the rung-3 session create/join RE probe (`coop/session_probe`): rising-edge logging of
    /// every `CSSessionManager` lobby/protocol FSM transition, plus a logging hook on the session
    /// create/join initiation functions — all under the greppable `session-probe:` log prefix. Off by
    /// default. The FSM-transition half works solo (it just stays at `lobby=None` without a peer); the
    /// hook half stays **inert** until the create/join function AOBs are charted on the rig (a precise
    /// TODO — see `docs/SESSION-RE-RUNBOOK.md`). Enable for a two-player rig run that captures the
    /// `None -> TryToCreateSession -> Host` / `None -> TryToJoinSession -> Client` transition.
    pub session_probe: bool,
    /// Run the rung-4 `RunCallbacks` probe (`coop/steam::run_lobby_callback_probe`): register one
    /// harmless private `CreateLobby` call-result at startup and log (under the `lobby-probe:` prefix)
    /// whether it fires under ELDEN RING's own Steam pump. Answered the rung-4 design question — fires →
    /// ER uses `RunCallbacks`, lobby discovery is viable; never fires → likely `ManualDispatch`, rung 4
    /// blocked. Solo, single-player. Off by default. (Kept as the fast re-derive after a game update.)
    pub lobby_callback_probe: bool,
    /// **EXPERIMENTAL rung-3 direct-drive probe** (`coop/session_probe::SessionCreateDriver`): once
    /// in-game with `lobby_state == None`, CALL the charted create-session initiation on `[G]` to
    /// confirm we can drive `None -> TryToCreateSession` without an in-game item or a peer (the pivot
    /// to driving `CSSessionManager` directly — see `docs/SESSION-DRIVE.md`). One-shot, main thread.
    /// Off by default; a rig experiment, not shipped behavior. The create request builder calls
    /// `is_offline()` twice, so pair this with `gameplay.enable_offline_multiplayer = true`. A driven
    /// `lobby_state -> 1` confirms direct-drive; `-> 2` (FailedToCreate) means an internal gate
    /// rejected it. Solo-confirmable (create needs no peer); join stays 2-player.
    pub drive_create: bool,
    /// **EXPERIMENTAL rung-3 reject-#1 lever** (pairs with [`drive_create`]): just before the driven
    /// create call, force the network-create vmethod's first synchronous reject to pass by writing a
    /// nonzero to the readiness flag at `*([G]+0x60)+0x710 + 0x10` (`NetworkSession+0x10`, the dword the
    /// charted leg-B vmethod `0x1423f5c00` tests first; its other two rejects can't fire offline — see
    /// `docs/SESSION-DRIVE.md` > "Leg B charted"). The driver logs the flag's pre-call value regardless;
    /// this only *writes* it. A rig experiment to see whether satisfying reject #1 lets create walk
    /// `None -> TryToCreateSession -> Host` offline. Off by default.
    pub force_netsession_ready: bool,
}

/// Upper bound on [`DebugProbes::event_flag_scan_count`] — scanning more than this many flags every
/// few frames buys nothing and costs time. (ER event-flag ids are sparse; a targeted window finds it.)
pub const MAX_FLAG_SCAN: u32 = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gameplay {
    /// Let co-op partners damage an enemy during a critical (riposte/backstab/guard counter) instead
    /// of it being invulnerable to everyone but the player who landed it. Host-enforced; default on.
    /// The mod clears the enemy's crit-invuln flag each frame (see `coop/features/crit_coop`).
    pub crit_coop: bool,
    pub death_debuffs: bool,
    pub allow_summons: bool,
    /// Let the party roam the whole map together instead of being tethered to the host's multiplay
    /// area (the defining "seamless" behavior). Host-enforced across the session. The mod holds the
    /// game's `disable_multiplay_restriction` to this. Default on.
    pub roam_anywhere: bool,
    pub overhead_display: OverheadDisplay,
    pub skip_splash_screens: bool,
    /// Re-enable Elden Ring's online **multiplayer items** (Tarnished's Furled Finger, Furlcalling
    /// Finger Remedy, Small Golden Effigy, the duelist/invader fingers, Taunter's Tongue, …) when the
    /// game is launched offline / outside EAC. Vanilla greys these out because FromSoft matchmaking is
    /// unreachable, which also blocks the game's own co-op session FSM — so re-enabling them is what
    /// lets an item-use drive a session (the rung-3 unblock). The mod applies a boot-time code patch
    /// that neutralizes the game's central "is offline" predicate (see `coop/app::apply_boot_patches`
    /// and `docs/OFFLINE-ITEMS-FINDINGS.md`). Default **on**: we only ever load outside EAC, so this
    /// is core to the mod's purpose. The patch is AOB-scanned and fails safe (no-op, logged) if the
    /// signature ever drifts. **Rig result (2026-06-28): BENIGN but INSUFFICIENT** — the patch applies
    /// cleanly and is harmless, but it does **not** ungrey the items: forcing `is_offline()` false at
    /// the root left them greyed, proving the item-grey gate isn't `is_offline()`. Left on (forcing
    /// `is_offline()` false is plausibly still needed for the session FSM), but the real item gate is
    /// pending a follow-up RE pass — see `docs/OFFLINE-ITEMS-FINDINGS.md`.
    pub enable_offline_multiplayer: bool,
    /// **EXPERIMENTAL / UNVERIFIED** candidate for the offline multiplayer-item gate (rung-3
    /// follow-up). [`enable_offline_multiplayer`] forcing `is_offline()` false was rig-proven
    /// *insufficient* — the item-grey gate doesn't read the offline mode enum at all. This flag tries
    /// the next candidate: force the game's `Menu.IsEnableOnlineMode` getter to always report **true**
    /// (a boot-time code patch over its return path; see `coop/app::apply_boot_patches` and
    /// `docs/OFFLINE-ITEMS-FINDINGS.md` > "2026-06-28 item-gate pass"). Static analysis can't tell
    /// whether this getter is part of the real item gate, so this ships **off by default** purely as a
    /// rig-testable lever: flip it on, relaunch, and see if the multiplayer items ungrey. Fail-safe
    /// (no-op + logged) if the AOB drifts, exactly like the patches above. Remove or promote once the
    /// rig settles which signal is the gate.
    pub force_online_menu_mode: bool,
    /// **EXPERIMENTAL / UNVERIFIED** rung-3 lever for *driving a session directly*. A direct call to
    /// the game's create-session wrapper (`0x140cad4c0`) returns `false` offline and the FSM moves
    /// `lobby_state None → FailedToCreateSession` **synchronously** — even with
    /// [`enable_offline_multiplayer`] (`is_offline()` forced false) applied. Static RE found why: the
    /// create inner (`0x140cb1f70`) and the join inner (`0x140cb2470`) both call a **shared,
    /// Arxan-encrypted availability gate** (`0x140cb4b50(this)`) *before* building params, and bail to
    /// `FailedToCreate`/`FailedToJoin` if it returns false. That gate takes only `this` (so our
    /// `flag`/`mode`/`settings` args can't affect it) and runs *before* `is_offline()` (which lives in
    /// the params builder and only sets fields, never rejects) — which is exactly why forcing
    /// `is_offline()` false was insufficient. This flag patches the create call site so the gate's
    /// **veto is ignored** (the gate still runs; its `false` result no longer fails the create),
    /// letting the create proceed to the network-session create so we can see whether the gate was the
    /// reject. Ships **off by default** purely as a rig-testable lever — flip it on, relaunch, and
    /// re-drive create (watch `lobby_state` for `TryToCreateSession` instead of `FailedToCreateSession`).
    /// Fail-safe (no-op + logged) if the AOB drifts, exactly like the patches above. Full RE write-up:
    /// `docs/SESSION-DRIVE.md` > "Why a direct create fails offline".
    pub bypass_session_create_gate: bool,
    pub append_steam_id: bool,
    pub always_spectate_on_death: bool,
    /// Opt-in for [`boot_master_volume`]: when off (the default) the mod never touches the
    /// game's volume, so the player's own setting stands. Only when on does the boot-volume write
    /// happen — otherwise forcing a volume on every launch would override the in-game slider.
    pub boot_master_volume_enabled: bool,
    /// Boot master volume, 0 (mute) .. 10 (max). Only applied when [`boot_master_volume_enabled`] is on.
    /// Clamped by [`Config::validate`].
    pub boot_master_volume: u8,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Co-op session password — the shared key that pairs you with your friends. A fresh install
    /// gets a random one (see [`generate_password`]); everyone in a party must use the *same* value.
    pub password: String,
    /// Maximum players allowed in a co-op session. The mod relaxes the vanilla cap (4 open world /
    /// 6 arena) by writing the game's `session_player_limit_override` to this value. Clamped to
    /// [`MIN_SESSION_PLAYERS`]..=[`MAX_SESSION_PLAYERS`] by [`Config::validate`].
    pub max_players: u32,
}

impl Default for Session {
    fn default() -> Self {
        Self { password: String::new(), max_players: DEFAULT_SESSION_PLAYERS }
    }
}

/// Characters a generated password is drawn from: upper-case letters + digits, minus the
/// ambiguous ones (`0/O`, `1/I/L`), so it's easy to read aloud and retype.
const PASSWORD_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// How many characters a generated default password has (and so how many entropy bytes to supply).
pub const DEFAULT_PASSWORD_LEN: usize = 12;
/// Minimum acceptable co-op password length. Shorter (including empty) is rejected at startup: the
/// password is the session key, and an empty/weak one risks accidental or trivially-joinable
/// sessions. Set to 8 (not the bare minimum) because the peer auth proof is a fast SHA-256, so a
/// captured proof + nonces could be offline-brute-forced against a short password. A fresh install's
/// generated password ([`DEFAULT_PASSWORD_LEN`] = 12) always clears this.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Build a session password from raw entropy: one [`PASSWORD_ALPHABET`] char per input byte. Pure
/// (the charset/format is host-tested) — the cdylib supplies the random bytes, since core has no
/// entropy source. Pass [`DEFAULT_PASSWORD_LEN`] bytes for a standard-length password.
pub fn generate_password(entropy: &[u8]) -> String {
    entropy.iter().map(|b| PASSWORD_ALPHABET[*b as usize % PASSWORD_ALPHABET.len()] as char).collect()
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
            crit_coop: true,
            death_debuffs: true,
            allow_summons: true,
            roam_anywhere: true,
            overhead_display: OverheadDisplay::Normal,
            skip_splash_screens: true,
            enable_offline_multiplayer: true,
            force_online_menu_mode: false,
            bypass_session_create_gate: false,
            append_steam_id: false,
            always_spectate_on_death: false,
            boot_master_volume_enabled: false,
            boot_master_volume: 5,
        }
    }
}

impl Gameplay {
    /// The master volume to force at boot, or `None` when the opt-in ([`boot_master_volume_enabled`]) is off so
    /// the player's own setting stands. The value is clamped to [`MAX_MASTER_VOLUME`]. This is the
    /// host-tested decision behind `coop/features/boot_volume`, kept in core (the cdylib just writes
    /// the returned value to the game).
    ///
    /// [`boot_master_volume_enabled`]: Gameplay::boot_master_volume_enabled
    pub fn boot_volume_to_apply(&self) -> Option<u8> {
        self.boot_master_volume_enabled.then(|| self.boot_master_volume.min(MAX_MASTER_VOLUME))
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

    /// Whether the co-op password meets [`MIN_PASSWORD_LEN`]. A too-short (or empty) password is a
    /// hard error the binding layer rejects at startup, rather than a clampable [`validate`] warning
    /// — there's no safe value to substitute for a session key the user must choose.
    pub fn password_is_valid(&self) -> bool {
        self.session.password.chars().count() >= MIN_PASSWORD_LEN
    }

    /// Clamp/repair out-of-range values in place, reporting what changed.
    pub fn validate(&mut self) -> Vec<ConfigWarning> {
        let mut warnings = Vec::new();

        // Scaling percentages share their upper bound with the menu and the wire decoder, so a
        // hand-edited file can't exceed what the UI allows (and downstream multiplier math stays
        // in a sane range). Same clamp the ConfigSync decoder applies to untrusted peers.
        for name in self.scaling.clamp_percentages() {
            warnings.push(ConfigWarning {
                field: format!("scaling.{name}"),
                message: format!("exceeded {MAX_SCALING_PERCENT}%; clamped"),
            });
        }

        if self.gameplay.boot_master_volume > MAX_MASTER_VOLUME {
            warnings.push(ConfigWarning {
                field: "gameplay.boot_master_volume".into(),
                message: format!(
                    "{} out of range 0..={MAX_MASTER_VOLUME}; clamped to {MAX_MASTER_VOLUME}",
                    self.gameplay.boot_master_volume
                ),
            });
            self.gameplay.boot_master_volume = MAX_MASTER_VOLUME;
        }

        if self.session.max_players < MIN_SESSION_PLAYERS
            || self.session.max_players > MAX_SESSION_PLAYERS
        {
            let clamped = self.session.max_players.clamp(MIN_SESSION_PLAYERS, MAX_SESSION_PLAYERS);
            warnings.push(ConfigWarning {
                field: "session.max_players".into(),
                message: format!(
                    "{} out of range {MIN_SESSION_PLAYERS}..={MAX_SESSION_PLAYERS}; clamped to {clamped}",
                    self.session.max_players
                ),
            });
            self.session.max_players = clamped;
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

        for (field, message) in self.death_debuffs.clamp() {
            warnings.push(ConfigWarning { field: field.into(), message });
        }

        if self.debug.probes.event_flag_scan_count > MAX_FLAG_SCAN {
            warnings.push(ConfigWarning {
                field: "debug.probes.event_flag_scan_count".into(),
                message: format!("{} exceeds {MAX_FLAG_SCAN}; clamped", self.debug.probes.event_flag_scan_count),
            });
            self.debug.probes.event_flag_scan_count = MAX_FLAG_SCAN;
        }

        if self.nameplates.max_distance_m < MIN_NAMEPLATE_DISTANCE
            || self.nameplates.max_distance_m > MAX_NAMEPLATE_DISTANCE
        {
            let clamped =
                self.nameplates.max_distance_m.clamp(MIN_NAMEPLATE_DISTANCE, MAX_NAMEPLATE_DISTANCE);
            warnings.push(ConfigWarning {
                field: "nameplates.max_distance_m".into(),
                message: format!(
                    "{} out of range {MIN_NAMEPLATE_DISTANCE}..={MAX_NAMEPLATE_DISTANCE}; clamped to {clamped}",
                    self.nameplates.max_distance_m
                ),
            });
            self.nameplates.max_distance_m = clamped;
        }

        if self.world_time.hour > 23 {
            warnings.push(ConfigWarning {
                field: "world_time.hour".into(),
                message: format!("{} out of range 0..=23; clamped to 23", self.world_time.hour),
            });
            self.world_time.hour = 23;
        }
        if self.world_time.minute > 59 {
            warnings.push(ConfigWarning {
                field: "world_time.minute".into(),
                message: format!("{} out of range 0..=59; clamped to 59", self.world_time.minute),
            });
            self.world_time.minute = 59;
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rig test seed config (`scripts/rig/seed-config.toml`, installed by `scripts/rig.sh
    /// apply`) must parse cleanly against the current schema — a broken seed silently wastes a rig
    /// launch. Pin it: parses with no clamp warnings, debug logging on, password valid.
    #[test]
    fn rig_seed_config_is_valid() {
        let seed = include_str!("../../../scripts/rig/seed-config.toml");
        let (cfg, warnings) = Config::from_toml_str(seed).expect("seed config must be valid TOML");
        assert!(warnings.is_empty(), "seed config should not warn: {warnings:?}");
        assert!(cfg.debug.enabled, "seed enables debug logging so rig runs capture verbose lines");
        assert!(cfg.password_is_valid(), "seed password must clear MIN_PASSWORD_LEN");
    }

    #[test]
    fn session_probe_defaults_off_and_round_trips() {
        // The rung-3 RE probe must be opt-in (off by default)...
        assert!(!DebugProbes::default().session_probe, "session_probe must default off");

        // ...it must default off when [debug.probes] is present but omits the key (the realistic shape
        // of a config that predates this flag — guards #[serde(default)] on the field, not just Default).
        let (old, w) = Config::from_toml_str("[debug.probes]\nsnapshot_secs = 5\n").unwrap();
        assert!(!old.debug.probes.session_probe, "missing key must default off");
        assert!(w.is_empty(), "{w:?}");

        // ...and a hand-set `true` must survive a real serialize -> parse round-trip (catches a future
        // skip_serializing / key-rename that would silently drop the value when config is persisted).
        let mut cfg = Config::default();
        cfg.debug.probes.session_probe = true;
        let (reparsed, w) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert!(reparsed.debug.probes.session_probe, "session_probe must survive round-trip");
        assert!(w.is_empty(), "{w:?}");
    }

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
            Config::from_toml_str("[gameplay]\ncrit_coop = false\nfuture_option = 42\n")
                .unwrap();
        assert!(!cfg.gameplay.crit_coop);
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
    fn world_time_out_of_range_is_clamped_with_warnings() {
        let (cfg, warnings) =
            Config::from_toml_str("[world_time]\nlock = true\nhour = 30\nminute = 90\n").unwrap();
        assert_eq!(cfg.world_time.hour, 23);
        assert_eq!(cfg.world_time.minute, 59);
        assert!(cfg.world_time.lock);
        // Exactly the two world_time warnings, with the user-facing clamp target in the message.
        let wt: Vec<&ConfigWarning> = warnings.iter().filter(|w| w.field.starts_with("world_time.")).collect();
        assert_eq!(wt.len(), 2, "{warnings:?}");
        assert!(wt.iter().any(|w| w.field == "world_time.hour" && w.message.contains("clamped to 23")));
        assert!(wt.iter().any(|w| w.field == "world_time.minute" && w.message.contains("clamped to 59")));
    }

    #[test]
    fn world_time_in_range_is_accepted_without_warning() {
        // The boundary values must be valid (the clamp uses `>`, not `>=`) and silent.
        let (cfg, warnings) =
            Config::from_toml_str("[world_time]\nlock = true\nhour = 23\nminute = 59\n").unwrap();
        assert_eq!((cfg.world_time.hour, cfg.world_time.minute), (23, 59));
        assert!(!warnings.iter().any(|w| w.field.starts_with("world_time.")), "{warnings:?}");
    }

    #[test]
    fn generated_password_is_deterministic_and_uses_the_safe_charset() {
        // One char per byte, taken from the unambiguous alphabet; deterministic given the bytes.
        assert_eq!(super::generate_password(&[0, 0, 0]), "AAA");
        let pw = super::generate_password(&[0, 1, 2, 30, 31, 255]); // bytes wrap into the alphabet
        assert_eq!(pw.len(), 6);
        assert!(pw.bytes().all(|b| super::PASSWORD_ALPHABET.contains(&b)), "only safe chars: {pw}");
        // None of the ambiguous characters can appear.
        assert!(!pw.contains(['0', 'O', '1', 'I', 'L']));
        // DEFAULT_PASSWORD_LEN bytes yields a DEFAULT_PASSWORD_LEN-char password.
        assert_eq!(super::generate_password(&[7; super::DEFAULT_PASSWORD_LEN]).len(), super::DEFAULT_PASSWORD_LEN);
    }

    #[test]
    fn password_validity_enforces_minimum_length() {
        let with = |pw: &str| {
            let mut c = Config::default();
            c.session.password = pw.into();
            c.password_is_valid()
        };
        assert!(!with(""), "empty rejected");
        assert!(!with("abcde"), "below the minimum (5 < 8) rejected");
        assert!(!with("abcdefg"), "one under the minimum (7) rejected");
        assert!(with("abcdefgh"), "exactly the minimum (8) accepted");
        assert!(with("a-strong-password"), "longer accepted");
        // A freshly generated default always clears the bar.
        assert!(super::generate_password(&[1; super::DEFAULT_PASSWORD_LEN]).chars().count() >= super::MIN_PASSWORD_LEN);
    }

    #[test]
    fn volume_is_clamped_with_warning() {
        let (cfg, warnings) =
            Config::from_toml_str("[gameplay]\nboot_master_volume = 99\n").unwrap();
        assert_eq!(cfg.gameplay.boot_master_volume, 10);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].field, "gameplay.boot_master_volume");
    }

    #[test]
    fn boot_volume_to_apply_gates_and_clamps() {
        // Off by default ⇒ never touch the volume (the opt-in safety invariant).
        let mut g = Gameplay::default();
        assert!(!g.boot_master_volume_enabled, "boot volume must be opt-in (off by default)");
        assert_eq!(g.boot_volume_to_apply(), None);

        // On ⇒ the configured level, clamped to the engine max.
        g.boot_master_volume_enabled = true;
        g.boot_master_volume = 3;
        assert_eq!(g.boot_volume_to_apply(), Some(3));
        g.boot_master_volume = 99; // a hand-edited file past the bound
        assert_eq!(g.boot_volume_to_apply(), Some(MAX_MASTER_VOLUME));
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
    fn max_players_defaults_and_persists() {
        assert_eq!(Config::default().session.max_players, super::DEFAULT_SESSION_PLAYERS);
        let (cfg, w) = Config::from_toml_str("[session]\nmax_players = 4\n").unwrap();
        assert_eq!(cfg.session.max_players, 4);
        assert!(w.is_empty(), "{w:?}");

        // A non-default value survives a real serialize -> parse round-trip (guards the
        // #[serde(default)] + manual Default wiring on the new field).
        let mut cfg = Config::default();
        cfg.session.max_players = 3;
        let (round, w) = Config::from_toml_str(&cfg.to_toml_string()).unwrap();
        assert_eq!(round.session.max_players, 3);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn max_players_clamped_with_warning() {
        // Above the engine cap and below the floor both clamp, with a warning naming the field.
        let max = super::MAX_SESSION_PLAYERS;
        let min = super::MIN_SESSION_PLAYERS;
        let (cfg, w) = Config::from_toml_str(&format!("[session]\nmax_players = {}\n", max + 10)).unwrap();
        assert_eq!(cfg.session.max_players, max);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].field, "session.max_players");

        let (cfg, w) = Config::from_toml_str("[session]\nmax_players = 0\n").unwrap();
        assert_eq!(cfg.session.max_players, min);
        assert_eq!(w.len(), 1);

        // Both boundary values are themselves valid (no warning) — guards the `<`/`>` from
        // drifting to `<=`/`>=` and warn-clamping a legitimate min or max.
        let (_, w) = Config::from_toml_str(&format!("[session]\nmax_players = {max}\n")).unwrap();
        assert!(w.is_empty(), "max boundary should not warn: {w:?}");
        let (_, w) = Config::from_toml_str(&format!("[session]\nmax_players = {min}\n")).unwrap();
        assert!(w.is_empty(), "min boundary should not warn: {w:?}");
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
    fn scaling_clamp_boundary_is_exact() {
        // MAX is valid (untouched, no warning); MAX+1 clamps.
        let max = super::MAX_SCALING_PERCENT;
        let (cfg, w) = Config::from_toml_str(&format!("[scaling]\nenemy_health = {max}\n")).unwrap();
        assert_eq!(cfg.scaling.enemy_health, max);
        assert!(w.is_empty());
        let (cfg, w) = Config::from_toml_str(&format!("[scaling]\nenemy_health = {}\n", max + 1)).unwrap();
        assert_eq!(cfg.scaling.enemy_health, max);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn volume_boundary_is_exact() {
        // 10 is valid (no warning); 11 clamps.
        let (cfg, w) = Config::from_toml_str("[gameplay]\nboot_master_volume = 10\n").unwrap();
        assert_eq!(cfg.gameplay.boot_master_volume, 10);
        assert!(w.is_empty());
        let (cfg, w) = Config::from_toml_str("[gameplay]\nboot_master_volume = 11\n").unwrap();
        assert_eq!(cfg.gameplay.boot_master_volume, 10);
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

    #[test]
    fn auto_session_parses_and_unknown_degrades_to_off() {
        let (cfg, _) = Config::from_toml_str("[debug]\nauto_session = \"host\"\n").unwrap();
        assert_eq!(cfg.debug.auto_session, super::AutoSession::Host);
        let (cfg, _) = Config::from_toml_str("[debug]\nauto_session = \"join\"\n").unwrap();
        assert_eq!(cfg.debug.auto_session, super::AutoSession::Join);
        // Absent -> Off (default); an unknown value -> Off (serde(other)), never a surprise connect.
        let (cfg, _) = Config::from_toml_str("[debug]\nenabled = true\n").unwrap();
        assert_eq!(cfg.debug.auto_session, super::AutoSession::Off);
        let (cfg, _) = Config::from_toml_str("[debug]\nauto_session = \"bogus\"\n").unwrap();
        assert_eq!(cfg.debug.auto_session, super::AutoSession::Off);
    }
}
