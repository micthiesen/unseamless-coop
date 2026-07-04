//! Decision logic for the **stay-connected** feature (suppress game-driven co-op disconnects).
//!
//! The cdylib side (`coop/stay_connected`) gates the game's whole-session leave primitive behind an
//! armed flag; its hooks only bump an atomic counter per suppressed leave. This module owns the
//! host-tested policy for *announcing* those suppressions to the player: the counter can rise once
//! per rare event (a boss defeat) or every frame (the update task re-deciding a polled leave while
//! armed), so announcements must aggregate — first suppression announces immediately, then further
//! ones batch behind a cooldown instead of toasting/logging per frame.

/// Seconds between suppression announcements (after the immediate first one). Long enough that a
/// polled per-frame leave source reads as one event, short enough that a genuinely new event (a
/// second boss, another death) isn't silently swallowed for long.
pub const ANNOUNCE_COOLDOWN_SECS: f32 = 8.0;

/// Aggregating rate-limiter for "N disconnects suppressed" announcements.
///
/// Feed it the hook counter's running total plus the frame delta each tick; it returns
/// `Some(newly_suppressed)` when an announcement is due — immediately for the first suppression
/// after a quiet period, then at most once per [`ANNOUNCE_COOLDOWN_SECS`], with everything since
/// the last announcement folded into one count. Pure and host-tested; the cdylib maps the returned
/// count onto the toast + milestone log line.
#[derive(Debug, Default)]
pub struct SuppressAnnouncer {
    /// Total already announced (the counter value as of the last announcement).
    announced: u64,
    /// Seconds of cooldown remaining before the next announcement may fire.
    cooldown: f32,
}

impl SuppressAnnouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance time by `delta` seconds and reconcile against the hooks' running `total`. Returns
    /// `Some(n)` — the number of suppressions not yet announced — when an announcement should fire
    /// now, consuming them and starting a new cooldown window.
    pub fn tick(&mut self, total: u64, delta: f32) -> Option<u64> {
        self.cooldown = (self.cooldown - delta).max(0.0);
        // A counter that moved backwards means the source was reset (shouldn't happen — the hook
        // counter only increments); resync silently rather than underflow.
        if total < self.announced {
            self.announced = total;
            return None;
        }
        let pending = total - self.announced;
        if pending == 0 || self.cooldown > 0.0 {
            return None;
        }
        self.announced = total;
        self.cooldown = ANNOUNCE_COOLDOWN_SECS;
        Some(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f32 = 1.0 / 60.0;

    #[test]
    fn quiet_ticks_announce_nothing() {
        let mut a = SuppressAnnouncer::new();
        for _ in 0..1000 {
            assert_eq!(a.tick(0, FRAME), None);
        }
    }

    #[test]
    fn first_suppression_announces_immediately() {
        let mut a = SuppressAnnouncer::new();
        assert_eq!(a.tick(0, FRAME), None);
        assert_eq!(a.tick(1, FRAME), Some(1));
    }

    #[test]
    fn burst_within_cooldown_batches_into_one_followup() {
        let mut a = SuppressAnnouncer::new();
        assert_eq!(a.tick(1, FRAME), Some(1));
        // A polled leave source re-fires every frame while armed: no per-frame announcements, and
        // — the point of this test — the cooldown genuinely *suppresses* a rising total. Stop a
        // frame short of the cooldown so we're unambiguously still inside it (no reliance on float
        // accumulation landing exactly on the clamp boundary).
        let mut elapsed = 0.0;
        let mut total = 1;
        while elapsed + 2.0 * FRAME < ANNOUNCE_COOLDOWN_SECS {
            total += 1;
            assert_eq!(a.tick(total, FRAME), None, "must stay quiet inside the cooldown");
            elapsed += FRAME;
        }
        // Expire the cooldown with an explicit over-shoot delta (not an accumulated near-miss), so
        // the one aggregated announcement is asserted with obvious slack.
        total += 1;
        assert_eq!(a.tick(total, ANNOUNCE_COOLDOWN_SECS), Some(total - 1));
    }

    #[test]
    fn next_event_after_a_quiet_cooldown_announces_immediately() {
        let mut a = SuppressAnnouncer::new();
        assert_eq!(a.tick(1, FRAME), Some(1));
        // A fresh event *arriving during* the cooldown is actively suppressed by the cooldown guard
        // (total rises to 2, but pending is withheld) — this is what exercises the guard, unlike a
        // flat total which short-circuits on pending==0 before the guard is consulted.
        assert_eq!(a.tick(2, FRAME), None, "cooldown must withhold a new event mid-window");
        // Once the cooldown expires, the withheld event is announced (batched).
        assert_eq!(a.tick(2, ANNOUNCE_COOLDOWN_SECS), Some(1));
        // And a further fresh event after a fully quiet window announces immediately again.
        assert_eq!(a.tick(2, ANNOUNCE_COOLDOWN_SECS), None); // quiet: nothing new
        assert_eq!(a.tick(3, FRAME), Some(1));
    }

    #[test]
    fn backwards_counter_resyncs_without_announcing() {
        let mut a = SuppressAnnouncer::new();
        assert_eq!(a.tick(5, FRAME), Some(5));
        assert_eq!(a.tick(0, ANNOUNCE_COOLDOWN_SECS), None); // reset source: resync, stay quiet
        assert_eq!(a.tick(1, ANNOUNCE_COOLDOWN_SECS), Some(1)); // and keep working after
    }
}
