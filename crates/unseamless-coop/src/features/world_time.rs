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
//! **Mechanism confirmed on the rig:** locking to 22:00 at the default `FrameBegin` phase held the
//! clock steady for several minutes — no creep, no jitter — so the per-frame `request_time` re-assert
//! is sufficient on its own. (A later task phase, or zeroing `time_passage_multiplier`, were the
//! fallbacks if the game's own time task had clobbered our write; the rig showed neither is needed.)
//!
//! Host-enforced (synced across the party): the world-time lock is part of the shared subset
//! ([`SharedSettings`](unseamless_core::protocol::SharedSettings)), so a client adopts the host's
//! `lock`/`hour`/`minute` and the whole party shares time-of-day, rather than each player locking
//! their own.

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

    // Default phase (`FrameBegin`): plain time state, rig-confirmed not frame-order-sensitive — the
    // per-frame re-assert holds the clock here without needing a later phase (see the module docs).

    fn on_frame(&mut self, _tick: Tick) {
        let (lock, hour, minute) =
            crate::state::with(|c| (c.world_time.lock, c.world_time.hour, c.world_time.minute));
        if !lock {
            return; // stop re-asserting → normal time progression resumes
        }
        // Defensive clamp before writing live game memory: config + the menu already bound these, and
        // the host-synced path clamps on decode too, but this feature shouldn't trust every path did so
        // — mirrors session_limit.
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
