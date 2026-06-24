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

use crate::config::Scaling;
use crate::diagnostics::{LogLevel, LogRecord};

/// Magic prefix identifying one of our side-channel frames.
pub const MAGIC: [u8; 2] = *b"UC";
/// Current wire version. Bump on any incompatible change; decoders reject mismatches.
pub const VERSION: u8 = 1;
/// Cap on a forwarded log message's bytes, to keep side-channel packets small. Longer messages
/// are truncated on a UTF-8 boundary at encode time.
pub const MAX_LOG_MSG: usize = 2048;

/// A message exchanged between modded clients over the side-channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModMessage {
    /// Sent on connect to advertise the sender's mod version. Mismatches let clients warn the
    /// user rather than desync silently.
    Hello { mod_version: u32 },
    /// The host's authoritative shared settings, pushed to clients so everyone agrees on rules.
    ConfigSync(SharedSettings),
    /// A session action the host (or a permitted client) is performing.
    SessionAction(SessionAction),
    /// Liveness ping carrying the sender's frame counter (cheap clock/heartbeat).
    Ping { frame: u64 },
    /// A forwarded debug log line (sent to the host when `forward_to_host` is on). See
    /// [`crate::diagnostics`].
    Log(LogRecord),
}

/// The subset of config that must be identical across the party (the host enforces it). Distinct
/// from the full local [`crate::config::Config`], which also holds machine-local prefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedSettings {
    pub scaling: Scaling,
    pub allow_invaders: bool,
    pub death_debuffs: bool,
    pub allow_summons: bool,
}

/// Host/client session actions, mirroring ERSC's `OPTIONSELECT_*` menu surface (FEATURES.md).
/// Discriminants are explicit so the wire value is stable across refactors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionAction {
    OpenWorld = 0,
    JoinWorld = 1,
    BreakInWorld = 2,
    LeaveWorld = 3,
    LockWorld = 4,
    UnlockWorld = 5,
    TogglePvp = 6,
    TogglePvpTeams = 7,
    ToggleFriendlyFire = 8,
    ToggleDriedFinger = 9,
    GiveEmber = 10,
}

impl SessionAction {
    pub fn from_u8(v: u8) -> Option<Self> {
        use SessionAction::*;
        Some(match v {
            0 => OpenWorld,
            1 => JoinWorld,
            2 => BreakInWorld,
            3 => LeaveWorld,
            4 => LockWorld,
            5 => UnlockWorld,
            6 => TogglePvp,
            7 => TogglePvpTeams,
            8 => ToggleFriendlyFire,
            9 => ToggleDriedFinger,
            10 => GiveEmber,
            _ => return None,
        })
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
            ModMessage::ConfigSync(s) => {
                w.push(tag::CONFIG_SYNC);
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
                w.push(pack_bools([s.allow_invaders, s.death_debuffs, s.allow_summons]));
            }
            ModMessage::SessionAction(a) => {
                w.push(tag::SESSION_ACTION);
                w.push(*a as u8);
            }
            ModMessage::Ping { frame } => {
                w.push(tag::PING);
                w.extend_from_slice(&frame.to_be_bytes());
            }
            ModMessage::Log(rec) => {
                w.push(tag::LOG);
                w.extend_from_slice(&rec.seq.to_be_bytes());
                w.push(rec.level.to_u8());
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
                let scaling = Scaling {
                    enemy_health: r.u32()?,
                    enemy_damage: r.u32()?,
                    enemy_posture: r.u32()?,
                    boss_health: r.u32()?,
                    boss_damage: r.u32()?,
                    boss_posture: r.u32()?,
                };
                let [allow_invaders, death_debuffs, allow_summons] = unpack_bools(r.u8()?);
                ModMessage::ConfigSync(SharedSettings {
                    scaling,
                    allow_invaders,
                    death_debuffs,
                    allow_summons,
                })
            }
            tag::SESSION_ACTION => {
                ModMessage::SessionAction(SessionAction::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?)
            }
            tag::PING => ModMessage::Ping { frame: r.u64()? },
            tag::LOG => {
                let seq = r.u32()?;
                let level = LogLevel::from_u8(r.u8()?).ok_or(DecodeError::BadValue)?;
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
            allow_invaders: true,
            death_debuffs: false,
            allow_summons: true,
        }
    }

    fn samples() -> Vec<ModMessage> {
        vec![
            ModMessage::Hello { mod_version: 0x0001_0203 },
            ModMessage::ConfigSync(shared()),
            ModMessage::SessionAction(SessionAction::LockWorld),
            ModMessage::SessionAction(SessionAction::GiveEmber),
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
    fn config_sync_preserves_all_fields() {
        let msg = ModMessage::ConfigSync(shared());
        match ModMessage::decode(&msg.encode()).unwrap() {
            ModMessage::ConfigSync(s) => assert_eq!(s, shared()),
            other => panic!("decoded wrong variant: {other:?}"),
        }
    }

    #[test]
    fn bool_packing_is_independent_per_bit() {
        // death_debuffs=false must not leak into the other two flags.
        let s = shared();
        assert!(s.allow_invaders && !s.death_debuffs && s.allow_summons);
        let decoded = ModMessage::decode(&ModMessage::ConfigSync(s).encode()).unwrap();
        assert_eq!(decoded, ModMessage::ConfigSync(s));
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
        let mut bytes = ModMessage::SessionAction(SessionAction::OpenWorld).encode();
        bytes.push(0xff);
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn rejects_undefined_session_action() {
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::SESSION_ACTION, 250];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadValue));
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
        // Multibyte chars so a naive byte cut could split one.
        let long = "é".repeat(MAX_LOG_MSG); // 2 bytes each -> well over the cap
        let msg = ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: long });
        let decoded = ModMessage::decode(&msg.encode()).unwrap();
        match decoded {
            ModMessage::Log(rec) => {
                assert!(rec.message.len() <= MAX_LOG_MSG);
                assert!(rec.message.chars().all(|c| c == 'é')); // no split/replacement char
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn log_with_invalid_utf8_is_rejected() {
        // magic, version, LOG tag, seq=0, level=Info(2), len=1, then an invalid UTF-8 byte.
        let bytes = [MAGIC[0], MAGIC[1], VERSION, tag::LOG, 0, 0, 0, 0, 2, 0, 1, 0xff];
        assert_eq!(ModMessage::decode(&bytes), Err(DecodeError::BadValue));
    }
}
