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
    if !config.debug.probes.session_probe {
        return; // probe disabled — fully inert, the common case
    }
    install_one("create-session", &SESSION_CREATE_SITE);
    install_one("join-session", &SESSION_JOIN_SITE);
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

/// The session probe's gated frame features, for [`crate::app::build_features`] to `extend` with —
/// mirroring [`crate::diag::probe_features`] so every `[debug.probes]`-gated feature is appended the
/// same way (one assembly style, gating kept inside this module). Empty unless `[debug.probes]
/// session_probe` is on; currently just the FSM-transition logger.
pub fn probe_features(config: &Config) -> Vec<Box<dyn Feature>> {
    if config.debug.probes.session_probe {
        vec![Box::new(SessionFsmProbe::new())]
    } else {
        Vec::new()
    }
}
