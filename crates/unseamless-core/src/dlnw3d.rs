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

/// Max total on-wire packet the transport length header can encode: the frame-length check
/// (`0x1426425d0`) reads an **11-bit** length (`byte0 | ((byte1 & 7) << 8)`), so `0x7ff`.
pub const TRANSPORT_FRAME_MAX: usize = 0x7ff;

/// The transport flag bit our 14-byte SYN set in `byte1` (and which passed the recv frame-length gate
/// `0x1426425d0`). We reuse it for the hand-framed type-5 — the exact meaning of the byte1 high-5
/// flag bits is Arxan-opaque on disk, so this is the one flag we've observed a frame get admitted with.
const TRANSPORT_FLAG_BYTE1: u8 = 0x40;

/// Wrap a DLNW3D message `payload` in the transport **length header** the game's recv path expects
/// before it reaches the per-connection pump. The recv frame-length check `0x1426425d0` reads an
/// 11-bit length as `byte0 | ((byte1 & 7) << 8)` and requires `2 <= len <= size`; the reassembler then
/// strips this 2-byte header and enqueues the message for the pump (which reads `msgbuf[0]` = the type).
///
/// The encoded length is the **total on-wire size** (header + payload), matching our 14-byte SYN, whose
/// `byte0 = 0x0e = 14` = the whole packet length. Returns `None` if the total exceeds the 11-bit field.
///
/// ⚠️ This reproduces only the **length header**. The chart flags a possible *inner* transport frame
/// (bits in `byte1` beyond the length, added by the Arxan-opaque `0x142642860`); if a plain
/// length-wrapped type-5 doesn't dispatch on the peer, the fallback is to capture a real ERSC type-5's
/// on-wire bytes and copy the framing verbatim (docs/STATE.md > Next, decision 1).
pub fn wrap_transport_frame(payload: &[u8]) -> Option<Vec<u8>> {
    let total = payload.len().checked_add(2)?;
    if total > TRANSPORT_FRAME_MAX {
        return None;
    }
    let mut framed = Vec::with_capacity(total);
    framed.push((total & 0xff) as u8);
    framed.push(TRANSPORT_FLAG_BYTE1 | ((total >> 8) & 7) as u8);
    framed.extend_from_slice(payload);
    Some(framed)
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

    #[test]
    fn transport_header_encodes_total_length() {
        // A 12-byte payload → 14-byte packet, matching the SYN's byte0=0x0e=14 / byte1=0x40 shape.
        let framed = wrap_transport_frame(&[0u8; 12]).expect("fits the 11-bit field");
        assert_eq!(framed.len(), 14, "header + payload");
        assert_eq!(framed[0], 0x0e, "byte0 = total & 0xff = 14");
        assert_eq!(framed[1], 0x40, "byte1 = flag 0x40 | high-3-bits of length (0)");
        // Recompute the 11-bit length the recv gate reads: byte0 | ((byte1 & 7) << 8).
        let framed_len = framed[0] as usize | ((framed[1] as usize & 7) << 8);
        assert_eq!(framed_len, 14, "the encoded length is the total on-wire size");
    }

    #[test]
    fn transport_header_spills_into_byte1_over_255() {
        // 300-byte payload → 302 total; low byte 0x2e, high bits 1 → byte1 = 0x41.
        let framed = wrap_transport_frame(&[0u8; 300]).expect("fits the 11-bit field");
        assert_eq!(framed[0], (302 & 0xff) as u8);
        assert_eq!(framed[1], 0x40 | 1);
        let framed_len = framed[0] as usize | ((framed[1] as usize & 7) << 8);
        assert_eq!(framed_len, 302);
    }

    #[test]
    fn transport_header_wraps_a_real_type5() {
        // The end-to-end shape the sender emits: [len-hdr][5][token][len][ticket].
        let ticket = vec![0x11u8; 234]; // a typical Steam session-ticket size
        let payload = frame_type5(0, &ticket).expect("frames");
        let framed = wrap_transport_frame(&payload).expect("wraps");
        assert_eq!(framed.len(), payload.len() + 2);
        assert_eq!(framed[2], 5, "the message (post-header) starts with the type byte");
        let framed_len = framed[0] as usize | ((framed[1] as usize & 7) << 8);
        assert_eq!(framed_len, framed.len(), "encoded length == actual packet size");
    }

    #[test]
    fn transport_header_rejects_over_11_bits() {
        let too_big = vec![0u8; TRANSPORT_FRAME_MAX - 1]; // +2 header pushes total past 0x7ff
        assert!(wrap_transport_frame(&too_big).is_none(), "total exceeds the 11-bit length field");
        let at_cap = vec![0u8; TRANSPORT_FRAME_MAX - 2];
        assert!(wrap_transport_frame(&at_cap).is_some(), "exactly the 11-bit cap is allowed");
    }
}
