//! Spectate-on-death target selection — the pure, host-tested decision behind the
//! `always_spectate_on_death` modifier (`coop/features/spectate.rs`).
//!
//! When the local player dies in a co-op session, instead of staring at their own corpse the camera
//! follows a living partner until they revive. *Which* partner to follow each frame is the only real
//! decision, and it must be **sticky**: pick one and stay on them while they live, so the view doesn't
//! flicker between teammates as the roster reorders frame to frame (a phantom set's iteration order is
//! not stable across joins/leaves — see `coop/features/nameplates.rs`). This module is that choice and
//! nothing else; the cdylib feeds it the live roster and applies the result to the game camera.
//!
//! Kept here (not in the cdylib) so the policy is unit-tested on the host, per the project's
//! "decision logic in core" split (CLAUDE.md > Code layout).

/// One co-op partner the local (dead) player could spectate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpectateCandidate {
    /// Stable per-partner identity. The cdylib uses the phantom `ChrIns` pointer as the handle until
    /// the session core wires real SteamIDs (the same stand-in `nameplates` uses for its color key) —
    /// constant for a loaded phantom across frames, which is exactly what stickiness needs.
    pub id: u64,
    /// Whether this partner is currently alive (a dead partner is no use to spectate).
    pub alive: bool,
}

impl SpectateCandidate {
    pub fn new(id: u64, alive: bool) -> Self {
        Self { id, alive }
    }
}

/// Choose the partner to follow this frame, sticky on the current target:
/// - **Keep `current`** while it is still present *and* alive — no per-frame flicker between teammates.
/// - Otherwise **adopt the first living candidate** in roster order (the current target died, left, or
///   we weren't spectating anyone yet).
/// - `None` when no partner is alive — the caller then releases the camera (there's nothing to watch).
///
/// Pure: the caller owns all liveness/availability reads; this only arbitrates between them.
pub fn select_target(candidates: &[SpectateCandidate], current: Option<u64>) -> Option<u64> {
    if let Some(cur) = current
        && candidates.iter().any(|c| c.id == cur && c.alive)
    {
        return Some(cur);
    }
    candidates.iter().find(|c| c.alive).map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(id: u64) -> SpectateCandidate {
        SpectateCandidate::new(id, true)
    }
    fn dead(id: u64) -> SpectateCandidate {
        SpectateCandidate::new(id, false)
    }

    #[test]
    fn no_candidates_yields_none() {
        assert_eq!(select_target(&[], None), None);
        assert_eq!(select_target(&[], Some(7)), None);
    }

    #[test]
    fn all_dead_yields_none() {
        assert_eq!(select_target(&[dead(1), dead(2)], None), None);
        // even if we were spectating one of them, a now-dead roster releases.
        assert_eq!(select_target(&[dead(1), dead(2)], Some(1)), None);
    }

    #[test]
    fn picks_first_living_when_not_yet_spectating() {
        assert_eq!(select_target(&[dead(1), alive(2), alive(3)], None), Some(2));
    }

    #[test]
    fn sticky_keeps_current_while_alive_despite_order() {
        // 3 is listed last but is the current target and still alive → stay on 3, don't snap to 1.
        assert_eq!(select_target(&[alive(1), alive(2), alive(3)], Some(3)), Some(3));
    }

    #[test]
    fn switches_when_current_dies() {
        assert_eq!(select_target(&[alive(1), dead(3)], Some(3)), Some(1));
    }

    #[test]
    fn switches_when_current_leaves_roster() {
        // current target 9 is gone from the roster entirely → adopt the first living one.
        assert_eq!(select_target(&[alive(1), alive(2)], Some(9)), Some(1));
    }
}
