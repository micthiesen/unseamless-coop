//! Relax the co-op session player cap.
//!
//! The game caps a session at 4 (open world) / 6 (arena). `CSSessionManager` exposes
//! `session_player_limit_override`: it's `1` by default ("use the per-context default"), and if set
//! to anything else the game uses that value as the limit for every session. We write our configured
//! [`max_players`](unseamless_core::config::Session::max_players) there so a party can exceed the
//! vanilla cap — the documented, low-risk lever (a single `u32` field), not a code hook.
//!
//! It reads the **live config** each frame (`crate::state`) rather than a construction snapshot, so
//! a config change — e.g. a `ConfigSync` the bridge applies — re-applies here without rebuilding the
//! feature. It writes only when the game's value differs from the desired one, so it's a self-healing
//! one-shot per value. Writing it before a session forms is fine — it's read when the session is
//! created. The multi-player *effect* is rig-gated (needs a real party); the write itself is
//! observable solo (the observer logs the override, and a config-driven change re-logs it here).

use eldenring::cs::CSSessionManager;
use unseamless_core::config::{MAX_SESSION_PLAYERS, MIN_SESSION_PLAYERS};

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct SessionLimit {
    /// The last override value logged at `info`, so steady-state re-asserts stay at `debug` but a
    /// genuine change (config-driven) logs loudly.
    last_logged: Option<u32>,
}

impl SessionLimit {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Feature for SessionLimit {
    fn name(&self) -> &'static str {
        "session-limit"
    }

    // Default phase (`FrameBegin`): the override is plain session config, not frame-order-sensitive
    // game state, and it must be set before a session forms — which can happen at the menu.

    fn on_frame(&mut self, _tick: Tick) {
        // Self-bound the value we write into live game memory: the config is clamped on load and on
        // the wire, but this feature shouldn't trust that every path did so.
        let desired = crate::state::with(|c| c.session.max_players)
            .clamp(MIN_SESSION_PLAYERS, MAX_SESSION_PLAYERS);

        // `Some(true)` = we just wrote it; `Some(false)` = already correct; `None` = no session
        // manager live yet (pre-init / between loads) — retry next frame.
        let wrote = crate::sdk::with_instance_mut::<CSSessionManager, _>(|s| {
            if s.session_player_limit_override == desired {
                return false;
            }
            s.session_player_limit_override = desired;
            true
        });

        if wrote == Some(true) {
            if self.last_logged == Some(desired) {
                log::debug!("re-applied session player limit override = {desired}");
            } else {
                log::info!("session player limit override set to {desired}");
                self.last_logged = Some(desired);
            }
        }
    }
}
