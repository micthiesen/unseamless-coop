//! Framing for the game's own **DLNW3D connect protocol** — the per-connection handshake messages the
//! game's pump (`0x1424007e0`) dispatches by `buf[0]` = type (1..8). This is the *game's* wire format
//! (distinct from our side-channel [`crate::protocol`]), reverse-engineered from a live ERSC capture
//! (docs/ERSC-LIVE-CAPTURE-FINDINGS.md > "★ The DLNW3D connect protocol").
//!
//! We only need to *produce* one message: **type 5**, the handshake-completion message. It carries a Steam
//! session **auth ticket** the host validates (`0x142402ee0` / `BeginAuthSession`) to set the peer member's
//! `+0x152=1` and complete the co-op handshake (`players` → 2). Everything up to a persistent, endpoint-wired
//! joiner member already works; this message is the final mile (docs/STATE.md > Next (b)).
//!
//! Charted type-5 layout (the pump reads `buf[0]`=type, then the type-5 case reads an 8-byte token, then the
//! validator reads a 4-byte length `1..=0x400` and that many blob bytes):
//! ```text
//! +--------+-----------+-----------+------------------+
//! | type=5 |   token   |    len    |    blob (ticket) |
//! |   1B   |    8B     |    4B     |     len bytes    |
//! +--------+-----------+-----------+------------------+
//! ```
//! **Byte order is little-endian** (the game reads these fields with direct little-endian loads on x86; the
//! transport is not network-byte-order). ⚠️ The exact transport framing (whether a length/header prefix wraps
//! this before it reaches the pump) and the **token semantics** (`member+0x148` — an echoed value, a session
//! nonce, or ignored?) are being charted by the `type5-chart` lane; `frame_type5` takes the token as a
//! parameter so it stays correct whatever the source turns out to be. Keep this module host-tested — the
//! cdylib only hands the resulting bytes to the game.

/// DLNW3D connect-message type discriminant (`buf[0]`). Only [`Type::HandshakeComplete`] (5) is produced by
/// the mod; the rest are named for legibility against the charted 8-type jump table (`0x1424009f8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Type {
    /// Type 5 — carries the auth ticket; the host's validator sets `member+0x152=1` (handshake complete).
    HandshakeComplete = 5,
}

/// Max blob (auth-ticket) length the type-5 length field admits (charted `len` gate: `1..=0x400`). A Steam
/// session ticket (~234B typical, ~1024B cap) always fits.
pub const TYPE5_BLOB_MAX: usize = 0x400;

/// Frame a **type-5** handshake-completion message: `[5][token: 8B LE][len: 4B LE][blob]`. `blob` is the
/// Steam auth ticket (`crate::steam::get_auth_session_ticket` in the cdylib). Returns `None` if the blob is
/// empty or exceeds the charted `len` cap (`0x400`) — an over-long blob would be rejected by the validator's
/// length gate, so we refuse to frame it rather than send a doomed message.
pub fn frame_type5(token: u64, blob: &[u8]) -> Option<Vec<u8>> {
    if blob.is_empty() || blob.len() > TYPE5_BLOB_MAX {
        return None;
    }
    let mut msg = Vec::with_capacity(1 + 8 + 4 + blob.len());
    msg.push(Type::HandshakeComplete as u8);
    msg.extend_from_slice(&token.to_le_bytes());
    msg.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    msg.extend_from_slice(blob);
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_type5_layout() {
        let blob = [0xAAu8, 0xBB, 0xCC];
        let msg = frame_type5(0x0102_0304_0506_0708, &blob).expect("valid blob frames");
        // [type=5][token LE 8B][len LE 4B][blob]
        assert_eq!(msg[0], 5, "buf[0] is the type discriminant");
        assert_eq!(&msg[1..9], &0x0102_0304_0506_0708u64.to_le_bytes(), "8B little-endian token");
        assert_eq!(&msg[9..13], &3u32.to_le_bytes(), "4B little-endian length");
        assert_eq!(&msg[13..], &blob, "blob follows verbatim");
        assert_eq!(msg.len(), 1 + 8 + 4 + 3);
    }

    #[test]
    fn rejects_empty_blob() {
        assert!(frame_type5(0, &[]).is_none(), "an empty ticket can't be a valid type-5");
    }

    #[test]
    fn rejects_oversized_blob() {
        let too_big = vec![0u8; TYPE5_BLOB_MAX + 1];
        assert!(frame_type5(0, &too_big).is_none(), "the validator's len gate is 1..=0x400");
        let at_cap = vec![0u8; TYPE5_BLOB_MAX];
        assert!(frame_type5(0, &at_cap).is_some(), "exactly 0x400 is allowed");
    }
}
