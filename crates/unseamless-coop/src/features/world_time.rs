//! Lock the in-game time of day to a configured `hour:minute`.
//!
//! When [`world_time.lock`](unseamless_core::config::WorldTime::lock) is on, it re-asserts the game's
//! time *target* (`WorldAreaTime::request_time` — an SDK-charted **setter** for `target_hour` /
//! `target_minute` / `target_second`, not a game routine) **every frame**, so the clock is held at
//! the chosen time instead of progressing — a permanent day/night setting. Re-asserting each frame
//! (rather than once) pins it regardless of whether the engine treats the target as a one-shot jump
//! or a continuously-steered setpoint: it keeps getting pulled back. Lock off → we stop re-asserting,
//! normal progression resumes. A state write, like `session_limit` / `seamless`; off by default.
//!
//! **Rig-derivation note (the mechanism is unverified):** if the rig shows the clock *jittering* or
//! creeping forward, the robust freeze is the adjacent `WorldAreaTime::time_passage_multiplier = 0.0`
//! (halts advance with no oscillation) — that freezes at the *current* time, so locking to a specific
//! time would be "request_time(target) until reached, then set the multiplier to 0". And if the game's
//! time task runs *after* `FrameBegin`, our write is overwritten that frame; a later phase is the knob
//! to try. Confirm which behavior the engine has on the rig before settling the approach.
//!
//! Local config for now (each player sets their own time); host-enforced sync is a noted follow-up —
//! time-of-day desync between co-op players is a known annoyance worth syncing later.

use eldenring::cs::WorldAreaTime;
use unseamless_core::util::Latch;

use crate::feature::{Feature, Tick};

#[derive(Default)]
pub struct WorldTimeLock {
    /// Classifies the locked time so we announce a change once, not every frame. `(hour, minute)`.
    latch: Latch<(u32, u32)>,
}

impl WorldTimeLock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Feature for WorldTimeLock {
    fn name(&self) -> &'static str {
        "world-time-lock"
    }

    // Default phase (`FrameBegin`): plain time state, not frame-order-sensitive — but see the
    // rig-derivation note (a later phase is the fix if the engine's time task clobbers our write).

    fn on_frame(&mut self, _tick: Tick) {
        let (lock, hour, minute) =
            crate::state::with(|c| (c.world_time.lock, c.world_time.hour, c.world_time.minute));
        if !lock {
            return; // stop re-asserting → normal time progression resumes
        }
        // Defensive clamp before writing live game memory: config + the menu already bound these, but
        // this feature shouldn't trust every (future, e.g. host-synced) path did so — mirrors session_limit.
        let (hour, minute) = (hour.min(23), minute.min(59));

        // Re-assert the target every frame to pin the clock. `None` = no WorldAreaTime singleton yet
        // (menu / loading) — nothing to hold, retry next frame.
        let live = crate::sdk::with_instance_mut::<WorldAreaTime, _>(|t| t.request_time(hour, minute, 0))
            .is_some();
        if !live {
            return;
        }

        // Announce once on enable/change; silent on the per-frame re-assert (the shared helper logs
        // only First/Changed, and the lazy messages mean the steady-state path allocates nothing).
        crate::features::announce_held(
            &mut self.latch,
            (hour, minute),
            || format!("world time locked to {hour:02}:{minute:02}"),
            || format!("Time of day locked to {hour:02}:{minute:02}"),
        );
    }
}
