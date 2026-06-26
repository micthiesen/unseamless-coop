//! AOB scan + in-place code patch over the **live game image**. For the handful of features that
//! have no typed SDK field to write — the only lever is to find the relevant machine code and edit
//! it (typically NOP a conditional branch so the game falls through a gate). The first consumer is
//! skip-intros (`app::install` NOPs the boot/title logo gate); future ones (offline-popup trigger
//! suppression) reuse `nop_landmark` / `apply`. Design notes: `docs/CODE-PATCHING.md`.
//!
//! All the machinery is already on the tree: pelite's masked AOB scanner (the PE parser the SDK
//! uses for its RVA work), `Program::current()` over the mapped exe, and the `windows` crate for
//! page protection. No new transport, no new hooking lib.
//!
//! ## Lifetime & safety (mirrors the task-handle invariants — see CLAUDE.md)
//! A code patch is **applied once, at install, on the init thread, and never undone**:
//! - Not in `DllMain` (loader-lock hazard) — it runs inside `app::install` on the init thread.
//! - Safety comes from running *before the patched code path is first taken* (the logo gate hasn't
//!   fired at install) and from the edit being a self-contained instruction rewrite, not a
//!   cross-thread state mutation. A future patch targeting a hot path must be reasoned about per-site.
//! - No `DLL_PROCESS_DETACH` restore: the DLL stays resident (registered tasks point into our
//!   image), so there's no unload path to restore from — exactly like `mem::forget`-ing a task handle.
//!
//! ## Fail-safe locating
//! We AOB-scan rather than hardcode a VA even though we version-pin: a stale offset fails *silent and
//! dangerous* (patches the wrong byte → corrupt code), while a scan fails *loud and safe* (no match →
//! we skip the feature, log, and the game runs unmodded). That maps onto our degrade-and-notify error
//! policy. The pin still earns its keep: it guarantees one known game build, so the patterns can be
//! tight and specific.

use fromsoftware_shared::program::Program;
use pelite::pattern::Atom;
use pelite::pe64::Pe;

use crate::hook::HookError;

/// Overwrite `bytes.len()` bytes at `addr` in the live image: VirtualProtect → write → restore →
/// FlushInstructionCache. Returns the bytes that were there before (for logging/diagnostics only —
/// we do **not** keep them to restore; patches are permanent, see the module docs).
///
/// # Safety
/// `addr` must point at `bytes.len()` valid bytes inside the loaded game image (e.g. a site
/// validated by [`nop_landmark`]). Must run on the init thread at install, before the patched code
/// path first executes.
pub unsafe fn apply(addr: *mut u8, bytes: &[u8]) -> Result<Vec<u8>, HookError> {
    use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows::Win32::System::Memory::{
        PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let len = bytes.len();
    let original = unsafe { std::slice::from_raw_parts(addr, len).to_vec() };
    unsafe {
        // 1. Make the page(s) writable, remembering the old protection. VirtualProtect rounds to
        //    whole pages, so a sub-page (addr, len) — even one straddling a boundary — is covered.
        //    This is the only failure that means *nothing happened*, so propagate it: the page is
        //    untouched and the patch never started.
        let mut old = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(addr.cast(), len, PAGE_EXECUTE_READWRITE, &mut old)?;
        // 2. Write the replacement bytes. From here the patch is committed.
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr, len);
        // 3. Restore the original protection (don't leave the page RWX). The write already landed, so
        //    a restore failure is logged, not propagated — returning Err for an already-applied patch
        //    would be a misleading "write failed", and must not skip the flush below.
        let mut restored = PAGE_PROTECTION_FLAGS(0);
        if let Err(e) = VirtualProtect(addr.cast(), len, old, &mut restored) {
            log::warn!("patch: could not restore page protection after write: {e}");
        }
        // 4. Flush the i-cache so the CPU re-fetches the patched bytes (required by contract even
        //    though x86 is largely coherent; a near-no-op under Wine but cheap insurance). Always run.
        if let Err(e) = FlushInstructionCache(GetCurrentProcess(), Some(addr.cast()), len) {
            log::warn!("patch: FlushInstructionCache failed: {e}");
        }
    }
    Ok(original)
}

/// Resolve a patch site *relative to a stable landmark*: scan for the unique `landmark`, step
/// `offset` bytes to the site (in RVA space, so `rva_to_va` bounds-checks it against the mapped image
/// — we never form a wild pointer), and verify the byte there equals `expect`. Returns the live site
/// pointer, or `None` (logged) on any failure: no/ambiguous match, out-of-range site, or wrong
/// opcode. Shared by [`overwrite_landmark`] / [`nop_landmark`].
fn resolve_landmark(name: &str, landmark: &[Atom], offset: isize, expect: u8) -> Option<*mut u8> {
    let program = Program::current();
    let mut save = [0u32; 1]; // pelite writes the unique match-start RVA into the implicit Save(0)
    if !program.scanner().finds_code(landmark, &mut save) {
        // finds_code is false on zero OR multiple matches, so a too-loose landmark fails safe here.
        log::warn!("patch '{name}': landmark not found or not unique; feature disabled this session");
        return None;
    }
    let Ok(site_rva) = u32::try_from(i64::from(save[0]) + offset as i64) else {
        log::warn!("patch '{name}': patch site (match {:#X} {offset:+}) out of range; skipping", save[0]);
        return None;
    };
    let Some(site) = program.rva_to_va(site_rva).ok().map(|va| va as usize as *mut u8) else {
        log::warn!("patch '{name}': patch site RVA {site_rva:#X} not in the loaded image; skipping");
        return None;
    };
    // Guard against AOB drift landing on the wrong instruction: the byte must be what we expect.
    let actual = unsafe { *site };
    if actual != expect {
        log::warn!(
            "patch '{name}': site byte {actual:#04X} != expected {expect:#04X} (AOB drift?); skipping"
        );
        return None;
    }
    Some(site)
}

/// Locate a patch site relative to a unique `landmark` (see [`resolve_landmark`]) and overwrite
/// `replacement.len()` bytes there — the general code-patch primitive (e.g. write a `0xC3` `ret` at a
/// function entry to neuter it). Returns whether it applied. Every failure path leaves the game
/// unmodded and logs why: degrade, never abort.
///
/// Safe to call where it's sound to patch (init thread at install, before the patched path runs): the
/// site is bounds-checked and the `expect` byte verified before any write.
pub fn overwrite_landmark(
    name: &str,
    landmark: &[Atom],
    offset: isize,
    expect: u8,
    replacement: &[u8],
) -> bool {
    let Some(site) = resolve_landmark(name, landmark, offset, expect) else {
        return false;
    };
    match unsafe { apply(site, replacement) } {
        Ok(orig) => {
            log::info!("patched '{name}': {orig:02X?} -> {replacement:02X?}");
            true
        }
        Err(e) => {
            log::error!("patch '{name}' write failed: {e}");
            false
        }
    }
}

/// Overwrite `count` bytes at a landmark-relative site with `0x90` (NOP) — the common case for
/// neutering a short conditional jump (e.g. the skip-intros boot-logo gate). Thin wrapper over
/// [`overwrite_landmark`].
pub fn nop_landmark(name: &str, landmark: &[Atom], offset: isize, expect: u8, count: usize) -> bool {
    overwrite_landmark(name, landmark, offset, expect, &vec![0x90u8; count])
}
