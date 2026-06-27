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
//! cross-thread state is the live config ([`crate::state`], a `Mutex`), the notifications
//! ([`crate::notify`], a `Mutex`), the forward queue ([`crate::forward`]), the connect report
//! ([`REPORT`]/[`EPOCH`], `Mutex`es read by the debug panel), and a handful of relaxed atomics: the
//! link diagnostic [`PHASE`], and the user-facing session lifecycle [`SESSION`]/[`IS_HOST`] (read each
//! frame by the overlay's Present thread via [`session_flags`]) coordinated by [`SESSION_GEN`] (the
//! generation that signals an in-flight driver to tear down — see [`stale`]).
//!
//! ## Cadence is load-bearing
//! `pump` (receive) runs every [`POLL_INTERVAL`] for a responsive handshake; `maintain` runs every
//! [`MAINTAIN_INTERVAL`] (~1 Hz) because the `Peer`'s liveness/refill constants are denominated in
//! *maintain ticks* (≈30 ticks of silence before "Lost contact"; see `peer.rs`). Keep that ~1 s beat
//! stable or those wall-clock meanings drift.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use unseamless_core::config::Config;
use unseamless_core::diagnostics::{ConnectReport, LobbyProgress, LobbyRole, VersionCheck, peer_tag};
use unseamless_core::notifications::Severity;
use unseamless_core::peer::{
    CONFIG_SYNCED_MESSAGE, Peer, Session, lost_contact_message, version_mismatch_message,
};
use unseamless_core::protocol::{PROTOCOL_VERSION, SharedSettings};
use unseamless_core::transport::{PeerId, Transport};

use crate::notify::{clear_banner, set_banner, toast};
use crate::steam::{self, LobbyDiscovery, LobbyIntent, LobbyResult, Networking};

/// Poll the receive side this often (responsive handshake / config sync).
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Run `Session::maintain` this often — the heartbeat + host config re-assert + liveness sweep. The
/// `Peer`'s tick-denominated constants assume ~1 Hz here (see the module note).
const MAINTAIN_INTERVAL: Duration = Duration::from_secs(1);

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

/// User-facing session lifecycle tag stored in [`SESSION`]. Mirrors [`crate::steam_ready::Status`]'s
/// `as_u8`/`from_u8` shape so the `AtomicU8` keeps a stable wire of the variant rather than a bare
/// primitive the call sites have to remember the meaning of. (Named `SessionState` to avoid colliding
/// with the rung-2 [`Session`] transport type.)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
enum SessionState {
    /// Solo — no session.
    #[default]
    Off = 0,
    /// Host setting up / joiner searching / rung-2 linking.
    Connecting = 1,
    /// Host: lobby open, waiting for a friend.
    Hosting = 2,
    /// Partner linked.
    Connected = 3,
}

impl SessionState {
    /// Stable `u8` tag for publishing through [`SESSION`].
    fn as_u8(self) -> u8 {
        self as u8
    }

    /// Inverse of [`as_u8`](SessionState::as_u8); an unknown tag falls back to [`SessionState::Off`]
    /// (the safe default — "no session"), so an unexpected tag never fabricates an in-session state.
    fn from_u8(v: u8) -> Self {
        match v {
            1 => SessionState::Connecting,
            2 => SessionState::Hosting,
            3 => SessionState::Connected,
            _ => SessionState::Off,
        }
    }
}

static SESSION: AtomicU8 = AtomicU8::new(SessionState::Off as u8);
/// Whether the active session is ours to host (Open World) vs joined (Join world). Only meaningful when
/// [`SESSION`] is not [`SessionState::Off`].
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

/// The co-op session flags the menu gates on. A named struct (not a `(bool, bool)` tuple) so the two
/// same-typed fields can't be silently swapped at the call site — a swap there would invert every menu
/// gate (enable Open/Join in a session, Leave out of it).
#[derive(Debug, Clone, Copy)]
pub struct SessionFlags {
    /// In a session (hosting/joining/connected) — gates Open World/Join off and Leave on.
    pub in_session: bool,
    /// The active session is ours to host (Open World) vs joined (Join world). Only meaningful when
    /// `in_session`.
    pub is_host: bool,
}

/// The current session flags for the menu's `SessionContext`, read from the overlay's Present thread.
pub fn session_flags() -> SessionFlags {
    SessionFlags {
        in_session: SessionState::from_u8(SESSION.load(Ordering::Relaxed)) != SessionState::Off,
        is_host: IS_HOST.load(Ordering::Relaxed),
    }
}

/// Host setup budget: a host should reach an open lobby near-instantly (one existence-check list + a
/// `CreateLobby`); past this, Steam matchmaking is wedged. Once the lobby is open the timeout is
/// dropped (a host then waits for a friend indefinitely).
const HOST_SETUP_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a host waits in the open-and-waiting state before showing the one-time "both opened a
/// world?" nudge. Not a timeout — a legit host may wait far longer for a friend; this only updates the
/// banner. Sized well past the per-list retry + Valve indexing lag so a joiner who is simply still
/// searching has had ample time to appear before we suggest the swap.
const HOST_NUDGE_DELAY: Duration = Duration::from_secs(45);
/// Join search budget: short retry + give up (we only search for an already-open world). Past this, no
/// matching world was found — tell the user rather than spin forever.
const JOIN_TIMEOUT: Duration = Duration::from_secs(20);
/// Handshake budget once the partner is resolved: if the peer's `Hello` never arrives (one-way NAT,
/// peer crashed, P2P never opened), give up rather than sit "Linking…" forever. Generous headroom for a
/// slow P2P route to open.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Published connection phase for the diagnostic report's `coop` line (read from the game thread).
/// The driver is on its own thread, so this single relaxed atomic is the whole cross-thread readout.
/// Published connection-phase tag stored in [`PHASE`]. Same `as_u8`/`from_u8` shape as [`Session`] /
/// [`crate::steam_ready::Status`], so the diagnostic readout matches a named variant rather than a bare
/// `u8` the reader has to decode by hand.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
enum Phase {
    /// Discovery never started (no password / gated off) — driver never ran.
    #[default]
    Off = 0,
    /// Resolving identity + networking; partner not yet heard from.
    Linking = 1,
    /// Handshake complete (partner's Hello seen).
    Linked = 2,
    /// Was linked, partner went silent.
    Lost = 3,
    /// Discovery was attempted, but startup failed (no Steam ID / networking / timeout).
    Failed = 4,
}

impl Phase {
    /// Stable `u8` tag for publishing through [`PHASE`].
    fn as_u8(self) -> u8 {
        self as u8
    }

    /// Inverse of [`as_u8`](Phase::as_u8); an unknown tag falls back to [`Phase::Off`] (the safe
    /// default — "discovery never started").
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Phase::Linking,
            2 => Phase::Linked,
            3 => Phase::Lost,
            4 => Phase::Failed,
            _ => Phase::Off,
        }
    }
}

static PHASE: AtomicU8 = AtomicU8::new(Phase::Off as u8);

/// One-line co-op connection status for the diagnostic report's `coop` field (inside the `steam`
/// section), so a rig run (or a friend's shared log) shows at a glance whether the side-channel
/// linked up — and, crucially, distinguishes "not configured" from "configured but failed to start".
pub fn status_line() -> &'static str {
    match Phase::from_u8(PHASE.load(Ordering::Relaxed)) {
        Phase::Linking => "linking (Steam P2P)",
        Phase::Linked => "linked",
        Phase::Lost => "partner lost (silent)",
        Phase::Failed => "attempted, but couldn't start (see log)",
        Phase::Off => "off (no co-op password, or discovery disabled)",
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
/// Epoch the report's `+Ns` stage timings are relative to — re-pinned at the start of *each* attempt.
/// Relative ordering (which stage came when) is what diagnoses a stuck link; wall-clock isn't needed.
/// A `Mutex<Option<Instant>>` (not a `OnceLock`) so a second Open/Join after a Leave re-arms it instead
/// of measuring against the first attempt's epoch.
static EPOCH: Mutex<Option<Instant>> = Mutex::new(None);

/// Mark the start of a connection attempt: arm [`connect_report`], **reset the report**, and (re-)pin the
/// timing epoch. Connection is now repeatable per process (Leave → Open/Join), so each attempt starts
/// from a clean report + a fresh epoch — otherwise a retry's stage stamps read against the first
/// attempt's epoch and its `messages_sent/received` accumulate across attempts (misleading the one
/// shareable friend-test artifact).
fn begin_attempt() {
    ATTEMPTED.store(true, Ordering::Relaxed);
    with_report(|r| *r = ConnectReport::new());
    *EPOCH.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

/// Elapsed since [`begin_attempt`], for stamping a stage. `0` if somehow called before an attempt began
/// (shouldn't happen — every stage note follows `begin_attempt` — but keeps this total).
fn elapsed() -> Duration {
    EPOCH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map_or(Duration::ZERO, |t| t.elapsed())
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
// run on the rung-4 lobby-discovery path driven by [`host`]/[`join`].

/// Record which side of lobby discovery we're playing (creates the `lobby` sub-report). The role is the
/// user's choice (`Host` for Open World, `Joiner` for Join world), recorded once when discovery acts on
/// it. Idempotent for a repeated *same* role (a `CreateLobby` retry re-announces `Host`) so it doesn't
/// wipe earlier stamps.
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
    SESSION.store(SessionState::Connecting.as_u8(), Ordering::Relaxed);
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
    SESSION.store(SessionState::Off.as_u8(), Ordering::Relaxed);
    IS_HOST.store(false, Ordering::Relaxed);
    PHASE.store(Phase::Off.as_u8(), Ordering::Relaxed);
    clear_banner(BANNER_SESSION);
    clear_banner(BANNER_VERSION);
    clear_banner(BANNER_LIVENESS);
    toast(Severity::Info, "Left the session.");
}

/// Reset the published session state to "off" after a *failed* host/join attempt, toast the plain-words
/// reason, and clear the setup banner — so the menu re-enables Open World/Join for a retry. Distinct from
/// [`leave`] (a user-initiated end): this is the driver giving up. Does **not** bump the generation (the
/// driver is already exiting).
///
/// **No-op when [`stale`]:** if the session that captured `generation` was already ended (Leave) or
/// superseded (a new Open/Join), that newer session owns the published state, so a dying driver must not
/// clobber it back to "off". The guard lives here (not at each call site) so no caller can forget it —
/// the time-of-check/time-of-use gap between a caller's own `stale` check and these stores is what let a
/// timed-out driver reset a freshly-started session.
fn fail_session(generation: u64, why: impl Into<String>) {
    if stale(generation) {
        return;
    }
    let why = why.into();
    record_failure(why.clone());
    PHASE.store(Phase::Failed.as_u8(), Ordering::Relaxed);
    SESSION.store(SessionState::Off.as_u8(), Ordering::Relaxed);
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
    PHASE.store(Phase::Linking.as_u8(), Ordering::Relaxed);

    let Some(self_id) = wait_self_id(generation) else {
        // `None` means either we went stale (user left — `fail_session` then no-ops) or the SteamID
        // genuinely never resolved; the toast is correct only in the latter, which the guard handles.
        log::warn!("coop: own SteamID never resolved (or session left); lobby discovery not started");
        fail_session(generation, "Steam isn't ready (own SteamID never resolved) — is Steam running and logged in?");
        return;
    };
    with_report(|r| r.self_id_at = Some(elapsed()));

    let Some(mut discovery) = LobbyDiscovery::start(self_id, &password, intent) else {
        log::warn!("coop: Steam matchmaking unavailable; lobby discovery not started");
        fail_session(generation, "Steam matchmaking unavailable (Steam not up or an export is missing).");
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
    // First moment we entered the open-and-waiting state, to time the one-time nudge below.
    let mut hosting_since: Option<Instant> = None;
    let mut nudged = false;
    let (peer_id, is_host) = loop {
        if stale(generation) {
            return; // user left (or superseded): dropping `discovery` leaves the lobby (its Drop)
        }
        match discovery.poll() {
            LobbyResult::Resolved { peer, is_host } => break (peer, is_host),
            LobbyResult::Hosting => {
                // Lobby is open: stop the setup timeout and switch the banner to the waiting state, once.
                // Guard the store on the generation so a driver that went stale between the loop-top check
                // and here can't publish HOSTING over a newer session's state.
                if deadline.take().is_some() && !stale(generation) {
                    SESSION.store(SessionState::Hosting.as_u8(), Ordering::Relaxed);
                    set_banner(BANNER_SESSION, Severity::Info, "World open — waiting for a friend to join.");
                    hosting_since = Some(Instant::now());
                }
                // Soft one-time nudge: a host with no joiner after a good while may be the both-opened-a-
                // world case (if two friends both pick Open World, Valve indexing lag can leave both
                // hosting and waiting forever). Suggest one of them Join instead — but don't tear down (a
                // legit host may wait a long time). Generation-guarded like the HOSTING store so a stale
                // driver can't overwrite a newer session's banner.
                if !nudged
                    && hosting_since.is_some_and(|t| t.elapsed() >= HOST_NUDGE_DELAY)
                    && !stale(generation)
                {
                    nudged = true;
                    set_banner(
                        BANNER_SESSION,
                        Severity::Info,
                        "Still no one. If your friend also opened a world, one of you should Leave and Join instead.",
                    );
                }
            }
            LobbyResult::Pending => {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    fail_session(generation, match intent {
                        LobbyIntent::Host => "Couldn't open the world — Steam matchmaking timed out.",
                        LobbyIntent::Join => "No open world found with this password.",
                    });
                    return;
                }
            }
            LobbyResult::Failed(why) => {
                fail_session(generation, why);
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

/// RAII reset for client log-forwarding: created (set on) only for a forwarding client, it turns
/// forwarding back off on drop, so no run_session exit path — Leave, failure, or a panic unwinding the
/// driver thread — can leave the forward queue capturing with nothing to drain it.
struct ForwardGuard;
impl Drop for ForwardGuard {
    fn drop(&mut self) {
        crate::forward::set_enabled(false);
    }
}

/// The rung-2 driver: stand up a `Session` over Steam P2P to the resolved partner, then pump/maintain
/// it until the user Leaves (the captured `generation` goes [`stale`]). Holds `_discovery` purely for its
/// scope: its [`Drop`] leaves the Steam lobby on every exit path. A failure to *start* the transport
/// degrades via [`fail_session`] (toast + reset), never aborts.
fn run_session(
    self_id: PeerId,
    peer_id: PeerId,
    is_host: bool,
    forward: bool,
    generation: u64,
    // Retained (not referenced) only for its `Drop`, which leaves the Steam lobby on any exit below.
    _discovery: LobbyDiscovery,
) {
    // Every early return / loop exit below drops `_discovery`, whose `Drop` leaves the Steam lobby — so
    // teardown can't forget to leave it on any path (timeout, failure, Leave, or a panic unwinding the
    // driver thread). `fail_session` no-ops when the generation is stale, so these are safe to call
    // unconditionally.
    if self_id == peer_id {
        log::error!("coop: resolved partner is our own SteamID; nothing to connect to");
        fail_session(generation, "Resolved partner is our own SteamID — nothing to connect to.");
        return;
    }
    // The host (the lobby creator) is authoritative for the shared settings; the client adopts them.
    let host_id = if is_host { self_id } else { peer_id };

    let Some(net) = Networking::resolve() else {
        log::warn!("coop: ISteamNetworkingMessages unavailable; side-channel not started");
        fail_session(generation, "Steam networking unavailable — couldn't open the co-op channel.");
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
        Session::new(
            Peer::new(self_id, host_id, PROTOCOL_VERSION, crate::state::snapshot(), crate::config::fresh_auth_nonce()),
            transport,
        );
    session.connect();

    // Enable client log-forwarding for the session behind an RAII guard, so it's reset on *every* exit —
    // Leave, a panic unwinding the loop, anything — never left stuck on for the rest of the process.
    let _forward_guard = forward.then(|| {
        crate::forward::set_enabled(true);
        log::info!("coop: forwarding this client's debug log to the host");
        ForwardGuard
    });

    // Seed the change-detector with the config we started from, so only a *received* host ConfigSync
    // (which differs) drives a live-config write + "synced" toast — not the no-op initial seed.
    let mut mirrored = session.peer().config().clone();
    let mut last_maintain = Instant::now();
    // Bound the handshake: once the partner is resolved we expect its `Hello` promptly. If it never
    // arrives (one-way NAT, peer crashed, P2P never opened), don't sit "Linking…"/"waiting" forever —
    // give up with a plain-words toast so the user can retry instead of being stuck until they Leave.
    let connect_deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut linked = false;
    let mut lost = false;

    loop {
        // Teardown: the user Left (or a new session superseded us). Dropping `discovery` leaves the lobby
        // and `_forward_guard` resets forwarding; nothing else to undo.
        if stale(generation) {
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

        adopt_host_config(&session, is_host, &mut mirrored);

        let was_linked = linked;
        update_link_status(&session, peer_id, &mut linked, &mut lost);
        if linked && !was_linked && !stale(generation) {
            // Handshake landed: mark the session connected and drop the setup banner (the connected
            // toast from `update_link_status` is the user-facing confirmation). Guarded on the generation
            // so a superseded driver can't publish CONNECTED over a newer session's state.
            SESSION.store(SessionState::Connected.as_u8(), Ordering::Relaxed);
            clear_banner(BANNER_SESSION);
        }

        // Handshake never landed within budget: give up rather than hang. (Once linked, liveness — not
        // this deadline — governs; a transient drop is handled by the "lost contact" banner.)
        if !linked && Instant::now() >= connect_deadline {
            log::warn!("coop: partner {} never completed the handshake; giving up", peer_tag(peer_id));
            fail_session(generation, "Couldn't reach your friend — no response. Try again.");
            return;
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Client-side config adoption, run each loop tick: when the host pushes an updated authoritative
/// `ConfigSync`, apply *only* the shared subset into the live config (a narrowed
/// [`crate::state::update`], not a whole-config `set`), so a concurrent menu write to a machine-local
/// field isn't clobbered by the sync — then toast it once. `mirrored` is the change-detector seed,
/// updated in place so only a *changed* sync re-applies (the no-op initial seed never toasts). A no-op
/// for a host: its own config is authoritative, so it has nothing to adopt.
fn adopt_host_config(session: &Session<SteamP2PTransport>, is_host: bool, mirrored: &mut Config) {
    if is_host {
        return;
    }
    let cfg = session.peer().config();
    if *cfg != *mirrored {
        let shared = SharedSettings::from(cfg);
        *mirrored = cfg.clone();
        crate::state::update(move |c| shared.apply_to(c));
        toast(Severity::Info, CONFIG_SYNCED_MESSAGE);
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
        PHASE.store(Phase::Linked.as_u8(), Ordering::Relaxed);
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
                PHASE.store(Phase::Lost.as_u8(), Ordering::Relaxed);
                set_banner(BANNER_LIVENESS, Severity::Warning, lost_contact_message(peer_id));
            } else {
                PHASE.store(Phase::Linked.as_u8(), Ordering::Relaxed);
                clear_banner(BANNER_LIVENESS);
            }
        }
    }
}

/// Block until rung 1 publishes our own SteamID, or [`SELF_ID_TIMEOUT`] elapses, or the session goes
/// [`stale`] (the user Left / superseded while we waited) — all three return `None`. The stale check
/// stops a driver from lingering up to the full timeout after a Leave. In practice this returns near-
/// instantly: Open/Join are gated on Steam being ready, which already requires the SteamID resolved.
fn wait_self_id(generation: u64) -> Option<PeerId> {
    // The plain wait loop + its budget/cadence live in [`steam`] (shared with the readiness probe);
    // here we keep our own loop only to interleave the stale-session early-out, reusing those
    // constants so the two waiters can't drift.
    let deadline = Instant::now() + steam::SELF_ID_TIMEOUT;
    loop {
        if stale(generation) {
            return None;
        }
        if let Some(id) = steam::self_steam_id() {
            return Some(id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(steam::SELF_ID_POLL);
    }
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
