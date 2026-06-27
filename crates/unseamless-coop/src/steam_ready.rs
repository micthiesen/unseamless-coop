//! Process-global **Steam-readiness status** — the single gate the explicit lobby actions
//! (Open World / Join) hang off of. Until Steam networking is up there's no point letting the player
//! try to host or join, so we publish a coarse [`Status`] here and the overlay/orchestrator read it
//! non-blocking to enable or disable those actions, while a banner narrates the wait.
//!
//! Mirrors [`crate::playstate`] / [`unseamless_core::game_state`]: one process-global [`AtomicU8`]
//! holding the [`Status`] tag, published from one place (the [`start`] probe thread) and read
//! non-blocking from any thread via [`is_ready`] / [`status`]. `Relaxed` ordering is correct — no other
//! memory is published through it (same reasoning as `crate::playstate`). Defaults to
//! [`Status::Connecting`] so before the probe resolves we read "still coming up", never a fabricated
//! ready/failed state.
//!
//! ## Wiring (orchestrator)
//! The probe does not start itself. Add exactly this one line to `app::pre_task_startup`, right next to
//! the existing `crate::steam::start();`:
//!
//! ```ignore
//! crate::steam_ready::start();
//! ```
//!
//! That spot is correct because `app::install` runs `init_subsystems` (which calls
//! `crate::notify::init()`) *before* `pre_task_startup`, so the notifications store is already
//! initialized when the probe posts its "Connecting…" banner — no ordering care needed beyond keeping
//! it inside/after `pre_task_startup`.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use unseamless_core::notifications::Severity;

/// Stable banner id for the readiness condition. One id, updated in place: the "Connecting…" banner
/// becomes the error banner on failure, or is cleared on success.
const BANNER_STEAM_READY: &str = "steam-ready";

/// After the SteamID lands, Steam is up — so [`crate::steam::Networking::resolve`] and the lobby
/// interfaces ([`crate::steam::lobby_interfaces_available`]) should resolve almost immediately. Give
/// them a short retry window anyway in case an accessor is null for a beat (the same transient rung-1
/// sees), then call it failed.
const NETWORKING_TIMEOUT: Duration = Duration::from_secs(5);
const NETWORKING_POLL: Duration = Duration::from_millis(500);

/// Coarse Steam-readiness state, published from the [`start`] probe and read by the gated lobby
/// actions. Three states, no finer granularity than the gate needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum Status {
    /// The probe is still resolving identity + networking. The default before the probe finishes, so a
    /// pre-probe read reads "not yet", never a fabricated ready/failed.
    #[default]
    Connecting = 0,
    /// SteamID resolved, `ISteamNetworkingMessages` bound, and the lobby interfaces
    /// (`ISteamMatchmaking` + `ISteamUtils`) resolvable — co-op actions can be enabled.
    Ready = 1,
    /// The probe gave up (no SteamID, no networking, or the lobby interfaces never resolved within the
    /// timeout). Co-op stays disabled.
    Failed = 2,
}

impl Status {
    /// Stable `u8` tag for publishing through the atomic.
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Inverse of [`as_u8`](Status::as_u8); an unknown tag falls back to [`Status::Connecting`] (the
    /// safe default — "not ready yet"), so an unexpected tag never fabricates a ready state.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Status::Ready,
            2 => Status::Failed,
            _ => Status::Connecting,
        }
    }
}

static STATUS: AtomicU8 = AtomicU8::new(Status::Connecting as u8);

/// Publish the current readiness state. Called only from the [`start`] probe thread.
fn set(status: Status) {
    STATUS.store(status.as_u8(), Ordering::Relaxed);
}

/// The current Steam-readiness state, read non-blocking from any thread.
pub fn status() -> Status {
    Status::from_u8(STATUS.load(Ordering::Relaxed))
}

/// Whether Steam networking is up and co-op actions can be enabled. Thin wrapper over [`status`] for
/// the overlay's action gate.
pub fn is_ready() -> bool {
    status() == Status::Ready
}

/// Spawn the one-shot readiness probe on a short-lived detached thread (mirrors
/// [`crate::steam::start`]). It posts a "Connecting…" banner, waits for the rung-1 SteamID, then
/// confirms `ISteamNetworkingMessages` *and* the lobby interfaces (`ISteamMatchmaking` + `ISteamUtils`)
/// resolve; on all, publishes [`Status::Ready`] (clears the banner, brief success toast); otherwise
/// publishes [`Status::Failed`] (permanent error banner). The banner is diagnostic, so it stays in
/// **plain voice** (CLAUDE.md > "Surfacing errors").
pub fn start() {
    std::thread::spawn(|| {
        set_banner(Severity::Info, "Connecting to Steam...");

        if probe() {
            set(Status::Ready);
            clear_banner();
            crate::notify::toast(Severity::Info, "Steam networking ready.");
            log::info!("steam-ready: networking is up; co-op actions enabled");
        } else {
            set(Status::Failed);
            set_banner(
                Severity::Error,
                "Steam networking unavailable. Co-op is disabled. Is Steam running and logged in?",
            );
            log::warn!("steam-ready: Steam networking never came up; co-op actions disabled");
        }
    });
}

/// Wait for the rung-1 SteamID, then confirm networking + the lobby interfaces resolve. Returns
/// whether all succeeded within their timeouts. Runs entirely on the probe thread — the transient
/// interfaces it resolves (raw pointers, `!Send`) are created and dropped here, never crossing a
/// thread.
fn probe() -> bool {
    if crate::steam::wait_for_self_id(crate::steam::SELF_ID_TIMEOUT).is_none() {
        log::warn!(
            "steam-ready: own SteamID not resolved within {}s; treating Steam as unavailable",
            crate::steam::SELF_ID_TIMEOUT.as_secs()
        );
        return false;
    }
    wait_for_interfaces()
}

/// Poll until both [`crate::steam::Networking::resolve`] and the lobby interfaces
/// ([`crate::steam::lobby_interfaces_available`]) succeed, or [`NETWORKING_TIMEOUT`] elapses. The
/// resolved interfaces are dropped immediately — this is only a readiness check; the co-op driver
/// resolves its own when it needs them. Requiring the lobby interfaces here (not just networking)
/// keeps the gate honest: every interface an Open/Join action needs is confirmed before the actions
/// light up.
fn wait_for_interfaces() -> bool {
    let deadline = Instant::now() + NETWORKING_TIMEOUT;
    loop {
        if crate::steam::Networking::resolve().is_some()
            && crate::steam::lobby_interfaces_available()
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(NETWORKING_POLL);
    }
}

/// Set (or update) the readiness banner under [`BANNER_STEAM_READY`] — binds the fixed id, delegating
/// the push to the canonical [`crate::notify::set_banner`].
fn set_banner(severity: Severity, message: impl Into<String>) {
    crate::notify::set_banner(BANNER_STEAM_READY, severity, message);
}

/// Clear the readiness banner under [`BANNER_STEAM_READY`].
fn clear_banner() {
    crate::notify::clear_banner(BANNER_STEAM_READY);
}
