//! Crash diagnostics for the native-Windows overlay crash that vkd3d/WARP don't trigger (see
//! [`docs/OVERLAY-RENDERING.md`](../../../docs/OVERLAY-RENDERING.md) > "Native-Windows Crash").
//!
//! Installs a process-global unhandled-exception filter that, on a hard SEH fault (e.g. the access
//! violation at the first hooked `Present` on native NVIDIA), logs the **decisive datum** the
//! breadcrumb trail can't give: the *faulting module+offset* — i.e. *which* module the instruction
//! pointer is in when it dies. That alone discriminates the live hypotheses:
//! `nvwgf2umx.dll`/`nvd3dumx.dll` ⇒ inside the NVIDIA driver (hyp #1 trigger — its present threading);
//! a streamline/overlay interposer DLL ⇒ hyp #2; `hudhook`/our own module ⇒ something in the detour
//! glue. It also logs the exception code, access-violation read/write target, and the faulting
//! registers (`Rip`/`Rsp`/`Rbp`).
//!
//! Unsymbolicated by design — mingw builds ship no PDB. The logged `+offset` is **module-relative**
//! (an RVA), so to resolve our *own* frames give addr2line the PE virtual address = the module's
//! `ImageBase` + the logged offset, against a `--diag` build (which keeps symbols):
//! `x86_64-w64-mingw32-addr2line -f -C -e <diag exe/dll> $((ImageBase + offset))` — the exe links at
//! `0x140000000`; read a DLL's ImageBase from `objdump -p`. Driver/interposer frames are read by module
//! name. This is the artifact a real-NVIDIA run produces that the VM/WARP cannot, so it's staged now and
//! fires the moment any NVIDIA box runs it. (Verified on WARP via the self-test, 2026-06-29.)
//!
//! Self-contained (only `std` + `windows` + `log`) so it is shared verbatim by the cdylib (the player
//! build / a full ER friend run) and the `dx12-harness` (a friend's lightweight, ER-free repro) via a
//! `#[path]` include — keep it free of `crate::` references.
//!
//! **Displacement guard:** the top-level filter is a single process-global slot, and anything else in
//! the process (Steam's overlay, the CRT, another mod) can silently overwrite it. That happened in the
//! 2026-07-01 friend crash: the access violation reached WER, yet this handler — installed at t+0.03s —
//! logged nothing (see OVERLAY-RENDERING.md > "WER Verdict"). So [`install`] also spawns a detached
//! guard thread that re-asserts the filter every few seconds; `SetUnhandledExceptionFilter` returns the
//! *previous* filter, so each re-assert doubles as detection, and the guard warns (once per displacement
//! episode, not per tick) naming the module that took the slot — or that the slot was cleared to null.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows::core::PCWSTR;

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// How often the guard thread re-asserts the filter. A sleeping thread at this cadence is free, and a
/// few seconds caps how long a set-and-hold displacer keeps the slot unlogged (the 2026-07-01 crash
/// landed ~16s after install; a displacer that re-hooks between ticks still gets detected and warned,
/// though it keeps winning the slot).
const REASSERT_PERIOD: Duration = Duration::from_secs(3);

/// `LAST_DISPLACER` sentinel: no displacement episode is currently being reported. (`0` can't serve as
/// the sentinel — it means the slot was found cleared to null.)
const NO_DISPLACER: usize = usize::MAX;

/// The foreign filter value already warned about for the current displacement episode, so a displacer
/// that re-hooks between ticks logs once per episode, not per tick. Reset to [`NO_DISPLACER`] whenever
/// the slot is observed intact, so a later, fresh displacement — same actor or not — warns again.
static LAST_DISPLACER: AtomicUsize = AtomicUsize::new(NO_DISPLACER);

/// Install the unhandled-exception filter once. Idempotent; safe to call before logging is up (the
/// handler logs via `log`, which no-ops until a logger is set). Call as early as possible so it covers
/// later hook installs (the overlay's DX12 present-hook in particular).
///
/// Also spawns the detached displacement-guard thread (module docs) — the DLL stays resident for the
/// process lifetime, so a thread looping over its code is safe; it dies with the process.
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: registering a process-global top-level filter; `handler` is a valid `extern "system"` fn.
    unsafe { SetUnhandledExceptionFilter(Some(handler)) };
    log::info!("crashdump: unhandled-exception handler installed (logs the faulting module on a hard crash; re-asserted every {}s against displacement)", REASSERT_PERIOD.as_secs());

    let guard = std::thread::Builder::new()
        .name("usc-crashdump-guard".into())
        .spawn(|| {
            loop {
                std::thread::sleep(REASSERT_PERIOD);
                reassert_filter();
            }
        });
    if let Err(e) = guard {
        // Best-effort: the handler is still installed, just unguarded against displacement.
        log::warn!("crashdump: couldn't spawn the filter guard thread ({e}); the handler can be silently displaced");
    }

    force_test_displacement_if("UNSEAMLESS_TEST_FILTER_DISPLACEMENT");
}

/// Guard self-test (inert by default): if `env_var` is set to `1`, hand the slot to a dummy foreign
/// filter right away — the guard's first tick must then log the DISPLACED warning (naming our own
/// module, since the dummy lives in it) and re-assert ours. Validates the detect/warn/re-assert cycle
/// on any box, WARP/VM included, without waiting for a real displacer (same idea as
/// [`force_test_crash_if`]).
fn force_test_displacement_if(env_var: &str) {
    if std::env::var(env_var).as_deref() != Ok("1") {
        return;
    }
    log::warn!("crashdump: {env_var}=1 — installing a dummy top-level filter to validate the displacement guard");
    // SAFETY: registering a valid `extern "system"` filter fn; displaces ours on purpose.
    unsafe { SetUnhandledExceptionFilter(Some(test_displacer)) };
}

/// The self-test's stand-in "foreign" filter (see [`force_test_displacement_if`]). Well-behaved: if a
/// real crash lands inside the ≤one-tick test window, it defers to the OS default (WER) rather than
/// swallowing it. Trivial body — nothing here can panic across the FFI boundary.
unsafe extern "system" fn test_displacer(_info: *const EXCEPTION_POINTERS) -> i32 {
    0 // EXCEPTION_CONTINUE_SEARCH
}

/// One guard tick: re-assert our filter and inspect what held the slot. The return value of
/// `SetUnhandledExceptionFilter` is the *previous* top-level filter, so re-asserting is also the
/// detection: ours back means intact; anything else means we were displaced — warn loudly (once per
/// displacement episode) with the owning module, because a hard crash during that window would have
/// bypassed our logging entirely (exactly the 2026-07-01 silence).
fn reassert_filter() {
    // SAFETY: same process-global registration as `install`.
    let prev = unsafe { SetUnhandledExceptionFilter(Some(handler)) };
    let prev_addr = prev.map_or(0usize, |f| f as *const () as usize);
    if prev_addr == handler as *const () as usize {
        // Intact — close any open episode so a fresh displacement warns again.
        LAST_DISPLACER.store(NO_DISPLACER, Ordering::Relaxed);
        return;
    }
    if LAST_DISPLACER.swap(prev_addr, Ordering::Relaxed) == prev_addr {
        return; // same episode — the displacer re-hooks between ticks; don't spam
    }
    if prev_addr == 0 {
        log::warn!("crashdump: unhandled-exception filter had been CLEARED (top-level filter was null) — re-asserted ours");
    } else {
        log::warn!(
            "crashdump: unhandled-exception filter had been DISPLACED by {} — re-asserted ours (a crash while displaced logs nothing here; check WER)",
            module_at(prev_addr),
        );
    }
}

/// Deliberately crash (null write) if `env_var` is set to `1`, to validate the handler + its log format
/// on a machine where the *real* crash won't fire (e.g. WARP). Diagnostic self-test only.
// Used by the `dx12-harness` `#[path]` include; unreferenced in the cdylib build (which only installs).
#[allow(dead_code)]
pub fn force_test_crash_if(env_var: &str) {
    if std::env::var(env_var).as_deref() == Ok("1") {
        log::warn!("crashdump: {env_var}=1 — forcing a test access violation to validate the handler");
        // SAFETY: intentional null write to provoke a 0xC0000005 the filter will report.
        unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u8>(), 1) };
    }
}

/// Resolve a code address to `module.name+0xoffset` (or a raw-address note if no module owns it).
fn module_at(addr: usize) -> String {
    if addr == 0 {
        return "0x0 (null)".to_string();
    }
    unsafe {
        let mut hmod = HMODULE::default();
        let got = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            // With FROM_ADDRESS, lpModuleName is reinterpreted as an address inside the module.
            PCWSTR(addr as *const u16),
            &mut hmod,
        );
        if got.is_err() || hmod.0.is_null() {
            return format!("{addr:#018x} (no owning module)");
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(Some(hmod), &mut buf) as usize;
        let path = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        let name = path.rsplit(['\\', '/']).next().unwrap_or(path.as_str());
        format!("{name}+{:#x}", addr.wrapping_sub(hmod.0 as usize))
    }
}

/// Scan the stack from `rsp` for values that fall inside the main `eldenring.exe` image and log them
/// as `eldenring.exe+offset` — a best-effort backtrace (return addresses left on the stack) that
/// recovers the call chain that reached the fault. Heuristic (no unwind info): it prints any stack
/// slot pointing into the exe, so a reader disassembles each to confirm it's a real return site (the
/// byte before a return address is a `call`). Bounded to a small window so it can't fault on unmapped
/// stack. The decisive datum for chasing a null-deref: which create/session function called in.
fn log_stack_backtrace(rsp: usize) {
    let base = unsafe { GetModuleHandleW(None) }
        .map(|h| h.0 as usize)
        .unwrap_or(0);
    if base == 0 {
        return;
    }
    // The exe image spans a few tens of MB from its base; return addresses live in its two `.text`
    // sections. Filter to [base+0x1000, base+0x7000000) — wide enough to cover both, tight enough to
    // drop obvious non-image values. Read at most 256 qwords (2 KiB) above rsp: current frame + callers,
    // safely within mapped stack.
    let hi = base + 0x0700_0000;
    let mut printed = 0u32;
    for i in 0..256usize {
        let v = unsafe { ((rsp + i * 8) as *const usize).read_volatile() };
        if v >= base + 0x1000 && v < hi {
            log::error!(
                "crashdump:   bt[{printed}] eldenring.exe+{:#x}  (stack {:#018x})",
                v - base,
                rsp + i * 8,
            );
            printed += 1;
            if printed >= 24 {
                break;
            }
        }
    }
    if printed == 0 {
        log::error!("crashdump:   bt: no in-image return addresses found in the scanned window");
    }
}

/// Human label for the common SEH exception codes (so a log reader doesn't decode hex).
fn code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "ACCESS_VIOLATION",
        0xC000_001D => "ILLEGAL_INSTRUCTION",
        0xC000_0025 => "NONCONTINUABLE_EXCEPTION",
        0xC000_008C => "ARRAY_BOUNDS_EXCEEDED",
        0xC000_0094 => "INT_DIVIDE_BY_ZERO",
        0xC000_00FD => "STACK_OVERFLOW",
        0x8000_0003 => "BREAKPOINT",
        0xC000_0096 => "PRIV_INSTRUCTION",
        _ => "UNKNOWN",
    }
}

/// The top-level filter. Logs the fault, then returns `EXCEPTION_EXECUTE_HANDLER` (1) so the process
/// terminates after we've recorded it (rather than hang on a WER dialog). Best-effort and panic-safe.
unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
    const EXECUTE_HANDLER: i32 = 1; // EXCEPTION_EXECUTE_HANDLER
    let _ = std::panic::catch_unwind(|| {
        let Some(p) = (unsafe { info.as_ref() }) else {
            log::error!("crashdump: unhandled exception (no EXCEPTION_POINTERS)");
            return;
        };
        let Some(rec) = (unsafe { p.ExceptionRecord.as_ref() }) else {
            log::error!("crashdump: unhandled exception (no EXCEPTION_RECORD)");
            return;
        };
        let code = rec.ExceptionCode.0 as u32;
        let fault = rec.ExceptionAddress as usize;
        log::error!(
            "crashdump: ==== UNHANDLED EXCEPTION ==== code={code:#010x} ({}) at {} [raw {fault:#018x}]",
            code_name(code),
            module_at(fault),
        );
        if code == 0xC000_0005 && rec.NumberParameters >= 2 {
            let op = match rec.ExceptionInformation[0] {
                0 => "read",
                1 => "write",
                8 => "execute (DEP)",
                other => return log::error!("crashdump:   access violation (op code {other})"),
            };
            log::error!(
                "crashdump:   access violation: tried to {op} {:#018x}",
                rec.ExceptionInformation[1],
            );
        }
        // Faulting registers from the captured context (Rip is the faulting instruction; Rsp/Rbp frame
        // it). Resolving Rip's module corroborates ExceptionAddress and is the decisive datum.
        if let Some(ctx) = unsafe { p.ContextRecord.as_ref() } {
            log::error!(
                "crashdump:   regs: rip={:#018x} ({}) rsp={:#018x} rbp={:#018x}",
                ctx.Rip,
                module_at(ctx.Rip as usize),
                ctx.Rsp,
                ctx.Rbp,
            );
            log_stack_backtrace(ctx.Rsp as usize);
        }
        log::error!("crashdump: ==== end ==== (symbolicate our frames: addr2line on a --diag build at ImageBase+offset; see the /windows-test skill)");
        log::logger().flush();
    });
    EXECUTE_HANDLER
}
