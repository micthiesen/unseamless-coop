//! The mod side-channel wire format.
//!
//! Co-op transport is the game's own P2P layer (see ARCHITECTURE.md). On top of it the mod needs
//! a small private channel to coordinate *mod* state between modded clients — sync the host's
//! enforced settings, broadcast session actions (open/lock/leave/…), and heartbeat. Those
//! messages ride inside one game packet type via `NetworkSession::broadcast_packet`; this module
//! defines how a [`ModMessage`] is framed into the bytes that packet carries.
//!
//! This is **our** format — we define it cleanly, and every player runs our mod, so there's no
//! compatibility constraint (no vanilla-ERSC interop; see ARCHITECTURE.md). The encoding is
//! deliberately trivial and self-describing so it's fully host-testable here; the cdylib only
//! has to hand the resulting bytes to the game and back.
//!
//! Frame layout (big-endian):
//! ```text
//! +------+---------+------+-----------------+
//! | "UC" | version | type |    payload …    |
//! |  2B  |   1B    |  1B  |   type-specific |
//! +------+---------+------+-----------------+
//! ```

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::config::{Config, MAX_SESSION_PLAYERS, MIN_SESSION_PLAYERS, Scaling};
use crate::diagnostics::{LogLevel, LogRecord};

/// Magic prefix identifying one of our side-channel frames.
pub const MAGIC: [u8; 2] = *b"UC";
/// Current wire version. Bump on any incompatible change; decoders reject mismatches. v2 added the
/// `generation`/`seq` identity fields to `ConfigSync`/`SessionAction`; v3 added `max_players`, which
/// *shifted* the `ConfigSync` payload (an older decoder would misparse). v4 added `roam_anywhere` as a
/// 4th bit in the existing settings byte — the wire length is unchanged, so an un-bumped older decoder
/// would parse fine but silently *drop the new flag* and diverge on roam; the bump turns that silent
/// setting-divergence into a clean `UnknownVersion` rejection instead. v5 repurposed settings bit 0
/// from the removed `allow_invaders` to `crit_coop` (same wire width, changed *meaning*), so a v4
/// decoder would misread the flag — the bump rejects it cleanly instead.
pub const VERSION: u8 = 5;

/// Number of bools packed into the `ConfigSync` settings byte (`crit_coop`, `death_debuffs`,
/// `allow_summons`, `roam_anywhere`). Single-sources the count so the encode (`pack_bools`) and decode
/// (`unpack_bools`) can't drift to different `N` and silently cross-map bits — a count change here is a
/// compile error at both call sites until their arrays/destructures match.
const SETTINGS_BOOL_COUNT: usize = 4;
/// Cap on a forwarded log message's bytes, to keep side-channel packets small. Longer messages
/// are truncated on a UTF-8 boundary at encode time.
pub const MAX_LOG_MSG: usize = 2048;

/// The mod's semantic version exchanged in the `Hello` handshake. The single source of truth for
/// every peer the mod stands up (the cdylib bridge and the harness both reference this), so the two
/// can't drift and spuriously report a "version mismatch". Distinct from [`VERSION`] (the on-wire
/// frame format): this is the human/compatibility version, that is the byte layout.
pub const PROTOCOL_VERSION: crate::util::Version = crate::util::Version::new(0, 1, 0);

/// A message exchanged between modded clients over the side-channel.
///
/// The side-channel rides the game's P2P broadcast, whose delivery guarantees we don't yet know
/// (Steam P2P can be unreliable and unordered). So stateful/event messages carry their **own
/// identity** — a config `generation`, an action `seq`, a log `seq`, a ping `frame` — letting the
/// receiver ([`crate::peer::Peer`]) ignore stale/duplicate frames and converge regardless of
/// drops, duplicates, or reordering. Idempotent messages (`Hello`) need none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModMessage {
    /// Sent on connect to advertise the sender's mod version (pack a [`crate::util::Version`] via
    /// `to_u32`). Idempotent: re-handling it just re-records the sender's version, so it needs no
    /// sequence of its own.
    Hello { mod_version: u32 },
    /// The host's authoritative shared settings, tagged with a monotonic `generation` so a client
    /// applies only newer settings (a reordered/duplicated sync is ignored) and the host can safely
    /// **re-assert** the same generation to heal drops.
    ConfigSync { generation: u32, settings: SharedSettings },
    /// A session action the host (or a permitted client) is performing, tagged with the sender's
    /// monotonic `seq` so a receiver applies each action exactly once (a duplicated frame is a
    /// no-op).
    SessionAction { seq: u32, action: SessionAction },
    /// Liveness ping carrying the sender's frame counter (cheap clock/heartbeat). Naturally
    /// idempotent — the receiver just records "seen now".
    Ping { frame: u64 },
    /// A forwarded debug log line (sent to the host when `forward_to_host` is on). Its
    /// [`LogRecord::seq`] dedups duplicates at the host. See [`crate::diagnostics`].
    Log(LogRecord),
}

/// The subset of config that must be identical across the party (the host enforces it). Distinct
/// from the full local [`crate::config::Config`], which also holds machine-local prefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedSettings {
    pub scaling: Scaling,
    pub crit_coop: bool,
    pub death_debuffs: bool,
    pub allow_summons: bool,
    pub roam_anywhere: bool,
    /// The session player cap. Host-enforced: a client adopts the host's so the whole party agrees
    /// on session size. Clamped on decode like `scaling`, since it comes from an untrusted peer.
    pub max_players: u32,
}

/// Project a [`Config`] into the host-enforced subset broadcast over the wire. Keeps the
/// "which fields are session-wide" decision in core (host-tested) rather than in the cdylib.
impl From<&Config> for SharedSettings {
    fn from(c: &Config) -> Self {
        Self {
            scaling: c.scaling,
            crit_coop: c.gameplay.crit_coop,
            death_debuffs: c.gameplay.death_debuffs,
            allow_summons: c.gameplay.allow_summons,
            roam_anywhere: c.gameplay.roam_anywhere,
            max_players: c.session.max_players,
        }
    }
}

impl SharedSettings {
    /// Apply a received host-enforced subset onto a local [`Config`] (the inverse of
    /// [`From<&Config>`]). A client calls this when it receives the host's `ConfigSync` so its
    /// local rules match — the per-field mapping lives here in core, not in the cdylib.
    pub fn apply_to(&self, cfg: &mut Config) {
        cfg.scaling = self.scaling;
        cfg.gameplay.crit_coop = self.crit_coop;
        cfg.gameplay.death_debuffs = self.death_debuffs;
        cfg.gameplay.allow_summons = self.allow_summons;
        cfg.gameplay.roam_anywhere = self.roam_anywhere;
        cfg.session.max_players = self.max_players;
    }
}

/// Host/client session actions, mirroring ERSC's `OPTIONSELECT_*` menu surface (FEATURES.md).
/// Discriminants are explicit and the wire conversions are **derived** (`num_enum`), so adding a
/// variant can't drift the encoder and decoder apart. `u8::from(action)` encodes;
/// `SessionAction::try_from(byte)` decodes (rejecting unknown values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum SessionAction {
    OpenWorld = 0,
    JoinWorld = 1,
    LeaveWorld = 2,
    LockWorld = 3,
    UnlockWorld = 4,
    TogglePvp = 5,
    TogglePvpTeams = 6,
    ToggleFriendlyFire = 7,
}

impl SessionAction {
    /// Every variant, for enumerating the action set (menu build, tests) without re-typing the list.
    pub const ALL: [SessionAction; 8] = {
        use SessionAction::*;
        [
            OpenWorld, JoinWorld, LeaveWorld, LockWorld, UnlockWorld, TogglePvp,
            TogglePvpTeams, ToggleFriendlyFire,
        ]
    };

    /// Human label for this action, shared by the in-game menu and any feedback toast so the two
    /// can't drift. UI copy lives here (one source) rather than being re-typed per call site.
    pub fn label(self) -> &'static str {
        use SessionAction::*;
        match self {
            OpenWorld => "Host / open world",
            JoinWorld => "Join world",
            LeaveWorld => "Leave world",
            LockWorld => "Lock world",
            UnlockWorld => "Unlock world",
            TogglePvp => "Toggle PvP",
            TogglePvpTeams => "Toggle PvP teams",
            ToggleFriendlyFire => "Toggle friendly fire",
        }
    }

    /// Whether only the host may perform this action (lock/unlock and the PvP toggles). The apply
    /// layer authorizes an inbound action by the **sender's** role using this, since the menu's
    /// local-UI gating doesn't constrain a packet from a peer.
    pub fn is_host_only(self) -> bool {
        use SessionAction::*;
        matches!(
            self,
            LockWorld | UnlockWorld | TogglePvp | TogglePvpTeams | ToggleFriendlyFire
        )
    }
}

/// Message type tags (the 4th frame byte).
mod tag {
    pub const HELLO: u8 = 0;
    pub const CONFIG_SYNC: u8 = 1;
    pub const SESSION_ACTION: u8 = 2;
    pub const PING: u8 = 3;
    pub const LOG: u8 = 4;
}

/// Why a frame failed to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Missing/wrong `"UC"` magic — not one of our frames.
    BadMagic,
    /// Frame version we don't speak.
    UnknownVersion(u8),
    /// Unknown message type tag.
    UnknownType(u8),
    /// Ran out of bytes mid-field.
    Truncated,
    /// A field held a value outside its valid set (e.g. an undefined `SessionAction`).
    BadValue,
    /// Extra trailing bytes after a complete message (likely corruption/desync).
    TrailingBytes,
}

impl ModMessage {
    /// Encode to a self-contained frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::with_capacity(8);
        w.extend_from_slice(&MAGIC);
        w.push(VERSION);
        match self {
            ModMessage::Hello { mod_version } => {
                w.push(tag::HELLO);
                w.extend_from_slice(&mod_version.to_be_bytes());
            }
            ModMessage::ConfigSync { generation, settings: s } => {
                w.push(tag::CONFIG_SYNC);
                w.extend_from_slice(&generation.to_be_bytes());
                for v in [
                    s.scaling.enemy_health,
                    s.scaling.enemy_damage,
                    s.scaling.enemy_posture,
                    s.scaling.boss_health,
                    s.scaling.boss_damage,
                    s.scaling.boss_posture,
                ] {
                    w.extend_from_slice(&v.to_be_bytes());
                }
                w.extend_from_slice(&s.max_players.to_be_bytes());
                w.push(pack_bools::<SETTINGS_BOOL_COUNT>([
                    s.crit_coop,
                    s.death_debuffs,
                    s.allow_summons,
                    s.roam_anywhere,
                ]));
            }
            ModMessage::SessionAction { seq, action } => {
                w.push(tag::SESSION_ACTION);
                w.extend_from_slice(&seq.to_be_bytes());
                w.push(u8::from(*action));
            }
            ModMessage::Ping { frame } => {
                w.push(tag::PING);
                w.extend_from_slice(&frame.to_be_bytes());
            }
            ModMessage::Log(rec) => {
                w.push(tag::LOG);
                w.extend_from_slice(&rec.seq.to_be_bytes());
                w.push(u8::from(rec.level));
                let msg = truncate_on_boundary(&rec.message, MAX_LOG_MSG);
                w.extend_from_slice(&(msg.len() as u16).to_be_bytes());
                w.extend_from_slice(msg.as_bytes());
            }
        }
        w
    }

    /// Decode a frame produced by [`encode`](ModMessage::encode). Rejects anything malformed.
    pub fn decode(bytes: &[u8]) -> Result<ModMessage, DecodeError> {
        let mut r = Reader::new(bytes);
        if r.take(2)? != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(DecodeError::UnknownVersion(version));
        }
        let tag = r.u8()?;
        let msg = match tag {
            tag::HELLO => ModMessage::Hello { mod_version: r.u32()? },
            tag::CONFIG_SYNC => {
                let generation = r.u32()?;
                let mut scaling = Scaling {
                    enemy_health: r.u32()?,
                    enemy_damage: r.u32()?,
                    enemy_posture: r.u32()?,
                    boss_health: r.u32()?,
                    boss_damage: r.u32()?,
                    boss_posture: r.u32()?,
                };
                // Untrusted peer: hold wire scaling to the same bound as a local config file, so a
                // malicious host can't push an out-of-range multiplier the user never consented to.
                scaling.clamp_percentages();
                // Same reasoning for the player cap: clamp to the config's accepted range.
                let max_players = r.u32()?.clamp(MIN_SESSION_PLAYERS, MAX_SESSION_PLAYERS);
                let [crit_coop, death_debuffs, allow_summons, roam_anywhere] =
                    unpack_bools::<SETTINGS_BOOL_COUNT>(r.u8()?);
                ModMessage::ConfigSync {
                    generation,
                    settings: SharedSettings {
                        scaling,
                        crit_coop,
                        death_debuffs,
                        allow_summons,
                        roam_anywhere,
                        max_players,
                    },
                }
            }
            tag::SESSION_ACTION => {
                let seq = r.u32()?;
                let action = SessionAction::try_from(r.u8()?).map_err(|_| DecodeError::BadValue)?;
                ModMessage::SessionAction { seq, action }
            }
            tag::PING => ModMessage::Ping { frame: r.u64()? },
            tag::LOG => {
                let seq = r.u32()?;
                let level = LogLevel::try_from(r.u8()?).map_err(|_| DecodeError::BadValue)?;
                let message = r.string_u16()?;
                ModMessage::Log(LogRecord { seq, level, message })
            }
            other => return Err(DecodeError::UnknownType(other)),
        };
        if !r.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(msg)
    }
}

fn pack_bools<const N: usize>(bits: [bool; N]) -> u8 {
    debug_assert!(N <= 8);
    let mut byte = 0u8;
    for (i, b) in bits.into_iter().enumerate() {
        if b {
            byte |= 1 << i;
        }
    }
    byte
}

fn unpack_bools<const N: usize>(byte: u8) -> [bool; N] {
    debug_assert!(N <= 8); // parity with pack_bools; a bit index >= 8 would overflow the u8 shift
    std::array::from_fn(|i| byte & (1 << i) != 0)
}

/// Minimal big-endian byte reader.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// A `u16`-length-prefixed UTF-8 string.
    fn string_u16(&mut self) -> Result<String, DecodeError> {
        let len = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::BadValue)
    }
    fn is_empty(&self) -> bool {
        self.pos == self.buf.len()
    }
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> SharedSettings {
        SharedSettings {
            scaling: Scaling {
                enemy_health: 35,
                enemy_damage: 0,
                enemy_posture: 15,
                boss_health: 100,
                boss_damage: 0,
                boss_posture: 20,
            },
            crit_coop: true,
            death_debuffs: false,
            allow_summons: true,
            roam_anywhere: true,
            max_players: 4,
        }
    }

    fn samples() -> Vec<ModMessage> {
        vec![
            ModMessage::Hello { mod_version: 0x0001_0203 },
            ModMessage::ConfigSync { generation: 7, settings: shared() },
            ModMessage::SessionAction { seq: 1, action: SessionAction::LockWorld },
            ModMessage::SessionAction { seq: u32::MAX, action: SessionAction::JoinWorld },
            ModMessage::Ping { frame: u64::MAX },
            ModMessage::Log(LogRecord {
                seq: 42,
                level: LogLevel::Warn,
                message: "something looked off in WorldChrMan".into(),
            }),
        ]
    }

    #[test]
    fn every_message_round_trips() {
        for msg in samples() {
            let bytes = msg.encode();
            assert_eq!(ModMessage::decode(&bytes), Ok(msg.clone()), "round-trip failed for {msg:?}");
        }
    }

    #[test]
    fn shared_settings_projects_every_host_enforced_field() {
        // Set each projected field to a non-default value so a wrong mapping (e.g. two fields
        // reading the same config bool) is caught.
        let mut cfg = crate::config::Config::default();
        cfg.gameplay.crit_coop = false;
        cfg.gameplay.death_debuffs = false;
        cfg.gameplay.allow_summons = false;
        cfg.gameplay.roam_anywhere = false; // non-default (default on)
        cfg.scaling.boss_health = 200;
        cfg.session.max_players = 4; // non-default, host-enforced
        cfg.session.password = "secret".into(); // machine-local; SharedSettings has no such field
        let shared = SharedSettings::from(&cfg);
        assert!(!shared.crit_coop);
        assert!(!shared.death_debuffs);
        assert!(!shared.allow_summons);
        assert!(!shared.roam_anywhere);
        assert_eq!(shared.scaling.boss_health, 200);
        assert_eq!(shared.max_players, 4);
        let msg = ModMessage::ConfigSync { generation: 3, settings: shared };
        assert_eq!(ModMessage::decode(&msg.encode()), Ok(msg));
    }

    #[test]
    fn shared_settings_apply_to_is_the_inverse_of_projection() {
        // Set EVERY shared field to a non-default value, so a forgotten `apply_to` assignment for any
        // one of them fails this test (default==default would otherwise hide the omission). The shared
        // bools all default to `true`/`true`/`true`/`true`, so flip each to `false`.
        let mut host = crate::config::Config::default();
        host.gameplay.crit_coop = false;
        host.gameplay.death_debuffs = false;
        host.gameplay.allow_summons = false;
        host.gameplay.roam_anywhere = false;
        host.scaling.enemy_health = 80;
        host.session.max_players = 3; // non-default + != the client's default, so apply must override
        let shared = SharedSettings::from(&host);

        // A client with different local settings receives and applies the host's subset.
        let mut client = crate::config::Config::default();
        client.session.password = "client-local".into(); // must be untouched (not shared)
        shared.apply_to(&mut client);

        assert_eq!(SharedSettings::from(&client), shared, "client now agrees on the shared subset");
        assert_eq!(client.session.max_players, shared.max_players, "host's player cap adopted");
        assert_eq!(client.session.password, "client-local", "machine-local fields untouched");
    }

    #[test]
    fn narrowed_writers_compose_without_a_lost_update() {
        // Models the cdylib's `state::update` narrowing (see `unseamless-coop/src/state.rs`): two
        // concurrent writers each touch only the fields they own — the overlay menu writes a
        // machine-local field, the co-op `ConfigSync` path writes the host's shared subset. Applied to
        // the same live config in *either order*, both changes must survive (a whole-config `set`
        // would clobber whichever ran first). This is the property that makes the second writer safe.
        let shared = SharedSettings::from(&{
            let mut host = crate::config::Config::default();
            host.gameplay.crit_coop = false; // host-enforced shared field, flipped off default
            host.session.max_players = 4;
            host
        });
        // The menu's narrowed write: a machine-local field SharedSettings has no say over.
        let menu_write = |c: &mut crate::config::Config| c.debug.enabled = true;
        // The sync's narrowed write: only the shared subset.
        let sync_write = |c: &mut crate::config::Config| shared.apply_to(c);

        let mut menu_then_sync = crate::config::Config::default();
        menu_write(&mut menu_then_sync);
        sync_write(&mut menu_then_sync);

        let mut sync_then_menu = crate::config::Config::default();
        sync_write(&mut sync_then_menu);
        menu_write(&mut sync_then_menu);

        // Order doesn't matter, and both writers' changes are present in each result.
        assert_eq!(menu_then_sync, sync_then_menu, "narrowed writes are order-independent");
        for c in [&menu_then_sync, &sync_then_menu] {
            assert!(c.debug.enabled, "the menu's local write survived the sync");
            assert!(!c.gameplay.crit_coop, "the sync's shared write survived the menu write");
            assert_eq!(c.session.max_players, 4, "the sync's player cap survived");
        }
    }

    #[test]
    fn config_sync_clamps_out_of_range_max_players_from_the_wire() {
        // Decode must bound an untrusted player cap to the config's range on both sides, and leave
        // an in-range value untouched (so the clamp isn't accidentally `.clamp(MAX, MAX)` etc.).
        let min = crate::config::MIN_SESSION_PLAYERS;
        let max = crate::config::MAX_SESSION_PLAYERS;
        for (wire, expected) in [(0u32, min), (1, min), (u32::MAX, max), (max + 1, max), (4, 4)] {
            let mut s = shared();
            s.max_players = wire;
            let frame = ModMessage::ConfigSync { generation: 1, settings: s }.encode();
            match ModMessage::decode(&frame).unwrap() {
                ModMessage::ConfigSync { settings, .. } => {
                    assert_eq!(settings.max_players, expected, "wire {wire} should decode to {expected}");
                }
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn config_sync_clamps_out_of_range_scaling_from_the_wire() {
        // A hostile peer sends an absurd multiplier; decode must bound it just like a local file.
        let mut evil = shared();
        evil.scaling.enemy_health = u32::MAX;
        evil.scaling.boss_health = 9999;
        let frame = ModMessage::ConfigSync { generation: 1, settings: evil }.encode();
        match ModMessage::decode(&frame).unwrap() {
            ModMessage::ConfigSync { settings: s, .. } => {
                assert_eq!(s.scaling.enemy_health, crate::config::MAX_SCALING_PERCENT);
                assert_eq!(s.scaling.boss_health, crate::config::MAX_SCALING_PERCENT);
                assert_eq!(s.scaling.enemy_posture, shared().scaling.enemy_posture); // in-range kept
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn config_sync_preserves_all_fields() {
        let msg = ModMessage::ConfigSync { generation: 9, settings: shared() };
        match ModMessage::decode(&msg.encode()).unwrap() {
            ModMessage::ConfigSync { generation, settings } => {
                assert_eq!(generation, 9);
                assert_eq!(settings, shared());
            }
            other => panic!("decoded wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bool_packing_is_independent_across_all_combinations() {
        // Every one of the 2^4 flag combinations must round-trip exactly — proves no bit
        // cross-contaminates another (a single-combo test couldn't catch an OR-ing bug).
        for bits in 0u8..16 {
            let mut s = shared();
            s.crit_coop = bits & 1 != 0;
            s.death_debuffs = bits & 2 != 0;
            s.allow_summons = bits & 4 != 0;
            s.roam_anywhere = bits & 8 != 0;
            let msg = ModMessage::ConfigSync { generation: 1, settings: s };
            assert_eq!(ModMessage::decode(&msg.encode()).unwrap(), msg, "combo {bits:04b} corrupted");
        }
    }

    #[test]
    fn session_action_labels_are_unique_and_nonempty() {
        let mut labels: Vec<&str> = SessionAction::ALL.iter().map(|a| a.label()).collect();
        assert!(labels.iter().all(|l| !l.is_empty()), "every action needs a label");
        labels.sort_unstable();
        let n = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), n, "action labels must be unique");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = ModMessage::Ping { frame: 1 }.encode();
        bytes[0] = b'X';
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadMagic));
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes = ModMessage::Ping { frame: 1 }.encode();
        bytes[2] = 99;
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::UnknownVersion(99)));
    }

    #[test]
    fn rejects_superseded_v1_frame() {
        // A peer on the pre-identity (v1) format is rejected by the version gate, so the layout
        // skew can never reach the field decoders and silently misparse.
        let mut bytes = ModMessage::Ping { frame: 1 }.encode();
        bytes[2] = 1;
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::UnknownVersion(1)));
    }

    #[test]
    fn rejects_superseded_v2_frame() {
        // v3 added `max_players` to ConfigSync, so a v2 frame is shorter by 4 bytes. The whole point
        // of the version bump is that the gate rejects it rather than misparsing the shifted payload.
        let mut bytes = ModMessage::ConfigSync { generation: 1, settings: shared() }.encode();
        bytes[2] = 2;
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::UnknownVersion(2)));
    }

    #[test]
    fn rejects_superseded_v4_frame() {
        // v5 repurposed settings bit 0 (`allow_invaders` -> `crit_coop`) at unchanged wire width, so a
        // v4 frame would parse but silently misread the flag. This pins the 4->5 bump: it fails the
        // moment someone reverts `VERSION` to 4 (which the generic `rejects_unknown_version`'s 99 can't
        // catch, since 99 != 4 would still reject).
        let mut bytes = ModMessage::ConfigSync { generation: 1, settings: shared() }.encode();
        bytes[2] = 4;
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::UnknownVersion(4)));
    }

    #[test]
    fn config_sync_generation_sits_at_a_fixed_offset() {
        // Pin the on-wire position of `generation` (bytes 4..8, right after magic+version+tag), so
        // an encoder offset regression that a symmetric decode bug would hide is still caught.
        let bytes = ModMessage::ConfigSync { generation: 0x0A0B_0C0D, settings: shared() }.encode();
        assert_eq!(&bytes[3..4], &[tag::CONFIG_SYNC]);
        assert_eq!(&bytes[4..8], &0x0A0B_0C0Du32.to_be_bytes());
    }

    #[test]
    fn rejects_unknown_type() {
        let bytes = [MAGIC[0], MAGIC[1], VERSION, 200];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::UnknownType(200)));
    }

    #[test]
    fn rejects_truncated_payload() {
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::PING, 0, 0]; // ping needs 8 payload bytes
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::Truncated));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = ModMessage::SessionAction { seq: 1, action: SessionAction::OpenWorld }.encode();
        bytes.push(0xff);
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn rejects_undefined_session_action() {
        // magic, version, SESSION_ACTION tag, seq=1, action=250 (undefined).
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::SESSION_ACTION, 0, 0, 0, 1, 250];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadValue));
    }

    #[test]
    fn session_action_discriminant_boundary_after_removal() {
        // After removing `BreakInWorld`, `ToggleDriedFinger`, and `GiveEmber` and renumbering
        // contiguously, the top valid discriminant is 7 (`ToggleFriendlyFire`) and byte 8 is now
        // undefined. Pin both ends so a future off-by-one renumber or an accidental re-add at slot 8 is
        // caught (the `ALL`-length/label-uniqueness tests only catch additions, not this freed slot).
        let frame = |action: u8| [MAGIC[0], MAGIC[1], VERSION, tag::SESSION_ACTION, 0, 0, 0, 1, action];
        assert_eq!(
            ModMessage::decode(&frame(7)),
            Ok(ModMessage::SessionAction { seq: 1, action: SessionAction::ToggleFriendlyFire }),
        );
        assert_eq!(ModMessage::decode(&frame(8)), Err(DecodeError::BadValue));
    }

    #[test]
    fn empty_input_is_truncated_not_panic() {
        assert_eq!(ModMessage::decode(&[]), Err(DecodeError::Truncated));
    }

    #[test]
    fn log_message_with_empty_and_unicode_text_round_trips() {
        for text in ["", "plain", "unicode: 日本語 🎮 café"] {
            let msg = ModMessage::Log(LogRecord {
                seq: 7,
                level: LogLevel::Debug,
                message: text.into(),
            });
            assert_eq!(ModMessage::decode(&msg.encode()), Ok(msg));
        }
    }

    #[test]
    fn oversized_log_message_is_truncated_on_a_char_boundary() {
        // 3-byte chars so the cap (2048) does NOT fall on a char boundary (2048 % 3 != 0): a
        // naive `&s[..MAX_LOG_MSG]` would panic mid-character, so this actually exercises the
        // boundary-backoff loop in truncate_on_boundary.
        assert_ne!(MAX_LOG_MSG % 3, 0, "test premise: cap must land mid-char for a 3-byte char");
        let long = "あ".repeat(MAX_LOG_MSG); // 3 bytes each -> well over the cap
        let msg = ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: long });
        let decoded = ModMessage::decode(&msg.encode()).unwrap();
        match decoded {
            ModMessage::Log(rec) => {
                assert!(rec.message.len() <= MAX_LOG_MSG);
                assert!(rec.message.len() > MAX_LOG_MSG - 3, "should fill up to the last whole char");
                assert!(rec.message.chars().all(|c| c == 'あ')); // no split/replacement char
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn log_length_prefix_overrun_is_rejected() {
        // A hostile Log frame claiming a 0xFFFF-byte string but carrying only a few bytes must
        // be rejected as Truncated, never over-read or over-allocate.
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::LOG, 0, 0, 0, 7, 2, 0xff, 0xff, b'h', b'i'];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::Truncated));
    }

    #[test]
    fn log_with_out_of_range_level_is_rejected() {
        // magic, version, LOG tag, seq=0, level=5 (undefined; valid range is 0..=4), len=0.
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::LOG, 0, 0, 0, 0, 5, 0, 0];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadValue));
    }

    #[test]
    fn log_with_invalid_utf8_is_rejected() {
        // magic, version, LOG tag, seq=0, level=Info(2), len=1, then an invalid UTF-8 byte.
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::LOG, 0, 0, 0, 0, 2, 0, 1, 0xff];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadValue));
    }
}
