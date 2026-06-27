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

use std::ffi::{CString, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use unseamless_core::diagnostics::peer_tag;
use unseamless_core::protocol::PROTOCOL_VERSION;
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

// ===================================================================================================
// Rung 2: ISteamNetworkingMessages — the poll-based Steam P2P side-channel.
//
// The data path for the private side-channel ([`crate::coop`]). Same hand-bind discipline as rung 1
// above: resolve the flat exports by name against the already-loaded `steam_api64.dll`, never touch
// Steam's lifecycle/callback dispatch (the game owns it). `ISteamNetworkingMessages` is chosen
// precisely because its receive side is a **poll** (`ReceiveMessagesOnChannel`), so we never run the
// callback queue — sending to a user auto-opens the session, and because we know the peer out of band
// we also `AcceptSessionWithUser` proactively instead of waiting on the SessionRequest callback.
//
// Re-deriving the exports after a Steam update (per CLAUDE.md > "Document how to re-derive RE
// results"): the four flat methods are unversioned; only the interface accessor carries a version
// (`…_v002`, confirmed in ELDEN RING's bundled DLL on 2026-06-25). Re-confirm with:
//   x86_64-w64-mingw32-objdump -p "ELDEN RING/Game/steam_api64.dll" | grep SteamNetworkingMessages
// The struct layouts (`SteamNetworkingIdentity`, `SteamNetworkingMessage_t`) are public POD from the
// Steamworks SDK header `steamnetworkingtypes.h` — stable ABI, charted by offset below.

/// Our private side-channel's channel on `ISteamNetworkingMessages`. Distinctive and non-zero so it
/// can't collide with any channel the game itself might use (Elden Ring's netcode predates this API,
/// so a clash is unlikely regardless). Both peers agree because it's a constant in the one mod they
/// both run. (`"UC"` in ASCII.)
const SIDE_CHANNEL: i32 = 0x5543;
/// `k_nSteamNetworkingSend_Reliable` (steamnetworkingtypes.h). Our control frames are small and must
/// arrive (handshake, config sync); the reliable lane retransmits and auto-opens the session. The
/// host-tested `Peer` still tolerates loss/dup/reorder, so this is belt-and-suspenders, not a crutch.
const SEND_RELIABLE: i32 = 8;
/// `k_EResultOK` — `SendMessageToUser`'s success return.
const RESULT_OK: i32 = 1;
/// `k_ESteamNetworkingIdentityType_SteamID` — the identity-union discriminant we set/expect.
const IDENTITY_TYPE_STEAM_ID: i32 = 16;
/// How many messages to pull per `ReceiveMessagesOnChannel` call, and a hard cap on calls per poll
/// (`RECV_BATCH * RECV_MAX_CALLS` frames) so a flood can't pin the driver thread — the rest waits for
/// the next ~10 Hz poll. Far above our control-message rate.
const RECV_BATCH: usize = 32;
const RECV_MAX_CALLS: usize = 64;
/// Sanity ceiling on a single received frame before we allocate to copy it out. Our control frames
/// are tiny (the largest, a forwarded log, is `protocol::MAX_LOG_MSG` + a small header ≈ 2 KB); 8 KB
/// is generous headroom. A frame past this is dropped (the message is still released, just not
/// copied), so a buggy/hostile partner can't force a huge per-frame allocation ahead of the core
/// decoder's own validation.
const MAX_FRAME: i32 = 8 * 1024;

/// Descending probe window for the versioned accessor
/// `SteamAPI_SteamNetworkingMessages_SteamAPI_vNNN` (note the doubled `SteamAPI` — that *is* the
/// exported name). `v002` confirmed; the window self-heals a future bump, like the rung-1 accessor.
const NET_IFACE_VERSION_MIN: u32 = 1;
const NET_IFACE_VERSION_MAX: u32 = 10;

/// `SteamNetworkingIdentity` — the public POD from `steamnetworkingtypes.h`: a 136-byte tagged union
/// (`m_eType` @0, `m_cbSize` @4, then a 128-byte union @8), 8-aligned because the union holds a
/// `uint64`. We only ever populate/read the SteamID variant, so we build/parse it from the documented
/// layout directly rather than depend on the `SteamAPI_SteamNetworkingIdentity_*` helper exports
/// (fewer resolves that could be absent; the layout is stable ABI).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct SteamNetworkingIdentity {
    /// `m_eType` — [`IDENTITY_TYPE_STEAM_ID`] for the only variant we use.
    e_type: i32,
    /// `m_cbSize` — bytes of the active union member (`8` for a SteamID64).
    cb_size: i32,
    /// The 128-byte union; for a SteamID the first 8 bytes are the id, little-endian.
    union_data: [u8; 128],
}

impl SteamNetworkingIdentity {
    fn for_steam_id(id: u64) -> Self {
        let mut union_data = [0u8; 128];
        union_data[..8].copy_from_slice(&id.to_le_bytes());
        Self { e_type: IDENTITY_TYPE_STEAM_ID, cb_size: 8, union_data }
    }

    /// The SteamID64 if this identity is the SteamID variant, else `None` (a foreign identity type we
    /// don't speak — the message is dropped rather than misattributed to a phantom peer).
    fn steam_id(&self) -> Option<u64> {
        (self.e_type == IDENTITY_TYPE_STEAM_ID)
            .then(|| u64::from_le_bytes(self.union_data[..8].try_into().expect("8 of 128 bytes")))
    }
}

/// `SteamNetworkingMessage_t` — the received-message struct (`steamnetworkingtypes.h`). Steam
/// allocates and owns it; we read `m_pData`/`m_cbSize`/`m_identityPeer`, copy the payload out, then
/// hand it back via `m_pfnRelease` (the canonical free — the SDK's inline `Release()` just calls this
/// pointer). Charted in full so the release pointer at offset 184 lands correctly.
#[repr(C)]
struct SteamNetworkingMessage {
    data: *mut c_void,                      // m_pData        @0
    size: i32,                              // m_cbSize       @8
    conn: u32,                              // m_conn         @12
    identity_peer: SteamNetworkingIdentity, // m_identityPeer @16 (136)
    conn_user_data: i64,                    // m_nConnUserData       @152
    usec_time_received: i64,                // m_usecTimeReceived    @160
    message_number: i64,                    // m_nMessageNumber      @168
    pfn_free_data: Option<unsafe extern "C" fn(*mut SteamNetworkingMessage)>, // @176
    pfn_release: Option<unsafe extern "C" fn(*mut SteamNetworkingMessage)>,   // @184
    channel: i32,                           // m_nChannel     @192
    flags: i32,                             // m_nFlags       @196
    user_data: i64,                         // m_nUserData    @200
    idx_lane: u16,                          // m_idxLane      @208
    _pad: u16,                              // @210
}

// A wrong field offset here is silent UB at runtime (we deref Steam-owned memory by these offsets),
// so pin the load-bearing ones against the public `steamnetworkingtypes.h` layout at compile time —
// `offset_of!`/`size_of` are const, so this fails the build (even cross-compiling) if the structs
// ever skew from the documented ABI.
const _: () = {
    assert!(size_of::<SteamNetworkingIdentity>() == 136);
    assert!(size_of::<SteamNetworkingMessage>() == 216);
    assert!(std::mem::offset_of!(SteamNetworkingMessage, data) == 0);
    assert!(std::mem::offset_of!(SteamNetworkingMessage, size) == 8);
    assert!(std::mem::offset_of!(SteamNetworkingMessage, identity_peer) == 16);
    assert!(std::mem::offset_of!(SteamNetworkingMessage, pfn_release) == 184);
};

/// `ISteamNetworkingMessages* SteamAPI_SteamNetworkingMessages_SteamAPI_vNNN(void)`.
type NetMessagesAccessor = unsafe extern "C" fn() -> *mut c_void;
/// `EResult SendMessageToUser(self, const SteamNetworkingIdentity&, const void* data, uint32 len,
/// int sendFlags, int channel)` — the identity is by const-ref, i.e. a pointer in the ABI.
type SendMessageToUserFn = unsafe extern "C" fn(
    *mut c_void,
    *const SteamNetworkingIdentity,
    *const c_void,
    u32,
    i32,
    i32,
) -> i32;
/// `int ReceiveMessagesOnChannel(self, int channel, SteamNetworkingMessage_t** out, int maxMessages)`.
type ReceiveMessagesFn =
    unsafe extern "C" fn(*mut c_void, i32, *mut *mut SteamNetworkingMessage, i32) -> i32;
/// `bool AcceptSessionWithUser(self, const SteamNetworkingIdentity&)`.
type AcceptSessionFn = unsafe extern "C" fn(*mut c_void, *const SteamNetworkingIdentity) -> bool;

/// The resolved `ISteamNetworkingMessages` interface plus the flat methods we use. Created once on
/// the co-op driver thread ([`crate::coop`]) and used only there — it holds raw pointers, so it is
/// `!Send` and must never cross threads.
pub struct Networking {
    iface: *mut c_void,
    send: SendMessageToUserFn,
    receive: ReceiveMessagesFn,
    accept: AcceptSessionFn,
}

impl Networking {
    /// Resolve the interface accessor + flat methods against the already-loaded `steam_api64.dll`.
    /// `None` (with a logged reason) if Steam isn't up yet or an export is missing — the caller treats
    /// that as "side-channel unavailable" and degrades, never aborts. (We deliberately do **not** bind
    /// `CloseSessionWithUser`: there's no session teardown in rung 2 — the channel lives for the
    /// process — so resolving it would be dead weight; add it when teardown lands.)
    pub fn resolve() -> Option<Networking> {
        let module = match unsafe { GetModuleHandleA(s!("steam_api64.dll")) } {
            Ok(m) => m,
            Err(e) => {
                log::warn!("steam: steam_api64.dll not loaded ({e}); no networking");
                return None;
            }
        };
        let (accessor, version) = resolve_net_accessor(module)?;
        // SAFETY: `accessor` is a resolved Steam export taking no args; calling it is the documented
        // way to obtain the interface pointer. Null means Steam isn't initialized yet.
        let iface = unsafe { accessor() };
        if iface.is_null() {
            log::warn!("steam: networking accessor returned null (Steam not initialized yet)");
            return None;
        }
        // SAFETY: each resolved address has the documented flat-API signature on the type; transmuting
        // from `usize` is the standard GetProcAddress binding (same as rung 1 / input.rs / saves.rs).
        // A missing export is named in the log (the "re-derive after a Steam update" scenario the
        // module doc is for) rather than degrading anonymously.
        let send: SendMessageToUserFn = unsafe {
            std::mem::transmute(resolve_method(module, "SendMessageToUser", s!("SteamAPI_ISteamNetworkingMessages_SendMessageToUser"))?)
        };
        let receive: ReceiveMessagesFn = unsafe {
            std::mem::transmute(resolve_method(module, "ReceiveMessagesOnChannel", s!("SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel"))?)
        };
        let accept: AcceptSessionFn = unsafe {
            std::mem::transmute(resolve_method(module, "AcceptSessionWithUser", s!("SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser"))?)
        };
        log::info!("steam: networking ready (SteamAPI_SteamNetworkingMessages_SteamAPI_v{version:03})");
        Some(Networking { iface, send, receive, accept })
    }

    /// Proactively accept a P2P session with a known peer, so an incoming message establishes without
    /// us pumping the `SteamNetworkingMessagesSessionRequest_t` callback (we know who the peer is).
    pub fn accept_session(&self, peer: u64) -> bool {
        let identity = SteamNetworkingIdentity::for_steam_id(peer);
        // SAFETY: `self.accept` is the resolved flat method; `identity` is a valid SteamID identity.
        unsafe { (self.accept)(self.iface, &identity) }
    }

    /// Send one encoded frame to `peer` on our channel (reliable). Returns whether Steam accepted it
    /// for delivery; a non-OK result is logged at `debug` (the `Peer` self-heals) so a transient
    /// failure can't spam the log or feed back into log-forwarding.
    pub fn send_to(&self, peer: u64, bytes: &[u8]) -> bool {
        // Our control frames are tiny; a >4 GiB frame (impossible here) would truncate the u32 length
        // below. Pin the invariant so a future caller that hands in a huge buffer fails loudly in diag
        // rather than silently corrupting the frame length.
        debug_assert!(bytes.len() <= u32::MAX as usize, "side-channel frame exceeds u32 length");
        let identity = SteamNetworkingIdentity::for_steam_id(peer);
        // SAFETY: resolved flat method; `identity` outlives the call, `bytes` is a valid slice.
        let result = unsafe {
            (self.send)(
                self.iface,
                &identity,
                bytes.as_ptr() as *const c_void,
                bytes.len() as u32,
                SEND_RELIABLE,
                SIDE_CHANNEL,
            )
        };
        if result != RESULT_OK {
            log::debug!("steam: SendMessageToUser -> EResult {result} (peer {})", peer_tag(peer));
            return false;
        }
        true
    }

    /// Drain pending frames on our channel, each tagged with its real sender's SteamID. Copies every
    /// payload out and releases the Steam-owned message before returning, so nothing leaks and no
    /// pointer outlives this call. Messages from a non-SteamID identity are dropped.
    pub fn receive(&self) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        for _ in 0..RECV_MAX_CALLS {
            let mut msgs: [*mut SteamNetworkingMessage; RECV_BATCH] = [std::ptr::null_mut(); RECV_BATCH];
            // SAFETY: resolved flat method; `msgs` is a valid out-array of `RECV_BATCH` pointers.
            let n = unsafe { (self.receive)(self.iface, SIDE_CHANNEL, msgs.as_mut_ptr(), RECV_BATCH as i32) };
            if n <= 0 {
                break;
            }
            let n = (n as usize).min(RECV_BATCH);
            for &msg in &msgs[..n] {
                if msg.is_null() {
                    continue;
                }
                // SAFETY: a non-null message Steam just handed us. Read every field we need into owned
                // values FIRST (release frees `m_pData`, invalidating any borrow), then hand it back.
                unsafe {
                    let data = (*msg).data;
                    let size = (*msg).size;
                    let sender = (*msg).identity_peer.steam_id();
                    let release = (*msg).pfn_release;
                    if let Some(sender) = sender
                        && !data.is_null()
                        && size > 0
                        && size <= MAX_FRAME
                    {
                        let bytes =
                            std::slice::from_raw_parts(data as *const u8, size as usize).to_vec();
                        out.push((sender, bytes));
                    }
                    match release {
                        Some(release) => release(msg),
                        // Steam always populates m_pfnRelease; a null would leak this message + its
                        // payload for the process lifetime, so surface the anomaly rather than hide it.
                        None => log::debug!("steam: received message with null m_pfnRelease; leaked"),
                    }
                }
            }
            if n < RECV_BATCH {
                break; // fewer than a full batch => the channel is drained
            }
        }
        out
    }
}

/// `GetProcAddress` as a plain address, for transmuting to a typed flat-API fn. `None` if absent.
fn proc_addr(module: HMODULE, name: PCSTR) -> Option<usize> {
    unsafe { GetProcAddress(module, name) }.map(|p| p as usize)
}

/// Resolve one required flat method, logging *which* export is missing on failure (so a Steam SDK
/// rename names the culprit instead of degrading anonymously). `label` is the short method name for
/// the log; `name` is the full exported symbol.
fn resolve_method(module: HMODULE, label: &str, name: PCSTR) -> Option<usize> {
    let addr = proc_addr(module, name);
    if addr.is_none() {
        log::warn!("steam: networking export ISteamNetworkingMessages_{label} not found (Steam SDK rename?)");
    }
    addr
}

/// Probe `SteamAPI_SteamNetworkingMessages_SteamAPI_vNNN` from the highest candidate version down,
/// returning the first that resolves (and its version, for the log line) — mirrors the rung-1
/// [`resolve_user_accessor`].
fn resolve_net_accessor(module: HMODULE) -> Option<(NetMessagesAccessor, u32)> {
    for version in (NET_IFACE_VERSION_MIN..=NET_IFACE_VERSION_MAX).rev() {
        let name = CString::new(format!("SteamAPI_SteamNetworkingMessages_SteamAPI_v{version:03}")).ok()?;
        if let Some(addr) = proc_addr(module, PCSTR(name.as_ptr() as *const u8)) {
            // SAFETY: resolved export; the accessor's ABI is the flat-API one declared on the type.
            let accessor: NetMessagesAccessor = unsafe { std::mem::transmute(addr) };
            return Some((accessor, version));
        }
    }
    None
}

// ===================================================================================================
// Rung 4: Steam matchmaking lobbies — password-keyed discovery via the C++-ABI call-result path.
//
//   *** DORMANT — gated in `crate::coop::start` on `LOBBY_DISCOVERY_ENABLED`, off until a rig probe
//       confirms ELDEN RING pumps Steam via legacy `SteamAPI_RunCallbacks` (so our registered
//       call-results fire), not `ManualDispatch`. See docs/COOP-CONNECTION.md rung-4 build order. ***
//
// This REPLACES the manual `[coop] peer_steam_id` copy-paste: the host creates a public lobby tagged
// with `hash(password)`, the joiner filters the lobby list by that hash, joins, reads the host's
// SteamID, and that seeds the rung-2 side-channel (`crate::coop`). Lobbies are *discovery*, not a new
// transport — once the peer SteamID is known, rung 2 runs unchanged.
//
// ## Why this is a gnarlier bind than rungs 1-3
// Rungs 1-3 are poll-only, so they never touch Steam's callback dispatch. Lobbies are inherently
// **async**: `CreateLobby`/`RequestLobbyList`/`JoinLobby` each return a `SteamAPICall_t` and deliver
// the result via a **call-result** — a C++ `CCallbackBase*` object Steam invokes through a vtable. We
// hand-build that object (vtable + the ABI header fields) in Rust, register it via the flat
// `SteamAPI_RegisterCallResult`, and let the game's own `RunCallbacks` pump deliver it to us. We never
// pump dispatch ourselves (that would steal the game's events). The thunks are foreign→us FFI entry
// points, so each is `catch_unwind`-firewalled per docs/FFI-UNWIND-AUDIT.md.
//
// ## Clean-room note
// The `CCallbackBase` layout, the `SteamAPI_*` flat names, the callback ids, and the result-struct
// fields below are all from the **public** Steamworks SDK headers (`steam_api_common.h`,
// `isteammatchmaking.h`), which are public knowledge like the rest of the flat API we bind — not from
// disassembling anything. Re-derive the export names after a Steam update with:
//   x86_64-w64-mingw32-objdump -p "ELDEN RING/Game/steam_api64.dll" | grep -E 'RegisterCallResult|ISteamMatchmaking|SteamMatchmaking_v'
//
// RIG-VERIFY checklist (each marked inline too): the `SteamAPI_RegisterCallResult` export exists; the
// matchmaking accessor version; the `CCallbackBase` field offsets + the third vtable slot's semantics;
// the callback-struct packing; and the headline question — do our registered call-results fire at all
// under ER's pump.

/// A Steamworks async-call handle (`SteamAPICall_t`); `0` is `k_uAPICallInvalid`.
type SteamAPICall = u64;
const API_CALL_INVALID: SteamAPICall = 0;
/// `k_EResultOK` — success for `LobbyCreated_t::m_eResult` (same value as the networking `RESULT_OK`,
/// repeated with the lobby-specific name for legibility).
const E_RESULT_OK: i32 = 1;

/// `ELobbyType::k_ELobbyTypePublic` — listed in `RequestLobbyList`, so the password filter can find it.
const ELOBBY_TYPE_PUBLIC: i32 = 2;
/// `ELobbyComparison::k_ELobbyComparisonEqual` — exact-match the password-hash filter.
const ELOBBY_CMP_EQUAL: i32 = 0;
/// Two players per co-op lobby.
const LOBBY_MAX_MEMBERS: i32 = 2;

/// Lobby-data keys. `usc_pw` carries the password hash (the joiner filters on it); `usc_ver` carries
/// our protocol version (informational — a cross-version mismatch is caught later in the rung-2
/// handshake, not by the filter, so an old client can still find + report it).
const LOBBY_KEY_PASSWORD: &str = "usc_pw";
const LOBBY_KEY_VERSION: &str = "usc_ver";

/// Lobby call-result callback ids (`k_iSteamMatchmakingCallbacks` = 500, plus the per-struct offset
/// from `isteammatchmaking.h`). Steam routes a result to the registered object whose `m_iCallback`
/// matches, so these must be exact.
const CB_LOBBY_CREATED: i32 = 500 + 13; // LobbyCreated_t
const CB_LOBBY_MATCH_LIST: i32 = 500 + 10; // LobbyMatchList_t
const CB_LOBBY_ENTER: i32 = 500 + 4; // LobbyEnter_t

/// Descending probe window for `SteamAPI_SteamMatchmaking_vNNN` (current export `v009`; window
/// self-heals a future bump, like the rung-1/2 accessors). RIG-VERIFY the exact version with objdump.
const MM_IFACE_VERSION_MIN: u32 = 5;
const MM_IFACE_VERSION_MAX: u32 = 15;

// --- The three lobby call-result structs (public POD from `isteammatchmaking.h`) -------------------
// Named without the Steamworks `_t` suffix (Rust naming); the original names are in the comments. The
// SDK packs callback structs to 8 on 64-bit (`VALVE_CALLBACK_PACK_LARGE`), which matches Rust's natural
// `repr(C)` layout here — pinned by the compile-time asserts below.

/// `LobbyCreated_t` — host's `CreateLobby` result.
#[repr(C)]
struct LobbyCreated {
    result: i32, // m_eResult       @0
    lobby: u64,  // m_ulSteamIDLobby @8 (u64 8-aligns to 8 → 4 bytes padding at @4)
}

/// `LobbyMatchList_t` — joiner's `RequestLobbyList` result.
#[repr(C)]
struct LobbyMatchList {
    matching: u32, // m_nLobbiesMatching @0
}

/// `LobbyEnter_t` — joiner's `JoinLobby` result. We only read the lobby id (then `GetLobbyOwner`).
#[repr(C)]
struct LobbyEnter {
    lobby: u64,           // m_ulSteamIDLobby        @0
    chat_permissions: u32, // m_rgfChatPermissions   @8
    locked: bool,          // m_bLocked              @12
    enter_response: u32,   // m_EChatRoomEnterResponse @16
}

// RIG-VERIFY: these match the public-SDK pack(8) layout; a skew is silent UB (we read Steam-owned
// memory by these offsets), so pin them at compile time like the networking structs above.
const _: () = {
    assert!(size_of::<LobbyCreated>() == 16);
    assert!(std::mem::offset_of!(LobbyCreated, lobby) == 8);
    assert!(size_of::<LobbyMatchList>() == 4);
    assert!(size_of::<LobbyEnter>() == 24);
    assert!(std::mem::offset_of!(LobbyEnter, lobby) == 0);
};

// --- CCallbackBase: the C++ object Steam invokes through a vtable ----------------------------------
//
// `RegisterCallResult` takes a `CCallbackBase*` whose first bytes Steam reads/writes:
//   @0  vfptr             (the 3-entry vtable below)
//   @8  m_nCallbackFlags  (uint8 — Steam sets the "registered" bit)
//   @12 m_iCallback       (int   — the expected callback id, so Steam routes the right result)
// We append our own context (the boxed Rust handler) past offset 16; Steam never touches it.
//
// RIG-VERIFY: the public header's third *virtual* is `GetCallbackSizeBytes()` (returns the result
// struct size so the dispatcher knows how many bytes `pvParam` covers); `GetICallback()` is a
// *non-virtual* inline accessor over `m_iCallback`. docs/COOP-CONNECTION.md labels the third slot
// `GetICallback()` — same slot, the doc's shorthand. We implement it as `GetCallbackSizeBytes`
// (returning our stored size); confirm on the rig that the dispatcher is happy with this.

/// The 3-entry vtable shared by every [`CallResult`]. On Win64 the C++ member-call ABI and `extern "C"`
/// pass `this` in `rcx` identically, so plain `extern "C"` thunks match Steam's expected calls.
#[repr(C)]
struct CallbackVtable {
    /// `virtual void Run(void *pvParam)` — the plain-callback entry; unused for a call-result.
    run: unsafe extern "C" fn(*mut CallResult, *mut c_void),
    /// `virtual void Run(void *pvParam, bool bIOFailure, SteamAPICall_t hSteamAPICall)` — the
    /// call-result entry; this is the one Steam invokes when our async call completes.
    run_call_result: unsafe extern "C" fn(*mut CallResult, *mut c_void, bool, SteamAPICall),
    /// `virtual int GetCallbackSizeBytes()` — the result struct's size (see the RIG-VERIFY note above).
    get_callback_size_bytes: unsafe extern "C" fn(*mut CallResult) -> i32,
}

static CALLBACK_VTABLE: CallbackVtable = CallbackVtable {
    run: cr_run,
    run_call_result: cr_run_call_result,
    get_callback_size_bytes: cr_size,
};

/// A registered call-result: the ABI-fixed `CCallbackBase` header (Steam's first 16 bytes) followed by
/// our boxed handler. Heap-allocated and handed to Steam by raw pointer; reclaimed in
/// [`cr_run_call_result`] after it fires exactly once.
#[repr(C)]
struct CallResult {
    vtable: *const CallbackVtable, // @0  vfptr
    flags: u8,                     // @8  m_nCallbackFlags
    _pad: [u8; 3],                 // @9  (m_iCallback is 4-aligned)
    callback_id: i32,              // @12 m_iCallback
    // ---- past here Steam never reads ----
    expected_size: i32,
    handler: Box<dyn FnMut(*const c_void, bool) + Send>,
}

const _: () = {
    assert!(std::mem::offset_of!(CallResult, vtable) == 0);
    assert!(std::mem::offset_of!(CallResult, flags) == 8);
    assert!(std::mem::offset_of!(CallResult, callback_id) == 12);
};

/// Contain a panic at a call-result FFI boundary: Steam invokes these thunks from the game's pump
/// across `extern "C"`, so an unwind would cross into non-Rust frames (UB under `panic = "unwind"`).
/// Mirrors the `input.rs`/`saves.rs` firewall; logs once via the self-protecting `error_contained`.
fn cr_firewall(what: &str, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        crate::logger::error_contained(format_args!(
            "steam: lobby call-result thunk '{what}' panicked; suppressed at the FFI boundary"
        ));
    }
}

/// `Run(void*)` — plain callbacks aren't used for our call-results; firewall + ignore (it's still an
/// FFI entry, and unlike the call-result path it must NOT free the object — it can fire repeatedly).
unsafe extern "C" fn cr_run(_this: *mut CallResult, _param: *mut c_void) {
    cr_firewall("Run(callback)", || {});
}

/// `Run(void*, bool, SteamAPICall_t)` — the call-result fires here. Dispatch to the boxed handler, then
/// reclaim the heap object (a call-result fires exactly once, after which Steam is done with it).
unsafe extern "C" fn cr_run_call_result(
    this: *mut CallResult,
    param: *mut c_void,
    io_failure: bool,
    _call: SteamAPICall,
) {
    cr_firewall("Run(call-result)", || {
        if this.is_null() {
            return;
        }
        // SAFETY: `this` is the object we registered (still alive — only freed below, after this).
        let cr = unsafe { &mut *this };
        (cr.handler)(param as *const c_void, io_failure);
    });
    if !this.is_null() {
        // Reclaim is also firewalled: dropping the box drops the boxed handler, and a panicking `Drop`
        // would otherwise unwind across this `extern "C"` boundary. Separate firewall (not one wrapping
        // both) so the object is still freed even if the handler above panicked.
        // SAFETY: we created `this` with `Box::into_raw` in `register_call_result`; reclaim it once.
        cr_firewall("call-result reclaim", || unsafe { drop(Box::from_raw(this)) });
    }
}

/// `GetCallbackSizeBytes()` — the registered result struct's size.
unsafe extern "C" fn cr_size(this: *mut CallResult) -> i32 {
    if this.is_null() {
        return 0;
    }
    // SAFETY: `this` is one of our live `CallResult`s.
    unsafe { (*this).expected_size }
}

/// `void SteamAPI_RegisterCallResult(CCallbackBase *pCallback, SteamAPICall_t hAPICall)`.
type RegisterCallResultFn = unsafe extern "C" fn(*mut c_void, SteamAPICall);

/// Heap-allocate a [`CallResult`] for `callback_id`/`expected_size` wrapping `handler`, and register it
/// against the pending `call`. The object stays alive (leaked into Steam's keeping) until its result
/// fires and [`cr_run_call_result`] frees it. A never-firing call (the dormant/blocked case) leaks one
/// small object for the process lifetime — acceptable.
fn register_call_result(
    register: RegisterCallResultFn,
    call: SteamAPICall,
    callback_id: i32,
    expected_size: i32,
    handler: Box<dyn FnMut(*const c_void, bool) + Send>,
) {
    let cr = Box::new(CallResult {
        vtable: &CALLBACK_VTABLE as *const CallbackVtable,
        flags: 0,
        _pad: [0; 3],
        callback_id,
        expected_size,
        handler,
    });
    let raw = Box::into_raw(cr);
    // SAFETY: `register` is the resolved flat export; `raw` is a valid CCallbackBase-shaped object that
    // outlives the pending call (freed only when the result fires). us → Steam; not an unwind boundary.
    unsafe { register(raw as *mut c_void, call) };
}

// --- ISteamMatchmaking flat methods ----------------------------------------------------------------

type MatchmakingAccessor = unsafe extern "C" fn() -> *mut c_void;
type CreateLobbyFn = unsafe extern "C" fn(*mut c_void, i32, i32) -> SteamAPICall;
type SetLobbyDataFn = unsafe extern "C" fn(*mut c_void, u64, *const c_char, *const c_char) -> bool;
type AddListStringFilterFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, i32);
type RequestLobbyListFn = unsafe extern "C" fn(*mut c_void) -> SteamAPICall;
type GetLobbyByIndexFn = unsafe extern "C" fn(*mut c_void, i32) -> u64;
type JoinLobbyFn = unsafe extern "C" fn(*mut c_void, u64) -> SteamAPICall;
type GetLobbyOwnerFn = unsafe extern "C" fn(*mut c_void, u64) -> u64;
type GetNumLobbyMembersFn = unsafe extern "C" fn(*mut c_void, u64) -> i32;
type GetLobbyMemberByIndexFn = unsafe extern "C" fn(*mut c_void, u64, i32) -> u64;

/// The resolved `ISteamMatchmaking` interface + the flat methods rung 4 uses. The raw `iface` pointer
/// makes this `!Send`/`!Sync` by default, but the lobby flow spans threads (the call-result handlers
/// run on the game's pump thread; the host's member poll runs on the co-op driver thread), and Steam's
/// interface methods are internally thread-safe — so we assert `Send`/`Sync` and only ever *read*
/// `iface` to pass it to those thread-safe flat methods (never mutate the pointee).
struct Matchmaking {
    iface: *mut c_void,
    create_lobby: CreateLobbyFn,
    set_lobby_data: SetLobbyDataFn,
    add_filter: AddListStringFilterFn,
    request_list: RequestLobbyListFn,
    get_by_index: GetLobbyByIndexFn,
    join_lobby: JoinLobbyFn,
    get_lobby_owner: GetLobbyOwnerFn,
    get_num_members: GetNumLobbyMembersFn,
    get_member_by_index: GetLobbyMemberByIndexFn,
}

// SAFETY: see the `Matchmaking` doc — Steam's flat matchmaking methods are thread-safe and we only read
// `iface`. The function pointers are themselves `Send`/`Sync`.
unsafe impl Send for Matchmaking {}
unsafe impl Sync for Matchmaking {}

impl Matchmaking {
    /// Resolve the accessor + every flat method we need. `None` (logged) if Steam isn't up or any export
    /// is missing — the caller treats that as "lobby discovery unavailable" and degrades.
    fn resolve(module: HMODULE) -> Option<Matchmaking> {
        let (accessor, version) = resolve_matchmaking_accessor(module)?;
        // SAFETY: resolved accessor taking no args; null means Steam isn't initialized yet.
        let iface = unsafe { accessor() };
        if iface.is_null() {
            log::warn!("steam: matchmaking accessor returned null (Steam not initialized yet)");
            return None;
        }
        // RIG-VERIFY: flat names from the public Steamworks flat API; confirm against ER's bundled dll
        // (`objdump -p … | grep ISteamMatchmaking`). A missing one is named in the log below.
        // SAFETY (each transmute): the resolved address has the documented flat-API signature declared
        // on its fn type — the standard GetProcAddress binding used throughout this module.
        let create_lobby = unsafe { std::mem::transmute::<usize, CreateLobbyFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_CreateLobby", s!("SteamAPI_ISteamMatchmaking_CreateLobby"))?) };
        let set_lobby_data = unsafe { std::mem::transmute::<usize, SetLobbyDataFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_SetLobbyData", s!("SteamAPI_ISteamMatchmaking_SetLobbyData"))?) };
        let add_filter = unsafe { std::mem::transmute::<usize, AddListStringFilterFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_AddRequestLobbyListStringFilter", s!("SteamAPI_ISteamMatchmaking_AddRequestLobbyListStringFilter"))?) };
        let request_list = unsafe { std::mem::transmute::<usize, RequestLobbyListFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_RequestLobbyList", s!("SteamAPI_ISteamMatchmaking_RequestLobbyList"))?) };
        let get_by_index = unsafe { std::mem::transmute::<usize, GetLobbyByIndexFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_GetLobbyByIndex", s!("SteamAPI_ISteamMatchmaking_GetLobbyByIndex"))?) };
        let join_lobby = unsafe { std::mem::transmute::<usize, JoinLobbyFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_JoinLobby", s!("SteamAPI_ISteamMatchmaking_JoinLobby"))?) };
        let get_lobby_owner = unsafe { std::mem::transmute::<usize, GetLobbyOwnerFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_GetLobbyOwner", s!("SteamAPI_ISteamMatchmaking_GetLobbyOwner"))?) };
        let get_num_members = unsafe { std::mem::transmute::<usize, GetNumLobbyMembersFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_GetNumLobbyMembers", s!("SteamAPI_ISteamMatchmaking_GetNumLobbyMembers"))?) };
        let get_member_by_index = unsafe { std::mem::transmute::<usize, GetLobbyMemberByIndexFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_GetLobbyMemberByIndex", s!("SteamAPI_ISteamMatchmaking_GetLobbyMemberByIndex"))?) };
        log::info!("steam: matchmaking ready (SteamAPI_SteamMatchmaking_v{version:03})");
        Some(Matchmaking {
            iface,
            create_lobby,
            set_lobby_data,
            add_filter,
            request_list,
            get_by_index,
            join_lobby,
            get_lobby_owner,
            get_num_members,
            get_member_by_index,
        })
    }
}

/// The shared matchmaking binding, parked where the `'static` call-result handlers reach it across
/// threads. Set once by [`LobbyDiscovery::start`].
static MATCHMAKING: OnceLock<Matchmaking> = OnceLock::new();
/// The other player's resolved SteamID — the rung-2 peer (joiner→host via `GetLobbyOwner`, host→joiner
/// via member poll). `0` until resolved.
static DISCOVERED_PEER: AtomicU64 = AtomicU64::new(0);
/// Host only: our created lobby, so the driver thread can poll its members for the joiner. `0` until
/// `LobbyCreated_t` lands.
static HOST_LOBBY_ID: AtomicU64 = AtomicU64::new(0);
/// Joiner only: the lobby we entered, so the driver can poll `GetLobbyOwner` until it's populated (owner
/// metadata can lag the `LobbyEnter_t` by a beat). `0` until we've joined.
static JOINED_LOBBY_ID: AtomicU64 = AtomicU64::new(0);
/// Joiner only: a `RequestLobbyList` is outstanding, so the driver's retry doesn't issue overlapping
/// requests (the SDK allows only one in flight). Cleared when its `LobbyMatchList_t` lands.
static JOINER_REQUEST_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// Joiner: re-issue `RequestLobbyList` this often until a match is found. The host tags its lobby only
/// *after* `CreateLobby` completes, so a joiner that starts first must retry rather than latch the first
/// empty result as failure — the `run_discovery` timeout is the real deadline.
const LOBBY_LIST_RETRY: Duration = Duration::from_secs(2);

/// Drives password-keyed lobby discovery to resolve the rung-2 peer. Construct with [`start`]; then the
/// co-op driver calls [`poll`](LobbyDiscovery::poll) each tick until it yields the peer SteamID.
pub struct LobbyDiscovery {
    is_host: bool,
    mm: &'static Matchmaking,
    register: RegisterCallResultFn,
    /// Joiner: the password filter value, re-applied on each list retry (the filter is consumed per
    /// request).
    key: String,
    /// Joiner: when we last issued a `RequestLobbyList`, to pace retries.
    last_request: Option<Instant>,
}

impl LobbyDiscovery {
    /// Bind matchmaking + the call-result register, and kick off the role's first async call (host:
    /// `CreateLobby`; joiner: filter + `RequestLobbyList`), registering the call-result that advances
    /// the flow. `None` (logged) if the Steam binding isn't available.
    pub fn start(is_host: bool, password: &str) -> Option<LobbyDiscovery> {
        // SAFETY: borrow the already-loaded module handle (same as rung 1/2); not-found means Steam
        // hasn't loaded yet.
        let module = match unsafe { GetModuleHandleA(s!("steam_api64.dll")) } {
            Ok(m) => m,
            Err(e) => {
                log::warn!("steam: steam_api64.dll not loaded ({e}); no lobby discovery");
                return None;
            }
        };
        // RIG-VERIFY: that `SteamAPI_RegisterCallResult` is even exported by ER's dll (it's part of the
        // flat API, but confirm with objdump) — and the headline unknown, that the call-results we
        // register actually fire under ER's `RunCallbacks` pump.
        let register: RegisterCallResultFn = unsafe {
            std::mem::transmute::<usize, RegisterCallResultFn>(resolve_required(module, "SteamAPI_RegisterCallResult", s!("SteamAPI_RegisterCallResult"))?)
        };
        let mm = Matchmaking::resolve(module)?;
        // Park the binding for the 'static handlers; ignore "already set" (a second start is a no-op).
        let _ = MATCHMAKING.set(mm);
        let mm: &'static Matchmaking = MATCHMAKING.get()?;

        let key = password_lobby_key(password);
        let mut discovery = LobbyDiscovery { is_host, mm, register, key, last_request: None };
        if is_host {
            // SAFETY: resolved flat method; us → Steam.
            let call = unsafe { (mm.create_lobby)(mm.iface, ELOBBY_TYPE_PUBLIC, LOBBY_MAX_MEMBERS) };
            register_lobby_created(register, mm, call, discovery.key.clone());
        } else {
            // First filtered list request; the driver re-issues it on a timer until a match appears.
            discovery.request_list_now();
        }
        Some(discovery)
    }

    /// Poll for the resolved rung-2 peer (`None` until it lands). Called each tick by the co-op driver.
    pub fn poll(&mut self) -> Option<u64> {
        if let Some(peer) = peer_or_none() {
            return Some(peer);
        }
        if self.is_host { self.poll_host() } else { self.poll_joiner() }
    }

    /// Host: once our lobby exists, scan its members (poll-based, no extra callback) for the non-self
    /// entry — that's the joiner, our rung-2 peer.
    fn poll_host(&self) -> Option<u64> {
        let lobby = HOST_LOBBY_ID.load(Ordering::Acquire);
        if lobby == 0 {
            return None; // our lobby isn't created yet
        }
        let mm = MATCHMAKING.get()?;
        // Need our own id to exclude ourselves from the member list; if rung 1 hasn't published it yet,
        // skip this scan (don't fall back to `0`, which would mis-pick our own membership as the peer).
        let me = self_steam_id()?;
        // SAFETY: resolved flat methods; `iface` read-only (see the `Matchmaking` thread-safety note).
        let count = unsafe { (mm.get_num_members)(mm.iface, lobby) };
        for i in 0..count {
            let member = unsafe { (mm.get_member_by_index)(mm.iface, lobby, i) };
            if member != 0 && member != me {
                DISCOVERED_PEER.store(member, Ordering::Release);
                crate::coop::note_lobby_host_resolved();
                return Some(member);
            }
        }
        None
    }

    /// Joiner: once joined, resolve the host from `GetLobbyOwner` (owner metadata can lag `LobbyEnter`,
    /// so we poll until it's non-zero rather than treating a transient `0` as failure). Until joined,
    /// re-issue the filtered `RequestLobbyList` on a timer.
    fn poll_joiner(&mut self) -> Option<u64> {
        let lobby = JOINED_LOBBY_ID.load(Ordering::Acquire);
        if lobby != 0 {
            let mm = MATCHMAKING.get()?;
            // SAFETY: resolved flat method; `iface` read-only.
            let owner = unsafe { (mm.get_lobby_owner)(mm.iface, lobby) };
            if owner != 0 {
                DISCOVERED_PEER.store(owner, Ordering::Release);
                crate::coop::note_lobby_host_resolved();
                return Some(owner);
            }
            return None; // owner not populated yet; poll again
        }
        // Not joined yet — (re)issue the filtered list request on an interval, unless one is in flight.
        let due = self.last_request.is_none_or(|t| t.elapsed() >= LOBBY_LIST_RETRY);
        if due && !JOINER_REQUEST_IN_FLIGHT.load(Ordering::Acquire) {
            self.request_list_now();
        }
        None
    }

    /// Joiner: apply the password filter and issue one `RequestLobbyList`, marking a request outstanding.
    fn request_list_now(&mut self) {
        self.last_request = Some(Instant::now());
        joiner_request_list(self.register, self.mm, &self.key);
    }
}

/// The resolved peer SteamID, or `None` while still `0`.
fn peer_or_none() -> Option<u64> {
    match DISCOVERED_PEER.load(Ordering::Acquire) {
        0 => None,
        id => Some(id),
    }
}

/// Host: register the `LobbyCreated_t` handler — on success, stash the lobby id, publish the password +
/// version data so a joiner's filter finds us, and mark the stage.
fn register_lobby_created(register: RegisterCallResultFn, mm: &'static Matchmaking, call: SteamAPICall, key: String) {
    if call == API_CALL_INVALID {
        crate::coop::note_lobby_failure("CreateLobby returned no API call (Steam refused it)");
        return;
    }
    let handler = Box::new(move |param: *const c_void, io_failure: bool| {
        if io_failure || param.is_null() {
            crate::coop::note_lobby_failure("LobbyCreated_t reported an IO failure");
            return;
        }
        // SAFETY: for this callback id Steam hands us a `LobbyCreated_t`; we read it as POD.
        let res = unsafe { &*(param as *const LobbyCreated) };
        if res.result != E_RESULT_OK {
            crate::coop::note_lobby_failure(format!("CreateLobby failed (EResult {})", res.result));
            return;
        }
        HOST_LOBBY_ID.store(res.lobby, Ordering::Release);
        set_lobby_data(mm, res.lobby, LOBBY_KEY_PASSWORD, &key);
        set_lobby_data(mm, res.lobby, LOBBY_KEY_VERSION, &PROTOCOL_VERSION.to_string());
        crate::coop::note_lobby_created();
        crate::coop::note_lobby_host_resolved(); // the host id is our own — trivially known
    });
    register_call_result(register, call, CB_LOBBY_CREATED, size_of::<LobbyCreated>() as i32, handler);
}

/// Joiner: apply the password filter and request the lobby list, registering the `LobbyMatchList_t`
/// handler. The string filter is consumed per request, so re-apply it on every (retry) call.
fn joiner_request_list(register: RegisterCallResultFn, mm: &'static Matchmaking, key: &str) {
    let (Ok(fk), Ok(fv)) = (CString::new(LOBBY_KEY_PASSWORD), CString::new(key.to_string())) else {
        crate::coop::note_lobby_failure("could not marshal the lobby password filter");
        return;
    };
    JOINER_REQUEST_IN_FLIGHT.store(true, Ordering::Release);
    // SAFETY: resolved flat methods; the CStrings outlive the calls.
    unsafe { (mm.add_filter)(mm.iface, fk.as_ptr(), fv.as_ptr(), ELOBBY_CMP_EQUAL) };
    let call = unsafe { (mm.request_list)(mm.iface) };
    register_lobby_match_list(register, mm, call);
}

/// Joiner: register the `LobbyMatchList_t` handler — pick the first matching lobby and join it. An empty
/// list (or a refused request) is **not** terminal: clear the in-flight flag so the driver retries (the
/// host may not have created its lobby yet); the discovery timeout is the real deadline, and the report
/// keeps the last `candidates` count so a persistent empty filter is still legible.
fn register_lobby_match_list(register: RegisterCallResultFn, mm: &'static Matchmaking, call: SteamAPICall) {
    if call == API_CALL_INVALID {
        JOINER_REQUEST_IN_FLIGHT.store(false, Ordering::Release); // let the driver retry
        return;
    }
    let handler = Box::new(move |param: *const c_void, io_failure: bool| {
        JOINER_REQUEST_IN_FLIGHT.store(false, Ordering::Release); // this request completed
        if io_failure || param.is_null() {
            return; // transient; the driver retries
        }
        // SAFETY: for this callback id Steam hands us a `LobbyMatchList_t`.
        let res = unsafe { &*(param as *const LobbyMatchList) };
        crate::coop::note_lobby_list(res.matching);
        if res.matching == 0 {
            return; // empty this round — the driver retries until the timeout
        }
        // SAFETY: resolved flat method; index 0 is in range (matching >= 1).
        let lobby = unsafe { (mm.get_by_index)(mm.iface, 0) };
        if lobby == 0 {
            return; // odd, but non-terminal; the driver retries
        }
        let join_call = unsafe { (mm.join_lobby)(mm.iface, lobby) };
        register_lobby_enter(register, join_call);
    });
    register_call_result(register, call, CB_LOBBY_MATCH_LIST, size_of::<LobbyMatchList>() as i32, handler);
}

/// Joiner: register the `LobbyEnter_t` handler — once in, publish the joined lobby so the driver can
/// resolve the host owner from it (the host read is done on the poll side to tolerate the owner-metadata
/// lag right after entering).
fn register_lobby_enter(register: RegisterCallResultFn, call: SteamAPICall) {
    if call == API_CALL_INVALID {
        crate::coop::note_lobby_failure("JoinLobby returned no API call (Steam refused it)");
        return;
    }
    let handler = Box::new(move |param: *const c_void, io_failure: bool| {
        if io_failure || param.is_null() {
            crate::coop::note_lobby_failure("LobbyEnter_t reported an IO failure");
            return;
        }
        // SAFETY: for this callback id Steam hands us a `LobbyEnter_t`.
        let res = unsafe { &*(param as *const LobbyEnter) };
        crate::coop::note_lobby_joined();
        JOINED_LOBBY_ID.store(res.lobby, Ordering::Release);
    });
    register_call_result(register, call, CB_LOBBY_ENTER, size_of::<LobbyEnter>() as i32, handler);
}

/// `SetLobbyData(lobby, key, value)` with the strings marshaled to C. Best-effort (a non-UTF-safe key
/// can't occur here — they're our constants — and a Steam-side failure just means the joiner won't find
/// us, which surfaces as the joiner's empty-filter report).
fn set_lobby_data(mm: &Matchmaking, lobby: u64, key: &str, value: &str) -> bool {
    let (Ok(k), Ok(v)) = (CString::new(key), CString::new(value)) else {
        return false;
    };
    // SAFETY: resolved flat method; the CStrings outlive the call.
    unsafe { (mm.set_lobby_data)(mm.iface, lobby, k.as_ptr(), v.as_ptr()) }
}

/// The lobby-discovery password token — the `usc_pw` value a host publishes and a joiner filters on.
/// This is the cross-implementation **contract** with the `harness` prototype, so it lives in
/// host-tested core ([`unseamless_core::diagnostics::lobby_discovery_token`], pinned by a known-answer
/// test the harness mirrors) rather than being re-derived here — the two must be byte-identical or
/// discovery silently fails. The password is keyed **verbatim**: we pass the raw configured bytes with
/// no trim/case-fold/normalization (a stray normalize would break agreement).
fn password_lobby_key(password: &str) -> String {
    unseamless_core::diagnostics::lobby_discovery_token(password)
}

/// Probe `SteamAPI_SteamMatchmaking_vNNN` highest-first — mirrors the rung-1/2 accessor probes.
fn resolve_matchmaking_accessor(module: HMODULE) -> Option<(MatchmakingAccessor, u32)> {
    for version in (MM_IFACE_VERSION_MIN..=MM_IFACE_VERSION_MAX).rev() {
        let name = CString::new(format!("SteamAPI_SteamMatchmaking_v{version:03}")).ok()?;
        if let Some(addr) = proc_addr(module, PCSTR(name.as_ptr() as *const u8)) {
            // SAFETY: resolved export; the accessor's ABI is the flat-API one declared on the type.
            let accessor: MatchmakingAccessor = unsafe { std::mem::transmute(addr) };
            return Some((accessor, version));
        }
    }
    None
}

/// Resolve one required export, logging which one is missing on failure (so a Steam SDK rename names the
/// culprit). Like [`resolve_method`] but with the full symbol as the label (matchmaking + register).
fn resolve_required(module: HMODULE, what: &str, name: PCSTR) -> Option<usize> {
    let addr = proc_addr(module, name);
    if addr.is_none() {
        log::warn!("steam: required export {what} not found (Steam SDK rename? re-derive with objdump)");
    }
    addr
}
