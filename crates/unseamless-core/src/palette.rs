//! Distinct per-peer colors for overhead nameplates (and their future at-distance dots).
//!
//! A small fixed palette of clear, high-value hues, indexed by peer slot so each co-op partner gets a
//! visually distinct, readable color. Pure + host-tested: the cdylib's nameplate feature maps a peer to
//! an index and calls [`peer_color`], and the overlay tints the label/dot with it.
//!
//! **Stability caveat (co-op-core TODO):** today the cdylib indexes by *roster iteration order*, which
//! isn't stable as peers join/leave — so a player's color can shift. The fix is to key the index off a
//! stable per-peer identity (the SteamID) once the real peer feed lands (rung 3); this palette is the
//! ready-to-wire half. See `crates/unseamless-coop/src/features/nameplates.rs`.

/// Clear, distinct, high-value RGB colors (each component `0.0..=1.0`), chosen to read well as text
/// **and** as a small dot over the game world. Length is headroom over the 6-player session cap so a
/// full party never has to wrap.
const PALETTE: [[f32; 3]; 8] = [
    [1.00, 0.82, 0.40], // amber
    [0.40, 0.80, 1.00], // sky blue
    [0.55, 0.95, 0.55], // light green
    [1.00, 0.55, 0.80], // pink
    [1.00, 0.65, 0.30], // orange
    [0.70, 0.66, 1.00], // lavender
    [0.45, 0.95, 0.85], // teal
    [1.00, 0.52, 0.52], // coral
];

/// The color for the peer at `index` (a peer slot, not an identity — see the stability caveat in the
/// module docs). Wraps the palette so any index is valid; the same index always yields the same color.
pub fn peer_color(index: usize) -> [f32; 3] {
    PALETTE[index % PALETTE.len()]
}

/// How many distinct colors before [`peer_color`] repeats.
pub fn palette_len() -> usize {
    PALETTE.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_in_range() {
        for c in PALETTE {
            for ch in c {
                assert!((0.0..=1.0).contains(&ch), "channel {ch} out of range in {c:?}");
            }
        }
    }

    #[test]
    fn all_palette_entries_are_distinct() {
        for i in 0..PALETTE.len() {
            for j in (i + 1)..PALETTE.len() {
                assert_ne!(PALETTE[i], PALETTE[j], "palette {i} and {j} are identical");
            }
        }
    }

    #[test]
    fn peer_color_is_stable_and_wraps() {
        // Same index → same color (a player's color can't flicker for a fixed slot).
        assert_eq!(peer_color(2), peer_color(2));
        // Distinct within a party of palette_len.
        assert_eq!(peer_color(0), PALETTE[0]);
        assert_eq!(peer_color(palette_len() - 1), PALETTE[palette_len() - 1]);
        // Wraps past the palette length rather than panicking on a large index.
        assert_eq!(peer_color(palette_len()), peer_color(0));
        assert_eq!(peer_color(palette_len() + 3), peer_color(3));
    }
}
