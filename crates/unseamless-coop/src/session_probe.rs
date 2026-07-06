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

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use eldenring::cs::{CSSessionManager, CSTaskGroupIndex, LobbyState, ProtocolState};
use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_back};
use unseamless_core::config::{AutoSession, Config};
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
// tracers + SessionCreateDriver below. (Correction 2026-07-05: a static AOB IS derivable for these
// sites — the on-disk bytes at all the session-lifecycle entries are plain, not Arxan-encrypted,
// and extending the shared `88 54 24 10 57 48 83 ec ..` prologue through the `mov qword [rsp+..],-2`
// + first spill makes each wrapper unique in the whole exe; proven by the upstream fromsoftware-rs
// mapper-profile patterns we contributed, which binary-mapper resolves to exactly these offsets.
// We keep fixed offsets + prologue guards here anyway: equally drift-safe and already wired.)
// The guard verifies the entry's charted prologue bytes before patching — after a game update the
// check fails safe (warn + no hook), never hooks
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
    // Read-only host-side admit/roster observation (fires on the receiving machine). Role-independent.
    install_host_accept_trace(config);
    // Read-only stall-B session-machine + P2P-callback observation (B0/B1 of the stall-B aim sheet).
    install_stall_b_trace(config);
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

/// `leave_session 0x140cae730` — the sole out-of-line writer of `lobby_state=OnLeaveSession(7)` and the
/// game-driven-disconnect chokepoint (see docs/SESSION-LIFECYCLE-FINDINGS.md). We hook it read-only to chart
/// what tears down the driven host, or patch its entry to `ret` (`suppress_leave`) to hold the session.
/// The offset is single-sourced from [`crate::stay_connected::LEAVE_SESSION_OFFSET`] (which also carries the
/// byte-verified prologue drift guard) so a game update re-derives this address in exactly one place.
use crate::stay_connected::LEAVE_SESSION_OFFSET;
/// `0x1423f46d0` = the DLNW3D async session teardown handler (`ManagerImplSteam@DLNR3D` vtable slot; 0
/// direct callers, dispatched on a transport connection-down). Hooked read-only to confirm it's what
/// tears down the driven host ~2s after it forms (no real peer → connection-down). See SESSION-LIFECYCLE.
const TEARDOWN_HANDLER_OFFSET: usize = 0x1_423f_46d0 - 0x1_4000_0000;
/// `0x140de2620` — host-setup's final validity gate (reached from `0x140cb2ae0` via the `0x140ddfb20` thunk).
/// Reads the online-session availability signal (`[0x143d855c8]+0x10` / singleton `0x144842d40`); returns
/// false offline, driving `0x140cb2ae0`'s degraded/reset path. `suppress_leave` force-patches it to `ret true`.
const HOST_VALIDITY_GATE_OFFSET: usize = 0x1_40de_2620 - 0x1_4000_0000;

/// Armed by `[debug.probes] drive_establish_handler`: at the veto hook, drive the game's own
/// connection-establish handler `0x1423f2820(container, descriptor)` (which calls the Arxan native builder
/// `container->vtable[0x80]`) to build + wrap + store a connection at `[container+0x708]` natively.
static DRIVE_ESTABLISH_HANDLER: AtomicBool = AtomicBool::new(false);
/// Resolved absolute address of the connection-establish handler `0x1423f2820` (set at install).
static ESTABLISH_HANDLER_FN: AtomicUsize = AtomicUsize::new(0);
/// The live `SessionSteam@DLNR3D` pointer, captured off the `ADD-MEMBER` hook (`rcx`) the first time the
/// driven establishment builds the member pool. Stable while the host stays up; the add-peer driver reads
/// it to drive `0x1423fdc80` without re-deriving the container→session chain. 0 until establishment runs.
static LIVE_SESSION: AtomicUsize = AtomicUsize::new(0);
/// Resolved absolute address of the add-peer entry `0x1423fdc80` (set at install).
static ADD_PEER_FN: AtomicUsize = AtomicUsize::new(0);
/// `0x1423f2820` = `ManagerImpl@DLNR3D`'s connection-establish handler: `container->vtable[0x80]` builds
/// the raw connection, `0x1423f7180` wraps it, it's stored at `[container+0x708]` + addref'd.
const ESTABLISH_HANDLER_OFFSET: usize = 0x1_423f_2820 - 0x1_4000_0000;
/// `0x1423fdf20` = `SessionSteam@DLNR3D` vtable slot 26, the **member-add**. The live writer-trace
/// (ERSC-LIVE-CAPTURE-FINDINGS > "Writer-trace capture") proved a real join reaches this synchronously
/// inside the establish-handler chain (`0x1423f2820 → 0x1423f7070 → 0x1423fdf20 → member ctor
/// 0x142400210 → member+0x80 = peer SteamID64`). Hooked read-only so a DRIVEN establish shows whether it
/// reaches the member-add — the reproduction milestone.
const ADD_MEMBER_OFFSET: usize = 0x1_423f_df20 - 0x1_4000_0000;
/// `0x1423fdc80` = the **add-peer** entry (charted 2026-07-05, docs/SESSION-DRIVE.md > "★★ MEMBER
/// PIPELINE CHARTED"). Distinct from the pre-alloc `0x1423fdf20`: this is the per-peer path, reached
/// from the session's per-frame event drain (`SessionSteam` vt[28] `0x1423ff440` → type-1 event →
/// `0x1423fe350` → here). Given `(rcx=session, rdx=peerInfo, r8=key, r9b=flag)` it looks the peer up by
/// key (dedup `0x1423fbd80`), allocates a session-peer conn (`0x1423fb980`), inits it from `peerInfo`
/// (`0x142402d70`, which writes the peer SteamID64 to the member's `+0x80` via `0x142400480`), and
/// enqueues it on the session's pending-conn queue `[session+0x4f0..+0x4f8]`. Hooking it read-only tells
/// us — on a two-machine run — whether the host ever tries to add the Deck as a peer, and with what
/// identity: if it fires with the Deck's key, the natural producer works and only the handshake pump
/// remains; if it never fires for the Deck, the add-peer *event* was never posted (post it ourselves).
const ADD_PEER_OFFSET: usize = 0x1_423f_dc80 - 0x1_4000_0000;

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

/// The effective per-machine rung-3 driver role `(do_create, do_join)`. The create driver (host) and the
/// join driver (joiner) are mutually exclusive per machine, but the seed config is SHARED across both
/// machines — so gating them on the standalone `drive_create`/`drive_join` flags alone forces the
/// shared-seed footgun the two-machine runs kept hitting (flip the seed's flags between the rig apply and
/// the Deck apply, whoever cycles last wins). `auto_session` is already the per-machine role knob
/// (`--auto-session host|join`, written into each machine's pushed config, never the shared seed); it
/// drives the rung-3 role too, exactly as it drives the rung-2 side-channel role. `off` (solo / manual)
/// falls back to the explicit flags so the solo host workflow (seed `drive_create=true`) is unchanged.
fn rung3_role(config: &Config) -> (bool, bool) {
    // Symmetric-peer mode: BOTH machines take the host role (create+establish, no join). The asymmetric
    // join path conflicts with the establish-built session and crashes the joiner (docs/SESSION-DRIVE.md).
    if config.debug.probes.symmetric_peer {
        return (true, false);
    }
    match config.debug.auto_session {
        AutoSession::Host => (true, false),
        AutoSession::Join => (false, true),
        AutoSession::Off => (config.debug.probes.drive_create, config.debug.probes.drive_join),
    }
}

/// Place read-only `jmp-back` tracers on leg-B entry and the 4th create gate when this machine drives
/// a rung-3 session (create or join). No-op otherwise. Best-effort: a failed hook logs and is skipped,
/// never aborts (it's a diagnostic).
fn install_create_gate_trace(config: &Config) {
    // Installs for the joiner too: the join driver needs the same veto-vmethod hook (land_socket_holder,
    // session-established, the online-availability gate patch) the host does.
    let (do_create, do_join) = rung3_role(config);
    if !do_create && !do_join {
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
    // add-member reach hook (the live-capture reproduction milestone): fires iff a driven establish
    // reaches SessionSteam vt[26] 0x1423fdf20 — the synchronous member-add the writer-trace confirmed.
    // Solo/offline it can only fire if our drivers reproduced the establish chain, so it's the "did we
    // get there" signal. Read-only jmp-back (like leave-session/host-admit — no prologue guard).
    install_offset_hook("add-member", exe_base + ADD_MEMBER_OFFSET, log_add_member);
    // Joiner connection-establish localizers (only meaningful with drive_join): the joiner's [G+0x28]
    // handle comes from the registry's connection-from-blob 0x1423f62e0; it returns 0 for our synthesized
    // host, stranding the joiner at TryToJoinSession. join-conn-entry confirms it's called + logs the blob;
    // join-blob-parse fires ONLY if 0x1423f62e0 got past its registry-ready + descriptor checks and reached
    // the blob parser 0x1423fb260 — so whether it fires localizes the failure (before vs at the blob parse).
    if do_join {
        install_offset_hook("join-conn-entry", exe_base + JOIN_CONN_CREATE_OFFSET, log_join_conn_entry);
        install_offset_hook("join-blob-parse", exe_base + JOIN_BLOB_PARSE_OFFSET, log_join_blob_parse);
    }
    // leave-session (0x140cae730): the sole out-of-line writer of lobby_state=OnLeaveSession. Our driven
    // host session forms (protocol=Ingame, player added, warp starts) then is torn down ~2s later. With
    // `suppress_leave` off we install a READ-ONLY hook logging when it fires + its caller (does the teardown
    // route through here, or the inlined twin 0x140cb08bc in the update task?). With `suppress_leave` on we
    // raw-patch its entry to `ret` — the doc's "early-return before the lobby_state=7 write" gate — to see
    // if the host then sticks in Host/Ingame. (A jmp-back hook can't early-return, so suppression is a patch.)
    // Read-only diagnostics: leave-session + teardown-handler both proved NOT on the driven-host reset path
    // (neither fires). The leave-session tracer contends for the same entry bytes as stay_connected's
    // site-A gate (whoever installs first wins; the other degrades with a logged byte-check refusal), so
    // it is gated on `session_probe` — an RE/charting run flips that on and owns the site; a validation
    // run with `drive_create` + `stay_connected` leaves it off and the gate installs cleanly.
    if config.debug.probes.session_probe {
        install_offset_hook("leave-session", exe_base + LEAVE_SESSION_OFFSET, log_leave_session);
    }
    install_offset_hook("teardown-handler", exe_base + TEARDOWN_HANDLER_OFFSET, log_teardown_handler);
    // `suppress_leave` (repurposed): the driven host actually resets via host-setup's OWN final validity
    // gate `0x140de2620` (reached from `0x140cb2ae0` via `0x140ddfb20`), which reads the online-session
    // availability signal (`[0x143d855c8]+0x10`, the item-grey singleton `0x144842d40`) and returns false
    // offline → `0x140cb2ae0` takes its degraded/reset path `0x140cb3b80`. Bypassing that gate is legitimate
    // for our offline co-op by construction. When armed, patch `0x140de2620` to `mov al,1; ret` (always
    // "online available") to test whether the host then STICKS in Host/Ingame and the warp into the co-op
    // map completes. Bounded rig experiment.
    if config.debug.probes.suppress_leave {
        let gate = exe_base + HOST_VALIDITY_GATE_OFFSET;
        match patch_bytes(gate, &[0xB0, 0x01, 0xC3]) {
            Ok(()) => log::info!(
                "session-probe: host-validity gate 0x140de2620 FORCED true (mov al,1; ret) — bypassing the \
                 online-availability signal; testing whether the driven host sticks + warps in"
            ),
            Err(e) => log::error!("session-probe: host-validity gate force-patch failed: {e}"),
        }
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
    // Drive the establish handler on the HOST role only. It's the host's session-build path; on a joiner
    // (do_join) driving `drive_join` toward Client(6), also driving establish toward Host(3) is the FSM
    // conflict that teardown-crashes the joiner (docs/STATE.md > "Next"; the crash `symmetric_peer` sidestepped
    // by making both hosts). The seed is SHARED across both machines, so a joiner would otherwise inherit the
    // host's `drive_establish_handler = true`; gate it on `do_create` so the per-machine role (auto_session)
    // governs — host establishes, client joins, no fight. `symmetric_peer` (both = host role) still drives it
    // on both, as intended.
    DRIVE_ESTABLISH_HANDLER
        .store(do_create && config.debug.probes.drive_establish_handler, Ordering::Relaxed);
    ESTABLISH_HANDLER_FN.store(exe_base + ESTABLISH_HANDLER_OFFSET, Ordering::Relaxed);
    ADD_PEER_FN.store(exe_base + ADD_PEER_OFFSET, Ordering::Relaxed);
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

/// leave-session tracer: fires when `leave_session 0x140cae730` runs (the game deciding to end the
/// session). Logs the `CSSessionManager*` (rcx), the current `lobby_state`, and the caller's return
/// address (`[rsp]` at a function-entry hook) so we can chart WHO tears down the driven host. Read-only.
fn log_leave_session(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook hands us the saved registers at the entry; rcx = the CSSessionManager, and [rsp]
        // is the return address the `call` pushed (the caller). All reads are bounded and null-guarded.
        let r = unsafe { &*regs };
        let this = r.rcx as usize;
        let caller = if r.rsp != 0 { unsafe { (r.rsp as *const usize).read_volatile() } } else { 0 };
        let lobby = if this != 0 { unsafe { ((this + 0xc) as *const u32).read_volatile() } } else { 0xffff_ffff };
        log::info!(
            "session-probe: leave-session — 0x140cae730(this={this:#x}) lobby_state={lobby} caller={caller:#x} \
             (the game is ending the session; caller-ImageBase offset = caller-0x140000000)",
        );
    }));
}

/// add-member reach hook: `0x1423fdf20` = `SessionSteam@DLNR3D` vt[26]. Fires iff a driven establish
/// reaches the member-add (the live-capture milestone). Logs the session (rcx), the two handle args
/// (rdx/r8 = the future `member+0x70`/`+0x78`), and the caller so we can confirm it came via the
/// establish-handler chain. Read-only.
fn log_add_member(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: entry registers from ilhook; rcx = the SessionSteam, rdx/r8 = the two handle args,
        // [rsp] = the return address. All reads bounded/null-guarded.
        let r = unsafe { &*regs };
        let session = r.rcx as usize;
        let arg1 = r.rdx as usize;
        let arg2 = r.r8 as usize;
        // Capture the live SessionSteam so the add-peer driver can reach it without the container chain.
        if session != 0 {
            LIVE_SESSION.store(session, Ordering::Relaxed);
        }
        let caller = if r.rsp != 0 { unsafe { (r.rsp as *const usize).read_volatile() } } else { 0 };
        log::info!(
            "session-probe: ★ ADD-MEMBER REACHED — 0x1423fdf20(session={session:#x}, arg1={arg1:#x}, \
             arg2={arg2:#x}) caller={caller:#x} — a driven establish reached SessionSteam vt[26]; the \
             member's +0x80 gets the peer SteamID64 (live-capture chain reproduced this far)",
        );
    }));
}

/// add-peer reach hook: `0x1423fdc80(rcx=session, rdx=peerInfo, r8=key, r9b=flag)`. Fires when the session
/// event-drain dispatches a type-1 (add-peer) event — i.e. the game decided to bring a peer into the
/// session. Logs the session, the peer key it read from `[peerInfo]` and `[key]` (formatted both raw and as
/// a SteamID64 tag, so we can see which field carries the identity), the flag, and the caller. Read-only.
/// On the host, this firing for the Deck's SteamID means the producer works and the remaining gap is the
/// handshake pump; its NOT firing for the Deck means the add-peer event was never posted for it.
fn log_add_peer(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers. rcx=session, rdx=peerInfo (a small stack struct), r8=key (ptr to a
        // resolved key), r9b=flag, [rsp]=return addr. peerInfo[0]/key[0] are single bounded qword reads,
        // null-guarded; we only read scalars and log them, never deref deeper.
        let r = unsafe { &*regs };
        let session = r.rcx as usize;
        let peer_info = r.rdx as usize;
        let key = r.r8 as usize;
        let pi0 = if peer_info != 0 { unsafe { (peer_info as *const u64).read_volatile() } } else { 0 };
        let key0 = if key != 0 { unsafe { (key as *const u64).read_volatile() } } else { 0 };
        let flag = r.r9 as u8;
        let caller = if r.rsp != 0 { unsafe { (r.rsp as *const usize).read_volatile() } } else { 0 };
        log::info!(
            "session-probe: ★ ADD-PEER — 0x1423fdc80(session={session:#x}, peerInfo={peer_info:#x}, key={key:#x}, \
             flag={flag}) [peerInfo]={pi0:#x} ({}) [key]={key0:#x} ({}) caller={caller:#x} — the session is \
             bringing a peer in; if the id is the Deck's, the natural producer works",
            unseamless_core::diagnostics::peer_tag(pi0),
            unseamless_core::diagnostics::peer_tag(key0),
        );
    }));
}

/// Registry connection-from-blob `0x1423f62e0(registry, descriptor, blob_begin, blob_len)` — the joiner's
/// [G+0x28] handle source. Hooked read-only to confirm it's called and log the blob range.
const JOIN_CONN_CREATE_OFFSET: usize = 0x1_423f_62e0 - 0x1_4000_0000;
/// Blob parser `0x1423fb260(conn, blob_begin, blob_len, arg)` — reached only if `0x1423f62e0` passed its
/// registry-ready + descriptor checks + created the connection. Whether it fires localizes the failure.
const JOIN_BLOB_PARSE_OFFSET: usize = 0x1_423f_b260 - 0x1_4000_0000;

/// join-conn-entry tracer: `0x1423f62e0(rcx=registry, rdx=descriptor, r8=blob_begin, r9d=blob_len)`.
fn log_join_conn_entry(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; args are by-value / bounded reads, all logged not derefed deeply.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: join-conn-entry — 0x1423f62e0(registry={:#x}, descriptor={:#x}, blob_begin={:#x}, \
             blob_len={}) — the joiner's [G+0x28] handle source (returns 0 => joiner stuck at TryToJoinSession)",
            r.rcx, r.rdx, r.r8, r.r9 as u32,
        );
    }));
}

/// join-blob-parse tracer: `0x1423fb260(rcx=conn, rdx=blob_begin, r8d=blob_len, r9d=arg)`. If this fires, the
/// connection WAS created (0x1423f62e0's earlier checks passed) and the failure (if any) is at/after the blob
/// parse; if it never fires, the failure is earlier (registry-ready or descriptor validation).
fn log_join_blob_parse(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; conn+0x58 is a bounded read, null-guarded.
        let r = unsafe { &*regs };
        let conn = r.rcx as usize;
        let c58 = if conn != 0 { unsafe { ((conn + 0x58) as *const usize).read_volatile() } } else { 0 };
        log::info!(
            "session-probe: join-blob-parse REACHED — 0x1423fb260(conn={conn:#x}, blob_begin={:#x}, \
             blob_len={}, arg={}); [conn+0x58]={c58:#x} (connection WAS created; failure is at/after the parse)",
            r.rdx, r.r8 as u32, r.r9 as u32,
        );
    }));
}

// --- Host-side admit / roster observation (read-only) -------------------------------------------
//
// The receiving (host) machine's inbound path, charted 2026-07-04 (docs/SESSION-DRIVE.md > "HOST-SIDE
// ADMIT/ROSTER"). The socket-manager worker thread `0x142640bc0` drains ISteamNetworking006 packets;
// for a datagram whose sender SteamID64 matches no existing connection it calls the **admit-new-peer**
// helper `0x142640e30(this=socketmgr, sender=rdx, buf=r8, msgSize=r9d)` — which admits the peer only if
// the datagram is a real DLNW3D SYN (size + control-type gate). Separately, the session-update task
// promotes drained connection messages into `players` (the roster) via `0x140cb31b0(this=CSSessionManager,
// msg=rdx)` — the append happens (no offline gate) when the peer isn't already a `players` entry,
// `lobby_state==Host`, and it's not us. Hooking both read-only tells us, on a two-machine run, whether the
// joiner's game-P2P reaches the host's admit path at all (and with what packet shape), and whether a
// roster-add is ever attempted. Pure observers — they log and jmp back, never touch the args.
/// Host admit-new-peer helper `0x142640e30` (socket-manager worker thread; sender SteamID64 in rdx).
const HOST_ADMIT_OFFSET: usize = 0x1_4264_0e30 - 0x1_4000_0000;
/// Session-layer roster-add `0x140cb31b0` (main-thread update task; `msg` in rdx, `CSSessionManager` in rcx).
const HOST_ROSTER_ADD_OFFSET: usize = 0x1_40cb_31b0 - 0x1_4000_0000;
/// Admit gate-c result `0x142640ecd` (`test eax,eax` right after `call [socketmgr+0x40]`): the identity
/// callback's verdict — `eax==0` accepts, non-zero rejects. Localizes whether a SYN that reached the admit
/// entry passes the one identity-keyed gate.
const HOST_ADMIT_GATEC_OFFSET: usize = 0x1_4264_0ecd - 0x1_4000_0000;
/// Admit SUCCESS `0x142640ee4` (past all gates): the host is creating a connection object for the new peer
/// (`call [socketmgr_vtable+8]`). If this fires, the host ADMITTED the joiner — a host-side connection exists.
const HOST_ADMIT_SUCCESS_OFFSET: usize = 0x1_4264_0ee4 - 0x1_4000_0000;
/// Socket-manager worker-thread P2P drain loop `0x142640bc0` (`this=socketmgr` in rcx). Hooking its entry
/// confirms the worker actually runs offline and reveals the channel it reads (`[this+0x50]`) + how many
/// connections it services (`([this+0xc0]-[this+0xb8])/entry`) — the linchpin for whether a joiner SYN on
/// our probe channel is even seen. Throttled (the loop runs every worker tick).
const HOST_WORKER_DRAIN_OFFSET: usize = 0x1_4264_0bc0 - 0x1_4000_0000;
/// Throttle for [`log_host_worker_drain`] — the drain loop fires every worker tick, so log only the first
/// few to confirm it runs + capture the channel, then go silent.
static WORKER_DRAIN_LOGS: AtomicU32 = AtomicU32::new(0);
/// The exe base, latched at `install_host_accept_trace`, so [`log_host_admit`] can convert the live
/// member-resolve pointers into RVAs to name them (is `[S+0x168]` the stub `0x1423fdf00`, or a real lookup?).
static ADMIT_EXE_BASE: AtomicUsize = AtomicUsize::new(0);
/// Throttle for the S-chain detail in [`log_host_admit`] — the admit fires on every retried SYN, so log the
/// member-manager/lookup/collection dump only the first few times, then let the plain admit line stand alone.
static ADMIT_SCHAIN_LOGS: AtomicU32 = AtomicU32::new(0);
/// The stub `0x1423fdf00` (`mov eax,1; ret`, always "not found"). If the live `[S+0x168]` lookup vmethod
/// equals this RVA, aim-sheet lever 3a (register a member) is inert and we need a fuller service/context
/// init (lever 3b). Charted 2026-07-04 (SESSION-DRIVE.md > host-side admit); confirmed real-vs-stub live here.
const MEMBER_LOOKUP_STUB_RVA: usize = 0x1_423f_df00 - 0x1_4000_0000;
/// The real member-resolve `0x142639d00` (`[socketmgr+0x40]`) — sanity-check the callback we deref through.
const MEMBER_RESOLVE_RVA: usize = 0x1_4263_9d00 - 0x1_4000_0000;

/// host-admit tracer: `0x142640e30(rcx=socketmgr, rdx=senderSteamID64, r8=buf, r9d=msgSize)`. Fires on the
/// worker thread when a datagram from an UNKNOWN peer reaches the admit path — so it firing at all means the
/// joiner's game-P2P crossed to the host's game layer; the size tells us whether it's a real 14-byte SYN
/// (admitted) or our raw probe ping (rejected by the shape gate).
fn log_host_admit(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rdx/r9d are by-value scalars (sender id, size), logged not derefed.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: host-admit — 0x142640e30 admit-new-peer from sender {} (msgSize={}) \
             — the joiner's game-P2P REACHED the host admit path (size 14 => real SYN; else rejected by the shape gate)",
            unseamless_core::diagnostics::peer_tag(r.rdx),
            r.r9 as u32,
        );
        // ★ REAL-VS-STUB probe (aim-sheet lever 3a gate, 2026-07-06): at admit entry rcx = socketmgr = mgr,
        // so we can walk the member-resolve chain the gate rejects on and name its pieces by RVA. Answers:
        // is the lookup vmethod `[S+0x168]` a REAL lookup over the collection, or the stub 0x1423fdf00 (which
        // makes registering a member inert)? And is the collection actually empty? Throttled to the first few.
        let n = ADMIT_SCHAIN_LOGS.fetch_add(1, Ordering::Relaxed);
        if n >= 4 {
            return;
        }
        let base = ADMIT_EXE_BASE.load(Ordering::Relaxed);
        let mgr = r.rcx as usize;
        // SAFETY: mgr = socketmgr (rcx at entry). `+0x40` (resolve cb), `+0x48` (context S) are in-bounds
        // fields; S's `+0x168` (lookup vmethod ptr), `+0x98`/`+0xa0` (collection begin/end), `+0x170`
        // (collection root) are the charted ManagerImpl fields. Every deref null-guarded; read-only.
        let rva = |p: usize| if base != 0 && p >= base { p - base } else { usize::MAX };
        unsafe {
            let rd = |p: usize| (p as *const usize).read_volatile();
            if mgr == 0 {
                return;
            }
            let resolve_cb = rd(mgr + 0x40);
            let s = rd(mgr + 0x48);
            if s == 0 {
                log::info!("session-probe: host-admit S-chain #{n} — mgr={mgr:#x} resolve_cb rva={:#x} (want {MEMBER_RESOLVE_RVA:#x}); S=[mgr+0x48] is NULL", rva(resolve_cb));
                return;
            }
            let lookup_fn = rd(s + 0x168);
            let begin = rd(s + 0x98);
            let end = rd(s + 0xa0);
            let root = rd(s + 0x170);
            let lookup_rva = rva(lookup_fn);
            let is_stub = lookup_rva == MEMBER_LOOKUP_STUB_RVA;
            let coll_bytes = end.saturating_sub(begin);
            log::info!(
                "session-probe: ★ host-admit S-chain #{n} — mgr={mgr:#x} resolve_cb rva={:#x}(want {MEMBER_RESOLVE_RVA:#x}) \
                 S=[mgr+0x48]={s:#x} lookup[S+0x168] rva={lookup_rva:#x} => {} | collection[S+0x98..0xa0]={begin:#x}..{end:#x} \
                 span={coll_bytes:#x}B ({}) root[S+0x170]={root:#x} — {}",
                rva(resolve_cb),
                if is_stub { "STUB 0x1423fdf00 (always not-found)" } else { "REAL lookup" },
                if coll_bytes == 0 { "EMPTY" } else { "has members" },
                if is_stub {
                    "⇒ lever 3a INERT: registering a member won't help; need a fuller service/context init (3b)"
                } else {
                    "⇒ lever 3a VIABLE: register the peer in [S+0x170]/[S+0x98] and the resolve should find it"
                },
            );
        }
    }));
}

/// host-roster-add tracer: `0x140cb31b0(rcx=CSSessionManager, rdx=connectionMessage)`. Fires per drained
/// connection message on the update task; logs the live `players` count (`([G+0x80]-[G+0x78])/0x100`) and
/// `lobby_state` so a growth from 1→2 is visible right at the append site.
fn log_host_roster_add(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx = CSSessionManager. `+0x78/+0x80` (players vector first/last) and
        // `+0xc` (lobby_state) are in-bounds fields; all reads null-guarded.
        let r = unsafe { &*regs };
        let g = r.rcx as usize;
        let (count, lobby) = if g != 0 {
            unsafe {
                let first = ((g + 0x78) as *const usize).read_volatile();
                let last = ((g + 0x80) as *const usize).read_volatile();
                let count = if last >= first && first != 0 { (last - first) / 0x100 } else { 0 };
                (count, ((g + 0xc) as *const u32).read_volatile())
            }
        } else {
            (0, 0xffff_ffff)
        };
        log::info!(
            "session-probe: host-roster-add — 0x140cb31b0(msg={:#x}) on CSSessionManager={g:#x} \
             players_count={count} lobby_state={lobby} (a remote peer becoming a roster entry grows this 1->2)",
            r.rdx,
        );
    }));
}

/// Throttle for [`log_host_admit_gatec`] — a rejected SYN retries every ~2s, so cap the verdict logging.
static ADMIT_GATEC_LOGS: AtomicU32 = AtomicU32::new(0);
/// Armed by `[debug.probes] force_gatec_accept`: force the host admit gate-c verdict to ACCEPT (rax=0) so an
/// inbound not-yet-a-member peer's SYN passes the gate that otherwise rejects it. Set at install.
static FORCE_GATEC_ACCEPT: AtomicBool = AtomicBool::new(false);

/// host-admit-gate-c tracer: `0x142640ecd` (`test eax,eax` after the identity callback). `eax==0` => the
/// gate ACCEPTS and admit proceeds; non-zero => REJECT (the identity-keyed offline wall, if any). When
/// `force_gatec_accept` is armed it also **writes `rax=0`** (every call, ahead of the log throttle) so the
/// following `test eax,eax; jne reject` falls through to ACCEPT — the joiner-admit lever. Throttled logging.
fn log_host_admit_gatec(_name: &'static str, regs: *mut Registers) {
    // SAFETY: ilhook entry registers; capture the callback's real verdict (low 32 of rax) before any force.
    let orig = unsafe { (*regs).rax as u32 };
    let forced = FORCE_GATEC_ACCEPT.load(Ordering::Relaxed);
    if forced {
        // SAFETY: writing rax=0 into the saved context; ilhook restores it before the stolen `test eax,eax`,
        // so the gate sees ACCEPT. Done on every call (not throttled) so every retried SYN is accepted.
        unsafe { (*regs).rax = 0 };
    }
    let n = ADMIT_GATEC_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 6 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log::info!(
            "session-probe: host-admit-gate-c #{n} — identity callback [socketmgr+0x40] returned {orig} \
             ({}){} — 0 ACCEPTS (admit proceeds), non-zero REJECTS the new peer",
            if orig == 0 { "ACCEPT" } else { "REJECT" },
            if forced { " -> FORCED to ACCEPT (rax=0)" } else { "" },
        );
    }));
}

/// host-admit-success tracer: `0x142640ee4` (past all admit gates). If this fires the host is CREATING a
/// connection object for the joiner — the host admitted us. The decisive signal that the joiner's SYN
/// produced a real host-side connection (the step before it can become a `players` roster entry).
fn log_host_admit_success(_name: &'static str, _regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        log::info!(
            "session-probe: host-admit-SUCCESS — 0x142640ee4 the host is CREATING a connection for the \
             admitted peer (all admit gates passed; a host-side connection now exists — the step before roster)",
        );
    }));
}

/// host-worker-drain tracer: `0x142640bc0(rcx=socketmgr)`. Fires on the worker thread each drain tick;
/// throttled to the first few so we learn (a) the worker RUNS offline, (b) which channel it reads
/// (`[socketmgr+0x50]`), and (c) how many connections it services (`[socketmgr+0xb8..0xc0]`). If it reads a
/// channel other than our probe channel 0, a joiner SYN on channel 0 will never reach the admit path.
fn log_host_worker_drain(_name: &'static str, regs: *mut Registers) {
    let n = WORKER_DRAIN_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 5 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx = socketmgr. `+0x50` (channel), `+0xb8/+0xc0` (connection
        // vector begin/end) are in-bounds fields; all reads null-guarded.
        let r = unsafe { &*regs };
        let sm = r.rcx as usize;
        let (channel, begin, end) = if sm != 0 {
            unsafe {
                (
                    ((sm + 0x50) as *const i32).read_volatile(),
                    ((sm + 0xb8) as *const usize).read_volatile(),
                    ((sm + 0xc0) as *const usize).read_volatile(),
                )
            }
        } else {
            (-1, 0, 0)
        };
        // Connection-vector stride is unknown here; report the raw byte span so a non-empty span is visible.
        let span = end.saturating_sub(begin);
        log::info!(
            "session-probe: host-worker-drain #{n} — 0x142640bc0 socketmgr={sm:#x} reads channel={channel} \
             connections_span={span:#x}B (worker RUNS offline; a joiner SYN must land on THIS channel to be admitted)",
        );
    }));
}

/// Install the read-only host-admit + roster-add tracers when `[debug.probes] instrument_host_accept` is on.
/// Role-independent (fires on whichever machine receives an inbound peer). Best-effort: a failed hook logs
/// and is skipped. NB: `host-admit` fires on the socket-manager WORKER THREAD, not the main thread — the
/// handler only logs (no game-state touch), so that's safe.
fn install_host_accept_trace(config: &Config) {
    if !config.debug.probes.instrument_host_accept {
        return;
    }
    let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
        Ok(h) => h.0 as usize,
        Err(e) => {
            log::error!("session-probe: host-accept trace — GetModuleHandle(NULL) failed: {e}");
            return;
        }
    };
    ADMIT_EXE_BASE.store(exe_base, Ordering::Relaxed);
    FORCE_GATEC_ACCEPT.store(config.debug.probes.force_gatec_accept, Ordering::Relaxed);
    if config.debug.probes.force_gatec_accept {
        log::info!("session-probe: host-admit gate-c FORCE-ACCEPT armed (joiner-admit lever) — inbound SYNs pass gate-c");
    }
    install_offset_hook("host-admit", exe_base + HOST_ADMIT_OFFSET, log_host_admit);
    install_offset_hook("host-admit-gate-c", exe_base + HOST_ADMIT_GATEC_OFFSET, log_host_admit_gatec);
    install_offset_hook("host-admit-success", exe_base + HOST_ADMIT_SUCCESS_OFFSET, log_host_admit_success);
    install_offset_hook("host-roster-add", exe_base + HOST_ROSTER_ADD_OFFSET, log_host_roster_add);
    install_offset_hook("host-worker-drain", exe_base + HOST_WORKER_DRAIN_OFFSET, log_host_worker_drain);
    install_offset_hook("add-peer", exe_base + ADD_PEER_OFFSET, log_add_peer);
    log::info!(
        "session-probe: host-accept trace installed (host-admit 0x142640e30 + gate-c 0x142640ecd + success \
         0x142640ee4 + host-roster-add 0x140cb31b0 + host-worker-drain 0x142640bc0 + add-peer 0x1423fdc80)"
    );
}

// --- Stall-B session-machine observation (B0/B1, read-only) --------------------------------------
//
// Charted 2026-07-05 (docs/SESSION-DRIVE.md > "★ STALL-B HANDSHAKE AIM SHEET"): there are TWO phase
// machines sharing one driver, and the client's `WaitInitData` park is the SESSION machine (state
// `SessionSteam+0x3cc`) waiting for the host's init-data — one level above the member/type-5 machine.
// The host moves first, but only once a real joiner `SteamConnection` exists, and that connection is
// created by the DLNW3D socket's three Steam P2P callbacks on a real inbound connect (drive_add_peer
// builds only the identity handle). So the two decisive observations on a two-machine run are:
//   B0 (client): does the session state (+0x3cc) ever reach 2 (established)? does the pending-conn
//       span (+0x4f0..+0x4f8) ever hold a connection? which wait status is it parked in?
//   B1 (host):  do the P2P socket callbacks EVER fire when the Deck connects? If none fire, the
//       Deck's transport connect never reaches the host's DLNW3D socket (a transport-layer problem
//       upstream of the session), the host builds no connection, sends no init-data, and the client
//       waits forever — the whole stall in one latch.
// All read-only, log-on-change / first-N throttled, panic-firewalled. Addresses re-derivable from the
// aim sheet (each was verified by an independent static pass on the clean exe, 2026-07-05).

// ⚠ DO NOT HOOK `0x1423fb684` (the charted "per-tick session update"). It is a MID-FUNCTION label
// inside the client-side update path, not a call boundary: hooking it (2026-07-05 run) crashed the
// Deck client within ~2 frames of its join — the main thread died with no crashdump, exactly when the
// joined session's pump first executed through the patched bytes — while the host, which never runs
// that branch, stayed alive with the identical hook and its tracer logged NOTHING on either machine
// (rcx there is not a SessionSteam). The session state + pending-conn span are watched by POLLING
// instead: the init-gate/wait hooks (real vtable-dispatched entries) capture the live SessionSteam
// pointer into [`STALLB_SESSION`], and [`SessionFsmProbe::on_frame`] reads `+0x3cc/+0x3d0/+0x4f0/
// +0x4f8` from our own frame task, logging on change.
/// The session INIT gate `0x1423fbe10` (`session_vtable[0x120]`): returns `session+0x48 > threshold`
/// (threshold = `[phaseblock+0x10]`, phaseblock = `[[[session+0x18]]+0x10]`). Returning 1 = still
/// waiting for the host's init-data (the stall-B park condition); flipping to 0 = init-data received.
const SESSION_INIT_GATE_OFFSET: usize = 0x1_423f_be10 - 0x1_4000_0000;
/// The session wait handler `0x1423fb900` (`session_vtable[0x118]`): entered with a packed status code
/// in rdx that names WHICH wait the session machine parked in (e.g. `0x8104020200000000`).
const SESSION_WAIT_HANDLER_OFFSET: usize = 0x1_423f_b900 - 0x1_4000_0000;
/// The three DLNW3D socket P2P-callback REGISTRARS. The 2026-07-05 two-machine run corrected the aim
/// sheet's reading: these fire once at the session's own standup (host establish / client join), each
/// passed the REAL runtime callback target as a function pointer in rdx — `0x1423fd550/560/570`,
/// identical on both machines. They are registration observers; the connect-event tracers are the
/// `P2P_EVT_*` hooks on the observed targets below.
const P2P_CB_A_OFFSET: usize = 0x1_423f_84a0 - 0x1_4000_0000;
/// Second P2P callback registrar (see [`P2P_CB_A_OFFSET`]).
const P2P_CB_B_OFFSET: usize = 0x1_423f_8420 - 0x1_4000_0000;
/// Third P2P callback registrar (see [`P2P_CB_A_OFFSET`]).
const P2P_CB_C_OFFSET: usize = 0x1_423f_8620 - 0x1_4000_0000;
/// The REAL runtime P2P callbacks — the fn-pointer values the registrars were observed passing (rdx)
/// live on both machines, 2026-07-05. Genuine function entries by construction (they're stored and
/// invoked as pointers). ANY fire on the host after a joiner connects is the B1 money signal: an
/// inbound P2P connect event reached the DLNW3D socket layer (the connection-creation door).
const P2P_EVT_A_OFFSET: usize = 0x1_423f_d550 - 0x1_4000_0000;
/// Second real P2P callback (see [`P2P_EVT_A_OFFSET`]).
const P2P_EVT_B_OFFSET: usize = 0x1_423f_d560 - 0x1_4000_0000;
/// Third real P2P callback (see [`P2P_EVT_A_OFFSET`]).
const P2P_EVT_C_OFFSET: usize = 0x1_423f_d570 - 0x1_4000_0000;

/// The live `SessionSteam` pointer, captured from rcx at the init-gate/wait hooks (real vtable
/// entries), read by [`SessionFsmProbe::on_frame`]'s stall-B state poll. Cleared when the FSM
/// returns to `None` (the object is torn down with the session — don't read a dangling pointer).
static STALLB_SESSION: AtomicUsize = AtomicUsize::new(0);
/// Last-seen packed session state at the poll: `+0x3cc | (+0x3d0 << 32)`. Sentinel = never seen.
static LAST_SESSION_STATE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Last-seen pending-conn span (raw bytes between `+0x4f0` and `+0x4f8`). Sentinel = never seen.
static LAST_PENDING_SPAN: AtomicUsize = AtomicUsize::new(usize::MAX);
/// Last-seen INIT-gate verdict (0/1); 2 = never seen. Logged on flip (plus the first few samples).
static LAST_INIT_GATE_VERDICT: AtomicU32 = AtomicU32::new(2);
/// Sample counter for [`log_session_init_gate`] — the gate runs per tick while parked, so log the
/// first few (to capture counter/threshold) then only verdict flips.
static INIT_GATE_LOGS: AtomicU32 = AtomicU32::new(0);
/// Last-seen wait-handler status code (rdx). Sentinel = never seen; logged on change, capped.
static LAST_WAIT_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);
/// Distinct-status log cap for [`log_session_wait_handler`].
static WAIT_STATUS_LOGS: AtomicU32 = AtomicU32::new(0);
/// Shared log cap across the three P2P-callback tracers (each fire is a ★ event; cap the retries).
static P2P_CB_LOGS: AtomicU32 = AtomicU32::new(0);

// --- B4: the client connect chain + the host tag-handlers (2026-07-05 evening chart) --------------
// From SESSION-DRIVE.md > "★ P2P-EVENT + CLIENT-CONNECT AIM SHEET". The client's transport connect is
// a state-4 session-phase chain (fc400 → faa00 → fcfc0 → fcdd0 → vt[0xf8] = 0x142401e80, the actual
// Steam connect); the host's connection-creator is the tag-1 event handler 0x1423fe350. NB: the event
// DRAIN 0x1423ff446 from B4-e is deliberately NOT hooked — another unaligned mid-region address in the
// same update path where 0x1423fb684 crashed the client; the entry latches below decide the fork
// without it.

/// The client connect-init `0x142401e80` (`SessionSteam` vt[0xf8]): resolves the Steam networking
/// interface `0x143b48a00` and calls its `vtable[0x40]` (the actual transport open/connect), clearing
/// `[(session+0x5a8)+0x12]` on completion. THE decisive B4-a latch: fires = the client attempted the
/// connect (wall downstream); never = the phase chain stalls upstream.
const CLIENT_CONNECT_INIT_OFFSET: usize = 0x1_4240_1e80 - 0x1_4000_0000;
/// State-4 phase-chain step `0x1423fcdd0` (the step that dispatches vt[0xf8]). Latch: how far the
/// chain walks.
const PHASE_FCDD0_OFFSET: usize = 0x1_423f_cdd0 - 0x1_4000_0000;
/// State-4 phase-chain step `0x1423fcfc0` (vt[0xa0], upstream of fcdd0). Latch: chain progress.
const PHASE_FCFC0_OFFSET: usize = 0x1_423f_cfc0 - 0x1_4000_0000;
/// The host tag-1 event handler `0x1423fe350` (ConnectionStatusChanged — the REAL connection-creator:
/// host-accept `0x1423fe190` + add-peer `0x1423fdc80`). B4-d latch: it firing on the host = the
/// client's connect reached the host = stall B moved.
const TAG1_HANDLER_OFFSET: usize = 0x1_423f_e350 - 0x1_4000_0000;
/// The tag-0 handler's bail site `0x1423fe52a` (`test rax,rax; je <return>` right after the
/// connection lookup `0x1423fbd80`). Mid-function — hooked only under the prologue guard below.
/// rax==0 here = d560 bailed on "no existing connection for this peer" (B4-c).
const TAG0_BAIL_OFFSET: usize = 0x1_423f_e52a - 0x1_4000_0000;
/// Charted bytes at [`TAG0_BAIL_OFFSET`]: `48 85 C0` = `test rax,rax`, `0F 84` = the NEAR `je`
/// (rel32 target left unpinned; run 5's guard refusal showed the on-disk form is near, not rel8).
/// Mid-function hook ⇒ guard or skip, per the legb-finalize precedent.
const TAG0_BAIL_PROLOGUE: [u8; 5] = [0x48, 0x85, 0xC0, 0x0F, 0x84];

// --- B5: the host SEND phase + the real game SendP2PPacket (2026-07-05 night) --------------------
// The peerwire chart's B5 lever is "prime session+0x4f0 so the host's send phase 0x1423ff2e0 emits the
// first SendP2PPacket(joinerID)". BUT the run-4/6 logs show our drive_add_peer ALREADY enqueues the
// joiner member (queue 600..608 → 600..610, returns 1) and it persists the whole session, yet no send
// leaves. So the real wall is the send phase not firing on a queued member. These two entry latches
// answer it directly: does the game's own type-6 send phase run, and does ANY real SendP2PPacket leave
// (and to whom)? Both are clean fn entries.
/// Host session type-6 send phase `0x1423ff2e0` (finds the queued member via `0x1423fbd80`, sends
/// init-data). B5: does it EVER run on the host despite a persistently-queued joiner member?
const HOST_SEND_PHASE_OFFSET: usize = 0x1_423f_f2e0 - 0x1_4000_0000;
/// The game's own `SendP2PPacket` wrapper `0x142640b20(mgr, steamID, data, len, sendtype, chan)` in the
/// socket-manager region — distinct from our probe's direct iface call. B5 decisive: any fire = the
/// GAME emitted a real DLNW3D packet; `rdx` = the target SteamID64 (is it the joiner?).
const GAME_SEND_OFFSET: usize = 0x1_4264_0b20 - 0x1_4000_0000;
/// Latch for the host send-phase tracer.
static HOST_SEND_LOGS: AtomicU32 = AtomicU32::new(0);
/// Latch for the game SendP2PPacket tracer.
static GAME_SEND_LOGS: AtomicU32 = AtomicU32::new(0);

/// B5: host type-6 send-phase entry latch. ★ Firing = the host session machine reached its init-data
/// send step (so a queued member IS being serviced); never = the send phase is parked/unreached.
fn log_host_send_phase(_name: &'static str, regs: *mut Registers) {
    let n = HOST_SEND_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 8 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx logged as an opaque value.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: ★ HOST-SEND-PHASE #{n} — 0x1423ff2e0(rcx={:#x}) — the host session's type-6 \
             init-data send step RAN (it searches the pending-conn queue for the joiner member and \
             sends); if this fires but no SendP2PPacket follows, the member lookup/endpoint is the gate",
            r.rcx,
        );
    }));
}

/// B5: the game's own `SendP2PPacket` entry latch — THE decisive probe. Distinct from our probe's
/// direct iface call, so a fire here means the GAME's session/transport code emitted a real DLNW3D
/// packet. `rdx` = the destination SteamID64.
fn log_game_send(_name: &'static str, regs: *mut Registers) {
    let n = GAME_SEND_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 12 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rdx = a by-value SteamID64, r8/r9d by-value scalars.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: ★ GAME-SENDP2P #{n} — 0x142640b20 to {} (len={}) — the GAME emitted a real \
             DLNW3D packet (NOT our probe's synthetic send); target = the joiner ⇒ the send phase works \
             and the bootstrap is downstream (joiner recv)",
            unseamless_core::diagnostics::peer_tag(r.rdx),
            r.r9 as u32,
        );
    }));
}

/// One-shot latches for the B4 entry tracers (log the first few fires each, then quiet).
static CONNECT_INIT_LOGS: AtomicU32 = AtomicU32::new(0);
/// Latch for the fcdd0/fcfc0 phase-step tracers (shared cap; the name disambiguates).
static PHASE_STEP_LOGS: AtomicU32 = AtomicU32::new(0);
/// Latch for the tag-1 handler tracer.
static TAG1_LOGS: AtomicU32 = AtomicU32::new(0);
/// Throttle for the tag-0 bail tracer (a busy host can hit it per inbound event).
static TAG0_BAIL_LOGS: AtomicU32 = AtomicU32::new(0);
/// Last-seen client connect-completion flag `[(session+0x5a8)+0x12]` (B4-a poll; 0xff = never seen).
static LAST_CONNECT_FLAG: AtomicU32 = AtomicU32::new(0xff00);

/// B4-a: client connect-init entry tracer. ★ Firing at all = the phase chain reached the actual
/// transport connect.
fn log_client_connect_init(_name: &'static str, regs: *mut Registers) {
    let n = CONNECT_INIT_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 6 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx/rdx logged as opaque values.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: ★ CLIENT-CONNECT-INIT #{n} — 0x142401e80(rcx={:#x}, rdx={:#x}) — the \
             state-4 phase chain REACHED the transport connect (wall is downstream: the Steam \
             vtable[0x40] call or its target)",
            r.rcx, r.rdx,
        );
    }));
}

/// B4-a: state-4 phase-chain step latches (how far the chain walks before stalling).
fn log_phase_step(name: &'static str, regs: *mut Registers) {
    let n = PHASE_STEP_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 10 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx logged as an opaque value.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: phase-step {name} #{n} — rcx={:#x} (state-4 connect chain progress: \
             fc400 -> faa00 -> fcfc0 -> fcdd0 -> connect-init 0x142401e80)",
            r.rcx,
        );
    }));
}

/// B4-d: host tag-1 (ConnectionStatusChanged) handler latch — the REAL connection-creator. ★ Firing
/// on the host = the client's transport connect arrived = stall B has moved.
fn log_tag1_handler(_name: &'static str, regs: *mut Registers) {
    let n = TAG1_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 8 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx/rdx logged as opaque values.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: ★ TAG1-CONNSTATUS #{n} — 0x1423fe350(rcx={:#x}, rdx={:#x}) — the \
             connection-creating event handler ran (host-accept 0x1423fe190 + add-peer follow); a \
             peer's REAL transport connect reached the session",
            r.rcx, r.rdx,
        );
    }));
}

/// B4-c: tag-0 handler bail-site tracer (`0x1423fe52a`, guarded mid-function). Reads rax = the
/// connection-lookup result: 0 = d560 is bailing on "no existing connection" (the expected cause of
/// "d560 fires but nothing happens").
fn log_tag0_bail(_name: &'static str, regs: *mut Registers) {
    let n = TAG0_BAIL_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 8 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook saved registers at the guarded test site; rax is a by-value scalar.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: tag0-lookup #{n} — 0x1423fe52a rax={:#x} ({}) — the tag-0 (peer-name) \
             handler's connection lookup; 0 = bail (no connection exists for the peer, as charted)",
            r.rax,
            if r.rax == 0 { "BAIL" } else { "found" },
        );
    }));
}

/// Stall-B session-state poll, called each frame from [`SessionFsmProbe::on_frame`] (our own task —
/// no game-code patch; see the DO-NOT-HOOK note above [`SESSION_INIT_GATE_OFFSET`]). Reads the
/// captured `SessionSteam`'s state (`+0x3cc`/`+0x3d0`) and pending-conn span (`+0x4f0..+0x4f8`),
/// logging on change. `session_alive` = the FSM still reports a session (lobby != None); when it
/// drops, the pointer is cleared instead of read (the object dies with the session).
fn poll_stall_b_session(session_alive: bool) {
    let session = STALLB_SESSION.load(Ordering::Relaxed);
    if session == 0 {
        return;
    }
    if !session_alive {
        STALLB_SESSION.store(0, Ordering::Relaxed);
        LAST_SESSION_STATE.store(u64::MAX, Ordering::Relaxed);
        LAST_PENDING_SPAN.store(usize::MAX, Ordering::Relaxed);
        log::info!("session-probe: stall-B poll — session gone (FSM back to None); pointer cleared");
        return;
    }
    // SAFETY: `session` was rcx at a live init-gate/wait call this session (a real SessionSteam),
    // and it's only read while the FSM still reports the session alive. `+0x3cc/+0x3d0` (states) and
    // `+0x4f0/+0x4f8` (pending-conn vector begin/end) are in-bounds fields; bounded scalar reads.
    let (state, state2, begin, end) = unsafe {
        (
            ((session + 0x3cc) as *const u32).read_volatile(),
            ((session + 0x3d0) as *const u32).read_volatile(),
            ((session + 0x4f0) as *const usize).read_volatile(),
            ((session + 0x4f8) as *const usize).read_volatile(),
        )
    };
    let span = end.saturating_sub(begin);
    // B4-a (corrected run 5): the `+0x5a8` connect object is EMBEDDED in the SessionSteam (the
    // connect-init's rcx was literally session+0x5a8), so the completion flag is the byte at
    // session+0x5ba — no deref. The connect-init body clears it unconditionally (`mov byte
    // [rbx+0x12],0` at 0x142401eaf).
    // SAFETY: same live-session guard as above; in-bounds byte reads on the embedded object.
    let connect_flag = u32::from(unsafe { ((session + 0x5a8 + 0x12) as *const u8).read_volatile() });
    let packed = (state as u64) | ((state2 as u64) << 32);
    let prev_state = LAST_SESSION_STATE.swap(packed, Ordering::Relaxed);
    let prev_span = LAST_PENDING_SPAN.swap(span, Ordering::Relaxed);
    let prev_flag = LAST_CONNECT_FLAG.swap(connect_flag, Ordering::Relaxed);
    if prev_state == packed && prev_span == span && prev_flag == connect_flag {
        return;
    }
    // Dump the embedded connect object on change — if a per-peer connect target (e.g. the host's
    // SteamID64) is supposed to live here and it's all zero, the vt[0x40] connect had no target.
    // SAFETY: 0x20 in-bounds bytes of the live SessionSteam.
    let obj: Vec<u8> =
        (0..0x20).map(|i| unsafe { ((session + 0x5a8 + i) as *const u8).read_volatile() }).collect();
    log::info!(
        "session-probe: stall-B poll — session={session:#x} state(+0x3cc)={state} state2(+0x3d0)={state2} \
         pending_conn_span={span:#x}B (~{} ptrs) connect_flag(+0x5ba)={connect_flag:#x} \
         connect_obj[+0x5a8..+0x5c8]={obj:02x?} — state 2 = ESTABLISHED; span 0 = no connection ever \
         landed; connect_flag 0 = connect-init ran to completion",
        span / 8,
    );
}

/// session-init-gate tracer: `0x1423fbe10(rcx=session)` — the INIT gate the session machine parks on.
/// Logs the receive counter (`session+0x48`) vs the phase-block threshold; verdict 1 (counter >
/// threshold) = still waiting for the host's init-data. First few samples + every verdict flip.
fn log_session_init_gate(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx = the session. The threshold chain
        // `[[[session+0x18]]+0x10]+0x10` is walked one bounded, null-guarded qword read at a time.
        let r = unsafe { &*regs };
        let session = r.rcx as usize;
        if session == 0 {
            return;
        }
        // Capture the live SessionSteam for the frame-task stall-B poll (see poll_stall_b_session).
        STALLB_SESSION.store(session, Ordering::Relaxed);
        let counter = unsafe { ((session + 0x48) as *const u64).read_volatile() };
        let p1 = unsafe { ((session + 0x18) as *const usize).read_volatile() };
        let p2 = if p1 != 0 { unsafe { (p1 as *const usize).read_volatile() } } else { 0 };
        let pb = if p2 != 0 { unsafe { ((p2 + 0x10) as *const usize).read_volatile() } } else { 0 };
        let threshold = if pb != 0 { unsafe { ((pb + 0x10) as *const u64).read_volatile() } } else { 0 };
        let verdict = u32::from(counter > threshold);
        let prev = LAST_INIT_GATE_VERDICT.swap(verdict, Ordering::Relaxed);
        let n = INIT_GATE_LOGS.fetch_add(1, Ordering::Relaxed);
        if prev == verdict && n >= 4 {
            return;
        }
        log::info!(
            "session-probe: session-init-gate #{n} — 0x1423fbe10 counter(+0x48)={counter:#x} \
             threshold={threshold:#x} => returns {verdict} (1 = still WAITING for the host's init-data; \
             a flip to 0 = init-data received, the session machine advances)",
        );
    }));
}

/// session-wait-handler tracer: `0x1423fb900(rcx=session, rdx=status)` — the park target. The packed
/// status code in rdx names WHICH wait the session machine is stuck in; logged on change.
fn log_session_wait_handler(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rdx is a by-value packed status code, logged not derefed.
        let r = unsafe { &*regs };
        if r.rcx != 0 {
            STALLB_SESSION.store(r.rcx as usize, Ordering::Relaxed);
        }
        let status = r.rdx;
        let prev = LAST_WAIT_STATUS.swap(status, Ordering::Relaxed);
        if prev == status || WAIT_STATUS_LOGS.fetch_add(1, Ordering::Relaxed) >= 8 {
            return;
        }
        log::info!(
            "session-probe: session-wait — 0x1423fb900 status={status:#x} (which wait the session \
             machine parked in; see the aim sheet's status codes)",
        );
    }));
}

/// P2P callback-REGISTRAR tracer (shared by all three; `name` disambiguates). Fires once at the
/// session's own standup with the REAL runtime callback target in rdx — the observation that pinned
/// `0x1423fd550/560/570` as the actual connect-event callbacks (2026-07-05). Read-only.
fn log_p2p_socket_callback(name: &'static str, regs: *mut Registers) {
    let n = P2P_CB_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 12 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx/rdx are logged as opaque values, never derefed.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: P2P-CALLBACK-REGISTRAR {name} #{n} — ctx={:#x} callback_fn={:#x} — the \
             session standup registering its P2P callback (the fn pointer is the REAL connect-event \
             target; the p2p-evt-* hooks watch those)",
            r.rcx, r.rdx,
        );
    }));
}

/// Shared log cap across the three REAL P2P callback tracers.
static P2P_EVT_LOGS: AtomicU32 = AtomicU32::new(0);

/// Real P2P callback tracer (shared by the three observed targets; `name` disambiguates). ANY fire on
/// the host after a joiner connects is the B1 money signal: an inbound P2P connect event reached the
/// DLNW3D socket layer — the connection-creation door that builds the joiner's `SteamConnection` and
/// lets the host's session send init-data. Read-only.
fn log_p2p_event_callback(name: &'static str, regs: *mut Registers) {
    let n = P2P_EVT_LOGS.fetch_add(1, Ordering::Relaxed);
    if n >= 12 {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook entry registers; rcx/rdx/r8 are logged as opaque values, never derefed.
        let r = unsafe { &*regs };
        log::info!(
            "session-probe: ★ P2P-EVENT {name} #{n} — rcx={:#x} rdx={:#x} r8={:#x} — a REAL P2P \
             connect/session event reached the DLNW3D socket layer (the connection-creation door; \
             on the host this is what builds the joiner's connection so init-data can be sent)",
            r.rcx, r.rdx, r.r8,
        );
    }));
}

/// Install the read-only stall-B tracers (B0: the client session machine; B1: the host P2P socket
/// callbacks) whenever this machine drives a rung-3 session in either role. Both blocks install on
/// both machines — the tick/gate/wait tracers watch whichever session this machine builds, and the
/// P2P callbacks only fire when a real inbound connect arrives (the host, in the current shape).
/// Best-effort like every probe: a failed hook logs and is skipped.
fn install_stall_b_trace(config: &Config) {
    let (do_create, do_join) = rung3_role(config);
    if !do_create && !do_join {
        return;
    }
    let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
        Ok(h) => h.0 as usize,
        Err(e) => {
            log::error!("session-probe: stall-B trace — GetModuleHandle(NULL) failed: {e}");
            return;
        }
    };
    install_offset_hook("session-init-gate", exe_base + SESSION_INIT_GATE_OFFSET, log_session_init_gate);
    install_offset_hook("session-wait", exe_base + SESSION_WAIT_HANDLER_OFFSET, log_session_wait_handler);
    install_offset_hook("p2p-cb-84a0", exe_base + P2P_CB_A_OFFSET, log_p2p_socket_callback);
    install_offset_hook("p2p-cb-8420", exe_base + P2P_CB_B_OFFSET, log_p2p_socket_callback);
    install_offset_hook("p2p-cb-8620", exe_base + P2P_CB_C_OFFSET, log_p2p_socket_callback);
    install_offset_hook("p2p-evt-d550", exe_base + P2P_EVT_A_OFFSET, log_p2p_event_callback);
    install_offset_hook("p2p-evt-d560", exe_base + P2P_EVT_B_OFFSET, log_p2p_event_callback);
    install_offset_hook("p2p-evt-d570", exe_base + P2P_EVT_C_OFFSET, log_p2p_event_callback);
    // B4 (the client connect chain + host tag-handlers; SESSION-DRIVE.md > "P2P-EVENT + CLIENT-CONNECT
    // AIM SHEET"). All fn entries except tag0-bail, which is mid-function and prologue-guarded.
    install_offset_hook("connect-init", exe_base + CLIENT_CONNECT_INIT_OFFSET, log_client_connect_init);
    install_offset_hook("phase-fcdd0", exe_base + PHASE_FCDD0_OFFSET, log_phase_step);
    install_offset_hook("phase-fcfc0", exe_base + PHASE_FCFC0_OFFSET, log_phase_step);
    install_offset_hook("tag1-connstatus", exe_base + TAG1_HANDLER_OFFSET, log_tag1_handler);
    let tb = exe_base + TAG0_BAIL_OFFSET;
    if prologue_ok("tag0-bail", tb, &TAG0_BAIL_PROLOGUE) {
        install_offset_hook("tag0-bail", tb, log_tag0_bail);
    }
    // B5: the host send phase + the real game SendP2PPacket (does the game emit any real packet?).
    install_offset_hook("host-send-phase", exe_base + HOST_SEND_PHASE_OFFSET, log_host_send_phase);
    install_offset_hook("game-sendp2p", exe_base + GAME_SEND_OFFSET, log_game_send);
    log::info!(
        "session-probe: stall-B trace installed (init gate 0x1423fbe10 + wait handler 0x1423fb900 + \
         P2P registrars 0x1423f84a0/8420/8620 + REAL P2P callbacks 0x1423fd550/560/570; session state \
         polled per-frame from the FSM probe) — B0/B1 of docs/SESSION-DRIVE.md > \"STALL-B HANDSHAKE \
         AIM SHEET\""
    );
}

/// teardown-handler tracer: fires when the DLNW3D async teardown handler `0x1423f46d0` runs (the transport
/// reacting to a connection-down). Logs rcx (the container) + `lobby_state`/`[container+0x7c0]` status, so
/// we can confirm this is what tears the driven host down. Read-only.
fn log_teardown_handler(_name: &'static str, regs: *mut Registers) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: ilhook hands us the entry registers; rcx = the container (`ManagerImplSteam@DLNR3D`),
        // and `+0x7c0` is its in-bounds status bitfield. Both reads are null-guarded.
        let r = unsafe { &*regs };
        let container = r.rcx as usize;
        let status = if container != 0 {
            unsafe { ((container + 0x7c0) as *const u32).read_volatile() }
        } else {
            0
        };
        log::info!(
            "session-probe: teardown-handler — 0x1423f46d0(container={container:#x}) [+0x7c0]={status:#x} \
             (async connection-down teardown; if this fires at the ~2s teardown, the solo host has no real peer)",
        );
    }));
}

/// Raw-patch `bytes` over a charted function entry (e.g. `[0xC3]` = `ret`, or `[0xB0,0x01,0xC3]` =
/// `mov al,1; ret`). Flips the page to RWX, writes, restores protection, flushes the icache. Returns the
/// error string on failure. Used by `suppress_leave` to force host-setup's online-availability gate true.
fn patch_bytes(addr: usize, bytes: &[u8]) -> Result<(), String> {
    use windows::Win32::System::Memory::{
        VirtualProtect, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
    };
    // SAFETY: `addr` is a charted, resident function entry in our own process image. We flip its page(s) to
    // RWX, overwrite the leading bytes, restore the old protection, and flush the icache — the standard
    // in-process code patch. The caller supplies a self-contained instruction sequence ending in `ret`.
    unsafe {
        let n = bytes.len();
        let mut old = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(addr as *const _, n, PAGE_EXECUTE_READWRITE, &mut old)
            .map_err(|e| format!("VirtualProtect(RWX) failed: {e}"))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, n);
        let _ = VirtualProtect(addr as *const _, n, old, &mut old);
        let _ = windows::Win32::System::Diagnostics::Debug::FlushInstructionCache(
            windows::Win32::System::Threading::GetCurrentProcess(),
            Some(addr as *const _),
            n,
        );
    }
    Ok(())
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
    // Leaked descriptor. The handler copies dwords [desc+0..0x34] into the builder's local[0x18..], which
    // the socketmgr sub-init then copies into socketmgr[0x58..0xa0] — the config region a ZEROED input
    // clobbers (2026-07-05 finding: the builder's socketmgr init fails for this reason while
    // land_socket_holder's succeeds). PATH A: seed [desc+0..0x48] from the stood-up socketmgr's post-ctor
    // defaults (socketmgr[0x58..0xa0]) so the builder builds a configured socketmgr. Caveat: the handler's
    // input→local copy has GAPS (local[0x30/0x34/0x4c] aren't sourced from input → some defaults stay stack
    // garbage), so this cycle tests empirically whether that's fatal (→ path B) or the build proceeds.
    let desc: &'static mut [u8] = vec![0u8; 0x140].leak();
    let desc_ptr = desc.as_mut_ptr() as usize;
    let standup = STANDUP_CONNECTION.load(Ordering::Relaxed);
    if standup != 0 {
        // SAFETY: `standup` is the published socket-manager wrapper; [+8] is its socketmgr, whose
        // [0x58..0xa0] config region is in-bounds (the object is 0x150 bytes). Read-only copy into our buf.
        let socketmgr = unsafe { ((standup + 8) as *const usize).read_volatile() };
        if socketmgr != 0 {
            unsafe {
                std::ptr::copy_nonoverlapping((socketmgr + 0x58) as *const u8, desc_ptr as *mut u8, 0x48);
            }
            log::info!(
                "session-probe: drive-establish — seeded input desc[0..0x48] from standup socketmgr \
                 {socketmgr:#x}[0x58..0xa0] (path A: preserve config defaults through the builder sub-init copy)",
            );
        }
    }
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
            poll_stall_b_session(false);
            if self.heartbeat.tick() {
                log::info!("session-probe: live, no CSSessionManager yet (frame {})", tick.frame);
            }
            return;
        };

        // Stall-B: watch the captured SessionSteam's state/pending-conn span while a session exists
        // (the SessionSteam dies with the lobby, so a None FSM clears the pointer instead of reading).
        poll_stall_b_session(fsm.lobby != LobbyState::None);

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

// --- Rung-3 JOIN driver (the joiner counterpart to SessionCreateDriver) -------------------------
//
// Drives the join wrapper 0x140cae640(this, flag, a, b, c) so a second machine goes
// None -> TryToJoinSession -> Client and joins the driven host. Charted (docs/SESSION-DRIVE.md):
//   wrapper 0x140cae640 -> inner 0x140cb2470: past the availability gate 0x140cb4b50 (bypassed by
//   bypass_session_create_gate), the inner reads the payload `a` as {begin=[a+0], end=[a+8]} (a byte
//   range), passes [begin,end) to a deserializer vmethod whose result lands at [this+0x28]; nonzero =>
//   lobby_state=4 (TryToJoinSession). Our synthesized host produces no real matchmaker blob, so we feed a
//   minimal blob (the host SteamID64) and — since the parse likely rejects it — the JOIN_FORCE_RESULT
//   hook forces [this+0x28] nonzero at the inner's result check so the FSM still advances (the same
//   "bypass the gate" approach that made the host stick). Fires like the create driver (in-world; holds
//   for the rung-2 link unless drive_fire_solo). Reuses stand_up_transport + land_socket_holder +
//   suppress_leave (the online-availability gate) exactly as the host does.

/// The join wrapper's win64 signature: `this`(rcx), `flag`(dl), `a`(r8), `b`(r9d), `c`(5th, stack).
/// (`JOIN_WRAPPER_OFFSET` is defined near the create-initiation hook — reused here.)
type JoinFn = unsafe extern "system" fn(*mut CSSessionManager, u8, *const JoinBlobDesc, u32, usize) -> bool;

/// The payload descriptor the join inner reads: `{ begin, end }` — a byte range `[begin, end)` (the host
/// join blob). The inner computes `len = end - begin` and hands `[begin, end)` to the deserializer.
#[repr(C)]
struct JoinBlobDesc {
    begin: usize,
    end: usize,
}

/// One-shot join driver. Mirrors [`SessionCreateDriver`]'s fire gating; builds a minimal blob from the
/// host SteamID64 and calls the join wrapper. See the module comment above.
pub struct SessionJoinDriver {
    fired: bool,
    linked_since: Option<u64>,
    fire_solo: bool,
    /// Two-machine host-id override (both machines' SteamID64s; the joiner picks whichever isn't its own).
    peer_override: [u64; 2],
    /// Set the container's session-established bit (`container+0x7c0` bit 2) before the join, so the
    /// join-created session passes the readiness gate `0x1423fd7a0` and builds the emitter connection.
    /// See [`crate::config`] `join_set_established_bit` + docs/SESSION-DRIVE.md > "★ CLIENT-JOIN AIM SHEET".
    set_established_bit: bool,
}

impl SessionJoinDriver {
    fn new(fire_solo: bool, peer_a: u64, peer_b: u64, set_established_bit: bool) -> Self {
        Self {
            fired: false,
            linked_since: None,
            fire_solo,
            peer_override: [peer_a, peer_b],
            set_established_bit,
        }
    }

    /// The host SteamID64 to join: the rung-2-linked partner if present, else the config peer override
    /// that isn't our own (the autonomous two-machine path — the joiner picks the host's id).
    fn host_steam_id(&self) -> Option<u64> {
        if let Some(p) = crate::coop::linked_peer() {
            return Some(p);
        }
        let self_id = crate::steam::self_steam_id();
        self.peer_override.into_iter().find(|&id| id != 0 && Some(id) != self_id)
    }
}

impl Feature for SessionJoinDriver {
    fn name(&self) -> &'static str {
        "session-join-driver"
    }

    fn phase(&self) -> CSTaskGroupIndex {
        CSTaskGroupIndex::FrameBegin
    }

    fn on_frame(&mut self, tick: Tick) {
        if self.fired {
            return;
        }
        if !crate::playstate::current().in_game() {
            return;
        }
        if crate::sdk::with_active_main_player(|_| ()).is_none() {
            return;
        }
        if !self.fire_solo && !crate::coop::is_linked() {
            self.linked_since = None;
            return;
        }
        let linked_since = *self.linked_since.get_or_insert_with(|| {
            let trigger = if self.fire_solo { "solo (drive_fire_solo)" } else { "side-channel linked" };
            log::info!(
                "session-probe: drive-join armed @frame {} — {trigger}; firing after {LINK_SETTLE_FRAMES}-frame settle",
                tick.frame,
            );
            tick.frame
        });
        if tick.frame.saturating_sub(linked_since) < LINK_SETTLE_FRAMES {
            return;
        }
        let Some((base, lobby)) = crate::sdk::with_instance::<CSSessionManager, _>(|s| {
            (s as *const CSSessionManager as usize, crate::session::read(s).lobby_state)
        }) else {
            return;
        };
        if lobby != LobbyState::None {
            log::info!("session-probe: drive-join skipped — lobby_state is {lobby:?}, need None");
            self.fired = true;
            return;
        }
        self.fired = true;

        let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
            Ok(h) => h.0 as usize,
            Err(e) => {
                log::error!("session-probe: drive-join — GetModuleHandle(NULL) failed: {e}");
                return;
            }
        };
        // Build a minimal blob = the host SteamID64 (8 bytes). The parse likely needs more, but the
        // JOIN_FORCE_RESULT hook forces the result gate so the FSM advances regardless (see module comment).
        let host_id = self.host_steam_id().unwrap_or(0);
        let blob: &'static mut [u8] = vec![0u8; 8].leak();
        blob.copy_from_slice(&host_id.to_le_bytes());
        let bp = blob.as_ptr() as usize;
        let desc = JoinBlobDesc { begin: bp, end: bp + 8 };
        let fn_addr = exe_base + JOIN_WRAPPER_OFFSET;
        // SAFETY: `fn_addr` is the join wrapper at the live exe base; called with this=[G] (live singleton),
        // a minimal {begin,end} blob descriptor that outlives the call, and guessed scalar args, on the main
        // thread with lobby_state==None. A fault surfaces via the crashdump SEH handler.
        // Initialize the connection registry the join needs. The join's connection-from-blob 0x1423f62e0
        // bails at its FIRST check (registry-ready 0x141eba210 = `return [registry+0x10]`) because on the
        // joiner the registry (*(G+0x60)+0x710) is fully uninitialized ([+0x10]=0, array/cap/count=0) — the
        // host's create inits it, the join expects it pre-inited. Wire it: a leaked slot array at +0x18, a
        // capacity at +0x20, count 0 at +0x24, and the ready flag +0x10=1. Then 0x1423f62e0 passes the ready
        // check and proceeds to create + register the host connection (next fault, if any, localizes further).
        // SAFETY: `base` is the live CSSessionManager; +0x60 is in-bounds; the registry is a live sub-object.
        unsafe {
            let netman = ((base + 0x60) as *const usize).read_volatile();
            if netman != 0 {
                let reg = netman + 0x710;
                if ((reg + 0x10) as *const u32).read_volatile() == 0 {
                    let slots: &'static mut [usize] = vec![0usize; 16].leak(); // 16 connection slots
                    ((reg + 0x18) as *mut usize).write_volatile(slots.as_mut_ptr() as usize);
                    ((reg + 0x20) as *mut u32).write_volatile(16); // capacity
                    ((reg + 0x24) as *mut u32).write_volatile(0); // count
                    ((reg + 0x10) as *mut u32).write_volatile(1); // ready
                    log::info!(
                        "session-probe: drive-join — initialized empty registry {reg:#x} (ready[+0x10]=1, \
                         array[+0x18]={:#x} cap=16) so 0x1423f62e0 passes its ready check",
                        slots.as_ptr() as usize,
                    );
                }

                // Readiness pre-wire (the CLIENT-JOIN AIM SHEET fix). The join creates a fresh SessionSteam
                // and gates it on readiness 0x1423fd7a0, whose container sub-predicate 0x1423f4330 returns
                // false unless container+0x7c0 bit 2 (the session-established bit) is set. `netman` (=*(G+0x60))
                // IS that container (the registry above is netman+0x710, and create allocs the fresh session
                // from [[registry+8]+0x48] = container+0x48, so [registry+8]==container==netman). A join-only
                // client never set the bit → readiness fails → 0x1423f62e0 destroys the session and bails
                // before the blob-parse ([G+0x28]=0, no emitter).
                //
                // SET IT WITH A DIRECT OR — NOT by calling the session-established handler 0x1423f4870. The
                // 2026-07-05 run PROVED calling that handler passes readiness + builds the emitter + reaches
                // Client/JoinCheck, but it ALSO builds a real +0x708 establish-session connection, and ~30s
                // later the joiner crashed at eldenring.exe+0x3f4860 reading 0x1c5 — the exact null-session
                // signature of the establish-vs-join FSM conflict (config.rs `symmetric_peer` / drive_join
                // notes). The readiness socket-service step doesn't need the handler: `land_socket_holder`
                // already lands a real holder at [container+0x708] on the client (rig-confirmed same run,
                // "socketmgr init returned 1 … wrote [container+0x708]"). So OR-in bit 2 alone — exactly what
                // the container predicate 0x1423f4330 tests, nothing else — and let stand_up_transport +
                // land_socket_holder (already on) provide the socket layer readiness allocs after the bit.
                // (join-aim /ultracheck correction, 2026-07-05.)
                if self.set_established_bit {
                    let field_ptr = (netman + 0x7c0) as *mut usize;
                    let before = field_ptr.read_volatile();
                    field_ptr.write_volatile(before | 0b100);
                    let identity = ((netman + 0x7f8) as *const usize).read_volatile();
                    log::info!(
                        "session-probe: drive-join — session-established bit OR-in on container={netman:#x}: \
                         [+0x7c0] {before:#x} -> {:#x} (bit2); +0x7f8 identity={identity:#x} \
                         (bit2 set => readiness 0x1423fd7a0 container-predicate 0x1423f4330 should pass; \
                         no handler call => no +0x708 establish artifact / +0x3f4860 crash)",
                        before | 0b100,
                    );
                }
            }
        }
        let join: JoinFn = unsafe { std::mem::transmute::<usize, JoinFn>(fn_addr) };
        log::info!(
            "session-probe: drive-join @frame {} — calling join wrapper {fn_addr:#x}(this={base:#x}, flag=0, \
             blob={{{bp:#x}..+8 = host {}}}, b=0, c=0); lobby was None",
            tick.frame,
            unseamless_core::diagnostics::peer_tag(host_id),
        );
        let ret = unsafe { join(base as *mut CSSessionManager, 0, &desc, 0, 0) };
        let after = crate::sdk::with_instance::<CSSessionManager, _>(|s| crate::session::read(s).lobby_state);
        log::info!(
            "session-probe: drive-join returned {ret} — lobby_state now {after:?} \
             (TryToJoinSession/Client=driven OK; FailedToJoinSession=internal gate rejected)",
        );
        // Chart the connection-handle registry the update task polls. The joiner path (0x140caff11) polls
        // [G+0x28]; the registry is *(G+0x60)+0x710 (via 0x1423f1930), and its vtable slot +0x10 is the
        // blob→connection method (create uses +8 → [G+0x24]). Log [G+0x24]/[G+0x28] + the registry vtable +
        // slot +0x10/+8 addresses so we can disassemble the blob parser and build a valid blob (a real
        // handle in [G+0x28] is what advances the joiner to Client).
        // SAFETY: `base` is the live CSSessionManager; +0x24/+0x28/+0x60 are in-bounds; the registry chain
        // is null-guarded before each deref.
        unsafe {
            let h24 = ((base + 0x24) as *const u32).read_volatile();
            let h28 = ((base + 0x28) as *const u32).read_volatile();
            let netman = ((base + 0x60) as *const usize).read_volatile();
            let (reg, reg_vt, slot8, slot10) = if netman != 0 {
                let reg = netman + 0x710;
                let vt = (reg as *const usize).read_volatile();
                let s8 = if vt != 0 { ((vt + 8) as *const usize).read_volatile() } else { 0 };
                let s10 = if vt != 0 { ((vt + 0x10) as *const usize).read_volatile() } else { 0 };
                (reg, vt, s8, s10)
            } else {
                (0, 0, 0, 0)
            };
            // The registry-ready check 0x141eba210 just returns [registry+0x10]; 0x1423f62e0 fails there if
            // it's 0. Also read the connection array [registry+0x18], capacity [+0x20], count [+0x24].
            let (r10, r18, r20, r24) = if reg != 0 {
                (
                    ((reg + 0x10) as *const u32).read_volatile(),
                    ((reg + 0x18) as *const usize).read_volatile(),
                    ((reg + 0x20) as *const u32).read_volatile(),
                    ((reg + 0x24) as *const u32).read_volatile(),
                )
            } else {
                (0, 0, 0, 0)
            };
            log::info!(
                "session-probe: drive-join — handles [G+0x24]={h24:#x} (create/host) [G+0x28]={h28:#x} \
                 (join/client — update task polls THIS); registry {reg:#x} vtable={reg_vt:#x} \
                 create-slot[+8]={slot8:#x} join-slot[+0x10]={slot10:#x} | registry-ready[+0x10]={r10:#x} \
                 (0 => 0x1423f62e0 fails the ready check) array[+0x18]={r18:#x} cap[+0x20]={r20} count[+0x24]={r24}",
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

// NB: the raw resolver `0x142640b90` is NO LONGER called directly — doing so with the holder base stored
// the iface over the SteamInternal `pFn` slot and crashed the socketmgr worker (fault #4). We now resolve
// via `SteamInternal_ContextInit` (the IAT import at `0x144c0d0a4`), the same idempotent path the game uses.
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
    /// This machine is the host (rung-3 create role). The host lets the game's own socket-manager worker
    /// thread service inbound P2P (so the joiner's SYN reaches the admit path `0x142640e30`); the joiner
    /// emits a real DLNW3D SYN to the host. Derived from `auto_session` (see [`rung3_role`]).
    is_host: bool,
    /// Skip our own inbound P2P drain so the game's worker thread receives the packets instead (our drain
    /// otherwise steals every channel-0 datagram before the worker sees it). On when host-accept
    /// instrumentation is armed — the worker-drain / host-admit hooks are the signal then, not our RECV log.
    suppress_drain: bool,
    /// Host-side: drive the session-layer add-peer entry `0x1423fdc80` for the two-machine peer once the
    /// host session is up (`drive_add_peer` config). Off = leave the joiner-member to a natural producer.
    drive_add_peer: bool,
    /// Re-fire throttle for the add-peer drive. NOT a one-shot: an incomplete member (no packets yet) is
    /// dropped by the per-frame pump, so we re-drive on a throttle while the peer is linked — add-peer
    /// dedups (0x1423fbd80) once a member persists, so this is a no-op after the endpoint completes. This
    /// keeps a member in the pending-conn queue for the pump to build the endpoint on once the Deck's real
    /// handshake packets arrive (ERSC capture: the pump 0x1424007e0 builds member+0x130 during the handshake).
    add_peer_throttle: FrameThrottle,
    /// Log the drive only on the first fire + on result changes (avoid spamming the re-fire throttle).
    add_peer_logged: bool,
    /// Symmetric-peer mode: send the DLNW3D SYN even as host role (both peers send, so both worker threads
    /// receive and both pumps build the other's endpoint). See `symmetric_peer` in config.
    symmetric: bool,
    /// Host-side accept-unmask: skip our own `AcceptP2PSessionWithUser` on the host role so the peer's
    /// first packet raises the game's `P2PSessionRequest` event for its registered callbacks
    /// (`0x1423fd550/560/570`) instead of being pre-accepted by us. See `host_skip_p2p_accept` in config.
    skip_accept: bool,
    /// Symmetric add-peer: the JOINER also drives add-peer (queuing the host in its Client session) so its
    /// game emits real SYNs too and the handshake can close both ways. See `drive_add_peer_joiner` in config.
    add_peer_joiner: bool,
}

impl TransportStandupDriver {
    #[allow(clippy::too_many_arguments)]
    fn new(
        peer_a: u64,
        peer_b: u64,
        is_host: bool,
        suppress_drain: bool,
        drive_add_peer: bool,
        symmetric: bool,
        skip_accept: bool,
        add_peer_joiner: bool,
    ) -> Self {
        Self {
            built: false,
            iface: 0,
            accepted: false,
            ping_throttle: FrameThrottle::every(120),
            ping_seq: 0,
            peer_override: [peer_a, peer_b],
            is_host,
            suppress_drain,
            drive_add_peer,
            add_peer_throttle: FrameThrottle::every(60),
            add_peer_logged: false,
            symmetric,
            skip_accept,
            add_peer_joiner,
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
/// The channel the game's socket-manager worker thread actually reads (`ReadP2PPacket(nChannel=
/// [socketmgr+0x50])` in `0x142640bc0`). Rig-observed live = **30** (the worker-drain probe logs it). A
/// joiner SYN must land on THIS channel or the host worker never sees it. Re-derive after a game update
/// from the `host-worker-drain` log line (`instrument_host_accept`).
const GAME_WORKER_CHANNEL: i32 = 30;

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
            // 1. Resolve ISteamNetworking006 into its holder — the CORRECT way, via SteamInternal_ContextInit.
            // The holder 0x143c602b0 is a SteamInternal context-init struct `{ pFn@+0, resolved-iface@+8 }`
            // where pFn = the resolver 0x142640b90. Calling that resolver DIRECTLY with the holder base (what
            // we used to do) stores the iface at [holder+0], OVERWRITING pFn — and the socket-manager's worker
            // thread later calls SteamInternal_ContextInit(holder), reads the corrupted pFn (now the iface),
            // and calls the iface as a function → crash (host-setup fault #4, 2026-07-04 pm). ContextInit is
            // the idempotent API the game itself uses: it invokes pFn(ctx-slot) once (guarded) so the iface
            // lands in the ctx slot without clobbering pFn. Call it through its IAT import [0x144c0d0a4].
            let holder = exe_base + ISTEAM_HOLDER_OFFSET;
            let before = (holder as *const usize).read_volatile();
            let ctxinit_ptr = ((exe_base + 0x4c0d0a4) as *const usize).read_unaligned();
            log::info!(
                "session-probe: transport-standup — resolving ISteamNetworking006 via SteamInternal_ContextInit \
                 {ctxinit_ptr:#x} (holder {holder:#x}, before[+0]={before:#x})",
            );
            let ctxinit: extern "win64" fn(usize) -> usize = std::mem::transmute(ctxinit_ptr);
            let ret = ctxinit(holder);
            let h0 = (holder as *const usize).read_volatile();
            let h8 = ((holder + 8) as *const usize).read_volatile();
            // Layout confirmed live: [holder+0]=pFn (stays 0x142640b90), [holder+8]=counter, and pFn stores
            // the iface at pCtx = ret = holder+0x10, so the resolved iface is *[holder+0x10] (= *ret).
            let iface = if ret != 0 { (ret as *const usize).read_volatile() } else { 0 };
            resolved_iface = iface; // hand to phase 2 (drive_p2p) even if a later build step bails
            log::info!(
                "session-probe: transport-standup — ContextInit ret={ret:#x} holder[+0]={h0:#x} (should stay \
                 pFn 0x142640b90) holder[+8]={h8:#x} => ISteamNetworking006 = {iface:#x} ({})",
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
        // Accept-unmask: the host role leaves the inbound session un-accepted so the game's own
        // P2PSessionRequest callback dispatch (the registered 0x1423fd5xx callbacks) sees the request.
        let unmask = self.skip_accept && self.is_host;
        if unmask && !self.accepted {
            self.accepted = true; // never accept on this machine
            log::info!(
                "session-probe: game-p2p — host accept-unmask ON: NOT calling AcceptP2PSessionWithUser; \
                 the peer's first packet should raise P2PSessionRequest for the game's registered \
                 callbacks (watch p2p-evt-*)"
            );
        }
        let do_accept = !self.accepted;
        // Under unmask the host must be FULLY silent on legacy P2P: run 3 (2026-07-05) showed that
        // skipping Accept alone is defeated by our own outbound pings — SendP2PPacket implicitly opens
        // the P2P session, so the peer's packets flow and P2PSessionRequest is never raised.
        let do_ping = self.ping_throttle.tick() && !unmask;
        let seq = self.ping_seq.wrapping_add(1);
        let suppress_drain = self.suppress_drain;
        let is_host = self.is_host;
        let symmetric = self.symmetric;
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
            // SKIPPED when `suppress_drain` (host-accept instrumentation): our drain otherwise steals
            // every channel-0 datagram before the game's own socket-manager worker thread can read it, so
            // the joiner's SYN never reaches the admit path. With it suppressed, the worker-drain
            // (0x142640bc0) + host-admit (0x142640e30) hooks are the signal instead of our RECV log.
            if !suppress_drain {
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
            }
            // Joiner one-shot: send a real 14-byte DLNW3D SYN to the host so the host's admit path
            // (0x142640e30 gate 0x142642830) can admit us — it accepts ONLY a 14-byte packet whose header
            // encodes control length 14: byte[0]=0x0e (len low), byte[1] has bit 0x40 set with low-3-bits=0
            // (len high). The remaining 12 bytes are the connection-request payload (host doesn't gate on
            // them at admit; a full handshake would). Charted 2026-07-04, docs/SESSION-DRIVE.md >
            // "HOST-SIDE ADMIT/ROSTER". One-shot: we're probing whether the admit path fires at all offline.
            // Repeat on the ping throttle (a real connect retries its SYN until the peer answers), not
            // one-shot: the first reaches the host's admit path 0x142640e30; if the host creates a
            // connection, later ones match it and feed it (0x142643db0) instead. Send on channel 30 (the
            // channel the host worker drains), never our probe channel 0.
            // In symmetric-peer mode BOTH peers send the SYN (each worker receives, each pump builds the
            // other's endpoint); otherwise only the non-host (joiner) sends.
            if (!is_host || symmetric) && do_ping {
                let mut syn = [0u8; 14];
                syn[0] = 0x0e;
                syn[1] = 0x40;
                let ok = send(iface, peer, syn.as_ptr(), syn.len() as u32, P2P_SEND_RELIABLE, GAME_WORKER_CHANNEL);
                log::info!(
                    "session-probe: game-p2p — sent real 14B DLNW3D SYN [0x0e,0x40,..] to peer {} on channel {GAME_WORKER_CHANNEL} = {ok} \
                     (trips the peer's host-admit 0x142640e30 + feeds its pump — the channel the worker drains)",
                    unseamless_core::diagnostics::peer_tag(peer),
                );
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
        // Host-side joiner-member drive: keep a member in the pending-conn queue for the peer while it's
        // linked, so the game's per-frame pump can build the member's endpoint (member+0x130) once the
        // Deck's real handshake packets arrive (ERSC capture: the endpoint is built by the pump during the
        // handshake, not by a separate driver). Re-fired on a throttle (an incomplete member with no packets
        // yet is dropped by the pump); add-peer dedups once a member persists, so it no-ops after completion.
        // Host always (drive_add_peer); joiner too when add_peer_joiner (symmetric — so both games emit
        // real SYNs and the handshake closes both ways). try_drive_add_peer picks the right session +
        // lobby-state gate per role.
        let drive = (self.is_host && self.drive_add_peer) || (!self.is_host && self.add_peer_joiner);
        if drive && self.add_peer_throttle.tick() {
            self.try_drive_add_peer(peer);
        }
    }

    /// Drive the session-layer add-peer entry `0x1423fdc80(session, &peerSteamID, &selfSteamID, flag)` for the
    /// two-machine peer. Host queues the joiner in its Host session; with `add_peer_joiner` the joiner queues
    /// the host in its Client session (symmetric — so both games emit real SYNs). This pops an empty member
    /// from the session's pool, sets `member+0x80` =
    /// the peer SteamID64, initialises it, and enqueues it on the session's pending-conn queue — the exact
    /// thing the game's own event-drain does for a natural join, which never fires for our driven Deck. The
    /// game's running per-frame session update then pumps the new member's handshake. One-shot; logs the member
    /// pool + pending-conn queue before and after so a new populated slot is visible. Firewalled; a hard fault
    /// surfaces via crashdump (we're calling a game function on the main thread with a live session).
    fn try_drive_add_peer(&mut self, peer: u64) {
        let add_peer = ADD_PEER_FN.load(Ordering::Relaxed);
        // Host: LIVE_SESSION (captured at the establish add-member hook). Joiner: the establish hook
        // never fires (no establish handler on the client), so use the poll-captured STALLB_SESSION.
        let session = if self.is_host {
            LIVE_SESSION.load(Ordering::Relaxed)
        } else {
            STALLB_SESSION.load(Ordering::Relaxed)
        };
        if session == 0 || add_peer == 0 || peer == 0 {
            return; // session not built yet, or no peer — try again next frame
        }
        // Only drive once the session is stably in its role state (host = Host, joiner = Client). Firing
        // during TryToCreate/TryToJoin disrupts the transition (rig-observed 2026-07-05: an add-peer
        // mid-create tore the session down ~30s later). Wait for the settled role.
        let want = if self.is_host { LobbyState::Host } else { LobbyState::Client };
        let lobby = crate::sdk::with_instance::<CSSessionManager, _>(|s| crate::session::read(s).lobby_state);
        if lobby != Some(want) {
            return; // not settled in-role yet — try again next frame (still latched until it fires)
        }
        let self_id = crate::steam::self_steam_id().unwrap_or(0);
        if self_id == 0 {
            return;
        }
        // If the peer is already a member (its endpoint completed and it persists), don't churn: scan the
        // session's member registry for a +0x80 == peer. Cheap 6-slot walk. (add-peer also dedups internally,
        // but skipping the call entirely avoids re-popping/re-dropping the pool while the handshake is mid-flight.)
        let log_this = !self.add_peer_logged;
        self.add_peer_logged = true;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            // Read the member pool head (+0x538) and the pending-conn queue (+0x4f0/+0x4f8) so the before/after
            // shows a slot consumed + a conn enqueued. All in-bounds session fields.
            let rq = |a: usize| (a as *const usize).read_volatile();
            let (pool_before, pend_b0, pend_b1) =
                (rq(session + 0x538), rq(session + 0x4f0), rq(session + 0x4f8));
            // peerInfo = &{peer SteamID64}; key = &{self SteamID64}. 0x1423fdc80 reads [peerInfo] as the
            // member identity (→ member+0x80) and [key] for the is-self compare (peer != self ⇒ remote).
            // Both are single-qword structs on our stack; the callee only reads [+0]. flag=1 (announce).
            let peer_info: u64 = peer;
            let key: u64 = self_id;
            let add: extern "win64" fn(usize, *const u64, *const u64, u8) -> u8 =
                std::mem::transmute(add_peer);
            let ret = add(session, &peer_info, &key, 1);
            let (pend_a0, pend_a1) = (rq(session + 0x4f0), rq(session + 0x4f8));
            // Log the first fire, plus any time the pending queue grew (a fresh member was actually enqueued
            // this call — vs a dedup no-op once the member persists). Keeps the re-fire throttle quiet.
            if log_this || pend_a1 != pend_b1 {
                log::info!(
                    "session-probe: drive-add-peer — 0x1423fdc80(peer={}, self={}) returned {ret} \
                     [pool_head {pool_before:#x} pending_queue {pend_b0:#x}..{pend_b1:#x} → {pend_a0:#x}..{pend_a1:#x}] \
                     (queue grew ⇒ member enqueued for the peer; the per-frame pump builds its endpoint once the peer's packets arrive)",
                    unseamless_core::diagnostics::peer_tag(peer),
                    unseamless_core::diagnostics::peer_tag(self_id),
                );
            }
        }));
    }
}

/// The session probe's gated frame features, for [`crate::app::build_features`] to `extend` with —
/// mirroring [`crate::diag::probe_features`] so every `[debug.probes]`-gated feature is appended the
/// same way (one assembly style, gating kept inside this module). The FSM-transition logger when
/// `session_probe` is on; the experimental [`SessionCreateDriver`] when `drive_create` is on; the
/// [`TransportStandupDriver`] (path C) when `stand_up_transport` is on.
pub fn probe_features(config: &Config) -> Vec<Box<dyn Feature>> {
    let mut features: Vec<Box<dyn Feature>> = Vec::new();
    let (do_create, do_join) = rung3_role(config);
    if config.debug.probes.stand_up_transport {
        features.push(Box::new(TransportStandupDriver::new(
            config.debug.probes.p2p_test_peer_a,
            config.debug.probes.p2p_test_peer_b,
            do_create, // host role = the create driver's machine
            config.debug.probes.instrument_host_accept,
            config.debug.probes.drive_add_peer,
            config.debug.probes.symmetric_peer,
            config.debug.probes.host_skip_p2p_accept,
            config.debug.probes.drive_add_peer_joiner,
        )));
    }
    if config.debug.probes.session_probe {
        features.push(Box::new(SessionFsmProbe::new()));
    }
    if do_create {
        features.push(Box::new(SessionCreateDriver::new(
            config.debug.probes.force_netsession_ready,
            config.debug.probes.drive_fire_solo,
            config.debug.probes.force_host_transition,
        )));
    }
    if do_join {
        features.push(Box::new(SessionJoinDriver::new(
            config.debug.probes.drive_fire_solo,
            config.debug.probes.p2p_test_peer_a,
            config.debug.probes.p2p_test_peer_b,
            config.debug.probes.join_set_established_bit,
        )));
    }
    features
}
