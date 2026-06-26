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

use unseamless_core::diagnostics::peer_tag;
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
