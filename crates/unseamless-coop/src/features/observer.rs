//! Session observation harness — the primary tool for unblocking the co-op core on the rig.
//!
//! It reads `CSSessionManager` each frame and logs, on every change: the lobby/protocol state
//! machine, the connected-player roster, the session player limit, the "stay in multiplay area"
//! tether governance, and the per-player scaling multipliers our (host-tested)
//! [`unseamless_core::scaling`] math would produce for the current party size. It writes nothing —
//! pure observation, safe to run anywhere.
//!
//! Why this first: the co-op core (relaxing player limits, persistent sessions, sync) hinges on
//! understanding this state machine and which count is the true "players in my world". That can
//! only be learned by watching it live, so this is what we hand to the rig; the log it produces
//! is the spec for the next phase.
//!
//! ## Teardown probe (which lever does each disconnect need — see the co-op-teardown design)
//! Our plan is to keep co-op seamless by *holding state invariants*, not byte-patching ERSC's many
//! disconnect call sites (held state is Arxan-immune; a code patch is not). Which lever each event
//! needs — hold a field vs. hook a chokepoint — is decided empirically here: in a 2-player session,
//! trigger each event (kill a boss, cross a fog gate, host dies, walk out of the area) and read the
//! log.
//! - The **roster shrinking 2->1** is the ground-truth "a phantom left" marker; we call it out loudly
//!   with the surrounding tether state so it's easy to find in the log.
//! - Every change line carries its **frame number**, so the order of flips leading up to a teardown
//!   is recoverable. If a field flips a frame or two *before* the roster drop, that field is a
//!   candidate state lever (hold it). If *nothing* observable changes before the drop, the teardown
//!   is atomic within a frame — that one needs a hook, not a held field.
//! - `CSStayInMultiplayAreaWarpData` is already SDK-charted: `disable_multiplay_restriction`
//!   (documented "set true to let players go anywhere on the map") and `multiplay_start_area_id`
//!   ("set 0 to disable" the boss-area-mismatch warp) are the prime candidate levers for the
//!   roam/area-tether half. The probe logs their baseline + transitions so we can confirm before a
//!   feature writes them.

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex};
use unseamless_core::util::{FrameThrottle, Latch};

use crate::feature::{Feature, Tick};
use crate::session::SessionView;

pub struct SessionObserver {
    /// Fires only when the watched session state changes, so we log transitions not every frame.
    state: Latch<Snapshot>,
    /// "Still alive, no session yet" heartbeat (~30s at 60fps) while idle at the title screen.
    heartbeat: FrameThrottle,
}

impl Default for SessionObserver {
    fn default() -> Self {
        Self { state: Latch::new(), heartbeat: FrameThrottle::every(1800) }
    }
}

/// The subset of session state we diff on. Deliberately all **discrete** signals (enums, ints,
/// bools, lengths) so change-detection is clean — a continuously-varying `f32` (e.g. the warp
/// countdown) would fire the latch every frame, so we fold those into bools (`warp_pending`) and
/// log the raw value in the detail line instead.
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    lobby: u32,
    protocol: u32,
    players: usize,
    limit: u32,
    /// `session_player_limit_override` — `1` is vanilla ("use the per-context default"); our
    /// session-limit feature writes our cap here, so diffing on it makes that write visible.
    limit_override: u32,
    /// `disable_multiplay_restriction` — the documented "go anywhere on the map" lever. Baseline
    /// `false`; a feature would hold it `true`. Diffing it shows the game (or us) toggling it.
    restriction_disabled: bool,
    /// `multiplay_start_area_id` — host's play-area id at connect; `0` disables the boss-area
    /// mismatch warp. Watch what it is mid-session vs. when a phantom gets warped home.
    start_area_id: u32,
    /// `warp_request_delay > 0` — a tether warp is pending (player is being pulled back into the
    /// area). The raw countdown is logged, not diffed (it's task-driven and would churn the latch).
    warp_pending: bool,
    /// `is_warp_possible` — game's gate on whether the tether warp may fire this frame.
    warp_possible: bool,
    /// `player_fade_tracker.len()` — remote players currently faded out mid-warp; a teardown-ish
    /// signal distinct from the roster count.
    fading_players: usize,
}

impl Snapshot {
    /// Fold a [`SessionView`] into the discrete diff snapshot. When the tether isn't wired yet
    /// (`tether == None`, pre-session) its fields read as neutral defaults — a well-defined snapshot
    /// that never dereferences a null pointer. (The latch then fires on the first real change, e.g.
    /// lobby/protocol advancing; the tether values folding in only fires it if they differ from the
    /// defaults.)
    fn from_view(v: &SessionView) -> Self {
        let t = v.tether.as_ref();
        Snapshot {
            lobby: v.lobby_state as u32,
            protocol: v.protocol_state as u32,
            players: v.players,
            limit: v.player_limit,
            limit_override: v.limit_override,
            restriction_disabled: t.is_some_and(|t| t.restriction_disabled),
            start_area_id: t.map_or(0, |t| t.start_area_id),
            warp_pending: t.is_some_and(|t| t.warp_pending()),
            warp_possible: t.is_some_and(|t| t.warp_possible),
            fading_players: t.map_or(0, |t| t.fading_players),
        }
    }
}

impl SessionObserver {
    pub fn new() -> Self {
        Self::default()
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
        let observed = crate::sdk::with_instance::<CSSessionManager, _>(|s| self.observe(s, tick.frame));
        if observed.is_none() && self.heartbeat.tick() {
            log::info!("observer live; no CSSessionManager yet (frame {})", tick.frame);
        }
    }
}

impl SessionObserver {
    /// Log the session state if it changed since last frame, tagging the frame so the order of
    /// transitions leading up to a teardown is recoverable from the log.
    fn observe(&mut self, session: &CSSessionManager, frame: u64) {
        // Read the session the shared way (the warp-data deref is null-guarded once, in `session`),
        // then fold it into the discrete diff snapshot.
        let view = crate::session::read(session);
        let players = view.players;
        let snapshot = Snapshot::from_view(&view);

        // Capture the prior roster size before the latch overwrites it, to detect a shrink below.
        let prev_players = self.state.last().map(|s| s.players);

        if !self.state.changed(&snapshot) {
            return;
        }

        // One tether rendering for both log lines — distinguishing "not wired yet" from a wired
        // baseline (the diag report makes the same distinction), so the rig log isn't misread.
        let tether = match view.tether.as_ref() {
            Some(t) => format!(
                "restriction_disabled={} start_area_id={} warp_pending={} warp_delay={:.2} warp_possible={} fading={}",
                t.restriction_disabled,
                t.start_area_id,
                t.warp_pending(),
                t.warp_request_delay,
                t.warp_possible,
                t.fading_players,
            ),
            None => "(not initialized)".to_string(),
        };

        // Loud, greppable teardown marker: the roster shrank, i.e. a phantom left. This is the
        // event the probe exists to catch, pinned to the tether state at that instant — if a tether
        // field flipped in the frames just before this (visible as its own earlier change line),
        // it's a candidate state lever; if nothing changed before this, the teardown is atomic and
        // needs a hook. See the module-level "Teardown probe" notes.
        if let Some(prev) = prev_players
            && players < prev
        {
            log::warn!("TEARDOWN @frame {frame}: roster {prev} -> {players} | {tether}");
        }

        log::info!(
            "session change @frame {frame}: lobby={:?} protocol={:?} players={players} limit={} override={} | tether: {tether}",
            view.lobby_state,
            view.protocol_state,
            view.player_limit,
            view.limit_override,
        );
        for (i, p) in session.players.iter().enumerate() {
            // Pseudonymous tag, not the raw 64-bit Steam ID: this log is shareable, and a raw
            // SteamID would leak other players' identities (see diagnostics::peer_tag). `cid` is a
            // game-internal character/event id (not resolvable to a person), kept for rig debugging.
            // `join_wait`/`rebreak_in` are per-player teardown-adjacent flags worth watching as a
            // phantom connects or leaves.
            log::info!(
                "  player[{i}] {} host={} local={} cid={} join_wait={} rebreak_in={}",
                unseamless_core::diagnostics::peer_tag(p.base.steam_id),
                p.is_host,
                p.is_local_player,
                p.character_event_id,
                p.join_wait,
                p.rebreak_in,
            );
        }

        // What scaling WOULD be for this party size, via the host-tested core. The exact
        // player-count source and application mechanism are RE-gated; this logs the candidate so
        // we can confirm it on the rig. The core's multiplier math saturates for any count, so we
        // only need to guard the usize->u32 narrowing.
        let count = u32::try_from(players).unwrap_or(u32::MAX).max(1);
        // Reads the live config, so these multipliers reflect a config the bridge may have synced
        // (still read-only here — the observer writes nothing).
        let scaling = crate::state::with(|c| c.scaling);
        let enemy = scaling.enemy_multipliers(count);
        let boss = scaling.boss_multipliers(count);
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
