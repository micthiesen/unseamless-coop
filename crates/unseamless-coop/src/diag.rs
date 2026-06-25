//! On-demand diagnostics: a comprehensive runtime snapshot logged at key moments, plus requestable
//! probes. The **log is the shareable artifact** (the [`RunInfo`](unseamless_core::diagnostics::RunInfo)
//! boot header + lines), so everything here writes into the log — a tester just sends the file. The
//! pure model + renderer live in host-tested [`unseamless_core::diagnostics`]; this fills it from live
//! game state and wires the probes.
//!
//! ## Two surfaces, sized for remote testers (who can't be driven interactively)
//! - **Always-on snapshots** ([`dump`]): a full report at boot and on a feature panic — unconditional
//!   `dump` calls. With logging on (friend/diag builds), these land in the shared log automatically —
//!   no action needed — so "what was the live state when it broke" is already captured.
//! - **Requestable probes** ([`probe_features`], gated by `[debug.probes]`): the levers we ask a
//!   specific tester to flip ("set this, reproduce, send the log"). Today: a **periodic snapshot** and
//!   an **event-flag scanner** — the reusable "which flag flips when I do X in-game" finder (e.g. the
//!   death-debuff cure flag, by resting at a grace).

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

    let mut r = DiagnosticReport::new(title);
    r.section("build")
        .field("version", env!("CARGO_PKG_VERSION"))
        .field("build_id", env!("UNSEAMLESS_BUILD_ID"));

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
        // Re-entrant call (a dump from inside a tick) couldn't read the list — note it, don't fake it.
        None => {
            feat.field("status", "unavailable (snapshot taken mid-tick; APP lock held)");
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
