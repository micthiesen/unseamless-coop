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
use std::sync::atomic::{AtomicBool, Ordering};

use ilhook::x64::{CallbackOption, HookFlags, Registers, hook_closure_jmp_back};
use unseamless_core::saves::{coop_save_path, isolates_saves};
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows::core::s;

/// Windows extended-length path maximum (`\\?\` paths). We scan `lpFileName` up to this for its NUL
/// terminator; a string with no NUL in range is treated as unparseable and passed through untouched.
const MAX_PATH_SCAN: usize = 32_768;

thread_local! {
    /// Holds the rewritten, NUL-terminated wide path while the original `CreateFileW` reads it. One
    /// per thread; reused each call. `rcx` points into this, so it must not be reallocated between
    /// the detour returning and `CreateFileW` reading — which holds, since the read is synchronous
    /// and the next rewrite on this thread is a later, separate call.
    static REWRITE_BUF: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// Logged-once latch so the first redirect emits a milestone `info` line (visible even with hot-path
/// logging off), while subsequent ones stay at `debug`.
static ANNOUNCED: AtomicBool = AtomicBool::new(false);

/// Install the co-op-save redirect for extension `ext` (the validated `save.file_extension`). No-op
/// (returns `Ok`) when isolation is off (`ext` empty or `sl2`) — the user opted back into the vanilla
/// save, so we leave `CreateFileW` untouched. Returns `Err` only if the hook itself can't be placed.
///
/// # Safety
/// Patches executable memory in `kernel32`. Call once, on the init thread, before the game first
/// opens its save (well before the title/load screen, which is the first save read). The `ext` is
/// captured into the resident detour.
pub unsafe fn install(ext: &str) -> Result<(), String> {
    if !isolates_saves(ext) {
        log::info!("co-op saves: isolation off (save.file_extension = {ext:?}); using vanilla .sl2");
        return Ok(());
    }
    let ext = ext.to_ascii_lowercase();

    let kernel32 = unsafe { GetModuleHandleA(s!("kernel32.dll")) }
        .map_err(|e| format!("kernel32.dll not loaded: {e}"))?;
    let proc = unsafe { GetProcAddress(kernel32, s!("CreateFileW")) }
        .ok_or_else(|| "CreateFileW export not found".to_string())?;
    let addr = proc as usize;

    let ext_for_hook = ext.clone();
    let hook = unsafe {
        hook_closure_jmp_back(
            addr,
            move |regs: *mut Registers| create_file_detour(regs, &ext_for_hook),
            CallbackOption::None,
            HookFlags::empty(),
        )
    }
    .map_err(|e| format!("hooking CreateFileW: {e:?}"))?;
    std::mem::forget(hook); // resident for the process lifetime — never unhook a live IO path

    log::info!("co-op saves: redirecting *.sl2 -> *.{ext} (CreateFileW hooked at {addr:#x})");
    Ok(())
}

/// The `CreateFileW` detour. `lpFileName` is in `rcx`. If it names a vanilla save, repoint `rcx` at a
/// rewritten co-op path held in the thread-local buffer; otherwise leave the registers untouched and
/// the original opens the path as given.
fn create_file_detour(regs: *mut Registers, ext: &str) {
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
    if !(wide_ends_with_ci(wide, ".sl2") || wide_ends_with_ci(wide, ".sl2.bak")) {
        return;
    }
    // Suffix hit: now it's worth converting and asking the tested core for the exact rewrite. Invalid
    // UTF-16 (never a real save path) → pass through untouched.
    let Ok(path) = String::from_utf16(wide) else {
        return;
    };
    let Some(coop) = coop_save_path(&path, ext) else {
        return;
    };

    // Milestone on the first redirect; debug thereafter (this is a warm path — see the logging rule).
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        log::info!("co-op saves: redirecting {path} -> {coop}");
    } else {
        log::debug!("co-op saves: redirecting {path} -> {coop}");
    }

    // Stage the rewritten path in the thread-local buffer and repoint rcx at it. Done last, with no
    // further work after, so a (hypothetical) re-entrant CreateFileW during the logging above can't
    // clobber the buffer between staging it and returning.
    REWRITE_BUF.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend(coop.encode_utf16());
        buf.push(0); // NUL-terminate for CreateFileW
        unsafe { (*regs).rcx = buf.as_ptr() as u64 };
    });
}

/// Scan a wide (UTF-16) C string for its NUL terminator, up to `cap` units. Returns the length in
/// units (excluding the NUL), or `None` if no NUL is found within `cap`.
unsafe fn wide_len(ptr: *const u16, cap: usize) -> Option<usize> {
    (0..cap).find(|&i| unsafe { *ptr.add(i) } == 0)
}

/// Allocation-free: does the wide slice end with `suffix` (ASCII, case-insensitive)? Used as the
/// hot-path pre-filter so non-save opens never allocate a `String`.
fn wide_ends_with_ci(wide: &[u16], suffix: &str) -> bool {
    let s = suffix.as_bytes();
    let Some(start) = wide.len().checked_sub(s.len()) else {
        return false;
    };
    wide[start..]
        .iter()
        .zip(s)
        .all(|(&w, &b)| w < 0x80 && (w as u8).eq_ignore_ascii_case(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn pre_filter_matches_vanilla_suffixes_case_insensitively() {
        assert!(wide_ends_with_ci(&wide(r"X\ER0000.sl2"), ".sl2"));
        assert!(wide_ends_with_ci(&wide(r"X\ER0000.SL2"), ".sl2"));
        assert!(wide_ends_with_ci(&wide(r"X\ER0000.sl2.bak"), ".sl2.bak"));
        assert!(!wide_ends_with_ci(&wide(r"X\ER0000.sl2.bak"), ".sl2"));
        assert!(!wide_ends_with_ci(&wide(r"X\foo.txt"), ".sl2"));
        assert!(!wide_ends_with_ci(&wide("ab"), ".sl2.bak")); // shorter than the suffix
    }
}
