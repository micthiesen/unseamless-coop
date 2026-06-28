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
use crate::crypto::{auth_proof, proofs_match};
use crate::diagnostics::{LogBundle, LogLevel, LogRecord, peer_tag};
use crate::notifications::{Notifications, Severity};
use crate::protocol::{AuthNonce, ModMessage, SessionAction, SharedSettings};
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

/// User-facing message for a peer whose authentication proof didn't verify — i.e. it presented the
/// wrong co-op password (or isn't actually running our mod with our key). Plain/diagnostic voice,
/// shared like [`version_mismatch_message`] so the `Peer`'s notification and the cdylib's overlay
/// can't drift. A failed peer is **not linked**: its `ConfigSync`/actions are never applied.
pub fn auth_failed_message(peer: PeerId) -> String {
    format!("Authentication failed with {} (wrong co-op password)", peer_tag(peer))
}

// Per-peer banner keys. Single-sourced because each is now set and cleared from *different* methods
// (auth/version in `verify_auth`, cleared in `sweep_liveness`; liveness set+cleared in the sweep), so
// an inline `format!` at each site could drift the prefix and leave a banner that never clears.
fn auth_banner_key(peer: PeerId) -> String {
    format!("auth:{peer}")
}
fn version_banner_key(peer: PeerId) -> String {
    format!("version:{peer}")
}
fn liveness_banner_key(peer: PeerId) -> String {
    format!("liveness:{peer}")
}

/// User-facing toast when a client adopts the host's pushed settings. Shared like
/// [`version_mismatch_message`].
pub const CONFIG_SYNCED_MESSAGE: &str = "Session settings synced from host";

/// ER-voiced in-world presence toast shown when a co-op partner's handshake lands — the lore-register
/// counterpart to the plain "connected" confirmation, emitted *alongside* it (see `coop/coop.rs`).
/// Player join/leave is an *effect*, so per CLAUDE.md's "Message voice" rule it's worded in
/// FromSoft's terse, weighty register and carries **no raw mechanical values** — no SteamID, no peer
/// tag: presence reads fine without an identity, and leaving it out keeps a player's id off the
/// overlay. Single-sourced like [`CONFIG_SYNCED_MESSAGE`] so core and the overlay can't drift.
/// ("Cooperator" is the game's own term for a summoned co-op phantom, so it stays in register.)
pub const PEER_ARRIVED_MESSAGE: &str = "A cooperator has arrived in your world.";

/// ER-voiced presence toast shown when a linked partner falls silent (the liveness "lost" edge). The
/// lore-voice companion to the plain diagnostic "Lost contact" banner — purely **additive**, it does
/// not replace the banner or change its plain voice. Identity-free and value-free for the same
/// reasons as [`PEER_ARRIVED_MESSAGE`].
pub const PEER_DEPARTED_MESSAGE: &str = "A cooperator has departed your world.";

/// ER-voiced presence toast shown when a partner we'd flagged as silent is heard from again (the
/// liveness *recovery* edge). The liveness flag flaps lost↔recovered on a jittery connection, so
/// [`PEER_DEPARTED_MESSAGE`] alone would read as the partner "departing" repeatedly and never coming
/// back; this is its symmetric companion so the presence pair stays balanced. (Distinct from
/// [`PEER_ARRIVED_MESSAGE`], the once-per-session *first* link — a transient liveness blip never
/// un-links a peer, so a recovery is a return, not a fresh arrival.) Additive to clearing the plain
/// "Lost contact" banner; identity- and value-free like the rest.
pub const PEER_RETURNED_MESSAGE: &str = "A cooperator has returned to your world.";

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
    /// Our per-session authentication nonce, advertised in every `Hello`. Random per session
    /// (supplied at construction by the binding layer, which owns the entropy source); it makes our
    /// outbound [`ModMessage::Auth`] proofs non-replayable. See [`Peer::new`].
    auth_nonce: AuthNonce,
    /// Versions advertised by other peers (from their `Hello`).
    peers: BTreeMap<PeerId, Version>,
    /// Nonces advertised by other peers (from their `Hello`), needed to verify their `Auth` proof and
    /// to build our proof *to* them.
    peer_nonces: BTreeMap<PeerId, AuthNonce>,
    /// Peers whose [`ModMessage::Auth`] proof we've verified against our shared co-op password. Only
    /// a linked peer's `ConfigSync` is applied and only a linked peer's session actions are accepted —
    /// a stranger who merely discovered the lobby never clears this bar. This distinguishes a `peers`
    /// entry (merely discovered/known) from a linked one (authenticated).
    ///
    /// Like `peers`/`peer_nonces`/`last_seen`, this is keyed by the transport [`PeerId`] (the stable
    /// Steam id in production) and is **never pruned** here: re-linking a peer after a transient
    /// liveness blip is a no-op, which is what makes the handshake self-heal. Eviction on a real
    /// session-*leave* is deferred to the binding layer (Layer 2), which owns the game's session FSM —
    /// the same boundary the `applied_config_gen` host-restart note (below) defers to.
    linked: BTreeSet<PeerId>,
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
    /// Build a peer.
    ///
    /// `auth_nonce` is this session's random authentication nonce — the binding layer (cdylib) must
    /// supply **fresh, unpredictable** bytes per session from its CSPRNG (the same entropy source it
    /// uses to generate the default password); core has none. Freshness is what makes a captured proof
    /// non-replayable across sessions, and core can't verify it, so it's a binding-layer obligation.
    /// The shared co-op password is read from `config.session.password` (it is never sent over the
    /// wire and `ConfigSync` never overwrites it), so it is the secret both sides key their
    /// [`auth_proof`]s with — no separate password arg. The password is assumed already validated for
    /// length by the startup guard (`Config::password_is_valid`, enforced in the cdylib before
    /// install); core imposes no floor, so an empty password would link other empty-password peers.
    pub fn new(
        id: PeerId,
        host_id: PeerId,
        version: Version,
        config: Config,
        auth_nonce: AuthNonce,
    ) -> Self {
        Self {
            id,
            host_id,
            version,
            config,
            auth_nonce,
            peers: BTreeMap::new(),
            peer_nonces: BTreeMap::new(),
            linked: BTreeSet::new(),
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

    /// Messages to send on joining the session: announce our mod version + our auth nonce so peers
    /// can verify the [`ModMessage::Auth`] proof we send them in reply to their `Hello`.
    pub fn connect(&mut self) -> Vec<ModMessage> {
        vec![self.hello()]
    }

    /// Our `Hello` (version + this session's auth nonce). Sent at [`connect`](Peer::connect) and
    /// re-asserted each [`maintain`](Peer::maintain) so a peer eventually learns our nonce even over a
    /// lossy channel (the handshake self-heals like `ConfigSync` does).
    fn hello(&self) -> ModMessage {
        ModMessage::Hello { mod_version: self.version.to_u32(), nonce: self.auth_nonce }
    }

    /// The proof we present to `peer` (we are the prover, `peer` is the verifier): keyed by the shared
    /// password and bound to both nonces. `None` until we've heard `peer`'s `Hello` (we need its
    /// nonce). See [`auth_proof`].
    fn proof_for(&self, peer: PeerId) -> Option<ModMessage> {
        let peer_nonce = self.peer_nonces.get(&peer)?;
        // We are the prover, `peer` the verifier — same (verifier, prover) ordering both sides use.
        let proof = auth_proof(
            peer,
            self.id,
            peer_nonce,
            &self.auth_nonce,
            &self.config.session.password,
        );
        Some(ModMessage::Auth { to: peer, proof })
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
            ModMessage::Hello { mod_version, nonce } => {
                self.peers.insert(from, Version::from_u32(mod_version));
                self.peer_nonces.insert(from, nonce);
                // The version-mismatch banner is deferred to `verify_auth` (raised once the peer
                // authenticates), so an unauthenticated stranger who merely discovered the lobby can't
                // plant a banner on a real player's overlay.
                //
                // Reply with our password-keyed proof so the peer can authenticate us. We do NOT echo
                // a `Hello` here (that would ping-pong forever between two peers); our nonce reaches
                // the peer via `connect`'s `Hello` and `maintain`'s periodic re-assert. We hold off on
                // the host's `ConfigSync` until the peer is *linked* — an unauthenticated peer gets no
                // settings. `proof_for` is `Some` since we just recorded the peer's nonce.
                self.proof_for(from).into_iter().collect()
            }
            ModMessage::Auth { to, proof } => {
                // Ignore a proof addressed to another peer (a broadcast frame meant for someone else):
                // it's keyed to *their* nonce, so verifying it here would spuriously fail.
                if to != self.id {
                    return vec![];
                }
                self.verify_auth(from, proof)
            }
            ModMessage::ConfigSync { generation, settings } => {
                if !self.is_linked(from) {
                    // Unauthenticated sender — a stranger, or the host before its proof verifies. Drop
                    // quietly: no warn toast (a stranger could otherwise spam toasts and evict
                    // legitimate ones), and the host's `maintain` re-assert re-delivers the sync once
                    // the handshake completes. This check is first so an unlinked peer never reaches
                    // the non-host warn below.
                } else if from != self.host_id {
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
                // Reject actions from an unauthenticated peer before anything else — a stranger who
                // discovered the lobby must not be able to drive the session (grab a seat, lock the
                // world, etc.). Drop quietly (no banner: a stranger could otherwise spam banners) and
                // without touching the dedup gate (so it can't desync a later linked sender's seq).
                if !self.is_linked(from) {
                    return vec![];
                }
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
                // Only the host aggregates forwarded logs, only from linked peers (so a stranger can't
                // inject lines into the host's shareable diagnostic bundle), and only newer records.
                if self.is_host() && self.is_linked(from) && self.log_gate.accept(from, record.seq) {
                    self.log_bundle.add(peer_tag(from), record);
                }
                vec![]
            }
        }
    }

    /// Verify a peer's `Auth` proof (it is addressed to us). On success we mark it **linked** and the
    /// host brings the freshly-authenticated newcomer in sync with the current shared settings. On
    /// mismatch we banner (plain/diagnostic voice) and leave it unlinked, so its `ConfigSync`/actions
    /// are never applied.
    fn verify_auth(&mut self, from: PeerId, proof: crate::protocol::AuthProofBytes) -> Vec<ModMessage> {
        // We need the peer's nonce (from its `Hello`) to recompute the expected proof. If its `Auth`
        // raced ahead of its `Hello`, drop quietly — the peer's re-asserted `Hello` heals it.
        let Some(&peer_nonce) = self.peer_nonces.get(&from) else {
            return vec![];
        };
        // We are the verifier, `from` the prover: same (verifier, prover) ordering both sides use, with
        // the id pair taken from the transport (not the wire) so a reflected proof fails (see
        // `auth_proof`).
        let expected = auth_proof(
            self.id,
            from,
            &self.auth_nonce,
            &peer_nonce,
            &self.config.session.password,
        );
        if proofs_match(&expected, &proof) {
            let newly_linked = self.linked.insert(from);
            self.notifications.clear_banner(&auth_banner_key(from));
            if newly_linked {
                // Now that the peer is a verified party member, surface a version-incompatibility
                // banner (deferred from the `Hello` so a stranger can't plant one). The peer's
                // version was recorded when its `Hello` arrived.
                if let Some(&theirs) = self.peers.get(&from)
                    && !self.version.compatible_with(theirs)
                {
                    self.notifications.set_banner(
                        version_banner_key(from),
                        Severity::Warning,
                        version_mismatch_message(from, theirs, self.version),
                    );
                }
                // Host: a newly-linked peer gets the current settings now (don't wait for the next
                // `maintain` re-assert). A re-verified already-linked peer needs nothing new.
                if self.is_host() {
                    return self.broadcast_config();
                }
            }
            return vec![];
        }
        // Wrong password (or not our mod). Don't un-link an already-linked peer — that would let a
        // forged bad proof spoofing its id evict a legitimately-authenticated peer.
        if !self.is_linked(from) {
            self.notifications.set_banner(
                auth_banner_key(from),
                Severity::Warning,
                auth_failed_message(from),
            );
        }
        vec![]
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
        // Re-assert our `Hello` so a peer eventually learns our nonce (and re-triggers our proof
        // reply) even over a lossy channel — the handshake self-heals like `ConfigSync` does.
        out.push(self.hello());
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
            // A departed peer's handshake banners are no longer actionable — tear them down so a
            // wrong-password or version-mismatched peer that leaves doesn't strand a stuck banner.
            self.notifications.clear_banner(&auth_banner_key(pid));
            self.notifications.clear_banner(&version_banner_key(pid));
            // Only warn about losing a peer we actually authenticated; an unlinked stranger that
            // pinged once and vanished is not a party member worth a "Lost contact" banner.
            if self.is_linked(pid) {
                self.notifications.set_banner(
                    liveness_banner_key(pid),
                    Severity::Warning,
                    lost_contact_message(pid),
                );
            }
        }
        for pid in recovered {
            self.stale_peers.remove(&pid);
            self.notifications.clear_banner(&liveness_banner_key(pid));
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
    /// Whether `peer` has authenticated (its `Auth` proof verified against our co-op password). Only
    /// a linked peer's `ConfigSync` is applied and only a linked peer's actions are accepted — the
    /// overlay roster can show this to distinguish a discovered-but-unverified peer from a real one.
    pub fn is_linked(&self, peer: PeerId) -> bool {
        self.linked.contains(&peer)
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
    use crate::protocol::AUTH_NONCE_LEN;
    use crate::transport::{FaultModel, Loopback, Transport};

    const HOST: PeerId = 1;
    const CLIENT: PeerId = 2;
    /// Distinct per-session nonces so the two peers' proofs differ on the wire.
    const HOST_NONCE: AuthNonce = [0x11; AUTH_NONCE_LEN];
    const CLIENT_NONCE: AuthNonce = [0x22; AUTH_NONCE_LEN];
    /// Default matching co-op password for a pair, so the handshake links by default.
    const PW: &str = "co-op-password";

    /// A [`Config`] with the given co-op password (the shared key the auth proof is keyed by).
    fn config_with_pw(password: &str) -> Config {
        let mut c = Config::default();
        c.session.password = password.into();
        c
    }

    /// Build a host+client pair over a shared loopback, each at the given version, sharing [`PW`].
    fn pair(host_v: Version, client_v: Version) -> (Session<Loopback>, Session<Loopback>) {
        pair_over(Loopback::mesh(&[HOST, CLIENT]), host_v, client_v)
    }

    fn pair_over(
        ends: Vec<Loopback>,
        host_v: Version,
        client_v: Version,
    ) -> (Session<Loopback>, Session<Loopback>) {
        pair_over_with_pw(ends, host_v, client_v, PW, PW)
    }

    /// Like [`pair_over`] but with explicit (possibly mismatched) passwords — for auth tests.
    fn pair_over_with_pw(
        ends: Vec<Loopback>,
        host_v: Version,
        client_v: Version,
        host_pw: &str,
        client_pw: &str,
    ) -> (Session<Loopback>, Session<Loopback>) {
        let mut it = ends.into_iter();
        let host = Session::new(
            Peer::new(HOST, HOST, host_v, config_with_pw(host_pw), HOST_NONCE),
            it.next().unwrap(),
        );
        let client = Session::new(
            Peer::new(CLIENT, HOST, client_v, config_with_pw(client_pw), CLIENT_NONCE),
            it.next().unwrap(),
        );
        (host, client)
    }

    /// Build a bare host [`Peer`] (no transport) and drive it to *link* `CLIENT` by feeding the
    /// client's `Hello` + a valid `Auth` proof, so the isolated host-side tests below (which inject
    /// gated frames directly) operate on an authenticated peer.
    fn linked_host(v: Version) -> Peer {
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        // CLIENT is the prover, HOST the verifier.
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(host.is_linked(CLIENT), "test setup: CLIENT should be linked");
        host
    }

    /// Like [`linked_host`] but a bare client [`Peer`] that has linked `HOST`.
    fn linked_client(v: Version) -> Peer {
        let mut client = Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE);
        client.handle(HOST, ModMessage::Hello { mod_version: v.to_u32(), nonce: HOST_NONCE });
        let proof = crate::crypto::auth_proof(CLIENT, HOST, &CLIENT_NONCE, &HOST_NONCE, PW);
        client.handle(HOST, ModMessage::Auth { to: CLIENT, proof });
        assert!(client.is_linked(HOST), "test setup: HOST should be linked");
        client
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
    fn presence_messages_are_er_voiced_and_value_free() {
        // Presence (join/leave) is an in-world EFFECT, so its messages stay in the lore register:
        // non-empty, no raw mechanical values (no digits, no SteamID/peer tag), and identity-free for
        // privacy. This deliberately differs from the *diagnostic* helpers (lost_contact_message etc.),
        // which DO carry a peer tag — these presence lines must not, so pin that here.
        let all = [PEER_ARRIVED_MESSAGE, PEER_DEPARTED_MESSAGE, PEER_RETURNED_MESSAGE];
        for msg in all {
            assert!(!msg.is_empty(), "presence message must say something");
            assert!(
                !msg.chars().any(|c| c.is_ascii_digit()),
                "lore voice carries no raw values: {msg:?}"
            );
            assert!(msg.contains("cooperator"), "names the in-world presence in-register: {msg:?}");
        }
        // Arrival, departure, and return must each read differently — the recovery toast in
        // particular must not duplicate the first-arrival one.
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "the three presence lines must be distinct");
    }

    #[test]
    fn matching_password_authenticates_both_peers() {
        // The happy path: a shared password makes each side's Auth proof verify, so both link.
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair(v, v); // both share PW
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        assert!(host.peer().is_linked(CLIENT), "host authenticated the client");
        assert!(client.peer().is_linked(HOST), "client authenticated the host");
        // No auth banner on either side.
        assert!(!client.peer().notifications().banners().iter().any(|b| b.message.contains("Authentication failed")));
        assert!(!host.peer().notifications().banners().iter().any(|b| b.message.contains("Authentication failed")));
    }

    #[test]
    fn mismatched_password_is_rejected_and_never_links_or_applies_config() {
        // A peer that found the lobby but has the WRONG password must never be treated as linked, and
        // a ConfigSync it (or a real host) sends must never apply to the other side.
        let v = Version::new(0, 1, 0);
        let (mut host, mut client) = pair_over_with_pw(
            Loopback::mesh(&[HOST, CLIENT]),
            v,
            v,
            "host-password",
            "different-password",
        );
        // Host has a non-default shared setting it would push *if* the client authenticated.
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().mark_config_changed();
        // Drive maintain rounds (no faults): the host re-asserts its ConfigSync onto the wire every
        // tick regardless of links, so the client's *linked-gate* — not merely the absence of a sync —
        // is what must reject it. 25 < LIVENESS_TIMEOUT_TICKS, so no liveness banner confounds this.
        run_lossy(&mut host, &mut client, 25, |_| false);

        assert!(!client.peer().is_linked(HOST), "wrong password must not authenticate the host");
        assert!(!host.peer().is_linked(CLIENT), "wrong password must not authenticate the client");
        // The host's settings never reach the unauthenticated client.
        assert_eq!(
            client.peer().config().scaling.boss_health,
            Config::default().scaling.boss_health,
            "no ConfigSync is applied across a failed handshake"
        );
        // Plain/diagnostic banner on the failure, mirroring the version-mismatch path.
        let banners = client.peer().notifications().banners();
        assert!(
            banners.iter().any(|b| b.message.contains("Authentication failed")),
            "an auth-failure banner should be raised: {banners:?}"
        );
    }

    #[test]
    fn reflection_attack_does_not_link_a_passwordless_peer() {
        // An attacker with NO password mirrors the victim's advertised nonce, then reflects the
        // victim's own outgoing Auth proof straight back, hoping it verifies as inbound. Identity
        // binding in the proof must defeat this — the attacker never knows the password yet must not
        // become linked. (Without the id pair this test fails: the reflected proof would verify.)
        let v = Version::new(0, 1, 0);
        const ATTACKER: PeerId = 99;
        let mut victim = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        // The attacker advertises a Hello whose nonce equals the victim's own nonce.
        let reply =
            victim.handle(ATTACKER, ModMessage::Hello { mod_version: v.to_u32(), nonce: HOST_NONCE });
        // Grab the proof the victim handed out (its Auth reply addressed to the attacker).
        let leaked = match reply.as_slice() {
            [ModMessage::Auth { to, proof }] if *to == ATTACKER => *proof,
            other => panic!("expected an Auth reply to the attacker, got {other:?}"),
        };
        // The attacker reflects it back unchanged.
        victim.handle(ATTACKER, ModMessage::Auth { to: HOST, proof: leaked });
        assert!(!victim.is_linked(ATTACKER), "a reflected proof must never authenticate a peer");
    }

    #[test]
    fn config_sync_from_unauthenticated_host_is_dropped() {
        // The gate in isolation: an un-linked client must ignore a ConfigSync even from its host_id.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE);
        let mut s = SharedSettings::from(&Config::default());
        s.scaling.boss_health = 250;
        client.handle(HOST, ModMessage::ConfigSync { generation: 5, settings: s });
        assert_eq!(
            client.config().scaling.boss_health,
            Config::default().scaling.boss_health,
            "ConfigSync before authentication must not apply"
        );
        // And it does NOT banner (transient race, self-heals once linked).
        assert!(client.notifications().banners().is_empty());
    }

    #[test]
    fn session_action_from_unauthenticated_peer_is_dropped() {
        // A stranger who discovered the lobby can't drive the session before authenticating.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld });
        assert_eq!(host.last_action(), None, "unauthenticated action ignored");
        // The seq gate is untouched, so a later *authenticated* action at the same seq still applies.
        let mut host = linked_host(v);
        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld });
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::JoinWorld)));
    }

    #[test]
    fn auth_addressed_to_another_peer_is_ignored() {
        // A broadcast Auth carries `to`; a peer it isn't addressed to must drop it (it's keyed to the
        // addressee's id+nonce, so verifying it here would spuriously fail and banner). No link, no
        // banner. This pins the `to != self.id` routing gate.
        let v = Version::new(0, 1, 0);
        const OTHER: PeerId = 3;
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        // A well-formed proof, but addressed to OTHER rather than to us.
        let proof = crate::crypto::auth_proof(OTHER, CLIENT, &[0u8; AUTH_NONCE_LEN], &CLIENT_NONCE, PW);
        let out = host.handle(CLIENT, ModMessage::Auth { to: OTHER, proof });
        assert!(out.is_empty());
        assert!(!host.is_linked(CLIENT), "a proof addressed to another peer must not link us");
        assert!(host.notifications().banners().is_empty(), "and must not banner");
    }

    #[test]
    fn forwarded_log_from_unauthenticated_peer_is_dropped() {
        // A stranger must not be able to inject lines into the host's shareable diagnostic bundle.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(
            CLIENT,
            ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: "spam".into() }),
        );
        assert_eq!(host.log_bundle().len(), 0, "an unauthenticated peer's logs are not aggregated");
    }

    #[test]
    fn auth_before_hello_is_dropped_then_heals() {
        // An Auth that races ahead of the peer's Hello (so we don't yet know its nonce) must drop
        // quietly — no link, no auth-failure banner — and then verify once Hello + a re-sent Auth
        // arrive. This pins the missing-nonce early-out and its self-heal.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(!host.is_linked(CLIENT), "can't verify before the peer's Hello/nonce arrives");
        assert!(host.notifications().banners().is_empty(), "drops quietly, no premature auth banner");

        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(host.is_linked(CLIENT), "handshake heals once Hello + Auth both arrive");
    }

    #[test]
    fn a_captured_proof_does_not_replay_against_a_fresh_session_nonce() {
        // Replay resistance: a proof captured from a past session (bound to that session's verifier
        // nonce) must not link when replayed at a peer that has since drawn a fresh nonce.
        let v = Version::new(0, 1, 0);
        let stale_host_nonce = [0xAB; AUTH_NONCE_LEN]; // the host's nonce in the *previous* session
        let captured = crate::crypto::auth_proof(HOST, CLIENT, &stale_host_nonce, &CLIENT_NONCE, PW);
        // New session: the host's nonce is different (HOST_NONCE != stale_host_nonce).
        assert_ne!(HOST_NONCE, stale_host_nonce, "test premise: the session nonce rotated");
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: captured });
        assert!(!host.is_linked(CLIENT), "a proof bound to a previous session's nonce must not link");
    }

    #[test]
    fn empty_passwords_link_only_via_the_matching_secret() {
        // Documents the core-level boundary: core imposes no password-length floor, so two empty
        // password peers DO link (matching secret). Production prevents this via the startup guard
        // (`Config::password_is_valid`, enforced in the cdylib), which is therefore load-bearing.
        let v = Version::new(0, 1, 0);
        let mut a = Peer::new(HOST, HOST, v, Config::default(), HOST_NONCE); // empty password
        a.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, "");
        a.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(a.is_linked(CLIENT), "matching (empty) secret links — guarded only at startup");
    }

    #[test]
    fn a_stranger_in_the_mesh_cannot_link_or_disrupt_the_authenticated_pair() {
        // The realistic adversarial topology: a third peer with the wrong password shares the mesh.
        // The honest pair must still authenticate each other and converge on config, and the stranger
        // must never link nor receive the host's settings.
        let v = Version::new(0, 1, 0);
        const STRANGER: PeerId = 3;
        let mut ends = Loopback::mesh(&[HOST, CLIENT, STRANGER]).into_iter();
        let mut host =
            Session::new(Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE), ends.next().unwrap());
        let mut client = Session::new(
            Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE),
            ends.next().unwrap(),
        );
        let mut stranger = Session::new(
            Peer::new(STRANGER, HOST, v, config_with_pw("wrong-password"), [0x33; AUTH_NONCE_LEN]),
            ends.next().unwrap(),
        );
        host.peer_mut().config_mut().scaling.boss_health = 250;
        host.peer_mut().mark_config_changed();
        host.connect();
        client.connect();
        stranger.connect();
        run(&mut [&mut host, &mut client, &mut stranger]);

        assert!(host.peer().is_linked(CLIENT), "honest pair authenticates");
        assert!(client.peer().is_linked(HOST));
        assert!(!host.peer().is_linked(STRANGER), "stranger with wrong password never links to host");
        assert!(!client.peer().is_linked(STRANGER));
        assert!(!stranger.peer().is_linked(HOST), "stranger can't authenticate the host either");
        assert_eq!(client.peer().config().scaling.boss_health, 250, "honest pair still converges");
        assert_ne!(
            stranger.peer().config().scaling.boss_health,
            250,
            "the stranger never receives the host's settings"
        );
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
        let mut client = linked_client(v); // HOST authenticated; ConfigSync from it now applies
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
        let mut client = linked_client(v); // HOST authenticated; ConfigSync from it now applies
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
        let mut host = linked_host(v); // CLIENT authenticated; its actions are now accepted
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
        let mut host = linked_host(v); // CLIENT authenticated; its logs are now aggregated
        let rec = LogRecord { seq: 4, level: LogLevel::Info, message: "once".into() };
        host.handle(CLIENT, ModMessage::Log(rec.clone()));
        host.handle(CLIENT, ModMessage::Log(rec)); // duplicate
        assert_eq!(host.log_bundle().len(), 1, "same seq from same peer counted once");
    }

    #[test]
    fn liveness_flags_a_silent_peer_then_clears_on_return() {
        let v = Version::new(0, 1, 0);
        // CLIENT must be linked for the "Lost contact" banner to fire (we don't banner about
        // unauthenticated strangers). linked_host feeds CLIENT's Hello+Auth, so it's seen at tick 0.
        let mut host = linked_host(v);

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
        let mut client = Peer::new(CLIENT, HOST, v, Config::default(), CLIENT_NONCE);
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
        let mut host = Peer::new(HOST, HOST, v, Config::default(), HOST_NONCE);
        let out = host.handle(HOST, ModMessage::Hello { mod_version: v.to_u32(), nonce: HOST_NONCE });
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
        let mut host = Session::new(Peer::new(HOST, HOST, v, Config::default(), HOST_NONCE), host_end);

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
