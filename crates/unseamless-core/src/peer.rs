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
/// layer chooses.
const LIVENESS_TIMEOUT_TICKS: u64 = 10;
/// Burst of forwarded log records a client may emit before the limiter throttles it.
const LOG_FORWARD_BURST: u32 = 32;
/// Forwarded-log tokens restored per [`Peer::maintain`] call (the steady-state forwarding rate).
const LOG_FORWARD_REFILL_PER_TICK: f64 = 8.0;

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
    /// Highest config generation we've applied from the host (`None` until the first sync).
    applied_config_gen: Option<u32>,
    /// Highest action seq seen from each sender.
    last_action_seq: BTreeMap<PeerId, u32>,
    /// Highest forwarded-log seq seen from each sender (host-side dedup).
    last_log_seq: BTreeMap<PeerId, u32>,

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
            last_action_seq: BTreeMap::new(),
            last_log_seq: BTreeMap::new(),
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
        // Any frame is evidence the sender is alive.
        self.last_seen.insert(from, self.local_tick);

        match msg {
            ModMessage::Hello { mod_version } => {
                let theirs = Version::from_u32(mod_version);
                self.peers.insert(from, theirs);
                if !self.version.compatible_with(theirs) {
                    self.notifications.set_banner(
                        format!("version:{from}"),
                        Severity::Warning,
                        format!(
                            "Mod version mismatch with {}: they have {}, you have {}",
                            peer_tag(from),
                            fmt_version(theirs),
                            fmt_version(self.version),
                        ),
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
                    self.notifications.info("Session settings synced from host");
                }
                vec![]
            }
            ModMessage::SessionAction { seq, action } => {
                // Drop duplicate/reordered-old action frames (apply each exactly once).
                if seq <= self.last_action_seq.get(&from).copied().unwrap_or(0) {
                    return vec![];
                }
                self.last_action_seq.insert(from, seq);
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
                if self.is_host() && record.seq > self.last_log_seq.get(&from).copied().unwrap_or(0) {
                    self.last_log_seq.insert(from, record.seq);
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

    /// One maintenance tick, driven by the binding layer on a cadence (e.g. once a second). Returns
    /// the frames to broadcast: a liveness heartbeat from everyone, plus the host's authoritative
    /// config re-assertion (which heals any dropped sync). Also refills the forward limiter and
    /// updates per-peer liveness.
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
                format!("Lost contact with {}", peer_tag(pid)),
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

fn fmt_version(v: Version) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

/// Binds a [`Peer`] to a [`Transport`]: encodes the peer's outbound messages onto the wire and
/// feeds decoded inbound frames back into it. The harness uses `Session<Loopback>`; the cdylib
/// will use `Session<GameTransport>` over `broadcast_packet`.
pub struct Session<T: Transport> {
    peer: Peer,
    transport: T,
}

impl<T: Transport> Session<T> {
    pub fn new(peer: Peer, transport: T) -> Self {
        Self { peer, transport }
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
            if let Ok(msg) = ModMessage::decode(&bytes) {
                let out = self.peer.handle(from, msg);
                self.broadcast(out);
            }
        }
        count
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
    use crate::transport::{FaultModel, Loopback};

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
        host.peer_mut().config_mut().gameplay.allow_invaders = false;

        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]); // handshake triggers the host's ConfigSync

        assert_eq!(client.peer().config().scaling.boss_health, 250);
        assert!(!client.peer().config().gameplay.allow_invaders);
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

        // A non-host-only action (GiveEmber) from the client is accepted.
        let ember = client.peer_mut().session_action(SessionAction::GiveEmber);
        client.broadcast(ember);
        run(&mut [&mut host, &mut client]);
        assert_eq!(host.peer().last_action(), Some((CLIENT, SessionAction::GiveEmber)));
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
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { drop_rate: 0.6, ..Default::default() };
        let (mut host, mut client) = pair_over(Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 0xBADF00D), v, v);
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().mark_config_changed();
        host.connect();
        client.connect();

        run_lossy(&mut host, &mut client, 500, |c| c.peer().config().scaling.boss_health == 250);
        assert_eq!(
            client.peer().config().scaling.boss_health,
            250,
            "host's re-assertion eventually lands despite 60% loss"
        );
    }

    #[test]
    fn duplicated_delivery_applies_config_once() {
        // Every frame is delivered twice; the generation guard must make the second a no-op (one
        // "synced" toast, not two).
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { duplicate_rate: 1.0, ..Default::default() };
        let (mut host, mut client) = pair_over(Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 1), v, v);
        host.peer_mut().config_mut().scaling.boss_health = 175;
        host.peer_mut().mark_config_changed();
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);

        assert_eq!(client.peer().config().scaling.boss_health, 175);
        let synced = client.peer().notifications().toasts().iter().filter(|t| t.message.contains("synced")).count();
        assert_eq!(synced, 1, "duplicate ConfigSync must not re-apply / re-toast");
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
        let frame = ModMessage::SessionAction { seq: 1, action: SessionAction::GiveEmber };
        host.handle(CLIENT, frame.clone());
        host.handle(CLIENT, frame); // duplicate delivery
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::GiveEmber)));
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

        for _ in 0..(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain(); // no ping from CLIENT arrives
        }
        assert!(host.is_stale(CLIENT), "silent peer flagged after the timeout");
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

        // A maintenance tick refills some tokens, allowing forwarding to resume.
        client.maintain();
        assert!(!client.forward_log(LogLevel::Trace, "after refill").is_empty());
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
        host.connect();
        client.connect();

        run_lossy(&mut host, &mut client, 800, |c| {
            c.peer().config().scaling.enemy_health == 80 && !c.peer().config().gameplay.allow_summons
        });
        assert_eq!(client.peer().config().scaling.enemy_health, 80);
        assert!(!client.peer().config().gameplay.allow_summons);
    }
}
