//! **Stay connected**: suppress every *game-driven* co-op disconnect (boss defeat, area
//! transition, player death, host-grace timeout, remote leave/disband packet) behind one armed
//! flag — the core "seamless" behavior, per `docs/SESSION-LIFECYCLE-FINDINGS.md`.
//!
//! ## The chokepoint (charted, static — see the findings doc for the full map)
//! All game-driven session leaves converge on one primitive: `leave_session = 0x140cae730`, the
//! sole out-of-line writer of `lobby_state = OnLeaveSession(7)` (24 external callers + the session
//! update task), plus exactly one **inlined twin** of its body inside the CSSessionManager update
//! task (`update_step = 0x140cafd10`; inline entry `0x140cb0840`, its `lobby_state = 7` write at
//! `0x140cb08bc`, join point `0x140cb08f7`). Gating those two sites suppresses every game-driven
//! leave at once; a **genuine peer drop / network loss** tears down through the async transport
//! handler `0x1423f46d0`, which never runs either site — deliberately left alone (hard boundary:
//! never gate that teardown handler; by the time it runs, teardown is committed and its cleanup is
//! load-bearing).
//!
//! ## Mechanism
//! Two ilhook detours installed once at boot (init thread, sites necessarily idle pre-title) when
//! `gameplay.stay_connected` is on, both consulting a runtime [`ARMED`] flag the
//! [`StayConnectedTick`] feature holds to the live config each frame — so the menu toggle
//! arms/disarms live, but installing at all needs the flag on at boot (the feature warns when the
//! toggle is flipped on with no hooks live):
//! - **Site A** (`leave_session` entry): a `Retn` function hook. Disarmed → forward to the
//!   relocated original; armed → return without running it, mirroring the function's own
//!   guard-path "return, do nothing" exits *ahead of* the deferred-leave latch (`[this+0x20]`), so
//!   a suppressed leave is dropped, never queued (the findings doc's "gate ahead of step 3").
//! - **Site B** (the inline twin's entry `0x140cb0840`): a `JmpToRet` hook. Disarmed → jump to the
//!   relocated original bytes; armed → clear the deferred-leave byte (the reason this path runs at
//!   all — it is the deferred-leave executor, entered only via the `[this+0x20] != 0` gate at
//!   `0x140cb0835`) and jump to the join point `0x140cb08f7`, exactly where the inline's own guard
//!   skips land.
//!
//! Both sites are byte-verified against their charted bytes before hooking, so a game update (or
//! the `session_probe` leave tracer already owning site A on an RE run) degrades to vanilla with a
//! plain log instead of patching drifted code. Suppression events only bump atomics here; the
//! feature aggregates them through the host-tested
//! [`unseamless_core::stay_connected::SuppressAnnouncer`] into a milestone log line + an ER-voiced
//! toast (a polled leave source can re-fire every frame while armed, so per-fire logging stays
//! `debug!` per the logging rule).
//!
//! ## Rig-validation status (why the config default is off)
//! The gate mechanism is charted static-RE; the findings doc's risks are the rig pass: no third
//! `lobby_state = 7` writer (risk #1), boss/area/death actually route here (risk #2), and the
//! sibling effects of a suppressed leave (queued map reload, death fade) stay playable (risk #3).
//! Risk #4 is by design: a real peer drop still tears down. Out of scope (future work): the
//! additive re-sync layer that keeps both worlds coherent across an area transition once the
//! disconnect no longer happens.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_to_ret, hook_closure_retn};
use unseamless_core::stay_connected::SuppressAnnouncer;
use unseamless_core::util::Latch;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::core::PCSTR;

use crate::feature::{Feature, Tick};

/// The exe's preferred image base; the charted VAs below are documented relative to it and
/// resolved against the live `GetModuleHandle(NULL)` base (same scheme as `session_probe`).
const PREFERRED_BASE: usize = 0x1_4000_0000;

/// `leave_session 0x140cae730` — the whole-session leave primitive (site A). Re-derive after a game
/// update per docs/SESSION-LIFECYCLE-FINDINGS.md > "Re-derivation recipes": scan the CSSessionManager
/// method block for the `mov dword [reg+0xc], 7` immediate stores; A is the writer whose function
/// opens with the `[G]`-null log guard then the states-{0,2,5} skip mask and a `[this+0x2c]`
/// idempotency check.
const LEAVE_SESSION_OFFSET: usize = 0x1_40ca_e730 - PREFERRED_BASE;
/// The inline twin's entry inside `update_step` (site B): first instruction after the
/// `cmp byte [r14+0x20], 0; je <join>` deferred-leave gate at `0x140cb0835`. The *other* `imm=7`
/// store (`0x140cb08bc`) lives in this inline body. Re-derive: the second `mov [reg+0xc], 7` site,
/// inside the 0-direct-caller update task; the inline entry is the `mov eax, [r14+0xc]` right after
/// the `[r14+0x20]` gate.
const LEAVE_INLINE_ENTRY_OFFSET: usize = 0x1_40cb_0840 - PREFERRED_BASE;
/// The inline body's join point (`0x140cb08f7`) — where all six of its internal skip/finish branches
/// land (verified by scanning `update_step` for branch targets: nothing jumps to the entry
/// `0x140cb0840` itself, and every exit of the inline region targets this address). The armed site-B
/// hook jumps here, taking exactly the path the inline's own guards take.
const LEAVE_INLINE_JOIN_OFFSET: usize = 0x1_40cb_08f7 - PREFERRED_BASE;

/// Charted entry bytes of `leave_session` (site A), read off the pinned 2026-06-02 exe
/// (`scripts/re/static.py fn 0x140cae730`): `push rbx; sub rsp,0x20; mov rax,[rip+0x30cbd93]
/// (= the CSSessionManager keystone G 0x143d7a4d0); mov rbx,rcx`. 15 bytes — past ilhook's 14-byte
/// stolen window, so the verified bytes cover everything the hook relocates. The rip disp32
/// (`93 BD 0C 03`) is layout-specific, which is what makes this a sharp drift detector.
const LEAVE_SESSION_PROLOGUE: [u8; 15] =
    [0x53, 0x48, 0x83, 0xEC, 0x20, 0x48, 0x8B, 0x05, 0x93, 0xBD, 0x0C, 0x03, 0x48, 0x8B, 0xD9];
/// Charted entry bytes of the inline twin (site B), read off the pinned exe (windowed disasm at
/// `0x140cb0840`): `mov eax,[r14+0xc]; cmp eax,1; je +0xaa (-> 0x140cb08f7); cmp eax,4`. 16 bytes,
/// covering the full stolen window; the concrete `je` rel32 (`AA 00 00 00`) pins the join-point
/// distance, so a drifted/reshaped inline fails the check rather than hooking mid-expression.
const LEAVE_INLINE_PROLOGUE: [u8; 16] = [
    0x41, 0x8B, 0x46, 0x0C, 0x83, 0xF8, 0x01, 0x0F, 0x84, 0xAA, 0x00, 0x00, 0x00, 0x83, 0xF8, 0x04,
];

/// CSSessionManager's deferred-leave byte (`[this+0x20]`): latched 1 by `leave_session`'s
/// mid-handshake path, consumed by the inline twin (site B is entered only while it's set). The
/// armed site-B hook writes it back to 0 so the suppressed leave is dropped rather than re-polled
/// every frame / replayed on disarm.
const DEFERRED_LEAVE_BYTE_OFF: usize = 0x20;

/// Live armed state: the feature holds this to `gameplay.stay_connected` (live config) each frame;
/// the hooks read it per fire. False until the feature's first tick, so the boot flow (no session
/// exists yet anyway) is vanilla.
static ARMED: AtomicBool = AtomicBool::new(false);
/// One-shot pass-through: lets the next `leave_session` call run vanilla even while armed. For a
/// future *mod-driven* leave (the overlay's Leave action, once it drives the game FSM) — nothing
/// sets it yet; `coop::leave()` today only tears down the side-channel.
static PASS_ONE: AtomicBool = AtomicBool::new(false);
/// Bitmask of successfully installed hook sites ([`SITE_PRIMITIVE`] | [`SITE_INLINE`]). Zero when
/// the boot flag was off or both prologue checks refused; the feature reads it to warn on a
/// toggled-on-but-uninstalled state.
static HOOKS_LIVE: AtomicU8 = AtomicU8::new(0);
/// Running count of suppressed leaves (both sites); drained by the feature's announcer.
static SUPPRESSED_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Which site last suppressed ([`SITE_PRIMITIVE`] / [`SITE_INLINE`]), for the milestone line.
static LAST_SITE: AtomicU8 = AtomicU8::new(0);
/// Site A's last suppressed caller return address (rig legibility: which event path fired).
static LAST_CALLER: AtomicUsize = AtomicUsize::new(0);
/// Resolved live VA of the inline join point (set before the site-B hook installs; the armed hook
/// returns it as the jump destination).
static INLINE_JOIN_VA: AtomicUsize = AtomicUsize::new(0);

/// Site bit/tag for the `leave_session` function hook (A).
const SITE_PRIMITIVE: u8 = 1;
/// Site bit/tag for the update-task inline-twin hook (B).
const SITE_INLINE: u8 = 2;

/// Install the two leave-gate hooks. Call once, on the init thread, from `app::pre_task_startup`
/// (same contract as `saves::install` / `session_probe::install_hooks`); the sites are event-driven
/// session code, necessarily idle at boot-time install, which is what makes ilhook's unsuspended
/// entry rewrite safe here. No-op when `gameplay.stay_connected` is off at boot — a later menu
/// toggle then arms nothing and the feature says so. Best-effort per site: a failed/refused site
/// degrades to vanilla behavior for the paths through it, loudly logged, never aborts.
///
/// Ordered *after* `session_probe::install_hooks` on purpose: an RE run's read-only leave tracer
/// hooks site A first, our prologue check then reads its `jmp` and refuses cleanly — teardown
/// charting runs want leaves observable, not suppressed.
pub fn install(config: &unseamless_core::config::Config) {
    if !config.gameplay.stay_connected {
        return;
    }
    let exe_base = match unsafe { GetModuleHandleA(PCSTR::null()) } {
        Ok(h) => h.0 as usize,
        Err(e) => {
            log::error!("stay-connected: GetModuleHandle(NULL) failed: {e}; suppression unavailable");
            return;
        }
    };
    // Resolve the site-B jump target before either hook installs (and so before either can fire).
    // Both the store here and the `HOOKS_LIVE` publishes below are Relaxed: correctness rests on this
    // store preceding the site-B install in program order (not on atomic ordering), and on the fact
    // that a session — the only state that reaches these sites — is entered long after boot with
    // ample intervening synchronization. The read side is null-guarded (`gate_leave_inline` falls
    // back to vanilla on a `0`), so even an unpublished value degrades safely, not to UB.
    INLINE_JOIN_VA.store(exe_base + LEAVE_INLINE_JOIN_OFFSET, Ordering::Relaxed);

    // SAFETY (both sites): each `site_*` is the charted, byte-verified entry (site A a function
    // start, required by `Retn`; site B a mid-function entry whose verified 16 bytes cover the whole
    // stolen window — nothing branches into it except the fall-through gate above it, so relocation
    // can't be jumped past). The callbacks are whole-body panic-firewalled (see `gate_*`); site B's
    // one byte write (`[r14+0x20] = 0`) mirrors the vanilla path's own write at `0x140cb0856`.
    let site_a = exe_base + LEAVE_SESSION_OFFSET;
    if charted_bytes_ok("leave_session (site A)", site_a, &LEAVE_SESSION_PROLOGUE) {
        let hook = unsafe {
            hook_closure_retn(site_a, gate_leave_session, CallbackOption::None, HookFlags::empty())
        };
        finish_install("leave_session (site A)", SITE_PRIMITIVE, site_a, hook.map(|h| Box::new(h) as _));
    }

    let site_b = exe_base + LEAVE_INLINE_ENTRY_OFFSET;
    if charted_bytes_ok("update-task inline twin (site B)", site_b, &LEAVE_INLINE_PROLOGUE) {
        let hook = unsafe {
            hook_closure_jmp_to_ret(site_b, gate_leave_inline, CallbackOption::None, HookFlags::empty())
        };
        finish_install("update-task inline twin (site B)", SITE_INLINE, site_b, hook.map(|h| Box::new(h) as _));
    }
}

/// Shared post-install bookkeeping for one hook site: on success, `mem::forget` the handle (resident
/// for the process lifetime, like every hook here — never unhook a live code path), mark the site's
/// bit in [`HOOKS_LIVE`], and log; on failure, log and leave the bit clear so the feature can detect
/// the partial/absent install. Takes the handle boxed as `Any` so both hook-constructor return types
/// share one path — we only ever forget it, so the concrete type is irrelevant.
fn finish_install(
    name: &str,
    site_bit: u8,
    addr: usize,
    hook: Result<Box<dyn std::any::Any>, ilhook::HookError>,
) {
    match hook {
        Ok(h) => {
            std::mem::forget(h);
            HOOKS_LIVE.fetch_or(site_bit, Ordering::Relaxed);
            log::info!("stay-connected: leave gate hooked at {name} {addr:#x}");
        }
        Err(e) => log::error!("stay-connected: hooking {name} failed: {e:?}"),
    }
}

/// True iff the charted bytes are present at `addr` — the anti-drift/anti-double-hook guard (same
/// role as `session_probe`'s prologue checks). Refusal is loud and names both likely causes.
fn charted_bytes_ok(name: &str, addr: usize, expected: &[u8]) -> bool {
    // SAFETY: `addr..addr+expected.len()` = live exe base + a charted `.text` offset — inside the
    // mapped, readable image. Read-only, byte-at-a-time.
    let seen: Vec<u8> =
        (0..expected.len()).map(|i| unsafe { ((addr + i) as *const u8).read_volatile() }).collect();
    if seen != expected {
        log::warn!(
            "stay-connected: {name} at {addr:#x} reads {seen:02x?}, expected {expected:02x?} — \
             either the offset drifted (game update? re-chart per docs/SESSION-LIFECYCLE-FINDINGS.md) \
             or another hook owns the site (session_probe leave tracer on an RE run). \
             Suppression at this site disabled; vanilla disconnects apply."
        );
        return false;
    }
    true
}

/// Let the next `leave_session` call through even while armed — for a future mod-driven leave (the
/// player choosing to end the session must not be suppressed by our own gate). One-shot; consumed
/// by whichever gated site fires next.
#[allow(dead_code)] // no mod-driven game-FSM leave exists yet; wired for when coop::leave grows one
pub fn allow_next_leave() {
    PASS_ONE.store(true, Ordering::Relaxed);
}

/// Shared armed check for both gates. Loads [`ARMED`] first and only consumes the one-shot
/// [`PASS_ONE`] pass-through on an otherwise-suppressing decision — so a *disarmed* gate fire never
/// burns a pass-through the player armed for a later intentional leave (the TOCTOU an unconditional
/// `swap`-then-load would have). Returns whether this fire should be suppressed.
fn suppress_now() -> bool {
    if !ARMED.load(Ordering::Relaxed) {
        return false; // disarmed: vanilla, and leave PASS_ONE untouched for the next armed leave
    }
    // Armed: honor a pending one-shot pass-through (consume it), else suppress.
    !PASS_ONE.swap(false, Ordering::Relaxed)
}

/// Count one suppressed leave and note where. Called from inside the (whole-body firewalled) hook
/// callbacks, so this stays panic-free atomics + a `debug!`. `caller` is meaningful only for site A
/// (a `leave_session` return address); site B has no single caller, so [`LAST_CALLER`] is cleared to
/// `0` on a site-B fire rather than left showing a stale site-A address. Per-fire logging is `debug!`
/// per the logging rule (the update task can re-decide a polled leave every frame while armed); the
/// aggregated milestone line comes from the feature.
fn record_suppression(site: u8, caller: usize) {
    SUPPRESSED_TOTAL.fetch_add(1, Ordering::Relaxed);
    LAST_SITE.store(site, Ordering::Relaxed);
    // Site A stores its caller; site B clears it (0) so the milestone line never misattributes a
    // stale site-A return address to a site-B event.
    LAST_CALLER.store(if site == SITE_PRIMITIVE { caller } else { 0 }, Ordering::Relaxed);
    log::debug!(
        "stay-connected: suppressed leave (site {}) caller={caller:#x} (caller-ImageBase = \
         caller-0x140000000)",
        if site == SITE_PRIMITIVE { "A: leave_session" } else { "B: update-task inline" },
    );
}

/// Site-A gate body (`Retn` hook on `leave_session`). Armed → count + return 0 without running the
/// original, exactly like its own guard-path exits (which return with no meaningful eax — callers
/// treat it as void) and *before* the deferred-leave latch can be written. Disarmed → forward to
/// the relocated original with the entry registers.
///
/// FFI firewall: this runs from ilhook's un-firewalled `extern "win64"` shim
/// (docs/FFI-UNWIND-AUDIT.md), so a panic must never unwind across it. The whole *decision* (which
/// includes the only panicky call, `record_suppression`'s `debug!`) is wrapped in `catch_unwind`;
/// on a panic we default to **vanilla** (don't suppress). The forward-call to the game trampoline is
/// deliberately outside the firewall — it is foreign code we must not catch across.
fn gate_leave_session(regs: *mut Registers, ori: usize) -> usize {
    // SAFETY: ilhook hands us the saved entry registers; rcx/rdx/r8/r9 are forwarded by value and
    // [rsp] is the return address the caller's `call` pushed. All reads are bounded / null-guarded.
    let r = unsafe { &*regs };
    let suppress = catch_unwind(AssertUnwindSafe(|| {
        if !suppress_now() {
            return false;
        }
        let caller = if r.rsp != 0 { unsafe { (r.rsp as *const usize).read_volatile() } } else { 0 };
        record_suppression(SITE_PRIMITIVE, caller);
        true
    }))
    .unwrap_or(false); // a panic degrades to vanilla, never unwinds across the boundary
    if suppress {
        return 0;
    }
    // Forward to the relocated original. `ori` is ilhook's trampoline for the hooked function.
    // The transmuted signature is valid ONLY because `leave_session`'s charted prologue shows a
    // single `this` argument (in rcx): the extra register args are inert under win64 and there are
    // no stack/xmm args. A re-chart that finds a 5th (stack) or float arg must re-verify this before
    // widening the transmute.
    // SAFETY: `ori` is a valid code pointer to leave_session's relocated prologue; usize↔fn-ptr are
    // the same width on this 64-bit target and `extern "win64"` is the game's ABI.
    let ori_fn: extern "win64" fn(u64, u64, u64, u64) -> u64 = unsafe { std::mem::transmute(ori) };
    ori_fn(r.rcx, r.rdx, r.r8, r.r9) as usize
}

/// Site-B gate body (`JmpToRet` hook on the inline twin's entry). Armed → clear the deferred-leave
/// latch (this path only runs while `[this+0x20] != 0`; leaving it set would re-fire every frame
/// and replay the leave on disarm) and jump to the inline's own join point `0x140cb08f7` (which
/// opens with a `call` that clobbers rax + rflags, so the entry-state ilhook's `JmpToRet` epilog
/// restores is irrelevant there; r14 is restored by that epilog and the join reads `[r14+…]`).
/// Disarmed → jump into the relocated original bytes.
///
/// FFI firewall: same boundary as site A. The whole armed branch (the byte write +
/// `record_suppression`) is wrapped; on a panic we default to **vanilla** (jump to `ori`).
fn gate_leave_inline(regs: *mut Registers, ori: usize) -> usize {
    let join = catch_unwind(AssertUnwindSafe(|| {
        if !suppress_now() {
            return None; // disarmed: vanilla — fall into the relocated original
        }
        // SAFETY: r14 is `update_step`'s `this` (the live CSSessionManager) at this site; the
        // one-byte write mirrors the vanilla path's own `mov byte [r14+0x20], 0` at 0x140cb0856.
        let r = unsafe { &*regs };
        if r.r14 != 0 {
            unsafe { ((r.r14 as usize + DEFERRED_LEAVE_BYTE_OFF) as *mut u8).write_volatile(0) };
        }
        record_suppression(SITE_INLINE, 0);
        Some(INLINE_JOIN_VA.load(Ordering::Relaxed))
    }))
    .unwrap_or(None); // a panic degrades to vanilla, never unwinds across the boundary
    // Suppress → the resolved join point; disarmed or a null/unresolved target → vanilla (`ori`).
    // Never hand ilhook a null jump destination.
    match join {
        Some(j) if j != 0 => j,
        _ => ori,
    }
}

/// Both hook sites installed — the state in which suppression is *complete* (both `leave_session`
/// and its inline twin gated). A partial install (one bit) still leaves one game-driven leave route
/// open, so the feature refuses to arm on it.
const BOTH_SITES: u8 = SITE_PRIMITIVE | SITE_INLINE;

/// Per-frame binding: holds [`ARMED`] to the live config (so the menu toggle / a future ConfigSync
/// arms live), warns once when the toggle is on without *both* hooks installed, and aggregates the
/// hooks' suppression counter into the milestone log + ER-voiced toast via the host-tested announcer.
pub struct StayConnectedTick {
    /// Classifies the effective armed state (First/Changed/Reasserted) for the announce policy.
    latch: Latch<bool>,
    announcer: SuppressAnnouncer,
    /// One-shot for the "enabled but not fully installed" warning (plain voice, diagnostic).
    warned_incomplete: bool,
}

impl StayConnectedTick {
    pub fn new() -> Self {
        Self { latch: Latch::new(), announcer: SuppressAnnouncer::new(), warned_incomplete: false }
    }

    /// One-shot diagnostic when the config asks for suppression but not *both* hook sites are live —
    /// zero installed (off at boot / both drifted) or a partial install (one site drifted, or the
    /// `session_probe` leave tracer owns site A on an RE run). Either way suppression can't be
    /// complete this session, so say so plainly (diagnostic voice) and once.
    fn warn_if_incomplete(&mut self, desired: bool, hooks: u8) {
        if !desired || hooks == BOTH_SITES || self.warned_incomplete {
            return;
        }
        self.warned_incomplete = true;
        log::warn!(
            "stay-connected: enabled in config but leave-gate hooks are incomplete \
             (installed={hooks:#04b}, need {BOTH_SITES:#04b}) — off at boot, drifted bytes, or an RE \
             tracer owns a site. Not arming; relaunch to install. Vanilla disconnects apply."
        );
        crate::notify::with_mut(|n| {
            n.warn("Stay connected can't arm this session (needs a relaunch to install)".to_string())
        });
    }

    /// Announce arm/disarm transitions (info on First/Changed, toast only on Changed), but only once
    /// both hooks exist — a flag-off or not-fully-installed boot stays silent.
    fn announce_arm_state(&mut self, effective: bool, hooks: u8) {
        if hooks != BOTH_SITES {
            return;
        }
        crate::features::announce_held(
            &mut self.latch,
            effective,
            || format!("stay-connected: leave-suppression {}", if effective { "ARMED" } else { "disarmed" }),
            || {
                if effective {
                    "Stay connected enabled".to_string()
                } else {
                    "Stay connected disabled (vanilla disconnects)".to_string()
                }
            },
        );
    }

    /// Fold the hooks' running suppression counter through the aggregating announcer into one
    /// milestone line + ER-voiced toast (never per-fire; see the module docs).
    fn announce_suppressions(&mut self, delta: f32) {
        let Some(n) = self.announcer.tick(SUPPRESSED_TOTAL.load(Ordering::Relaxed), delta) else {
            return;
        };
        // `caller` is meaningful only for site A; record_suppression clears it to 0 on site B, so
        // print it only for A to avoid a misleading `caller=0x0` on an inline suppression.
        let site = LAST_SITE.load(Ordering::Relaxed);
        if site == SITE_INLINE {
            log::info!("stay-connected: suppressed {n} game-driven disconnect(s) (last: site B: update-task inline)");
        } else {
            let caller = LAST_CALLER.load(Ordering::Relaxed);
            log::info!(
                "stay-connected: suppressed {n} game-driven disconnect(s) (last: site A: leave_session, caller={caller:#x})"
            );
        }
        // In-world effect → ER voice, no mechanical values (CLAUDE.md > message voice).
        crate::notify::with_mut(|notes| notes.info("The bond endures".to_string()));
    }
}

impl Feature for StayConnectedTick {
    fn name(&self) -> &'static str {
        "stay-connected"
    }

    // Default phase (`FrameBegin`): session-lifecycle config, not frame-order-sensitive world state —
    // same reasoning as `session_limit`/`seamless`. The gate must be armed before the frame's
    // session update runs its leave decisions; FrameBegin precedes the session update task's work.

    fn on_frame(&mut self, tick: Tick) {
        let desired = crate::state::with(|c| c.gameplay.stay_connected);
        let hooks = HOOKS_LIVE.load(Ordering::Relaxed);
        // Arm only when the config asks for it AND *both* sites are gated — a partial install would
        // otherwise claim suppression while leaving one game-driven leave route open.
        let effective = desired && hooks == BOTH_SITES;
        ARMED.store(effective, Ordering::Relaxed);

        self.warn_if_incomplete(desired, hooks);
        self.announce_arm_state(effective, hooks);
        self.announce_suppressions(tick.delta);
    }
}
