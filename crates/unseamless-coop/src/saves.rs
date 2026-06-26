//! Co-op save isolation: redirect the game's save file from vanilla `ER0000.sl2` to `ER0000.<ext>`
//! (default `co2`) by detouring `kernel32!CreateFileW` and rewriting the path argument. ERSC does
//! the same so a character's co-op progress and single-player progress live in two independent
//! files; for us it is **safety-critical** — the failure mode is corrupting the player's vanilla
//! save. The decision/transform is the host-tested [`unseamless_core::saves`]; this file is the thin,
//! unavoidably-`unsafe` binding. Design + the re-derivation trail: `docs/COOP-SAVES.md`.
//!
//! ## Why a `CreateFileW` detour (not an SDK field, not a game hook)
//! The save path is built and opened deep in the game's IO, with no typed SDK field to point
//! elsewhere. The robust, version-stable interception is the Win32 call every save open funnels
//! through — `kernel32!CreateFileW` — exactly the lever the MIT `vswarte/alt-saves` mod uses
//! (mechanism re-derived here per CLAUDE.md > Clean-room; we do **not** port its `regulation.bin`
//! patch). Because it's a *system* DLL export, this is **not** Arxan-protected game `.text`, so it's
//! safe the way the `input.rs` user32/dinput8 hooks are.
//!
//! ## Mechanism (jmp-back, one register edit)
//! We install a `jmp-back` hook: our callback runs at `CreateFileW` entry, may edit the saved
//! registers, and the original then continues with them. The first arg (`lpFileName`) is in `rcx`
//! (win64). If the path is a vanilla save we point `rcx` at a rewritten copy and return; the original
//! `CreateFileW` opens our co-op path instead. Editing one register avoids forwarding `CreateFileW`'s
//! 7 args (3 on the stack) and the inline-detour recursion trap (we never call `CreateFileW` ourselves).
//!
//! ## Lifetime & threading (mirrors input.rs)
//! Installed once on the init thread, **forgotten** (resident for the process lifetime — unhooking a
//! live IO path is a use-after-free). The rewritten path lives in a **thread-local** buffer: the
//! detour runs on whatever thread opens the file, and `CreateFileW` reads `lpFileName` synchronously
//! before returning, so the buffer only needs to outlive this one call on this one thread — the next
//! open on the same thread reuses it. The hot path is allocation-free for non-save opens (every file
//! the process opens passes through here): we scan for the path length and pre-filter the suffix on
//! the borrowed wide slice, only converting/rewriting on a `*.sl2` / `*.sl2.bak` hit.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_back};
use unseamless_core::saves::{coop_save_path, isolates_saves, wide_has_vanilla_suffix};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::s;

use crate::hook::HookError;

/// Scan ceiling for `lpFileName`: the Windows extended-length (`\\?\`) path range is ~32K units, so a
/// real save path hits its NUL well before this. A string with no NUL in range is treated as
/// unparseable and passed through untouched.
const MAX_PATH_SCAN: usize = 32_768;

thread_local! {
    /// Holds the rewritten, NUL-terminated wide path while the original `CreateFileW` reads it. One
    /// per thread; reused each call. `rcx` points into this, so it must not be reallocated between
    /// the detour returning and `CreateFileW` reading — which holds, since the read is synchronous
    /// and the next rewrite on this thread is a later, separate call.
    static REWRITE_BUF: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// Distinct save paths we've already announced a redirect for, so each mapping is logged **once** (at
/// `info`, visible even with hot-path logging off) rather than on every file open. Elden Ring reopens
/// `ER0000.sl2` constantly during a session — dozens of identical lines per save — which otherwise
/// drowns the diag/shared log. The set stays tiny (a couple of paths: the save + its `.bak`). Locked
/// only on a save-suffix hit (already rare — the non-save hot path returns at the suffix pre-filter)
/// and never while logging, so it can't deadlock against a re-entrant open.
static LOGGED_REDIRECTS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Install the co-op-save redirect for extension `ext` (the validated `save.file_extension`). No-op
/// (returns `Ok`) when isolation is off (`ext` empty or `sl2`) — the user opted back into the vanilla
/// save, so we leave `CreateFileW` untouched. Returns `Err` only if the hook itself can't be placed.
///
/// # Safety
/// Patches executable memory in `kernel32`. Call once, on the init thread, before the game first
/// opens its save (well before the title/load screen, which is the first save read). The `ext` is
/// captured into the resident detour.
pub unsafe fn install(ext: &str) -> Result<(), HookError> {
    if !isolates_saves(ext) {
        log::info!("co-op saves: isolation off (save.file_extension = {ext:?}); using vanilla .sl2");
        return Ok(());
    }
    // Use the validated extension verbatim (config guarantees 1..=120 ASCII alphanumerics) — don't
    // silently case-fold it, so the on-disk extension matches what the user configured.
    let ext = ext.to_string();

    let kernel32 = unsafe { GetModuleHandleA(s!("kernel32.dll")) }
        .map_err(|e| HookError::ModuleNotLoaded { module: "kernel32.dll", err: e })?;
    let proc = unsafe { GetProcAddress(kernel32, s!("CreateFileW")) }
        .ok_or(HookError::ExportNotFound("CreateFileW export"))?;
    let addr = proc as usize;

    // Residual install-time race (accepted, matches input.rs convention): ilhook patches CreateFileW's
    // first bytes without suspending other threads (CallbackOption::None), so a thread calling
    // CreateFileW concurrently during this write could fetch a torn instruction. We install very early
    // (our init thread, before the title screen) to shrink the window, and it's held up in practice on
    // the rig. If intermittent startup crashes ever appear here, the fix is a thread-suspending
    // ThreadCallback around the patch.
    let ext_for_hook = ext.clone();
    let hook = unsafe {
        hook_closure_jmp_back(
            addr,
            move |regs: *mut Registers| create_file_detour(regs, &ext_for_hook),
            CallbackOption::None,
            HookFlags::empty(),
        )
    }
    .map_err(|e| HookError::Install { what: "CreateFileW".to_string(), detail: format!("{e:?}") })?;
    std::mem::forget(hook); // resident for the process lifetime — never unhook a live IO path

    log::info!("co-op saves: redirecting *.sl2 -> *.{ext} (CreateFileW hooked at {addr:#x})");
    Ok(())
}

/// The `CreateFileW` detour. `lpFileName` is in `rcx`. If it names a vanilla save, repoint `rcx` at a
/// rewritten co-op path held in the thread-local buffer; otherwise leave the registers untouched and
/// the original opens the path as given.
///
/// # Safety
/// `regs` must point at the saved registers for a `CreateFileW` call (ilhook's hook-site contract):
/// `rcx` is null or a valid, NUL-terminated `LPCWSTR`. Invoked only from the installed detour.
unsafe fn create_file_detour(regs: *mut Registers, ext: &str) {
    let ptr = unsafe { (*regs).rcx } as *const u16;
    if ptr.is_null() {
        return;
    }
    // Length-scan first (no allocation), then pre-filter the suffix on the borrowed slice, so the
    // overwhelming majority of opens (non-saves) cost only a NUL scan + a few byte compares.
    let Some(len) = (unsafe { wide_len(ptr, MAX_PATH_SCAN) }) else {
        return; // no NUL in range — don't touch a path we can't bound
    };
    let wide = unsafe { std::slice::from_raw_parts(ptr, len) };
    if !wide_has_vanilla_suffix(wide) {
        return;
    }
    // Suffix hit: now it's worth converting and asking the tested core for the exact rewrite. A
    // save-suffixed path that isn't valid UTF-16 is never a real save path, but it's an anomaly worth
    // surfacing — and passing it through would open the vanilla file, the one thing we must avoid — so
    // log it loudly rather than dropping it silently.
    let Ok(path) = String::from_utf16(wide) else {
        log::warn!("co-op saves: a .sl2-suffixed path wasn't valid UTF-16; left untouched");
        return;
    };
    let Some(coop) = coop_save_path(&path, ext) else {
        return;
    };

    // Announce each distinct redirect once (at info, so it shows even with hot-path logging off), then
    // stay silent on the game's constant reopens of the same save — otherwise one save load logs the
    // same line dozens of times and buries the rest of the diag log. Insert under the lock, then log
    // *after* the guard drops (end of the let statement), so a re-entrant open from the logger can't
    // deadlock on this mutex.
    let first_time = LOGGED_REDIRECTS.lock().unwrap_or_else(|p| p.into_inner()).insert(path.clone());
    if first_time {
        log::info!("co-op saves: redirecting {path} -> {coop}");
    }

    // Stage the rewritten path in the thread-local buffer and repoint rcx at it. The buffer is safe
    // from re-entrant clobbering because the only thing in this detour that could re-enter CreateFileW
    // on this thread is the logging above, and the logger never opens a *.sl2 path (so a re-entrant
    // call returns at the suffix pre-filter, never reaching this staging block). Staging last — with
    // no work after — is belt-and-suspenders on top of that invariant.
    REWRITE_BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend(coop.encode_utf16());
        buf.push(0); // NUL-terminate for CreateFileW
        unsafe { (*regs).rcx = buf.as_ptr() as u64 };
    });
}

/// Scan a wide (UTF-16) C string for its NUL terminator, up to `cap` units. Returns the length in
/// units (excluding the NUL), or `None` if no NUL is found within `cap`. Assumes `ptr` is a valid,
/// 2-byte-aligned, NUL-terminated wide string (the contract of any real `LPCWSTR` `CreateFileW`
/// caller); the `cap` bound keeps even a malformed pointer from running away.
unsafe fn wide_len(ptr: *const u16, cap: usize) -> Option<usize> {
    (0..cap).find(|&i| unsafe { *ptr.add(i) } == 0)
}
