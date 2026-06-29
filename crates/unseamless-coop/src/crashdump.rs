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

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the unhandled-exception filter once. Idempotent; safe to call before logging is up (the
/// handler logs via `log`, which no-ops until a logger is set). Call as early as possible so it covers
/// later hook installs (the overlay's DX12 present-hook in particular).
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: registering a process-global top-level filter; `handler` is a valid `extern "system"` fn.
    unsafe { SetUnhandledExceptionFilter(Some(handler)) };
    log::info!("crashdump: unhandled-exception handler installed (logs the faulting module on a hard crash)");
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
        }
        log::error!("crashdump: ==== end ==== (symbolicate our frames: addr2line on a --diag build at ImageBase+offset; see the /windows-test skill)");
        log::logger().flush();
    });
    EXECUTE_HANDLER
}
