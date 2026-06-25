//! Session observation harness — the primary tool for unblocking the co-op core on the rig.
//!
//! It reads `CSSessionManager` each frame and logs, on every change: the lobby/protocol state
//! machine, the connected-player roster, the session player limit, and the per-player scaling
//! multipliers our (host-tested) [`unseamless_core::scaling`] math would produce for the current
//! party size. It writes nothing — pure observation, safe to run anywhere.
//!
//! Why this first: the co-op core (relaxing player limits, persistent sessions, sync) hinges on
//! understanding this state machine and which count is the true "players in my world". That can
//! only be learned by watching it live, so this is what we hand to the rig; the log it produces
//! is the spec for the next phase.

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex};
use unseamless_core::config::Config;
use unseamless_core::util::{FrameThrottle, Latch};

use crate::feature::{Feature, Tick};

pub struct SessionObserver {
    config: Config,
    /// Fires only when the watched session state changes, so we log transitions not every frame.
    state: Latch<Snapshot>,
    /// "Still alive, no session yet" heartbeat (~30s at 60fps) while idle at the title screen.
    heartbeat: FrameThrottle,
}

/// The subset of session state we diff on.
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    lobby: u32,
    protocol: u32,
    players: usize,
    limit: u32,
    /// `session_player_limit_override` — `1` is vanilla ("use the per-context default"); our
    /// session-limit feature writes our cap here, so diffing on it makes that write visible.
    limit_override: u32,
}

impl SessionObserver {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            state: Latch::new(),
            heartbeat: FrameThrottle::every(1800),
        }
    }
}

impl Feature for SessionObserver {
    fn name(&self) -> &'static str {
        "session-observer"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, tick: Tick) {
        let observed = crate::sdk::with_instance::<CSSessionManager, _>(|s| self.observe(s));
        if observed.is_none() && self.heartbeat.tick() {
            log::info!("observer live; no CSSessionManager yet (frame {})", tick.frame);
        }
    }
}

impl SessionObserver {
    /// Log the session state if it changed since last frame.
    fn observe(&mut self, session: &CSSessionManager) {
        let players = session.players.len();
        let snapshot = Snapshot {
            lobby: session.lobby_state as u32,
            protocol: session.protocol_state as u32,
            players,
            limit: session.session_player_limit,
            limit_override: session.session_player_limit_override,
        };

        if !self.state.changed(&snapshot) {
            return;
        }

        log::info!(
            "session change: lobby={:?} protocol={:?} players={} limit={} override={}",
            session.lobby_state,
            session.protocol_state,
            players,
            session.session_player_limit,
            session.session_player_limit_override,
        );
        for (i, p) in session.players.iter().enumerate() {
            // Pseudonymous tag, not the raw 64-bit Steam ID: this log is shareable, and a raw
            // SteamID would leak other players' identities (see diagnostics::peer_tag). `cid` is a
            // game-internal character/event id (not resolvable to a person), kept for rig debugging.
            log::info!(
                "  player[{i}] {} host={} local={} cid={}",
                unseamless_core::diagnostics::peer_tag(p.base.steam_id),
                p.is_host,
                p.is_local_player,
                p.character_event_id,
            );
        }

        // What scaling WOULD be for this party size, via the host-tested core. The exact
        // player-count source and application mechanism are RE-gated; this logs the candidate so
        // we can confirm it on the rig. The core's multiplier math saturates for any count, so we
        // only need to guard the usize->u32 narrowing.
        let count = u32::try_from(players).unwrap_or(u32::MAX).max(1);
        let enemy = self.config.scaling.enemy_multipliers(count);
        let boss = self.config.scaling.boss_multipliers(count);
        log::info!(
            "  scaling@{count}p: enemy(hp×{:.2} dmg×{:.2} pos×{:.2}) boss(hp×{:.2} dmg×{:.2} pos×{:.2})",
            enemy.health,
            enemy.damage,
            enemy.posture,
            boss.health,
            boss.damage,
            boss.posture,
        );
    }
}
