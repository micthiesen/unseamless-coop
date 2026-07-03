//! XInput inline-hook-collision phase — the native-Windows repro the DX12 present-hook harness was
//! always missing.
//!
//! Root cause of the native-Windows crash (docs/OVERLAY-RENDERING.md > "WER Verdict", 2026-07-01):
//! it was **never** the DX12 present hook. It was an inline-hook collision on
//! `XINPUT1_4.dll!XInputGetState`. Our retired overlay input hook laid an ilhook 14-byte absolute-jmp
//! patch over the entry; a **second** 5-byte-`E9` inline hooker (Steam's `gameoverlayrenderer64.dll`
//! the leading suspect) got there first, saved the pristine 5-byte prologue, and built a trampoline
//! that runs that prologue then `jmp XInputGetState+5`. Once our longer patch overwrote the entry,
//! `entry+5` was mid-*our*-patch garbage, so that blind jump-back executed garbage → AV with
//! `RIP = XInputGetState+5` (WER P8 = `0x9a65`). The vkd3d rig can't reproduce it (different xinput +
//! overlay stack); the WARP `dx12-harness` never installed the input hooks at all. This module is
//! that missing repro, run on a real Windows loader in the Win11 VM with **no ELDEN RING**.
//!
//! Two selectable phases (`DX12_HARNESS_XINPUT=repro|iat`, or `--xinput=repro|iat`):
//!
//! - **`repro`** — reproduce the collision and prove it is fatal. Install a 5-byte inline hooker on
//!   `XInputGetState` (captures the pristine prologue), then our inline patch on top, then poll →
//!   deterministic **ACCESS_VIOLATION at `XInputGetState+5`**, logged by [`crate::crashdump`].
//! - **`iat`** — same second hooker, but our capture is an **IAT hook** on the harness exe's own
//!   `XInputGetState` import (mirroring the shipped fix, `unseamless-coop/src/input.rs::install_xinput`):
//!   the function body is never touched, so `entry+5` stays pristine and polling **survives** — no AV.
//!
//! # Clean-room / scope
//! This is our own repro of a collision *we* diagnosed, built from the public WER datum + our own
//! shipped fix — no upstream bytes, no anti-cheat/DRM interaction (see CLAUDE.md > Clean-room hygiene).
//!
//! # One divergence from the wild, called out
//! In the wild, the bytes at `entry+5` were the ASLR-random tail of our 14-byte abs-jmp patch (address
//! bytes decoded as an instruction), which *happened* to AV. To make the repro **deterministic on
//! every run**, our stand-in inline patch is a 5-byte `E9`→detour followed by an explicit
//! read-address-0 instruction ([`FAULT_SLED`]) parked exactly at `entry+5`. The mechanism proven is
//! identical — a >5-byte patch whose `entry+5` tail the second hooker's trampoline blindly jumps into
//! — but the fault is a guaranteed read AV at `0x0` instead of an ASLR lottery.

use std::sync::atomic::{AtomicUsize, Ordering};

use pelite::pe64::imports::Import;
use pelite::pe64::{Pe, PeView};
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetModuleHandleW, GetProcAddress, LoadLibraryA};
use windows::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, PAGE_READWRITE,
    VirtualAlloc, VirtualProtect,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::{PCWSTR, s};

// XINPUT_STATE / XINPUT_GAMEPAD, laid out per the documented xinput.h ABI (mirrors the shipped
// input.rs types). We only ever pass a `*mut` through to the real function, but the layout must be
// right so the real `XInputGetState` writes within our stack slot.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputGamepadRaw {
    buttons: u16,
    left_trigger: u8,
    right_trigger: u8,
    thumb_lx: i16,
    thumb_ly: i16,
    thumb_rx: i16,
    thumb_ry: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XInputStateRaw {
    packet_number: u32,
    gamepad: XInputGamepadRaw,
}

// Layout guard (mirrors input.rs): the real `XInputGetState` writes a full 16-byte `XINPUT_STATE` into
// the caller's `&mut` slot, so a field/padding skew would be a silent out-of-bounds stack write across
// the FFI boundary. Pin the layout against the documented xinput.h ABI at compile time.
const _: () = {
    assert!(size_of::<XInputGamepadRaw>() == 12);
    assert!(size_of::<XInputStateRaw>() == 16);
    assert!(std::mem::offset_of!(XInputStateRaw, gamepad) == 4);
};

/// The `XInputGetState` ABI, stated once so the transmute sites below can't drift (mirrors
/// `input.rs::XInputGetStateFn`). A transmute target is unchecked, so a single alias is the guard.
type XInputGetStateFn = unsafe extern "system" fn(u32, *mut XInputStateRaw) -> u32;

// The harness exe's own static import of `XInputGetState`, by name, from xinput1_4.dll. `raw-dylib`
// synthesizes the import with no import library (mingw ships none for xinput), giving the exe a real
// IAT slot — the exact thing the survival phase swaps, mirroring how the game imports it and
// input.rs::install_xinput hooks that slot. Calls to it compile to an indirect call through the IAT.
#[link(name = "xinput1_4", kind = "raw-dylib")]
unsafe extern "system" {
    fn XInputGetState(user_index: u32, state: *mut XInputStateRaw) -> u32;
}

/// Which XInput phase to run. Parsed from `DX12_HARNESS_XINPUT` / `--xinput=`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Reproduce the inline-hook collision → deterministic AV at `XInputGetState+5`.
    Repro,
    /// Prove the shipped IAT hook survives the same second hooker → no AV.
    Iat,
}

impl Phase {
    /// Parse a phase selector (case-insensitive). `repro`/`inline` → [`Phase::Repro`];
    /// `iat`/`survival`/`survive` → [`Phase::Iat`]. Returns `None` for anything else so the caller can
    /// reject a typo'd selector loudly rather than silently doing the wrong thing.
    pub fn parse(sel: &str) -> Option<Phase> {
        match sel.trim().to_ascii_lowercase().as_str() {
            "repro" | "inline" => Some(Phase::Repro),
            "iat" | "survival" | "survive" => Some(Phase::Iat),
            _ => None,
        }
    }
}

/// `mov rax, qword ptr [0]` (`48 8b 04 25 00000000`): a read of address `0`, the always-unmapped null
/// page — a guaranteed ACCESS_VIOLATION regardless of register state. Parked at `entry+5` so the
/// second hooker's blind `jmp entry+5` faults deterministically (see the module's "one divergence").
const FAULT_SLED: [u8; 8] = [0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00];

/// The prior hook our inline detour chains through — in the repro that is the second (5-byte)
/// hooker's trampoline, exactly as the retired ilhook detour called `original`.
static CHAIN: AtomicUsize = AtomicUsize::new(0);
/// The pre-swap value of the harness's `XInputGetState` IAT slot (the real function), recorded by
/// [`swap_our_iat`] and chained through by [`our_iat_replacement`].
static REAL_IAT: AtomicUsize = AtomicUsize::new(0);
/// Count of survival-phase polls that reached our IAT hook (for a compact log).
static IAT_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Entry point from `main`: run the selected XInput phase, then return (the process exits after — the
/// repro phase AVs mid-poll instead of returning).
///
/// The phase body is wrapped in `catch_unwind` so a *Rust* setup panic (a failed `LoadLibrary`,
/// `VirtualProtect`, unreachable allocation, etc.) is logged to the per-record-flushed harness log —
/// the artifact the operator pulls and reads — instead of only reaching stderr/`run-out.txt`. The
/// repro's *intended* crash is a hardware ACCESS_VIOLATION (SEH), which `catch_unwind` does not catch,
/// so it still propagates to the [`crate::crashdump`] handler as designed.
pub fn run(phase: Phase) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match phase {
        Phase::Repro => unsafe { run_repro() },
        Phase::Iat => unsafe { run_iat() },
    }));
    if let Err(payload) = outcome {
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        log::error!("xinput: phase {phase:?} panicked during setup: {msg} (backtrace in run-out.txt)");
        log::logger().flush();
    }
}

// ---- byte-level helpers -------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Read `n` bytes from a code address into an owned buffer (for logging entry bytes before/after a
/// patch, so the collision is legible in the pulled log).
///
/// # Safety
/// `addr..addr+n` must be readable.
unsafe fn read_bytes(addr: usize, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    unsafe { std::ptr::copy_nonoverlapping(addr as *const u8, v.as_mut_ptr(), n) };
    v
}

/// Overwrite code at `dst` with `bytes`: `VirtualProtect(RWX)` → copy → restore protection → flush the
/// instruction cache. Used to lay the inline patches on the live `XInputGetState` entry.
///
/// # Safety
/// `dst..dst+bytes.len()` must be a valid code region we may repurpose for this test.
unsafe fn write_code(dst: usize, bytes: &[u8]) {
    unsafe {
        let mut old = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(dst as *const core::ffi::c_void, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old)
            .expect("VirtualProtect RWX on the XInputGetState entry");
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
        let mut restored = PAGE_PROTECTION_FLAGS(0);
        let _ = VirtualProtect(dst as *const core::ffi::c_void, bytes.len(), old, &mut restored);
        let _ = FlushInstructionCache(GetCurrentProcess(), Some(dst as *const core::ffi::c_void), bytes.len());
    }
}

/// Allocate an executable page holding `bytes` within ±1 GB of `near`, so a 5-byte `E9 rel32` at
/// `near` can reach it (rel32 tops out at ±2 GB; VirtualAlloc's default placement can land further).
/// Walks 64 KB-granular candidates outward from `near` until one commits.
///
/// # Safety
/// Returns executable memory; the caller must only jump to well-formed code within it.
unsafe fn alloc_exec_near(near: usize, bytes: &[u8]) -> usize {
    const GRAN: usize = 0x1_0000; // Windows allocation granularity
    const SPAN: usize = 0x4000_0000; // ±1 GB, comfortably inside rel32 reach
    let mut delta = GRAN;
    while delta < SPAN {
        for cand in [near.wrapping_sub(delta), near.wrapping_add(delta)] {
            let base = cand & !(GRAN - 1);
            let p = unsafe {
                VirtualAlloc(Some(base as *const core::ffi::c_void), bytes.len(), MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
            };
            if !p.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
                    let _ = FlushInstructionCache(GetCurrentProcess(), Some(p as *const core::ffi::c_void), bytes.len());
                }
                return p as usize;
            }
        }
        delta += GRAN;
    }
    panic!("no executable allocation within ±1GB of {near:#x}");
}

/// A 14-byte absolute indirect jump `jmp qword ptr [rip+0]; <target>` — the exact shape ilhook's
/// `Retn` detour writes (what our retired overlay hook used); has unlimited reach (unlike `E9`). Used
/// two ways here: by the second hooker's trampoline (jump back to `entry+5`) and by the repro's near
/// gateway (bridge the 5-byte `E9` at the entry to the far detour in the exe).
fn abs_jmp(target: usize) -> [u8; 14] {
    let mut b = [0u8; 14];
    b[0] = 0xff;
    b[1] = 0x25; // jmp qword ptr [rip+0]
    b[6..14].copy_from_slice(&(target as u64).to_le_bytes());
    b
}

/// A 5-byte relative jump `E9 rel32` from `at` to `target` — the classic hot-patch convention the
/// second hooker (and our stand-in inline detour) use.
fn rel32_jmp(at: usize, target: usize) -> [u8; 5] {
    let rel = i32::try_from((target as i64) - (at as i64 + 5)).expect("E9 target out of rel32 range");
    let mut b = [0u8; 5];
    b[0] = 0xe9;
    b[1..5].copy_from_slice(&rel.to_le_bytes());
    b
}

// ---- the second (5-byte) hooker, shared by both phases -------------------------------------------

/// Simulate a well-behaved 5-byte inline hooker (the Steam-overlay convention) on `XInputGetState`,
/// installed **first** so it captures the *pristine* 5-byte prologue — exactly the order that made the
/// wild crash (docs/OVERLAY-RENDERING.md > "WER Verdict"). Builds a trampoline `[prologue][jmp
/// entry+5]` (its "call original" path) and lays `E9`→trampoline over the entry. Returns the
/// trampoline address.
///
/// # Safety
/// `entry` must be the live `XInputGetState` code entry.
unsafe fn lay_second_hooker(entry: usize) -> usize {
    let orig5 = unsafe { read_bytes(entry, 5) };
    // The trampoline runs these 5 copied bytes then jumps to entry+5, which is only correct if they are
    // a whole number of complete, position-independent instructions (the standard 5-byte hot-patch
    // prologue). XInputGetState ships the canonical `mov [rsp+8], rbx` (48 89 5c 24 08), and Steam's
    // overlay does this same 5-byte copy in the wild, so it is copy-safe on shipping builds. If a future
    // Windows build changes the prologue, warn loudly: a split instruction would make BOTH phases fault
    // inside the trampoline (not at entry+5), and in the survival phase that would misread as "the IAT
    // fix failed" rather than "this box's prologue isn't 5-byte-copy-safe".
    if orig5 != [0x48, 0x89, 0x5c, 0x24, 0x08] {
        log::warn!(
            "xinput: [second hooker] prologue [{}] != expected `mov [rsp+8],rbx` (48 89 5c 24 08) — the 5-byte-copy trampoline assumes a clean instruction boundary at +5; if this build splits an instruction, results below are UNTRUSTWORTHY (a trampoline-site fault, not the entry+5 collision)",
            hex(&orig5)
        );
    }
    let mut tramp = Vec::with_capacity(5 + 14);
    tramp.extend_from_slice(&orig5);
    tramp.extend_from_slice(&abs_jmp(entry + 5));
    let tramp_addr = unsafe { alloc_exec_near(entry, &tramp) };
    log::info!(
        "xinput: [second hooker] saved pristine 5-byte prologue [{}]; trampoline @ {tramp_addr:#x} = prologue + jmp XInputGetState+5",
        hex(&orig5)
    );
    unsafe { write_code(entry, &rel32_jmp(entry, tramp_addr)) };
    log::info!(
        "xinput: [second hooker] laid E9 over XInputGetState entry {entry:#x} -> trampoline (5-byte-jmp convention, installed first)"
    );
    tramp_addr
}

// ---- resolution ---------------------------------------------------------------------------------

/// Resolve the live `XInputGetState` entry and log its module base + RVAs, so the crashdump's
/// `XINPUT1_4.dll+0x<rva>` fault line can be matched against `XInputGetState+5` by eye.
///
/// # Safety
/// Loads xinput1_4.dll and reads its export; standard loader calls.
unsafe fn resolve_entry() -> usize {
    let module = unsafe { LoadLibraryA(s!("xinput1_4.dll")) }.expect("LoadLibrary xinput1_4.dll");
    let proc = unsafe { GetProcAddress(module, s!("XInputGetState")) }.expect("GetProcAddress XInputGetState");
    let entry = proc as usize;
    let base = unsafe { GetModuleHandleA(s!("xinput1_4.dll")) }.map(|m| m.0 as usize).unwrap_or(0);
    log::info!(
        "xinput: XInputGetState @ {entry:#x} | XINPUT1_4.dll base {base:#x} | RVA {:#x} | entry+5 RVA {:#x} (WER P8 was 0x9a65)",
        entry.wrapping_sub(base),
        (entry + 5).wrapping_sub(base)
    );
    log::info!("xinput: XInputGetState first 16 bytes [{}]", hex(&unsafe { read_bytes(entry, 16) }));
    entry
}

// ---- repro phase --------------------------------------------------------------------------------

/// Our retired overlay input detour, reached via the 5-byte `E9` our inline patch writes at the entry.
/// Mirrors the shape of the old inline hook: do our work, then chain to "the original" — which, because
/// the second hooker got there first, is that hooker's trampoline. Its blind `jmp entry+5` lands in the
/// tail of *our* patch ([`FAULT_SLED`]) → AV at `entry+5`.
///
/// # Safety
/// Invoked only as the target of the inline patch on `XInputGetState`; matches its calling convention.
unsafe extern "system" fn our_inline_detour(user_index: u32, state: *mut XInputStateRaw) -> u32 {
    // FFI firewall (release is panic=unwind; mirrors input.rs): a Rust panic across this `extern`
    // boundary is UB/abort — and in the repro that abort would fire *before* the intended AV at
    // entry+5, corrupting the very result this phase exists to capture. Only the `log::` calls can
    // panic; the chain call itself AVs (an SEH fault, not caught here → reaches crashdump as intended).
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        log::info!("xinput: [our inline detour] invoked; chaining to the original (second hooker's trampoline)");
        log::logger().flush();
        let chain: XInputGetStateFn = std::mem::transmute(CHAIN.load(Ordering::SeqCst));
        chain(user_index, state)
    }))
    .unwrap_or(0)
}

/// # Safety
/// Patches the live `XInputGetState` entry; run once, single-threaded (the harness is).
unsafe fn run_repro() {
    let entry = unsafe { resolve_entry() };
    log::info!("xinput: === REPRO phase: reproduce the inline-hook collision (expect AV at +5) ===");

    // 1) Second (5-byte) hooker first — captures the pristine prologue.
    let tramp = unsafe { lay_second_hooker(entry) };
    CHAIN.store(tramp, Ordering::SeqCst);

    // Sanity: the second hook ALONE is harmless (entry+5 still pristine). A direct poll must return
    // normally (no controller → ERROR_DEVICE_NOT_CONNECTED = 1167). Proves the collision needs *our*
    // patch on top, not the second hooker by itself.
    let get: XInputGetStateFn = unsafe { std::mem::transmute(entry) };
    let mut st = XInputStateRaw::default();
    let rc = unsafe { get(0, &mut st) };
    log::info!("xinput: [sanity] poll through the second hook alone returned {rc} (no AV — entry+5 still pristine)");

    // 2) Our inline patch ON TOP: E9→our detour at entry+0, fault sled at entry+5. This stands in for
    // ilhook's 14-byte abs-jmp; the load-bearing property (patch extends past entry+5) is preserved,
    // so the second hooker's trampoline now jumps into the middle of our patch.
    //
    // A raw 5-byte `E9` can't reach our detour: the exe (~0x1_4000_0000) is hundreds of GB from a
    // system DLL under ASLR, far past rel32's ±2 GB. So the E9 targets a NEAR gateway (allocated
    // beside the entry) that abs-jumps to the far detour — exactly how real inline hookers bridge a
    // 5-byte hot-patch to a distant detour. ilhook's `Retn` does this with its own 14-byte abs-jmp;
    // we split it so `entry+5` can hold a deterministic fault instead of ASLR-random address bytes.
    let gateway = unsafe { alloc_exec_near(entry, &abs_jmp(our_inline_detour as *const () as usize)) };
    let mut patch = Vec::with_capacity(5 + FAULT_SLED.len());
    patch.extend_from_slice(&rel32_jmp(entry, gateway));
    patch.extend_from_slice(&FAULT_SLED);
    unsafe { write_code(entry, &patch) };
    log::info!(
        "xinput: [our inline hook] patched entry {entry:#x} on top (E9->detour + fault sled at +5); bytes now [{}]",
        hex(&unsafe { read_bytes(entry, 16) })
    );

    // 3) Poll. entry+0 → our E9 → detour → chain → second hooker's trampoline → prologue → jmp entry+5
    //    → fault sled → ACCESS_VIOLATION at XInputGetState+5, logged by crate::crashdump.
    log::error!("xinput: polling XInputGetState now — EXPECT ACCESS_VIOLATION at XInputGetState+5 ({:#x})", entry + 5);
    log::logger().flush();
    let rc = unsafe { get(0, &mut st) };

    // Unreachable if the collision reproduced (the poll AVs). Reaching here means the box did NOT
    // reproduce it — a real, logged outcome (e.g. a decode of the sled that didn't fault).
    log::error!("xinput: REPRO did NOT crash (poll returned {rc}) — collision not reproduced on this box");
}

// ---- IAT (survival) phase -----------------------------------------------------------------------

/// Our IAT replacement for `XInputGetState` — mirrors `input.rs::xinput_get_state_iat`: observe, then
/// chain to the recorded original slot value (the real function, on which the second hooker's `E9`
/// sits). Because the function *body* is never patched by us, `entry+5` stays pristine and the second
/// hooker's trampoline is safe → no AV.
///
/// # Safety
/// Invoked only through the harness's swapped IAT slot; matches `XInputGetState`'s convention.
unsafe extern "system" fn our_iat_replacement(user_index: u32, state: *mut XInputStateRaw) -> u32 {
    // FFI firewall, same rationale as `our_inline_detour` (release is panic=unwind; mirrors input.rs).
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let n = IAT_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        let raw = REAL_IAT.load(Ordering::SeqCst);
        debug_assert!(raw != 0, "IAT replacement installed before the original was recorded");
        let real: XInputGetStateFn = std::mem::transmute(raw);
        let rc = real(user_index, state);
        if n <= 3 {
            log::info!("xinput: [our IAT hook] call #{n} chained to the real XInputGetState -> {rc} (no AV; body untouched)");
            log::logger().flush();
        }
        rc
    }))
    .unwrap_or(0)
}

/// Swap the harness exe's own IAT slot for `xinput1_4!XInputGetState` → `new`, recording the prior
/// value as the chain target. Mirrors `input.rs::install_xinput` + `swap_iat_slot`, but against our own
/// import table instead of the game's. Returns `false` if the import isn't found.
///
/// # Safety
/// Writes our own IAT (loader data); run once, single-threaded.
unsafe fn swap_our_iat(new: usize) -> bool {
    // The harness imports XInputGetState by *name* (raw-dylib), so its own IAT entry is `ByName` and the
    // `ByOrdinal` arm below is dead *for this binary*; it exists only to mirror how the game imports
    // XInput (by ordinal 2 — input.rs::install_xinput), keeping the two IAT walks recognizably parallel.
    const XINPUT_GET_STATE_ORDINAL: u16 = 2;

    let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }.expect("GetModuleHandle(self)");
    let base = hmod.0 as *const u8;
    let view = unsafe { PeView::module(base) };
    let imports = match view.imports() {
        Ok(i) => i,
        Err(e) => {
            log::error!("xinput: harness has no import directory: {e}");
            return false;
        }
    };
    for desc in imports {
        let is_xinput = desc
            .dll_name()
            .ok()
            .and_then(|n| n.to_str().ok())
            .is_some_and(|s| s.eq_ignore_ascii_case("xinput1_4.dll"));
        if !is_xinput {
            continue;
        }
        let (Ok(int), Ok(iat)) = (desc.int(), desc.iat()) else { continue };
        for (idx, (imp, _)) in int.zip(iat).enumerate() {
            let hit = match imp {
                Ok(Import::ByName { name, .. }) => name.to_str() == Ok("XInputGetState"),
                Ok(Import::ByOrdinal { ord }) => ord == XINPUT_GET_STATE_ORDINAL,
                Err(_) => false,
            };
            if !hit {
                continue;
            }
            let slot = unsafe {
                base.add(desc.image().FirstThunk as usize).cast::<usize>().add(idx) as *mut usize
            };
            let old = unsafe { slot.read() };
            REAL_IAT.store(old, Ordering::SeqCst);
            unsafe {
                let mut prev = PAGE_PROTECTION_FLAGS(0);
                VirtualProtect(slot as *const core::ffi::c_void, size_of::<usize>(), PAGE_READWRITE, &mut prev)
                    .expect("VirtualProtect RW on the harness IAT slot");
                (*(slot as *const std::sync::atomic::AtomicUsize)).store(new, Ordering::SeqCst);
                let mut restored = PAGE_PROTECTION_FLAGS(0);
                let _ = VirtualProtect(slot as *const core::ffi::c_void, size_of::<usize>(), prev, &mut restored);
            }
            log::info!("xinput: [our IAT hook] swapped harness IAT slot {:#x} ({old:#x} -> {new:#x})", slot as usize);
            return true;
        }
    }
    false
}

/// # Safety
/// Patches the live `XInputGetState` entry (the second hooker) + our own IAT; run once.
unsafe fn run_iat() {
    let entry = unsafe { resolve_entry() };
    log::info!("xinput: === IAT (survival) phase: same second hooker, our capture via the IAT (expect NO AV) ===");

    // Same second (5-byte) hooker as the repro — lays E9 + trampoline on the function body.
    let _tramp = unsafe { lay_second_hooker(entry) };

    // Our capture as an IAT hook on the harness's OWN import — NOT an inline patch on the body. This is
    // the shipped fix's approach (input.rs::install_xinput).
    if !unsafe { swap_our_iat(our_iat_replacement as *const () as usize) } {
        log::error!("xinput: could not find XInputGetState in the harness IAT — is the raw-dylib import present? (x86_64-w64-mingw32-objdump -p dx12-harness.exe)");
        return;
    }

    // Poll through the IAT (the raw-dylib import compiles to `call *[__imp_XInputGetState]`). Each call:
    // our IAT hook → real XInputGetState → second hooker's E9 → trampoline → jmp entry+5 (PRISTINE) →
    // real body. No collision, no AV.
    let mut last = 0u32;
    for i in 1..=10u32 {
        let mut st = XInputStateRaw::default();
        last = unsafe { XInputGetState(0, &mut st) };
        if i == 1 || i == 10 {
            log::info!("xinput: [survival] IAT poll #{i} returned {last}");
        }
    }
    log::info!(
        "xinput: IAT (survival) phase SURVIVED 10 polls (last rc {last}) with the second hooker present — no AV. The IAT hook composes with the inline hooker by construction."
    );
}
