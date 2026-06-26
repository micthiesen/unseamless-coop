//! Small, dependency-free building blocks used across features. Pure and host-tested — the
//! cdylib feeds them frame data and reads their decisions.

/// Fire on a fixed **frame** cadence (e.g. a heartbeat every 600 frames). Fires on the very
/// first tick, then every `period` ticks thereafter.
#[derive(Debug, Clone)]
pub struct FrameThrottle {
    period: u64,
    count: u64,
}

impl FrameThrottle {
    /// `period` is clamped to at least 1 (a period of 0 would fire every frame).
    pub fn every(period: u64) -> Self {
        Self { period: period.max(1), count: 0 }
    }

    /// Advance one frame; returns `true` on the frames it fires.
    pub fn tick(&mut self) -> bool {
        let fire = self.count.is_multiple_of(self.period);
        self.count = self.count.wrapping_add(1);
        fire
    }
}

/// Fire on a fixed **time** cadence, framerate-independent, by accumulating `delta` seconds.
/// Use this (not [`FrameThrottle`]) when the interval should be wall-clock, e.g. "every 10s".
#[derive(Debug, Clone)]
pub struct Timer {
    interval: f32,
    accumulated: f32,
}

impl Timer {
    pub fn every_secs(interval: f32) -> Self {
        Self { interval: interval.max(f32::EPSILON), accumulated: 0.0 }
    }

    /// Add `delta` seconds; returns `true` once per elapsed interval. Catches up at most one
    /// interval per call (a long stall fires once, not a burst).
    pub fn tick(&mut self, delta: f32) -> bool {
        self.accumulated += delta.max(0.0);
        if self.accumulated >= self.interval {
            // Drop any backlog (a long stall fires once, not a burst) while keeping the
            // sub-interval remainder so steady small steps stay accurate.
            self.accumulated %= self.interval;
            true
        } else {
            false
        }
    }
}

/// How a value compares to the last one a [`Latch`] saw — the three-way classification behind both
/// "log only on change" diffs ([`Latch::changed`]) and the "hold a config value into a game field,
/// then announce it" features ([`Latch::classify`], used by `session_limit` / `seamless`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// Same value as last time (steady state / self-heal). Features log at `debug`, don't toast.
    Reasserted,
    /// The first value ever seen (startup baseline). Features log at `info`, don't toast (every
    /// launch would otherwise toast).
    First,
    /// A genuinely *new* value replaced a previous one (e.g. a host `ConfigSync`). Features log at
    /// `info` **and** toast. (The debug/info/toast *mapping* is the feature's job; this only classifies.)
    Changed,
}

/// Tracks a value and classifies each new one against the last — replacing hand-rolled "log only on
/// change" diffs. [`changed`](Latch::changed) gives the boolean answer; [`classify`](Latch::classify)
/// gives the three-way [`Applied`] (first / reasserted / changed) the hold-a-config-value features need.
#[derive(Debug, Clone, Default)]
pub struct Latch<T> {
    last: Option<T>,
}

impl<T: Clone + PartialEq> Latch<T> {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Classify `value` against the last seen one and record it (the store advances on `First`/
    /// `Changed`; a `Reasserted` leaves it untouched, since it's already equal). This is the
    /// host-tested *classification* — the debug/info/toast policy keyed off it lives at the call site.
    pub fn classify(&mut self, value: &T) -> Applied {
        let applied = match self.last {
            Some(ref last) if last == value => Applied::Reasserted,
            Some(_) => Applied::Changed,
            None => Applied::First,
        };
        if applied != Applied::Reasserted {
            self.last = Some(value.clone());
        }
        applied
    }

    /// Returns `true` if `value` differs from the last seen value (or this is the first call), and
    /// records it. Thin wrapper over [`classify`](Latch::classify) — same store behavior.
    pub fn changed(&mut self, value: &T) -> bool {
        self.classify(value) != Applied::Reasserted
    }

    pub fn last(&self) -> Option<&T> {
        self.last.as_ref()
    }
}

/// A 0→1 / 1→0 transition of a boolean signal (e.g. a polled key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Rising,
    Falling,
    None,
}

/// Edge detector for a boolean signal. Feed the current level each frame; get the transition.
#[derive(Debug, Clone, Default)]
pub struct Edge {
    prev: bool,
}

impl Edge {
    pub fn new() -> Self {
        Self { prev: false }
    }

    pub fn update(&mut self, now: bool) -> Transition {
        let t = match (self.prev, now) {
            (false, true) => Transition::Rising,
            (true, false) => Transition::Falling,
            _ => Transition::None,
        };
        self.prev = now;
        t
    }

    /// Convenience: just-pressed (rising edge).
    pub fn just_pressed(&mut self, now: bool) -> bool {
        self.update(now) == Transition::Rising
    }
}

/// A token-bucket rate limiter: allow a burst of up to `capacity`, then one event per refilled
/// token. Time-agnostic — the caller [`refill`](RateLimiter::refill)s on whatever cadence it has
/// (frames, seconds, maintenance ticks), so this stays pure and host-testable. Used to cap
/// client→host log forwarding so a misbehaving client can't flood the side-channel.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    tokens: f64,
    capacity: f64,
}

impl RateLimiter {
    /// A bucket that starts **full** (one immediate burst of `capacity` is allowed).
    pub fn new(capacity: u32) -> Self {
        let capacity = capacity as f64;
        Self { tokens: capacity, capacity }
    }

    /// Add `tokens` (clamped at the capacity; negative/non-finite amounts add nothing).
    pub fn refill(&mut self, tokens: f64) {
        if tokens.is_finite() && tokens > 0.0 {
            self.tokens = (self.tokens + tokens).min(self.capacity);
        }
    }

    /// Consume one token; returns `true` if one was available (the event is allowed).
    pub fn try_take(&mut self) -> bool {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Whole tokens currently available.
    pub fn available(&self) -> u32 {
        self.tokens as u32
    }
}

/// A `major.minor.patch` version, for the side-channel handshake (warn on mismatch).
///
/// Field widths (`u8`/`u8`/`u16`) match the wire layout exactly, so [`to_u32`](Version::to_u32)
/// /[`from_u32`](Version::from_u32) is a **lossless** round-trip for every constructible value —
/// no silent truncation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u16,
}

impl Version {
    pub const fn new(major: u8, minor: u8, patch: u16) -> Self {
        Self { major, minor, patch }
    }

    /// Parse `"X.Y.Z"` (extra pre-release/build metadata after `-`/`+` is ignored). Returns
    /// `None` on anything malformed, including components that overflow their field width.
    pub fn parse(s: &str) -> Option<Self> {
        let core = s.split(['-', '+']).next().unwrap_or(s);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self { major, minor, patch })
    }

    /// Pack into a `u32` for the wire (`major<<24 | minor<<16 | patch`). Lossless.
    pub fn to_u32(self) -> u32 {
        ((self.major as u32) << 24) | ((self.minor as u32) << 16) | (self.patch as u32)
    }

    pub fn from_u32(v: u32) -> Self {
        Self {
            major: ((v >> 24) & 0xff) as u8,
            minor: ((v >> 16) & 0xff) as u8,
            patch: (v & 0xffff) as u16,
        }
    }

    /// Compatible if the major version matches (semver: same major = no breaking changes).
    pub fn compatible_with(self, other: Version) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for Version {
    /// Canonical `major.minor.patch` rendering, single-sourced so the handshake/version-mismatch
    /// surfaces (the `Peer`'s notifications and the cdylib's overlay) format a version identically.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Push `item` onto a bounded queue, evicting the **oldest** entries first until it fits within
/// `cap`. Returns how many were evicted (`0` in the common, not-full case). This is the shared
/// drop-oldest discipline the cdylib's cross-thread queues use — the forward-log backlog
/// (`forward.rs`) and the session-action queue (`actionq.rs`) — so a producer burst can't grow
/// either without bound: a stale oldest line/verb is the right thing to lose. Lives in core (not
/// those modules) so the eviction logic is host-tested here, where the cdylib can't run its tests.
///
/// `cap` is treated as **at least 1**: a `cap` of 0 still stores the one `item` (the empty-queue
/// `pop_front` breaks the loop), since "a queue that can hold nothing" is never what a caller wants.
pub fn push_capped<T>(queue: &mut std::collections::VecDeque<T>, item: T, cap: usize) -> usize {
    let mut evicted = 0;
    while queue.len() >= cap {
        // `pop_front` is `None` only when already empty (cap == 0) — stop rather than spin.
        if queue.pop_front().is_none() {
            break;
        }
        evicted += 1;
    }
    queue.push_back(item);
    evicted
}

/// Scoped, single-thread **reentrancy** flag. While a [`ReentryGuard`] from [`ReentryGuard::enter`]
/// is alive, [`Cell::get`](std::cell::Cell::get) on the same flag reads `true`, so code can detect
/// and skip recursive re-entry into a region. The guard clears the flag on drop — including on a
/// panic unwind — so a panic inside the region can't wedge it stuck-on.
///
/// The cdylib's forward-log tee (`forward.rs`) uses this against a `thread_local` flag: a forwarding
/// send may itself log, which would re-enter the logger; the flag set across the send loop makes
/// those records drop instead of re-enqueue. Extracted here so the RAII set/clear (and its
/// panic-safety) is host-tested in core, which the cdylib's own tests can't be.
pub struct ReentryGuard<'a>(&'a std::cell::Cell<bool>);

impl<'a> ReentryGuard<'a> {
    /// Mark `flag` active for the lifetime of the returned guard.
    pub fn enter(flag: &'a std::cell::Cell<bool>) -> Self {
        flag.set(true);
        ReentryGuard(flag)
    }
}

impl Drop for ReentryGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_throttle_fires_first_then_every_period() {
        let mut t = FrameThrottle::every(3);
        let fires: Vec<bool> = (0..7).map(|_| t.tick()).collect();
        assert_eq!(fires, [true, false, false, true, false, false, true]);
    }

    #[test]
    fn frame_throttle_period_zero_is_every_frame() {
        let mut t = FrameThrottle::every(0);
        assert!(t.tick() && t.tick() && t.tick());
    }

    #[test]
    fn timer_fires_per_interval_regardless_of_step_size() {
        let mut t = Timer::every_secs(1.0);
        assert!(!t.tick(0.4));
        assert!(!t.tick(0.4));
        assert!(t.tick(0.4)); // 1.2 total -> fires, keeps 0.2
        assert!(!t.tick(0.5)); // 0.7
        assert!(t.tick(0.5)); // 1.2 -> fires
    }

    #[test]
    fn timer_long_stall_fires_once() {
        let mut t = Timer::every_secs(1.0);
        assert!(t.tick(100.0)); // single fire, not a burst
        assert!(!t.tick(0.0));
    }

    #[test]
    fn latch_reports_first_and_changes_only() {
        let mut l = Latch::new();
        assert!(l.changed(&5)); // first
        assert!(!l.changed(&5)); // same
        assert!(l.changed(&6)); // changed
        assert!(!l.changed(&6));
        assert_eq!(l.last(), Some(&6));
    }

    #[test]
    fn classify_first_reassert_change_and_advances_the_store_each_change() {
        let mut l = Latch::new();
        assert_eq!(l.classify(&6), Applied::First, "first apply is the baseline");
        assert_eq!(l.classify(&6), Applied::Reasserted, "same value re-applied");
        // Back-to-back changes: the store must advance on EVERY Changed, not only after a steady state.
        assert_eq!(l.classify(&4), Applied::Changed, "new value");
        assert_eq!(l.classify(&7), Applied::Changed, "another new value, straight after a change");
        assert_eq!(l.classify(&7), Applied::Reasserted, "...and the store advanced to 7");
        assert_eq!(l.last(), Some(&7), "store reflects the last First/Changed value");
        // Returning to an old value is a change — the latch remembers only the immediately-prior one.
        assert_eq!(l.classify(&6), Applied::Changed, "back to an old value is still a change");
    }

    #[test]
    fn classify_does_not_advance_the_store_on_a_reassert() {
        let mut l = Latch::new();
        l.classify(&6); // First -> store = 6
        assert_eq!(l.last(), Some(&6));
        l.classify(&6); // Reasserted -> store untouched (observable via last())
        assert_eq!(l.last(), Some(&6));
    }

    #[test]
    fn classify_works_for_bools_including_the_toggle_back() {
        // The seamless-roam case: a bool held into a game field. For a 2-state type every post-first
        // change is a return to a previously-seen value, so the toggle-back IS the characteristic case.
        let mut l = Latch::new();
        assert_eq!(l.classify(&true), Applied::First);
        assert_eq!(l.classify(&true), Applied::Reasserted);
        assert_eq!(l.classify(&false), Applied::Changed);
        assert_eq!(l.classify(&true), Applied::Changed, "toggled back -> change, not reassert");
    }

    #[test]
    fn classify_via_default_construction_starts_fresh() {
        // The features build their Latch via #[derive(Default)], not new() — pin that path.
        assert_eq!(Latch::<u32>::default().classify(&1), Applied::First);
    }

    #[test]
    fn changed_is_classify_minus_the_reassert() {
        // `changed` must agree with `classify`: true on First/Changed, false on Reasserted.
        let mut l = Latch::new();
        assert!(l.changed(&1), "First -> changed");
        assert!(!l.changed(&1), "Reasserted -> not changed");
        assert!(l.changed(&2), "Changed -> changed");
    }

    #[test]
    fn edge_detects_transitions() {
        let mut e = Edge::new();
        assert_eq!(e.update(false), Transition::None);
        assert_eq!(e.update(true), Transition::Rising);
        assert_eq!(e.update(true), Transition::None);
        assert_eq!(e.update(false), Transition::Falling);
    }

    #[test]
    fn edge_just_pressed_only_on_rising() {
        let mut e = Edge::new();
        assert!(e.just_pressed(true));
        assert!(!e.just_pressed(true)); // held, not a new press
        assert!(!e.just_pressed(false));
        assert!(e.just_pressed(true)); // pressed again
    }

    #[test]
    fn rate_limiter_allows_a_burst_then_throttles_until_refilled() {
        let mut rl = RateLimiter::new(3);
        assert!(rl.try_take() && rl.try_take() && rl.try_take(), "full burst of capacity");
        assert!(!rl.try_take(), "bucket empty");
        rl.refill(2.0);
        assert_eq!(rl.available(), 2);
        assert!(rl.try_take() && rl.try_take());
        assert!(!rl.try_take(), "drained again");
    }

    #[test]
    fn rate_limiter_refill_grants_exactly_that_many_takes() {
        let mut rl = RateLimiter::new(100);
        while rl.try_take() {} // drain to empty
        rl.refill(8.0);
        let mut granted = 0;
        while rl.try_take() {
            granted += 1;
        }
        assert_eq!(granted, 8, "a refill of N grants exactly N takes");
    }

    #[test]
    fn rate_limiter_caps_at_capacity_and_ignores_bad_refills() {
        let mut rl = RateLimiter::new(2);
        rl.refill(100.0); // can't exceed capacity
        assert_eq!(rl.available(), 2);
        rl.try_take();
        rl.refill(-5.0); // negative is a no-op
        rl.refill(f64::NAN); // non-finite is a no-op
        assert_eq!(rl.available(), 1);
    }

    #[test]
    fn version_parses_and_compares() {
        assert_eq!(Version::parse("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(Version::parse("0.1.0-dev+meta"), Some(Version::new(0, 1, 0)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.x"), None);
        assert!(Version::new(1, 4, 0) > Version::new(1, 2, 9));
        assert!(Version::new(1, 0, 0).compatible_with(Version::new(1, 9, 9)));
        assert!(!Version::new(1, 0, 0).compatible_with(Version::new(2, 0, 0)));
    }

    #[test]
    fn version_round_trips_through_u32() {
        // Lossless for every constructible value, including the field-width extremes.
        for v in [Version::new(1, 7, 300), Version::new(0, 0, 0), Version::new(255, 255, 65535)] {
            assert_eq!(Version::from_u32(v.to_u32()), v);
        }
    }

    #[test]
    fn version_parse_rejects_out_of_range_components() {
        assert_eq!(Version::parse("256.0.0"), None); // major > u8
        assert_eq!(Version::parse("1.999.0"), None); // minor > u8
        assert_eq!(Version::parse("1.0.70000"), None); // patch > u16
        assert_eq!(Version::parse("255.255.65535"), Some(Version::new(255, 255, 65535)));
    }

    #[test]
    fn push_capped_below_cap_keeps_everything_in_order() {
        let mut q = std::collections::VecDeque::new();
        for n in 0..3 {
            assert_eq!(push_capped(&mut q, n, 4), 0, "no eviction below cap");
        }
        assert_eq!(q.into_iter().collect::<Vec<_>>(), [0, 1, 2]);
    }

    #[test]
    fn push_capped_at_cap_drops_oldest_and_keeps_newest() {
        let mut q = std::collections::VecDeque::new();
        // Fill to cap (no eviction), then one more push evicts exactly the oldest.
        for n in 0..3 {
            push_capped(&mut q, n, 3);
        }
        assert_eq!(push_capped(&mut q, 3, 3), 1, "one eviction once at cap");
        assert_eq!(q.iter().copied().collect::<Vec<_>>(), [1, 2, 3], "oldest (0) dropped, newest kept");
        // Steady state at cap: each further push evicts one and the window slides.
        assert_eq!(push_capped(&mut q, 4, 3), 1);
        assert_eq!(q.into_iter().collect::<Vec<_>>(), [2, 3, 4]);
    }

    #[test]
    fn push_capped_evicts_a_preexisting_overflow_down_to_cap() {
        // A queue already over cap (cap lowered, or seeded large) is brought back within cap in one
        // push — the while-loop evicts until it fits, not just a single pop.
        let mut q: std::collections::VecDeque<i32> = (0..5).collect();
        assert_eq!(push_capped(&mut q, 99, 2), 4, "evict 4 to land at cap (1 survivor + new = 2)");
        assert_eq!(q.into_iter().collect::<Vec<_>>(), [4, 99]);
    }

    #[test]
    fn push_capped_cap_zero_stores_the_one_item_without_spinning() {
        // Degenerate cap: the empty-queue pop_front breaks the loop, so we store the item rather
        // than loop forever or panic.
        let mut q = std::collections::VecDeque::new();
        assert_eq!(push_capped(&mut q, 7, 0), 0);
        assert_eq!(q.into_iter().collect::<Vec<_>>(), [7]);
    }

    #[test]
    fn reentry_guard_sets_while_alive_and_clears_on_drop() {
        let flag = std::cell::Cell::new(false);
        assert!(!flag.get(), "starts inactive");
        {
            let _g = ReentryGuard::enter(&flag);
            assert!(flag.get(), "active while the guard is alive");
        }
        assert!(!flag.get(), "cleared once the guard drops");
    }

    #[test]
    fn reentry_guard_clears_even_when_the_scope_panics() {
        let flag = std::cell::Cell::new(false);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = ReentryGuard::enter(&flag);
            assert!(flag.get());
            panic!("boom inside the guarded region");
        }));
        assert!(caught.is_err(), "the panic propagated");
        assert!(!flag.get(), "Drop ran on unwind, so the flag isn't wedged on");
    }
}
