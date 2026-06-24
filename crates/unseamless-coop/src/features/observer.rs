//! Session observation harness — the primary tool for unblocking the co-op core on the rig.
//!
//! It reads `CSSessionManager` each frame and logs, on every change: the lobby/protocol state
//! machine, the connected-player roster, the session player limit, and the per-player scaling
//! multipliers our (host-tested) [`unseamless_core::scaling`] math would produce for the current
//! party size. It writes nothing — it's pure observation, safe to run anywhere.
//!
//! Why this first: the co-op core (relaxing player limits, persistent sessions, sync) hinges on
//! understanding this state machine and which count is the true "players in my world". That
//! can only be learned by watching it live in a real session, so this is what we hand to the
//! rig. The log it produces is the spec for the next phase.

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex};
use fromsoftware_shared::FromStatic;
use unseamless_core::config::Config;

use crate::feature::Feature;

/// Throttle for the "still alive, no session yet" heartbeat (~30s at 60fps).
const HEARTBEAT_FRAMES: u64 = 1800;

pub struct SessionObserver {
    config: Config,
    last: Option<Snapshot>,
    frame: u64,
}

/// The subset of session state we diff on, so we log only on change.
#[derive(PartialEq, Eq)]
struct Snapshot {
    lobby: u32,
    protocol: u32,
    players: usize,
    limit: u32,
}

impl SessionObserver {
    pub fn new(config: Config) -> Self {
        Self { config, last: None, frame: 0 }
    }
}

impl Feature for SessionObserver {
    fn name(&self) -> &'static str {
        "session-observer"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self) {
        self.frame += 1;

        let session = match unsafe { CSSessionManager::instance() } {
            Ok(s) => s,
            Err(_) => {
                if self.frame == 1 || self.frame.is_multiple_of(HEARTBEAT_FRAMES) {
                    log::info!("observer live; no CSSessionManager yet (frame {})", self.frame);
                }
                return;
            }
        };

        let players = session.players.len();
        let snapshot = Snapshot {
            lobby: session.lobby_state as u32,
            protocol: session.protocol_state as u32,
            players,
            limit: session.session_player_limit,
        };

        if self.last.as_ref() == Some(&snapshot) {
            return;
        }

        log::info!(
            "session change: lobby={:?} protocol={:?} players={} limit={}",
            session.lobby_state,
            session.protocol_state,
            players,
            session.session_player_limit,
        );
        for (i, p) in session.players.iter().enumerate() {
            log::info!(
                "  player[{i}] steam_id={:#018x} host={} local={} cid={}",
                p.base.steam_id,
                p.is_host,
                p.is_local_player,
                p.character_event_id,
            );
        }

        // Compute what scaling WOULD be for this party size, via the host-tested core. The exact
        // "player count" source and the application mechanism (MultiPlayCorrectionParam vs raw
        // NpcParam) are RE-gated — this logs the candidate so we can confirm it on the rig.
        let count = (players as u32).max(1);
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

        self.last = Some(snapshot);
    }
}
