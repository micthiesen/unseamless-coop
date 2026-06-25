//! Shared read of `CSSessionManager` into a plain [`SessionView`], so the session observer and the
//! diagnostic report read the session **the same way** (no drift between the two surfaces) and the
//! warp-data null-guard lives in **one** place — a live `CSSessionManager` doesn't guarantee its
//! `stay_in_multiplay_area_warp_data` `OwnedPtr` is wired, so the deref is guarded here for both
//! consumers. Reads only; writes nothing.

use eldenring::cs::{CSSessionManager, CSStayInMultiplayAreaWarpData, LobbyState, ProtocolState};

/// A plain snapshot of the session-wide state both the observer (for change-detection + logging) and
/// the diagnostic report read. The per-player roster is *not* here — only the observer iterates it,
/// and it carries pseudonymized identity that each consumer handles itself.
pub struct SessionView {
    pub lobby_state: LobbyState,
    pub protocol_state: ProtocolState,
    pub players: usize,
    pub player_limit: u32,
    /// `session_player_limit_override` — `1` is vanilla; our `session_limit` feature writes the cap here.
    pub limit_override: u32,
    /// The "stay in multiplay area" tether governance, or `None` when its pointer isn't initialized
    /// yet (pre-session) — so both consumers degrade gracefully instead of dereferencing a null.
    pub tether: Option<TetherView>,
}

/// `CSStayInMultiplayAreaWarpData` — the co-op area tether the seamless-roam feature relaxes.
pub struct TetherView {
    /// `disable_multiplay_restriction` — the "go anywhere on the map" lever.
    pub restriction_disabled: bool,
    /// `multiplay_start_area_id` — host's play-area id at connect (`0` disables the mismatch warp).
    pub start_area_id: u32,
    /// `warp_request_delay` — raw countdown until a pending tether warp fires (task-driven; the SDK
    /// doesn't assert a per-frame cadence). `0` when no warp is pending.
    pub warp_request_delay: f32,
    /// `is_warp_possible` — the game's gate on whether the tether warp may fire this frame.
    pub warp_possible: bool,
    /// `player_fade_tracker.len()` — remote players currently faded out mid-warp.
    pub fading_players: usize,
}

impl TetherView {
    /// A tether warp is pending. The raw delay ticks every frame, so callers diff on this bool and
    /// log [`warp_request_delay`](TetherView::warp_request_delay) separately.
    pub fn warp_pending(&self) -> bool {
        self.warp_request_delay > 0.0
    }
}

/// Read the session-wide state. The warp-data deref is null-guarded (reading the `OwnedPtr`'s address
/// is not a deref): `tether` is `None` when it isn't wired, so neither consumer can null-deref.
pub fn read(s: &CSSessionManager) -> SessionView {
    let tether = (!s.stay_in_multiplay_area_warp_data.as_ptr().is_null()).then(|| {
        let w = &*s.stay_in_multiplay_area_warp_data;
        TetherView {
            restriction_disabled: w.disable_multiplay_restriction,
            start_area_id: w.multiplay_start_area_id,
            warp_request_delay: w.warp_request_delay,
            warp_possible: w.is_warp_possible,
            fading_players: w.player_fade_tracker.len(),
        }
    });
    SessionView {
        lobby_state: s.lobby_state,
        protocol_state: s.protocol_state,
        players: s.players.len(),
        player_limit: s.session_player_limit,
        limit_override: s.session_player_limit_override,
        tether,
    }
}

/// Mutable access to the tether warp data, with the same null-guard as [`read`] — so the *write*
/// path (the seamless-roam feature) and the read path share one place that knows the `OwnedPtr` may
/// be unwired pre-session. `None` when it isn't wired; the caller skips its write.
pub fn tether_mut(s: &mut CSSessionManager) -> Option<&mut CSStayInMultiplayAreaWarpData> {
    if s.stay_in_multiplay_area_warp_data.as_ptr().is_null() {
        return None;
    }
    Some(&mut s.stay_in_multiplay_area_warp_data)
}
