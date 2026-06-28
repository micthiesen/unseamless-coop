//! Overhead-nameplate **label content** — what text sits over a co-op partner's head.
//!
//! Pure + host-tested: given the player's chosen [`OverheadDisplay`] mode and one peer's stats
//! ([`PeerLabelData`]), [`nameplate_lines`] returns the label text to draw. The cdylib's nameplate
//! feature (`coop/features/nameplates.rs`) builds a [`PeerLabelData`] per peer and renders through
//! this one seam, so once the co-op/session core lands (rung 3) the *only* remaining work is filling
//! the real per-peer fields — the formatting is already decided and verified here.
//!
//! ## Conventions
//! - **Other players only.** The peer's name is always the first line; you never label yourself in
//!   real play (the cdylib's `show_self` debug knob bypasses this formatter entirely).
//! - **Missing data is omitted, not placeholdered.** A stat that isn't available yet (`None` —
//!   every stat is `None` until the session core feeds real values) simply drops its line, so a
//!   peer renders name-only rather than with noisy `--` placeholders. This means today, with all
//!   stats `None`, every mode degrades cleanly to just the name (no regression over the prior
//!   `Player N` placeholder).
//! - **ASCII-only output.** The imgui overlay's bitmap font has no em-dash / ellipsis glyph (see
//!   ROADMAP), so labels must stay 7-bit ASCII — pinned by a test below.

use crate::config::OverheadDisplay;

/// One co-op partner's nameplate stats, as the session core will eventually supply them. Each stat is
/// optional because the real values are rung-3-gated: until the session layer maps a phantom to an
/// identity, the cdylib builds this with a placeholder `name` and every stat `None`. Filling the real
/// fields at that one seam is all that's left to light up live labels.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PeerLabelData {
    /// The peer's display name (a `Player N` placeholder until the session core supplies the real one).
    pub name: String,
    /// Round-trip latency in milliseconds, or `None` if not yet known.
    pub ping_ms: Option<u32>,
    /// The peer's soul level, or `None` if not yet known.
    pub soul_level: Option<u32>,
    /// How many times the peer has died this session, or `None` if not yet known.
    pub death_count: Option<u32>,
}

impl PeerLabelData {
    /// A label carrying only a name (no stats yet) — the rung-3-pending case the cdylib uses today.
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }
}

/// The label lines to draw over `peer`, per the chosen `mode`, or `None` when the mode shows no
/// overhead label at all ([`OverheadDisplay::None`] — the caller skips drawing entirely).
///
/// The first line is always the peer's name; per-mode stat lines follow, in a fixed order, and any
/// stat that's `None` is omitted (so an unavailable stat costs a line, not a placeholder). The result
/// is always ASCII (see the module docs).
pub fn nameplate_lines(mode: OverheadDisplay, peer: &PeerLabelData) -> Option<Vec<String>> {
    if mode == OverheadDisplay::None {
        return None;
    }

    let mut lines = vec![peer.name.clone()];

    // Stat lines, in display order. `SoulLevelAndPing` shows both (soul level first, then ping); the
    // single-stat modes show just their one. A `None` stat contributes nothing.
    let show_soul_level = matches!(mode, OverheadDisplay::SoulLevel | OverheadDisplay::SoulLevelAndPing);
    let show_ping = matches!(mode, OverheadDisplay::Ping | OverheadDisplay::SoulLevelAndPing);
    let show_deaths = matches!(mode, OverheadDisplay::DeathCount);

    if show_soul_level && let Some(sl) = peer.soul_level {
        lines.push(format!("SL {sl}"));
    }
    if show_ping && let Some(ping) = peer.ping_ms {
        lines.push(format!("{ping} ms"));
    }
    if show_deaths && let Some(deaths) = peer.death_count {
        lines.push(format!("Deaths {deaths}"));
    }

    Some(lines)
}

/// The drawable label text for `peer` (the [`nameplate_lines`] joined with newlines, which the overlay
/// renders as a stacked block), or `None` when the mode suppresses the label entirely. This is the
/// shape the cdylib's `NameplateLabel.text` wants — one string per peer.
pub fn nameplate_text(mode: OverheadDisplay, peer: &PeerLabelData) -> Option<String> {
    nameplate_lines(mode, peer).map(|lines| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A peer with every stat populated, for the all-data-present cases.
    fn full_peer() -> PeerLabelData {
        PeerLabelData {
            name: "Tarnished".into(),
            ping_ms: Some(45),
            soul_level: Some(120),
            death_count: Some(3),
        }
    }

    #[test]
    fn none_mode_suppresses_the_label() {
        // `None` means "show nothing overhead" — the caller draws no plate at all.
        assert_eq!(nameplate_lines(OverheadDisplay::None, &full_peer()), None);
        assert_eq!(nameplate_text(OverheadDisplay::None, &full_peer()), None);
    }

    #[test]
    fn normal_mode_is_name_only() {
        // Even with every stat available, Normal shows just the name.
        assert_eq!(
            nameplate_lines(OverheadDisplay::Normal, &full_peer()),
            Some(vec!["Tarnished".to_string()])
        );
    }

    #[test]
    fn ping_mode_with_data() {
        assert_eq!(
            nameplate_lines(OverheadDisplay::Ping, &full_peer()),
            Some(vec!["Tarnished".to_string(), "45 ms".to_string()])
        );
    }

    #[test]
    fn soul_level_mode_with_data() {
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevel, &full_peer()),
            Some(vec!["Tarnished".to_string(), "SL 120".to_string()])
        );
    }

    #[test]
    fn death_count_mode_with_data() {
        assert_eq!(
            nameplate_lines(OverheadDisplay::DeathCount, &full_peer()),
            Some(vec!["Tarnished".to_string(), "Deaths 3".to_string()])
        );
    }

    #[test]
    fn soul_level_and_ping_shows_both_in_order() {
        // Soul level first, then ping — a stable order so a peer's label doesn't reshuffle frame to frame.
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevelAndPing, &full_peer()),
            Some(vec!["Tarnished".to_string(), "SL 120".to_string(), "45 ms".to_string()])
        );
    }

    #[test]
    fn missing_stat_is_omitted_not_placeholdered() {
        // A `None` stat drops its line rather than rendering a placeholder — so a single-stat mode
        // with no data degrades to name-only.
        let no_ping = PeerLabelData { ping_ms: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::Ping, &no_ping),
            Some(vec!["Tarnished".to_string()])
        );

        let no_sl = PeerLabelData { soul_level: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevel, &no_sl),
            Some(vec!["Tarnished".to_string()])
        );

        let no_deaths = PeerLabelData { death_count: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::DeathCount, &no_deaths),
            Some(vec!["Tarnished".to_string()])
        );
    }

    #[test]
    fn soul_level_and_ping_with_partial_data() {
        // Only one of the two present → only that line follows the name; the other is omitted, and the
        // surviving stat keeps its place in the fixed order.
        let only_sl = PeerLabelData { ping_ms: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevelAndPing, &only_sl),
            Some(vec!["Tarnished".to_string(), "SL 120".to_string()])
        );

        let only_ping = PeerLabelData { soul_level: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevelAndPing, &only_ping),
            Some(vec!["Tarnished".to_string(), "45 ms".to_string()])
        );

        let neither = PeerLabelData { soul_level: None, ping_ms: None, ..full_peer() };
        assert_eq!(
            nameplate_lines(OverheadDisplay::SoulLevelAndPing, &neither),
            Some(vec!["Tarnished".to_string()])
        );
    }

    #[test]
    fn all_stats_none_is_name_only_for_every_mode() {
        // The rung-3-pending reality: every stat is `None`, so every mode (bar `None`) renders just the
        // name — exactly the prior `Player N` behavior, no regression.
        let bare = PeerLabelData::named("Player 1");
        for mode in OverheadDisplay::ALL {
            if mode == OverheadDisplay::None {
                assert_eq!(nameplate_lines(mode, &bare), None);
            } else {
                assert_eq!(
                    nameplate_lines(mode, &bare),
                    Some(vec!["Player 1".to_string()]),
                    "mode {mode:?} should be name-only when no stats are available"
                );
            }
        }
    }

    #[test]
    fn output_is_ascii_only() {
        // The overlay's bitmap font has no em-dash/ellipsis glyph, so every label line must be 7-bit
        // ASCII across every mode and data shape.
        let peers = [full_peer(), PeerLabelData::named("Player 2"), PeerLabelData::default()];
        for peer in &peers {
            for mode in OverheadDisplay::ALL {
                if let Some(text) = nameplate_text(mode, peer) {
                    assert!(text.is_ascii(), "non-ASCII label for {mode:?}: {text:?}");
                }
            }
        }
    }

    #[test]
    fn text_is_a_single_line_when_no_stats_follow() {
        // The only shape rendered today (all stats `None`) — name only, no trailing newline.
        assert_eq!(nameplate_text(OverheadDisplay::Normal, &full_peer()), Some("Tarnished".to_string()));
        assert_eq!(
            nameplate_text(OverheadDisplay::Ping, &PeerLabelData::named("Player 1")),
            Some("Player 1".to_string())
        );
    }

    #[test]
    fn text_joins_lines_with_newlines() {
        // The drawable form is the lines stacked with '\n' (how the overlay renders a multi-line plate).
        assert_eq!(
            nameplate_text(OverheadDisplay::SoulLevelAndPing, &full_peer()),
            Some("Tarnished\nSL 120\n45 ms".to_string())
        );
    }
}
