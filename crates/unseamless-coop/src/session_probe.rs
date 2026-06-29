//! Rung-3 RE prep: instrumentation for the **session create/join initiation** — the one networking
//! gap the SDK doesn't chart (see [`docs/COOP-CONNECTION.md`](../../../docs/COOP-CONNECTION.md) >
//! "What the SDK gives us vs. the RE gap" and [`docs/SDK-COVERAGE.md`](../../../docs/SDK-COVERAGE.md)).
//! The SDK gives us the FSM *state* (`CSSessionManager.{lobby_state, protocol_state}`), the roster,
//! and the transport vtable — but **not** the internal functions that drive
//! `lobby_state None -> TryToCreateSession -> Host` (host) and `None -> TryToJoinSession -> Client`
//! (joiner). Finding those is rig-gated RE; this module is the scaffold that makes that run cheap.
//!
//! It ships **gated** (`[debug.probes] session_probe`, off by default) and splits into two surfaces,
//! both emitting the unique, greppable `session-probe:` prefix so a *batched* rig run (several lanes,
//! one game launch) can `grep session-probe:` and read exactly the create/join story out of the log:
//!
//! 1. **FSM rising-edge logger** ([`SessionFsmProbe`], a frame task) — logs every lobby/protocol
//!    transition with its frame number, plus the live `CSSessionManager` base address once. This half
//!    is **fully exercised solo**: without a peer it just sits at `lobby=None`, but the transition-
//!    detection machinery runs and is correct, ready for the two-player run. Reads through the shared
//!    [`crate::session::read`] (the same path the observer + diag report use) so the probe sees the
//!    session identically and there's one session-read path, not a parallel one to drift.
//! 2. **Create/join entry hooks** ([`install_hooks`]) — once the initiation-function AOBs are charted
//!    on the rig, a `jmp-back` hook at each entry logs the call and its argument registers (the
//!    candidate `this` pointer + peer SteamID), correlated by frame/timestamp to the FSM transition
//!    the call triggers. This half is **inert until an address lands**: the AOB landmarks below are
//!    `None` (a precise TODO), so `install_hooks` logs "not yet charted" and installs nothing. Filling
//!    [`SESSION_CREATE_SITE`] / [`SESSION_JOIN_SITE`] is the *only* remaining step — the whole install
//!    path (resolve landmark, place the hook, log) already compiles and is in place.
//!
//! The hand-off recipe (which two functions, why they're the create/join initiation, how to AOB-scan
//! for them, what `session-probe:` lines mean) is [`docs/SESSION-RE-RUNBOOK.md`](../../../docs/SESSION-RE-RUNBOOK.md).
//!
//! ## Clean-room
//! Everything here is grounded in the public SDK (the charted FSM enums/fields) or in our own
//! observations; no upstream ERSC code or decompiler output is transcribed (CLAUDE.md > Clean-room).
//!
//! ## Lifetime & safety
//! The entry hooks (when live) follow the same invariants as [`crate::saves`]: installed once on the
//! init thread, `mem::forget`-ten (resident for the process lifetime — never unhook a live code
//! path). The callbacks are **read-only** — they log register values and never write game memory or
//! dereference a pointer they were handed, so a probe can't perturb the session it's observing.

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex, LobbyState, ProtocolState};
use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_back};
use pelite::pattern::Atom;
use unseamless_core::config::Config;
use unseamless_core::util::{FrameThrottle, Latch};
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::core::PCSTR;

use crate::feature::{Feature, Tick};

/// A landmark-relative function entry to hook: scan for the unique `landmark`, step `offset` bytes to
/// the entry, and verify the first byte equals `expect` (the prologue opcode) before hooking — the
/// same fail-loud-and-safe contract [`crate::patch::resolve_landmark`] gives the code patches. The
/// fields stay `None` at the const sites below until the rig RE fills them; see the module docs.
struct HookSite {
    /// pelite masked-AOB pattern that uniquely locates a fixed point near the function entry.
    landmark: &'static [Atom],
    /// Signed byte distance from the landmark match-start to the function entry (0 if the landmark
    /// *is* the entry).
    offset: isize,
    /// The opcode byte expected at the entry, as an anti-drift guard (e.g. `0x48` for a
    /// `48 8B C4` / `48 83 EC ..` prologue, `0x40` for `40 53 ..`).
    expect: u8,
}

// ---------------------------------------------------------------------------------------------------
// RIG TODO (rung 3): the create/join initiation function entries are not charted by the SDK and can
// only be found with the game running. Until then both sites are `None` and the hook half is inert
// (the FSM logger above still works solo). A static pass (docs/SESSION-RE-FINDINGS.md) charted the
// supporting anchors and proved this must be a *runtime write-watch*, not a static byte scan:
//   - The live `CSSessionManager` is `[0x143d7a4d0]`, == the `base` the FSM logger prints below.
//   - `lobby_state` (+0xc; the vftable is 8 bytes + `unk8` u32, so the FSM pair is at +0xc/+0x10, NOT
//     +0x8) is written on the singleton by a *register* store, never an immediate — so scanning for
//     `C7 4? 0C 01/04` finds only unrelated objects (it was tried; see the findings doc).
// To chart, follow docs/SESSION-RE-RUNBOOK.md > step 2 (strategy A):
//   1. Frida-watch a 4-byte write on `base + 0xc`; host once (→1 = create) and join once (→4 = join).
//      The watch names the writing instruction on the first `None →` edge.
//   2. Walk back to the enclosing function's prologue; translate ~16 unique entry bytes to a pelite
//      pattern (one `?` per wildcard byte). Mind that the writer is a `this`-param callee — hook the
//      outermost initiation entry the host/join path calls, per the captured stack.
//   3. Set the const below to `Some(HookSite { landmark: pattern!("…"), offset: …, expect: 0x.. })`
//      and rebuild. `install_hooks` then resolves + hooks it; watch for the `session-probe: hooked …`
//      line, then the `session-probe: create-session initiated …` line on a real connect.
// ---------------------------------------------------------------------------------------------------

/// Entry of the function that starts hosting (drives `lobby_state -> TryToCreateSession`). `None`
/// until charted on the rig — see the RIG TODO above and `docs/SESSION-RE-RUNBOOK.md`.
const SESSION_CREATE_SITE: Option<HookSite> = None;

/// Entry of the function that starts joining (drives `lobby_state -> TryToJoinSession`). `None` until
/// charted on the rig — see the RIG TODO above and `docs/SESSION-RE-RUNBOOK.md`.
const SESSION_JOIN_SITE: Option<HookSite> = None;

/// Install the create/join entry hooks if the probe is enabled. A no-op when the probe is off; when
/// on but the AOBs aren't charted yet, logs that each hook is inert and installs nothing. Mirrors
/// [`crate::saves::install`] / [`crate::app::apply_boot_patches`]: call once, on the init thread, at
/// install. Best-effort throughout — a probe never aborts the game (it's not a `guard::fatal`
/// condition; it's a diagnostic).
pub fn install_hooks(config: &Config) {
    if config.debug.probes.session_probe {
        install_one("create-session", &SESSION_CREATE_SITE);
        install_one("join-session", &SESSION_JOIN_SITE);
    }
    // Independently gated on `drive_create` (you only want the gate trace alongside a driven create).
    install_create_gate_trace(config);
}

// --- Leg-B create-gate tracer (pairs with `drive_create`) ---------------------------------------
//
// Static RE (docs/SESSION-DRIVE.md > "Leg-B re-charted") narrowed the offline create failure to two
// synchronous gates inside leg B (the network-create vmethod), both reading fields a real peer/match
// context populates. This tracer reads those fields at runtime on a driven create, so the log says
// exactly WHICH gate stops it and the exact zero fields — the artifact to have before the 2-player run.
// Both targets are clean (non-Arxan) functions reached only during session create, so they stay quiet
// offline outside our one-shot drive. Resolved by fixed offset from the live exe base (like
// SessionCreateDriver), not an AOB — the addresses are charted.
//
// Re-derive after a game update: re-chart leg B per SESSION-DRIVE.md and update the two offsets.

/// Leg-B network-create entry (`0x1423f5c00`). Its first act tests reject #1 (`[NetworkSession+0x10]`);
/// `rcx` here IS the live `NetworkSession`, so reading `[rcx+0x10]` at this entry resolves the
/// `*(this+0x60)` P-drift caveat the `force_netsession_ready` probe hit.
const LEGB_ENTRY_OFFSET: usize = 0x1_423f_5c00 - 0x1_4000_0000;
/// The **4th create gate** (`0x1423fd7a0` = `[new_obj_vtable+8]`), reached only if rejects #1–3 passed.
/// `rcx` is the freshly-built `0x5f8`-byte session object; the gate vetoes (offline) when its config
/// fields are zero.
const CREATE_GATE4_OFFSET: usize = 0x1_423f_d7a0 - 0x1_4000_0000;

/// Place read-only `jmp-back` tracers on leg-B entry and the 4th create gate when `drive_create` is on.
/// No-op otherwise. Best-effort: a failed hook logs and is skipped, never aborts (it's a diagnostic).
fn install_create_gate_trace(config: &Config) {
    if !config.debug.probes.drive_create {
        return;
    }
    let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
        Ok(h) => h.0 as usize,
        Err(e) => {
            log::error!("session-probe: gate-trace — GetModuleHandle(NULL) failed: {e}");
            return;
        }
    };
    install_offset_hook("legb-entry", exe_base + LEGB_ENTRY_OFFSET, log_legb_entry);
    install_offset_hook("create-gate4", exe_base + CREATE_GATE4_OFFSET, log_create_gate4);
}

/// Place one read-only `jmp-back` hook at a resolved address. `mem::forget`s the handle (resident for
/// the process lifetime, like every hook here — never unhook a live code path).
fn install_offset_hook(name: &'static str, addr: usize, body: fn(&'static str, *mut Registers)) {
    // SAFETY: `addr` is a charted, clean function entry (exe base + a fixed offset to a verified
    // prologue); the detour body is read-only and panic-firewalled (see the log_* fns).
    let hook = unsafe {
        hook_closure_jmp_back(
            addr,
            move |regs: *mut Registers| body(name, regs),
            CallbackOption::None,
            HookFlags::empty(),
        )
    };
    match hook {
        Ok(h) => {
            std::mem::forget(h);
            log::info!("session-probe: gate-trace hooked {name} at {addr:#x}");
        }
        Err(e) => log::error!("session-probe: gate-trace failed to hook {name}: {e:?}"),
    }
}

/// Leg-B entry tracer: confirms we reach leg B and reads reject #1's readiness flag (`[NetworkSession
/// +0x10]`) at the real call site. Read-only; firewalled against unwind across the FFI boundary.
fn log_legb_entry(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook hands us the saved registers; rcx is the NetworkSession the game just passed
        // to leg B, so `+0x10` is an in-bounds read of that live object.
        let r = unsafe { &*regs };
        let ns = r.rcx as usize;
        if ns == 0 {
            log::info!("session-probe: gate-trace legb-entry — NetworkSession (rcx) null");
            return;
        }
        let rd = |off: usize| unsafe { ((ns + off) as *const u32).read_volatile() };
        // [+0x10] = reject #1 readiness flag. [+0x20]/[+0x24] = the session-slot array's capacity/count
        // on the NetworkSession itself: leg B's *tail* stores the new session object at array[count] only
        // if count < capacity (`cmp [+0x24],[+0x20]; jae fail`). If capacity (+0x20) is 0 offline, the
        // store can't happen even after a successful finalize → FailedToCreateSession (the capacity-0
        // hypothesis). See docs/SESSION-DRIVE.md > "Rig result (2026-06-29 …)".
        log::info!(
            "session-probe: gate-trace legb-entry REACHED — NetworkSession={ns:#x} reject#1 [+0x10]={} \
             slot-array [+0x20]cap={} [+0x24]count={} (cap 0 => leg B tail can't store the session)",
            rd(0x10),
            rd(0x20),
            rd(0x24),
        );
    }));
}

/// 4th-gate tracer: reaching here means rejects #1–3 passed. Reads the session-object config fields the
/// gate (`0x1423fd7a0`) + its helper (`0x1423faf60`) require nonzero — all-zero is the offline veto.
/// Read-only; firewalled against unwind across the FFI boundary.
fn log_create_gate4(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: rcx is the freshly-built `0x5f8`-byte session object; every offset read below is
        // well within its bounds. Read-only.
        let r = unsafe { &*regs };
        let o = r.rcx as usize;
        if o == 0 {
            log::info!("session-probe: gate-trace create-gate4 — session obj (rcx) null");
            return;
        }
        let rd = |off: usize| unsafe { ((o + off) as *const u32).read_volatile() };
        log::info!(
            "session-probe: gate-trace create-gate4 REACHED (rejects #1-3 passed) — obj={o:#x} \
             gate[+0x3b0]={} gate[+0x3b4]={} helper[+0x68..0x78]=[{},{},{},{},{}] \
             (gate vetoes iff +0x3b0==0 && +0x3b4==0; helper bails if any of the five is 0)",
            rd(0x3b0),
            rd(0x3b4),
            rd(0x68),
            rd(0x6c),
            rd(0x70),
            rd(0x74),
            rd(0x78),
        );
    }));
}

/// Resolve one initiation-function entry from its (currently-`None`) landmark and place a `jmp-back`
/// logging hook there. Logs and returns on any failure — degrade, never abort.
///
/// **Call-once / init-thread precondition** (same contract as [`crate::saves::install`]): the only
/// caller is `install_hooks` from `app::pre_task_startup`, which runs once on the short-lived init
/// thread before the hooked path can fire. Like every `ilhook` install it rewrites the entry's first
/// bytes *without suspending other threads* (the unsuspended-install race `saves.rs` documents at
/// length), so the charted site must be a real, idle-at-install function prologue. `name` is `'static`
/// because the detour closure captures it for the lifetime of the (forgotten, process-resident) hook.
fn install_one(name: &'static str, site: &Option<HookSite>) {
    let Some(site) = site else {
        log::info!(
            "session-probe: {name} hook AOB not yet charted (rig RE pending); hook inert — see docs/SESSION-RE-RUNBOOK.md"
        );
        return;
    };
    // resolve_landmark bounds-checks the site against the mapped image and verifies the entry opcode,
    // logging any miss under its own `patch '<name>':` prefix; a too-loose/drifted landmark fails safe
    // (no hook placed). The opcode check confirms the *byte*, not that the entry has enough relocatable
    // prologue for the 14-byte jmp-back — confirm the landmark sits on a clean function prologue (see
    // the RIG TODO). Emit a `session-probe:` line on the miss too, so a `grep session-probe:` of an RE
    // log still tells the whole story (the resolve detail is in the adjacent `patch '<name>':` line).
    let Some(addr) = crate::patch::resolve_landmark(name, site.landmark, site.offset, site.expect)
    else {
        log::warn!("session-probe: {name} landmark did not resolve; hook not placed (see patch log above)");
        return;
    };
    // jmp-back so the original initiation runs untouched right after we log — we only observe.
    // SAFETY: `hook_closure_jmp_back` is unsafe (it patches live `.text`); `addr` was bounds-checked +
    // opcode-verified by `resolve_landmark`, and the closure body is panic-firewalled (see `log_initiation`).
    let hook = unsafe {
        hook_closure_jmp_back(
            addr as usize,
            move |regs: *mut Registers| log_initiation(name, regs),
            CallbackOption::None,
            HookFlags::empty(),
        )
    };
    match hook {
        Ok(h) => {
            std::mem::forget(h); // resident for the process lifetime — never unhook a live code path
            log::info!("session-probe: hooked {name} initiation at {:#x}", addr as usize);
        }
        Err(e) => log::error!("session-probe: failed to hook {name}: {e:?}"),
    }
}

/// `jmp-back` detour body for a session create/join initiation call, shared by both entries (they
/// differ only in `name`). Logs the call and the four integer-arg registers (win64 ABI: `rcx`=`this`,
/// then `rdx`/`r8`/`r9`) so the rig RE can read off the candidate `CSSessionManager` pointer (matches
/// the base the FSM logger prints) and the peer SteamID argument.
///
/// Two load-bearing safety properties this body keeps:
/// 1. **No unwind across the FFI boundary.** `ilhook` invokes this from an `extern "win64"` trampoline
///    with no `catch_unwind` of its own; a panic unwinding into game code is UB — the same reason the
///    task-tick path is wrapped in `app::install`. Every shipped profile is now `panic = "unwind"`
///    (release and `diag` alike — see docs/FFI-UNWIND-AUDIT.md), so this firewall is load-bearing in
///    the player's build, not just the rig's diag build; we wrap the body in `catch_unwind` here.
/// 2. **Read-only.** It only reads scalar register values; it never dereferences a handed pointer or
///    writes game memory, so it can't perturb the session it observes.
///
/// The register dump carries an **un-pseudonymized peer SteamID64** once live (a raw SteamID resolves
/// straight to a Steam profile — see [`unseamless_core::diagnostics::peer_tag`]), so at discovery time,
/// when we don't yet know *which* register holds it, the raw dump logs at `debug!` to keep it out of
/// the default `info`-level shareable log. Enable `[debug] verbosity` for an RE run, and don't share
/// that log verbatim; once the SteamID register is identified, route it through `peer_tag`.
fn log_initiation(name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: `regs` points at the saved registers at the hook site (ilhook's contract); we only
        // read scalar fields, never deref a pointer they hold.
        let r = unsafe { &*regs };
        log::debug!(
            "session-probe: {name} initiated | rcx={:#018x} rdx={:#018x} r8={:#018x} r9={:#018x} \
             (rdx/r8/r9 may carry a raw peer SteamID64 — do not share this log verbatim)",
            r.rcx, r.rdx, r.r8, r.r9
        );
    }));
}

/// The frame task that logs every `CSSessionManager` lobby/protocol FSM transition under the
/// `session-probe:` prefix. Registered only when the probe is enabled (see
/// [`crate::app::build_features`]). Distinct from the always-on [`crate::features::observer`], which
/// logs the broader session snapshot (roster, tether, scaling): this one is the tight, greppable FSM
/// trace for a create/join RE run.
pub struct SessionFsmProbe {
    /// Fires only when the lobby/protocol pair changes, so we log transitions, not every frame.
    state: Latch<Fsm>,
    /// "Still alive, no session yet" heartbeat (~30s at 60fps) while idle pre-session.
    heartbeat: FrameThrottle,
}

/// The discrete FSM pair we diff on. Both are `Copy` `repr(u32)` SDK enums, so we keep the named
/// values (for readable `lobby Host->Client` log lines) rather than folding to ints.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fsm {
    lobby: LobbyState,
    protocol: ProtocolState,
}

impl SessionFsmProbe {
    fn new() -> Self {
        Self { state: Latch::new(), heartbeat: FrameThrottle::every(1800) }
    }
}

impl Feature for SessionFsmProbe {
    fn name(&self) -> &'static str {
        "session-fsm-probe"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, tick: Tick) {
        // Read through the shared `crate::session::read` (the same path the observer + diag use, so
        // the probe can't drift from them), taking only the FSM pair from the view, and grab the live
        // base address alongside it for the RE correlation (matching a hooked call's `rcx`).
        let observed = crate::sdk::with_instance::<CSSessionManager, _>(|s| {
            let base = s as *const CSSessionManager as usize;
            let view = crate::session::read(s);
            (base, Fsm { lobby: view.lobby_state, protocol: view.protocol_state })
        });
        let Some((base, fsm)) = observed else {
            // Session not up (or torn down). Re-arm the latch so a later reconnect reprints the
            // baseline line *with the base address* — the `rcx`-correlation anchor a create/join RE run
            // depends on — instead of a bare `lobby A->B` with no fresh base. Reconnect cycling is the
            // target scenario, so don't let a stale terminal pair suppress the re-baseline.
            self.state = Latch::new();
            if self.heartbeat.tick() {
                log::info!("session-probe: live, no CSSessionManager yet (frame {})", tick.frame);
            }
            return;
        };

        // Capture the prior pair before the latch overwrites it, so we can render old->new.
        let prev = self.state.last().copied();
        if !self.state.changed(&fsm) {
            return;
        }
        match prev {
            // First live read: announce the baseline + the base address (so a hooked call's `rcx` can
            // be matched against this known `CSSessionManager` pointer).
            None => log::info!(
                "session-probe: FSM live @frame {} — CSSessionManager @{:#x} lobby={:?} protocol={:?}",
                tick.frame, base, fsm.lobby, fsm.protocol,
            ),
            Some(old) => log::info!(
                "session-probe: FSM @frame {} lobby {:?}->{:?} protocol {:?}->{:?}",
                tick.frame, old.lobby, fsm.lobby, old.protocol, fsm.protocol,
            ),
        }
    }
}

// --- Rung-3 DIRECT-DRIVE probe (experimental) ---------------------------------------------------
//
// Where the FSM logger + entry hooks OBSERVE the create/join initiation, this DRIVES it: a one-shot
// that CALLS the charted create-session wrapper on `[G]` to confirm we can move
// `lobby_state None -> TryToCreateSession` with no in-game item and no peer (the pivot to driving
// `CSSessionManager` directly — docs/SESSION-DRIVE.md + the create chart in docs/SESSION-RE-FINDINGS.md).
//
// Target: the create WRAPPER `bool 0x140cad4c0(this, u8 flag, u32 mode, void* settings)` — chosen over
// the inner because it owns the failure path (sets `lobby_state = 2` + cleanup) so a rejected call
// degrades cleanly instead of leaving half-state. Args are the near-constants the sign/host template
// passes: `mode = 4`, `settings = {u16@0 = 0, u32@4 = 2}`; `flag` comes from sign data in the natural
// path, so we try `0` (tweak [`DRIVE_FLAG`] if a run rejects). The request builder (`0x140cb20d0`)
// calls `is_offline()` twice, so this is meant to run WITH `gameplay.enable_offline_multiplayer = true`.
//
// Re-derive after a game update: the wrapper offset is from the exe's preferred base `0x140000000`; if
// the create chart in docs/SESSION-RE-FINDINGS.md shifts, update [`CREATE_WRAPPER_OFFSET`].

/// Offset of the create wrapper (`0x140cad4c0`) from the exe preferred base (`0x140000000`). Resolved
/// against the live `GetModuleHandle(NULL)` base so it survives a rebase, rather than a hardcoded VA.
const CREATE_WRAPPER_OFFSET: usize = 0x140c_ad4c0 - 0x1_4000_0000;
/// `flag` arg (`dl`). Sign data supplies this in the natural path; `0` is the first guess for a driven
/// create — change here and rebuild if a run lands on `FailedToCreate`.
const DRIVE_FLAG: u8 = 0;
/// `mode` arg (`r8d`) — the constant the sign/host create path passes.
const DRIVE_MODE: u32 = 4;

/// The 8-byte `settings` blob the create path points `r9` at: `{ u16@+0 = 0; u32@+4 = 2 }`. `repr(C)`
/// gives `u16` at 0 (pad 2..4) and `u32` at 4, matching the charted layout. Consumed synchronously by
/// the param builder, so a stack local outlives the call.
// Fields are read by the game through the `r9` pointer (FFI), never by Rust — so they read as dead.
#[allow(dead_code)]
#[repr(C)]
struct CreateSettings {
    a: u16,
    b: u32,
}

/// The create wrapper's win64 signature: `this`(rcx), `flag`(dl), `mode`(r8d), `settings`(r9).
type CreateFn =
    unsafe extern "system" fn(*mut CSSessionManager, u8, u32, *const CreateSettings) -> bool;

/// One-shot driver: when in-game and `lobby_state == None`, call the create wrapper once and log the
/// before/return/after under the `session-probe:` prefix (the FSM logger then traces the transition).
pub struct SessionCreateDriver {
    fired: bool,
    /// When true, satisfy leg B's reject #1 by writing `NetworkSession+0x10` nonzero just before the
    /// create call (`force_netsession_ready` probe). The flag's pre-call value is logged either way.
    force_ready: bool,
}

impl SessionCreateDriver {
    fn new(force_ready: bool) -> Self {
        Self { fired: false, force_ready }
    }
}

/// Resolve the embedded `NetworkSession`'s readiness flag `&*([G]+0x60)+0x710 + 0x10` from the live
/// `CSSessionManager*` — the dword the charted leg-B vmethod (`0x1423f5c00`) tests first (reject #1).
/// Returns `None` if the `*(this+0x60)` pointer is null (manager not fully wired). The chain was
/// charted live (`this->*(this+0x60)->+0x710 = NetworkSession`, vtable slot 1 = leg B) — see
/// `docs/SESSION-DRIVE.md` > "Leg B charted".
fn netsession_ready_flag(base: usize) -> Option<*mut u32> {
    // SAFETY: `base` is the live `CSSessionManager*` (just read from the SDK singleton); `+0x60` holds a
    // pointer `P` into a `.data` singleton. Read it as a pointer, and if non-null, `P+0x710+0x10` is the
    // `NetworkSession` readiness dword. Read-only deref of `base+0x60` here; the caller does any write.
    let p = unsafe { *((base + 0x60) as *const usize) };
    if p == 0 {
        return None;
    }
    Some((p + 0x710 + 0x10) as *mut u32)
}

impl Feature for SessionCreateDriver {
    fn name(&self) -> &'static str {
        "session-create-driver"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Main thread, same context the natural host path runs in — the create issues async work via a
        // vtable call, so it must be driven from the game thread, not our init thread.
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, tick: Tick) {
        if self.fired {
            return;
        }
        // Need a loaded world (the create touches player/world context) — don't fire at the title.
        if !crate::playstate::current().in_game() {
            return;
        }
        // `in_game()` flips true at the load *transition*, before WorldChrMan is populated; driving the
        // create then bails before leg B is even dispatched (rig-observed 2026-06-29: drive fired with
        // bypass+force yet neither gate-trace hook fired). Require the active main player present so the
        // create runs with real world context (matches the create wrapper's own player/world needs).
        if crate::sdk::with_active_main_player(|_| ()).is_none() {
            return;
        }
        // Need the live manager AND lobby_state == None (the inner guards on None; we also want a clean
        // baseline for the FSM logger's transition line).
        let Some((base, lobby)) =
            crate::sdk::with_instance::<CSSessionManager, _>(|s| {
                (s as *const CSSessionManager as usize, crate::session::read(s).lobby_state)
            })
        else {
            return;
        };
        if lobby != LobbyState::None {
            log::info!(
                "session-probe: drive-create skipped — lobby_state is {:?}, need None (already in/at a session)",
                lobby,
            );
            self.fired = true;
            return;
        }

        self.fired = true; // one-shot: set BEFORE the call so a crash/hang can't re-fire it

        let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
            Ok(h) => h.0 as usize,
            Err(e) => {
                log::error!("session-probe: drive-create — GetModuleHandle(NULL) failed: {e}");
                return;
            }
        };
        let fn_addr = exe_base + CREATE_WRAPPER_OFFSET;
        // SAFETY: `fn_addr` is the create wrapper resolved from the live exe base + its charted offset;
        // we call it with this=[G] (the live, non-null singleton just read) and the constant args the
        // natural host path uses, on the main thread, with lobby_state == None (its precondition).
        let create: CreateFn = unsafe { std::mem::transmute::<usize, CreateFn>(fn_addr) };
        let settings = CreateSettings { a: 0, b: 2 };

        // Reject #1 (rung-3): leg B (the network-create vmethod 0x1423f5c00) fails offline iff the dword
        // at NetworkSession+0x10 is 0. Log its pre-call value for confirmation, and — when the
        // force_netsession_ready probe is on — write it nonzero to see if create then proceeds past leg B.
        if let Some(flag) = netsession_ready_flag(base) {
            let before = unsafe { flag.read_volatile() };
            log::info!(
                "session-probe: drive-create — NetworkSession+0x10 (reject#1 flag) = {before} before create",
            );
            if self.force_ready {
                unsafe { flag.write_volatile(1) };
                log::info!(
                    "session-probe: drive-create — forced NetworkSession+0x10 = 1 (satisfy reject #1)",
                );
            }
        } else {
            log::info!("session-probe: drive-create — NetworkSession ptr (*(this+0x60)) null; skipping reject#1 probe");
        }

        log::info!(
            "session-probe: drive-create @frame {} — calling create wrapper {:#x}(this={:#x}, flag={}, mode={}, settings={{0,2}}); lobby was None",
            tick.frame, fn_addr, base, DRIVE_FLAG, DRIVE_MODE,
        );
        let ret = unsafe { create(base as *mut CSSessionManager, DRIVE_FLAG, DRIVE_MODE, &settings) };
        let after = crate::sdk::with_instance::<CSSessionManager, _>(|s| {
            crate::session::read(s).lobby_state
        });
        log::info!(
            "session-probe: drive-create returned {} — lobby_state now {:?} (TryToCreateSession=driven OK; FailedToCreateSession=internal gate rejected)",
            ret,
            after,
        );
    }
}

/// The session probe's gated frame features, for [`crate::app::build_features`] to `extend` with —
/// mirroring [`crate::diag::probe_features`] so every `[debug.probes]`-gated feature is appended the
/// same way (one assembly style, gating kept inside this module). The FSM-transition logger when
/// `session_probe` is on; the experimental [`SessionCreateDriver`] when `drive_create` is on.
pub fn probe_features(config: &Config) -> Vec<Box<dyn Feature>> {
    let mut features: Vec<Box<dyn Feature>> = Vec::new();
    if config.debug.probes.session_probe {
        features.push(Box::new(SessionFsmProbe::new()));
    }
    if config.debug.probes.drive_create {
        features.push(Box::new(SessionCreateDriver::new(config.debug.probes.force_netsession_ready)));
    }
    features
}
