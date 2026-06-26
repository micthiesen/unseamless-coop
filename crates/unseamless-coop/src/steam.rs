//! Steamworks **flat C API** binding — resolved at runtime against the already-loaded
//! `steam_api64.dll`, never link-time. This is rung 1 of [`docs/COOP-CONNECTION.md`]: read our own
//! SteamID so two players can exchange IDs out of band (Discord) and, later (rung 2), open a private
//! Steam P2P side-channel to each other.
//!
//! ## Why hand-bind, not the `steamworks` crate
//! The crate (a) doesn't link on our `windows-gnu` cdylib target (`steam_api64` is MSVC-oriented —
//! steamworks-rs issue #274) and (b) assumes it *owns* Steam's callback dispatch, which an injected
//! DLL must not do — the game already called `SteamAPI_Init` and runs the dispatch loop; stealing it
//! steals the game's events. So we resolve a handful of flat exports by name (the
//! [`input.rs`](crate::input)/[`saves.rs`](crate::saves) pattern: `GetModuleHandleA` +
//! `GetProcAddress` against a DLL the process already loaded) and **never** touch lifecycle
//! (`SteamAPI_Init`/`Shutdown`/`RunCallbacks`). Rung 1 needs exactly two exports; the data path
//! (rung 2) uses the poll-based `ISteamNetworkingMessages`, which also needs no callback queue.
//!
//! ## Re-deriving the export names after a Steam update
//! The interface **accessor** carries a version suffix (`SteamAPI_SteamUser_v021`) that a Steam client
//! update can bump; the flat method (`SteamAPI_ISteamUser_GetSteamID`) is unversioned. Confirmed
//! against ELDEN RING's bundled DLL on 2026-06-25 (accessor `v021`). We probe a descending version
//! window rather than hardcode, so a bump self-heals; re-confirm the exact names with:
//! ```text
//! x86_64-w64-mingw32-objdump -p "ELDEN RING/Game/steam_api64.dll" | grep SteamAPI_SteamUser_v
//! ```
//!
//! Threading: [`start`] runs on its own short-lived thread (Steam comes up *after* our early
//! `dinput8` load, so it polls until ready); [`self_steam_id`] is read non-blocking from the overlay's
//! Present thread. The only shared state is one [`AtomicU64`].

use std::ffi::{CString, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::{PCSTR, s};

/// Our own 64-bit SteamID once resolved; `0` until then. `0` is not a valid individual SteamID (its
/// universe/type nibbles are non-zero — see [`is_plausible_steam_id`]), so it doubles as "unknown".
static SELF_STEAM_ID: AtomicU64 = AtomicU64::new(0);

/// Descending probe window for the versioned `SteamAPI_SteamUser_vNNN` accessor. The current export is
/// `v021`; the window brackets it generously so a future Steam SDK bump resolves without a code change.
/// First (highest) version that resolves wins — Valve normally exports a single accessor version, and
/// the newest is the right one if several are present.
const IFACE_VERSION_MIN: u32 = 10;
const IFACE_VERSION_MAX: u32 = 40;

/// Poll budget for [`start`]: Steam is initialized by the game (`SteamAPI_Init`) early in startup, but
/// we load as `dinput8.dll` *earlier*, so the accessor can return null for a moment. 60 × 500 ms = 30 s
/// is far more headroom than the title screen needs; success is typically the first or second attempt.
const QUERY_MAX_ATTEMPTS: u32 = 60;
const QUERY_RETRY_DELAY: Duration = Duration::from_millis(500);

/// `ISteamUser* SteamAPI_SteamUser_vNNN(void)` — the interface accessor. Returns null before
/// `SteamAPI_Init`. On x64 Windows there is a single calling convention, so `extern "C"` matches the
/// flat API's declared linkage exactly.
type SteamUserAccessor = unsafe extern "C" fn() -> *mut c_void;
/// `uint64 SteamAPI_ISteamUser_GetSteamID(ISteamUser* self)` — the flat wrapper returns the CSteamID as
/// a plain `uint64` (in RAX; no by-value struct ABI to worry about).
type GetSteamIdFn = unsafe extern "C" fn(*mut c_void) -> u64;

/// Our own SteamID, or `None` until Steam has been queried successfully. Non-blocking single atomic
/// load — safe to call every frame from the overlay's Present thread.
pub fn self_steam_id() -> Option<u64> {
    match SELF_STEAM_ID.load(Ordering::Relaxed) {
        0 => None,
        id => Some(id),
    }
}

/// Spawn the one-shot resolver thread. Polls the flat API until Steam is up, then publishes our
/// SteamID into [`SELF_STEAM_ID`] and logs it. Independent of the overlay kill-switch so the ID is
/// logged even when the overlay is off. Detached and short-lived (like the init/overlay threads).
pub fn start() {
    std::thread::spawn(|| {
        for attempt in 1..=QUERY_MAX_ATTEMPTS {
            match query_self() {
                Ok((id, version)) => {
                    SELF_STEAM_ID.store(id, Ordering::Relaxed);
                    log::info!("steam: own SteamID {id} (via SteamAPI_SteamUser_v{version:03})");
                    return;
                }
                // Expected early (Steam not initialized yet) — keep it at debug so it doesn't spam the
                // milestone log, then surface a single warning if it never comes up.
                Err(e) => log::debug!("steam: SteamID not ready (attempt {attempt}/{QUERY_MAX_ATTEMPTS}): {e}"),
            }
            std::thread::sleep(QUERY_RETRY_DELAY);
        }
        log::warn!(
            "steam: couldn't read own SteamID after {QUERY_MAX_ATTEMPTS} attempts; the identity panel stays blank"
        );
    });
}

/// Resolve the two exports and read our SteamID. Returns the ID and the accessor version that resolved
/// (for the log line). `Err` on any missing export, a null interface (Steam not up yet), or an
/// implausible ID — all of which [`start`] treats as "retry".
fn query_self() -> Result<(u64, u32), String> {
    // We never loaded steam_api64 ourselves — the game did (it's dynamically linked at load time via
    // the game's import table). `GetModuleHandleA` just borrows that already-mapped handle, never
    // triggering a DLL search; not-found means Steam hasn't loaded yet (retry).
    let module = unsafe { GetModuleHandleA(s!("steam_api64.dll")) }
        .map_err(|e| format!("steam_api64.dll not loaded: {e}"))?;

    let (accessor, version) = resolve_user_accessor(module)
        .ok_or_else(|| format!("no SteamAPI_SteamUser_v{IFACE_VERSION_MIN:03}..={IFACE_VERSION_MAX:03} accessor export"))?;
    let addr = unsafe { GetProcAddress(module, s!("SteamAPI_ISteamUser_GetSteamID")) }
        .ok_or_else(|| "SteamAPI_ISteamUser_GetSteamID export not found".to_string())? as usize;
    // SAFETY: the export's signature is the documented flat-API one; transmuting the resolved address
    // to it is the standard GetProcAddress binding (same as input.rs/saves.rs).
    let get_steam_id: GetSteamIdFn = unsafe { std::mem::transmute(addr) };

    // SAFETY: `accessor`/`get_steam_id` are resolved Steam exports; the accessor takes no args, and
    // GetSteamID takes the interface pointer it returns. We call no lifecycle/dispatch functions.
    let user = unsafe { accessor() };
    if user.is_null() {
        return Err("ISteamUser accessor returned null (Steam not initialized yet)".to_string());
    }
    let id = unsafe { get_steam_id(user) };
    if !is_plausible_steam_id(id) {
        return Err(format!("implausible SteamID {id:#018x} (not a logged-in individual account?)"));
    }
    Ok((id, version))
}

/// Probe `SteamAPI_SteamUser_vNNN` from the highest candidate version down, returning the first that
/// resolves. The `CString` per probe is cheap relative to the (rare, one-shot) resolve.
fn resolve_user_accessor(module: HMODULE) -> Option<(SteamUserAccessor, u32)> {
    for version in (IFACE_VERSION_MIN..=IFACE_VERSION_MAX).rev() {
        let name = CString::new(format!("SteamAPI_SteamUser_v{version:03}")).ok()?;
        // GetProcAddress wants a NUL-terminated `PCSTR`; `CString` provides exactly that. It must stay
        // alive across the call — it does, owned by `name` for this iteration.
        if let Some(proc) = unsafe { GetProcAddress(module, PCSTR(name.as_ptr() as *const u8)) } {
            // SAFETY: resolved export; the accessor's ABI is the flat-API one declared on the type.
            let accessor: SteamUserAccessor = unsafe { std::mem::transmute(proc as usize) };
            return Some((accessor, version));
        }
    }
    None
}

/// Sanity-gate a value returned by `GetSteamID`. A real **individual** SteamID64 packs a non-zero
/// universe (bits 56..63) and account type (bits 52..55) above the 32-bit account id, so the high
/// dword is always non-zero — which rules out `0` (our "unknown" sentinel) and small garbage that a
/// not-yet-logged-in Steam could otherwise hand back. Deliberately lenient (not a strict type check)
/// so an unusual-but-real account isn't rejected.
fn is_plausible_steam_id(id: u64) -> bool {
    (id >> 32) != 0
}
