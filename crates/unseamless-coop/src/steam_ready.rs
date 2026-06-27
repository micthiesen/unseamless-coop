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

// Until the orchestrator wires `start()` into `app::install` and `is_ready()`/`status()` into the
// overlay's action gate, nothing in here is called yet. Drop this allow once that wiring lands.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use unseamless_core::notifications::{DEFAULT_TOAST_SECS, Severity};

/// Stable banner id for the readiness condition. One id, updated in place: the "Connecting…" banner
/// becomes the error banner on failure, or is cleared on success.
const BANNER_STEAM_READY: &str = "steam-ready";

/// How long to wait for the rung-1 SteamID before declaring failure. Sized just past rung 1's own
/// resolver budget (`steam::QUERY_MAX_ATTEMPTS * QUERY_RETRY_DELAY` = 30 s, after which
/// [`crate::steam::self_steam_id`] can never become non-zero): once that poller gives up there is
/// nothing left to wait for, so polling longer here would only spin uselessly. Matches
/// `crate::coop::SELF_ID_TIMEOUT`.
const SELF_ID_TIMEOUT: Duration = Duration::from_secs(35);
const SELF_ID_POLL: Duration = Duration::from_millis(250);

/// After the SteamID lands, Steam is up — so [`crate::steam::Networking::resolve`] should succeed
/// almost immediately. Give it a short retry window anyway in case the accessor is null for a beat
/// (the same transient rung-1 sees), then call it failed.
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
    /// SteamID resolved and `ISteamNetworkingMessages` bound — co-op actions can be enabled.
    Ready = 1,
    /// The probe gave up (no SteamID and/or no networking within the timeout). Co-op stays disabled.
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
/// confirms `ISteamNetworkingMessages` resolves; on both, publishes [`Status::Ready`] (clears the
/// banner, brief success toast); otherwise publishes [`Status::Failed`] (permanent error banner). The
/// banner is diagnostic, so it stays in **plain voice** (CLAUDE.md > "Surfacing errors").
pub fn start() {
    std::thread::spawn(|| {
        set_banner(Severity::Info, "Connecting to Steam…");

        if probe() {
            set(Status::Ready);
            clear_banner();
            toast(Severity::Info, "Steam networking ready.");
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

/// Wait for the rung-1 SteamID, then confirm networking resolves. Returns whether both succeeded
/// within their timeouts. Runs entirely on the probe thread — the transient
/// [`crate::steam::Networking`] it resolves (raw pointers, `!Send`) is created and dropped here,
/// never crossing a thread.
fn probe() -> bool {
    if !wait_for_self_id() {
        log::warn!(
            "steam-ready: own SteamID not resolved within {}s; treating Steam as unavailable",
            SELF_ID_TIMEOUT.as_secs()
        );
        return false;
    }
    wait_for_networking()
}

/// Poll [`crate::steam::self_steam_id`] until it resolves or [`SELF_ID_TIMEOUT`] elapses.
fn wait_for_self_id() -> bool {
    let deadline = Instant::now() + SELF_ID_TIMEOUT;
    loop {
        if crate::steam::self_steam_id().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SELF_ID_POLL);
    }
}

/// Poll [`crate::steam::Networking::resolve`] until it succeeds or [`NETWORKING_TIMEOUT`] elapses. The
/// resolved interface is dropped immediately — this is only a readiness check; the co-op driver
/// resolves its own when it needs one.
fn wait_for_networking() -> bool {
    let deadline = Instant::now() + NETWORKING_TIMEOUT;
    loop {
        if crate::steam::Networking::resolve().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(NETWORKING_POLL);
    }
}

fn toast(severity: Severity, message: impl Into<String>) {
    crate::notify::with_mut(|n| n.toast(severity, message, DEFAULT_TOAST_SECS));
}

fn set_banner(severity: Severity, message: impl Into<String>) {
    crate::notify::with_mut(|n| n.set_banner(BANNER_STEAM_READY, severity, message));
}

fn clear_banner() {
    crate::notify::with_mut(|n| {
        n.clear_banner(BANNER_STEAM_READY);
    });
}
