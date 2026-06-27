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
//!   the overlay's debug surfaces each ~10 Hz while one is shown, via [`crate::debug_panel`]. So
//!   the report model drives the log *and* a live HUD; the only thing that differs is the sink.

use eldenring::cs::{CSEventFlagMan, CSSessionManager, GameDataMan, WorldAreaTime};
use unseamless_core::config::Config;
use unseamless_core::diagnostics::{DiagnosticReport, FlagScanner};
use unseamless_core::util::{FrameThrottle, Timer};

use crate::feature::{Feature, Tick};

/// Labels for the local player's 7 status-ailment `resistance_gauges`. Ordered to match the SDK's own
/// adjacent `*_resist` fields on `PlayerGameData` (poison, rot, bleed, death, frost, sleep, madness) —
/// the canonical ER internal resist order, which the parallel gauge arrays almost certainly follow (note
/// this differs from the in-game status-bar order: death precedes frost here). Still rig-confirmable: see
/// the note in [`build_report`] — apply one known ailment and watch which index climbs, relabel if off.
const AILMENTS: [&str; 7] =
    ["poison", "scarlet_rot", "hemorrhage", "death_blight", "frostbite", "sleep", "madness"];

/// A snapshot of the local player's vitals + status gauges, copied out of `PlayerGameData` so no game
/// borrow escapes the singleton access (mirrors how the session read is gathered up front in
/// [`build_report`]). Each `(cur, max)` pair is current vs. current-max.
struct Vitals {
    hp: (u32, u32),
    fp: (u32, u32),
    stamina: (u32, u32),
    /// Status-ailment resistance *remaining* per ailment (indexed by [`AILMENTS`]) — `PlayerGameData`'s
    /// `resistance_gauges`. RIG-CONFIRMED: this reads as the resistance LEFT, full at rest and depleting
    /// as buildup accrues (procs near 0), NOT the accrued buildup. So accrued buildup = `gauge_max - gauge`.
    /// See the rig-confirmed note in [`build_report`]'s status section.
    gauges: [u32; 7],
    /// Proc threshold (full resistance / max buildup) per ailment.
    gauge_max: [u32; 7],
    /// Active-proc timer per ailment (nonzero while the ailment is procced).
    proc_timers: [f32; 7],
}

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
    // Live player vitals + status gauges, copied out of GameDataMan's main player. The registry can
    // surface GameDataMan before its members are wired (the CLAUDE.md unwired-pointer caveat — same guard
    // boot_volume uses on `game_settings`), so null-check the player-data pointer before dereferencing.
    // `Option<Option<Vitals>>`: outer `None` = no GameDataMan singleton; inner `None` = singleton up but
    // its player-data pointer not wired yet. Kept distinct (not flattened) so the section below reports
    // which of the two it is, rather than conflating them.
    let vitals = crate::sdk::with_instance::<GameDataMan, _>(|gd| {
        if gd.main_player_game_data.as_ptr().is_null() {
            return None;
        }
        let p: &eldenring::cs::PlayerGameData = &gd.main_player_game_data;
        Some(Vitals {
            hp: (p.current_hp, p.current_max_hp),
            fp: (p.current_fp, p.current_max_fp),
            stamina: (p.current_stamina, p.current_max_stamina),
            gauges: p.resistance_gauges,
            gauge_max: p.resistance_gauge_max,
            proc_timers: p.proc_status_timers,
        })
    });

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

    // Per-stage connect report: only present once an attempt has begun (a configured peer or rung-4
    // discovery), so a solo log stays uncluttered. This is what makes a failed two-player attempt
    // diagnosable from one log — one-way NAT vs no-receive vs version mismatch vs empty lobby filter.
    if let Some(cr) = crate::coop::connect_report() {
        let sec = r.section("coop_connect");
        let fields = cr.fields();
        // Summary headline: the failure if the attempt has one, else the handshake state, else just
        // "connecting" — the one line that says how the attempt is going without the full breakdown.
        let headline = fields
            .iter()
            .find(|(k, _)| k == "failure")
            .map(|(_, v)| format!("failed: {v}"))
            .or_else(|| {
                fields.iter().find(|(k, _)| k == "handshake").map(|(_, v)| format!("handshake {v}"))
            })
            .unwrap_or_else(|| "connecting".to_string());
        for (k, v) in &fields {
            sec.field(k, v);
        }
        sec.summary_line("connect", headline);
    }

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
            sec.summary_line(
                "session",
                format!("{:?}, {}/{} players", v.lobby_state, v.players, v.player_limit),
            );
        }
        None => {
            sec.field("status", "no CSSessionManager yet (pre-init / title screen)");
            sec.summary_line("session", "no session (pre-init / title screen)");
        }
    }

    // Full: in_gameplay + one row per feature. Summary (for the concise panel): in_gameplay + a single
    // ok-count rollup, naming only the DISABLED (panicked) ones — those are the lines you actually want
    // to see at a glance, so a healthy build collapses ~13 rows to two.
    let feat = r.section("features");
    feat.field("in_gameplay", in_gameplay);
    feat.summary_line("in_gameplay", in_gameplay);
    match features {
        // Before registration completes (earliest boot) the lock-free registry isn't set yet.
        None => {
            feat.field("status", "not registered yet (pre-init)");
            feat.summary_line("features", "not registered yet (pre-init)");
        }
        Some(list) if list.is_empty() => {
            feat.field("status", "not registered yet");
            feat.summary_line("features", "not registered yet");
        }
        Some(list) => {
            let total = list.len();
            let disabled: Vec<&str> = list.iter().filter(|(_, d)| *d).map(|(n, _)| *n).collect();
            for (name, d) in &list {
                feat.field(*name, if *d { "DISABLED (panicked)" } else { "ok" });
            }
            let ok = total - disabled.len();
            if disabled.is_empty() {
                feat.summary_line("features", format!("{ok}/{total} ok"));
            } else {
                feat.summary_line(
                    "features",
                    format!("{ok}/{total} ok, DISABLED: {}", disabled.join(", ")),
                );
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

    // World time of day: the live clock plus the engine's target-of-day, so the time-lock is
    // self-serve verifiable in the overlay panel (and any shared log dump) without grepping for a log
    // line. `engine_target` is `WorldAreaTime`'s own target field — our re-assert drives it while the
    // lock holds (so `clock` tracks it), but with the lock off it's the engine's natural progression
    // target, not our config. `None` until the WorldAreaTime singleton is live (menu / loading); the
    // `lock` flag is the config intent.
    let area_time = crate::sdk::with_instance::<WorldAreaTime, _>(|t| {
        (t.clock.hours(), t.clock.minutes(), t.target_hour, t.target_minute)
    });
    let wt = r.section("world_time");
    wt.field("lock", crate::state::with(|c| c.world_time.lock));
    match area_time {
        Some((h, m, th, tm)) => {
            wt.field("clock", format!("{h:02}:{m:02}")).field("engine_target", format!("{th:02}:{tm:02}"));
        }
        None => {
            wt.field("status", "no WorldAreaTime yet (menu / loading)");
        }
    }

    // Live player vitals + status, from GameDataMan's main player — exact current/max HP, FP, stamina
    // (not eyeballed off the bars) so death-debuff and scaling checks read precise numbers. Degrades to a
    // "not live" note off the playfield (title screen / pre-init), like the session section.
    match vitals {
        Some(Some(v)) => {
            r.section("vitals")
                .field("hp", format!("{}/{}", v.hp.0, v.hp.1))
                .field("fp", format!("{}/{}", v.fp.0, v.fp.1))
                .field("stamina", format!("{}/{}", v.stamina.0, v.stamina.1));

            // Status ailments (poison, rot, bleed, ...): accrued buildup / proc threshold, plus the
            // active-proc timer while one is ticking. Only ailments that are building or active are
            // listed, so the panel stays quiet when you're clean and spotlights what's accumulating
            // during a test.
            //
            // BUILDUP vs RESISTANCE-REMAINING, RIG-CONFIRMED: `resistance_gauges[i]` is the resistance
            // *remaining* (full at rest, depleting toward 0 as buildup accrues), NOT the accrued buildup.
            // Confirmed on the rig: a clean player reads "312/312" on every ailment, and applying one
            // ailment (standing in Scarlet Rot) makes the accrued buildup `gauge_max - gauge` climb 0 -> max
            // as expected. So we display `gauge_max - gauge` and only list an ailment once it's actually
            // building (`buildup > 0`) or procced.
            //
            // ORDER ASSUMED, rig-confirmable: the 7 gauges are labeled in the common ER status order
            // ([`AILMENTS`]). To verify/fix, watch which index climbs when that one ailment is applied —
            // relabel AILMENTS if it's off.
            let status = r.section("status");
            let mut active: Vec<&str> = Vec::new();
            for (i, name) in AILMENTS.iter().enumerate() {
                let (gauge, max, timer) = (v.gauges[i], v.gauge_max[i], v.proc_timers[i]);
                let buildup = max.saturating_sub(gauge);
                if buildup > 0 || timer > 0.0 {
                    active.push(*name);
                    if timer > 0.0 {
                        status.field(*name, format!("{buildup}/{max} (proc {timer:.1}s)"));
                    } else {
                        status.field(*name, format!("{buildup}/{max}"));
                    }
                }
            }
            // Summary (concise panel): just which ailments are in play, no per-ailment numbers — the
            // detail pane carries the buildup/proc values.
            if active.is_empty() {
                status.field("ailments", "none building or active");
                status.summary_line("ailments", "none building or active");
            } else {
                status.summary_line("ailments", active.join(", "));
            }
        }
        Some(None) => {
            r.section("vitals").field("status", "GameDataMan up, player data not wired yet");
        }
        None => {
            r.section("vitals").field("status", "no GameDataMan yet (pre-init / title screen)");
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

/// The game-thread feature that feeds the overlay's live debug panel. Always registered (it's not a
/// `[debug.probes]` lever — the panel is a built-in surface), but inert unless the overlay says a
/// report is wanted (summary panel or any detail pane showing), so when off it costs a single atomic
/// load per frame.
pub fn debug_panel_feature() -> Box<dyn Feature> {
    Box::new(DebugPanelProbe::new())
}

/// Publishes a live [`DiagnosticReport`] snapshot for the overlay's debug panel — but only while a
/// report is wanted, i.e. the summary panel or any detail pane is showing
/// ([`crate::debug_panel::report_wanted`]). Throttled to ~10 Hz: the panel is for reading,
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
        // Off costs one atomic load: do nothing unless the overlay says a report is wanted (summary
        // panel or any detail pane showing). The throttle only advances on wanted frames, so its
        // period is in wanted-frames (which is fine).
        if !crate::debug_panel::report_wanted() || !self.throttle.tick() {
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
