# Code Patching (AOB Scan + Memory Patch)

How features that must rewrite the game's own code locate a byte pattern in the running module and
overwrite it in place. The first consumer is **skip-intros** ([SKIP-INTROS.md](SKIP-INTROS.md)),
which NOPs a conditional branch in the boot/title flow; future consumers include offline-popup
trigger suppression ([OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md), the "suppress the trigger"
path) and any other "the SDK charts no field for this, so we patch the instruction" feature.

This is a **research/design note**, not yet implemented. Everything game-internal below is grounded
in the pinned `fromsoftware-rs` SDK source (cited) or is a behavioral observation to confirm on the
rig. Per [CLAUDE.md](../CLAUDE.md) > clean-room hygiene: we reimplement from the mechanism + the
public SDK, never from ERSC's bytes. The open-source references cited here are permissively licensed
(MIT/Apache) and read for *technique*; the actual byte patterns are version-specific and re-derived
against our rig's game version regardless.

## Why We Need This

The mod's normal lever is a typed SDK field write per frame (scaling, session limit, event flags —
see [SDK-COVERAGE.md](SDK-COVERAGE.md)). A handful of features have **no such field**: the only way
to change the behavior is to find the relevant machine code and edit it (typically NOP out a branch
so the game falls through a gate it would otherwise take). `er-crit-coop`'s `patch.rs` is a
per-frame *field* writer, not a *code* patcher, so this is genuinely new surface for us — but small,
and most of the machinery already ships inside our dependency tree.

## What We Already Have (pelite + the windows crate)

We do **not** need a new dependency. Two pieces are already on the tree:

**1. pelite's pattern scanner (transitive via the SDK).** `pelite` 0.10 (the PE parser the SDK uses
for all its RVA work) ships a masked AOB scanner with a `pattern!` compile-time macro. The SDK
already uses it in `fromsoftware-shared/src/arxan.rs`:

```rust
use pelite::pattern::Atom;
use pelite::pe64::Pe;

const CODE_RESTORATION_PATTERN: &[Atom] =
    pelite::pattern!("B9 ? ? ? ? E8 ? ? ? ? F3 0F 11 05 ? ? ? ? [0-128] ' 72 ? 48 8D ? ? ? ? ?");

let mut matches = program.scanner().matches_code(CODE_RESTORATION_PATTERN);
```

The pattern DSL covers everything an AOB-with-wildcards feature needs:

- `B9` etc. — a literal byte.
- `?` — a single wildcard byte (one nibble `?` is a fuzzy/half-byte match).
- `' ` (a quote) — `Save(n)`: capture the **RVA at the cursor** into the save array. This is how you
  recover "the address of the byte I want to patch", possibly *after* skipping a prefix.
- `[m-n]` — match the rest of the pattern within a window (`Many`), for tolerating small drift.
- jump/pointer follows (`Jump1`, `Jump4`, `Ptr`, `Pir`) — follow a rel8/rel32/abs reference, rarely
  needed for a simple NOP patch but available.

Scanner entry points on anything implementing `pelite::pe64::Pe` (which `Program` does):

- `program.scanner().finds_code(pat, &mut [Rva; N]) -> bool` — find the **first** match in the code
  section, write captures into `save`. This is the one-shot we want.
- `program.scanner().matches_code(pat) -> Matches` — iterator over all matches (what arxan uses);
  call `.next(&mut save)` in a loop.

Both `_code` variants restrict the search to `headers().code_range()` — i.e. the executable
section(s), effectively `.text`. (For an explicit `.text` bound the SDK's `rtti.rs` shows
`program.section_headers().by_name(".text")?.virtual_range()`, but `code_range()` is the idiomatic
shorthand and is what we'll use.)

**2. `Program::current()` (fromsoftware-shared).** `fromsoftware_shared::program::Program::current()`
returns a `Program::Mapping(PeView)` over the **live, in-memory** game image (it builds the `PeView`
from `GetModuleHandleA(NULL)`, the main exe). Critically this is the *mapped* view, so RVAs resolve
to real loaded VAs and the bytes the scanner reads are the bytes actually executing. `Program`
implements `Pe`, so it has `.scanner()` and `.rva_to_va(rva) -> Result<Va>` directly. The SDK's
whole singleton layer (`static.rs`) and `arxan.rs` use exactly this object — we reuse it rather than
re-deriving the module base.

**3. The `windows` crate (already a direct dep).** For flipping page protection and flushing the
i-cache. We just need to enable two more Win32 feature modules (see "Cargo wiring" below).

So the build cost of this whole utility is: zero new crates, two added `windows` feature flags, and
~50 lines of `coop`-side code.

## Applying a Patch Safely (the Windows API flow)

A `.text` page is mapped `PAGE_EXECUTE_READ` — readable and executable, **not** writable. The
canonical four-step in-place code patch (confirmed against `windows` 0.62 signatures):

```rust
use windows::Win32::System::Memory::{
    VirtualProtect, PAGE_PROTECTION_FLAGS, PAGE_EXECUTE_READWRITE,
};
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Threading::GetCurrentProcess;

// addr: *mut u8 into the live image; bytes: the replacement (e.g. &[0x90, 0x90])
unsafe {
    let mut old = PAGE_PROTECTION_FLAGS(0);
    // 1. Make the page writable, remembering the old protection.
    VirtualProtect(addr as *const _, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old)?;
    // 2. Write the new bytes.
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr, bytes.len());
    // 3. Restore the original protection.
    VirtualProtect(addr as *const _, bytes.len(), old, &mut PAGE_PROTECTION_FLAGS(0))?;
    // 4. Flush the instruction cache so the CPU re-fetches the patched bytes.
    FlushInstructionCache(GetCurrentProcess(), Some(addr as *const _), bytes.len())?;
}
```

Notes that matter:

- **`VirtualProtect` operates on whole pages.** Passing a sub-page `(addr, len)` is fine — the OS
  rounds to the containing page(s). A patch that straddles a page boundary is still covered because
  `len` spans into the next page; for a 2-byte NOP this is a non-issue.
- **Restore the original protection** rather than leaving the page RWX. Leaving it writable is a
  smaller anti-cheat / stability surface and matches what the reference mods do. (We run outside EAC,
  so this is hygiene, not an EAC concern.)
- **`FlushInstructionCache` is required by contract**, even though x86 is largely cache-coherent for
  self-modifying code in practice. MSDN: "the calling program bears responsibility for ensuring cache
  coherency via FlushInstructionCache." Cheap insurance; always call it.
- **Order of read-vs-patch.** Run the scan, capture the address, then patch. The scanner reads
  through the same `PeView` (read-only access to mapped memory, no protection change needed for the
  *scan* — only the *write* needs RW).

### Wine / Proton caveats

The game runs as a Windows PE under Wine/Proton on the Linux rig. Relevant points (the first is
fact; the rest are reasoned expectations to confirm on the rig):

- **`VirtualProtect` and `FlushInstructionCache` are implemented in Wine** (`ntdll/virtual.c` /
  the kernel32 forwards) and are exercised by Wine's own conformance tests. The standard
  RW→write→restore→flush flow works under Wine the same as on Windows; this is the exact path every
  Wine-run game trainer/mod uses. No special-casing expected.
- **Page granularity is the same 4 KiB** under Wine on x86-64, so the page-rounding behavior above
  holds.
- **`FlushInstructionCache` may be close to a no-op under Wine** (it maps onto host semantics and
  x86 coherency), but call it anyway — correctness shouldn't depend on the host's leniency, and it
  costs nothing.
- **One thing to actually watch on the rig:** Arxan/"guardIT" code-restoration. Elden Ring ships
  Arxan protection that can *rewrite* tampered `.text` back to the original at runtime. If a NOP
  patch silently reverts mid-session, that's Arxan, and the fix is the SDK's own
  `fromsoftware_shared::arxan` neuter (`get_arxan_code_restoration_rvas` + `disable_code_restoration_at`,
  which itself does a `VirtualProtect`+write). The SDK's guidance is to **prefer the task runtime over
  hooking the image** precisely to avoid this — so a boot-flow gate we patch *once, early, before the
  logo plays* may well land before Arxan re-checks, and may not need the neuter at all. Determine
  empirically on the rig: patch, observe, and only reach for the Arxan neuter if the patch doesn't
  stick. (This is the single biggest unknown for code-patch features and is rig-only.)

## RVA vs AOB vs Hardcoded VA (the policy)

We version-pin hard already — the SDK's `rva.rs` only resolves for ER **2.6.2.0 (WW) / 2.6.2.1 (JP)**
and **panics** otherwise, and we pin the SDK by exact commit. So in principle a per-version hardcoded
RVA would "work." The question is which locator to prefer for *our own* code patches.

**Recommendation: AOB-scan, capturing the patch site, even though we version-pin.** Reasoning:

- **A hardcoded VA/RVA and an AOB are equally brittle on a *major* game update** — both break, and
  both demand a human RE pass to re-derive. The pin doesn't save us there; nothing does.
- **But an AOB is strictly more robust to *minor* drift.** A small game patch (or our own SDK-pin
  bump to an adjacent rev) can shift code by a few bytes without changing the surrounding
  instructions; an AOB keyed on the instruction *shape* still finds it, a hardcoded offset silently
  points at the wrong byte and corrupts code. For a self-modifying code patch, "silently points at the
  wrong byte" is the worst failure mode — far worse than "didn't find it."
- **An AOB fails *loud and safe*: no match → we don't patch, we log + toast, the game runs unmodded.**
  A stale hardcoded offset fails *silent and dangerous*: we patch garbage. Given our error policy
  (degrade-and-notify for anything past install — [CLAUDE.md](../CLAUDE.md) > "Surfacing errors"), a
  scanner's "Option::None means skip the feature" maps onto that policy perfectly.
- **It's the same machinery the SDK already trusts** (arxan), and the same approach veeenu's
  practice tool uses to *generate* its version offsets. We get to skip the offset-codegen step and
  just scan at install.
- **Cost is negligible:** one `finds_code` over the code section at install time, once. Not a
  hot path.

The pin still pulls its weight: it guarantees the *struct layouts and singleton RVAs* the rest of the
mod reads are correct, and it means our AOB only has to be valid for one known game build (so we can
write a tight, specific pattern rather than a fuzzy cross-version one). AOB + a hard version pin is
belt-and-suspenders: the pin keeps the SDK honest, the AOB keeps our code patches self-locating and
fail-safe.

> Escape hatch: if a particular site genuinely can't be expressed as a stable AOB (too generic a
> byte sequence, multiple false matches), fall back to a **version-gated RVA constant** for *that*
> site, guarded by the SDK's already-resolved game-version check so it can only apply on the build it
> was derived for. Treat that as the exception, document the derivation, and re-verify on every pin
> bump.

## Lifetime & Safety (cross-ref CLAUDE.md invariants)

A code patch is **applied once, at install, on the init thread** — and never undone. This mirrors the
task-handle invariants, for the same underlying reason:

- **Not in `DllMain`.** `DllMain` only does `DLL_PROCESS_ATTACH` (logging + spawn the init thread)
  and returns off the loader lock. The patch runs inside `app::install` on the init thread, *after*
  the module is mapped and (for boot-flow patches) before the gated code executes. Doing memory work
  under the loader lock is the same deadlock/reentrancy hazard the task system already avoids.
- **Apply once, leave it.** Not a recurring task — a single `apply` call at install behind the
  feature's setting (e.g. `skip_splash_screens`). The patch is permanent for the process lifetime.
- **No `DLL_PROCESS_DETACH` restore.** This is the direct analogue of `std::mem::forget`-ing the task
  handle. We never reverse a patch on unload: the DLL stays resident (it must — registered tasks hold
  pointers into our image), so there is no unload path to restore from. Restoring a patch as the
  image tears down is as dangerous as freeing an image a still-registered task points into — don't.
  The "restore original bytes on `Drop`" pattern you see in `erfps2`'s FMG override
  ([OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md)) is for a *reversible data override during a
  live session*, not for a permanent boot-flow code patch — different lifetime, different rule.
- **Timing window vs. the frame model.** Field writes are safe because they run in a chosen task
  *phase* ordered against the game's reads/writes ([CLAUDE.md](../CLAUDE.md) > safety invariants). A
  one-shot boot patch isn't phased the same way; its safety comes from running *before the patched
  code path is first taken* (the logo gate hasn't fired yet at install) and from the patch being a
  self-contained instruction rewrite, not a cross-thread state mutation. If a future code patch
  targets a hot path the game is *already* executing, that one must be reasoned about per-site (worst
  case: do the write inside a task phase, or accept a benign one-frame race for an idempotent NOP).
- **`unsafe` is real and localized.** Raw pointer write into executable memory. Keep it inside the
  `patch` module behind a small safe-ish API, document the invariants once, and don't sprinkle raw
  `VirtualProtect` calls across features.

## Proposed API

A new `coop` module — `crates/unseamless-coop/src/patch.rs` (sibling of `sdk.rs`). Pure binding
glue, so it lives in the cdylib crate, not `unseamless-core` (which has no OS deps). Minimal shape:

```rust
//! AOB scan + in-place code patch over the live game image. Used by features that must rewrite
//! the game's machine code (e.g. skip-intros NOPs a boot-flow branch) where the SDK charts no
//! field. Apply once at install on the init thread; patches are permanent (no restore on unload).

use fromsoftware_shared::program::Program;
use pelite::pattern::Atom;
use pelite::pe64::Pe;

/// Find the first match of `pattern` in the game's code section, returning a live pointer to the
/// byte captured by the pattern's first `Save` slot (the `'` in the AOB), or `None` if no match.
///
/// Build `pattern` with `pelite::pattern!("..")`. Put a `'` (Save 0) at the byte you intend to
/// patch. `None` means "not found" — the caller skips the feature and notifies; it never patches
/// a guessed address.
pub fn scan(pattern: &[Atom]) -> Option<*mut u8> {
    let program = Program::current();
    let mut save = [0u32; 1]; // RVA of the Save(0) capture
    if !program.scanner().finds_code(pattern, &mut save) {
        return None;
    }
    program.rva_to_va(save[0]).ok().map(|va| va as *mut u8)
}

/// Overwrite `bytes.len()` bytes at `addr` in the live image: VirtualProtect → write → restore →
/// FlushInstructionCache. Returns the bytes that were there before (for logging/diagnostics; we do
/// NOT keep them to restore — patches are permanent, see the module docs).
///
/// # Safety
/// `addr` must point at `bytes.len()` valid bytes inside the loaded game image (use [`scan`] to
/// obtain it). Must run on the init thread at install, before the patched code path first executes.
pub unsafe fn apply(addr: *mut u8, bytes: &[u8]) -> Result<Vec<u8>, windows::core::Error> {
    use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows::Win32::System::Memory::{
        PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS, VirtualProtect,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let len = bytes.len();
    let original = unsafe { std::slice::from_raw_parts(addr, len).to_vec() };
    unsafe {
        let mut old = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(addr.cast(), len, PAGE_EXECUTE_READWRITE, &mut old)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr, len);
        let mut _restored = PAGE_PROTECTION_FLAGS(0);
        VirtualProtect(addr.cast(), len, old, &mut _restored)?;
        FlushInstructionCache(GetCurrentProcess(), Some(addr.cast()), len)?;
    }
    Ok(original)
}

/// Convenience: scan for `pattern`, NOP `count` bytes at the captured site. The common case
/// (overwrite a short conditional jump with 0x90s). Logs and returns `false` on no-match.
pub fn nop_at(name: &str, pattern: &[Atom], count: usize) -> bool {
    match scan(pattern) {
        Some(addr) => {
            let nops = vec![0x90u8; count];
            match unsafe { apply(addr, &nops) } {
                Ok(orig) => { log::info!("patched '{name}': {orig:02X?} -> NOP×{count}"); true }
                Err(e) => { log::error!("patch '{name}' write failed: {e}"); false }
            }
        }
        None => { log::warn!("patch '{name}': pattern not found; feature disabled this session"); false }
    }
}
```

### Intended call shape (skip-intros)

skip-intros is **not a `Feature`/task** — it's a one-shot in `install`, gated on its setting:

```rust
// in app::install, after config load + the password guard, before the task-system wait
if config.gameplay.skip_splash_screens {
    // Scan a distinctive landmark; the logo gate is a `74` (JZ) a fixed 60 bytes before it. NOP the
    // 2-byte jump. Adapted from the MIT techiew/SkipTheIntro signature, rig-confirmed on our pin.
    const BOOT_LOGO_LANDMARK: &[Atom] =
        pelite::pattern!("C6 ? ? ? ? ? 01 ? 03 00 00 00 ? 8B ? E8 ? ? ? ? E9 ? ? ? ? ? 8D");
    crate::patch::nop_landmark("skip_splash_screens", BOOT_LOGO_LANDMARK, -60, 0x74, 2);
}
```

That's the whole integration: a `skip_splash_screens` bool in the settings registry
([settings.rs](../crates/unseamless-core/src/settings.rs)) drives one `nop_landmark` call. No new
feature-trait plumbing, no task phase, no teardown.

> **Shipped API note.** The implementation replaced the proposed `scan` + `nop_at` with a single
> `nop_landmark(name, landmark, offset, expect, count)`: it scans a *unique* landmark (pelite's
> `finds_code` fails on zero *or* multiple matches), steps `offset` bytes to the patch site **in RVA
> space** (so `rva_to_va` bounds-checks the site against the mapped image rather than offsetting a raw
> pointer), verifies the byte equals `expect` (drift guard), then NOPs `count` bytes — exactly the
> techiew "landmark minus a fixed offset" shape. `apply` is unchanged. Future code-patch features
> (offline-popup suppression) reuse `nop_landmark` / `apply`.

### Cargo wiring (one small change)

`crates/unseamless-coop/Cargo.toml` already depends on `windows` 0.62, `fromsoftware-shared`, and
(transitively) `pelite`. Add `pelite` as a **direct** dep pinned to the SDK's version so we can name
`pelite::pattern!`/`pelite::pattern::Atom` (matching the lockfile's `pelite = 0.10.0`), and enable
two more `windows` features:

```toml
# add to [dependencies]
pelite = "0.10"

# add to the windows features list
"Win32_System_Memory",              # patch: VirtualProtect / PAGE_PROTECTION_FLAGS
"Win32_System_Diagnostics_Debug",   # patch: FlushInstructionCache
# (Win32_System_Threading is already enabled — GetCurrentProcess lives there)
```

(If we'd rather not add `pelite` directly, the SDK re-exports enough through
`fromsoftware_shared` for `Program`, but `pattern!`/`Atom` come from `pelite` itself, so a direct
dep is the clean way to write patterns. Keep it pinned to the same `0.10` the SDK resolves to.)

## Status / Next Steps

- [x] Land `crates/unseamless-coop/src/patch.rs` with `apply` / `nop_landmark` (host-compiles).
- [x] Add the `pelite` direct dep + the two `windows` features to the coop crate's `Cargo.toml`.
- [x] Wire the one-shot `nop_landmark` call into `app::install` behind the existing
      `gameplay.skip_splash_screens` setting (default flipped to on).
- [x] **Rig:** real boot-flow AOB confirmed against our game version; logos skip to the title screen.
- [x] **Rig:** the patch *sticks* through the logo sequence (Arxan did not revert it before the gate
      fires; the early one-shot lands first). The `fromsoftware_shared::arxan` neuter stays unused.
- [x] **Rig:** `VirtualProtect`/`FlushInstructionCache` behave under our Proton build (implicit — the
      patch applied and took effect).
- [ ] Decide per future feature (offline-popup suppression) whether it's a code patch via this util
      or a session-FSM/field write — see [OFFLINE-TITLE-SCREEN.md](OFFLINE-TITLE-SCREEN.md).

## Sources

- Pinned SDK `fromsoftware-rs` rev `8c67a84` (read directly):
  - `crates/shared/src/arxan.rs` — the canonical in-repo AOB-scan + code-patch example
    (`pelite::pattern!`, `program.scanner().matches_code`, `rva_to_va`, the VirtualProtect-then-write
    note, and the Arxan code-restoration neuter).
  - `crates/shared/src/program.rs` — `Program::current()` over the live mapped image (`PeView` from
    `GetModuleHandleA(NULL)`); implements `pelite::pe64::Pe`.
  - `crates/shared/src/rtti.rs` — `section_headers().by_name(".text")` / `virtual_range()` section
    enumeration via pelite.
  - `crates/eldenring/src/rva.rs` + `rva/bundle.rs` — version detection/pinning (ER 2.6.2.0 WW /
    2.6.2.1 JP); the "panics on unsupported version" behavior.
- `pelite` 0.10.0 (transitive dep; read locally) — `src/pattern.rs` (`Atom` enum: `Byte`, `Save`,
  `Skip`, `Many`, `Fuzzy`, `Jump1/4`, `Ptr`, …) and `src/pe64/scanner.rs` (`finds_code`,
  `matches_code`, `Matches::next`, `code_range()` restriction).
  <https://docs.rs/pelite/0.10.0/pelite/>
- [`windows` crate 0.62 docs](https://microsoft.github.io/windows-docs-rs/) —
  `Win32::System::Memory::VirtualProtect` (sig: `(*const c_void, usize, PAGE_PROTECTION_FLAGS,
  *mut PAGE_PROTECTION_FLAGS) -> Result<()>`),
  `Win32::System::Diagnostics::Debug::FlushInstructionCache` (sig: `(HANDLE, Option<*const c_void>,
  usize) -> Result<()>`), `Win32::System::Threading::GetCurrentProcess`.
- [MSDN — VirtualProtect](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualprotect)
  — page-granularity behavior and the FlushInstructionCache cache-coherency requirement.
- [veeenu/eldenring-practice-tool](https://github.com/veeenu/eldenring-practice-tool) — Rust ER tool;
  AOB-scans to *generate* version-specific base offsets, then runs off hardcoded offsets (the
  scan-once-then-offsets pattern; we scan-at-install instead). Built on
  [hudhook](https://github.com/veeenu/hudhook).
- [techiew/EldenRingMods — SkipTheIntro](https://github.com/techiew/EldenRingMods/blob/master/SkipTheIntro/DllMain.cpp)
  (MIT) — the C++ AOB+NOP boot-flow reference that skip-intros reimplements.
- [Dasaav-dsv/erfps2 `src/tutorial.rs`](https://github.com/Dasaav-dsv/erfps2/blob/main/src/tutorial.rs)
  (Apache-2.0) — reversible runtime override with `Drop`-restore (contrast: that's a session data
  override, not a permanent code patch).
- [Wine `ntdll/virtual.c`](https://github.com/wine-mirror/wine/blob/master/dlls/kernel32/tests/virtual.c)
  — Wine's `VirtualProtect`/memory-protection implementation + conformance tests (the flow works
  under Wine).
</content>
</invoke>
