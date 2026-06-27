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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use unseamless_core::diagnostics::{LobbyRole, peer_tag};
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
// Rung 4: Steam matchmaking lobbies — password-keyed discovery, POLL-BASED.
//
//   *** Triggered on demand by the in-overlay Open World / Join world actions (`crate::coop::host` /
//       `crate::coop::join`), not at launch. The rig PROVED the mechanism: an in-process `CreateLobby`
//       succeeds and its `SteamAPICall_t` resolves cleanly when *polled* via `ISteamUtils` (see
//       `run_lobby_callback_probe`). RIG-VERIFY still open: the joiner-finds-host leg across two
//       machines (the `GetLobbyByIndex`/`GetLobbyOwner` pre-join reads below), confirmed by the
//       two-player friend test. See docs/COOP-CONNECTION.md rung-4. ***
//
// This is the ONLY way two players pair — the manual peer-SteamID entry is gone. The role is the
// user's **choice** ([`LobbyIntent`]), not derived: a host CREATEs the one lobby (after a best-effort
// list to confirm none exists) + publishes the password/version data; a joiner LISTs lobbies filtered
// by `hash(password)`, JOINs a match, and reads the host from the lobby owner. The resolved peer +
// chosen `is_host` seed the rung-2 side-channel (`crate::coop`) — lobbies are *discovery*, not a new
// transport.
//
// ## Why poll-based, not call-results (the rig-proven lesson)
// Lobbies are inherently **async**: `CreateLobby`/`RequestLobbyList`/`JoinLobby` each return a
// `SteamAPICall_t`. The SDK offers two ways to get the result: register a `CCallbackBase` call-result
// (delivered by Steam's `RunCallbacks` pump), or POLL the handle via `ISteamUtils`
// (`IsAPICallCompleted` → `GetAPICallResult`). We POLL. The rig proved that registering a call-result
// on a handle we also poll is a trap: ELDEN RING pumps Steam via `RunCallbacks` and consumes the
// handle first, so our later poll then sees `InvalidHandle`. Poll-only sidesteps Steam's dispatch
// entirely (the same poll-not-pump discipline as rungs 1-3) and runs the whole flow on the co-op
// driver thread — so there are no call-result FFI thunks and no cross-thread shared state here.
//
// ## Clean-room note
// The `SteamAPI_*` flat names, the callback ids, and the result-struct fields below are all from the
// **public** Steamworks SDK headers (`isteammatchmaking.h`, `isteamutils.h`) — public knowledge like
// the rest of the flat API we bind, not from disassembling anything. Re-derive the export names after
// a Steam update with:
//   x86_64-w64-mingw32-objdump -p "ELDEN RING/Game/steam_api64.dll" | grep -E 'ISteamMatchmaking|SteamMatchmaking_v|ISteamUtils|SteamUtils_v'
//
// RIG-VERIFY checklist (each marked inline too): the matchmaking + utils accessor versions; the
// callback-struct packing (we read these structs out of `GetAPICallResult`); and the headline
// two-player question — does a joiner's filtered list find the host's freshly-tagged lobby.

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
    lobby: u64,            // m_ulSteamIDLobby         @0
    chat_permissions: u32, // m_rgfChatPermissions    @8
    // `m_bLocked` @12. Typed `u8`, not `bool`: `read_result` has Steam memcpy the raw result bytes into
    // this struct, and a byte other than 0/1 landing in a Rust `bool` is instant UB even if never read.
    locked: u8,
    enter_response: u32, // m_EChatRoomEnterResponse  @16
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

// --- ISteamMatchmaking flat methods ----------------------------------------------------------------

type MatchmakingAccessor = unsafe extern "C" fn() -> *mut c_void;
type CreateLobbyFn = unsafe extern "C" fn(*mut c_void, i32, i32) -> SteamAPICall;
type SetLobbyDataFn = unsafe extern "C" fn(*mut c_void, u64, *const c_char, *const c_char) -> bool;
type AddListStringFilterFn = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, i32);
type RequestLobbyListFn = unsafe extern "C" fn(*mut c_void) -> SteamAPICall;
type GetLobbyByIndexFn = unsafe extern "C" fn(*mut c_void, i32) -> u64;
type JoinLobbyFn = unsafe extern "C" fn(*mut c_void, u64) -> SteamAPICall;
type LeaveLobbyFn = unsafe extern "C" fn(*mut c_void, u64);
type GetLobbyOwnerFn = unsafe extern "C" fn(*mut c_void, u64) -> u64;
type GetNumLobbyMembersFn = unsafe extern "C" fn(*mut c_void, u64) -> i32;
type GetLobbyMemberByIndexFn = unsafe extern "C" fn(*mut c_void, u64, i32) -> u64;

/// The resolved `ISteamMatchmaking` interface + the flat methods rung 4 uses. The whole poll-based
/// lobby flow lives on the single co-op driver thread (see [`LobbyDiscovery`]), so this never crosses
/// threads and needs no `Send`/`Sync` — we hold the raw `iface` only to pass it to the flat methods.
struct Matchmaking {
    iface: *mut c_void,
    create_lobby: CreateLobbyFn,
    set_lobby_data: SetLobbyDataFn,
    add_filter: AddListStringFilterFn,
    request_list: RequestLobbyListFn,
    get_by_index: GetLobbyByIndexFn,
    join_lobby: JoinLobbyFn,
    leave_lobby: LeaveLobbyFn,
    get_lobby_owner: GetLobbyOwnerFn,
    get_num_members: GetNumLobbyMembersFn,
    get_member_by_index: GetLobbyMemberByIndexFn,
}

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
        let leave_lobby = unsafe { std::mem::transmute::<usize, LeaveLobbyFn>(resolve_required(module, "SteamAPI_ISteamMatchmaking_LeaveLobby", s!("SteamAPI_ISteamMatchmaking_LeaveLobby"))?) };
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
            leave_lobby,
            get_lobby_owner,
            get_num_members,
            get_member_by_index,
        })
    }
}

/// Re-issue `RequestLobbyList` this often. Valve does not index a freshly-published lobby's data
/// instantly, so the first filtered list is often empty even when a host is already up — re-issue rather
/// than latch that as failure. A host re-lists on the same beat to catch the both-create race. The
/// `run_discovery` timeout is the real deadline.
const LOBBY_LIST_RETRY: Duration = Duration::from_secs(2);

/// Which side of the pairing the user chose. The role is **configured by the menu action** (Open World
/// ⇒ host, Join world ⇒ joiner), not derived from what discovery finds — so only the host ever creates
/// a lobby and there is no both-create race to resolve. See [`LobbyDiscovery`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LobbyIntent {
    /// Open World: confirm no lobby with this password exists, create one, and wait for a friend.
    Host,
    /// Join world: search for an existing lobby keyed on the password and enter it.
    Join,
}

/// One tick of [`LobbyDiscovery::poll`]. The co-op driver toasts/banners off these and seeds rung 2 on
/// `Resolved`.
pub enum LobbyResult {
    /// Still working toward a stable state (subject to the driver's setup timeout).
    Pending,
    /// Host only: the lobby is open and we're waiting for a friend to join. No timeout — a host stays
    /// open as long as the user leaves the world open.
    Hosting,
    /// The partner is resolved (with the role we played); hand it to the rung-2 side-channel.
    Resolved { peer: u64, is_host: bool },
    /// Discovery failed terminally; the plain-words reason is for the toast/log.
    Failed(String),
}

/// The internal pairing state machine. The entry state is set by [`LobbyIntent`]: a host confirms no
/// lobby exists then creates one; a joiner lists on a cadence until it finds one.
enum Role {
    /// Host: one filtered list out to confirm no lobby with this password already exists (a host that
    /// finds one errors out and tells the user to Join instead).
    HostChecking,
    /// Host: our `CreateLobby` is in flight (polled via [`Utils`]).
    Creating { call: SteamAPICall },
    /// Host: we own the lobby. Poll its members for the joiner.
    Hosting { lobby: u64 },
    /// Joiner: listing on a cadence for a matching lobby.
    JoinSeeking,
    /// Joiner: our `JoinLobby` is in flight; `lobby` is the one we're entering.
    Joining { call: SteamAPICall, lobby: u64 },
    /// Joiner: we entered a lobby. Poll `GetLobbyOwner` until the host id is populated.
    Joined { lobby: u64 },
}

/// Drives password-keyed lobby discovery to resolve the rung-2 peer, playing the role the user chose
/// ([`LobbyIntent`]). Construct with [`start`](LobbyDiscovery::start); the co-op driver calls
/// [`poll`](LobbyDiscovery::poll) each tick until it yields a terminal [`LobbyResult`].
///
/// Entirely poll-based and single-threaded (the co-op driver thread): each async lobby call returns a
/// `SteamAPICall_t` we poll via [`Utils`] (`IsAPICallCompleted`/`GetAPICallResult`), never registering a
/// `CCallbackBase` call-result — ELDEN RING's own `RunCallbacks` pump would consume the handle first and
/// leave our poll seeing `InvalidHandle` (the rig-proven lesson; see the rung-4 module comment).
///
/// ## Explicit roles (no both-create race)
/// The role is the user's choice, not derived: a host (Open World) creates the only lobby and a joiner
/// (Join world) enters it, so the two never both create. A host first issues a single best-effort list
/// to detect a friend who already hosted on this password (Valve indexing lag can miss a very recent
/// one — acceptable for that edge case); finding one is reported as a failure so the UI can say "Join
/// instead". The joiner reads the host from `GetLobbyOwner` post-join (owner metadata is only readable
/// once we are a member — see [`poll_owner`]). RIG-VERIFY: the joiner-finds-host leg on the two-player
/// rig (`GetLobbyByIndex`/`GetLobbyOwner` across machines).
pub struct LobbyDiscovery {
    mm: Matchmaking,
    utils: Utils,
    /// Our own SteamID — excludes us from the host's member scan (the tiebreak keys on lobby ids, not
    /// this).
    self_id: u64,
    /// The `usc_pw` filter value (the password token), re-applied on each list retry (the filter is
    /// consumed per request).
    key: String,
    role: Role,
    /// A `RequestLobbyList` we've issued and are polling, if any (the SDK allows one in flight).
    list_call: Option<SteamAPICall>,
    /// When we last issued a `RequestLobbyList`, to pace retries to [`LOBBY_LIST_RETRY`].
    last_request: Option<Instant>,
}

impl LobbyDiscovery {
    /// Bind matchmaking + utils against the already-loaded `steam_api64.dll` and start discovery in the
    /// entry state for `intent` (host ⇒ `HostChecking`, joiner ⇒ `JoinSeeking`). `None` (logged) if the
    /// Steam binding isn't available — the caller degrades. `self_id` is our own resolved SteamID (rung
    /// 1), which the caller has already waited for.
    pub fn start(self_id: u64, password: &str, intent: LobbyIntent) -> Option<LobbyDiscovery> {
        // SAFETY: borrow the already-loaded module handle (same as rung 1/2); not-found means Steam
        // hasn't loaded yet.
        let module = match unsafe { GetModuleHandleA(s!("steam_api64.dll")) } {
            Ok(m) => m,
            Err(e) => {
                log::warn!("steam: steam_api64.dll not loaded ({e}); no lobby discovery");
                return None;
            }
        };
        let mm = Matchmaking::resolve(module)?;
        let utils = Utils::resolve(module)?;
        let role = match intent {
            LobbyIntent::Host => Role::HostChecking,
            LobbyIntent::Join => Role::JoinSeeking,
        };
        Some(LobbyDiscovery {
            mm,
            utils,
            self_id,
            key: password_lobby_key(password),
            role,
            list_call: None,
            last_request: None,
        })
    }

    /// Leave whatever lobby we currently hold, if any — called on session teardown (Leave World) so a
    /// host's lobby disappears and a joiner exits cleanly. A no-op before a lobby is created/entered.
    pub fn leave(&self) {
        let lobby = match self.role {
            Role::Hosting { lobby } | Role::Joined { lobby } | Role::Joining { lobby, .. } => Some(lobby),
            Role::HostChecking | Role::Creating { .. } | Role::JoinSeeking => None,
        };
        if let Some(lobby) = lobby {
            // SAFETY: resolved flat method; us → Steam.
            unsafe { (self.mm.leave_lobby)(self.mm.iface, lobby) };
        }
    }

    /// Poll the flow one tick, returning the current [`LobbyResult`]. Called each tick by the co-op
    /// driver until it yields a terminal result (`Resolved`/`Failed`).
    pub fn poll(&mut self) -> LobbyResult {
        // Service any outstanding filtered-list result first — it drives the create-vs-fail (host) or
        // join (joiner) decision and can yield a terminal failure (a host finding an existing lobby).
        if let Some(result) = self.service_list() {
            return result;
        }

        match self.role {
            Role::HostChecking => {
                // Issue the single existence-check list on the retry cadence until it returns.
                self.maybe_request_list();
                LobbyResult::Pending
            }
            Role::Creating { call } => self.poll_create(call),
            Role::Hosting { lobby } => match self.poll_host_members(lobby) {
                Some((peer, is_host)) => LobbyResult::Resolved { peer, is_host },
                None => LobbyResult::Hosting,
            },
            Role::JoinSeeking => {
                self.maybe_request_list();
                LobbyResult::Pending
            }
            Role::Joining { call, lobby } => self.poll_join(call, lobby),
            Role::Joined { lobby } => match self.poll_owner(lobby) {
                Some((peer, is_host)) => LobbyResult::Resolved { peer, is_host },
                None => LobbyResult::Pending,
            },
        }
    }

    /// Poll a pending `RequestLobbyList`; on completion, act on the match count per our role. Returns
    /// `Some(Failed)` only when a host's existence check finds a lobby (terminal); otherwise `None`
    /// (state may have advanced to `Creating`/`Joining`).
    fn service_list(&mut self) -> Option<LobbyResult> {
        let call = self.list_call?;
        match self.utils.completed(call) {
            None => None,                                     // still pending
            Some(true) => {
                self.list_call = None; // IO failure — drop it; the cadence re-issues
                None
            }
            Some(false) => {
                self.list_call = None;
                let mut res = LobbyMatchList { matching: 0 };
                if self.utils.read_result(call, CB_LOBBY_MATCH_LIST, &mut res) {
                    self.handle_list(res.matching)
                } else {
                    None
                }
            }
        }
    }

    /// Act on a returned filtered list per the role we're playing. Collects the matching **lobby
    /// SteamIDs** (the only key readable pre-join), then: `HostChecking` ⇒ any match means a lobby
    /// already exists (terminal failure, "Join instead"), none ⇒ create ours; `JoinSeeking` ⇒ join the
    /// lowest-id match if any, else keep seeking. Returns `Some(Failed)` only for the host-exists case.
    fn handle_list(&mut self, matching: u32) -> Option<LobbyResult> {
        crate::coop::note_lobby_list(matching);
        // Lobby SteamIDs of each match. `get_by_index` indexes the *last* list (the one we just
        // completed) and returns the id pre-join; the owner/members are NOT readable until we join.
        let mut lobbies: Vec<u64> = Vec::new();
        for i in 0..matching as i32 {
            // SAFETY: resolved flat method; `iface` read-only; `i` in range (i < matching).
            let lobby = unsafe { (self.mm.get_by_index)(self.mm.iface, i) };
            if lobby != 0 {
                lobbies.push(lobby);
            }
        }

        match self.role {
            Role::HostChecking => {
                if lobbies.is_empty() {
                    // No one is hosting on this password — create our own and become the host.
                    self.start_create();
                    None
                } else {
                    // A friend already opened a world on this password; tell the user to Join instead.
                    let why = "A world with this password is already open — use Join world instead.";
                    crate::coop::note_lobby_failure(why);
                    Some(LobbyResult::Failed(why.to_string()))
                }
            }
            Role::JoinSeeking => {
                // Existing lobby ⇒ join the lowest-id one (deterministic if several match); none ⇒ keep
                // listing on the cadence until one appears or the driver's join timeout fires.
                if let Some(lobby) = lobbies.iter().copied().min() {
                    self.start_join(lobby);
                }
                None
            }
            // A stray list result while a create/join is in flight or already joined — ignore it.
            _ => None,
        }
    }

    /// Poll our in-flight `CreateLobby`. On success, publish the password/version data and become the
    /// host (`Hosting`); a create failure is terminal (a host can't proceed without its lobby).
    fn poll_create(&mut self, call: SteamAPICall) -> LobbyResult {
        match self.utils.completed(call) {
            None => LobbyResult::Pending,
            Some(true) => {
                let why = "Couldn't open the world (Steam reported an IO failure creating the lobby).";
                crate::coop::note_lobby_failure(why);
                LobbyResult::Failed(why.to_string())
            }
            Some(false) => {
                let mut res = LobbyCreated { result: 0, lobby: 0 };
                if self.utils.read_result(call, CB_LOBBY_CREATED, &mut res)
                    && res.result == E_RESULT_OK
                {
                    set_lobby_data(&self.mm, res.lobby, LOBBY_KEY_PASSWORD, &self.key);
                    set_lobby_data(&self.mm, res.lobby, LOBBY_KEY_VERSION, &PROTOCOL_VERSION.to_string());
                    crate::coop::note_lobby_created();
                    self.role = Role::Hosting { lobby: res.lobby };
                    LobbyResult::Hosting
                } else {
                    let why = format!("Couldn't open the world (CreateLobby failed, EResult {}).", res.result);
                    crate::coop::note_lobby_failure(why.clone());
                    LobbyResult::Failed(why)
                }
            }
        }
    }

    /// Poll our in-flight `JoinLobby`. On success, move to `Joined` (the owner read is deferred to
    /// [`poll_owner`], to tolerate owner metadata lagging the enter); a join failure drops back to
    /// `JoinSeeking` so the cadence retries (the host's lobby may still be coming up) until the driver's
    /// join timeout fires. Always returns `Pending` — joining is never terminal here.
    fn poll_join(&mut self, call: SteamAPICall, lobby: u64) -> LobbyResult {
        match self.utils.completed(call) {
            None => {} // pending
            Some(true) => {
                crate::coop::note_lobby_failure("JoinLobby reported an IO failure");
                self.role = Role::JoinSeeking;
            }
            Some(false) => {
                let mut res =
                    LobbyEnter { lobby: 0, chat_permissions: 0, locked: 0, enter_response: 0 };
                if self.utils.read_result(call, CB_LOBBY_ENTER, &mut res) {
                    crate::coop::note_lobby_joined();
                    self.role = Role::Joined { lobby };
                } else {
                    crate::coop::note_lobby_failure("JoinLobby failed");
                    self.role = Role::JoinSeeking;
                }
            }
        }
        LobbyResult::Pending
    }

    /// Host: scan our lobby's members for the non-self entry — that's the joiner, our rung-2 peer.
    fn poll_host_members(&self, lobby: u64) -> Option<(u64, bool)> {
        // SAFETY: resolved flat methods; `iface` read-only.
        let count = unsafe { (self.mm.get_num_members)(self.mm.iface, lobby) };
        for i in 0..count {
            let member = unsafe { (self.mm.get_member_by_index)(self.mm.iface, lobby, i) };
            if member != 0 && member != self.self_id {
                crate::coop::note_lobby_host_resolved(); // the host id is our own — trivially known
                return Some((member, true));
            }
        }
        None
    }

    /// Joiner: resolve the host from `GetLobbyOwner`. Owner metadata can lag `LobbyEnter`, so a transient
    /// `0` means "poll again", not failure.
    fn poll_owner(&self, lobby: u64) -> Option<(u64, bool)> {
        // SAFETY: resolved flat method; `iface` read-only.
        let owner = unsafe { (self.mm.get_lobby_owner)(self.mm.iface, lobby) };
        if owner != 0 {
            crate::coop::note_lobby_host_resolved();
            Some((owner, false))
        } else {
            None
        }
    }

    /// Issue `CreateLobby` and enter the `Creating` state. A refused call leaves us in `HostChecking`,
    /// where the retry cadence re-lists and tries to create again (the driver's setup timeout bounds it).
    fn start_create(&mut self) {
        crate::coop::note_lobby_role(LobbyRole::Host);
        // SAFETY: resolved flat method; us → Steam.
        let call =
            unsafe { (self.mm.create_lobby)(self.mm.iface, ELOBBY_TYPE_PUBLIC, LOBBY_MAX_MEMBERS) };
        if call == API_CALL_INVALID {
            crate::coop::note_lobby_failure("CreateLobby returned no API call (Steam refused it)");
            return; // stay in HostChecking; the retry cadence tries again
        }
        self.role = Role::Creating { call };
    }

    /// Issue `JoinLobby` for `lobby` and enter the `Joining` state. A refused call drops back to
    /// `JoinSeeking` so the cadence retries.
    fn start_join(&mut self, lobby: u64) {
        crate::coop::note_lobby_role(LobbyRole::Joiner);
        // SAFETY: resolved flat method; us → Steam.
        let call = unsafe { (self.mm.join_lobby)(self.mm.iface, lobby) };
        if call == API_CALL_INVALID {
            crate::coop::note_lobby_failure("JoinLobby returned no API call (Steam refused it)");
            self.role = Role::JoinSeeking;
            return;
        }
        self.role = Role::Joining { call, lobby };
    }

    /// (Re)issue a filtered `RequestLobbyList` on the [`LOBBY_LIST_RETRY`] cadence, unless one is already
    /// in flight. The string filter is consumed per request, so re-apply it every time.
    fn maybe_request_list(&mut self) {
        if self.list_call.is_some() {
            return; // one already outstanding
        }
        if self.last_request.is_some_and(|t| t.elapsed() < LOBBY_LIST_RETRY) {
            return; // too soon since the last request
        }
        let (Ok(fk), Ok(fv)) = (CString::new(LOBBY_KEY_PASSWORD), CString::new(self.key.clone()))
        else {
            crate::coop::note_lobby_failure("could not marshal the lobby password filter");
            return;
        };
        self.last_request = Some(Instant::now());
        // SAFETY: resolved flat methods; the CStrings outlive the calls.
        unsafe { (self.mm.add_filter)(self.mm.iface, fk.as_ptr(), fv.as_ptr(), ELOBBY_CMP_EQUAL) };
        let call = unsafe { (self.mm.request_list)(self.mm.iface) };
        if call != API_CALL_INVALID {
            self.list_call = Some(call);
        }
        // A refused request leaves `list_call` None; the next cadence tick retries.
    }
}

/// **Rung-4 lobby probe** (the re-derivation tool) — gated by `[debug.probes] lobby_callback_probe`
/// (off by default), wired in [`crate::app`]. Confirms the mechanism the real discovery
/// ([`LobbyDiscovery`]) is built on: that an in-process `CreateLobby` succeeds and its `SteamAPICall_t`
/// resolves cleanly when **polled** via `ISteamUtils` (`IsAPICallCompleted`/`GetAPICallResult`).
///
/// This settled the rung-4 design question: ELDEN
/// RING pumps Steam via `RunCallbacks` and consumes a handle before we could poll it *if* we also
/// register a call-result on it — so registering is the trap and poll-only is the path. Kept as the
/// fast re-derive after a game/Steam update: it issues one harmless private `CreateLobby` and logs
/// (info, `lobby-probe:` prefix) the polled outcome. Deliberately isolated from discovery — no password,
/// no join — so a negative (the call never completes) reads just as clearly. Solo.
///
/// Re-derive after a game/Steam update: docs/COOP-CONNECTION.md rung-4 build order, step 1.
pub fn run_lobby_callback_probe() {
    // Off the loader path, after a short settle so Steam matchmaking is up (rung 1 resolves the
    // SteamID ~0.5s after load; matchmaking is ready well before this fires).
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(5));
        issue_probe_create_lobby();
    });
}

/// Body of [`run_lobby_callback_probe`], on its own thread. Resolves the binding, then retries
/// `CreateLobby` over ~a minute — polling each attempt via `ISteamUtils` (never registering a
/// call-result on the handle) — logging whether the call succeeds or IO-fails.
fn issue_probe_create_lobby() {
    // SAFETY: borrow the already-loaded module handle (same as rung 1/2); not-found = Steam not up.
    let module = match unsafe { GetModuleHandleA(s!("steam_api64.dll")) } {
        Ok(m) => m,
        Err(e) => {
            log::warn!("lobby-probe: steam_api64.dll not loaded ({e}); cannot probe");
            return;
        }
    };
    let Some(mm) = Matchmaking::resolve(module) else {
        log::warn!("lobby-probe: ISteamMatchmaking unavailable (Steam not initialized?); cannot probe");
        return;
    };

    let Some(utils) = Utils::resolve(module) else {
        log::warn!("lobby-probe: ISteamUtils poll unavailable; cannot run the retry/poll probe");
        return;
    };

    // Is THIS game session actually logged on to Steam's backend? Matchmaking (CreateLobby) needs it, but
    // identity resolves from cache even when it isn't — so a `false` here would directly explain the IO
    // failures (our offline/non-EAC launch may not bring up the Steam logon ER would for online play).
    if let Some((user_acc, _v)) = resolve_user_accessor(module) {
        // SAFETY: resolved accessor, no args; null = Steam not up.
        let user = unsafe { user_acc() };
        if !user.is_null()
            && let Some(addr) = resolve_required(module, "SteamAPI_ISteamUser_BLoggedOn", s!("SteamAPI_ISteamUser_BLoggedOn"))
        {
            type BLoggedOnFn = unsafe extern "C" fn(*mut c_void) -> bool;
            // SAFETY: documented flat-API signature declared on its fn type.
            let blogged_on: BLoggedOnFn = unsafe { std::mem::transmute::<usize, BLoggedOnFn>(addr) };
            let on = unsafe { blogged_on(user) };
            log::info!("lobby-probe: ISteamUser::BLoggedOn = {on} (is this game session connected to Steam's backend?)");
        }
    }

    // Retry CreateLobby over ~a minute, POLLING each attempt via ISteamUtils — and crucially NOT
    // registering a call-result on the handle. Registering lets ER's RunCallbacks consume/dispatch the
    // result first, so our poll then sees an expired handle (that was the earlier "InvalidHandle" IO
    // failures — an artifact, not a real CreateLobby failure). A public 2-seat lobby matches the proven
    // path (the harness on appid 480 and the real discovery code use public). A success here means
    // CreateLobby works and a poll-based rung 4 (no RegisterCallResult) is the path.
    const ATTEMPTS: u32 = 6;
    for attempt in 1..=ATTEMPTS {
        // SAFETY: resolved flat method; us → Steam.
        let call = unsafe { (mm.create_lobby)(mm.iface, ELOBBY_TYPE_PUBLIC, LOBBY_MAX_MEMBERS) };
        if call == API_CALL_INVALID {
            log::warn!("lobby-probe: attempt {attempt}/{ATTEMPTS}: CreateLobby returned no API call (Steam refused it)");
            std::thread::sleep(Duration::from_secs(8));
            continue;
        }

        // Poll this attempt's handle to completion (up to ~8s) — no call-result registered on it.
        let mut completed = false;
        let mut failed = false;
        for _ in 0..16 {
            std::thread::sleep(Duration::from_millis(500));
            let mut f = false;
            // SAFETY: resolved flat method; `iface` from the accessor; `f` is a valid out-param.
            if unsafe { (utils.is_completed)(utils.iface, call, &mut f) } {
                completed = true;
                failed = f;
                break;
            }
        }
        if completed && !failed {
            let mut buf = LobbyCreated { result: 0, lobby: 0 };
            let mut gf = false;
            // SAFETY: 16-byte LobbyCreated buffer matching CB_LOBBY_CREATED's expected size/id.
            let ok = unsafe {
                (utils.get_result)(utils.iface, call, (&mut buf as *mut LobbyCreated).cast(), size_of::<LobbyCreated>() as i32, CB_LOBBY_CREATED, &mut gf)
            };
            log::info!(
                "lobby-probe: VERDICT — CreateLobby SUCCEEDED on attempt {attempt}/{ATTEMPTS} (ok={ok} EResult={} lobby={}). The earlier IO failures were the register+poll conflict (RunCallbacks consumed the handle first), NOT a real failure. A POLL-BASED rung 4 (IsAPICallCompleted/GetAPICallResult, no RegisterCallResult) is the path.",
                buf.result, buf.lobby
            );
            return;
        }
        // SAFETY: resolved flat method; querying the just-completed call's failure reason.
        let reason = unsafe { (utils.get_failure_reason)(utils.iface, call) };
        log::warn!("lobby-probe: attempt {attempt}/{ATTEMPTS}: completed={completed} failed={failed} reason={reason} ({}) — retrying in 8s…", failure_reason_str(reason));
        std::thread::sleep(Duration::from_secs(8));
    }
    log::warn!("lobby-probe: VERDICT — CreateLobby IO-failed on all {ATTEMPTS} attempts over ~a minute while Steam is online and running ELDEN RING → not a timing issue. Our in-process CreateLobby is being rejected; next: compare how ERSC issues it (lobby type/params, Steam user/pipe context, whether a SteamAPI re-init / RunCallbacks priming is needed)");
}

// --- ISteamUtils manual call-result poll (the poll path rung 4 + the probe are built on) ---
type UtilsAccessor = unsafe extern "C" fn() -> *mut c_void;
type IsAPICallCompletedFn = unsafe extern "C" fn(*mut c_void, SteamAPICall, *mut bool) -> bool;
type GetAPICallResultFn =
    unsafe extern "C" fn(*mut c_void, SteamAPICall, *mut c_void, i32, i32, *mut bool) -> bool;
type GetFailureReasonFn = unsafe extern "C" fn(*mut c_void, SteamAPICall) -> i32;

/// `ISteamUtils` + the manual call-result methods. Lets us POLL a `SteamAPICall_t` for completion and
/// read its result/failure without the `CCallbackBase`/`RunCallbacks` dispatch — the same poll-not-pump
/// approach rung 1/2 use for networking. Used only on a single thread (the co-op driver, or the probe's
/// own thread), so it never crosses threads and needs no `Send`/`Sync`; we hold the raw `iface` only to
/// pass it to the flat methods.
struct Utils {
    iface: *mut c_void,
    is_completed: IsAPICallCompletedFn,
    get_result: GetAPICallResultFn,
    get_failure_reason: GetFailureReasonFn,
}

impl Utils {
    /// Has `call` completed? `None` = still pending; `Some(io_failure)` = completed, with the IO-failure
    /// flag Steam reported (a `true` means the call itself failed, e.g. `InvalidHandle`/`NetworkFailure`).
    fn completed(&self, call: SteamAPICall) -> Option<bool> {
        let mut io_failure = false;
        // SAFETY: resolved flat method; `io_failure` is a valid out-param.
        let done = unsafe { (self.is_completed)(self.iface, call, &mut io_failure) };
        done.then_some(io_failure)
    }

    /// Read a completed call's result struct (POD `T`) for `callback_id` into `out`. Returns whether the
    /// read succeeded *and* the call did not IO-fail. Call only after [`completed`](Utils::completed)
    /// returns `Some(false)`.
    fn read_result<T>(&self, call: SteamAPICall, callback_id: i32, out: &mut T) -> bool {
        let mut io_failure = false;
        // SAFETY: resolved flat method; `out` is a valid `T` buffer whose size/`callback_id` match the
        // pending call's result struct (the caller pairs them — see CB_LOBBY_* and the result structs).
        let ok = unsafe {
            (self.get_result)(
                self.iface,
                call,
                (out as *mut T).cast(),
                size_of::<T>() as i32,
                callback_id,
                &mut io_failure,
            )
        };
        ok && !io_failure
    }

    fn resolve(module: HMODULE) -> Option<Utils> {
        // SAFETY: resolved accessor taking no args; null = Steam not initialized.
        let accessor: UtilsAccessor = unsafe {
            std::mem::transmute::<usize, UtilsAccessor>(resolve_required(module, "SteamAPI_SteamUtils_v010", s!("SteamAPI_SteamUtils_v010"))?)
        };
        let iface = unsafe { accessor() };
        if iface.is_null() {
            log::warn!("lobby-probe: ISteamUtils accessor returned null (Steam not initialized?)");
            return None;
        }
        // SAFETY (each transmute): documented flat-API signature declared on its fn type.
        let is_completed: IsAPICallCompletedFn = unsafe {
            std::mem::transmute::<usize, IsAPICallCompletedFn>(resolve_required(module, "SteamAPI_ISteamUtils_IsAPICallCompleted", s!("SteamAPI_ISteamUtils_IsAPICallCompleted"))?)
        };
        let get_result: GetAPICallResultFn = unsafe {
            std::mem::transmute::<usize, GetAPICallResultFn>(resolve_required(module, "SteamAPI_ISteamUtils_GetAPICallResult", s!("SteamAPI_ISteamUtils_GetAPICallResult"))?)
        };
        let get_failure_reason: GetFailureReasonFn = unsafe {
            std::mem::transmute::<usize, GetFailureReasonFn>(resolve_required(module, "SteamAPI_ISteamUtils_GetAPICallFailureReason", s!("SteamAPI_ISteamUtils_GetAPICallFailureReason"))?)
        };
        Some(Utils { iface, is_completed, get_result, get_failure_reason })
    }
}

/// `ESteamAPICallFailure` → a short name. The reason a poll-completed call reports `failed=true`.
fn failure_reason_str(reason: i32) -> &'static str {
    match reason {
        -1 => "None",
        0 => "SteamGone (Steam client shut down)",
        1 => "NetworkFailure (lost connection to Steam backend)",
        2 => "InvalidHandle (call handle expired / already consumed)",
        3 => "MismatchedCallback (expected callback id doesn't match the result)",
        _ => "unknown",
    }
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
