//! Length-prefixed wire framing for the real-socket transports.
//!
//! Both the harness's `TcpTransport` (host-side tests) and the cdylib's debug **bridge** carry one
//! encoded [`crate::protocol::ModMessage`] per frame over a byte stream. They must agree on the
//! framing exactly, so the *pure* codec lives here — host-tested — and each transport supplies only
//! its own socket I/O.
//!
//! Frame layout (big-endian): `[u32 len][u64 sender][payload]`. `len` covers only `payload`;
//! `sender` carries the [`PeerId`] the in-memory bus otherwise tracks out of band.

use crate::transport::PeerId;

/// Header size: `u32` length + `u64` sender id.
pub const FRAME_HEADER_LEN: usize = 4 + 8;

/// Reject a frame claiming more than this payload — a desynced or hostile peer otherwise grows the
/// decode buffer without bound waiting for a frame that never completes. Side-channel `ModMessage`s
/// are tiny (a forwarded log caps at ~2 KiB), so 64 KiB is generous.
pub const MAX_FRAME: usize = 64 * 1024;

/// Encode one `payload` (an already-serialized `ModMessage`) as a frame tagged with `sender`.
pub fn encode_frame(sender: PeerId, payload: &[u8]) -> Vec<u8> {
    // The length prefix is a u32, and the decoder rejects anything over MAX_FRAME; keep the encode
    // side honest about the same bound (side-channel messages are KiB-scale, so this never trips in
    // practice — it just makes the "payload fits" invariant symmetric rather than decode-only).
    debug_assert!(payload.len() <= MAX_FRAME, "frame payload {} exceeds MAX_FRAME", payload.len());
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&sender.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// A frame declared a payload larger than [`MAX_FRAME`] — a framing desync or hostile peer. The
/// caller should treat the connection as broken (the decode buffer has been cleared to resync).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTooLarge(pub usize);

/// Accumulates received bytes and yields complete frames. Handles partial reads (a frame split
/// across socket reads) and multiple frames in one read.
#[derive(Default)]
pub struct FrameDecoder {
    inbuf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes just read from the socket.
    pub fn push(&mut self, bytes: &[u8]) {
        self.inbuf.extend_from_slice(bytes);
    }

    /// Remove and return every complete frame currently buffered, each as `(sender, payload)`.
    /// A trailing partial frame stays buffered for the next call. On a frame whose declared length
    /// exceeds [`MAX_FRAME`], the whole buffer is cleared and [`FrameTooLarge`] is returned (over a
    /// trusted localhost link this shouldn't happen, so the caller can treat it as fatal). Note any
    /// complete frames decoded *before* the oversize one in the same call are discarded with it —
    /// the stream is desynced past that point, so they have no value.
    pub fn drain(&mut self) -> Result<Vec<(PeerId, Vec<u8>)>, FrameTooLarge> {
        let mut out = Vec::new();
        let mut pos = 0;
        while self.inbuf.len() - pos >= FRAME_HEADER_LEN {
            let len = u32::from_be_bytes(self.inbuf[pos..pos + 4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                self.inbuf.clear();
                return Err(FrameTooLarge(len));
            }
            if self.inbuf.len() - pos < FRAME_HEADER_LEN + len {
                break; // header says more payload than we've received yet
            }
            let sender = u64::from_be_bytes(self.inbuf[pos + 4..pos + 12].try_into().unwrap());
            let payload = self.inbuf[pos + FRAME_HEADER_LEN..pos + FRAME_HEADER_LEN + len].to_vec();
            out.push((sender, payload));
            pos += FRAME_HEADER_LEN + len;
        }
        self.inbuf.drain(..pos);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(dec: &mut FrameDecoder, bytes: &[u8]) -> Vec<(PeerId, Vec<u8>)> {
        dec.push(bytes);
        dec.drain().unwrap()
    }

    #[test]
    fn round_trips_a_single_frame() {
        let mut dec = FrameDecoder::new();
        let got = feed(&mut dec, &encode_frame(7, b"hello"));
        assert_eq!(got, vec![(7u64, b"hello".to_vec())]);
    }

    #[test]
    fn decodes_multiple_frames_in_one_read() {
        let mut buf = encode_frame(1, b"a");
        buf.extend(encode_frame(2, b"bb"));
        let mut dec = FrameDecoder::new();
        let got = feed(&mut dec, &buf);
        assert_eq!(got, vec![(1, b"a".to_vec()), (2, b"bb".to_vec())]);
    }

    #[test]
    fn reassembles_a_frame_split_across_reads() {
        let frame = encode_frame(9, b"split me");
        let (head, tail) = frame.split_at(FRAME_HEADER_LEN + 3);
        let mut dec = FrameDecoder::new();
        // The first chunk has a partial payload: nothing complete yet.
        assert!(feed(&mut dec, head).is_empty());
        // The rest completes it.
        assert_eq!(feed(&mut dec, tail), vec![(9, b"split me".to_vec())]);
    }

    #[test]
    fn empty_payload_frame_round_trips() {
        let mut dec = FrameDecoder::new();
        assert_eq!(feed(&mut dec, &encode_frame(3, b"")), vec![(3, Vec::new())]);
    }

    #[test]
    fn exactly_max_frame_is_accepted() {
        // The boundary itself is legal (`len > MAX_FRAME` is strict) — guards a regression to `>=`
        // that would reject a max-size frame.
        let payload = vec![0xABu8; MAX_FRAME];
        let mut dec = FrameDecoder::new();
        let got = feed(&mut dec, &encode_frame(1, &payload));
        assert_eq!(got, vec![(1, payload)]);
    }

    #[test]
    fn buffers_a_partial_second_frame_after_a_whole_first() {
        // A complete frame followed by a partial one: the first is yielded, the partial remainder
        // stays buffered (guards the pos-advance vs `drain(..pos)` bookkeeping), then completes.
        let f1 = encode_frame(1, b"first");
        let f2 = encode_frame(2, b"second");
        let mut dec = FrameDecoder::new();
        dec.push(&f1);
        dec.push(&f2[..FRAME_HEADER_LEN + 2]); // only part of f2's payload
        assert_eq!(dec.drain().unwrap(), vec![(1, b"first".to_vec())]);
        // A second drain with no new bytes must not consume or corrupt the buffered partial.
        assert!(dec.drain().unwrap().is_empty());
        dec.push(&f2[FRAME_HEADER_LEN + 2..]);
        assert_eq!(dec.drain().unwrap(), vec![(2, b"second".to_vec())]);
    }

    #[test]
    fn full_width_sender_id_round_trips() {
        // The sender is a full u64 (a Steam id in production); a truncation/byte-slice regression
        // would corrupt it.
        let mut dec = FrameDecoder::new();
        let got = feed(&mut dec, &encode_frame(u64::MAX, b"z"));
        assert_eq!(got, vec![(u64::MAX, b"z".to_vec())]);
        let id = 0xDEAD_BEEF_FEED_FACE;
        assert_eq!(feed(&mut dec, &encode_frame(id, b"")), vec![(id, Vec::new())]);
    }

    #[test]
    fn oversize_frame_is_rejected_and_buffer_cleared() {
        let mut dec = FrameDecoder::new();
        // Hand-craft a header claiming MAX_FRAME+1 bytes.
        let mut bad = ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec();
        bad.extend_from_slice(&5u64.to_be_bytes());
        dec.push(&bad);
        assert_eq!(dec.drain(), Err(FrameTooLarge(MAX_FRAME + 1)));
        // Buffer was cleared, so a subsequent good frame decodes cleanly (resync).
        assert_eq!(feed(&mut dec, &encode_frame(1, b"ok")), vec![(1, b"ok".to_vec())]);
    }

    #[test]
    fn one_byte_under_max_frame_is_accepted() {
        // The low side of the MAX_FRAME boundary: a frame one byte under the cap must decode normally.
        // Together with `exactly_max_frame_is_accepted` (at the cap) and
        // `oversize_frame_is_rejected_and_buffer_cleared` (one over), this pins all three boundary cases.
        let payload = vec![0xCDu8; MAX_FRAME - 1];
        let mut dec = FrameDecoder::new();
        let got = feed(&mut dec, &encode_frame(1, &payload));
        assert_eq!(got, vec![(1, payload)]);
    }

    #[test]
    fn max_u32_length_prefix_is_rejected_without_overrun() {
        // The largest value the u32 length field can hold. The decoder must reject it as FrameTooLarge
        // *before* trying to size the buffer to ~4 GiB — a bound check that ran after the add would
        // either over-allocate or overflow `FRAME_HEADER_LEN + len`.
        let mut dec = FrameDecoder::new();
        let mut bad = u32::MAX.to_be_bytes().to_vec();
        bad.extend_from_slice(&7u64.to_be_bytes());
        dec.push(&bad);
        assert_eq!(dec.drain(), Err(FrameTooLarge(u32::MAX as usize)));
        // And it resyncs cleanly afterward.
        assert_eq!(feed(&mut dec, &encode_frame(1, b"ok")), vec![(1, b"ok".to_vec())]);
    }

    #[test]
    fn drain_never_panics_on_arbitrary_bytes() {
        // SplitMix64 — deterministic, dependency-free (mirrors protocol.rs's fuzz PRNG).
        struct Rng(u64);
        impl Rng {
            fn next_u64(&mut self) -> u64 {
                self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = self.0;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^ (z >> 31)
            }
        }
        // Feed pseudo-random bytes in pseudo-random chunk sizes. `drain` must always return a `Result`
        // (Ok frames or FrameTooLarge) and never panic on a malformed/partial header, an absurd length,
        // or a chunk split mid-header. A FrameTooLarge clears the buffer; we keep going to resync.
        let mut rng = Rng(0xF00D_FACE_1234_5678);
        let mut dec = FrameDecoder::new();
        for _ in 0..20_000 {
            let chunk = (rng.next_u64() % 24) as usize;
            let mut buf = Vec::with_capacity(chunk);
            for _ in 0..chunk {
                buf.push(rng.next_u64() as u8);
            }
            dec.push(&buf);
            if dec.drain().is_err() {
                // Buffer was cleared by the oversize path; the decoder stays usable.
                dec = FrameDecoder::new();
            }
        }
    }

    #[test]
    fn header_split_across_reads_reassembles_correctly() {
        // Split a real frame *inside* its 12-byte header (non-zero len + sender), so a
        // mis-positioned header parse would change the decoded (sender, payload), not just yield
        // zeros that pass regardless.
        let frame = encode_frame(0xAABB_CCDD_1122_3344, b"hi");
        let mut dec = FrameDecoder::new();
        dec.push(&frame[..2]); // only 2 of the 4 length bytes
        assert!(dec.drain().unwrap().is_empty());
        dec.push(&frame[2..]); // the rest of the header + the payload
        assert_eq!(dec.drain().unwrap(), vec![(0xAABB_CCDD_1122_3344, b"hi".to_vec())]);
    }
}
