//! Mod-coordination logic for the side-channel: the part of "co-op" that is **ours** and is
//! transport-agnostic, so it's host-tested and driven by the harness with no game.
//!
//! [`Peer`] is pure: it consumes inbound [`ModMessage`]s and produces outbound ones; it never
//! touches a transport. [`Session`] glues a `Peer` to a [`Transport`] (the [`Loopback`] bus in
//! tests/harness, the game's `broadcast_packet` in production), so the same logic runs in both.
//!
//! ## Self-healing over an untrusted, maybe-unreliable channel
//! The side-channel rides the game's P2P broadcast, whose delivery guarantees we don't yet know
//! (Steam P2P can drop, duplicate, and reorder). So the coordination is designed to **converge
//! regardless**, which is robust to whatever the rig later reveals:
//! - the host periodically **re-asserts** its authoritative config ([`Peer::maintain`]), so a
//!   dropped [`ModMessage::ConfigSync`] heals on the next tick;
//! - config carries a monotonic **generation** and actions/logs carry a **sequence**, so a
//!   duplicated or reordered frame is ignored rather than applied twice or rolled back;
//! - a heartbeat [`ModMessage::Ping`] drives **liveness**, flagging peers we stop hearing from.
//!
//! What it does NOT cover: the game's own player/world sync (positions, HP) — that's the game's
//! transport and is reverse-engineering-gated (see RIG-RUNBOOK.md). Host identity comes from the
//! game's session manager out of band (here, passed to [`Peer::new`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Config;
use crate::diagnostics::{LogBundle, LogLevel, LogRecord, peer_tag};
use crate::notifications::{Notifications, Severity};
use crate::protocol::{ModMessage, SessionAction, SharedSettings};
use crate::transport::{PeerId, Transport};
use crate::util::{RateLimiter, Version};

/// Maintenance ticks ([`Peer::maintain`] calls) we tolerate hearing nothing from a peer before
/// flagging it as lost. The wall-clock timeout is this times the maintenance cadence the binding
/// layer chooses — so the cadence is load-bearing (see [`Peer::maintain`]); pick it deliberately.
///
/// Set conservatively because liveness is **lossy and role-asymmetric**: a peer is "heard" only via
/// frames that survive the channel, and the host emits two frames per tick (Ping + ConfigSync re-
/// assert) while a client emits one (Ping), so the host→client signal survives loss better than
/// client→host. A small timeout would flicker spurious "Lost contact" banners at a live peer under
/// heavy loss (e.g. ~`drop_rate^(N+1)` per tick of N consecutive lost pings). The banner self-clears
/// on the next received frame, so it's a soft signal; the final value is a tuning decision that
/// wants the rig's measured Steam-P2P loss rate.
const LIVENESS_TIMEOUT_TICKS: u64 = 30;
/// Burst of forwarded log records a client may emit before the limiter throttles it.
const LOG_FORWARD_BURST: u32 = 32;
/// Forwarded-log tokens restored per [`Peer::maintain`] call (the steady-state forwarding rate).
/// Like [`LIVENESS_TIMEOUT_TICKS`], this is denominated in maintenance ticks, so its real
/// logs-per-second is the binding layer's maintain cadence times this — keep that cadence stable.
const LOG_FORWARD_REFILL_PER_TICK: f64 = 8.0;

/// User-facing message for a peer whose mod major-version is incompatible with ours. Single-sourced
/// so the `Peer`'s own notification (below, harness-visible) and the cdylib's overlay surface
/// (`coop/coop.rs`, which derives the same banner onto the drawn notification model) can't drift to
/// different wording.
pub fn version_mismatch_message(peer: PeerId, theirs: Version, ours: Version) -> String {
    format!("Mod version mismatch with {}: they have {theirs}, you have {ours}", peer_tag(peer))
}

/// User-facing message for losing contact with a peer (liveness). Shared like
/// [`version_mismatch_message`].
pub fn lost_contact_message(peer: PeerId) -> String {
    format!("Lost contact with {}", peer_tag(peer))
}

/// User-facing toast when a client adopts the host's pushed settings. Shared like
/// [`version_mismatch_message`].
pub const CONFIG_SYNCED_MESSAGE: &str = "Session settings synced from host";

/// Per-sender monotonic sequence gate: accepts a frame only if its `seq` advances past everything
/// seen from that sender, so a duplicated or reordered-old frame is rejected. The session-action
/// and log-forward dedups share this one tested concept rather than open-coding the comparison
/// (in two easily-skewed directions) at each site.
#[derive(Default)]
struct SeqGate {
    seen: BTreeMap<PeerId, u32>,
}

impl SeqGate {
    /// `true` (and records `seq`) if it's newer than anything seen from `from`; `false` for a
    /// duplicate or reordered-old frame. The first real seq (`>= 1`) always passes the `0` floor.
    fn accept(&mut self, from: PeerId, seq: u32) -> bool {
        let last = self.seen.get(&from).copied().unwrap_or(0);
        if seq > last {
            self.seen.insert(from, seq);
            true
        } else {
            false
        }
    }
}

pub struct Peer {
    id: PeerId,
    /// Who the host is (from the game's session manager). `is_host()` is `id == host_id`.
    host_id: PeerId,
    version: Version,
    config: Config,
    /// Versions advertised by other peers (from their `Hello`).
    peers: BTreeMap<PeerId, Version>,
    notifications: Notifications,
    /// Host-side aggregation of forwarded debug logs.
    log_bundle: LogBundle,

    // --- outbound identity (so receivers can dedup/order our frames) ---
    /// Host's authoritative config generation; bumped whenever the shared settings change.
    config_generation: u32,
    /// Monotonic sequence for our own outbound session actions.
    out_action_seq: u32,
    /// Monotonic sequence for our own outbound log records.
    out_log_seq: u32,
    /// Our heartbeat counter, advertised in `Ping`.
    ping_frame: u64,

    // --- inbound high-water marks (drop stale/duplicate frames) ---
    /// Highest config generation we've applied from the host (`None` until the first sync). This
    /// assumes the host's generation is monotonic across our lifetime, which holds within one host
    /// session. A host *restart* or host migration resets the source counter and would stall here;
    /// handling that needs a host-instance epoch, deferred until the rig shows how the game's
    /// session FSM signals a host change (it's a Layer-2 concern — see ARCHITECTURE.md).
    applied_config_gen: Option<u32>,
    /// Dedup gate for inbound session actions (exactly-once apply per sender).
    action_gate: SeqGate,
    /// Dedup gate for inbound forwarded logs (host-side, exactly-once aggregation per sender).
    log_gate: SeqGate,

    // --- liveness ---
    /// Maintenance-tick clock; advances once per `maintain()`.
    local_tick: u64,
    /// Tick at which we last heard from each peer (its `Hello` or `Ping`).
    last_seen: BTreeMap<PeerId, u64>,
    /// Peers currently flagged as lost (so we banner/clear on the transition, not every tick).
    stale_peers: BTreeSet<PeerId>,

    // --- forwarding throttle ---
    log_limiter: RateLimiter,
    dropped_logs: u64,

    /// Last accepted session action (for harness/inspection).
    last_action: Option<(PeerId, SessionAction)>,
}

impl Peer {
    pub fn new(id: PeerId, host_id: PeerId, version: Version, config: Config) -> Self {
        Self {
            id,
            host_id,
            version,
            config,
            peers: BTreeMap::new(),
            notifications: Notifications::new(),
            log_bundle: LogBundle::new(),
            config_generation: 1,
            out_action_seq: 0,
            out_log_seq: 0,
            ping_frame: 0,
            applied_config_gen: None,
            action_gate: SeqGate::default(),
            log_gate: SeqGate::default(),
            local_tick: 0,
            last_seen: BTreeMap::new(),
            stale_peers: BTreeSet::new(),
            log_limiter: RateLimiter::new(LOG_FORWARD_BURST),
            dropped_logs: 0,
            last_action: None,
        }
    }

    pub fn is_host(&self) -> bool {
        self.id == self.host_id
    }

    /// Messages to send on joining the session: announce our mod version.
    pub fn connect(&mut self) -> Vec<ModMessage> {
        vec![ModMessage::Hello { mod_version: self.version.to_u32() }]
    }

    /// Process one inbound message; return any outbound responses to broadcast.
    pub fn handle(&mut self, from: PeerId, msg: ModMessage) -> Vec<ModMessage> {
        // Ignore our own broadcast if the transport ever echoes it back (a real P2P mesh might):
        // self-frames would otherwise pollute the peer roster and liveness with our own id.
        if from == self.id {
            return vec![];
        }
        // Any frame is evidence the sender is alive — even one the body then discards (a duplicate
        // ConfigSync, a gate-rejected action). This write must stay UNCONDITIONAL: moving it inside
        // the match to "skip rejected frames" would worsen liveness false-positives under loss.
        self.last_seen.insert(from, self.local_tick);

        match msg {
            ModMessage::Hello { mod_version } => {
                let theirs = Version::from_u32(mod_version);
                self.peers.insert(from, theirs);
                if !self.version.compatible_with(theirs) {
                    self.notifications.set_banner(
                        format!("version:{from}"),
                        Severity::Warning,
                        version_mismatch_message(from, theirs, self.version),
                    );
                }
                // The host brings a newcomer in sync with the current shared settings.
                self.broadcast_config()
            }
            ModMessage::ConfigSync { generation, settings } => {
                if from != self.host_id {
                    self.notifications
                        .warn(format!("Ignored ConfigSync from non-host {}", peer_tag(from)));
                } else if generation > self.applied_config_gen.unwrap_or(0) {
                    // Newer than anything we've applied: adopt it. A re-asserted or reordered older
                    // generation falls through to the else and is ignored (idempotent + ordered).
                    self.applied_config_gen = Some(generation);
                    settings.apply_to(&mut self.config);
                    self.notifications.info(CONFIG_SYNCED_MESSAGE);
                }
                vec![]
            }
            ModMessage::SessionAction { seq, action } => {
                // Drop duplicate/reordered-old action frames (apply each exactly once).
                if !self.action_gate.accept(from, seq) {
                    return vec![];
                }
                // Authorize host-only actions by the SENDER's role (not the local UI).
                if action.is_host_only() && from != self.host_id {
                    self.notifications.warn(format!(
                        "Ignored host-only action {action:?} from non-host {}",
                        peer_tag(from)
                    ));
                } else {
                    self.last_action = Some((from, action));
                }
                vec![]
            }
            // Ping is liveness-only; `last_seen` was already refreshed above.
            ModMessage::Ping { .. } => vec![],
            ModMessage::Log(record) => {
                // Only the host aggregates forwarded logs, and only newer records per sender.
                if self.is_host() && self.log_gate.accept(from, record.seq) {
                    self.log_bundle.add(peer_tag(from), record);
                }
                vec![]
            }
        }
    }

    /// Host: the current authoritative shared settings, tagged with the live generation. Non-host
    /// peers have nothing authoritative to assert, so this is empty for them.
    pub fn broadcast_config(&self) -> Vec<ModMessage> {
        if self.is_host() {
            vec![ModMessage::ConfigSync {
                generation: self.config_generation,
                settings: SharedSettings::from(&self.config),
            }]
        } else {
            vec![]
        }
    }

    /// Host: record that the shared settings just changed (bump the generation) and return the
    /// re-broadcast so clients move forward. Call after editing a session-wide setting.
    pub fn mark_config_changed(&mut self) -> Vec<ModMessage> {
        if !self.is_host() {
            return vec![];
        }
        self.config_generation = self.config_generation.wrapping_add(1);
        self.broadcast_config()
    }

    /// Produce an outbound session action stamped with the next sequence (so receivers dedup it).
    pub fn session_action(&mut self, action: SessionAction) -> Vec<ModMessage> {
        self.out_action_seq = self.out_action_seq.wrapping_add(1);
        vec![ModMessage::SessionAction { seq: self.out_action_seq, action }]
    }

    /// Client: forward a local log line to the host, if `[debug] forward_to_host` is on. No-op on
    /// the host, when forwarding is disabled, or when the rate limiter is exhausted (a flooding
    /// client is throttled rather than allowed to bury the side-channel).
    pub fn forward_log(&mut self, level: LogLevel, message: impl Into<String>) -> Vec<ModMessage> {
        if self.is_host() || !self.config.debug.forward_to_host {
            return vec![];
        }
        if !self.log_limiter.try_take() {
            self.dropped_logs = self.dropped_logs.wrapping_add(1);
            return vec![];
        }
        self.out_log_seq = self.out_log_seq.wrapping_add(1);
        vec![ModMessage::Log(LogRecord { seq: self.out_log_seq, level, message: message.into() })]
    }

    /// One maintenance tick, driven by the binding layer on a **fixed cadence** (e.g. once a
    /// second). Returns the frames to broadcast: a liveness heartbeat from everyone, plus the
    /// host's authoritative config re-assertion (which heals any dropped sync). Also advances the
    /// liveness clock + sweep and refills the forward limiter.
    ///
    /// These concerns are intentionally bundled at one cadence because they share one logical
    /// clock — `LIVENESS_TIMEOUT_TICKS` and `LOG_FORWARD_REFILL_PER_TICK` are both per-tick, so the
    /// cadence must stay stable for their wall-clock meaning to hold. If a future consumer needs
    /// the config re-assert on a *slower* beat than the heartbeat, split this into separate
    /// emitters (a `util::Timer` per concern); there's no such consumer yet, so it stays one call.
    pub fn maintain(&mut self) -> Vec<ModMessage> {
        self.local_tick = self.local_tick.wrapping_add(1);
        self.log_limiter.refill(LOG_FORWARD_REFILL_PER_TICK);

        let mut out = Vec::new();
        self.ping_frame = self.ping_frame.wrapping_add(1);
        out.push(ModMessage::Ping { frame: self.ping_frame });
        out.extend(self.broadcast_config()); // host-only; self-heals dropped ConfigSync

        self.sweep_liveness();
        out
    }

    /// Flag peers we haven't heard from within [`LIVENESS_TIMEOUT_TICKS`], and clear the flag when
    /// they come back — bannering only on the transition so a persistent loss shows one banner.
    fn sweep_liveness(&mut self) {
        let now = self.local_tick;
        let mut newly_stale = Vec::new();
        let mut recovered = Vec::new();
        for (&pid, &seen) in &self.last_seen {
            let stale = now.saturating_sub(seen) > LIVENESS_TIMEOUT_TICKS;
            match (stale, self.stale_peers.contains(&pid)) {
                (true, false) => newly_stale.push(pid),
                (false, true) => recovered.push(pid),
                _ => {}
            }
        }
        for pid in newly_stale {
            self.stale_peers.insert(pid);
            self.notifications.set_banner(
                format!("liveness:{pid}"),
                Severity::Warning,
                lost_contact_message(pid),
            );
        }
        for pid in recovered {
            self.stale_peers.remove(&pid);
            self.notifications.clear_banner(&format!("liveness:{pid}"));
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }
    pub fn notifications(&self) -> &Notifications {
        &self.notifications
    }
    pub fn log_bundle(&self) -> &LogBundle {
        &self.log_bundle
    }
    pub fn known_peers(&self) -> &BTreeMap<PeerId, Version> {
        &self.peers
    }
    pub fn last_action(&self) -> Option<(PeerId, SessionAction)> {
        self.last_action
    }
    /// Whether `peer` is currently flagged as lost (for an overlay roster).
    pub fn is_stale(&self, peer: PeerId) -> bool {
        self.stale_peers.contains(&peer)
    }
    /// Forwarded log records this peer has dropped to the rate limiter (for diagnostics).
    pub fn dropped_logs(&self) -> u64 {
        self.dropped_logs
    }
}

/// Binds a [`Peer`] to a [`Transport`]: encodes the peer's outbound messages onto the wire and
/// feeds decoded inbound frames back into it. The harness uses `Session<Loopback>`; the cdylib
/// will use `Session<GameTransport>` over `broadcast_packet`.
pub struct Session<T: Transport> {
    peer: Peer,
    transport: T,
    /// Inbound frames that failed to decode (foreign/corrupt). Surfaced so the binding layer can
    /// tell "quiet" from "receiving garbage" on an unknown P2P channel.
    decode_failures: u64,
}

impl<T: Transport> Session<T> {
    pub fn new(peer: Peer, transport: T) -> Self {
        Self { peer, transport, decode_failures: 0 }
    }

    /// Announce ourselves to the session (sends `Hello`).
    pub fn connect(&mut self) {
        let out = self.peer.connect();
        self.broadcast(out);
    }

    /// One network step: deliver every inbound frame to the peer and broadcast its responses.
    /// Returns the number of frames processed (0 = quiescent), so a driver can loop to convergence.
    pub fn pump(&mut self) -> usize {
        let inbound = self.transport.poll();
        let count = inbound.len();
        for (from, bytes) in inbound {
            // Malformed/foreign frames are dropped — the decoder already rejects hostile input.
            match ModMessage::decode(&bytes) {
                Ok(msg) => {
                    let out = self.peer.handle(from, msg);
                    self.broadcast(out);
                }
                Err(_) => self.decode_failures = self.decode_failures.wrapping_add(1),
            }
        }
        count
    }

    /// Count of inbound frames that failed to decode over this session's life.
    pub fn decode_failures(&self) -> u64 {
        self.decode_failures
    }

    /// One maintenance tick: broadcast the peer's heartbeat + host config re-assertion (drives
    /// self-healing and liveness). The binding layer calls this on a cadence.
    pub fn maintain(&mut self) {
        let out = self.peer.maintain();
        self.broadcast(out);
    }

    /// Encode and broadcast a batch of messages (e.g. from `peer.session_action()`).
    pub fn broadcast(&mut self, messages: Vec<ModMessage>) {
        for m in messages {
            self.transport.send(&m.encode());
        }
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }
    pub fn peer_mut(&mut self) -> &mut Peer {
        &mut self.peer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{FaultModel, Loopback, Transport};

    const HOST: PeerId = 1;
    const CLIENT: PeerId = 2;

    /// Build a host+client pair over a shared loopback, each at the given version.
    fn pair(host_v: Version, client_v: Version) -> (Session<Loopback>, Session<Loopback>) {
        pair_over(Loopback::mesh(&[HOST, CLIENT]), host_v, client_v)
    }

    fn pair_over(
        ends: Vec<Loopback>,
        host_v: Version,
        client_v: Version,
    ) -> (Session<Loopback>, Session<Loopback>) {
        let mut it = ends.into_iter();
        let host = Session::new(Peer::new(HOST, HOST, host_v, Config::default()), it.next().unwrap());
        let client =
            Session::new(Peer::new(CLIENT, HOST, client_v, Config::default()), it.next().unwrap());
        (host, client)
    }

    /// Drive both sessions to convergence on a perfect channel (no frames left in flight).
    fn run(sessions: &mut [&mut Session<Loopback>]) {
        for _ in 0..100 {
            let mut activity = 0;
            for s in sessions.iter_mut() {
                activity += s.pump();
            }
            if activity == 0 {
                return;
            }
        }
        panic!("did not converge");
    }

    #[test]
    fn handshake_exchanges_versions() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().known_peers().get(&CLIENT), Some(&v));
        assert_eq!(client.peer().known_peers().get(&HOST), Some(&v));
        // Compatible versions => no version banner.
        assert!(client.peer().notifications().banners().is_empty());
    }

    #[test]
    fn incompatible_major_raises_a_banner() {
        let (mut host, mut client) = pair(Version::new(1, 0, 0), Version::new(2, 0, 0));
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        let banners = client.peer().notifications().banners();
        assert_eq!(banners.len(), 1);
        assert!(banners[0].message.contains("version mismatch"));
    }

    #[test]
    fn host_config_change_converges_to_the_client() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().config_mut().gameplay.crit_coop = false;

        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]); // handshake triggers the host's ConfigSync

        assert_eq!(client.peer().config().scaling.boss_health, 250);
        assert!(!client.peer().config().gameplay.crit_coop);
        assert!(
            client.peer().notifications().toasts().iter().any(|t| t.message.contains("synced")),
            "client should be notified of the sync"
        );
    }

    #[test]
    fn host_only_action_from_non_host_is_rejected() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);

        // Client (non-host) tries to lock the world: rejected on the host.
        let lock = client.peer_mut().session_action(SessionAction::LockWorld);
        client.broadcast(lock);
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().last_action(), None, "host-only action from a client is dropped");
        assert!(host.peer().notifications().toasts().iter().any(|t| t.message.contains("host-only")));

        // A non-host-only action (JoinWorld) from the client is accepted.
        let join = client.peer_mut().session_action(SessionAction::JoinWorld);
        client.broadcast(join);
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().last_action(), Some((CLIENT, SessionAction::JoinWorld)));
    }

    #[test]
    fn client_forwards_logs_into_the_host_bundle() {
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v);
        client.peer_mut().config_mut().debug.forward_to_host = true;
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);

        let out = client.peer_mut().forward_log(LogLevel::Warn, "something looked off");
        client.broadcast(out);
        run(&mut [&mut host, &mut client]);

        let rendered = host.peer().log_bundle().render();
        assert!(rendered.contains("something looked off"));
        assert!(rendered.contains(&peer_tag(CLIENT)), "attributed to the client's pseudonym");
    }

    // --- self-healing / robustness over a faulty channel ----------------------------------------

    /// Drive both sessions for a bounded number of rounds over a lossy channel, re-asserting each
    /// round (as the binding layer would). Returns when `done` holds or the budget is exhausted.
    fn run_lossy(
        host: &mut Session<Loopback>,
        client: &mut Session<Loopback>,
        rounds: usize,
        mut done: impl FnMut(&Session<Loopback>) -> bool,
    ) {
        for _ in 0..rounds {
            host.maintain();
            client.maintain();
            host.pump();
            client.pump();
            if done(client) {
                return;
            }
        }
    }

    #[test]
    fn config_self_heals_under_heavy_packet_loss() {
        // Deliberately NO connect(): the Hello->ConfigSync handshake is suppressed so the ONLY path
        // to convergence is the host's periodic re-assertion in maintain(). That isolates the
        // self-heal mechanism under test (otherwise a single surviving Hello reply could mask it).
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { drop_rate: 0.6, ..Default::default() };
        let (mut host, mut client) = pair_over(Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 0xBADF00D), v, v);
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().mark_config_changed();

        run_lossy(&mut host, &mut client, 500, |c| c.peer().config().scaling.boss_health == 250);
        assert_eq!(
            client.peer().config().scaling.boss_health,
            250,
            "the host's maintain() re-assertion eventually lands despite 60% loss"
        );
    }

    #[test]
    fn same_generation_redelivery_does_not_reapply() {
        // The generation guard, isolated: a re-delivered frame at the SAME generation must be a
        // no-op even if its payload differs (which is what a stale duplicate looks like). This bites
        // the `generation > applied` guard directly — flip it to `>=` and the second clobbers.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default());
        let mut first = SharedSettings::from(&Config::default());
        first.scaling.boss_health = 175;
        client.handle(HOST, ModMessage::ConfigSync { generation: 5, settings: first });
        assert_eq!(client.config().scaling.boss_health, 175);

        let mut spoof = SharedSettings::from(&Config::default());
        spoof.scaling.boss_health = 999;
        client.handle(HOST, ModMessage::ConfigSync { generation: 5, settings: spoof });
        assert_eq!(client.config().scaling.boss_health, 175, "same generation must not re-apply");
    }

    #[test]
    fn stale_reordered_config_does_not_roll_back() {
        // A newer generation already applied; a late, lower-generation sync must be ignored.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default());
        let mut newer = SharedSettings::from(&Config::default());
        newer.scaling.boss_health = 300;
        let mut older = SharedSettings::from(&Config::default());
        older.scaling.boss_health = 120;

        client.handle(HOST, ModMessage::ConfigSync { generation: 5, settings: newer });
        assert_eq!(client.config().scaling.boss_health, 300);
        client.handle(HOST, ModMessage::ConfigSync { generation: 4, settings: older });
        assert_eq!(client.config().scaling.boss_health, 300, "older generation ignored");
    }

    #[test]
    fn duplicate_action_is_applied_once() {
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, Config::default());
        let frame = ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld };
        host.handle(CLIENT, frame.clone());
        host.handle(CLIENT, frame); // duplicate delivery
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::JoinWorld)));
        // A second, genuinely-new action (higher seq) is still accepted.
        host.handle(CLIENT, ModMessage::SessionAction { seq: 2, action: SessionAction::OpenWorld });
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::OpenWorld)));
    }

    #[test]
    fn duplicate_forwarded_log_is_deduped_on_the_host() {
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, Config::default());
        let rec = LogRecord { seq: 4, level: LogLevel::Info, message: "once".into() };
        host.handle(CLIENT, ModMessage::Log(rec.clone()));
        host.handle(CLIENT, ModMessage::Log(rec)); // duplicate
        assert_eq!(host.log_bundle().len(), 1, "same seq from same peer counted once");
    }

    #[test]
    fn liveness_flags_a_silent_peer_then_clears_on_return() {
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, Config::default());
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32() }); // seen at tick 0

        // Pin the boundary: tolerated through exactly LIVENESS_TIMEOUT_TICKS of silence...
        for _ in 0..LIVENESS_TIMEOUT_TICKS {
            host.maintain(); // no ping from CLIENT arrives
        }
        assert!(!host.is_stale(CLIENT), "not flagged at the tolerance boundary");
        host.maintain(); // ...flagged one tick past it.
        assert!(host.is_stale(CLIENT), "silent peer flagged one tick past the timeout");
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("Lost contact")));

        host.handle(CLIENT, ModMessage::Ping { frame: 1 }); // it comes back
        host.maintain();
        assert!(!host.is_stale(CLIENT), "flag cleared once it's heard from again");
        assert!(host.notifications().banners().is_empty(), "banner torn down");
    }

    #[test]
    fn forward_log_is_rate_limited() {
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default());
        client.config_mut().debug.forward_to_host = true;

        // Drain the initial burst, then everything beyond it is dropped until a refill.
        let mut emitted = 0;
        for _ in 0..(LOG_FORWARD_BURST + 10) {
            if !client.forward_log(LogLevel::Trace, "spam").is_empty() {
                emitted += 1;
            }
        }
        assert_eq!(emitted, LOG_FORWARD_BURST, "only the burst is forwarded");
        assert_eq!(client.dropped_logs(), 10, "the rest are counted as dropped");

        // A maintenance tick refills exactly LOG_FORWARD_REFILL_PER_TICK tokens — pin the amount, not
        // just "some came back", so a change to the refill constant is caught here too.
        client.maintain();
        let mut after_refill = 0;
        while !client.forward_log(LogLevel::Trace, "after refill").is_empty() {
            after_refill += 1;
        }
        assert_eq!(after_refill, LOG_FORWARD_REFILL_PER_TICK as u32, "one tick grants exactly the refill");
    }

    #[test]
    fn self_frames_are_ignored() {
        // If the transport ever echoes our own broadcast back, it must not enter the roster or
        // liveness as a phantom peer.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, Config::default());
        let out = host.handle(HOST, ModMessage::Hello { mod_version: v.to_u32() });
        assert!(out.is_empty());
        assert!(host.known_peers().is_empty(), "self must not be added to the roster");
        assert!(!host.is_stale(HOST));
    }

    #[test]
    fn session_counts_undecodable_frames() {
        let v = Version::new(0, 1, 0);
        let mut ends = Loopback::mesh(&[HOST, CLIENT]);
        let mut raw = ends.pop().unwrap(); // CLIENT raw endpoint
        let host_end = ends.pop().unwrap(); // HOST endpoint
        let mut host = Session::new(Peer::new(HOST, HOST, v, Config::default()), host_end);

        raw.send(b"not a UC frame at all");
        host.pump();
        assert_eq!(host.decode_failures(), 1, "garbage on the wire is observable, not silent");
    }

    #[test]
    fn config_converges_under_drop_duplicate_and_reorder_together() {
        // The whole adversarial channel at once: heavy loss, duplication, and reordering.
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { drop_rate: 0.4, duplicate_rate: 0.4, reorder: true };
        let (mut host, mut client) = pair_over(Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 0x5EED), v, v);
        host.peer_mut().config_mut().scaling.enemy_health = 80;
        host.peer_mut().config_mut().gameplay.allow_summons = false;
        host.peer_mut().mark_config_changed();
        // No connect(): convergence here is solely via the host's maintain() re-assertion, proving
        // the self-heal survives drop + duplicate + reorder all at once, not just a lucky handshake.

        run_lossy(&mut host, &mut client, 800, |c| {
            c.peer().config().scaling.enemy_health == 80 && !c.peer().config().gameplay.allow_summons
        });
        assert_eq!(client.peer().config().scaling.enemy_health, 80);
        assert!(!client.peer().config().gameplay.allow_summons);
    }
}
