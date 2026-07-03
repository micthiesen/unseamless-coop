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
/// Cap on distinct senders a [`Peer`] tracks state for. Every frame's sender lands in `last_seen`
/// (and a `Hello`'s in `peers`/`peer_nonces`) *before* authentication — the handshake needs the
/// nonce to verify the proof — so without a cap, a flood of distinct transport identities grows
/// those maps without bound. A sender already tracked (which includes every linked peer) always
/// passes; a new one is admitted only while there's room, and [`Peer::evict`] frees its slot on a
/// real leave.
///
/// Sized far above any legitimate party ([`crate::config::MAX_SESSION_PLAYERS`] is 6) so the cap
/// only bites under a flood of *distinct* sender ids — which, over Steam P2P, means distinct
/// transport-authenticated Steam accounts (sender ids come from the transport, not the wire, so
/// they can't be forged cheaply). Slot exhaustion by dozens of real accounts is accepted residual
/// risk; today's production transports additionally filter to the one configured partner before a
/// frame ever reaches `Peer`, so this is the core-level backstop for future multi-peer bindings.
///
/// Caveat: slot *reclamation* currently depends entirely on [`Peer::evict`], which the binding
/// layer doesn't call yet (rung-3 TODO on that method) — a transient flood therefore pins the
/// roster at the cap until eviction is wired. When multi-peer arrives, consider also reaping
/// long-stale **unlinked** senders in `sweep_liveness` so the cap acts as a rate bound rather
/// than a permanent high-water mark.
const MAX_TRACKED_PEERS: usize = 64;

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

/// Banner id + plain-voice message for the roster cap ([`MAX_TRACKED_PEERS`]) turning new senders
/// away. One global banner (not per-peer): the condition is "the roster is full", and a keyed
/// banner can't be spammed by the strangers who trip it — re-raising the same key just replaces it.
const ROSTER_FULL_BANNER_KEY: &str = "roster:full";
const ROSTER_FULL_MESSAGE: &str =
    "Too many peers this session; ignoring newcomers until one leaves";

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

    /// Drop the per-sender high-water mark for `peer`. A peer evicted and re-joining restarts its
    /// outbound seq from 1, so a retained mark would reject its fresh frames as stale — forgetting
    /// it puts the sender back at the `0` floor. Returns whether a mark existed.
    ///
    /// The trade: a late in-flight duplicate from the *departed* session can pass the reset gate
    /// (re-applying an already-applied frame) until the rejoiner's new stream climbs past its seq.
    /// Telling the two streams apart would need a per-peer session epoch — a per-sender analogue of
    /// the host-session epoch `ConfigSync` now carries (`config_epoch`); acceptable to defer here
    /// because eviction fires on a real session-leave, where the transport has stopped carrying the
    /// old session's frames.
    fn forget(&mut self, peer: PeerId) -> bool {
        self.seen.remove(&peer).is_some()
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
    ///
    /// Bounding invariant: this map — like `peer_nonces`, `linked`, `stale_peers`, and both
    /// `SeqGate`s — is inserted into only from [`Peer::handle`] arms that run *after* the roster
    /// gate has admitted the sender into `last_seen`, so `last_seen`'s [`MAX_TRACKED_PEERS`] cap
    /// transitively bounds them all. An insert reachable outside `handle` would silently break
    /// that bound; route new per-sender state through `handle` (or gate it the same way).
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
    /// Steam id in production) and is **never pruned by the frame/maintenance paths**: re-linking a
    /// peer after a transient liveness blip is a no-op, which is what makes the handshake self-heal.
    /// The pruning mechanism for a real session-*leave* is [`Peer::evict`]; *deciding* when a peer
    /// has left is deferred to the binding layer (Layer 2), which owns the game's session FSM.
    linked: BTreeSet<PeerId>,
    notifications: Notifications,
    /// Host-side aggregation of forwarded debug logs.
    log_bundle: LogBundle,

    // --- outbound identity (so receivers can dedup/order our frames) ---
    /// Host's authoritative config generation; bumped whenever the shared settings change.
    config_generation: u32,
    /// Host's session epoch, carried on every outbound `ConfigSync` beside `generation` so a client
    /// can tell a NEW host session (whose generation restarts at 1) from a stale/reordered frame
    /// within the current one. Defaults to 0; a hosting binding layer stamps a fresh random value
    /// once per session via [`Peer::set_config_epoch`] (like the auth nonce in [`Peer::new`], core
    /// has no entropy source). Unused on a non-host (`broadcast_config` is empty for it).
    config_epoch: u32,
    /// Monotonic sequence for our own outbound session actions.
    out_action_seq: u32,
    /// Monotonic sequence for our own outbound log records.
    out_log_seq: u32,
    /// Our heartbeat counter, advertised in `Ping`.
    ping_frame: u64,

    // --- inbound high-water marks (drop stale/duplicate frames) ---
    /// Host-session epoch of the last applied `ConfigSync` (`None` until the first sync). A sync
    /// whose epoch **differs** is a new session of the host (it restarted and its generation
    /// counter reset): it is adopted unconditionally and `applied_config_gen` is reset to its
    /// generation, so a fresh host's gen-1 sync can't be stalled by the previous session's
    /// high-water mark. This covers only a *restart* of the same host: a ConfigSync is applied
    /// solely from the fixed `host_id`, so a host *migration* (a different peer taking over) is a
    /// separate, still-unhandled Layer-2 concern.
    applied_config_epoch: Option<u32>,
    /// Highest config generation we've applied from the host *within* `applied_config_epoch`
    /// (`None` until the first sync). Generation is compared only against same-epoch syncs, so it
    /// only needs to be monotonic within one host session — which `mark_config_changed` guarantees
    /// short of 2^32 changes in a single session (see the wrap test).
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

    // --- roster bound ---
    /// Frames dropped because their (new, untracked) sender arrived with the roster at
    /// [`MAX_TRACKED_PEERS`] (for diagnostics, like `dropped_logs`).
    roster_overflow_drops: u64,

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
            config_epoch: 0,
            out_action_seq: 0,
            out_log_seq: 0,
            ping_frame: 0,
            applied_config_epoch: None,
            applied_config_gen: None,
            action_gate: SeqGate::default(),
            log_gate: SeqGate::default(),
            local_tick: 0,
            last_seen: BTreeMap::new(),
            stale_peers: BTreeSet::new(),
            log_limiter: RateLimiter::new(LOG_FORWARD_BURST),
            dropped_logs: 0,
            roster_overflow_drops: 0,
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
        // Bound the per-sender state an untrusted sender can create (see [`MAX_TRACKED_PEERS`]):
        // a NEW sender is admitted only while there's room; anyone already tracked — every linked
        // peer included — always passes. Dropped quietly per frame (a stranger flood must not spam
        // toasts), with one keyed banner for the condition plus a counter for diagnostics.
        if !self.last_seen.contains_key(&from) && self.last_seen.len() >= MAX_TRACKED_PEERS {
            self.roster_overflow_drops = self.roster_overflow_drops.wrapping_add(1);
            // Re-set idempotently on every drop (a same-id set_banner is an in-place update, so
            // this can't churn or evict other banners) rather than caching "is it up" in a bool —
            // the notifications model may itself evict a session banner under its own cap, and a
            // cached flag would then never re-raise it.
            self.notifications.set_banner(
                ROSTER_FULL_BANNER_KEY,
                Severity::Warning,
                ROSTER_FULL_MESSAGE,
            );
            return vec![];
        }
        // Any frame is evidence the sender is alive — even one the body then discards (a duplicate
        // ConfigSync, a gate-rejected action). This write must stay UNCONDITIONAL past the roster
        // gate: moving it inside the match to "skip rejected frames" would worsen liveness
        // false-positives under loss.
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
            ModMessage::ConfigSync { epoch, generation, settings } => {
                if !self.is_linked(from) {
                    // Unauthenticated sender — a stranger, or the host before its proof verifies. Drop
                    // quietly: no warn toast (a stranger could otherwise spam toasts and evict
                    // legitimate ones), and the host's `maintain` re-assert re-delivers the sync once
                    // the handshake completes. This check is first so an unlinked peer never reaches
                    // the non-host warn below.
                } else if from != self.host_id {
                    self.notifications
                        .warn(format!("Ignored ConfigSync from non-host {}", peer_tag(from)));
                } else if self.applied_config_epoch != Some(epoch)
                    || generation > self.applied_config_gen.unwrap_or(0)
                {
                    // A differing epoch is a NEW session of the host (it restarted) — adopt it and
                    // reset the generation high-water mark, so the fresh host's restarted counter
                    // isn't stalled by the old session's. Within the same epoch only a strictly
                    // newer generation applies; a re-asserted or reordered older one falls through
                    // to the else and is ignored (idempotent + ordered). A late frame from a
                    // *previous* epoch also "differs" and can transiently apply, but the live
                    // host's periodic `maintain` re-assert supersedes it again (its epoch differs
                    // in turn), so the party converges on the live host.
                    self.applied_config_epoch = Some(epoch);
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
                epoch: self.config_epoch,
                generation: self.config_generation,
                settings: SharedSettings::from(&self.config),
            }]
        } else {
            vec![]
        }
    }

    /// Host: stamp this session's config epoch, carried on every outbound `ConfigSync`. The binding
    /// layer supplies a **fresh random** value once per hosted session (core has no entropy source —
    /// the same split as the auth nonce in [`Peer::new`]), so a client lingering from a previous
    /// host session sees the epoch change and adopts the new host's settings instead of stalling on
    /// the old generation high-water mark. Freshness only has to beat prior sessions' epochs, so a
    /// random u32 is plenty; the default (0) is fine for a peer that never hosts. Note the epoch is
    /// an unordered *discriminator* — random values can't rank sessions, so a receiver can only test
    /// "differs", which is why a late frame from a dead session can transiently re-apply until the
    /// live host's next re-assert (see the `ConfigSync` handle arm). An *ordered* epoch (a persisted
    /// counter or clock) would close that residual, at the cost of binding-layer state.
    pub fn set_config_epoch(&mut self, epoch: u32) {
        self.config_epoch = epoch;
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

    /// Fully forget a departed peer, so the SAME peer re-appearing (a fresh `Hello` + `Auth`) can
    /// re-link cleanly from scratch. Removes it from the roster (`peers`), the nonce table
    /// (`peer_nonces`), the link set, liveness (`last_seen`/`stale_peers`), and both per-sender
    /// dedup gates. Dropping the nonce means the departed session's `Auth` proof can no longer
    /// re-link the peer (a rejoin must present a fresh `Hello` and a proof bound to its new nonce —
    /// a retained nonce would let a replayed stale proof re-link without one), and forgetting the
    /// gates means the rejoiner's outbound seqs, restarted at 1, aren't rejected as stale by the
    /// old high-water marks. Its per-peer banners (auth/version/liveness) are torn down too: none is
    /// actionable once the peer is gone. Returns whether the peer was known (present in any of
    /// those collections).
    ///
    /// This is the core half of session-leave eviction; nothing calls it yet. The binding layer
    /// owns the game's session FSM (the same Layer-2 boundary the `linked` note defers to), so it
    /// decides *when* a peer has truly left.
    // TODO(rung-3): call this from the binding layer's session-leave handler when the game's
    // session roster shrinks (no live session exists yet).
    pub fn evict(&mut self, peer: PeerId) -> bool {
        let mut known = false;
        known |= self.peers.remove(&peer).is_some();
        known |= self.peer_nonces.remove(&peer).is_some();
        known |= self.linked.remove(&peer);
        known |= self.last_seen.remove(&peer).is_some();
        known |= self.stale_peers.remove(&peer);
        known |= self.action_gate.forget(peer);
        known |= self.log_gate.forget(peer);
        self.notifications.clear_banner(&auth_banner_key(peer));
        self.notifications.clear_banner(&version_banner_key(peer));
        self.notifications.clear_banner(&liveness_banner_key(peer));
        // An eviction frees a roster slot, so the roster-full condition (if raised) has cleared and
        // the next newcomer will be admitted again. clear_banner is a no-op if it wasn't up.
        if self.last_seen.len() < MAX_TRACKED_PEERS {
            self.notifications.clear_banner(ROSTER_FULL_BANNER_KEY);
        }
        known
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
    /// Inbound frames dropped because their new sender arrived with the roster already at
    /// [`MAX_TRACKED_PEERS`] (for diagnostics, like [`dropped_logs`](Peer::dropped_logs)).
    pub fn roster_overflow_drops(&self) -> u64 {
        self.roster_overflow_drops
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
    /// A second client, for multi-peer interleave tests (per-sender gate independence, multi-peer
    /// liveness). Distinct id + nonce so its frames don't alias `CLIENT`'s.
    const CLIENT2: PeerId = 4;
    /// Distinct per-session nonces so the two peers' proofs differ on the wire.
    const HOST_NONCE: AuthNonce = [0x11; AUTH_NONCE_LEN];
    const CLIENT_NONCE: AuthNonce = [0x22; AUTH_NONCE_LEN];
    const CLIENT2_NONCE: AuthNonce = [0x44; AUTH_NONCE_LEN];
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

    /// Link an arbitrary client (`cid`/`nonce`) into an existing host [`Peer`] by feeding its
    /// `Hello` + a valid `Auth` proof — the multi-client analogue of [`linked_host`], so a test can
    /// stand up a host with several authenticated clients to exercise per-sender behavior.
    fn link_client_into(host: &mut Peer, cid: PeerId, nonce: AuthNonce, v: Version) {
        host.handle(cid, ModMessage::Hello { mod_version: v.to_u32(), nonce });
        // `cid` is the prover, HOST the verifier — same ordering as `linked_host`.
        let proof = crate::crypto::auth_proof(HOST, cid, &HOST_NONCE, &nonce, PW);
        host.handle(cid, ModMessage::Auth { to: HOST, proof });
        assert!(host.is_linked(cid), "test setup: client {cid} should be linked");
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
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 5, settings: s});
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
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 5, settings: first});
        assert_eq!(client.config().scaling.boss_health, 175);

        let mut spoof = SharedSettings::from(&Config::default());
        spoof.scaling.boss_health = 999;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 5, settings: spoof});
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

        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 5, settings: newer});
        assert_eq!(client.config().scaling.boss_health, 300);
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 4, settings: older });
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

    // --- systematic, deterministic fault-model sweep ---------------------------------------------
    //
    // The two tests above pin a couple of hand-picked fault points. These sweep the *cartesian
    // product* of drop / duplicate / reorder (the whole `FaultModel`) across several fixed seeds, so
    // a regression in the self-heal shows up as a specific reproducible `drop/dup/reorder/seed` case
    // rather than a lucky-or-unlucky single configuration. Determinism comes from the transport's
    // seeded PRNG; no proptest or other dep (CLAUDE.md: hand-rolled deterministic cases preferred).

    /// A shared-settings profile that differs from `Config::default()` in **every** host-enforced
    /// field (all six scaling percents off their defaults, all four shared bools flipped off their
    /// `true` default, a non-default player cap). All values sit inside their clamp ranges, so the
    /// decoder leaves them untouched and convergence is exact equality — meaning a single dropped or
    /// mis-mapped field would fail the sweep, not just the one field the older tests check.
    fn distinct_shared_profile(c: &mut Config) {
        c.scaling.enemy_health = 80;
        c.scaling.enemy_damage = 25;
        c.scaling.enemy_posture = 40;
        c.scaling.boss_health = 250;
        c.scaling.boss_damage = 30;
        c.scaling.boss_posture = 45;
        c.gameplay.crit_coop = false;
        c.gameplay.death_debuffs = false;
        c.gameplay.allow_summons = false;
        c.gameplay.roam_anywhere = false;
        c.session.max_players = 4;
    }

    /// Drive a fresh host+client pair over a faulty channel until the client's shared subset equals
    /// the host's, or the `budget` of maintenance rounds is spent. Returns the round it converged on
    /// (`None` if it never did). Deliberately **no `connect()`**: convergence is driven solely by
    /// `maintain()`'s periodic Hello + ConfigSync re-assert, so a lucky surviving handshake reply
    /// can't mask a broken re-assert. Exercises the full handshake → link → config-apply path under
    /// loss, since the host only links (and thus syncs) the client after its Auth proof survives.
    fn rounds_to_converge(faults: FaultModel, seed: u64, budget: usize) -> Option<usize> {
        let v = Version::new(0, 1, 0);
        let ends = Loopback::mesh_with_faults(&[HOST, CLIENT], faults, seed);
        let (mut host, mut client) = pair_over(ends, v, v);
        distinct_shared_profile(host.peer_mut().config_mut());
        host.peer_mut().mark_config_changed(); // bump generation; maintain() re-asserts it
        let target = SharedSettings::from(host.peer().config());
        for round in 1..=budget {
            host.maintain();
            client.maintain();
            host.pump();
            client.pump();
            if SharedSettings::from(client.peer().config()) == target {
                return Some(round);
            }
        }
        None
    }

    #[test]
    fn config_converges_across_the_fault_grid() {
        // Every (drop, duplicate, reorder, seed) combination must converge. Round budgets scale with
        // the drop rate: at a given loss the convergence chain is a Bernoulli pipeline (handshake link
        // ~drop^2 per round, then the config delivery ~drop per round), so heavier loss needs a larger
        // bound. The budgets are generous multiples of the observed convergence so the test asserts
        // "converges", not a brittle exact count, while still being a real gate.
        let dups = [0.0, 0.6];
        let reorders = [false, true];
        let seeds = [0x1111_1111u64, 0x2222_2222, 0xDEAD_BEEF];
        for (drop, budget) in [(0.0, 30usize), (0.5, 800), (0.85, 8000)] {
            for &dup in &dups {
                for &reorder in &reorders {
                    for &seed in &seeds {
                        let faults = FaultModel { drop_rate: drop, duplicate_rate: dup, reorder };
                        assert!(
                            rounds_to_converge(faults, seed, budget).is_some(),
                            "no convergence within {budget} rounds: \
                             drop={drop} dup={dup} reorder={reorder} seed={seed:#x}",
                        );
                    }
                }
            }
        }
    }

    /// Build a host+client pair that is **already linked** (handshake done off-transport), then wrap
    /// each linked [`Peer`] in a [`Session`] over `ends`. The link set is never pruned, so this lets a
    /// test isolate the periodic ConfigSync re-assert from the handshake's own multi-delivery
    /// probability — which is what dominates the round count at extreme loss.
    fn linked_session_pair(ends: Vec<Loopback>, v: Version) -> (Session<Loopback>, Session<Loopback>) {
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        let mut client = Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE);
        // Exchange Hellos (records each other's nonce) then Auth proofs (links), with no transport in
        // the loop — the same direct-handle pattern as `linked_host`/`linked_client`.
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        client.handle(HOST, ModMessage::Hello { mod_version: v.to_u32(), nonce: HOST_NONCE });
        let client_proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: client_proof });
        let host_proof = crate::crypto::auth_proof(CLIENT, HOST, &CLIENT_NONCE, &HOST_NONCE, PW);
        client.handle(HOST, ModMessage::Auth { to: CLIENT, proof: host_proof });
        assert!(host.is_linked(CLIENT) && client.is_linked(HOST), "test setup: pair must be linked");
        let mut it = ends.into_iter();
        (Session::new(host, it.next().unwrap()), Session::new(client, it.next().unwrap()))
    }

    #[test]
    fn config_reassert_self_heals_under_extreme_loss() {
        // With the handshake already established (the link is permanent), the host's periodic
        // ConfigSync re-assert is the *sole* convergence path — exactly the maintenance-tick mechanism
        // the live stack relies on. Isolating it lets us push loss to the extreme (95–99%), where the
        // handshake's own ~drop^2-per-round link probability would otherwise dominate, and still assert
        // the host's settings reach the client: one surviving delivery per round suffices, so even at
        // 99% loss convergence is a bounded geometric wait (~1/0.01 ≈ 100 rounds expected). Duplication
        // is on throughout, so the generation guard is also exercised under the same loss.
        let v = Version::new(0, 1, 0);
        let seeds = [0xABCD_1234u64, 0x5555_AAAA];
        for drop in [0.95, 0.99] {
            for reorder in [false, true] {
                for &seed in &seeds {
                    let faults = FaultModel { drop_rate: drop, duplicate_rate: 0.5, reorder };
                    let ends = Loopback::mesh_with_faults(&[HOST, CLIENT], faults, seed);
                    let (mut host, mut client) = linked_session_pair(ends, v);
                    distinct_shared_profile(host.peer_mut().config_mut());
                    host.peer_mut().mark_config_changed();
                    let target = SharedSettings::from(host.peer().config());

                    const BUDGET: usize = 5000; // ~50x the expected wait at 99% loss
                    let mut converged = None;
                    for round in 1..=BUDGET {
                        host.maintain();
                        client.maintain();
                        host.pump();
                        client.pump();
                        if SharedSettings::from(client.peer().config()) == target {
                            converged = Some(round);
                            break;
                        }
                    }
                    assert!(
                        converged.is_some(),
                        "config re-assert never healed within {BUDGET} rounds: \
                         drop={drop} reorder={reorder} seed={seed:#x}",
                    );
                    // The link survived the whole lossy run (it is never pruned), which is *why* the
                    // re-assert can apply — pin that so a future "evict on silence" change is caught.
                    assert!(client.peer().is_linked(HOST), "link must persist through the loss");
                }
            }
        }
    }

    // --- SeqGate: pathological orderings, duplicates, multi-peer interleave ----------------------

    #[test]
    fn action_gate_keeps_only_advancing_seqs_under_extreme_reorder() {
        // The gate accepts a frame only when its seq advances past the per-sender high-water mark, so
        // a pathologically reordered + duplicated stream collapses to the monotonic subset. (This is
        // the deliberate exactly-once-and-ordered semantics: a reordered-OLD action is treated as
        // stale and dropped, never replayed — see `SeqGate`.)
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        let stream = [
            (5u32, SessionAction::JoinWorld), // first real seq -> applied (passes the 0 floor)
            (3, SessionAction::OpenWorld),    // reordered-old  -> dropped
            (5, SessionAction::OpenWorld),    // duplicate      -> dropped
            (8, SessionAction::OpenWorld),    // advances       -> applied
            (1, SessionAction::JoinWorld),    // ancient        -> dropped
            (8, SessionAction::JoinWorld),    // duplicate      -> dropped
            (2, SessionAction::JoinWorld),    // ancient        -> dropped
        ];
        for (seq, action) in stream {
            host.handle(CLIENT, ModMessage::SessionAction { seq, action });
        }
        assert_eq!(
            host.last_action(),
            Some((CLIENT, SessionAction::OpenWorld)),
            "the high-water seq (8, OpenWorld) is the last accepted action"
        );
        // A genuinely newer seq after all the churn still applies.
        host.handle(CLIENT, ModMessage::SessionAction { seq: 9, action: SessionAction::JoinWorld });
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::JoinWorld)), "seq 9 advances");
    }

    #[test]
    fn action_gate_collapses_a_large_duplicate_burst_to_one_apply() {
        // A flood of the exact same frame (a duplicating channel gone wild) applies once; a later
        // stale-seq frame is still rejected after the burst.
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        let frame = ModMessage::SessionAction { seq: 7, action: SessionAction::OpenWorld };
        for _ in 0..1000 {
            host.handle(CLIENT, frame.clone());
        }
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::OpenWorld)), "1000 dups -> one apply");
        host.handle(CLIENT, ModMessage::SessionAction { seq: 6, action: SessionAction::JoinWorld });
        assert_eq!(
            host.last_action(),
            Some((CLIENT, SessionAction::OpenWorld)),
            "a post-burst stale seq (6 < 7) is rejected"
        );
    }

    #[test]
    fn action_gate_dedups_each_sender_independently() {
        // Two linked clients with OVERLAPPING seq numbers: the gate is keyed per sender, so CLIENT's
        // seq-1 and CLIENT2's seq-1 are independent — neither shadows the other.
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        link_client_into(&mut host, CLIENT2, CLIENT2_NONCE, v);

        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld });
        // CLIENT2's seq-1 must still apply even though CLIENT already used seq-1.
        host.handle(CLIENT2, ModMessage::SessionAction { seq: 1, action: SessionAction::OpenWorld });
        assert_eq!(
            host.last_action(),
            Some((CLIENT2, SessionAction::OpenWorld)),
            "CLIENT2's seq-1 is not shadowed by CLIENT's seq-1"
        );
        // CLIENT's seq-1 duplicate is dropped; its seq-2 advances.
        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::LeaveWorld });
        assert_eq!(host.last_action(), Some((CLIENT2, SessionAction::OpenWorld)), "CLIENT dup dropped");
        host.handle(CLIENT, ModMessage::SessionAction { seq: 2, action: SessionAction::LeaveWorld });
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::LeaveWorld)), "CLIENT advances on its own");
        // CLIENT2's stale seq-1 redelivery is dropped.
        host.handle(CLIENT2, ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld });
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::LeaveWorld)), "CLIENT2 stale dup dropped");
    }

    #[test]
    fn log_gate_dedups_each_sender_independently_on_the_host() {
        // The host aggregates forwarded logs through a per-sender gate too: overlapping seqs from two
        // clients are independent, and a per-sender duplicate collapses.
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        link_client_into(&mut host, CLIENT2, CLIENT2_NONCE, v);

        host.handle(CLIENT, ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: "a1".into() }));
        host.handle(CLIENT2, ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: "b1".into() }));
        // CLIENT's seq-1 duplicate (different payload, as a stale dup looks) is dropped.
        host.handle(CLIENT, ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: "a1-dup".into() }));
        host.handle(CLIENT2, ModMessage::Log(LogRecord { seq: 2, level: LogLevel::Info, message: "b2".into() }));
        assert_eq!(host.log_bundle().len(), 3, "a1 + b1 + b2 aggregated; the duplicate a1 collapsed");
    }

    // --- handshake flows ------------------------------------------------------------------------

    #[test]
    fn version_banner_is_withheld_until_a_peer_authenticates() {
        // A peer with BOTH a wrong password and an incompatible major version: only the auth-failure
        // banner fires. The version banner is deferred to link time, so an unauthenticated stranger
        // can't plant a version banner on a real player's overlay.
        let v_us = Version::new(1, 0, 0);
        let v_them = Version::new(2, 0, 0);
        let mut host = Peer::new(HOST, HOST, v_us, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v_them.to_u32(), nonce: CLIENT_NONCE });
        let bad = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, "wrong-password");
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: bad });

        let banners = host.notifications().banners();
        assert!(banners.iter().any(|b| b.message.contains("Authentication failed")), "auth banner fires");
        assert!(
            !banners.iter().any(|b| b.message.contains("version mismatch")),
            "version banner is withheld for an unauthenticated peer"
        );
        assert!(!host.is_linked(CLIENT));
    }

    #[test]
    fn version_mismatch_banners_a_linked_peer_on_both_sides() {
        // The happy-auth-but-incompatible-version case end to end: a matching password links the pair,
        // and each side raises the version-mismatch banner about the other (it's symmetric).
        let (mut host, mut client) = pair(Version::new(1, 0, 0), Version::new(2, 5, 0));
        host.connect();
        client.connect();
        run(&mut [&mut host, &mut client]);
        assert!(host.peer().is_linked(CLIENT) && client.peer().is_linked(HOST), "still links across a major gap");
        assert!(host.peer().notifications().banners().iter().any(|b| b.message.contains("version mismatch")));
        assert!(client.peer().notifications().banners().iter().any(|b| b.message.contains("version mismatch")));
    }

    #[test]
    fn re_verifying_an_already_linked_peer_does_not_re_toast_or_re_broadcast() {
        // A re-asserted Auth (which `maintain`'s periodic Hello reply produces over a lossy channel)
        // for an already-linked peer is a no-op: it must not re-broadcast config nor surface a new
        // toast/banner. Pin all three (emitted frames, toast count, banner count stay put).
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        let toasts_before = host.notifications().toasts().len();
        let banners_before = host.notifications().banners().len();
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        let out = host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(out.is_empty(), "re-verifying an already-linked peer emits no frames (no config re-broadcast)");
        assert_eq!(host.notifications().toasts().len(), toasts_before, "no new toast on re-verify");
        assert_eq!(host.notifications().banners().len(), banners_before, "no new banner on re-verify");
    }

    #[test]
    fn simultaneous_handshake_heals_when_auth_races_ahead_of_hello_both_ways() {
        // The simultaneous-exchange race over a reordering channel: each side's `Auth` can arrive
        // before the peer's `Hello` (so the nonce isn't known yet). `verify_auth` must drop those
        // quietly (no link, no banner), then link once each `Hello` + a re-asserted `Auth` land — the
        // self-heal `maintain` drives. Exercise it directly on both peers.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        let mut client = Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE);
        let client_proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        let host_proof = crate::crypto::auth_proof(CLIENT, HOST, &CLIENT_NONCE, &HOST_NONCE, PW);

        // Both Auths arrive first (nonces unknown) — dropped quietly on each side.
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: client_proof });
        client.handle(HOST, ModMessage::Auth { to: CLIENT, proof: host_proof });
        assert!(!host.is_linked(CLIENT) && !client.is_linked(HOST), "no link before the nonce is known");
        assert!(host.notifications().banners().is_empty(), "no premature auth banner on the host");
        assert!(client.notifications().banners().is_empty(), "no premature auth banner on the client");

        // Then the Hellos, then the re-asserted Auths — the handshake heals on both sides.
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        client.handle(HOST, ModMessage::Hello { mod_version: v.to_u32(), nonce: HOST_NONCE });
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: client_proof });
        client.handle(HOST, ModMessage::Auth { to: CLIENT, proof: host_proof });
        assert!(host.is_linked(CLIENT), "host links once Hello + re-asserted Auth arrive");
        assert!(client.is_linked(HOST), "client links symmetrically");
    }

    // --- liveness: false-positives under loss, multi-peer ----------------------------------------

    #[test]
    fn liveness_tolerates_heavy_loss_without_false_flagging_a_live_peer() {
        // A live peer keeps maintaining; under heavy (but not total) loss enough heartbeats survive
        // within LIVENESS_TIMEOUT_TICKS that neither side ever flags the other as lost. At 60% drop,
        // each direction lands ≥1 of its per-tick frames with high probability, so a 30-tick silence
        // window is astronomically unlikely — this pins that the timeout is conservative enough not to
        // flap, the core liveness-false-positive guarantee on the live two-machine path.
        let v = Version::new(0, 1, 0);
        for &seed in &[0x1u64, 0xFEED, 0xBEEF, 0x1234_5678, 0xABCD_EF01] {
            let faults = FaultModel { drop_rate: 0.6, duplicate_rate: 0.2, reorder: true };
            let ends = Loopback::mesh_with_faults(&[HOST, CLIENT], faults, seed);
            let (mut host, mut client) = linked_session_pair(ends, v);
            for round in 1..=300 {
                host.maintain();
                client.maintain();
                host.pump();
                client.pump();
                assert!(
                    !host.peer().is_stale(CLIENT),
                    "host falsely flagged a live client at round {round} (seed {seed:#x})"
                );
                assert!(
                    !client.peer().is_stale(HOST),
                    "client falsely flagged a live host at round {round} (seed {seed:#x})"
                );
            }
        }
    }

    #[test]
    fn liveness_still_flags_a_peer_under_total_loss() {
        // The true-positive companion: with the channel fully dark, a still-maintaining peer correctly
        // goes stale once past the timeout (the heartbeat genuinely isn't getting through).
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { drop_rate: 1.0, ..Default::default() };
        let ends = Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 1);
        let (mut host, mut client) = linked_session_pair(ends, v);
        for _ in 0..=LIVENESS_TIMEOUT_TICKS {
            host.maintain();
            client.maintain();
            host.pump();
            client.pump();
        }
        assert!(host.peer().is_stale(CLIENT), "total loss: the silent client is flagged");
        assert!(client.peer().is_stale(HOST), "and the silent host is flagged");
    }

    #[test]
    fn liveness_flags_only_the_silent_peer_among_several() {
        // Two linked clients: one keeps pinging, the other falls silent. Only the silent one is
        // flagged, and exactly one "Lost contact" banner is raised.
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        link_client_into(&mut host, CLIENT2, CLIENT2_NONCE, v);
        for _ in 0..(LIVENESS_TIMEOUT_TICKS + 5) {
            host.handle(CLIENT, ModMessage::Ping { frame: 1 }); // CLIENT stays chatty
            host.maintain(); // CLIENT2 says nothing
        }
        assert!(!host.is_stale(CLIENT), "the chatty peer stays live");
        assert!(host.is_stale(CLIENT2), "the silent peer is flagged");
        let lost = host.notifications().banners().iter().filter(|b| b.message.contains("Lost contact")).count();
        assert_eq!(lost, 1, "exactly one lost-contact banner — for the silent peer only");
    }

    #[test]
    fn handshake_banners_are_torn_down_when_a_linked_peer_goes_silent() {
        // A linked but version-incompatible peer carries a version banner; if it then falls silent the
        // liveness sweep tears that now-unactionable banner down and replaces it with the lost-contact
        // banner (the peer WAS authenticated).
        let v_us = Version::new(1, 0, 0);
        let v_them = Version::new(2, 0, 0);
        let mut host = Peer::new(HOST, HOST, v_us, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v_them.to_u32(), nonce: CLIENT_NONCE });
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(host.is_linked(CLIENT));
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("version mismatch")));

        for _ in 0..(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain();
        }
        let banners = host.notifications().banners();
        assert!(!banners.iter().any(|b| b.message.contains("version mismatch")), "version banner torn down");
        assert!(banners.iter().any(|b| b.message.contains("Lost contact")), "linked peer's departure is bannered");
    }

    #[test]
    fn auth_failure_banner_clears_when_an_unlinked_stranger_departs() {
        // A wrong-password stranger gets an auth banner but no lost-contact banner (it never linked).
        // When it goes silent, the sweep clears the stale auth banner and still raises no lost-contact.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        let bad = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, "wrong-password");
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: bad });
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("Authentication failed")));

        for _ in 0..(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain();
        }
        let banners = host.notifications().banners();
        assert!(!banners.iter().any(|b| b.message.contains("Authentication failed")), "stale auth banner cleared");
        assert!(!banners.iter().any(|b| b.message.contains("Lost contact")), "no lost-contact for an unlinked stranger");
    }

    #[test]
    fn a_bare_ping_refreshes_liveness_but_not_the_roster_or_link() {
        // A Ping is liveness-only: it registers the sender for the liveness sweep but does NOT add a
        // version to the roster or link it. An unlinked stranger that pings once then vanishes goes
        // stale but is not bannered.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Ping { frame: 1 });
        assert!(host.known_peers().get(&CLIENT).is_none(), "a Ping alone adds no roster entry");
        assert!(!host.is_linked(CLIENT), "and does not link");
        for _ in 0..(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain();
        }
        assert!(host.is_stale(CLIENT), "tracked for liveness once heard...");
        assert!(host.notifications().banners().is_empty(), "...but an unlinked stranger's silence isn't bannered");
    }

    // --- config-sync generation edge cases ------------------------------------------------------

    #[test]
    fn config_sync_applies_each_monotonic_increment_then_ignores_redelivery() {
        // Rapid re-asserts at increasing generations each apply in order; a later redelivery of an
        // older generation (with a different payload) is ignored.
        let v = Version::new(0, 1, 0);
        let mut client = linked_client(v);
        for (generation, hp) in [(2u32, 120u32), (3, 175), (4, 250)] {
            let mut s = SharedSettings::from(&Config::default());
            s.scaling.boss_health = hp;
            client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation, settings: s });
            assert_eq!(client.config().scaling.boss_health, hp, "generation {generation} applied");
        }
        let mut stale = SharedSettings::from(&Config::default());
        stale.scaling.boss_health = 999;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 3, settings: stale });
        assert_eq!(client.config().scaling.boss_health, 250, "a redelivered older generation is ignored");
    }

    #[test]
    fn config_sync_from_a_linked_non_host_is_ignored_with_a_warning() {
        // The host receives a ConfigSync from a linked CLIENT (a non-host). Only the host is
        // authoritative, so it's ignored — and because the sender IS linked, a diagnostic warn fires
        // (distinct from the silent drop for an *un*linked sender, covered separately).
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        let before = host.config().scaling.boss_health;
        let mut s = SharedSettings::from(&Config::default());
        s.scaling.boss_health = before.wrapping_add(50);
        host.handle(CLIENT, ModMessage::ConfigSync { epoch: 1, generation: 9, settings: s });
        assert_eq!(host.config().scaling.boss_health, before, "a non-host ConfigSync never mutates config");
        assert!(
            host.notifications().toasts().iter().any(|t| t.message.contains("non-host")),
            "a linked non-host ConfigSync raises a diagnostic warn"
        );
    }

    #[test]
    fn config_generation_wrap_within_one_epoch_is_a_known_unhandled_boundary() {
        // Within one epoch the generation comparison is a plain `>`, which assumes a monotonic source.
        // If the host's u32 generation ever WRAPPED (2^32 changes in a single host session — not
        // reachable in practice), a post-wrap gen 0 compares below the applied high-water mark and
        // stalls until the next host session's fresh epoch. Pinned so the residual boundary is
        // explicit. The realistic version of this — a host *restart* resetting the counter — is
        // handled: the restarted host stamps a fresh epoch, which supersedes regardless of generation
        // (see `new_host_epoch_supersedes_the_old_generation_high_water`).
        let v = Version::new(0, 1, 0);
        let mut client = linked_client(v);
        let mut hi = SharedSettings::from(&Config::default());
        hi.scaling.boss_health = 250;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: u32::MAX, settings: hi });
        assert_eq!(client.config().scaling.boss_health, 250, "the high-water generation applies");
        let mut wrapped = SharedSettings::from(&Config::default());
        wrapped.scaling.boss_health = 120;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 0, settings: wrapped });
        assert_eq!(
            client.config().scaling.boss_health,
            250,
            "a same-epoch wrapped gen-0 sync is ignored — known limitation, not a silent rollback"
        );
    }

    #[test]
    fn new_host_epoch_supersedes_the_old_generation_high_water() {
        // The host-restart fix: a lingering client has applied a high generation from the previous
        // host session; the restarted host stamps a fresh epoch and its generation restarts at 1.
        // The differing epoch must adopt immediately (resetting the high-water mark) — before this
        // existed, the client ignored the new host until it out-counted the old session.
        let v = Version::new(0, 1, 0);
        let mut client = linked_client(v);
        let mut old = SharedSettings::from(&Config::default());
        old.scaling.boss_health = 250;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 1, generation: 40, settings: old });
        assert_eq!(client.config().scaling.boss_health, 250);

        let mut fresh = SharedSettings::from(&Config::default());
        fresh.scaling.boss_health = 120;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 2, generation: 1, settings: fresh });
        assert_eq!(
            client.config().scaling.boss_health,
            120,
            "a new epoch supersedes despite a lower generation"
        );

        // The high-water mark reset to the new epoch's generation, and ordering holds within it:
        // a same-generation redelivery (stale-duplicate shape) is a no-op…
        let mut dup = SharedSettings::from(&Config::default());
        dup.scaling.boss_health = 999;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 2, generation: 1, settings: dup });
        assert_eq!(
            client.config().scaling.boss_health,
            120,
            "same epoch + same generation must not re-apply"
        );
        // …and a strictly newer generation still applies.
        let mut newer = SharedSettings::from(&Config::default());
        newer.scaling.boss_health = 175;
        client.handle(HOST, ModMessage::ConfigSync { epoch: 2, generation: 2, settings: newer });
        assert_eq!(client.config().scaling.boss_health, 175, "same-epoch newer generation applies");
    }

    #[test]
    fn config_converges_across_a_host_restart_under_faults() {
        // The epoch fix, end to end under the fault model. Session one: the client converges on the
        // host's settings and its generation high-water mark climbs well past 1. Then the host
        // "restarts": a brand-new host `Peer` (same PeerId — the same player re-hosting), a fresh
        // epoch, and the generation counter back at its initial value. Without the epoch, every
        // post-restart sync (generation 1 vs the client's high-water mark) would be ignored forever —
        // `maintain` re-asserts the same generation, so the stall could never heal. With it, the
        // client adopts the differing epoch even while drop/duplicate/reorder keep stale epoch-1
        // frames circulating across the restart.
        let v = Version::new(0, 1, 0);
        let faults = FaultModel { drop_rate: 0.3, duplicate_rate: 0.4, reorder: true };
        let (mut host, mut client) =
            pair_over(Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 0xE90C_0FF5), v, v);
        host.peer_mut().set_config_epoch(1);
        host.peer_mut().config_mut().scaling.boss_health = 250;
        for _ in 0..40 {
            host.peer_mut().mark_config_changed(); // drive the generation well past the restart's 1
        }
        run_lossy(&mut host, &mut client, 500, |c| c.peer().config().scaling.boss_health == 250);
        assert_eq!(client.peer().config().scaling.boss_health, 250, "converged on session one");

        let mut restarted = config_with_pw(PW);
        restarted.scaling.boss_health = 120;
        *host.peer_mut() = Peer::new(HOST, HOST, v, restarted, HOST_NONCE);
        host.peer_mut().set_config_epoch(2); // the binding stamps a fresh epoch per hosted session
        run_lossy(&mut host, &mut client, 500, |c| c.peer().config().scaling.boss_health == 120);
        assert_eq!(
            client.peer().config().scaling.boss_health,
            120,
            "the restarted host's fresh epoch supersedes the lingering client's old high-water mark"
        );
    }

    #[test]
    fn broadcast_config_carries_the_stamped_epoch_across_generation_bumps() {
        // The host binding stamps the session epoch once (`set_config_epoch`); every subsequent
        // broadcast carries it unchanged while `mark_config_changed` bumps only the generation.
        fn sync_identity(msgs: &[ModMessage]) -> (u32, u32) {
            match msgs {
                [ModMessage::ConfigSync { epoch, generation, .. }] => (*epoch, *generation),
                other => panic!("expected exactly one ConfigSync, got {other:?}"),
            }
        }
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.set_config_epoch(0xABCD_1234);
        let (epoch, generation) = sync_identity(&host.broadcast_config());
        assert_eq!(epoch, 0xABCD_1234, "broadcast carries the stamped epoch");
        let (e2, g2) = sync_identity(&host.mark_config_changed());
        assert_eq!(e2, 0xABCD_1234, "the epoch is stable across generation bumps");
        assert_eq!(g2, generation + 1, "mark_config_changed bumps only the generation");
    }

    #[test]
    fn non_host_has_nothing_authoritative_to_assert() {
        // A client owns no generation: marking config changed and broadcasting config are both no-ops.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, config_with_pw(PW), CLIENT_NONCE);
        assert!(client.mark_config_changed().is_empty(), "a client has nothing authoritative to assert");
        assert!(client.broadcast_config().is_empty(), "and broadcasts no config");
    }

    // --- log-forward rate limiter: burst / refill / token counting ------------------------------

    #[test]
    fn forward_log_refill_saturates_at_capacity_across_many_idle_ticks() {
        // Idle ticks refill tokens but must SATURATE at the burst capacity, not accumulate unbounded —
        // otherwise a long-quiet client could later dump an arbitrarily large flood in one frame.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default(), CLIENT_NONCE);
        client.config_mut().debug.forward_to_host = true;
        let mut emitted = 0;
        while !client.forward_log(LogLevel::Trace, "x").is_empty() {
            emitted += 1;
        }
        assert_eq!(emitted, LOG_FORWARD_BURST, "drained exactly the initial burst");
        for _ in 0..100 {
            client.maintain();
        }
        let mut after = 0;
        while !client.forward_log(LogLevel::Trace, "x").is_empty() {
            after += 1;
        }
        assert_eq!(after, LOG_FORWARD_BURST, "100 ticks of refill cap at the burst capacity, not 100*refill");
    }

    #[test]
    fn forward_log_grants_exactly_refill_tokens_per_tick() {
        // Token counting across frame deltas: N maintenance ticks grant exactly N*refill takes (while
        // still under capacity), pinning the per-tick accrual amount.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default(), CLIENT_NONCE);
        client.config_mut().debug.forward_to_host = true;
        while !client.forward_log(LogLevel::Trace, "x").is_empty() {} // drain to empty
        client.maintain();
        client.maintain();
        let mut granted = 0;
        while !client.forward_log(LogLevel::Trace, "x").is_empty() {
            granted += 1;
        }
        assert_eq!(
            granted,
            (2.0 * LOG_FORWARD_REFILL_PER_TICK) as u32,
            "two ticks grant exactly two refills' worth of forwards"
        );
    }

    #[test]
    fn forward_log_is_a_noop_without_counting_drops_when_disabled_or_on_host() {
        // A "drop" means *throttled* — disabled forwarding and the host's own logs are silent no-ops,
        // not drops, so they must not inflate the dropped-logs diagnostic.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default(), CLIENT_NONCE);
        assert!(!client.config().debug.forward_to_host, "default is off");
        for _ in 0..10 {
            assert!(client.forward_log(LogLevel::Warn, "x").is_empty(), "disabled forwarding emits nothing");
        }
        assert_eq!(client.dropped_logs(), 0, "disabled forwarding is a no-op, not a drop");

        let mut host = Peer::new(HOST, HOST, v, Config::default(), HOST_NONCE);
        host.config_mut().debug.forward_to_host = true;
        assert!(host.forward_log(LogLevel::Warn, "x").is_empty(), "the host doesn't forward its own logs");
        assert_eq!(host.dropped_logs(), 0, "and that's not counted as a drop");
    }

    #[test]
    fn forward_log_drop_count_accumulates_across_separate_overflows() {
        // Over two separate bursts (each past the available tokens) the dropped-logs counter keeps
        // climbing — it's a session total, not reset per burst.
        let v = Version::new(0, 1, 0);
        let mut client = Peer::new(CLIENT, HOST, v, Config::default(), CLIENT_NONCE);
        client.config_mut().debug.forward_to_host = true;
        for _ in 0..(LOG_FORWARD_BURST + 5) {
            client.forward_log(LogLevel::Trace, "x");
        }
        assert_eq!(client.dropped_logs(), 5, "first overflow dropped 5");
        client.maintain(); // refill LOG_FORWARD_REFILL_PER_TICK tokens
        let refill = LOG_FORWARD_REFILL_PER_TICK as u64;
        for _ in 0..(refill + 3) {
            client.forward_log(LogLevel::Trace, "x");
        }
        assert_eq!(client.dropped_logs(), 5 + 3, "second overflow adds 3 to the running total");
    }

    // --- eviction (session-leave) ----------------------------------------------------------------

    #[test]
    fn evicting_a_linked_peer_clears_roster_link_liveness_and_banners() {
        // A linked, version-incompatible peer (so a version banner is up) is evicted: every per-peer
        // collection empties and the banner is torn down. The private `last_seen` entry is verified
        // indirectly — with it gone, ticking far past the liveness timeout can never flag the peer.
        let v_us = Version::new(1, 0, 0);
        let v_them = Version::new(2, 0, 0);
        let mut host = Peer::new(HOST, HOST, v_us, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v_them.to_u32(), nonce: CLIENT_NONCE });
        let proof = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof });
        assert!(host.is_linked(CLIENT), "test setup: linked");
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("version mismatch")));

        assert!(host.evict(CLIENT), "a linked peer is known");

        assert!(host.known_peers().is_empty(), "roster entry removed");
        assert!(!host.is_linked(CLIENT), "link removed");
        assert!(host.notifications().banners().is_empty(), "version banner torn down");
        for _ in 0..=(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain();
        }
        assert!(!host.is_stale(CLIENT), "no liveness tracking survives eviction");
        assert!(host.notifications().banners().is_empty(), "and no banner ever resurfaces");
    }

    #[test]
    fn evicting_a_stale_peer_clears_the_lost_contact_banner_and_stale_flag() {
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        for _ in 0..=LIVENESS_TIMEOUT_TICKS {
            host.maintain();
        }
        assert!(host.is_stale(CLIENT), "test setup: flagged as lost");
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("Lost contact")));

        assert!(host.evict(CLIENT));
        assert!(!host.is_stale(CLIENT), "stale flag removed");
        assert!(host.notifications().banners().is_empty(), "lost-contact banner torn down");
    }

    #[test]
    fn evicting_an_unlinked_wrong_password_peer_clears_its_auth_banner() {
        // A wrong-password stranger never linked, but it does hold roster/nonce entries and an
        // auth-failure banner — eviction forgets all of it.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: CLIENT_NONCE });
        let bad = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, "wrong-password");
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: bad });
        assert!(host.notifications().banners().iter().any(|b| b.message.contains("Authentication failed")));

        assert!(host.evict(CLIENT), "a discovered-but-unlinked peer is still known");
        assert!(host.known_peers().is_empty());
        assert!(host.notifications().banners().is_empty(), "auth banner torn down");
    }

    #[test]
    fn an_evicted_peer_re_links_from_scratch_with_a_fresh_nonce_and_seqs() {
        // The load-bearing eviction property: the SAME peer id re-appearing must re-link cleanly.
        // That requires the old nonce to be gone (its stale proof must not verify), and both seq
        // gates reset (the rejoiner restarts its outbound seqs at 1, which a retained high-water
        // mark would reject as stale).
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        // Advance both per-sender gates well past the seqs the rejoiner will reuse.
        host.handle(CLIENT, ModMessage::SessionAction { seq: 5, action: SessionAction::JoinWorld });
        host.handle(CLIENT, ModMessage::Log(LogRecord { seq: 5, level: LogLevel::Info, message: "pre".into() }));
        assert_eq!(host.last_action(), Some((CLIENT, SessionAction::JoinWorld)));
        assert_eq!(host.log_bundle().len(), 1);

        assert!(host.evict(CLIENT));

        // A replay of the departed session's proof finds no nonce on file and drops quietly.
        let stale = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &CLIENT_NONCE, PW);
        host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: stale });
        assert!(!host.is_linked(CLIENT), "the departed session's proof must not re-link");
        assert!(host.notifications().banners().is_empty(), "and drops without a banner");

        // Fresh session: new Hello (new nonce) + a proof bound to it re-links from scratch, and the
        // host treats it as newly linked (it pushes the current settings immediately).
        let fresh_nonce: AuthNonce = [0x77; AUTH_NONCE_LEN];
        host.handle(CLIENT, ModMessage::Hello { mod_version: v.to_u32(), nonce: fresh_nonce });
        let fresh = crate::crypto::auth_proof(HOST, CLIENT, &HOST_NONCE, &fresh_nonce, PW);
        let out = host.handle(CLIENT, ModMessage::Auth { to: HOST, proof: fresh });
        assert!(host.is_linked(CLIENT), "a fresh Hello + Auth re-links the evicted peer");
        assert!(
            out.iter().any(|m| matches!(m, ModMessage::ConfigSync { .. })),
            "re-linking is a fresh link: the host syncs settings to the rejoiner"
        );

        // The rejoiner's restarted seq streams are accepted from 1 — the gates were forgotten.
        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::OpenWorld });
        assert_eq!(
            host.last_action(),
            Some((CLIENT, SessionAction::OpenWorld)),
            "a rejoiner's seq-1 action applies (action gate was reset)"
        );
        host.handle(CLIENT, ModMessage::Log(LogRecord { seq: 1, level: LogLevel::Info, message: "post".into() }));
        assert_eq!(host.log_bundle().len(), 2, "a rejoiner's seq-1 log aggregates (log gate was reset)");
    }

    #[test]
    fn evicting_an_unknown_peer_is_a_noop_returning_false() {
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        const NEVER_SEEN: PeerId = 42;
        assert!(!host.evict(NEVER_SEEN), "an unknown peer is not known");
        assert!(host.is_linked(CLIENT), "and nothing else is disturbed");

        // Double-evict: the second call finds nothing left and reports unknown.
        assert!(host.evict(CLIENT));
        assert!(!host.evict(CLIENT), "an already-evicted peer is no longer known");
    }

    #[test]
    fn a_ping_only_stranger_counts_as_known_to_evict() {
        // A bare Ping registers the sender only in `last_seen` — eviction still reports it as known
        // (it held per-peer state) and clears that liveness tracking.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        host.handle(CLIENT, ModMessage::Ping { frame: 1 });
        assert!(host.evict(CLIENT), "liveness-only state still makes the peer known");
        for _ in 0..=(LIVENESS_TIMEOUT_TICKS + 1) {
            host.maintain();
        }
        assert!(!host.is_stale(CLIENT), "its liveness tracking is gone");
    }

    #[test]
    fn evicting_one_peer_does_not_disturb_another() {
        // Two linked clients — CLIENT at a compatible version, CLIENT2 version-incompatible (so it
        // carries a banner) and with an advanced action-gate mark. Evicting CLIENT must leave all of
        // CLIENT2's state exactly as it was: link, roster, banner, and gate high-water mark.
        let v_us = Version::new(1, 0, 0);
        let v_them = Version::new(2, 0, 0);
        let mut host = Peer::new(HOST, HOST, v_us, config_with_pw(PW), HOST_NONCE);
        link_client_into(&mut host, CLIENT, CLIENT_NONCE, v_us);
        host.handle(CLIENT2, ModMessage::Hello { mod_version: v_them.to_u32(), nonce: CLIENT2_NONCE });
        let proof2 = crate::crypto::auth_proof(HOST, CLIENT2, &HOST_NONCE, &CLIENT2_NONCE, PW);
        host.handle(CLIENT2, ModMessage::Auth { to: HOST, proof: proof2 });
        assert!(host.is_linked(CLIENT2), "test setup: CLIENT2 linked");
        host.handle(CLIENT2, ModMessage::SessionAction { seq: 3, action: SessionAction::JoinWorld });

        assert!(host.evict(CLIENT));

        assert!(host.is_linked(CLIENT2), "the other peer stays linked");
        assert!(host.known_peers().contains_key(&CLIENT2), "and stays on the roster");
        assert!(
            host.notifications().banners().iter().any(|b| b.message.contains("version mismatch")),
            "its banner is untouched"
        );
        // Its gate high-water mark survives: a stale redelivery is still rejected...
        host.handle(CLIENT2, ModMessage::SessionAction { seq: 3, action: SessionAction::OpenWorld });
        assert_eq!(
            host.last_action(),
            Some((CLIENT2, SessionAction::JoinWorld)),
            "CLIENT2's stale seq-3 redelivery is still rejected after evicting CLIENT"
        );
        // ...and its next real seq still advances.
        host.handle(CLIENT2, ModMessage::SessionAction { seq: 4, action: SessionAction::OpenWorld });
        assert_eq!(host.last_action(), Some((CLIENT2, SessionAction::OpenWorld)));
    }

    // --- roster bound (MAX_TRACKED_PEERS) ---------------------------------------------------------

    /// Stranger ids guaranteed disjoint from the fixture peers (HOST=1, CLIENT=2, CLIENT2=4).
    fn stranger_id(i: usize) -> PeerId {
        1000 + i as u64
    }

    #[test]
    fn a_stranger_flood_cannot_grow_peer_state_past_the_roster_cap() {
        // Feed frames from far more distinct sender ids than the cap. Tracked state (roster, nonce
        // table, liveness) must stay bounded, the overflow must be counted, and exactly one
        // roster-full banner raised — never one per dropped frame (a stranger flood must not be able
        // to spam the overlay).
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v); // CLIENT occupies one slot and is linked
        for i in 0..(MAX_TRACKED_PEERS * 3) {
            let out = host.handle(
                stranger_id(i),
                ModMessage::Hello { mod_version: v.to_u32(), nonce: [0x66; AUTH_NONCE_LEN] },
            );
            if i >= MAX_TRACKED_PEERS - 1 {
                // CLIENT holds a slot, so strangers fill the remaining cap-1; the rest are dropped
                // before any state insert — including the Hello's proof reply.
                assert!(out.is_empty(), "stranger {i} past the cap must get no reply");
            }
        }
        assert_eq!(host.known_peers().len(), MAX_TRACKED_PEERS, "roster bounded at the cap");
        assert_eq!(
            host.roster_overflow_drops(),
            (MAX_TRACKED_PEERS * 3 - (MAX_TRACKED_PEERS - 1)) as u64,
            "every frame past the cap is counted"
        );
        let roster_banners = host
            .notifications()
            .banners()
            .iter()
            .filter(|b| b.message == ROSTER_FULL_MESSAGE)
            .count();
        assert_eq!(roster_banners, 1, "one banner for the condition, not one per frame");
    }

    #[test]
    fn a_full_roster_still_serves_tracked_and_linked_peers() {
        // The cap turns away only NEW senders. The already-linked CLIENT (and every tracked peer)
        // must keep working exactly as before: liveness refresh, actions, config flow.
        let v = Version::new(0, 1, 0);
        let mut host = linked_host(v);
        for i in 0..MAX_TRACKED_PEERS {
            host.handle(stranger_id(i), ModMessage::Ping { frame: 1 });
        }
        assert!(host.roster_overflow_drops() > 0, "test premise: the cap is engaged");
        host.handle(CLIENT, ModMessage::SessionAction { seq: 1, action: SessionAction::JoinWorld });
        assert_eq!(
            host.last_action(),
            Some((CLIENT, SessionAction::JoinWorld)),
            "a linked peer's frames pass the full roster"
        );
    }

    #[test]
    fn evicting_a_peer_reopens_a_roster_slot_and_clears_the_banner() {
        // A real leave frees the slot: the roster-full banner comes down and the next newcomer is
        // admitted (tracked + replied to) again.
        let v = Version::new(0, 1, 0);
        let mut host = Peer::new(HOST, HOST, v, config_with_pw(PW), HOST_NONCE);
        for i in 0..MAX_TRACKED_PEERS {
            host.handle(stranger_id(i), ModMessage::Ping { frame: 1 });
        }
        // Cap reached: one more distinct sender is turned away and banners.
        host.handle(9999, ModMessage::Ping { frame: 1 });
        assert_eq!(host.roster_overflow_drops(), 1);
        assert!(host.notifications().banners().iter().any(|b| b.message == ROSTER_FULL_MESSAGE));

        assert!(host.evict(stranger_id(0)), "a tracked stranger is known to evict");
        assert!(
            !host.notifications().banners().iter().any(|b| b.message == ROSTER_FULL_MESSAGE),
            "freeing a slot clears the roster-full banner"
        );
        // The newcomer is admitted now: its Hello is tracked and answered with our Auth proof.
        let out = host.handle(
            9999,
            ModMessage::Hello { mod_version: v.to_u32(), nonce: [0x55; AUTH_NONCE_LEN] },
        );
        assert!(host.known_peers().contains_key(&9999), "admitted into the freed slot");
        assert!(!out.is_empty(), "and the handshake proceeds (proof reply sent)");
    }
}
