# FFI Unwind-Safety Audit

This audit enumerates every place where **foreign code calls into our Rust** across a C ABI
(`extern "C"` / `extern "system"` / `extern "win64"`), and checks that a Rust panic raised in our
code can never *unwind across that boundary*. A panic that unwinds out of an `extern` function into
non-Rust frames (the Windows loader, vkd3d, the game's threads, an asm hook trampoline) is undefined
behavior. It gated flipping the shipping profile from `panic = "abort"` to `panic = "unwind"` in
`Cargo.toml` (see that file's `[profile.release]` comment): under `abort` a panic kills the process
before it can unwind, so the boundaries are trivially safe; under `unwind` each one needs a real
`catch_unwind` firewall.

The firewall pattern everywhere is the same one app.rs has always used for the task tick:
`std::panic::catch_unwind(AssertUnwindSafe(|| ...))`, and on `Err` **log + degrade, never re-throw**
across the boundary. `AssertUnwindSafe` is needed because the raw pointers / `&mut` game state these
boundaries carry are not `UnwindSafe`; that's fine — we're not relying on unwind-safety of the
poisoned state, we discard or disable it.

**The recovery branch is part of the boundary.** A firewall's `Err` arm runs in the *same*
foreign-invoked frame, so it must be panic-safe too — anything fallible there (locking the log sinks,
allocating a message, reading torn game state for a diagnostic dump) can raise a *second* panic that
unwinds across the very boundary the catch just contained. Two consequences applied throughout: (1)
where the recovery does real work (app.rs disables the feature *and* dumps live singletons), the whole
branch is wrapped in its own `catch_unwind`; (2) recovery-branch logging goes through
`crate::logger::error_contained`, a one-line `catch_unwind` guard mirroring the panic hook's own
self-protection, so a poisoned/contended log sink can't escalate on a cold error path.

A "boundary" here means *foreign → us*. Calls in the other direction (*us → foreign*: we invoke a
Steam export, `VirtualProtect`, the real `dinput8.dll`, a Steam-supplied release callback) are **not**
unwind boundaries for us: the foreign code on the far side is not going to raise a Rust panic, and our
Rust frames sit *below* the call, not above it. Those are noted where they might be mistaken for
entry points.

## Summary

| Entry point | File | Foreign caller | Firewall |
|---|---|---|---|
| SDK frame task tick | `app.rs` | `CSTaskImp` scheduler (`extern "C"`) | **yes** — pre-existing; now also toasts on disable |
| `DllMain` | `lib.rs` | Windows loader (`extern "system"`) | **added** |
| `dinput8` proxy exports | `proxy.rs` | game / loader (`extern "system"`) | not needed — panic-free by construction |
| DirectInput / XInput / cursor detours | `input.rs` | ilhook trampoline (`extern "win64"`) | **added** (in `install_hook`) |
| `CreateFileW` save-redirect detour | `saves.rs` | ilhook trampoline (`extern "win64"`) | **added** |
| session create/join probe detours | `session_probe.rs` | ilhook trampoline (`extern "win64"`) | **yes** (in `log_initiation`) — gated, currently inert |
| code patch (`apply`) | `patch.rs` | — (not an entry point) | n/a |
| Steam message callbacks / methods | `steam.rs` | — (us → Steam) | n/a |
| overlay `render` | `overlay.rs` | hudhook present hook (`extern "system"`) | **yes** — pre-existing |
| overlay `initialize` / `before_render` / `message_filter` | `overlay.rs` | hudhook present / WndProc hook | **added** |
| co-op driver loop | `coop.rs` | — (our own thread) | n/a (see note) |

Key external finding: **neither hudhook 0.9.1 nor ilhook 2.3.0 wraps our callbacks in a panic
firewall of their own** (grep both crates for `catch_unwind` — zero hits). hudhook's DX12 present hook
`dxgi_swap_chain_present_impl` (`src/hooks/dx12.rs`) calls straight through to our `ImguiRenderLoop`
methods inside an `extern "system"` fn; ilhook's `run_jmp_back_closure` / `run_retn_closure`
(`src/x64.rs`) invoke our detour closures from an `extern "win64"` fn reached by a hand-written asm
trampoline. So every callback we hand either library is our responsibility to firewall.

## Per-entry-point detail

### `app.rs` — SDK frame task tick (already firewalled; recovery hardened)
Each feature is registered with `cs_task.run_recurring(closure, phase)`; the SDK calls that closure
from the game's scheduler across a C boundary every frame. The closure already wraps the per-feature
`tick(index, data)` in `catch_unwind`; on panic it flags the feature disabled (a lock-free atomic in
`FEATURES`, so it's never re-ticked on torn state) and dumps a diagnostic snapshot. This audit:
- added a plain-voice **toast** to that path (via `disable_feature` → `crate::notify`) so the player
  learns a feature went away while the game keeps running;
- **wrapped the entire recovery branch** (`disable_feature` + `diag::dump`) in its own `catch_unwind`.
  This matters because `diag::dump` reads live game singletons through the SDK, and right after a
  feature panic — exactly when state is most likely transitional/torn — those reads can `.expect()`/
  panic on an unwired pointer. Without the wrap, that second panic unwinds across the `extern "C"`
  scheduler boundary (UB). The `panic = "abort"` → `unwind` flip is what turned this latent structure
  into a live path, so the wrap is part of the same change. Recovery logging is `error_contained`.

### `lib.rs` — `DllMain` (firewall added)
The loader calls `DllMain` across `extern "system"`. The EAC guard runs first and only ever *aborts*
(never unwinds), so it stays outside the firewall and keeps its hard guarantee. The remaining work is
`std::thread::spawn(app::install)`, and `thread::spawn` **panics** if the OS refuses to create the
thread — the one realistic panic on this path. It's now wrapped so a spawn failure degrades to
"unmodded but running" instead of unwinding into the loader. (The panic hook isn't installed until
`app::install` runs, so the recovery here is a best-effort `eprintln!`.)

### `proxy.rs` — `dinput8.dll` proxy exports (no firewall needed)
The seven exported `extern "system"` shims (`DirectInput8Create`, `DllCanUnloadNow`, …) are called by
the game and the loader. Each does only: a cached `GetSystemDirectoryW` + `LoadLibraryW` (in a
`OnceLock`), a `GetProcAddress`, and a `transmute` + forward to the real `dinput8.dll`. There is no
`panic!`, `unwrap`, `expect`, slice index, or arithmetic that can overflow-panic anywhere on these
paths. The only theoretical panic is an allocation failure (the small `Vec<u16>` path buffer), which
*aborts* rather than unwinds regardless of profile. So these are **panic-free by construction** and
adding a firewall would be dead code; this is recorded here so the absence reads as a decision, not an
oversight. (The forwarded call *into* the real `dinput8.dll` is us → foreign and not our boundary.)

### `input.rs` — input detours (firewall added)
Four detours run on the game's input thread via ilhook: `get_state_detour` (DirectInput keyboard /
mouse blanking), `xinput_get_state_detour` (controller capture + blanking), `set_cursor_pos_detour`,
and `clip_cursor_detour`. All four are installed through one helper, `install_hook`, so the firewall
lives there once: it wraps the detour in `catch_unwind`, logs at most once per hook (a repeat at
input-poll rate would flood the log), and returns `0` on panic. The bodies are allocation-free with no
`unwrap`/index/`panic!`, and they call `original` *before* any of our own work, so a panic is
effectively unreachable — the firewall is soundness insurance. The `0` recovery value reads as
`DI_OK` / `ERROR_SUCCESS` / `FALSE`, all benign here (input simply isn't blanked that frame). A
stranded "input swallowed" state can't result: the `BLOCKED` flag is owned by the overlay's `render`,
whose own firewall clears it on an overlay panic.

### `saves.rs` — `CreateFileW` save-redirect detour (firewall added)
`create_file_detour` runs on whatever thread opens a file, via ilhook's jmp-back hook. It's
**safety-critical**: its job is to repoint vanilla `*.sl2` opens at the co-op save so the player's
single-player file is never written. The detour stages its `rcx` rewrite **before** its one
remaining fallible step (the once-per-path redirect `log::info!`), and after the genuinely fallible
work that's already handled (`String::from_utf16` is `Result`-checked, the de-dup mutex is
poison-recovered). The ordering is the safety lever: a panic before staging leaves the registers
untouched (original opens the path as given), while a panic during the redirect log leaves `rcx`
already pointing at the co-op buffer (original opens the **co-op** save) — so the reachable failure
mode preserves save isolation rather than dropping the open onto the vanilla `*.sl2` file, which is
the corruption case this feature exists to prevent. (An earlier draft staged the rewrite *last*; that
fails toward the vanilla save on a logging panic, so the order was flipped.) Either way there's never
a half-written redirect. The firewall contains the unwind, logs once via `error_contained`, and
returns; install-*time* failure remains fatal, unchanged. A per-call panic is the believed-unreachable
tail, now made safe in the direction that matters.

### `session_probe.rs` — session create/join probe detours (firewall in `log_initiation`)
Two read-only logging detours on the session create/join initiation functions, installed via ilhook's
jmp-back hook (same boundary class as `saves.rs`), gated behind `[debug.probes] session_probe` and
**currently inert** (both `HookSite` consts are `None` until the AOBs are charted on the rig). When
live they call the shared `log_initiation`, whose body is wrapped in `catch_unwind` for exactly this
reason — the diag build (used on the rig to first fire these) is `panic = "unwind"`, and the callback
runs from ilhook's `extern "win64"` trampoline. The body only reads scalar registers and logs (at
`debug!`, since the dump carries a raw peer SteamID), so a panic is unlikely, but the firewall makes
the "fill two consts and rebuild" path sound by construction rather than relying on a later re-audit.
Added in the parallel rung-3-RE-prep lane; cross-referenced here so the boundary enumeration stays
complete.

### `patch.rs` — in-place code patch (not an entry point)
`apply` / `nop_landmark` patch the live game image once, at install, on our own init thread. Nothing
foreign calls *into* this code; it calls *out* to `VirtualProtect` / `FlushInstructionCache` (us →
OS). A panic here unwinds up our own init thread to the thread root, where `std` catches it at the
thread boundary — not UB. No firewall required.

### `steam.rs` — Steam flat-API binding (not an entry point)
Everything here is us → Steam: we resolve exports with `GetProcAddress` and *call* them
(`SendMessageToUser`, `ReceiveMessagesOnChannel`, the versioned accessors). The
`m_pfnRelease` / `m_pfnFreeData` fields on a received message are function pointers **Steam** hands us
that **we call** to free the message — again us → foreign; our Rust does not execute *inside* them, so
a panic in our surrounding code can't unwind through them. `receive()` runs on the co-op driver thread
(below), not a Steam-invoked callback (we deliberately never pump Steam's callback dispatch — the game
owns it). No FFI-unwind boundary here.

### `overlay.rs` — hudhook render loop (firewall added on the remaining methods)
hudhook drives our `Overlay: ImguiRenderLoop` from its `extern "system"` present hook with no catch of
its own. `render` was already firewalled. This audit added the same firewall to the other three trait
methods we override:
- `initialize` — builds the font atlas once; a panic now disables the overlay for the session instead
  of unwinding into the swapchain init.
- `before_render` — a single `mouse_draw_cursor = false` write; can't realistically panic, firewalled
  for the same soundness reason and to disable on the off chance.
- `message_filter` — only loads the `self.open` bool (can't panic), but hudhook samples it in
  `prepare_render` on the Present thread (the same present-hook `extern "system"` boundary as
  `render`, not a separate WndProc entry); it now catches defensively and defaults to
  `MessageFilter::empty()` (don't filter) so a panic can never strand window input. It also
  early-returns `empty()` when the overlay is disabled, matching the other methods.

`ImguiRenderLoop` also has `before_wnd_proc` / `after_wnd_proc`, which hudhook calls from its WndProc
hook. `Overlay` does **not** override them, so the trait's trivial defaults (a `Continue` / a no-op)
run and can't panic — safe today, but a future override would silently add an unfirewalled boundary,
so it must get the same `catch_unwind` treatment if we ever take over input handling there.

### `coop.rs` — side-channel driver (our own thread, not a boundary)
The co-op `Session` driver runs `run()` in a thread we spawn. A panic there unwinds to the thread root
and `std` contains it at the thread boundary — not UB, though it would silently kill the side-channel.
That's a *robustness* consideration (a future hardening could wrap the driver loop so a transient
panic restarts the link rather than ending it), **not** an FFI-unwind-safety one, so it's out of scope
for this audit and the `panic = "unwind"` flip. Recorded here so it isn't mistaken for an unguarded
boundary.

## Re-checking this after a dependency bump
Both firewall gaps came from the hooking libraries not catching panics themselves. If hudhook or
ilhook is bumped, re-grep the new source for `catch_unwind`: if a future version *does* firewall its
callbacks, our wrappers become redundant (harmless) belt-and-suspenders; if it still doesn't, ours
remain load-bearing. Re-confirm with:

```text
grep -rn catch_unwind ~/.cargo/registry/src/*/hudhook-*/src ~/.cargo/registry/src/*/ilhook-*/src
```
