//! On-demand diagnostics: a comprehensive runtime snapshot logged at key moments, plus requestable
//! probes. The **log is the shareable artifact** (the [`RunInfo`](unseamless_core::diagnostics::RunInfo)
//! boot header + lines), so everything here writes into the log — a tester just sends the file. The
//! pure model + renderer live in host-tested [`unseamless_core::diagnostics`]; this fills it from live
//! game state and wires the probes.
//!
//! ## Surfaces, sized for remote testers (who can't be driven interactively)
//! - **Always-on snapshots** ([`dump`]): a full report at boot and on a feature panic — unconditional
//!   `dump` calls. With logging on (friend/diag builds), these land in the shared log automatically —
//!   no action needed — so "what was the live state when it broke" is already captured.
//! - **Requestable probes** ([`probe_features`], gated by `[debug.probes]`): the levers we ask a
//!   specific tester to flip ("set this, reproduce, send the log"). Today: a **periodic snapshot** and
//!   an **event-flag scanner** — the reusable "which flag flips when I do X in-game" finder (e.g. the
//!   death-debuff cure flag, by resting at a grace).
//! - **Live panel feed** ([`debug_panel_feature`]): the same [`build_report`] published (not logged) to
//!   the overlay's bottom-left debug panel each ~10 Hz while it's shown, via [`crate::debug_panel`]. So
//!   the report model drives the log *and* a live HUD; the only thing that differs is the sink.

use eldenring::cs::{CSEventFlagMan, CSSessionManager};
use unseamless_core::config::Config;
use unseamless_core::diagnostics::{DiagnosticReport, FlagScanner};
use unseamless_core::util::{FrameThrottle, Timer};

use crate::feature::{Feature, Tick};

/// Build a live-state [`DiagnosticReport`]. Reads are gathered first, then the borrow-simple report
/// is assembled — so a missing singleton (pre-init / title screen) degrades to a "not live" note
/// rather than an empty report.
pub fn build_report(title: &str) -> DiagnosticReport {
    // Gather live state up front (keeps report assembly free of interleaved singleton borrows). The
    // session read (incl. the null-guarded warp-data deref this report needs on the feature-panic
    // dump) is shared with the observer via `crate::session`.
    let session = crate::sdk::with_instance::<CSSessionManager, _>(crate::session::read);
    let in_gameplay = crate::playstate::in_gameplay();
    let features = crate::app::feature_status();
    // Borrow the roster size for the scaling section below before `session` is moved into the match.
    let party = session.as_ref().map_or(0, |v| v.players);

    let mut r = DiagnosticReport::new(title);
    r.section("build")
        .field("version", env!("CARGO_PKG_VERSION"))
        .field("build_id", env!("UNSEAMLESS_BUILD_ID"));

    // Our own Steam identity (the connection plan's rung 1) — live in the panel, and captured in every
    // boot/panic/periodic log dump, so a friend's report shows whose machine it is and whether the ID
    // resolved (a blank here is the first thing to check if a future rung-2 connect fails). Non-blocking
    // atomic read, safe from this game-thread caller; "(resolving)" until the off-thread query lands.
    r.section("steam")
        .field(
            "own_id",
            crate::steam::self_steam_id().map_or_else(|| "(resolving)".to_string(), |id| id.to_string()),
        )
        // Rung-2 side-channel link status, so a friend's shared log / the live panel shows whether the
        // private Steam P2P channel actually came up (the first thing to check when a connect fails).
        .field("coop", crate::coop::status_line());

    let sec = r.section("session");
    match session {
        Some(v) => {
            sec.field("lobby_state", format!("{:?}", v.lobby_state))
                .field("protocol_state", format!("{:?}", v.protocol_state))
                .field("players", v.players)
                .field("player_limit", v.player_limit)
                .field("limit_override", v.limit_override)
                .field(
                    "roam_unrestricted",
                    v.tether.as_ref().map_or_else(
                        || "(warp data not initialized)".to_string(),
                        |t| t.restriction_disabled.to_string(),
                    ),
                );
        }
        None => {
            sec.field("status", "no CSSessionManager yet (pre-init / title screen)");
        }
    }

    let feat = r.section("features");
    feat.field("in_gameplay", in_gameplay);
    match features {
        // Before registration completes (earliest boot) the lock-free registry isn't set yet.
        None => {
            feat.field("status", "not registered yet (pre-init)");
        }
        Some(list) if list.is_empty() => {
            feat.field("status", "not registered yet");
        }
        Some(list) => {
            for (name, disabled) in list {
                feat.field(name, if disabled { "DISABLED (panicked)" } else { "ok" });
            }
        }
    }

    // Per-player scaling the (host-tested) core would produce for the current party size, via the
    // shared `Scaling::party_multipliers` (the same derivation the observer logs), surfaced here so the
    // boot/panic/periodic dumps and the live debug panel show them too. ASCII `x` (not the observer's
    // `×`) so the panel's printable-ASCII menu font renders it.
    let (count, enemy, boss) = crate::state::with(|c| c.scaling).party_multipliers(party);
    r.section("scaling")
        .field("party_size", count)
        .field("enemy", format!("hp x{:.2} dmg x{:.2} pos x{:.2}", enemy.health, enemy.damage, enemy.posture))
        .field("boss", format!("hp x{:.2} dmg x{:.2} pos x{:.2}", boss.health, boss.damage, boss.posture));
    r
}

/// Build and log a snapshot at `info` (so it lands in a shared log). Called at boot, on a feature
/// panic, and from the periodic probe.
pub fn dump(title: &str) {
    log::info!("\n{}", build_report(title).render());
}

/// The requestable probe features enabled by `[debug.probes]`, to append to the feature set. Empty
/// when no probe is configured (the common case). Boot/panic snapshots are not features — they're
/// direct [`dump`] calls — so this is just the *recurring* probes.
pub fn probe_features(config: &Config) -> Vec<Box<dyn Feature>> {
    let p = &config.debug.probes;
    let mut features: Vec<Box<dyn Feature>> = Vec::new();
    if p.snapshot_secs > 0 {
        features.push(Box::new(SnapshotProbe::new(p.snapshot_secs)));
    }
    if p.event_flag_scan_count > 0 {
        features.push(Box::new(FlagScanProbe::new(p.event_flag_scan_start, p.event_flag_scan_count)));
    }
    features
}

/// The game-thread feature that feeds the overlay's live debug panel. Always registered (it's not a
/// `[debug.probes]` lever — the panel is a built-in surface), but inert unless the overlay says the
/// panel is shown, so when off it costs a single atomic load per frame.
pub fn debug_panel_feature() -> Box<dyn Feature> {
    Box::new(DebugPanelProbe::new())
}

/// Publishes a live [`DiagnosticReport`] snapshot for the overlay's debug panel — but only while the
/// panel is shown ([`crate::debug_panel::visible`]). Throttled to ~10 Hz: the panel is for reading,
/// and 60 Hz churn would flicker the fast fields and waste allocations. Reuses [`build_report`] so the
/// panel is a live view of the same diagnostic block the log dumps produce; a `runtime` (frame/fps)
/// section is appended here since the per-tick frame/delta aren't available to `build_report` itself.
/// Every section is live, including per-feature health — `feature_status` reads the lock-free
/// `FEATURES` registry, not the (tick-held) `APP` mutex, so it stays readable from inside this tick.
struct DebugPanelProbe {
    throttle: FrameThrottle,
}

impl DebugPanelProbe {
    /// Publish every ~6 frames (~10 Hz at 60 fps).
    const PUBLISH_PERIOD: u64 = 6;

    fn new() -> Self {
        Self { throttle: FrameThrottle::every(Self::PUBLISH_PERIOD) }
    }
}

impl Feature for DebugPanelProbe {
    fn name(&self) -> &'static str {
        "debug-panel"
    }

    fn on_frame(&mut self, tick: Tick) {
        // Off costs one atomic load: do nothing unless the overlay says the panel is shown. The
        // throttle only advances on visible frames, so its period is in shown-frames (which is fine).
        if !crate::debug_panel::visible() || !self.throttle.tick() {
            return;
        }
        let mut report = build_report("debug");
        // fps from this tick's delta; guard the divide (delta is 0 on the very first frame).
        let fps = if tick.delta > 0.0 { 1.0 / tick.delta } else { 0.0 };
        report.section("runtime").field("frame", tick.frame).field("fps", format!("{fps:.0}"));
        crate::debug_panel::publish(report);
    }
}

/// Periodically log a full diagnostic snapshot, for watching state evolve over a session on request.
struct SnapshotProbe {
    timer: Timer,
}

impl SnapshotProbe {
    fn new(secs: u32) -> Self {
        Self { timer: Timer::every_secs(secs as f32) }
    }
}

impl Feature for SnapshotProbe {
    fn name(&self) -> &'static str {
        "diag-snapshot"
    }

    fn on_frame(&mut self, tick: Tick) {
        if self.timer.tick(tick.delta) {
            dump("periodic");
        }
    }
}

/// Scan a window of event flags and log each that flips — the "which flag is behind this action"
/// finder. Throttled (flags don't change faster than gameplay events), and the edge diff lives in the
/// host-tested [`FlagScanner`]; this just reads the window from `CSEventFlagMan` each scan.
struct FlagScanProbe {
    start: u32,
    scanner: FlagScanner,
    throttle: FrameThrottle,
    /// `false` until the first successful read, so the baseline count is logged once.
    announced: bool,
}

impl FlagScanProbe {
    /// Scan every ~12 frames (~5 Hz at 60fps) — fast enough to catch a rest-at-grace edge, cheap
    /// enough for a wide window.
    const SCAN_PERIOD: u64 = 12;

    fn new(start: u32, count: u32) -> Self {
        Self {
            start,
            scanner: FlagScanner::new(start, count as usize),
            throttle: FrameThrottle::every(Self::SCAN_PERIOD),
            announced: false,
        }
    }
}

impl Feature for FlagScanProbe {
    fn name(&self) -> &'static str {
        "diag-flag-scan"
    }

    fn on_frame(&mut self, _tick: Tick) {
        if !self.throttle.tick() {
            return;
        }
        let (start, count) = (self.start, self.scanner.len());
        let flags = crate::sdk::with_instance::<CSEventFlagMan, _>(|m| {
            // saturating: `event_flag_scan_start` is unbounded config, so a near-u32::MAX start must
            // not overflow (wraps to a low id in release, panics under diag).
            (0..count).map(|i| m.virtual_memory_flag.get_flag(start.saturating_add(i as u32))).collect::<Vec<bool>>()
        });
        let Some(flags) = flags else { return }; // event-flag manager not live yet

        // First live scan establishes the baseline: record it (so later diffs are relative to it) but
        // don't report every already-on flag as a spurious "edge" — only post-baseline flips matter.
        let changes = self.scanner.changes(&flags);
        if !self.announced {
            let on = flags.iter().filter(|&&b| b).count();
            log::info!("diag-flag-scan: watching {count} flags from {start} ({on} on at baseline)");
            self.announced = true;
            return;
        }
        for (id, now) in changes {
            log::info!("diag-flag-scan: flag {id} -> {}", if now { "ON" } else { "off" });
        }
    }
}
