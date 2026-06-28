//! Distinct per-peer colors for overhead nameplates (and their future at-distance dots).
//!
//! A small fixed palette of clear, high-value hues, indexed by peer slot so each co-op partner gets a
//! visually distinct, readable color. Pure + host-tested: the cdylib's nameplate feature maps a peer to
//! an index and calls [`peer_color`], and the overlay tints the label/dot with it.
//!
//! **Stable assignment:** [`peer_color_for_id`] keys a peer's color off a *stable identity* rather than
//! roster slot, so it can't flicker as peers join/leave (the bug [`peer_color`]'s iteration index had).
//! The cdylib wires this with the phantom's `ChrIns` pointer as the stable per-peer handle today; it
//! swaps to the SteamID once the real peer feed lands (rung 3). See
//! `crates/unseamless-coop/src/features/nameplates.rs`.

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

/// The color for a peer keyed off a **stable identity** rather than roster slot — the fix the module
/// docs call for. Any stable per-peer `u64` (ultimately the SteamID; a stable per-phantom handle until
/// the session core lands) maps to a fixed palette entry, so the *same* peer keeps the *same* color for
/// the whole session regardless of join/leave order (which is what shifts [`peer_color`]'s slot index
/// and makes a color flicker). A SteamID has no useful low-bit spread on its own (the universe/instance
/// bits sit high and account-type bits low), so we hash before folding to the palette so two nearby IDs
/// don't collide onto the same hue; the same `id` always yields the same color (pure, deterministic).
pub fn peer_color_for_id(id: u64) -> [f32; 3] {
    PALETTE[(mix64(id) % PALETTE.len() as u64) as usize]
}

/// A cheap, deterministic 64-bit bit-mixer (the SplitMix64 finalizer) used to spread structured ids
/// (SteamIDs, pointers) across the palette before folding with `%`. Not cryptographic — just enough
/// avalanche that ids differing in only the high or low bits don't land on the same palette index.
fn mix64(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
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
        for (i, a) in PALETTE.iter().enumerate() {
            for (j, b) in PALETTE.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "palette {i} and {j} are identical");
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

    #[test]
    fn peer_color_for_id_is_deterministic_and_a_palette_entry() {
        // Same id → same color every call (stability across frames is the whole point — a peer keyed
        // by SteamID can't flicker even as the roster reorders).
        for id in [0_u64, 1, 7, 76561198000000000, u64::MAX] {
            assert_eq!(peer_color_for_id(id), peer_color_for_id(id), "id {id} not deterministic");
            assert!(PALETTE.contains(&peer_color_for_id(id)), "id {id} mapped outside the palette");
        }
    }

    #[test]
    fn peer_color_for_id_spreads_structured_ids_across_the_palette() {
        // SteamIDs in a real party are consecutive-ish (differ in only a few low bits) and would all
        // fold onto one hue without the mix. Across a realistic 6-peer run of adjacent ids, the hashed
        // fold must touch several distinct palette entries — not pile everyone onto one color.
        let base = 76561198000000000_u64;
        let colors: std::collections::HashSet<[u32; 3]> = (0..6)
            .map(|i| peer_color_for_id(base + i))
            .map(|c| [c[0].to_bits(), c[1].to_bits(), c[2].to_bits()])
            .collect();
        assert!(colors.len() >= 4, "adjacent SteamIDs collapsed to {} colors", colors.len());
    }
}
