//! Rung-3 RE prep: instrumentation for the **session create/join initiation** — the one networking
//! gap the SDK doesn't chart (see [`docs/COOP-CONNECTION.md`](../../../docs/COOP-CONNECTION.md) >
//! "What the SDK gives us vs. the RE gap" and [`docs/SDK-COVERAGE.md`](../../../docs/SDK-COVERAGE.md)).
//! The SDK gives us the FSM *state* (`CSSessionManager.{lobby_state, protocol_state}`), the roster,
//! and the transport vtable — but **not** the internal functions that drive
//! `lobby_state None -> TryToCreateSession -> Host` (host) and `None -> TryToJoinSession -> Client`
//! (joiner). Static RE has since charted those entries
//! ([`docs/SESSION-RE-FINDINGS.md`](../../../docs/SESSION-RE-FINDINGS.md)); this module instruments
//! them, so the two-player rig run confirms them live cheaply.
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
//! 2. **Create/join entry hooks** ([`install_hooks`]) — a `jmp-back` hook at each charted initiation
//!    entry (the create/join *wrappers*, statically charted 2026-06-27) logs the call and its argument
//!    registers (the candidate `this` pointer + peer SteamID), correlated by frame/timestamp to the
//!    FSM transition the call triggers. Resolved by **fixed offset** from the live exe base like the
//!    gate tracers + [`SessionCreateDriver`] below (no static AOB is derivable — the exe's on-disk
//!    `.text` is Arxan/Steam-encrypted), with a prologue-bytes guard so a drifted offset after a game
//!    update fails safe (warn, no hook) instead of patching the wrong bytes. What remains rig-gated is
//!    only the live *confirm*: watch a real host/join fire these hooks on the two-player run (the
//!    create leg is solo-confirmable — `drive_create` calls the very function the create hook sits on).
//!
//! The hand-off recipe (which two functions, why they're the create/join initiation, what the
//! `session-probe:` lines mean) is [`docs/SESSION-RE-RUNBOOK.md`](../../../docs/SESSION-RE-RUNBOOK.md).
//!
//! ## Clean-room
//! Everything here is grounded in the public SDK (the charted FSM enums/fields) or in our own
//! observations; no upstream ERSC code or decompiler output is transcribed (CLAUDE.md > Clean-room).
//!
//! ## Lifetime & safety
//! The entry hooks (when live) follow the same invariants as [`crate::saves`]: installed once on the
//! init thread, `mem::forget`-ten (resident for the process lifetime — never unhook a live code
//! path). The callbacks are **read-only** — they log register values and never write game memory or
//! dereference a pointer they were handed, so a probe can't perturb the session it's observing. The
//! one exception is the leg-B tracer's opt-in slot-array **fabrication** (`fabricate_slot_array`, off
//! by default): when armed it writes the `NetworkSession`'s empty slot-array fields so a solo create
//! can reach `Host` — a deliberate experiment, gated and guarded (only writes an unallocated array).

use std::sync::atomic::{AtomicBool, Ordering};

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex, LobbyState, ProtocolState};
use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_back};
use unseamless_core::config::Config;
use unseamless_core::util::{FrameThrottle, Latch};
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::core::PCSTR;

use crate::feature::{Feature, Tick};

// ---------------------------------------------------------------------------------------------------
// Create/join initiation entries — CHARTED (static pass 2026-06-27) and wired. Both chains landed
// bottom-up from the `lobby_state` write (docs/SESSION-RE-FINDINGS.md > "Session create/join
// initiation — STATIC chart"):
//   create: driver 0x140a23010 → wrapper 0x140cad4c0 → inner 0x140cb1f70 (store `[this+0xc]=1` @ 0x140cb208e)
//   join:   driver 0x1406fa850 → wrapper 0x140cae640 → inner 0x140cb2470 (store `[this+0xc]=4` @ 0x140cb25f0)
// What remains rig-gated is only the live *confirm* (a real host/join firing these hooks on the
// two-player run) — the hooks below are that instrument.
//
// Wrapper vs. inner: we hook the WRAPPERS, not the inners, per the findings doc ("hooking the
// wrapper is preferable … and is the right altitude to observe the call + args anyway"). The wrapper
// is the outermost *initiation* entry on each chain — the drivers above it are flow plumbing that
// load `this=[G]` out of a request object — and it owns the failure path: it forwards its registers
// to the inner untouched (so the hook reads the same args the inner gets) and on an inner `false`
// sets `lobby_state = FailedToCreateSession/FailedToJoinSession`, so one hook at this altitude sees
// every initiation attempt, synchronously-rejected ones included. Mechanically it's also the safer
// site: both wrappers open with the clean relocatable prologue `88 54 24 10 / 57 / 48 83 ec ..`
// (plenty of position-independent bytes for ilhook's 14-byte jmp-back), where the inners open a
// large 0x3c0 stack frame.
//
// Resolution is by FIXED OFFSET from the live GetModuleHandle(NULL) base, exactly like the gate
// tracers + SessionCreateDriver below: a static AOB is not derivable here (the exe's on-disk .text
// is Arxan/Steam-encrypted, and the shared `88 54 24 10` prologue is too common to be unique
// anyway), so we keep the charted offsets and guard drift by verifying the entry's charted prologue
// bytes before patching — after a game update the check fails safe (warn + no hook), never hooks
// garbage. Re-derive the offsets per docs/SESSION-RE-FINDINGS.md > "Re-derivation recipe" (scan the
// CSSessionManager method block 0x140cad000..0x140cb3000 for the `mov [reg+0xc], imm` setter family;
// the →1/→4 functions are the inners, their sole callers the wrappers).
// ---------------------------------------------------------------------------------------------------

/// Join-initiation entry: the join **wrapper** (`0x140cae640`), offset from the exe preferred base
/// (`0x140000000`). Signature `join(this /*rcx*/, u8 flag /*dl*/, HostBlob* blob /*r8*/, u32 arg4
/// /*r9d*/, stack arg5)` — the peer/host identity (SteamID64) rides inside the `{begin,end}` blob
/// `r8` points at; the hook logs the pointer value only, never derefs (read-only contract). The
/// create entry needs no twin const: it's the same wrapper the drive probe calls,
/// [`CREATE_WRAPPER_OFFSET`].
const JOIN_WRAPPER_OFFSET: usize = 0x140c_ae640 - 0x1_4000_0000;

/// Charted entry bytes of the create wrapper (`88 54 24 10` = `mov [rsp+0x10], dl` spilling the
/// `flag` arg; `57` = `push rdi`; `48 83 EC 30` = `sub rsp, 0x30`), verified at the resolved address
/// before hooking as the anti-drift guard — the same role `expect` plays in
/// [`crate::patch::resolve_landmark`], but multi-byte: the lead `0x88` alone is one of the most
/// common x64 opcode bytes, so a single byte would wave a drifted offset through. Nine bytes reach
/// the `sub rsp` immediate, which also differs between the two wrappers (`0x30` vs `0x40`), so each
/// guard pins its own wrapper, not just "some wrapper-shaped prologue". Source:
/// docs/SESSION-RE-FINDINGS.md > "Landmark hints for `session_probe.rs`".
const CREATE_WRAPPER_PROLOGUE: [u8; 9] = [0x88, 0x54, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x30];
/// The join wrapper's charted entry bytes — same shape as [`CREATE_WRAPPER_PROLOGUE`] but with a
/// `sub rsp, 0x40` frame.
const JOIN_WRAPPER_PROLOGUE: [u8; 9] = [0x88, 0x54, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x40];

/// Install the create/join initiation hooks and the leg-B gate tracers, each per its own config gate.
/// Mirrors [`crate::saves::install`] / [`crate::app::apply_boot_patches`]: call once, on the init
/// thread, at install. Best-effort throughout — a probe never aborts the game (it's not a
/// `guard::fatal` condition; it's a diagnostic).
pub fn install_hooks(config: &Config) {
    install_initiation_hooks(config);
    // Independently gated on `drive_create` (you only want the gate trace alongside a driven create).
    install_create_gate_trace(config);
}

/// Place the read-only create/join initiation hooks when `session_probe` is on. No-op otherwise.
///
/// **Call-once / init-thread precondition** (same contract as [`crate::saves::install`]): the only
/// caller is [`install_hooks`] from `app::pre_task_startup`, which runs once on the short-lived init
/// thread before the hooked paths can fire. Like every `ilhook` install this rewrites the entries'
/// first bytes *without suspending other threads* (the unsuspended-install race `saves.rs` documents
/// at length) — safe here because the wrappers are event-driven initiation entries (called once off
/// a host/join action), necessarily idle at boot-time install.
fn install_initiation_hooks(config: &Config) {
    if !config.debug.probes.session_probe {
        return;
    }
    let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
        Ok(h) => h.0 as usize,
        Err(e) => {
            log::error!("session-probe: initiation hooks — GetModuleHandle(NULL) failed: {e}");
            return;
        }
    };
    // The create entry reuses the drive probe's [`CREATE_WRAPPER_OFFSET`] (`0x140cad4c0`): the hook
    // sits on exactly the function `drive_create` calls, so a driven create also fires it — a free
    // solo self-test of the hook before the two-player run.
    install_initiation_hook("create-session", exe_base + CREATE_WRAPPER_OFFSET, &CREATE_WRAPPER_PROLOGUE);
    install_initiation_hook("join-session", exe_base + JOIN_WRAPPER_OFFSET, &JOIN_WRAPPER_PROLOGUE);
}

/// Verify the wrapper's charted prologue bytes at `addr`, then place the read-only
/// [`log_initiation`] `jmp-back` hook there. Logs and returns on any failure — degrade, never abort.
/// `name` is `'static` because the detour closure captures it for the lifetime of the (forgotten,
/// process-resident) hook.
fn install_initiation_hook(name: &'static str, addr: usize, expect: &'static [u8]) {
    // Anti-drift guard: confirm the charted entry is still what we expect before rewriting live
    // code — after a game update a stale offset would land mid-function and patch garbage. The warn
    // line dumps the observed bytes, so a drift report doubles as the first re-chart datum.
    // SAFETY: `addr..addr+expect.len()` is the live exe base + a charted `.text` offset — inside the
    // mapped, readable image (the image is orders of magnitude larger than the offset). Read-only,
    // byte-at-a-time (no reference formed over memory we don't own).
    let seen: Vec<u8> =
        (0..expect.len()).map(|i| unsafe { ((addr + i) as *const u8).read_volatile() }).collect();
    if seen != expect {
        log::warn!(
            "session-probe: {name} entry at {addr:#x} reads {seen:02x?}, expected {expect:02x?} \
             (offset drifted — game update?); hook not placed. Re-chart per docs/SESSION-RE-FINDINGS.md"
        );
        return;
    }
    // jmp-back so the original initiation runs untouched right after we log — we only observe.
    match place_jmp_back_hook(name, addr, log_initiation) {
        Ok(()) => log::info!("session-probe: hooked {name} initiation at {addr:#x}"),
        Err(e) => log::error!("session-probe: failed to hook {name}: {e}"),
    }
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

/// Offsets of the session-slot array's control fields on the `NetworkSession` (`rcx` at leg-B entry),
/// charted static (docs/SESSION-DRIVE.md > "Slot-array allocator charted"). Leg B's tail store is
/// `if (count < capacity) base[count++] = session_obj;` — a bounded, no-grow push, so the array must
/// be pre-sized. Offline all three are zero.
const SLOT_ARRAY_BASE_OFF: usize = 0x18; // `T* base` (array of session-object pointers)
const SLOT_ARRAY_CAP_OFF: usize = 0x20; //  `u32 capacity`
const SLOT_ARRAY_COUNT_OFF: usize = 0x24; // `u32 count`

/// Capacity to fabricate into an empty slot array (see [`FABRICATE_SLOT_ARRAY`]). One seat suffices to
/// land the host's own session object at slot 0 and reach `Host`; we size for a generous co-op party so
/// later peer inserts (each another leg-B push) have room without another fabrication.
const FABRICATED_SLOT_CAPACITY: u32 = 16;

/// Leg-B **cleanup target** (`0x1423f5cd2`) — the block reached when create fails, from *either* the
/// finalize-handle test (`je` at `0x1423f5cb9`) or the slot-capacity check (`jae`). At this point `esi`
/// still holds the finalize handle from `0x1423f5cb5` (`mov esi,eax`), `rdi` = the session object, and
/// `rbx` = the `NetworkSession`. We hook here rather than the mid-branch `0x1423f5cb5` because the
/// overwritten bytes are clean `mov`s (no rel8 branch for ilhook to relocate) and it fires exactly on
/// the failure we're studying. `esi==0` ⇒ finalize (`0x1423fab40`) returned a zero registry-node id;
/// `esi!=0` ⇒ the capacity branch failed instead. See docs/SESSION-DRIVE.md > "Leg B Post-Capacity
/// Tail Charted".
const LEGB_FINALIZE_OFFSET: usize = 0x1_423f_5cd2 - 0x1_4000_0000;
/// Charted bytes at [`LEGB_FINALIZE_OFFSET`]: `48 8B 07` = `mov rax,[rdi]`, `48 8B CF` = `mov rcx,rdi`,
/// `FF 50 10` = `call [rax+0x10]`, `48 8B 43 08` = `mov rax,[rbx+8]`. Verified before hooking — this is
/// a mid-function hook, so a drifted offset must not relocate the wrong instructions; a mismatch skips.
const LEGB_FINALIZE_PROLOGUE: [u8; 13] =
    [0x48, 0x8B, 0x07, 0x48, 0x8B, 0xCF, 0xFF, 0x50, 0x10, 0x48, 0x8B, 0x43, 0x08];
/// On the freshly-built session object, `+0x58` holds the pointer copied from `NetworkSession+0x08`;
/// `+0x6b8` off *that* is the registry-node id counter that `0x1423fa100` consumes as a new node's id
/// (then increments). Starting at 0 ⇒ the first node's id is 0 ⇒ finalize returns 0. Charted static in
/// docs/SESSION-DRIVE.md > "Leg B Post-Capacity Tail Charted".
const SESSION_OBJ_SUB_OFF: usize = 0x58;
const REGISTRY_NEXT_ID_OFF: usize = 0x6b8;

/// Charted first bytes of the leg-B entry (`0x1423f5c00`): `40 57` = `push rdi` (redundant REX),
/// `48 83 EC 40` = `sub rsp,0x40`, `48 C7 44 24` = the start of `mov qword [rsp+0x20], -2`. Verified at
/// the resolved address before the fabrication lever is armed — the same anti-drift role
/// [`CREATE_WRAPPER_PROLOGUE`] plays for the initiation hooks. The read-only tracers tolerate a drift
/// (a stale offset just logs the wrong bytes), but fabrication *writes* game memory keyed off `rcx`, so
/// a drifted offset there would scribble a base/capacity into an unknown object — this guard refuses to
/// arm the write unless the entry still looks like leg B. Re-chart per docs/SESSION-DRIVE.md if a game
/// update shifts it.
const LEGB_ENTRY_PROLOGUE: [u8; 10] =
    [0x40, 0x57, 0x48, 0x83, 0xEC, 0x40, 0x48, 0xC7, 0x44, 0x24];

/// Armed by [`install_create_gate_trace`] from `[debug.probes] fabricate_slot_array` (only after the
/// leg-B prologue verifies — see [`LEGB_ENTRY_PROLOGUE`]). When set, the leg-B entry tracer
/// ([`log_legb_entry`]) fabricates the session-slot array if it's still empty — the one write the
/// otherwise read-only tracer performs, kept behind this flag so the default tracer stays a pure
/// observer. See the `[debug.probes] fabricate_slot_array` config flag.
static FABRICATE_SLOT_ARRAY: AtomicBool = AtomicBool::new(false);

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
    // Arm the slot-array fabrication (read by the leg-B tracer) — but only if requested AND the leg-B
    // entry still carries its charted prologue. This must run BEFORE the hook is placed (ilhook
    // overwrites the entry bytes we verify), and gating the write on the prologue keeps a post-update
    // offset drift from turning the fabrication into a stray write at an arbitrary address. The
    // read-only trace installs regardless; only the *write* is withheld on drift.
    if config.debug.probes.fabricate_slot_array {
        let arm = legb_prologue_ok(exe_base + LEGB_ENTRY_OFFSET);
        FABRICATE_SLOT_ARRAY.store(arm, Ordering::Relaxed);
        if !arm {
            log::warn!(
                "session-probe: fabricate_slot_array requested but leg-B entry prologue didn't verify \
                 (offset drifted — game update?); NOT arming the slot-array write. Re-chart per \
                 docs/SESSION-DRIVE.md"
            );
        }
    }
    install_offset_hook("legb-entry", exe_base + LEGB_ENTRY_OFFSET, log_legb_entry);
    install_offset_hook("create-gate4", exe_base + CREATE_GATE4_OFFSET, log_create_gate4);
    // legb-finalize (the post-capacity failure cause). Guarded on its charted bytes because it's a
    // *mid-function* hook — a drifted offset there could relocate the wrong instructions — so unlike the
    // two entry hooks above we skip it on a byte mismatch rather than log wrong data.
    let fin = exe_base + LEGB_FINALIZE_OFFSET;
    if prologue_ok("legb-finalize", fin, &LEGB_FINALIZE_PROLOGUE) {
        install_offset_hook("legb-finalize", fin, log_legb_finalize);
    }
}

/// True iff the charted `expected` bytes are present at `addr`. Read-only, byte-at-a-time. The
/// anti-drift guard both the fabrication write (leg-B entry) and the mid-function `legb-finalize` hook
/// gate on — a game update that shifts an offset reads different bytes and withholds the risky action.
fn prologue_ok(name: &str, addr: usize, expected: &[u8]) -> bool {
    // SAFETY: `addr` = live exe base + a charted `.text` offset — inside the mapped, readable image
    // (orders of magnitude larger than the offset). Read-only, no reference over foreign memory.
    let seen: Vec<u8> = (0..expected.len())
        .map(|i| unsafe { ((addr + i) as *const u8).read_volatile() })
        .collect();
    if seen != expected {
        log::warn!("session-probe: {name} at {addr:#x} reads {seen:02x?}, expected {expected:02x?}");
        return false;
    }
    true
}

/// True iff the charted [`LEGB_ENTRY_PROLOGUE`] is present at `addr` (the fabrication-write guard).
fn legb_prologue_ok(addr: usize) -> bool {
    prologue_ok("leg-B entry", addr, &LEGB_ENTRY_PROLOGUE)
}

/// Place one read-only `jmp-back` hook at a resolved address for the gate tracers, logging the
/// outcome under their `gate-trace` tag.
fn install_offset_hook(name: &'static str, addr: usize, body: fn(&'static str, *mut Registers)) {
    match place_jmp_back_hook(name, addr, body) {
        Ok(()) => log::info!("session-probe: gate-trace hooked {name} at {addr:#x}"),
        Err(e) => log::error!("session-probe: gate-trace failed to hook {name}: {e}"),
    }
}

/// Place one read-only `jmp-back` hook at a resolved address, `mem::forget`ing the handle (resident
/// for the process lifetime, like every hook here — never unhook a live code path). Outcome logging
/// is the caller's (each hook surface has its own log-line contract); a failure comes back with the
/// ilhook error pre-rendered.
fn place_jmp_back_hook(
    name: &'static str,
    addr: usize,
    body: fn(&'static str, *mut Registers),
) -> Result<(), String> {
    // SAFETY: `addr` is a charted, clean function entry (exe base + a fixed offset; the initiation
    // sites are additionally prologue-verified by their caller); the detour bodies are panic-firewalled
    // and read-only, except the leg-B tracer's opt-in `fabricate_slot_array` write, which is itself
    // gated on a prologue check before it's armed (see the log_* fns).
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
            Ok(())
        }
        Err(e) => Err(format!("{e:?}")),
    }
}

/// Leg-B entry tracer: confirms we reach leg B and reads reject #1's readiness flag (`[NetworkSession
/// +0x10]`) at the real call site. Read-only **except** the opt-in [`fabricate_slot_array`] write when
/// `fabricate_slot_array` is armed. Firewalled against unwind across the FFI boundary.
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
        // NB: rig-guide `rung3-create-drive`'s drive-watch branch matches the `cap=0 ` token below (the
        // trailing space is load-bearing — it pins the exact zero); keep the `[+0x20]cap={}` rendering.
        log::info!(
            "session-probe: gate-trace legb-entry REACHED — NetworkSession={ns:#x} reject#1 [+0x10]={} \
             slot-array [+0x20]cap={} [+0x24]count={} (cap 0 => leg B tail can't store the session)",
            rd(0x10),
            rd(SLOT_ARRAY_CAP_OFF),
            rd(SLOT_ARRAY_COUNT_OFF),
        );
        // Slot-array fabrication (only when armed by `fabricate_slot_array`). We're at leg-B entry, so
        // `ns` (rcx) is the exact NetworkSession leg B will store into and we run BEFORE its tail
        // `cmp count,capacity`; sizing the array here is what lets the tail push succeed offline.
        if FABRICATE_SLOT_ARRAY.load(Ordering::Relaxed) {
            fabricate_slot_array(ns);
        }
    }));
}

/// Fabricate an empty session-slot array on the `NetworkSession` at `ns` (rcx at leg-B entry) so leg
/// B's tail store has room. No-op unless the array is still unallocated (both `capacity @+0x20` and
/// `base @+0x18` read 0), so we never clobber one a real session set up. Points `base` at a leaked,
/// zero-filled pointer buffer, sets `capacity` = [`FABRICATED_SLOT_CAPACITY`], and explicitly zeroes
/// `count @+0x24` so the buffer is indexed from slot 0 (leg B's tail store is `base[count++]`, so a
/// stale nonzero count would store past slot 0 / past the buffer — offline `count` is 0, but we don't
/// rely on that).
///
/// The buffer is process-**leaked** on purpose: leg B stores raw pointers into it and the session then
/// reads them, so it must outlive the session, and we have no clean unhook point. The tradeoff (the
/// game may try to free this foreign pointer on teardown) is documented on the
/// `[debug.probes] fabricate_slot_array` config flag and is acceptable for a one-shot *does-create-
/// reach-Host?* proof — the free would be at disconnect, after the transition we measure.
fn fabricate_slot_array(ns: usize) {
    // SAFETY: `ns` is the live NetworkSession leg B was just handed (rcx); `+0x18/+0x20/+0x24` are its
    // slot-array control fields, well within the object. Read the current capacity/base to decide, then
    // (only if empty) write the fabricated base+capacity. Data writes on a heap object — no code patch,
    // no VirtualProtect needed.
    let cap_ptr = (ns + SLOT_ARRAY_CAP_OFF) as *mut u32;
    let base_ptr = (ns + SLOT_ARRAY_BASE_OFF) as *mut usize;
    let count_ptr = (ns + SLOT_ARRAY_COUNT_OFF) as *mut u32;
    let cap = unsafe { cap_ptr.read_volatile() };
    let base = unsafe { base_ptr.read_volatile() };
    if cap != 0 || base != 0 {
        log::info!(
            "session-probe: fabricate-slot-array — already sized (cap={cap} base={base:#x}); leaving intact",
        );
        return;
    }
    // Leaked, zero-filled backing store of `FABRICATED_SLOT_CAPACITY` pointer slots. `Vec::leak` gives
    // a `'static` slice whose storage is never reclaimed by us — a stable base for the process lifetime.
    let buf: &'static mut [usize] = vec![0usize; FABRICATED_SLOT_CAPACITY as usize].leak();
    let new_base = buf.as_mut_ptr() as usize;
    // Write base first, then count=0, then capacity last: capacity is the field leg B's tail gates on
    // (`cmp count,capacity`), so publishing it last means any reader that sees a nonzero capacity also
    // sees the matching base + a from-zero count. Zeroing count makes the "indexed from slot 0"
    // postcondition real rather than assumed (offline it's already 0). All are `NetworkSession`-thread
    // sequential (leg B is driven synchronously on the main thread), so there's no cross-thread reader.
    unsafe {
        base_ptr.write_volatile(new_base);
        count_ptr.write_volatile(0);
        cap_ptr.write_volatile(FABRICATED_SLOT_CAPACITY);
    }
    log::info!(
        "session-probe: fabricate-slot-array — sized empty array on NetworkSession={ns:#x}: \
         base(+0x18)={new_base:#x} capacity(+0x20)={FABRICATED_SLOT_CAPACITY} count(+0x24)=0 \
         (leg B tail can now store the session; buffer is process-leaked — teardown caveat applies)",
    );
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

/// legb-finalize tracer: fires at leg B's cleanup block (`0x1423f5cd2`, the create-failure path).
/// Reports the finalize handle (`esi`), the registry-id counter *after* finalize, and the slot-array
/// cap/count — the datum that distinguishes *why* the fabricate+peer drive still fails
/// (docs/SESSION-DRIVE.md > "Leg B Post-Capacity Tail Charted"): `handle=0` means finalize
/// (`0x1423fab40`) returned a zero registry-node id, and `post-next-id=1` with fabrication armed means
/// id `0` was consumed pre-store — i.e. the counter at `[[NetworkSession+0x08]+0x6b8]` started at 0.
/// `handle!=0` would instead point at the capacity branch (fabrication not landing on the object leg B
/// used). Read-only; unwind-firewalled.
fn log_legb_finalize(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: at this charted cleanup block `esi` = the finalize handle, `rdi` = the session object,
        // `rbx` = the NetworkSession (all set upstream in leg B). The field reads below are in-bounds on
        // those live objects; all read-only.
        let r = unsafe { &*regs };
        let handle = r.rsi as u32;
        let session_obj = r.rdi as usize;
        let ns = r.rbx as usize;
        let (cap, count) = if ns != 0 {
            unsafe {
                (
                    ((ns + SLOT_ARRAY_CAP_OFF) as *const u32).read_volatile(),
                    ((ns + SLOT_ARRAY_COUNT_OFF) as *const u32).read_volatile(),
                )
            }
        } else {
            (0, 0)
        };
        // post-finalize registry-id counter: `*(*(session_obj+0x58)+0x6b8)`. If finalize consumed id 0
        // (the zero-handle model), the post-increment read is 1. `None` if a pointer link is null.
        let next_id = if session_obj != 0 {
            let sub = unsafe { ((session_obj + SESSION_OBJ_SUB_OFF) as *const usize).read_volatile() };
            (sub != 0).then(|| unsafe { ((sub + REGISTRY_NEXT_ID_OFF) as *const u32).read_volatile() })
        } else {
            None
        };
        log::info!(
            "session-probe: gate-trace legb-finalize REACHED (create failed) — handle(esi)={handle} \
             post-next-id={next_id:?} slot-array cap={cap} count={count} \
             (handle 0 => finalize 0x1423fab40 returned a zero registry-node id; post-next-id 1 with \
             fabricate armed => id 0 was consumed before the slot store)",
        );
    }));
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
// `lobby_state None -> TryToCreateSession` with no in-game item (the pivot to driving
// `CSSessionManager` directly — docs/SESSION-DRIVE.md + the create chart in docs/SESSION-RE-FINDINGS.md).
// It fires only once the rung-2 side-channel is linked (+ a settle), so the two-machine rung-3 run
// drives create with a real peer present — see docs/RUNG3-DRIVE-RUNBOOK.md.
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
/// Shared with the create-initiation hook ([`install_initiation_hooks`]): observer and driver point
/// at the same charted function, so they can't drift apart.
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

/// How long the driver holds fire after the side-channel link is first observed (~1.5s at 60fps),
/// so the Steam lobby + roster behind the link are fully live before the driven create reads them.
/// See docs/RUNG3-DRIVE-RUNBOOK.md > "Prerequisite — re-time the driver".
const LINK_SETTLE_FRAMES: u64 = 90;

/// One-shot driver: when in-game with the rung-2 side-channel linked (plus a short settle) and
/// `lobby_state == None`, call the create wrapper once and log the before/return/after under the
/// `session-probe:` prefix (the FSM logger then traces the transition). The link precondition is the
/// point of the two-machine rung-3 run: a live peer is what is hypothesized to size leg B's
/// session-slot array, so a create driven *before* any peer exists (the old first-in-game-frame
/// timing) only reproduces the solo capacity-0 failure.
pub struct SessionCreateDriver {
    fired: bool,
    /// Frame the side-channel link was first observed on (with the in-game preconditions above it
    /// already met), anchoring the [`LINK_SETTLE_FRAMES`] delay. Cleared if the link drops
    /// mid-settle, so the settle restarts against a recovered link.
    linked_since: Option<u64>,
    /// When true, satisfy leg B's reject #1 by writing `NetworkSession+0x10` nonzero just before the
    /// create call (`force_netsession_ready` probe). The flag's pre-call value is logged either way.
    force_ready: bool,
    /// When true (`[debug.probes] drive_fire_solo`), this is a **solo** run: fire once the in-game
    /// preconditions are met, without waiting for a rung-2 link (a solo `fabricate_slot_array` proof
    /// has no peer to link). Without it, the drive holds for a linked peer (the two-machine run).
    /// Decoupled from `fabricate_slot_array` itself so fabricate+peer can be tested together.
    fire_solo: bool,
}

impl SessionCreateDriver {
    fn new(force_ready: bool, fire_solo: bool) -> Self {
        Self { fired: false, linked_since: None, force_ready, fire_solo }
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
        // Hold fire until the rung-2 side-channel is LINKED, plus a short settle: the whole point of
        // the two-machine run is driving create WITH a peer present, and Open/Join (the rung-4 lobby +
        // rung-2 link) only come up after the in-game conditions above — firing on the first in-game
        // frame would always beat them (docs/RUNG3-DRIVE-RUNBOOK.md > "Prerequisite"). The link is a
        // separate Steam lobby, not the game's session FSM, so `lobby_state` stays None here and the
        // None precondition below remains correct.
        //
        // EXCEPT a `drive_fire_solo` run (e.g. a solo fabricate_slot_array proof): there's no link to
        // wait for — fire once the in-game preconditions are met (settle anchored off the first
        // eligible frame instead of a link edge).
        if !self.fire_solo && !crate::coop::is_linked() {
            // Link dropped (or never came up): restart the settle from the next link edge.
            self.linked_since = None;
            return;
        }
        let linked_since = *self.linked_since.get_or_insert_with(|| {
            let trigger = if self.fire_solo { "solo (drive_fire_solo)" } else { "side-channel linked" };
            log::info!(
                "session-probe: drive-create armed @frame {} — {trigger}; firing after \
                 {LINK_SETTLE_FRAMES}-frame settle",
                tick.frame,
            );
            tick.frame
        });
        if tick.frame.saturating_sub(linked_since) < LINK_SETTLE_FRAMES {
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
            // rig-guide `rung3-create-drive`'s drive-watch also finishes on `drive-create skipped`
            // (routes to its inspect step) — keep that substring.
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
        // rig-guide `rung3-create-drive`'s drive-watch auto-finishes on `drive-create returned` and
        // branches on `drive-create returned true` / `... false` — keep those substrings verbatim.
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
        features.push(Box::new(SessionCreateDriver::new(
            config.debug.probes.force_netsession_ready,
            config.debug.probes.drive_fire_solo,
        )));
    }
    features
}
