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
//! (see [`crate::steam`]) instead of a TCP socket, and the peer is resolved by **password-keyed lobby
//! discovery** (rung 4, [`steam::LobbyDiscovery`]): both players key a Steam lobby off the shared
//! session password, the host (Open World) creating it and the joiner (Join world) entering it. The
//! role is the user's choice, triggered by the menu action — not started at launch and not derived from
//! who-creates-first. Unlike the bridge (a pure test harness), this also surfaces connection events to
//! the in-game overlay via [`crate::notify`].
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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use unseamless_core::config::Config;
use unseamless_core::diagnostics::{ConnectReport, LobbyProgress, LobbyRole, VersionCheck, peer_tag};
use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Severity};
use unseamless_core::peer::{
    CONFIG_SYNCED_MESSAGE, Peer, Session, lost_contact_message, version_mismatch_message,
};
use unseamless_core::protocol::{PROTOCOL_VERSION, SharedSettings};
use unseamless_core::transport::{PeerId, Transport};

use crate::steam::{self, LobbyDiscovery, LobbyIntent, LobbyResult, Networking};

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

/// Banner ids for the persistent connection conditions, so each updates in place / clears cleanly.
const BANNER_VERSION: &str = "coop-version";
const BANNER_LIVENESS: &str = "coop-liveness";
/// The session-lifecycle banner (opening / searching / world-open / linking), cleared once connected
/// or the session ends.
const BANNER_SESSION: &str = "coop-session";

// ---- Session lifecycle (user-facing) --------------------------------------------------------------
//
// Distinct from `PHASE` (the rung-2 *link* diagnostic): `SESSION` is the user-facing "are we in a
// session, and as what" that drives the menu's [`unseamless_core::menu::SessionContext`] gating (Open
// World / Join disabled while in a session; Leave enabled). Read non-blocking by the overlay's Present
// thread via [`session_context`]; written only by the co-op driver thread + the action entry points.

static SESSION: AtomicU8 = AtomicU8::new(SESSION_OFF);
const SESSION_OFF: u8 = 0; // solo — no session
const SESSION_CONNECTING: u8 = 1; // host setting up / joiner searching / rung-2 linking
const SESSION_HOSTING: u8 = 2; // host: lobby open, waiting for a friend
const SESSION_CONNECTED: u8 = 3; // partner linked
/// Whether the active session is ours to host (Open World) vs joined (Join world). Only meaningful when
/// `SESSION != SESSION_OFF`.
static IS_HOST: AtomicBool = AtomicBool::new(false);

/// Bumped on every [`host`]/[`join`]/[`leave`] so an in-flight driver thread learns its session ended
/// (Leave) or was superseded (a new Open/Join): the driver captures its generation at spawn and tears
/// down — leaving its Steam lobby — once the global generation no longer matches ([`stale`]). One
/// session at a time, so a single counter is the whole stop mechanism.
static SESSION_GEN: AtomicU64 = AtomicU64::new(0);

/// Has the session that captured `generation` been ended or superseded? The driver checks this each tick.
fn stale(generation: u64) -> bool {
    SESSION_GEN.load(Ordering::Relaxed) != generation
}

/// The current `(in_session, is_host)` for the menu's `SessionContext`, read from the overlay's Present
/// thread. `in_session` gates Open World/Join (off when set) and Leave (on when set).
pub fn session_context() -> (bool, bool) {
    let in_session = SESSION.load(Ordering::Relaxed) != SESSION_OFF;
    (in_session, IS_HOST.load(Ordering::Relaxed))
}

/// Host setup budget: a host should reach an open lobby near-instantly (one existence-check list + a
/// `CreateLobby`); past this, Steam matchmaking is wedged. Once the lobby is open the timeout is
/// dropped (a host then waits for a friend indefinitely).
const HOST_SETUP_TIMEOUT: Duration = Duration::from_secs(20);
/// Join search budget: short retry + give up (we only search for an already-open world). Past this, no
/// matching world was found — tell the user rather than spin forever.
const JOIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Published connection phase for the diagnostic report's `coop` line (read from the game thread).
/// The driver is on its own thread, so this single relaxed atomic is the whole cross-thread readout.
static PHASE: AtomicU8 = AtomicU8::new(PHASE_OFF);
const PHASE_OFF: u8 = 0; // discovery never started (no password / gated off) — driver never ran
const PHASE_LINKING: u8 = 1; // resolving identity + networking; partner not yet heard from
const PHASE_LINKED: u8 = 2; // handshake complete (partner's Hello seen)
const PHASE_LOST: u8 = 3; // was linked, partner went silent
const PHASE_FAILED: u8 = 4; // discovery was attempted, but startup failed (no Steam ID / networking / timeout)

/// One-line co-op connection status for the diagnostic report's `coop` field (inside the `steam`
/// section), so a rig run (or a friend's shared log) shows at a glance whether the side-channel
/// linked up — and, crucially, distinguishes "not configured" from "configured but failed to start".
pub fn status_line() -> &'static str {
    match PHASE.load(Ordering::Relaxed) {
        PHASE_LINKING => "linking (Steam P2P)",
        PHASE_LINKED => "linked",
        PHASE_LOST => "partner lost (silent)",
        PHASE_FAILED => "attempted, but couldn't start (see log)",
        _ => "off (no co-op password, or discovery disabled)",
    }
}

// ---- Connect report (the per-stage "why did this attempt fail" telemetry) -------------------------
//
// Where `PHASE`/[`status_line`] is the one-word verdict, the [`ConnectReport`] records each stage and
// its timing so a *single* shared log from a failed two-player test distinguishes one-way NAT vs
// no-receive vs version mismatch vs an empty lobby filter without a second run. The driver lives on its
// own thread, so the report is shared behind a `Mutex` and snapshotted from the game thread for the
// diagnostic dump. Pure model + renderer are host-tested in [`unseamless_core::diagnostics`].

/// Live connect report, written by the driver/lobby code and snapshotted for the diag dump.
static REPORT: Mutex<ConnectReport> = Mutex::new(ConnectReport::new());
/// `true` once a connection attempt has begun (a peer was configured, or rung-4 discovery started), so
/// [`connect_report`] stays silent for a solo session that never tried to link.
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
/// Epoch the report's `+Ns` stage timings are relative to — set once when the attempt begins. Relative
/// ordering (which stage came when) is what diagnoses a stuck link; wall-clock isn't needed.
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Mark the start of a connection attempt: arm [`connect_report`] and pin the timing epoch.
fn begin_attempt() {
    ATTEMPTED.store(true, Ordering::Relaxed);
    let _ = EPOCH.get_or_init(Instant::now);
}

/// Elapsed since [`begin_attempt`], for stamping a stage. `0` if somehow called before the epoch is set
/// (the `get_or_init` makes that impossible in practice, but it keeps this total).
fn elapsed() -> Duration {
    EPOCH.get_or_init(Instant::now).elapsed()
}

/// Mutate the shared report under its lock, recovering a poisoned lock rather than wedging diagnostics
/// (a panic mid-update must never make the report unreadable for the rest of the session).
pub(crate) fn with_report(f: impl FnOnce(&mut ConnectReport)) {
    let mut g = REPORT.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g);
}

/// Snapshot the connect report for the diagnostic dump — `None` until an attempt has begun (so a solo
/// log isn't cluttered with an all-"not reached" block). Non-blocking-ish (a short lock); safe from the
/// game-thread diag caller.
pub fn connect_report() -> Option<ConnectReport> {
    if !ATTEMPTED.load(Ordering::Relaxed) {
        return None;
    }
    Some(REPORT.lock().unwrap_or_else(|e| e.into_inner()).clone())
}

/// Record a terminal failure reason (plain words) on the report, so a coarse `failed` phase carries its
/// "why" into the shared log.
fn record_failure(why: impl Into<String>) {
    let why = why.into();
    with_report(|r| r.failure = Some(why));
}

// Rung-4 lobby-discovery stage notes — called from [`crate::steam`]'s poll-based discovery (on the co-op
// driver thread). Centralized here so all report mutation + timing stamping lives in one place. These
// are referenced only by the (currently dormant) rung-4 path; see [`start`].

/// Record which side of lobby discovery we're playing (creates the `lobby` sub-report). Idempotent for a
/// repeated *same* role (a `CreateLobby` retry re-announces `Host`) so it doesn't wipe earlier stamps; a
/// genuine role flip (losing the both-create race ⇒ `Host`→`Joiner`) does reset, reflecting the new role.
pub(crate) fn note_lobby_role(role: LobbyRole) {
    with_report(|r| {
        if r.lobby.as_ref().map(|l| l.role) != Some(role) {
            r.lobby = Some(LobbyProgress::new(role));
        }
    });
}
/// Host: the lobby was created and its password data published.
pub(crate) fn note_lobby_created() {
    let at = elapsed();
    with_report(|r| {
        if let Some(l) = r.lobby.as_mut() {
            l.created_at = Some(at);
        }
    });
}
/// Joiner: the filtered lobby list returned, with `candidates` matches (`0` = empty filter).
pub(crate) fn note_lobby_list(candidates: u32) {
    let at = elapsed();
    with_report(|r| {
        if let Some(l) = r.lobby.as_mut() {
            l.list_returned_at = Some(at);
            l.candidates = Some(candidates);
        }
    });
}
/// Joiner: we entered the lobby.
pub(crate) fn note_lobby_joined() {
    let at = elapsed();
    with_report(|r| {
        if let Some(l) = r.lobby.as_mut() {
            l.joined_at = Some(at);
        }
    });
}
/// The host's SteamID was read out of the lobby (the value that seeds rung 2).
pub(crate) fn note_lobby_host_resolved() {
    with_report(|r| {
        if let Some(l) = r.lobby.as_mut() {
            l.host_id_resolved = true;
        }
    });
}
/// A rung-4 lobby-discovery step failed — record the plain-words "why" on the report.
pub(crate) fn note_lobby_failure(why: impl Into<String>) {
    record_failure(why);
}

/// How often the discovery driver polls the lobby state machine while resolving a partner.
const LOBBY_DISCOVERY_POLL: Duration = Duration::from_millis(250);

/// **Open World** (host): create a password-keyed lobby and wait for a friend to join. Triggered by the
/// menu action, not at launch — a solo session pays nothing. The session password (the lobby key) is
/// guaranteed valid by the startup guard ([`crate::guard`]).
pub fn host(config: &Config) {
    start_session(config, LobbyIntent::Host);
}

/// **Join world** (client): search for an existing password-keyed lobby and enter it. Triggered by the
/// menu action.
pub fn join(config: &Config) {
    start_session(config, LobbyIntent::Join);
}

/// Begin a session in the chosen role: bump the generation (superseding any prior session and signalling
/// any in-flight driver to tear down), publish the connecting state for the menu gating, and spawn one
/// detached driver thread. The create + poll mechanism is rig-proven
/// (`steam::run_lobby_callback_probe`); the joiner-finds-host leg across two machines is still validated
/// by the two-player friend test (RIG-VERIFY in `steam.rs`).
fn start_session(config: &Config, intent: LobbyIntent) {
    let generation = SESSION_GEN.fetch_add(1, Ordering::Relaxed) + 1;
    let is_host = matches!(intent, LobbyIntent::Host);
    IS_HOST.store(is_host, Ordering::Relaxed);
    SESSION.store(SESSION_CONNECTING, Ordering::Relaxed);
    set_banner(
        BANNER_SESSION,
        Severity::Info,
        if is_host { "Opening your world..." } else { "Searching for a world..." },
    );
    let password = config.session.password.clone();
    // Only a client forwards its debug log to the host, and only when asked; the host has nothing to
    // forward. Captured here and applied in `run_session` once we know the role held.
    let forward_pref = config.debug.forward_to_host;
    spawn_driver("unseamless-coop-lobby", move || run_discovery(password, intent, forward_pref, generation));
}

/// **Leave world**: end the active session. Bumps the generation (the in-flight driver sees it go
/// [`stale`], leaves its Steam lobby, and exits) and resets the published state + banners so the menu
/// re-enables Open World/Join. Safe to call with no active session (just resets state).
pub fn leave() {
    SESSION_GEN.fetch_add(1, Ordering::Relaxed);
    SESSION.store(SESSION_OFF, Ordering::Relaxed);
    IS_HOST.store(false, Ordering::Relaxed);
    PHASE.store(PHASE_OFF, Ordering::Relaxed);
    clear_banner(BANNER_SESSION);
    clear_banner(BANNER_VERSION);
    clear_banner(BANNER_LIVENESS);
    toast(Severity::Info, "Left the session.");
}

/// Reset the published session state to "off" after a *failed* host/join attempt, toast the plain-words
/// reason, and clear the setup banner — so the menu re-enables Open World/Join for a retry. Distinct from
/// [`leave`] (a user-initiated end): this is the driver giving up. Does **not** bump the generation (the
/// driver is already exiting) — and the caller must not call it when [`stale`] (a superseding session
/// owns the state).
fn fail_session(why: impl Into<String>) {
    let why = why.into();
    record_failure(why.clone());
    PHASE.store(PHASE_FAILED, Ordering::Relaxed);
    SESSION.store(SESSION_OFF, Ordering::Relaxed);
    IS_HOST.store(false, Ordering::Relaxed);
    clear_banner(BANNER_SESSION);
    toast(Severity::Warning, why);
}

/// Spawn a detached, named driver thread, logging (not panicking) if the OS refuses the thread.
fn spawn_driver(name: &str, body: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new().name(name.into()).spawn(body);
    if let Err(e) = spawned {
        log::warn!("coop: couldn't spawn the {name} thread: {e}");
    }
}

/// Rung-4 driver: resolve the partner via password-keyed Steam-lobby discovery in the role the user
/// chose (`intent`), then hand off to the rung-2 [`run_session`]. Tears down cleanly if the user Leaves
/// (the captured `generation` goes [`stale`]); every failure to start surfaces a plain-words toast via
/// [`fail_session`] and resets the menu so the user can retry.
fn run_discovery(password: String, intent: LobbyIntent, forward_pref: bool, generation: u64) {
    begin_attempt();
    PHASE.store(PHASE_LINKING, Ordering::Relaxed);

    let Some(self_id) = wait_self_id() else {
        if stale(generation) {
            return; // user already left; nothing to report
        }
        log::warn!("coop: own SteamID never resolved; lobby discovery not started");
        fail_session("Steam isn't ready (own SteamID never resolved) — is Steam running and logged in?");
        return;
    };
    if stale(generation) {
        return; // user left while we waited on the SteamID
    }
    with_report(|r| r.self_id_at = Some(elapsed()));

    let Some(mut discovery) = LobbyDiscovery::start(self_id, &password, intent) else {
        if stale(generation) {
            return;
        }
        log::warn!("coop: Steam matchmaking unavailable; lobby discovery not started");
        fail_session("Steam matchmaking unavailable (Steam not up or an export is missing).");
        return;
    };

    // Per-role setup budget: a host's is dropped once the lobby is open (then it waits for a friend
    // indefinitely); a joiner's bounds the whole search.
    let mut deadline = Some(
        Instant::now()
            + match intent {
                LobbyIntent::Host => HOST_SETUP_TIMEOUT,
                LobbyIntent::Join => JOIN_TIMEOUT,
            },
    );
    let (peer_id, is_host) = loop {
        if stale(generation) {
            discovery.leave(); // user left (or a new session superseded us): drop our lobby and exit
            return;
        }
        match discovery.poll() {
            LobbyResult::Resolved { peer, is_host } => break (peer, is_host),
            LobbyResult::Hosting => {
                // Lobby is open: stop the setup timeout and switch the banner to the waiting state, once.
                if deadline.take().is_some() {
                    SESSION.store(SESSION_HOSTING, Ordering::Relaxed);
                    set_banner(BANNER_SESSION, Severity::Info, "World open — waiting for a friend to join.");
                }
            }
            LobbyResult::Pending => {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    fail_session(match intent {
                        LobbyIntent::Host => "Couldn't open the world — Steam matchmaking timed out.",
                        LobbyIntent::Join => "No open world found with this password.",
                    });
                    return;
                }
            }
            LobbyResult::Failed(why) => {
                fail_session(why);
                return;
            }
        }
        std::thread::sleep(LOBBY_DISCOVERY_POLL);
    };

    let forward = !is_host && forward_pref;
    log::info!(
        "coop: lobby discovery resolved partner {} (we are the {}); seeding rung 2",
        peer_tag(peer_id),
        if is_host { "host" } else { "client" },
    );
    // Hand the resolved peer + role to the rung-2 driver, keeping `discovery` alive so teardown can leave
    // the lobby — lobbies are discovery, not a new transport.
    run_session(self_id, peer_id, is_host, forward, generation, discovery);
}

/// The rung-2 driver: stand up a `Session` over Steam P2P to the resolved partner, then pump/maintain
/// it until the user Leaves (the captured `generation` goes [`stale`]). Owns `discovery` so teardown can leave
/// the Steam lobby. A failure to *start* the transport degrades via [`fail_session`] (toast + reset),
/// never aborts.
fn run_session(
    self_id: PeerId,
    peer_id: PeerId,
    is_host: bool,
    forward: bool,
    generation: u64,
    discovery: LobbyDiscovery,
) {
    if self_id == peer_id {
        log::error!("coop: resolved partner is our own SteamID; nothing to connect to");
        discovery.leave();
        if !stale(generation) {
            fail_session("Resolved partner is our own SteamID — nothing to connect to.");
        }
        return;
    }
    // The host (the lobby creator) is authoritative for the shared settings; the client adopts them.
    let host_id = if is_host { self_id } else { peer_id };

    let Some(net) = Networking::resolve() else {
        log::warn!("coop: ISteamNetworkingMessages unavailable; side-channel not started");
        discovery.leave();
        if !stale(generation) {
            fail_session("Steam networking unavailable — couldn't open the co-op channel.");
        }
        return;
    };
    // We know the peer out of band, so accept its session up front rather than waiting on the
    // SessionRequest callback (which we deliberately don't pump — see COOP-CONNECTION.md). The first
    // outbound send also implicitly opens the session from our side.
    net.accept_session(peer_id);
    with_report(|r| r.session_accepted_at = Some(elapsed()));

    log::info!(
        "coop: side-channel up as {} with partner {} (Steam P2P)",
        if is_host { "host" } else { "client" },
        peer_tag(peer_id),
    );
    // A joiner has nothing open yet; show the linking state until the handshake lands. (A host already
    // shows "world open — waiting"; the connected toast supersedes both on link.)
    if !is_host {
        set_banner(BANNER_SESSION, Severity::Info, "Linking with your friend...");
    }

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
        // Teardown: the user Left (or a new session superseded us). Leave the lobby and exit the thread.
        if stale(generation) {
            discovery.leave();
            if forward {
                crate::forward::set_enabled(false);
            }
            return;
        }

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

        // Client adopts the host's authoritative shared settings: apply *only* the shared subset into
        // the live config (a narrowed [`crate::state::update`], not a whole-config `set`), so a
        // concurrent menu write to a machine-local field isn't clobbered by the sync. The host has
        // nothing to adopt — its own config is authoritative.
        if !is_host {
            let cfg = session.peer().config();
            if *cfg != mirrored {
                mirrored = cfg.clone();
                let shared = SharedSettings::from(&mirrored);
                crate::state::update(move |c| shared.apply_to(c));
                toast(Severity::Info, CONFIG_SYNCED_MESSAGE);
            }
        }

        let was_linked = linked;
        update_link_status(&session, peer_id, &mut linked, &mut lost);
        if linked && !was_linked {
            // Handshake landed: mark the session connected and drop the setup banner (the connected
            // toast from `update_link_status` is the user-facing confirmation).
            SESSION.store(SESSION_CONNECTED, Ordering::Relaxed);
            clear_banner(BANNER_SESSION);
        }

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
        let compatible = PROTOCOL_VERSION.compatible_with(their_version);
        // Record the handshake stage + version verdict on the connect report, so a stuck-then-linked
        // attempt shows *when* it linked and whether the versions agree (not just the coarse phase).
        with_report(|r| {
            r.handshake_at = Some(elapsed());
            r.version = if compatible { VersionCheck::Match } else { VersionCheck::Mismatch };
        });
        toast(Severity::Info, format!("Co-op partner connected ({})", peer_tag(peer_id)));
        if !compatible {
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
        // Count only sends Steam accepted for delivery — paired with the received count below, a
        // `sent > 0, received = 0` report is the unambiguous one-way-NAT signature.
        if self.net.send_to(self.peer_id, bytes) {
            with_report(|r| r.messages_sent += 1);
        }
    }

    fn poll(&mut self) -> Vec<(PeerId, Vec<u8>)> {
        let frames = self.net.receive();
        // Count *all* frames that arrived on our channel, before the peer filter below: the report
        // question is "did any P2P traffic reach us at all" (the NAT/auth answer), not "did the right
        // peer reply". In a 2-player session these are the same, but counting pre-filter keeps the
        // received tally honest even if a stray frame slips through.
        if !frames.is_empty() {
            let n = frames.len() as u64;
            with_report(|r| r.messages_received += n);
        }
        // Trust boundary: only accept frames from the *configured* partner. Steam already won't
        // deliver from a user whose session we never accepted (we accept only `peer_id`), but that's
        // an implicit Steam semantic — make the 2-player invariant explicit here so a stray frame
        // from anyone else is dropped before it can reach the `Peer` (roster, actions, liveness).
        frames.into_iter().filter(|(from, _)| *from == self.peer_id).collect()
    }

    fn local_id(&self) -> PeerId {
        self.local_id
    }
}
