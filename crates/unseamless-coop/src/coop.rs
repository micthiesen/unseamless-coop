//! Rung 2 of [`docs/COOP-CONNECTION.md`]: the private Steam P2P **side-channel**.
//!
//! Runs the host-tested [`unseamless_core::peer::Session`] over a real Steam transport, so two
//! modded games' coordination layers actually talk — version handshake, the host pushing its
//! authoritative `ConfigSync`, liveness, and (client→host) debug-log forwarding. This is the
//! mod-coordination channel, **not** the game's own world/position sync (that's the rung-3 RE).
//!
//! ## Shape (mirrors the dev `bridge`)
//! The [`bridge`](crate::bridge) was the loopback rehearsal for exactly this: drive a `Session` on a
//! dedicated thread, applying any received config into the live [`crate::state`] so the game-thread
//! features pick it up. Here the transport is [`SteamP2PTransport`] over `ISteamNetworkingMessages`
//! (see [`crate::steam`]) instead of a TCP socket, and the peer is a **manually-entered** SteamID
//! (`[coop] peer_steam_id`, exchanged out of band per rung 1) — lobby discovery is the deferred rung
//! 4. Unlike the bridge (a pure test harness), this also surfaces connection events to the in-game
//! overlay via [`crate::notify`].
//!
//! ## Why its own thread (not a game-frame task)
//! Steam's networking calls are thread-safe and the `Session`/`Peer` are pure core types, so the
//! driver lives off the game's task scheduler (like the bridge and the init/overlay threads). The
//! only cross-thread state is the live config ([`crate::state`], a `Mutex`), the notifications
//! ([`crate::notify`], a `Mutex`), the forward queue ([`crate::forward`]), and one status atomic
//! ([`PHASE`]) read by the debug panel.
//!
//! ## Cadence is load-bearing
//! `pump` (receive) runs every [`POLL_INTERVAL`] for a responsive handshake; `maintain` runs every
//! [`MAINTAIN_INTERVAL`] (~1 Hz) because the `Peer`'s liveness/refill constants are denominated in
//! *maintain ticks* (≈30 ticks of silence before "Lost contact"; see `peer.rs`). Keep that ~1 s beat
//! stable or those wall-clock meanings drift.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use unseamless_core::config::Config;
use unseamless_core::diagnostics::peer_tag;
use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Severity};
use unseamless_core::peer::{
    CONFIG_SYNCED_MESSAGE, Peer, Session, lost_contact_message, version_mismatch_message,
};
use unseamless_core::protocol::PROTOCOL_VERSION;
use unseamless_core::transport::{PeerId, Transport};

use crate::steam::{self, Networking};

/// Poll the receive side this often (responsive handshake / config sync).
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Run `Session::maintain` this often — the heartbeat + host config re-assert + liveness sweep. The
/// `Peer`'s tick-denominated constants assume ~1 Hz here (see the module note).
const MAINTAIN_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait for rung 1 to resolve our own SteamID before giving up on the side-channel.
/// Sized just past rung 1's own resolver budget (`steam::QUERY_MAX_ATTEMPTS * QUERY_RETRY_DELAY` =
/// 30 s, after which `SELF_STEAM_ID` can never become non-zero): once that poller gives up there's
/// nothing left to wait for, so polling much longer here would only spin uselessly.
const SELF_ID_TIMEOUT: Duration = Duration::from_secs(35);
const SELF_ID_POLL: Duration = Duration::from_millis(250);

/// Banner ids for the two persistent connection conditions, so each updates in place / clears cleanly.
const BANNER_VERSION: &str = "coop-version";
const BANNER_LIVENESS: &str = "coop-liveness";

/// Published connection phase for the diagnostic report's `coop` line (read from the game thread).
/// The driver is on its own thread, so this single relaxed atomic is the whole cross-thread readout.
static PHASE: AtomicU8 = AtomicU8::new(PHASE_OFF);
const PHASE_OFF: u8 = 0; // no peer configured — driver never started
const PHASE_LINKING: u8 = 1; // resolving identity + networking; partner not yet heard from
const PHASE_LINKED: u8 = 2; // handshake complete (partner's Hello seen)
const PHASE_LOST: u8 = 3; // was linked, partner went silent
const PHASE_FAILED: u8 = 4; // a partner WAS configured, but startup failed (no Steam ID / networking)

/// One-line co-op connection status for the diagnostic report's `coop` field (inside the `steam`
/// section), so a rig run (or a friend's shared log) shows at a glance whether the side-channel
/// linked up — and, crucially, distinguishes "not configured" from "configured but failed to start".
pub fn status_line() -> &'static str {
    match PHASE.load(Ordering::Relaxed) {
        PHASE_LINKING => "linking (Steam P2P)",
        PHASE_LINKED => "linked",
        PHASE_LOST => "partner lost (silent)",
        PHASE_FAILED => "configured, but couldn't start (see log)",
        _ => "off (no [coop] peer_steam_id set)",
    }
}

/// Start the side-channel if a partner SteamID is configured. No-op (the default until a friend test)
/// when `[coop] peer_steam_id` is unset, so a normal solo session pays nothing. Spawns one detached
/// driver thread; reads the few config values it needs up front (it then snapshots live config).
pub fn start(config: &Config) {
    let peer_id = config.coop.peer_steam_id;
    if peer_id == 0 {
        return;
    }
    let is_host = config.coop.is_host;
    // Only a client forwards its log to the host, and only when asked (`forward_to_host`).
    let forward = !is_host && config.debug.forward_to_host;
    let spawned = std::thread::Builder::new()
        .name("unseamless-coop".into())
        .spawn(move || run(peer_id, is_host, forward));
    if let Err(e) = spawned {
        log::warn!("coop: couldn't spawn the side-channel thread: {e}");
    }
}

/// The driver: resolve our identity + Steam networking, stand up a `Session`, then pump/maintain it
/// for the process lifetime. Every failure to *start* degrades gracefully (logs + leaves `PHASE`
/// off) — the side-channel is best-effort, never fatal.
fn run(peer_id: PeerId, is_host: bool, forward: bool) {
    PHASE.store(PHASE_LINKING, Ordering::Relaxed);

    let Some(self_id) = wait_self_id() else {
        log::warn!("coop: own SteamID never resolved; side-channel not started");
        PHASE.store(PHASE_FAILED, Ordering::Relaxed);
        return;
    };
    if self_id == peer_id {
        log::error!("coop: [coop] peer_steam_id is our own SteamID; nothing to connect to");
        PHASE.store(PHASE_FAILED, Ordering::Relaxed);
        return;
    }
    // Host identity is agreed out of band by who sets `is_host` (manual pairing for rung 2): the host
    // is authoritative for the shared settings; the client adopts them.
    let host_id = if is_host { self_id } else { peer_id };

    let Some(net) = Networking::resolve() else {
        log::warn!("coop: ISteamNetworkingMessages unavailable; side-channel not started");
        PHASE.store(PHASE_FAILED, Ordering::Relaxed);
        return;
    };
    // We know the peer out of band, so accept its session up front rather than waiting on the
    // SessionRequest callback (which we deliberately don't pump — see COOP-CONNECTION.md). The first
    // outbound send also implicitly opens the session from our side.
    net.accept_session(peer_id);

    log::info!(
        "coop: side-channel up as {} with partner {} (Steam P2P)",
        if is_host { "host" } else { "client" },
        peer_tag(peer_id),
    );

    let transport = SteamP2PTransport { net, local_id: self_id, peer_id };
    let mut session =
        Session::new(Peer::new(self_id, host_id, PROTOCOL_VERSION, crate::state::snapshot()), transport);
    session.connect();

    if forward {
        crate::forward::set_enabled(true);
        log::info!("coop: forwarding this client's debug log to the host");
    }

    // Seed the change-detector with the config we started from, so only a *received* host ConfigSync
    // (which differs) drives a live-config write + "synced" toast — not the no-op initial seed.
    let mut mirrored = session.peer().config().clone();
    let mut last_maintain = Instant::now();
    let mut linked = false;
    let mut lost = false;

    loop {
        // Client: ship locally-produced debug logs the host asked us to forward, before the pump so
        // they go out this tick. The `Peer` rate-limits, so a burst can't flood the channel.
        if forward {
            crate::forward::drain(|level, message| {
                let out = session.peer_mut().forward_log(level, message);
                session.broadcast(out);
            });
        }

        session.pump();

        if last_maintain.elapsed() >= MAINTAIN_INTERVAL {
            session.maintain();
            last_maintain = Instant::now();
        }

        // Client adopts the host's authoritative shared settings: mirror them into the live config so
        // the game-thread features pick them up (the same apply path the dev bridge exercises). The
        // host has nothing to adopt — its own config is authoritative.
        if !is_host {
            let cfg = session.peer().config();
            if *cfg != mirrored {
                mirrored = cfg.clone();
                crate::state::set(mirrored.clone());
                toast(Severity::Info, CONFIG_SYNCED_MESSAGE);
            }
        }

        update_link_status(&session, peer_id, &mut linked, &mut lost);

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Surface connection transitions to the in-game overlay: a one-time "connected" toast on the
/// partner's first `Hello` (with a version-mismatch banner if majors differ), and a "Lost contact"
/// banner that sets/clears on the liveness edge. Mirrors the `Peer`'s own internal notifications
/// (which only the harness reads) onto the global [`crate::notify`] surface the overlay draws.
fn update_link_status(
    session: &Session<SteamP2PTransport>,
    peer_id: PeerId,
    linked: &mut bool,
    lost: &mut bool,
) {
    let peer = session.peer();
    if !*linked
        && let Some(&their_version) = peer.known_peers().get(&peer_id)
    {
        *linked = true;
        PHASE.store(PHASE_LINKED, Ordering::Relaxed);
        toast(Severity::Info, format!("Co-op partner connected ({})", peer_tag(peer_id)));
        if !PROTOCOL_VERSION.compatible_with(their_version) {
            set_banner(
                BANNER_VERSION,
                Severity::Warning,
                version_mismatch_message(peer_id, their_version, PROTOCOL_VERSION),
            );
        }
    }
    // Liveness flips only matter once we've linked (before that, "not heard from" is just "not yet").
    if *linked {
        let now_lost = peer.is_stale(peer_id);
        if now_lost != *lost {
            *lost = now_lost;
            if now_lost {
                PHASE.store(PHASE_LOST, Ordering::Relaxed);
                set_banner(BANNER_LIVENESS, Severity::Warning, lost_contact_message(peer_id));
            } else {
                PHASE.store(PHASE_LINKED, Ordering::Relaxed);
                clear_banner(BANNER_LIVENESS);
            }
        }
    }
}

/// Block until rung 1 publishes our own SteamID, or [`SELF_ID_TIMEOUT`] elapses (`None`).
fn wait_self_id() -> Option<PeerId> {
    let deadline = Instant::now() + SELF_ID_TIMEOUT;
    loop {
        if let Some(id) = steam::self_steam_id() {
            return Some(id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(SELF_ID_POLL);
    }
}

fn toast(severity: Severity, message: impl Into<String>) {
    crate::notify::with_mut(|n| n.toast(severity, message, DEFAULT_TOAST_SECS));
}
fn set_banner(id: &str, severity: Severity, message: impl Into<String>) {
    crate::notify::with_mut(|n| n.set_banner(id, severity, message));
}
fn clear_banner(id: &str) {
    crate::notify::with_mut(|n| {
        n.clear_banner(id);
    });
}

/// A [`Transport`] over Steam's `ISteamNetworkingMessages` to a single, known partner. In a 2-player
/// session "broadcast" is just "send to the one peer"; `poll` drains our channel, reads each frame's
/// real sender, and keeps only those from the configured partner (the explicit trust gate). The byte
/// payload is one encoded [`unseamless_core::protocol::ModMessage`] — `ISteamNetworkingMessages` is
/// message-oriented, so each send maps to exactly one received frame (no stream framing needed, unlike
/// the TCP bridge).
struct SteamP2PTransport {
    net: Networking,
    local_id: PeerId,
    peer_id: PeerId,
}

impl Transport for SteamP2PTransport {
    fn send(&mut self, bytes: &[u8]) {
        self.net.send_to(self.peer_id, bytes);
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        // Trust boundary: only accept frames from the *configured* partner. Steam already won't
        // deliver from a user whose session we never accepted (we accept only `peer_id`), but that's
        // an implicit Steam semantic — make the 2-player invariant explicit here so a stray frame
        // from anyone else is dropped before it can reach the `Peer` (roster, actions, liveness).
        self.net.receive().into_iter().filter(|(from, _)| *from == self.peer_id).collect()
    }

    fn local_id(&self) -> PeerId {
        self.local_id
    }
}
