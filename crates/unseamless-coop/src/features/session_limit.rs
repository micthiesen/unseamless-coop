//! Relax the co-op session player cap.
//!
//! The game caps a session at 4 (open world) / 6 (arena). `CSSessionManager` exposes
//! `session_player_limit_override`: it's `1` by default ("use the per-context default"), and if set
//! to anything else the game uses that value as the limit for every session. We write our configured
//! [`max_players`](unseamless_core::config::Session::max_players) there so a party can exceed the
//! vanilla cap — the documented, low-risk lever (a single `u32` field), not a code hook.
//!
//! We write it only when it differs from our target, so it's effectively one-shot once a session
//! manager exists, and self-heals if the game ever resets it. Writing it before a session forms is
//! fine — it's read when the session is created. This is rig-gated for the *multi-player effect*
//! (needs a real party), but the write itself is observable solo (the observer logs the override).

use eldenring::cs::CSSessionManager;
use unseamless_core::config::{MAX_SESSION_PLAYERS, MIN_SESSION_PLAYERS};

use crate::feature::{Feature, Tick};

pub struct SessionLimit {
    /// The override value to hold the game's `session_player_limit_override` at.
    target: u32,
    /// Whether we've logged the initial apply (so re-asserts log quietly at `debug`, not `info`).
    applied: bool,
}

impl SessionLimit {
    pub fn new(max_players: u32) -> Self {
        // Self-bound the value we write into live game memory: config validation already clamps,
        // but this feature shouldn't trust that every future caller ran `Config::validate`.
        let target = max_players.clamp(MIN_SESSION_PLAYERS, MAX_SESSION_PLAYERS);
        Self { target, applied: false }
    }
}

impl Feature for SessionLimit {
    fn name(&self) -> &'static str {
        "session-limit"
    }

    // Default phase (`FrameBegin`): the override is plain session config, not frame-order-sensitive
    // game state, and it must be set before a session forms — which can happen at the menu.

    fn on_frame(&mut self, _tick: Tick) {
        let target = self.target;
        // `Some(true)` = we just wrote it; `Some(false)` = already at target; `None` = no session
        // manager live yet (pre-init / between loads) — retry next frame.
        let wrote = crate::sdk::with_instance_mut::<CSSessionManager, _>(|s| {
            if s.session_player_limit_override == target {
                return false;
            }
            s.session_player_limit_override = target;
            true
        });

        if wrote == Some(true) {
            if self.applied {
                log::debug!("re-applied session player limit override = {target}");
            } else {
                log::info!("session player limit override set to {target}");
                self.applied = true;
            }
        }
    }
}
