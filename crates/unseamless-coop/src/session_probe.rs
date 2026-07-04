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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Leg-B **finalize-result site** (`0x1423f5cb5`) — the instruction right after `call 0x1423fab40`
/// (`mov esi,eax`), so `eax` here IS the finalize handle. Unlike [`LEGB_FINALIZE_OFFSET`] (which fires
/// only on the *failure* cleanup path) this fires on **every** leg-B finalize, so it settles whether
/// leg B even reaches its tail and what the handle is — the datum the first cycle left open (the
/// cleanup hook never fired, so create either bailed before the tail or *succeeded* past the store and
/// failed later). Mid-function hook over a rel8 `je`, so a byte-guard gates it and a failed relocate is
/// logged + skipped. See docs/SESSION-DRIVE.md > "Leg B post-capacity tail".
const LEGB_FINHANDLE_OFFSET: usize = 0x1_423f_5cb5 - 0x1_4000_0000;
/// Charted bytes at [`LEGB_FINHANDLE_OFFSET`]: `8B F0` = `mov esi,eax`, `85 C0` = `test eax,eax`,
/// `74 17` = `je +0x17` (to the cleanup block), `8B 43 24` = `mov eax,[rbx+0x24]`, `3B 43 20` = `cmp
/// eax,[rbx+0x20]`, `73 0F` = `jae +0xf`. Verified before hooking (mid-function, so drift must not
/// relocate the wrong window); a mismatch skips.
const LEGB_FINHANDLE_PROLOGUE: [u8; 14] = [
    0x8B, 0xF0, 0x85, 0xC0, 0x74, 0x17, 0x8B, 0x43, 0x24, 0x3B, 0x43, 0x20, 0x73, 0x0F,
];

/// **Hook A — create-gate4's read of its helper's return** (`0x1423fd7c8`, the `test al,al` right
/// after `call 0x1423faf60` in gate4 `0x1423fd7a0`). `al` here is the helper's verdict; `al==0` ⇒ gate4
/// returns false ⇒ leg B skips the finalize path ⇒ create fails. See docs/SESSION-DRIVE.md >
/// "create-gate4 Helper 0x1423faf60 Charted".
const GATE4_HELPER_RET_OFFSET: usize = 0x1_423f_d7c8 - 0x1_4000_0000;
/// Charted bytes at [`GATE4_HELPER_RET_OFFSET`]: `84 C0` = `test al,al`, `74 EF` = `je 0x1423fd7bb`
/// (the `xor al,al; ret` false-exit), `48 8B 43 58` = `mov rax,[rbx+0x58]`, `BA 28` = `mov edx,0x28…`.
const GATE4_HELPER_RET_PROLOGUE: [u8; 10] =
    [0x84, 0xC0, 0x74, 0xEF, 0x48, 0x8B, 0x43, 0x58, 0xBA, 0x28];
/// **Hook B — the decisive Arxan-encoded vmethod's verdict, inside the helper** (`0x1423fafcc`, the
/// `test al,al` right after `call [container_vtable+8]`). `al` = the encoded vmethod's result; `al==0`
/// proves that vmethod is the in-world veto (the money datum — it's statically undecodable, so only a
/// live read localizes it). `je 0x1423fb1cb` on false. See docs/SESSION-DRIVE.md.
const GATE4_VMETHOD_OFFSET: usize = 0x1_423f_afcc - 0x1_4000_0000;
/// Charted bytes at [`GATE4_VMETHOD_OFFSET`]: `84 C0` = `test al,al`, `0F 84 F7 01 00 00` = `je
/// 0x1423fb1cb` (rel32), `48 8D` = the start of the `lea` on the pass path.
const GATE4_VMETHOD_PROLOGUE: [u8; 10] =
    [0x84, 0xC0, 0x0F, 0x84, 0xF7, 0x01, 0x00, 0x00, 0x48, 0x8D];

/// **L3 capture — the live vmethod target, read at the helper's call site.** The helper calls
/// `[[container]+8]` where the container's vtable is loaded at runtime from `[container]`
/// (`0x1423fafc1: mov rax,[rcx]` → `0x1423fafc9: call [rax+8]`). The static vtable `0x1431f8360` is
/// *not* the live object's vtable (a trampoline hook keyed off it never fired), so we read the real
/// vtable + target live. Hook at `0x1423fafc4` (`lea rdx,[rsp+0x40]`, the instruction after `mov
/// rax,[rcx]`), where `rax` = the live container vtable, so `[rax+8]` = the actual vmethod pointer —
/// the function whose predicate is the create veto. Clean 5-byte `lea`, no collision with Hook B.
const VMETHOD_TARGET_OFFSET: usize = 0x1_423f_afc4 - 0x1_4000_0000;
/// Charted bytes at [`VMETHOD_TARGET_OFFSET`]: `48 8D 54 24 40` = `lea rdx,[rsp+0x40]`, `FF 50 08` =
/// `call [rax+8]`. Guarded (mid-function) so drift skips rather than scribbles.
const VMETHOD_TARGET_PROLOGUE: [u8; 8] = [0x48, 0x8D, 0x54, 0x24, 0x40, 0xFF, 0x50, 0x08];
/// Latched once the live vmethod target is captured + logged.
static VMETHOD_TARGET_CAPTURED: AtomicBool = AtomicBool::new(false);

/// **Establish-handler bail localizers.** The handler `0x1423f2820` returns 0 (failure) offline but its
/// cleanup `0x1423f2f30` resets `+0x41`/`+0x42` on *every* bail, so those flags can't tell us WHERE it
/// bailed. The flow (own words from disasm) is: gate1 `0x1423f5190` (readiness) → gate2 `call
/// [vtable+0x68]` (= `0x1423f4870`, the session-established handler) at `0x1423f2899` → `[container+0xa0]
/// & 0x40` bit (clear ⇒ copy-descriptor path, returns success WITHOUT building) → builder `call
/// [vtable+0x80]` (= `0x1423f46b0`, a plain fn; NOT the Arxan trampoline the base vtable `0x1431f8360`
/// pointed at — the *live* derived vtable is `0x1431f8780`). Two read-only latched localizers pin the
/// bail:
///  - **gate2-ret** at `0x1423f289c` (`test al,al` after `call [rax+0x68]`): `al` = the session-established
///    handler's return. `al==0` ⇒ the handler bails here (before the builder).
///  - **builder-entry** at the builder thunk `0x1423f46b0`: fires IFF the handler reaches the builder;
///    `rcx`=container, `rdx`=`&local_struct`, `r8b`=`[desc+0x3d]` (the sub-builder selector). `[rdx]` =
///    `[container+0x48]` (heap ptr) is the builder's first-qword input.
const GATE2_RET_OFFSET: usize = 0x1_423f_289c - 0x1_4000_0000;
/// Charted bytes at [`GATE2_RET_OFFSET`]: `84 C0` = `test al,al`, `0F 84 F0 00 00 00` = `je` (long).
const GATE2_RET_PROLOGUE: [u8; 8] = [0x84, 0xC0, 0x0F, 0x84, 0xF0, 0x00, 0x00, 0x00];
/// Latched once the gate2 (session-established) return is captured + logged.
static GATE2_RET_CAPTURED: AtomicBool = AtomicBool::new(false);
/// The live derived-vtable builder thunk (`container->vtable[0x80]` on the live `ManagerImplSteam` vtable
/// `0x1431f8780`). `mov rcx,rdx; test r8b,r8b; jne 0x1426372e0; jmp 0x142637440` — dispatches on
/// `[desc+0x3d]` between two DLNW3D builders.
const BUILDER_ENTRY_OFFSET: usize = 0x1_423f_46b0 - 0x1_4000_0000;
/// Charted bytes at [`BUILDER_ENTRY_OFFSET`]: `48 8B CA` = `mov rcx,rdx`, `45 84 C0` = `test r8b,r8b`.
const BUILDER_ENTRY_PROLOGUE: [u8; 6] = [0x48, 0x8B, 0xCA, 0x45, 0x84, 0xC0];
/// Latched once the builder entry is observed (proves the handler reached the builder).
static BUILDER_ENTRY_CAPTURED: AtomicBool = AtomicBool::new(false);

// NOTE: an earlier "svc-standup" localizer hooked 0x14263ce9c (`mov rcx,rax` after the service-standup
// factory 0x142638b40) to read the factory's return. It is REMOVED because a jmp-back hook mid-way
// through that deep transport function was unstable — it perturbed rax/flags so the sub-init's
// `jne 0x14263ceb4` misfired into the teardown path, faulting (write to 0x0 at 0x14263cea7). Without
// the hook the same drive returns 0 CLEANLY, which is the reliable reading: the SteamServiceImpl
// standup returns null offline (the socket-manager sub-init 0x14263ce40 takes its clean failure path).
// If the factory's return must be confirmed again, hook the factory at a FUNCTION BOUNDARY (its entry
// + a return trampoline), not mid-caller. See docs/SESSION-DRIVE.md.

/// **The real create-veto vmethod** (`0x1423f4330`, live `[container_vtable+8]`). Its first predicate
/// is `mov eax,[rcx+0x7c0]; shr eax,2; test al,1; je return-false` — i.e. **bit 2 of the dword at
/// `[container+0x7c0]`** must be set or the vmethod returns false (create veto). `rcx` at entry = the
/// container (`[session_obj+0x58]`). We hook the entry to read that field and confirm bit 2 is the
/// offline veto. See docs/SESSION-DRIVE.md.
const VETO_VMETHOD_OFFSET: usize = 0x1_423f_4330 - 0x1_4000_0000;
/// Charted entry bytes at [`VETO_VMETHOD_OFFSET`]: `40 57` = `push rdi` (redundant REX, like leg B's
/// entry), `48 81 EC A0 00 00 00` = `sub rsp,0xa0`.
const VETO_VMETHOD_PROLOGUE: [u8; 8] = [0x40, 0x57, 0x48, 0x81, 0xEC, 0xA0, 0x00, 0x00];
/// Field on the container the veto vmethod reads; bit 2 gates create.
const VETO_FIELD_OFF: usize = 0x7c0;
/// Latched once the veto field is read + logged.
static VETO_FIELD_READ: AtomicBool = AtomicBool::new(false);
/// Armed by `[debug.probes] set_create_veto_bit`: the veto-field hook writes bit 2 set on the live
/// container before the vmethod reads it, to test whether create then passes (the L3 lever).
static SET_CREATE_VETO_BIT: AtomicBool = AtomicBool::new(false);
/// Armed by `[debug.probes] drive_session_established`: call the real container session-established
/// handler (`0x1423f4870`) to populate the container for real, instead of fabricating stubs.
static DRIVE_SESSION_ESTABLISHED: AtomicBool = AtomicBool::new(false);
/// Resolved absolute address of the session-established handler `0x1423f4870` (set at install).
static SESSION_ESTABLISHED_FN: AtomicUsize = AtomicUsize::new(0);
/// `0x1423f4870` = `ManagerImplSteam@DLNR3D`'s session-established handler (vtable slot +0x68).
const SESSION_ESTABLISHED_OFFSET: usize = 0x1_423f_4870 - 0x1_4000_0000;

/// Armed by `[debug.probes] drive_establish_handler`: at the veto hook, drive the game's own
/// connection-establish handler `0x1423f2820(container, descriptor)` (which calls the Arxan native builder
/// `container->vtable[0x80]`) to build + wrap + store a connection at `[container+0x708]` natively.
static DRIVE_ESTABLISH_HANDLER: AtomicBool = AtomicBool::new(false);
/// Resolved absolute address of the connection-establish handler `0x1423f2820` (set at install).
static ESTABLISH_HANDLER_FN: AtomicUsize = AtomicUsize::new(0);
/// `0x1423f2820` = `ManagerImpl@DLNR3D`'s connection-establish handler: `container->vtable[0x80]` builds
/// the raw connection, `0x1423f7180` wraps it, it's stored at `[container+0x708]` + addref'd.
const ESTABLISH_HANDLER_OFFSET: usize = 0x1_423f_2820 - 0x1_4000_0000;

/// Armed by `[debug.probes] land_socket_holder`: at the veto-vmethod hook, build a real
/// `SocketManagerHolder@DLNR3D` around the standup connection and land it at `[container+0x708]` — the
/// refcounted DLNR3D wrapper the driven create's `ConnectionRefInfo` loop reads + addrefs. Supersedes the
/// hollow `+0x708` fabrication. See docs/SESSION-DRIVE.md > "SEAM CHARTED".
static LAND_SOCKET_HOLDER: AtomicBool = AtomicBool::new(false);
/// The live `SteamConnection@DLNW3D` the transport-standup built (0 until phase 1 runs). The seam wraps
/// exactly this connection in the holder, so `land_socket_holder` needs `stand_up_transport` on too.
static STANDUP_CONNECTION: AtomicUsize = AtomicUsize::new(0);
/// Resolved absolute address of the `SocketManagerHolder@DLNR3D` ctor `0x1423f7180` (set at install).
static HOLDER_CTOR_FN: AtomicUsize = AtomicUsize::new(0);
/// Resolved absolute address of the game allocator `0x141eb9ed0` (set at install; reused by the seam).
static GAME_ALLOC_FN: AtomicUsize = AtomicUsize::new(0);
/// `SocketManagerHolder@DLNR3D` ctor (`fn(buf, conn) -> holder`; installs vtable `0x1431f9280`, sets the
/// refcount `+0x8 = 0` and stores the wrapped connection at `+0x10`). A 5-instruction leaf.
const HOLDER_CTOR_OFFSET: usize = 0x1_423f_7180 - 0x1_4000_0000;
/// The container heap pointer field (`[container+0x48]`) the game's establish handler allocates the
/// 0x18-byte holder from — use the same heap so the game's own deleter matches on teardown.
const CONTAINER_HEAP_OFF: usize = 0x48;
/// The peer SteamID64 offset on a `SteamConnection@DLNW3D` (`+0x128`, docs/FROMNET-LINK-FINDINGS.md §1b),
/// bound on the creator-built connection before it's wrapped. (NB: `+0x8` is NOT the iface on a
/// creator-built connection — it's a lock-bearing DLNW3D sub-object; do not write it. See the wire step.)
const CONN_PEER_OFF: usize = 0x128;

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
    // legb-finhandle: the fires-always finalize-result read (does leg B reach its tail? what handle?).
    let fh = exe_base + LEGB_FINHANDLE_OFFSET;
    if prologue_ok("legb-finhandle", fh, &LEGB_FINHANDLE_PROLOGUE) {
        install_offset_hook("legb-finhandle", fh, log_legb_finhandle);
    }
    // gate4-helper-ret (Hook A): gate4's read of helper 0x1423faf60's return.
    let ga = exe_base + GATE4_HELPER_RET_OFFSET;
    if prologue_ok("gate4-helper-ret", ga, &GATE4_HELPER_RET_PROLOGUE) {
        install_offset_hook("gate4-helper-ret", ga, log_gate4_helper_ret);
    }
    // gate4-vmethod (Hook B): the Arxan-encoded vmethod's verdict inside the helper — the money datum.
    let gb = exe_base + GATE4_VMETHOD_OFFSET;
    if prologue_ok("gate4-vmethod", gb, &GATE4_VMETHOD_PROLOGUE) {
        install_offset_hook("gate4-vmethod", gb, log_gate4_vmethod);
    }
    // vmethod-target (L3): capture the live vmethod target [rax+8] at the helper's call site.
    let vt = exe_base + VMETHOD_TARGET_OFFSET;
    if prologue_ok("vmethod-target", vt, &VMETHOD_TARGET_PROLOGUE) {
        install_offset_hook("vmethod-target", vt, log_vmethod_target);
    }
    // Establish-handler bail localizers (only meaningful while drive_establish_handler drives 0x1423f2820):
    // gate2-ret pins whether the handler bails at the session-established gate (before the builder), and
    // builder-entry fires iff it reaches the real builder. Together they localize the offline bail that the
    // cleanup-reset +0x41/+0x42 flags can't. See docs/SESSION-DRIVE.md > "► NEXT STEP".
    let g2 = exe_base + GATE2_RET_OFFSET;
    if prologue_ok("gate2-ret", g2, &GATE2_RET_PROLOGUE) {
        install_offset_hook("gate2-ret", g2, log_gate2_ret);
    }
    let be = exe_base + BUILDER_ENTRY_OFFSET;
    if prologue_ok("builder-entry", be, &BUILDER_ENTRY_PROLOGUE) {
        install_offset_hook("builder-entry", be, log_builder_entry);
    }
    // veto-field: read [container+0x7c0] at the real veto vmethod's entry (bit 2 = the create gate),
    // and — when `set_create_veto_bit` is armed — write bit 2 set to test the L3 lever.
    SET_CREATE_VETO_BIT.store(config.debug.probes.set_create_veto_bit, Ordering::Relaxed);
    DRIVE_SESSION_ESTABLISHED.store(config.debug.probes.drive_session_established, Ordering::Relaxed);
    SESSION_ESTABLISHED_FN.store(exe_base + SESSION_ESTABLISHED_OFFSET, Ordering::Relaxed);
    // Seam (land_socket_holder): resolve the holder ctor + game allocator so the veto-vmethod hook can
    // build a real SocketManagerHolder at [container+0x708]. The connection it wraps comes from the
    // separate `stand_up_transport` feature (STANDUP_CONNECTION), so both must be enabled together.
    LAND_SOCKET_HOLDER.store(config.debug.probes.land_socket_holder, Ordering::Relaxed);
    HOLDER_CTOR_FN.store(exe_base + HOLDER_CTOR_OFFSET, Ordering::Relaxed);
    GAME_ALLOC_FN.store(exe_base + GAME_ALLOC_OFFSET, Ordering::Relaxed);
    DRIVE_ESTABLISH_HANDLER.store(config.debug.probes.drive_establish_handler, Ordering::Relaxed);
    ESTABLISH_HANDLER_FN.store(exe_base + ESTABLISH_HANDLER_OFFSET, Ordering::Relaxed);
    let vv = exe_base + VETO_VMETHOD_OFFSET;
    if prologue_ok("veto-field", vv, &VETO_VMETHOD_PROLOGUE) {
        install_offset_hook("veto-field", vv, log_veto_field);
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

/// legb-finhandle tracer: fires at `0x1423f5cb5` on **every** leg-B finalize (the instruction after
/// `call 0x1423fab40`, so `eax` = the finalize handle, before the `je` to cleanup). Reports the handle
/// plus the registry-id counter and slot cap/count — read *before* the capacity check/store. Its value
/// vs. [`log_legb_finalize`] (the failure-only cleanup hook): if this fires with `handle!=0` and
/// `cap>0` but the cleanup hook does NOT and create still fails, leg B stored successfully and the
/// veto is post-leg-B (create-gate4 or later); `handle=0` confirms the zero registry-node id model.
/// Read-only; unwind-firewalled.
fn log_legb_finhandle(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: at this charted post-finalize site `eax` = the finalize handle, `rbx` = the
        // NetworkSession, `rdi` = the session object. In-bounds field reads on live objects; read-only.
        let r = unsafe { &*regs };
        let handle = r.rax as u32;
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
        let next_id = if session_obj != 0 {
            let sub = unsafe { ((session_obj + SESSION_OBJ_SUB_OFF) as *const usize).read_volatile() };
            (sub != 0).then(|| unsafe { ((sub + REGISTRY_NEXT_ID_OFF) as *const u32).read_volatile() })
        } else {
            None
        };
        log::info!(
            "session-probe: gate-trace legb-finhandle REACHED (leg B reached its tail) — \
             handle(eax)={handle} post-next-id={next_id:?} slot-array cap={cap} count={count} \
             (fires pre-store; handle!=0 && cap>0 => store should succeed => any failure is post-leg-B)",
        );
    }));
}

/// Hook A tracer: at gate4's `test al,al` after `call 0x1423faf60`, so `al` is the helper's return.
/// `al==0` ⇒ gate4 returns false ⇒ the confirmed rung-3 create veto. Read-only; unwind-firewalled.
fn log_gate4_helper_ret(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let al = unsafe { &*regs }.rax as u8;
        log::info!(
            "session-probe: gate-trace gate4-helper-ret — helper 0x1423faf60 returned al={al} \
             (0 => create-gate4 returns false => leg B skips finalize => create fails)",
        );
    }));
}

/// Hook B tracer: at the helper's `test al,al` after `call [container_vtable+8]`, so `al` is the
/// Arxan-encoded vmethod's verdict — the money datum. `al==0` proves that encoded vmethod is the sole
/// in-world veto (it's statically undecodable). Read-only; unwind-firewalled.
fn log_gate4_vmethod(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let al = unsafe { &*regs }.rax as u8;
        log::info!(
            "session-probe: gate-trace gate4-vmethod — Arxan-encoded vmethod [[session_obj+0x58]+8] \
             returned al={al} (0 => THIS is the create veto; the decisive predicate is inside the \
             encoded vmethod, so seed its input / capture its decoded target per L3)",
        );
    }));
}

/// L3 capture tracer: at the Arxan trampoline's `test rbx,rbx` (`0x14251c4a5`), `rbx` is the live
/// decoded target of the encoded vmethod. The trampoline is generic, so fire only when the return
/// address ([rsp+0x28], the trampoline pushed `rbx` then `sub rsp,0x20` over the call's return) is our
/// helper's vmethod call site, and latch once. Logs the real function address to disassemble offline —
/// the function whose predicate is the actual rung-3 create veto. Read-only; unwind-firewalled.
fn log_vmethod_target(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if VMETHOD_TARGET_CAPTURED.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: at `0x1423fafc4`, `rax` = the live container vtable (just loaded by `mov rax,[rcx]`),
        // so `[rax+8]` is the vmethod pointer the next `call [rax+8]` will invoke. Read-only.
        let r = unsafe { &*regs };
        let vtable = r.rax as usize;
        if vtable == 0 {
            return;
        }
        let target = unsafe { ((vtable + 8) as *const usize).read_volatile() };
        VMETHOD_TARGET_CAPTURED.store(true, Ordering::Relaxed);
        log::info!(
            "session-probe: vmethod-target CAPTURED — live container vtable={vtable:#x}, create-veto \
             vmethod [vtable+8]={target:#x} (static chart assumed vtable 0x1431f8360; disassemble the \
             target — its predicate is the veto)",
        );
    }));
}

/// gate2-ret localizer: at `0x1423f289c` (`test al,al` after the establish handler's `call
/// [vtable+0x68]` = the session-established handler `0x1423f4870`), `al` = that handler's return. The
/// handler bails to cleanup (and returns 0) if `al==0`, so this pins whether the bail is gate2 (before
/// the builder). Latch once; read-only; unwind-firewalled.
fn log_gate2_ret(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if GATE2_RET_CAPTURED.swap(true, Ordering::Relaxed) {
            return;
        }
        let al = unsafe { &*regs }.rax as u8;
        log::info!(
            "session-probe: gate2-ret — establish handler's [vtable+0x68] (session-established 0x1423f4870) \
             returned al={al} (0 => handler bails HERE, before the builder; nonzero => proceeds to the \
             [container+0xa0]&0x40 build/copy branch)",
        );
    }));
}

/// builder-entry localizer: fires IFF the establish handler reaches the connection builder thunk
/// `0x1423f46b0` (`container->vtable[0x80]` on the live `ManagerImplSteam` vtable). Proves the handler
/// got past gate1/gate2 and the `[container+0xa0]&0x40` bit into the build path. `rcx`=container,
/// `rdx`=`&local_struct` (built from our descriptor), `r8b`=`[desc+0x3d]` (sub-builder selector:
/// nonzero→`0x1426372e0`, zero→`0x142637440`). `[rdx]`=`[container+0x48]` (heap ptr) is the first-qword
/// input the builder null-checks. Latch once; read-only; unwind-firewalled.
fn log_builder_entry(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if BUILDER_ENTRY_CAPTURED.swap(true, Ordering::Relaxed) {
            return;
        }
        let r = unsafe { &*regs };
        let (container, local, sel) = (r.rcx as usize, r.rdx as usize, r.r8 as u8);
        // SAFETY: `local` = `&[rbp-0x49]` the handler just filled; its first qword is `[container+0x48]`.
        let first = if local != 0 { unsafe { (local as *const usize).read_volatile() } } else { 0 };
        log::info!(
            "session-probe: builder-entry REACHED — vtable[0x80] builder thunk 0x1423f46b0 \
             (container={container:#x}, &local={local:#x}, [local]={first:#x}, selector [desc+0x3d]={sel} => \
             {}) — the handler passed gate1+gate2+the 0xa0 bit; any create failure now is builder-internal",
            if sel != 0 { "0x1426372e0" } else { "0x142637440" },
        );
    }));
}

/// veto-field tracer: at the real veto vmethod entry (`0x1423f4330`), `rcx` = the container. Reads
/// `[container+0x7c0]` and reports bit 2 — the vmethod returns false when it's clear (the offline
/// create veto). Confirms the field + points at the lever (set bit 2 before create). Read-only.
fn log_veto_field(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if VETO_FIELD_READ.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: `rcx` at this entry = the container object; `+0x7c0` is an in-bounds dword field
        // (the object is ~0x800+ bytes). Read-only.
        let r = unsafe { &*regs };
        let container = r.rcx as usize;
        if container == 0 {
            return;
        }
        let field_ptr = (container + VETO_FIELD_OFF) as *mut u32;
        let field = unsafe { field_ptr.read_volatile() };
        // Real-init lever: drive the container's session-established handler 0x1423f4870(this=container)
        // so it populates +0x7c0 bit2, +0x7f8 identity, and the +0x708 connection FOR REAL (past its
        // live-Steam-context gate), instead of fabricating stubs. One-shot (reuses VETO_FIELD_READ's
        // latch below is separate — guard here so we only drive once).
        if DRIVE_SESSION_ESTABLISHED.load(Ordering::Relaxed) && !VETO_FIELD_READ.load(Ordering::Relaxed) {
            let fn_addr = SESSION_ESTABLISHED_FN.load(Ordering::Relaxed);
            let before_bit2 = (field >> 2) & 1;
            let before_708 = unsafe { ((container + 0x708) as *const usize).read_volatile() };
            if fn_addr != 0 {
                // SAFETY: 0x1423f4870 is ManagerImplSteam's session-established handler; win64 ABI, rcx =
                // this = the live container. It self-inits its Steam contexts and returns; a fault inside
                // is caught by the crashdump SEH handler (this catch_unwind can't catch a SIGSEGV).
                let handler: extern "win64" fn(usize) = unsafe { std::mem::transmute(fn_addr) };
                log::info!(
                    "session-probe: veto-field — DRIVING session-established handler 0x1423f4870(container={container:#x}) \
                     [before: bit2={before_bit2} +0x708={before_708:#x}]",
                );
                handler(container);
                let after = unsafe { field_ptr.read_volatile() };
                let after_708 = unsafe { ((container + 0x708) as *const usize).read_volatile() };
                let after_7f8 = unsafe { ((container + 0x7f8) as *const usize).read_volatile() };
                log::info!(
                    "session-probe: veto-field — handler returned; container now [+0x7c0]={after:#x} \
                     bit2={} +0x708={after_708:#x} +0x7f8={after_7f8:#x} (nonzero => REAL init worked)",
                    (after >> 2) & 1,
                );
            }
        }
        // NATIVE-BUILDER experiment: drive the game's own connection-establish handler 0x1423f2820 to
        // build + wrap + store a connection at [container+0x708] the game's way (via the Arxan native
        // builder container->vtable[0x80]). Answers whether the DLNW3D builder runs offline at all.
        if DRIVE_ESTABLISH_HANDLER.load(Ordering::Relaxed) {
            drive_establish_handler(container);
        }
        // SEAM: build a real SocketManagerHolder@DLNR3D around our standup connection and land it at
        // [container+0x708], so the driven create's ConnectionRefInfo loop reads a valid refcountable
        // object (refcount at +8) instead of null. This is the charted replacement for the hollow
        // fabrication below. We're at the veto vmethod entry (rcx=container) — the same container create
        // uses ([SessionSteam+0x58]) — and this fires before the +0x708 read in the gate4 helper.
        if LAND_SOCKET_HOLDER.load(Ordering::Relaxed) {
            land_socket_holder(container);
        }
        // L3 lever: set bit 2 before the vmethod reads it (a few instructions ahead at 0x1423f434b),
        // so its `test bit2; je return-false` first predicate passes. Only writes when armed + clear.
        if SET_CREATE_VETO_BIT.load(Ordering::Relaxed) {
            if (field >> 2) & 1 == 0 {
                unsafe { field_ptr.write_volatile(field | 0b100) };
                log::info!(
                    "session-probe: veto-field — LEVER set bit2 on container={container:#x} \
                     [+0x7c0] {field:#x} -> {:#x} (testing whether create now passes)",
                    field | 0b100,
                );
            }
            // Also fabricate the null sibling sub-object at [container+0x708]: the helper reads it as
            // ctor arg (rdx), which is stored at new_obj+0x18 and refcounted (lock xadd [rdx+8]). A
            // leaked zeroed buffer with refcount=1 at +8 lets the ctor complete; further use of it will
            // fault next (the backtrace then localizes the next missing field). Whack-a-mole toward a
            // working create, one crash at a time.
            let sub_ptr = (container + 0x708) as *mut usize;
            if unsafe { sub_ptr.read_volatile() } == 0 {
                let buf: &'static mut [usize] = vec![0usize; 0x100].leak(); // 0x800 bytes, generous
                let obj = buf.as_mut_ptr() as usize;
                unsafe { ((obj + 8) as *mut u32).write_volatile(1) }; // refcount = 1
                unsafe { sub_ptr.write_volatile(obj) };
                log::info!(
                    "session-probe: veto-field — LEVER fabricated [container+0x708]={obj:#x} \
                     (leaked 0x800B, refcount=1) to get past ctor 0x1423f3230",
                );
            }
        }
        if !VETO_FIELD_READ.swap(true, Ordering::Relaxed) {
            log::info!(
                "session-probe: veto-field — container={container:#x} [+0x7c0]={field:#x} bit2={} \
                 (vmethod returns false when bit2==0 => the create veto; lever: set bit2 before create)",
                (field >> 2) & 1,
            );
        }
    }));
}

/// Drive the game's own connection-establish handler `0x1423f2820(container, descriptor)` — the native
/// path that calls the Arxan builder `container->vtable[0x80]` to construct a fully-wired `SteamConnection`,
/// wraps it in a `SocketManagerHolder`, and stores it at `[container+0x708]` + addrefs. This is the
/// experiment: does the DLNW3D builder run offline? No-op if `+0x708` is already populated. Sets the
/// handler's entry preconditions (`[container+0x40]=1`, `[container+0x41]=0`) and hands it a leaked, mostly
/// zeroed descriptor with a guessed connection-count field (`desc+0 = 1` — the field that, if the stack
/// struct flows to the connection-creator as its params, becomes `params+0x18` = count).
fn drive_establish_handler(container: usize) {
    // SAFETY: `container` = the veto vmethod's live `ManagerImpl@DLNR3D`; `+0x708` is an in-bounds qword.
    let slot = (container + 0x708) as *mut usize;
    if unsafe { slot.read_volatile() } != 0 {
        return; // already populated — don't double-drive
    }
    let fn_addr = ESTABLISH_HANDLER_FN.load(Ordering::Relaxed);
    if fn_addr == 0 {
        return;
    }
    // Preconditions: the handler bails at entry unless [container+0x40]==1 and [container+0x41]==0.
    // SAFETY: both are in-bounds byte flags on the container.
    let p40 = (container + 0x40) as *mut u8;
    let p41 = (container + 0x41) as *mut u8;
    let (before40, before41) = unsafe { (p40.read_volatile(), p41.read_volatile()) };
    unsafe {
        p40.write_volatile(1);
        p41.write_volatile(0);
    }
    // Leaked, mostly-zeroed descriptor (the handler reads dwords [desc..desc+0x34] + bytes at +0x3c/+0x3d
    // and copies ~0x120 bytes to [container+0xb0]). 0x140 bytes is generous. Guess the connection-count
    // field at desc+0 = 1 (see the doc comment); everything else defaults inside the builder.
    let desc: &'static mut [u8] = vec![0u8; 0x140].leak();
    let desc_ptr = desc.as_mut_ptr() as usize;
    unsafe { (desc_ptr as *mut u32).write_volatile(1) };
    // Instrument the DLNW3D singleton (0x144852dc0 ptr + 0x144852dc8 readiness byte) that the handler's
    // internal readiness gate (0x1423f5190 -> 0x1423f4fa0) reads, across the drive. The gate returns 0
    // offline because this singleton is a self-referential empty sentinel (0x144852dd0 = &cell+0x10),
    // not a stood-up service — so the handler bails at +0x42=0 before the builder. Log the state before
    // the standalone gate call, after it (does the gate mutate/create the singleton?), and after the
    // handler, to chart exactly what the offline gate does. SAFETY: both are fixed global cells in the
    // mapped image (base 0x140000000, no ASLR).
    let sing = || unsafe { (0x1_4485_2dc0usize as *const usize).read_volatile() };
    let ready = || unsafe { (0x1_4485_2dc8usize as *const u8).read_volatile() };
    let (s0, r0) = (sing(), ready());
    let gate_fn = fn_addr.wrapping_add(0x2970);
    let gate: extern "win64" fn(usize) -> u8 = unsafe { std::mem::transmute(gate_fn) };
    let gate_ret = gate(container);
    let (s1, r1) = (sing(), ready());
    log::info!(
        "session-probe: drive-establish — readiness gate 0x1423f5190 = {gate_ret}; singleton \
         [0x144852dc0] {s0:#x}->{s1:#x} readiness [0x144852dc8] {r0}->{r1} (self-ref 0x144852dd0 = empty \
         sentinel; a stood-up service would be a distinct heap ptr)",
    );
    log::info!(
        "session-probe: drive-establish — calling 0x1423f2820(container={container:#x}, desc={desc_ptr:#x}) \
         [pre: +0x40={before40} +0x41={before41} +0x708=0; forced +0x40=1 +0x41=0, desc+0=1]",
    );
    // SAFETY: win64 ABI; rcx=container, rdx=descriptor. Builds the connection via the container's Arxan
    // vtable[0x80], wraps + stores at +0x708. A hard fault surfaces via the crashdump SEH handler; the
    // caller (log_veto_field) is unwind-firewalled.
    let handler: extern "win64" fn(usize, usize) -> u8 = unsafe { std::mem::transmute(fn_addr) };
    let ret = handler(container, desc_ptr);
    let after_708 = unsafe { slot.read_volatile() };
    // NB: +0x41/+0x42 are UNRELIABLE bail localizers — the handler's cleanup 0x1423f2f30 resets BOTH to 0
    // on every bail (0x1423f2fa5/0x1423f2fd7), so +0x42=0 here does NOT mean "bailed at the readiness
    // gate" (the old reading). Use the gate2-ret + builder-entry HOOK localizers instead: gate2-ret pins
    // the session-established gate, builder-entry proves the builder was reached. Rig-charted 2026-07-04:
    // with the double session-established drive removed, gate2 passes and the handler REACHES the builder
    // (0x142637440), which fails offline because the SteamServiceImpl standup (0x142638b40) returns null.
    // We still log +0x8ac (set once at entry, not reset) + the raw +0x41/+0x42 for reference.
    let (a41, a42, a8ac) = unsafe {
        (
            ((container + 0x41) as *const u8).read_volatile(),
            ((container + 0x42) as *const u8).read_volatile(),
            ((container + 0x8ac) as *const u8).read_volatile(),
        )
    };
    let (s2, r2) = (sing(), ready());
    log::info!(
        "session-probe: drive-establish — 0x1423f2820 returned {ret}; [container+0x708]={after_708:#x} \
         [+0x41={a41} +0x42={a42} +0x8ac={a8ac} (cleanup-reset; see gate2-ret/builder-entry)] singleton \
         {s2:#x} readiness {r2} (non-null +0x708 => the native build succeeded and wrapped a connection)",
    );
    if after_708 != 0 {
        // Peek the wrapped connection (holder+0x10) + its vtable, to confirm it's a real SteamConnection.
        let conn = unsafe { ((after_708 + 0x10) as *const usize).read_volatile() };
        let vt = if conn != 0 { unsafe { (conn as *const usize).read_volatile() } } else { 0 };
        log::info!(
            "session-probe: drive-establish — holder[+0x10]=connection={conn:#x} vtable={vt:#x} \
             (SteamConnection@DLNW3D static vtable = 0x143278358/0x143278370)",
        );
    }
}

/// Build a real `SocketManagerHolder@DLNR3D` around the transport-standup connection and write it to
/// `[container+0x708]` if that slot is still null — the seam that lets the driven create's
/// `ConnectionRefInfo` loop wrap a valid refcountable object instead of null-derefing (see the
/// "SEAM CHARTED" section of docs/SESSION-DRIVE.md). Called from the veto-vmethod hook with the live
/// container. No-op unless armed and the standup connection exists; never clobbers a `+0x708` a real
/// session set up.
fn land_socket_holder(container: usize) {
    // SAFETY: `container` is the veto vmethod's `rcx` — the live `ManagerImpl@DLNR3D`; `+0x708` is an
    // in-bounds qword field (the object is ~0x820+ bytes). Read-then-maybe-write of that one slot.
    let slot = (container + 0x708) as *mut usize;
    if unsafe { slot.read_volatile() } != 0 {
        return; // a real session already populated it — never clobber
    }
    let conn = STANDUP_CONNECTION.load(Ordering::Relaxed);
    if conn == 0 {
        log::warn!(
            "session-probe: land-socket-holder — no standup connection yet (need stand_up_transport built \
             first); leaving [container+0x708] null",
        );
        return;
    }
    let ctor_fn = HOLDER_CTOR_FN.load(Ordering::Relaxed);
    let alloc_fn = GAME_ALLOC_FN.load(Ordering::Relaxed);
    if ctor_fn == 0 || alloc_fn == 0 {
        return;
    }
    // Allocate the 0x18-byte holder off the container's own heap ([container+0x48]) — the heap the game's
    // establish handler (0x1423f2820) uses for exactly this wrapper — so the game's own deleter matches on
    // teardown rather than freeing a foreign pointer.
    // SAFETY: `container+0x48` is the in-bounds heap-pointer field; read-only.
    let heap = unsafe { ((container + CONTAINER_HEAP_OFF) as *const usize).read_volatile() };
    if heap == 0 {
        log::warn!("session-probe: land-socket-holder — container heap [+0x48] null; skipping");
        return;
    }
    // SAFETY: win64 ABI game fns resolved from the live exe base at install. `alloc(size, align, heap)` is
    // the game allocator; `ctor(buf, conn)` is the 5-instruction holder ctor (installs vtable, +8=0,
    // +0x10=conn). A hard fault surfaces via the crashdump SEH handler; the caller is unwind-firewalled.
    let alloc: extern "win64" fn(usize, usize, usize) -> usize = unsafe { std::mem::transmute(alloc_fn) };
    let buf = alloc(0x18, 8, heap);
    if buf == 0 {
        log::warn!("session-probe: land-socket-holder — 0x18B alloc off container heap returned null; skipping");
        return;
    }
    // Run the socket-manager's FULL init (`0x14263a9d0`) so its service stands up the game's way — the
    // sub-init `0x14263ce40` (charted 2026-07-04 pm) null-checks descriptor[0] (owner) + descriptor[8],
    // copies descriptor[0..0x60] → socketmgr[0x40..0xa0], then calls the service standup `0x142638b40`
    // with owner=descriptor[0]. KEY FINDING: the service init check `0x14263f450` ALWAYS returns true, so
    // the standup only returns null if owner==0 — the prior "standup null offline" was likely the removed
    // svc-standup probe perturbing flags, NOT an online gate. Descriptor from the native trace:
    // [0]=owner=[container+0x48], [8]=0x1423f2d70 (non-null, satisfies the 2nd check), [0x10]=container,
    // [0x1c]=ring size. `conn` is the wrapper; [conn+8]=socketmgr. Do NOT pre-set [socketmgr+0x40] (the
    // sub-init bails if it's already non-null). SAFETY: descriptor is a leaked 0x60-byte buffer; init is a
    // win64 game fn resolved from the live exe base; a hard fault surfaces via the crashdump SEH handler.
    let socketmgr = unsafe { ((conn + 8) as *const usize).read_volatile() };
    if socketmgr != 0 {
        let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
            Ok(h) => h.0 as usize,
            Err(_) => 0,
        };
        if exe_base != 0 {
            let desc: &'static mut [u8] = vec![0u8; 0x60].leak();
            let dp = desc.as_mut_ptr() as usize;
            // Seed the descriptor from the socket-manager's POST-CTOR state (socketmgr[0x40..0xa0]) so the
            // sub-init's copy (descriptor[0..0x60] → socketmgr[0x40..0xa0]) PRESERVES the config defaults the
            // base ctor 0x14263cb70 set there (ring sizes/timeouts at +0x58/0x5c/0x60/0x74..0x9c) — a
            // mostly-zero descriptor clobbered them and the socket-manager's worker thread then spun up
            // misconfigured and jumped to garbage (fault #4, 2026-07-04 pm). Then override only the three
            // init pointers the ctor left null: [0]=owner, [8]=local8 (non-null 2nd check), [0x10]=container.
            // SAFETY: `dp` is a freshly-leaked 0x60-byte buffer; source is the in-bounds socketmgr config region.
            unsafe {
                std::ptr::copy_nonoverlapping((socketmgr + 0x40) as *const u8, dp as *mut u8, 0x60);
                (dp as *mut usize).write_volatile(heap); // descriptor[0] = owner (the container heap)
                ((dp + 8) as *mut usize).write_volatile(exe_base + SOCKMGR_LOCAL8_OFFSET); // [8] non-null
                ((dp + 0x10) as *mut usize).write_volatile(container); // [0x10] = container
            }
            let init: extern "win64" fn(usize, usize) -> u8 =
                unsafe { std::mem::transmute(exe_base + SOCKMGR_INIT_OFFSET) };
            log::info!(
                "session-probe: land-socket-holder — driving socketmgr init 0x14263a9d0({socketmgr:#x}, \
                 desc{{owner={heap:#x}, [8]={:#x}, container={container:#x}}}) — standup owner=[container+0x48]",
                exe_base + SOCKMGR_LOCAL8_OFFSET,
            );
            let ok = init(socketmgr, dp);
            let svc = unsafe { ((socketmgr + 0x38) as *const usize).read_volatile() };
            let f40 = unsafe { ((socketmgr + 0x40) as *const usize).read_volatile() };
            log::info!(
                "session-probe: land-socket-holder — socketmgr init returned {ok} — [+0x38]service={svc:#x} \
                 [+0x40]owner={f40:#x} (nonzero service => the standup SUCCEEDED offline!)",
            );
        }
    }
    let ctor: extern "win64" fn(usize, usize) -> usize = unsafe { std::mem::transmute(ctor_fn) };
    let holder = ctor(buf, conn);
    // refcount (+8) = 1: the game's establish handler addrefs 0->1 right after building the holder, so
    // mirror it — the driven create's ConnectionRefInfo ctor then addrefs onto a live 1, not a stale 0.
    unsafe { ((holder + 8) as *mut u32).write_volatile(1) };
    unsafe { slot.write_volatile(holder) };
    log::info!(
        "session-probe: land-socket-holder — built SocketManagerHolder @ {holder:#x} \
         (wraps SteamConnection {conn:#x}, refcount=1) and wrote [container+0x708] on container={container:#x} \
         (create's ConnectionRefInfo loop now has a real refcountable object; drive create to test Host)",
    );
    dump_conn_graph(conn);
}

/// Read-only dump of the `SteamConnection@DLNW3D` sub-object graph the host-setup path walks, so a rig
/// run pins exactly which sub-object/vtable slot is null before we wire it. Host-setup fault #1
/// (2026-07-04 pm): `0x1423f6bf2` reads `[container+0x708]`→holder→`conn`, then `0x14203f1f0` does
/// `rcx=[conn+8]; rax=[rcx]; jmp [rax+0x18]` — a vtable dispatch on the `[conn+8]` sub-object. A solo
/// stood-up connection faults there (execute-null: `[[conn+8]]+0x18 == 0`). This logs the whole chain
/// (`[conn+0]` main vtable, `[conn+8]` sub-object ptr, its vtable, and slots +0x10/+0x18/+0x20 of that
/// vtable) so we can identify the sub-object's class + which method the host expects. All reads are
/// guarded (a null link stops the walk); never writes.
fn dump_conn_graph(conn: usize) {
    if conn == 0 {
        return;
    }
    // SAFETY: `conn` is the live standup `SteamConnection` just wrapped into the holder; every read below
    // is a bounded qword deref of an in-bounds field, and each link is null-checked before it's followed.
    unsafe {
        let rd = |p: usize| -> usize {
            if p == 0 { 0 } else { (p as *const usize).read_volatile() }
        };
        let conn_vt = rd(conn);
        let sub = rd(conn + 0x8); // [conn+8] — the sub-object the host-setup dispatches on
        let sub_vt = rd(sub); // [[conn+8]] — its vtable
        let slot10 = if sub_vt != 0 { rd(sub_vt + 0x10) } else { 0 };
        let slot18 = if sub_vt != 0 { rd(sub_vt + 0x18) } else { 0 }; // the null the host-setup jumps to
        let slot20 = if sub_vt != 0 { rd(sub_vt + 0x20) } else { 0 };
        let sub_is_embedded = sub == conn + 0x20;
        let vt10 = rd(conn + 0x10); // second vtable the ctor installs
        let f120 = rd(conn + 0x120); // iface-holder sub-object (real established conn has this)
        let peer = rd(conn + 0x128);
        log::info!(
            "session-probe: conn-graph {conn:#x} — [+0]vt={conn_vt:#x} [+0x10]vt={vt10:#x} \
             [+8]sub={sub:#x}{} sub.vtable={sub_vt:#x} sub.vt[+0x10]={slot10:#x} \
             sub.vt[+0x18]={slot18:#x} (host-setup jmps HERE; 0 => the fault) sub.vt[+0x20]={slot20:#x} \
             [+0x120]ifaceholder={f120:#x} [+0x128]peer={peer:#x}",
            if sub_is_embedded { " (=conn+0x20 embedded)" } else { " (separate obj)" },
        );
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

/// Offset of the Host-transition fn `0x140cb2ae0` — the **sole** writer of `lobby_state=Host(3)` (it also
/// sets `protocol_state=Ingame(6)` and runs the host setup). Normally reached by the session-update task
/// `0x140cafd10` after the create-connection setup completes; that setup is what crashes on our incomplete
/// connection, so `force_host_transition` calls this directly to jump the update task past it.
const HOST_TRANSITION_OFFSET: usize = 0x140c_b2ae0 - 0x1_4000_0000;
/// The Host-transition fn's signature: `fn(this /*rcx = CSSessionManager*/)`.
type HostTransitionFn = unsafe extern "system" fn(*mut CSSessionManager);

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
    /// When true (`[debug.probes] force_host_transition`), after the driven create reaches
    /// `TryToCreateSession`, call the game's Host-transition fn `0x140cb2ae0` directly to jump the
    /// session-update task past its crashing connection-activation path straight to `Host`.
    force_host: bool,
}

impl SessionCreateDriver {
    fn new(force_ready: bool, fire_solo: bool, force_host: bool) -> Self {
        Self { fired: false, linked_since: None, force_ready, fire_solo, force_host }
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

        // Force the Host transition: the session-update task (0x140cafd10) reaches Host only after the
        // create-connection setup completes, and that setup crashes on our incomplete connection. Calling
        // the game's own Host writer 0x140cb2ae0 directly sets lobby_state=Host(3) + protocol=Ingame(6) +
        // runs the host setup, so the update task then takes the host-maintenance branch instead of the
        // crashing connection-activation branch. Only meaningful once create built the session object.
        if self.force_host && after == Some(LobbyState::TryToCreateSession) {
            let host_fn = exe_base + HOST_TRANSITION_OFFSET;
            // SAFETY: `host_fn` is the charted Host-transition entry resolved from the live exe base; called
            // with this=[G] (the live singleton) on the main thread, right after create set TryToCreateSession.
            let host: HostTransitionFn = unsafe { std::mem::transmute::<usize, HostTransitionFn>(host_fn) };
            log::info!(
                "session-probe: drive-create — forcing Host transition via {host_fn:#x}(this={base:#x})",
            );
            unsafe { host(base as *mut CSSessionManager) };
            let after_host =
                crate::sdk::with_instance::<CSSessionManager, _>(|s| crate::session::read(s).lobby_state);
            log::info!(
                "session-probe: drive-create — after Host transition, lobby_state now {after_host:?} \
                 (Host => rung-3 create reached Host!)",
            );
        }
    }
}

// --- Rung-3 TRANSPORT-STANDUP driver (ERSC path C, experimental) --------------------------------
//
// The transport leg of the ERSC-faithful connection (docs/COOP-CONNECTION.md > "THE PLAN" +
// docs/FROMNET-LINK-FINDINGS.md). The game's DLNW3D Steam-P2P transport is DORMANT offline
// (scan-vtable.py: 0 SteamServiceImpl/Manager/Connection), so `[container+0x708]` — the SteamConnection
// the driven create needs — is never built. Path C stands the transport up ourselves; because ER's
// transport is the LEGACY `ISteamNetworking006` P2P API (addressed by CSteamID alone, Steam relay does
// NAT), the whole connection needs only the rung-4 peer SteamID64 — no server-brokered `join_data`.
//
// This is the FIRST, lowest-risk increment: resolve `ISteamNetworking006` and construct a
// `SteamServiceImpl@DLNW3D` via its charted base ctor, logging each step. A `scan-vtable.py` run then
// confirms whether a SteamServiceImpl exists offline — i.e. whether we can construct DLNW3D objects at
// all without the game's online flow. The manager + connection + peer-bind + Accept come in later
// increments (synthesized params; the connect itself needs a two-machine peer).
//
// Addresses (from the exe preferred base 0x140000000; docs/SESSION-DRIVE.md > "TRANSPORT CHARTED"):
// iface resolver 0x142640b90 stores the interface at holder 0x143c602b0; allocator 0x141eb9ed0(size,
// align); SteamServiceImpl base ctor 0x14263b6b0(this)->this installs vtable 0x143277270 (sub-ctor
// 0x14263f1e0). Re-derive after a game update via the RTTI/vtable walk in SESSION-DRIVE.md.

/// Offset (from exe base) of the ISteamNetworking006 resolver `0x142640b90`.
const ISTEAM_RESOLVER_OFFSET: usize = 0x2640b90;
/// Offset of the ISteamNetworking006 interface-context holder `0x143c602b0` (resolver stores the
/// interface pointer at `[holder]`).
const ISTEAM_HOLDER_OFFSET: usize = 0x3c602b0;
/// Offset of the `SteamServiceImpl@DLNW3D` base ctor `0x14263b6b0` (`fn(this) -> this`; installs
/// vtable `0x143277270`).
const SVC_BASE_CTOR_OFFSET: usize = 0x263b6b0;
/// Bytes the factory allocates for a `SteamServiceImpl` before base-ctoring it.
const SVC_SIZE: usize = 0x18;

/// Offset of the DLNR3D/container global heap pointer `0x144842d38` (lazily created — the heap the
/// container itself is allocated from, so the natural heap for the DLNW3D service/manager).
const HEAP_GLOBAL_OFFSET: usize = 0x4842d38;
/// Offset of the heap creator `0x141ec61d0` (`fn() -> heap`; only called if the global is still null).
const HEAP_CREATE_OFFSET: usize = 0x1ec61d0;
/// Offset of the game allocator `0x141eb9ed0` (`fn(size, align, heap) -> ptr`; tail-calls
/// `[[r8=heap]+0x50]`).
const GAME_ALLOC_OFFSET: usize = 0x1eb9ed0;
/// Offset of the `SteamConnectionManager@DLNW3D` ctor `0x14263f700` (`fn(this, heap) -> this`; installs
/// vtable `0x143278020`, allocates sub-buffers off `heap`).
const MANAGER_CTOR_OFFSET: usize = 0x263f700;
/// Bytes the connect thunk allocates for a `SteamConnectionManager` before ctoring it.
const MANAGER_SIZE: usize = 0x1b8;
/// Offset of the connection-creator `0x142640560` (`fn(manager, params) -> bool`; allocates the manager's
/// ring buffers + a `SteamConnection` array at `[manager+0x78]` (`params.count` slots of 0x140 bytes each,
/// ctored via `0x142643b50`) and registers the P2P callbacks (`0x142643fe0`). This is the **full** wire-up
/// a bare standalone ctor skips — without the manager ring buffers, the session's activate faults
/// (INVALID_HANDLE). Called by the connect thunk `0x14263b720`.
const CONN_CREATOR_OFFSET: usize = 0x2640560;
/// Offset on the manager of the pointer to its `SteamConnection` array (`slot i = [manager+0x78] + i*0x140`).
const CONN_ARRAY_PTR_OFF: usize = 0x78;
/// Params bytes copied to `[manager+0x40..0x70]`: only `[params]!=0` and `[params+0x18]=count!=0` are
/// guarded; `[params+0x1c]=ring size` (0 → default `0x4b0`). We pass `{ [0]=1, count=1 }`, rest 0.
const CONN_PARAMS_WORDS: usize = 0x30 / 4;
const CONN_PARAMS_COUNT_WORD: usize = 0x18 / 4;

// --- Socket-manager wrapper (the object [container+0x708]'s holder actually holds) ---------------
//
// CHART (2026-07-04 pm): the SocketManagerHolder@DLNR3D at [container+0x708] holds — at holder+0x10 —
// NOT a raw SteamConnection but the game builder's return: a 0x10-byte WRAPPER
// `{ vtable=0x143276a00, [+8]=socketmgr }` around an `MTInternalThreadSteamSocketManager@DLNW3D`
// (0x150 bytes, vtable 0x143276cb8). Host-setup (`0x14203f1f0`) does `wrapper->[+8]socketmgr->vtable[3]`
// (slot +0x18) — so landing a SteamConnection there made it read a connection data field as a vtable
// (garbage 0x100000000 → fault). We build the socket-manager via its CTOR ONLY (`0x142638140`; the ctor
// chain 0x14263a0b0→0x14263cb70 needs no service/heap — only the *init* 0x14263a9d0 hits the null
// SteamServiceImpl standup, which we skip), then the wrapper via `0x14203f100` + overwrite its vtable
// with 0x143276a00 exactly as the builder does. Re-derive: builder body `0x142637440` (alloc 0x150 →
// ctor 0x142638140 → init [vt+8] → on success alloc 0x10 → 0x14203f100(wrap, sm) → [wrap]=0x143276a00).
/// Socket-manager ctor `0x142638140` (`fn(this)`; installs vtable `0x143276cb8`, no service/heap needed).
const SOCKMGR_CTOR_OFFSET: usize = 0x2638140;
/// Bytes the builder allocates for the socket-manager.
const SOCKMGR_SIZE: usize = 0x150;
/// Wrapper init `0x14203f100` (`fn(this, socketmgr)`; sets `[this+8]=socketmgr`, `[this]=0x1430ea580`).
const WRAPPER_INIT_OFFSET: usize = 0x203f100;
/// Bytes the builder allocates for the 0x10-byte wrapper.
const WRAPPER_SIZE: usize = 0x10;
/// The wrapper's final vtable `0x143276a00` (the builder overwrites the init's 0x1430ea580 with this).
const WRAPPER_VTABLE_OFFSET: usize = 0x3276a00;
/// Socket-manager init vmethod `0x14263a9d0` (`fn(this, descriptor) -> bool`; sub-init 0x14263ce40 copies
/// descriptor[0..0x60] → this[0x40..0xa0] then stands up the service via 0x142638b40(owner=descriptor[0])).
const SOCKMGR_INIT_OFFSET: usize = 0x263a9d0;
/// Native descriptor `local[8]` value `0x1423f2d70` — a non-null the sub-init's 2nd null-check requires.
const SOCKMGR_LOCAL8_OFFSET: usize = 0x23f2d70;

/// One-shot: stand up a DLNW3D `SteamServiceImpl` offline (path C, first increment). Fires once in-world
/// (active main player present, same world gate as the create driver). Gated on
/// `[debug.probes] stand_up_transport`.
pub struct TransportStandupDriver {
    /// Phase 1 done: the transport (service/manager/connection) has been constructed.
    built: bool,
    /// The resolved `ISteamNetworking006` interface pointer (0 until phase 1 resolves it).
    iface: usize,
    /// Phase 2: have we called `AcceptP2PSessionWithUser(peer)` yet (once per link).
    accepted: bool,
    /// Throttle the outbound game-P2P ping (~2s at 60fps) so the log stays legible.
    ping_throttle: FrameThrottle,
    /// Outbound ping sequence number (so a received ping shows which send it answers).
    ping_seq: u32,
    /// Two-machine test peer override (both machines' SteamID64s; `0` = unset). Used when no rung-2
    /// link is present — each machine picks whichever isn't its own. See `p2p_test_peer_a` in config.
    peer_override: [u64; 2],
}

impl TransportStandupDriver {
    fn new(peer_a: u64, peer_b: u64) -> Self {
        Self {
            built: false,
            iface: 0,
            accepted: false,
            ping_throttle: FrameThrottle::every(120),
            ping_seq: 0,
            peer_override: [peer_a, peer_b],
        }
    }

    /// The peer SteamID64 to drive game-P2P at: the rung-2-linked partner if present, else the config
    /// override that isn't our own SteamID (the autonomous two-machine path — the link needs a menu action).
    fn target_peer(&self) -> Option<u64> {
        if let Some(p) = crate::coop::linked_peer() {
            return Some(p);
        }
        let self_id = crate::steam::self_steam_id();
        self.peer_override
            .into_iter()
            .find(|&id| id != 0 && Some(id) != self_id)
    }
}

/// ISteamNetworking006 flat-vtable slots (the legacy P2P API): `SendP2PPacket` (+0),
/// `IsP2PPacketAvailable` (+8), `ReadP2PPacket` (+0x10), `AcceptP2PSessionWithUser` (+0x18).
const ISTEAM_SEND_SLOT: usize = 0x0;
const ISTEAM_ISAVAIL_SLOT: usize = 0x8;
const ISTEAM_READ_SLOT: usize = 0x10;
const ISTEAM_ACCEPT_SLOT: usize = 0x18;
/// `k_EP2PSendReliable`.
const P2P_SEND_RELIABLE: u32 = 2;
/// P2P channel for our game-transport probe pings (0; the game's own transport is dormant offline).
const P2P_PROBE_CHANNEL: i32 = 0;

impl Feature for TransportStandupDriver {
    fn name(&self) -> &'static str {
        "transport-standup-driver"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        // Main thread — the ctors/interface resolve run in the game's own context, like the create driver.
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, _tick: Tick) {
        if self.built {
            self.drive_p2p();
            return;
        }
        // Same world gate as the create driver: a loaded world with the active main player present,
        // not the title/load-transition (the ctors touch game heap/context).
        if !crate::playstate::current().in_game() {
            return;
        }
        if crate::sdk::with_active_main_player(|_| ()).is_none() {
            return;
        }
        self.built = true;
        let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
            Ok(h) => h.0 as usize,
            Err(e) => {
                log::error!("session-probe: transport-standup — GetModuleHandle(NULL) failed: {e}");
                return;
            }
        };
        // Captured out of the build closure so phase 2 (drive_p2p) can drive the resolved interface.
        let mut resolved_iface = 0usize;
        // Captured for the SEAM: the built SteamConnection, and the peer we bind onto it (+0x128) so a
        // SocketManagerHolder wrapping it carries a real {iface,peer}. Peer resolved before the closure
        // (config override or rung-2 link) so the borrow-checker stays happy with the non-move closure.
        let mut conn_out = 0usize;
        let peer_for_conn = self.target_peer().unwrap_or(0);
        // SAFETY: each address is exe base + a charted `.text`/`.data` offset (win64 ABI). A Rust panic
        // is caught here; a hard fault inside a game call surfaces via the crashdump SEH handler
        // (catch_unwind can't catch SIGSEGV). Each step logs before/after so a crash localizes to the
        // last line printed. The constructed object is intentionally leaked (process-lifetime probe).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            // 1. Resolve ISteamNetworking006 into its holder (the legacy P2P interface the transport uses).
            let holder = exe_base + ISTEAM_HOLDER_OFFSET;
            let before = (holder as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — resolving ISteamNetworking006 (holder {holder:#x}, before={before:#x})",
            );
            let resolver: extern "win64" fn(usize) =
                std::mem::transmute(exe_base + ISTEAM_RESOLVER_OFFSET);
            resolver(holder);
            let iface = (holder as *const usize).read_volatile();
            resolved_iface = iface; // hand to phase 2 (drive_p2p) even if a later build step bails
            log::info!(
                "session-probe: transport-standup — ISteamNetworking006 = {iface:#x} ({})",
                if iface == 0 { "NULL — P2P interface unavailable offline!" } else { "resolved OK" },
            );

            // DIAGNOSTIC (2026-07-04 pm): the socketmgr worker thread crashes calling the Steam import at
            // IAT slot 0x144c0d0a4 (SteamInternal_ContextInit) → garbage. The resolver above uses the sibling
            // slots 0x144c0d09c/0x144c0d0ac successfully, so read all three to see if 0x144c0d0a4 alone is
            // unbound/garbage (a delay-load import the game never touches offline). If so, GetProcAddress on
            // steam_api64.dll + patch the slot is the fix. All are fixed .data addresses (no ASLR).
            // These slots are 4-aligned (not 8), so use unaligned reads (x86 qword read is fine; Rust's
            // debug-assert would otherwise panic on the misalignment).
            let iat9c = ((exe_base + 0x4c0d09c) as *const usize).read_unaligned();
            let iata4 = ((exe_base + 0x4c0d0a4) as *const usize).read_unaligned();
            let iatac = ((exe_base + 0x4c0d0ac) as *const usize).read_unaligned();
            log::info!(
                "session-probe: transport-standup — Steam IAT slots: [0x144c0d09c]={iat9c:#x} \
                 [0x144c0d0a4]={iata4:#x} (SteamInternal_ContextInit — worker crashes here) [0x144c0d0ac]={iatac:#x}",
            );

            // 2. Construct a SteamServiceImpl@DLNW3D via its base ctor (bypasses the factory's `owner`).
            // The game allocator `0x141eb9ed0(size, align, heap)` tail-calls `[[r8=heap]+0x50]` — it needs
            // a DLNew heap object in r8 that the factory sources from its `owner` (dormant offline). For
            // this construct-and-scan test we hand the base ctor a leaked, 8-aligned zeroed buffer instead
            // (the service is a process-lifetime probe, never freed). If the sub-ctor `0x14263f1e0` tries
            // to allocate off a heap wired into the object, the next crash localizes that — then we source
            // the game's default heap. `SVC_SIZE`/8 words = the factory's 0x18-byte alloc.
            let buf_vec: &'static mut [usize] = vec![0usize; SVC_SIZE / 8].leak();
            let buf = buf_vec.as_mut_ptr() as usize;
            log::info!("session-probe: transport-standup — service buf (leaked {SVC_SIZE:#x}B) = {buf:#x}");
            let ctor: extern "win64" fn(usize) -> usize =
                std::mem::transmute(exe_base + SVC_BASE_CTOR_OFFSET);
            let svc = ctor(buf);
            let vtable = (svc as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — SteamServiceImpl constructed @ {svc:#x} vtable={vtable:#x} \
                 (SteamServiceImpl vtable static = 0x143277270 + exe rebase; scan-vtable.py to confirm it's live)",
            );

            // 3. The game's DLNR3D heap (the container's own heap, global 0x144842d38 lazily made by
            // 0x141ec61d0). Wire it into [service+8] — the connect thunk sources the heap from there —
            // and use it to allocate the manager. Since the container is live, the heap already exists.
            let heap_global = (exe_base + HEAP_GLOBAL_OFFSET) as *mut usize;
            let mut heap = heap_global.read_volatile();
            if heap == 0 {
                let make_heap: extern "win64" fn() -> usize =
                    std::mem::transmute(exe_base + HEAP_CREATE_OFFSET);
                heap = make_heap();
                heap_global.write_volatile(heap);
                log::info!("session-probe: transport-standup — created DLNR3D heap = {heap:#x}");
            }
            log::info!("session-probe: transport-standup — DLNR3D game heap = {heap:#x}");
            ((svc + 8) as *mut usize).write_volatile(heap);

            // 4. Build the SteamConnectionManager: alloc 0x1b8 off the heap, ctor 0x14263f700(mgr, heap)
            // (installs vtable 0x143278020, allocates its sub-buffers off the heap). Solo-testable — the
            // manager needs no peer; only the connection's Accept does. scan-vtable confirms it's live.
            let alloc: extern "win64" fn(usize, usize, usize) -> usize =
                std::mem::transmute(exe_base + GAME_ALLOC_OFFSET);
            let mgr_buf = alloc(MANAGER_SIZE, 8, heap);
            log::info!("session-probe: transport-standup — manager buf ({MANAGER_SIZE:#x}B off game heap) = {mgr_buf:#x}");
            if mgr_buf == 0 {
                log::error!("session-probe: transport-standup — manager alloc returned NULL; aborting");
                return;
            }
            let mgr_ctor: extern "win64" fn(usize, usize) -> usize =
                std::mem::transmute(exe_base + MANAGER_CTOR_OFFSET);
            let mgr = mgr_ctor(mgr_buf, heap);
            let mgr_vtable = (mgr as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — SteamConnectionManager constructed @ {mgr:#x} vtable={mgr_vtable:#x} \
                 (static = 0x143278020 + exe rebase; scan-vtable.py 0x143278020 to confirm)",
            );

            // 5. FULLY WIRE the connection via the connection-creator 0x142640560(manager, params) — the
            // method the connect thunk 0x14263b720 runs after building the manager. It allocates the
            // manager's ring buffers ([manager+0x88/0xb0/0xd0/0xf8/0x198]) and a SteamConnection array at
            // [manager+0x78] (params.count slots, each 0x140 bytes, ctored via 0x142643b50), then registers
            // the P2P callbacks. The prior increment built a *bare standalone* connection with no ring
            // buffers, so the session's activate faulted (INVALID_HANDLE, crashdump 2026-07-04). params:
            // [0]!=0 and [0x18]=count!=0 are the only guards; count=1, ring size 0 => default 0x4b0.
            let mut params = [0u32; CONN_PARAMS_WORDS];
            params[0] = 1; // [params+0x00] -> [manager+0x40] (nonzero guard)
            params[CONN_PARAMS_COUNT_WORD] = 1; // [params+0x18] -> [manager+0x58] = connection count
            let creator: extern "win64" fn(usize, *const u32) -> bool =
                std::mem::transmute(exe_base + CONN_CREATOR_OFFSET);
            let created = creator(mgr, params.as_ptr());
            log::info!(
                "session-probe: transport-standup — connection-creator 0x142640560(manager, params{{count=1}}) = {created}",
            );
            if !created {
                log::error!("session-probe: transport-standup — connection-creator returned false; array not built");
                return;
            }
            // First connection slot: [manager+0x78] holds the array base; slot 0 = *(manager+0x78) + 0.
            let array_base = ((mgr + CONN_ARRAY_PTR_OFF) as *const usize).read_volatile();
            if array_base == 0 {
                log::error!("session-probe: transport-standup — manager connection array [+0x78] null; aborting");
                return;
            }
            let conn = array_base;
            // Bind ONLY the peer SteamID64 at +0x128 (the ctor zero-inits it; the session's connect path
            // keys on it). Do NOT touch +0x8 — on a *creator-built* connection [conn+0x8] is a DLNW3D
            // sub-object carrying a DLLightMutex (the session locks [[conn+8]+8]); the bare-ctor "+0x8 =
            // iface" from FROMNET §1b does NOT apply here, and overwriting it with the raw iface clobbered
            // that lock → INVALID_HANDLE crash (rig 2026-07-04, fn 0x1423fe030 → 0x142637410 → 0x142638410).
            if peer_for_conn != 0 {
                ((conn + CONN_PEER_OFF) as *mut u64).write_volatile(peer_for_conn);
            }
            // NB: we deliberately do NOT run the per-connection Accept setup (0x14263ffe0) here. It needs a
            // fully game-built connection (the +0x120 iface-holder sub-object our creator-built connection
            // lacks) and crashed at 0x142640075. The force_host_transition lever makes this connection's
            // *activation* unnecessary: it jumps straight to lobby_state=Host, so the session-update task
            // never runs the connection-activation path. The holder at +0x708 only needs to be a valid
            // refcountable object for create's ConnectionRefInfo loop, which it is.
            let conn_vtable = (conn as *const usize).read_volatile();
            let iface_field = ((conn + 0x8) as *const usize).read_volatile();
            let peer_field = ((conn + 0x128) as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — wired SteamConnection @ {conn:#x} (manager array slot 0) \
                 vtable={conn_vtable:#x} [+0x8 iface={iface_field:#x} +0x128 peer={peer_field:#x}]",
            );

            // 6. Build the socket-manager WRAPPER the SocketManagerHolder actually holds (see the
            // SOCKMGR_* chart above). Host-setup dispatches `wrapper->[+8]socketmgr->vtable[3]`, so
            // [holder+0x10] must be this wrapper, NOT the raw connection. Ctor-only socket-manager
            // (no init → no null standup); then the wrapper. Publish the WRAPPER to the seam.
            let sm_buf = alloc(SOCKMGR_SIZE, 8, heap);
            if sm_buf == 0 {
                log::error!("session-probe: transport-standup — socket-manager alloc returned NULL; aborting wrapper build");
                return;
            }
            let sm_ctor: extern "win64" fn(usize) -> usize =
                std::mem::transmute(exe_base + SOCKMGR_CTOR_OFFSET);
            let socketmgr = sm_ctor(sm_buf);
            let sm_vtable = (socketmgr as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — MTInternalThreadSteamSocketManager ctored @ {socketmgr:#x} \
                 vtable={sm_vtable:#x} (static 0x143276cb8 + rebase; ctor-only, no service init)",
            );
            let wrap_buf = alloc(WRAPPER_SIZE, 8, heap);
            if wrap_buf == 0 {
                log::error!("session-probe: transport-standup — wrapper alloc returned NULL; aborting");
                return;
            }
            let wrap_init: extern "win64" fn(usize, usize) -> usize =
                std::mem::transmute(exe_base + WRAPPER_INIT_OFFSET);
            let wrapper = wrap_init(wrap_buf, socketmgr);
            // The builder overwrites the init's transient vtable (0x1430ea580) with the final one.
            ((wrapper) as *mut usize).write_volatile(exe_base + WRAPPER_VTABLE_OFFSET);
            let wrap_vt = (wrapper as *const usize).read_volatile();
            let wrap_inner = ((wrapper + 8) as *const usize).read_volatile();
            log::info!(
                "session-probe: transport-standup — socket-manager WRAPPER built @ {wrapper:#x} \
                 vtable={wrap_vt:#x} (static 0x143276a00) [+8]socketmgr={wrap_inner:#x} — this is what [holder+0x10] holds",
            );
            // Publish the WRAPPER (not the raw connection) for the seam.
            conn_out = wrapper;
        }));
        self.iface = resolved_iface;
        // Publish the built socket-manager WRAPPER for the seam (`land_socket_holder` wraps it into a
        // SocketManagerHolder at [container+0x708], so [holder+0x10]=wrapper — the object host-setup
        // dispatches on). 0 if a build step bailed before the wrapper.
        if conn_out != 0 {
            STANDUP_CONNECTION.store(conn_out, Ordering::Relaxed);
            log::info!(
                "session-probe: transport-standup — published socket-manager wrapper {conn_out:#x} for the seam \
                 (land_socket_holder wraps it at [container+0x708]; host-setup dispatches wrapper->socketmgr->vtable[3])",
            );
        }
        if self.iface != 0 {
            log::info!(
                "session-probe: transport-standup — phase 2 armed: will drive game ISteamNetworking006 P2P \
                 at the rung-2-linked peer (Accept + reliable ping on channel {P2P_PROBE_CHANNEL}) once linked",
            );
        }
    }
}

impl TransportStandupDriver {
    /// Phase 2: drive the game's own **legacy `ISteamNetworking006`** P2P at the rung-2-resolved peer —
    /// the transport the DLNW3D layer uses (distinct from our rung-2 side-channel, which rides
    /// `ISteamNetworkingMessages`). Answers the open question: does the game's legacy P2P work offline,
    /// peer-to-peer, addressed by SteamID alone? Accepts the peer's session once, sends a reliable ping
    /// on a throttle, and drains any inbound packet — so a two-machine run shows whether pings cross.
    fn drive_p2p(&mut self) {
        if self.iface == 0 {
            return;
        }
        let Some(peer) = self.target_peer() else {
            return; // no linked peer and no usable override yet
        };
        let iface = self.iface;
        let do_accept = !self.accepted;
        let do_ping = self.ping_throttle.tick();
        let seq = self.ping_seq.wrapping_add(1);
        let mut accepted_ok = false;
        // SAFETY: `iface` is the resolved ISteamNetworking006 pointer; `[iface]` is its flat vtable and
        // each slot below is a documented method with the win64 signature transmuted here. Buffers are
        // stack-local and outlive each call. Firewalled: a Rust panic is caught; a hard fault surfaces
        // via crashdump. Read-only w.r.t. game state (P2P send/recv only).
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            let vtable = (iface as *const usize).read_volatile();
            let slot = |off: usize| ((vtable + off) as *const usize).read_volatile();
            let accept: extern "win64" fn(usize, u64) -> bool = std::mem::transmute(slot(ISTEAM_ACCEPT_SLOT));
            let send: extern "win64" fn(usize, u64, *const u8, u32, u32, i32) -> bool =
                std::mem::transmute(slot(ISTEAM_SEND_SLOT));
            let is_avail: extern "win64" fn(usize, *mut u32, i32) -> bool =
                std::mem::transmute(slot(ISTEAM_ISAVAIL_SLOT));
            let read: extern "win64" fn(usize, *mut u8, u32, *mut u32, *mut u64, i32) -> bool =
                std::mem::transmute(slot(ISTEAM_READ_SLOT));

            if do_accept {
                let ok = accept(iface, peer);
                accepted_ok = true;
                log::info!(
                    "session-probe: game-p2p — AcceptP2PSessionWithUser(peer {}) = {ok}",
                    unseamless_core::diagnostics::peer_tag(peer),
                );
            }
            // Drain inbound packets on our probe channel (bounded so a flood can't stall the frame).
            let mut buf = [0u8; 256];
            let mut size: u32 = 0;
            for _ in 0..16 {
                if !is_avail(iface, &mut size, P2P_PROBE_CHANNEL) {
                    break;
                }
                let mut remote: u64 = 0;
                let mut got: u32 = 0;
                if read(iface, buf.as_mut_ptr(), buf.len() as u32, &mut got, &mut remote, P2P_PROBE_CHANNEL) {
                    let n = (got as usize).min(buf.len());
                    log::info!(
                        "session-probe: game-p2p — RECV {n}B from peer {}: {:?} <<< GAME LEGACY P2P WORKS OFFLINE",
                        unseamless_core::diagnostics::peer_tag(remote),
                        String::from_utf8_lossy(&buf[..n]),
                    );
                }
            }
            // Throttled outbound reliable ping.
            if do_ping {
                let payload = format!("USC-GAMEP2P#{seq}");
                let ok = send(
                    iface,
                    peer,
                    payload.as_ptr(),
                    payload.len() as u32,
                    P2P_SEND_RELIABLE,
                    P2P_PROBE_CHANNEL,
                );
                log::info!(
                    "session-probe: game-p2p — SendP2PPacket #{seq} to peer {} = {ok}",
                    unseamless_core::diagnostics::peer_tag(peer),
                );
            }
        }));
        if accepted_ok {
            self.accepted = true;
        }
        if do_ping {
            self.ping_seq = seq;
        }
    }
}

/// The session probe's gated frame features, for [`crate::app::build_features`] to `extend` with —
/// mirroring [`crate::diag::probe_features`] so every `[debug.probes]`-gated feature is appended the
/// same way (one assembly style, gating kept inside this module). The FSM-transition logger when
/// `session_probe` is on; the experimental [`SessionCreateDriver`] when `drive_create` is on; the
/// [`TransportStandupDriver`] (path C) when `stand_up_transport` is on.
pub fn probe_features(config: &Config) -> Vec<Box<dyn Feature>> {
    let mut features: Vec<Box<dyn Feature>> = Vec::new();
    if config.debug.probes.stand_up_transport {
        features.push(Box::new(TransportStandupDriver::new(
            config.debug.probes.p2p_test_peer_a,
            config.debug.probes.p2p_test_peer_b,
        )));
    }
    if config.debug.probes.session_probe {
        features.push(Box::new(SessionFsmProbe::new()));
    }
    if config.debug.probes.drive_create {
        features.push(Box::new(SessionCreateDriver::new(
            config.debug.probes.force_netsession_ready,
            config.debug.probes.drive_fire_solo,
            config.debug.probes.force_host_transition,
        )));
    }
    features
}
