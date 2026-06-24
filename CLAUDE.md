# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A from-scratch **Rust rewrite of the Elden Ring Seamless Co-op mod** (ERSC, originally a C++
DLL). The goal is to reverse-engineer ERSC's behavior and re-implement it on top of the
[`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK so it can be built on and
extended in Rust rather than patched as opaque C++.

The proven scaffolding, toolchain, and runtime patterns come from the sibling project
**`../er-crit-coop`** (same author, same SDK, a single small DLL mod). When in doubt about
how to build, structure, load, or safely hook the game, read that repo first — its
`docs/DEVELOPMENT.md` and `src/patch.rs` module docs are the reference for everything below.

> Status: **early skeleton.** `Cargo.toml`, `rust-toolchain.toml`, `src/` (`lib.rs` +
> `logger.rs` + `hook.rs`), CI, and `scripts/` are scaffolded from `er-crit-coop`. The DLL
> loads, installs a recurring frame task, and heartbeats — the harness is proven. ERSC
> behavior is built out from `hook::on_frame`.

## Where things run (read this first)

Development happens on a **macOS** laptop that **cannot run Elden Ring**. That's fine and
expected — the workflow is deliberately split:

- **On this Mac (the dev host):** edit code, cross-compile the DLL, run `cargo check`/`clippy`,
  reason about the SDK and the reference `ersc.dll`. The full build toolchain works here
  (`brew install mingw-w64` + the pinned cross target). **Everything in normal development is
  doable here.**
- **On a separate Linux + Proton rig (async, not this machine):** deploy the DLL, launch the
  game, and watch the log to verify behavior. This is done **out of band** — do not expect to
  launch the game or read a live log from the Mac. `scripts/deploy.sh` and the run/verify loop
  below describe that rig, not this one.

So: never block on "let me run the game to check." Build, commit, and push from the Mac; the
in-game verification happens separately and asynchronously. The log-line contract (install →
heartbeat → effect lines) is the handoff between the two — write code so its behavior is
legible from the log, since that's all the remote verifier sees.

## Build

A Windows DLL (`cdylib`) cross-compiled to `x86_64-pc-windows-gnu` — no Windows host needed.

```bash
cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/<crate>.dll
```

- The cross target is pinned in `rust-toolchain.toml` (`channel = "stable"`,
  `targets = ["x86_64-pc-windows-gnu"]`) so `rustup` installs it automatically.
- The GNU target links with **mingw-w64**. Install it (`brew install mingw-w64` on macOS,
  `pacman -S mingw-w64-gcc` on Arch); cargo finds `x86_64-w64-mingw32-gcc` on PATH.
- Release profile mirrors `er-crit-coop`: `panic = "abort"`, `opt-level = "z"`,
  `codegen-units = 1`, `lto = true`, `strip = true`.
- Builds are **not** bit-reproducible across hosts (CI mingw vs local mingw differ); the
  release `.dll` won't sha-match a local build. Compare `.text` size with
  `x86_64-w64-mingw32-objdump -h` to sanity-check equivalence.

Building works on this Mac; running the game does not (see "Where things run"). The
run/verify loop below is the separate Linux + Proton rig.

## The SDK (the "pointer mappings" library)

Built on `fromsoftware-rs` (`eldenring` + `fromsoftware-shared` crates), **pinned by exact
git commit** in `Cargo.toml`. This is the high-value part for a rewrite: it exposes game
structs as **named typed fields** rather than raw offsets, so most of ERSC's offset/pointer
bookkeeping is already done.

- Always pin `eldenring` and `fromsoftware-shared` to the **same commit** — struct layouts
  are read against a specific revision; mixing revisions is a silent UB hazard. When bumping
  the pin, re-verify any field accesses against the new layouts.
- Entry points seen in `er-crit-coop`: `WorldChrMan::instance()` →
  `open_field_chr_set.base.characters()` to iterate map enemies as `&mut ChrIns`; typed
  module access like `chr.modules.action_flag...`. Prefer named SDK fields over offsets; only
  fall back to raw byte reads for investigation (see the diagnostic pattern below).

## Architecture & hard safety invariants

These are load-bearing rules learned in `er-crit-coop`, not style preferences. Violating them
is a use-after-free or a data race in someone's game.

- **`DllMain` handles only `DLL_PROCESS_ATTACH`.** It inits logging and spawns a short-lived
  init thread, then returns. Do real work off the loader lock and off the main thread —
  `CSTaskImp::wait_for_instance` blocks on main-thread init and will deadlock if called on it.
- **Hook the game by registering a recurring task on its own scheduler**, not a free-running
  background thread:
  ```rust
  let cs_task = CSTaskImp::wait_for_instance(timeout)?;   // on the init thread, NOT main
  let handle = cs_task.run_recurring(|_: &FD4TaskData| on_frame(), PHASE);
  std::mem::forget(handle);                               // registration is permanent
  ```
- **`std::mem::forget` the task handle.** The SDK never unregisters; its `cancel()` is a
  no-op stub and the task self-references. Dropping the handle (or adding a
  `DLL_PROCESS_DETACH` cleanup) frees an image the still-registered task points into →
  use-after-free. The DLL must stay resident for the process lifetime.
- **Safety is frame-ordering, not thread exclusivity.** Pick the task phase
  (`CSTaskGroupIndex`) so your code runs after the game writes the state you read and before
  the game consumes it. `WorldChrMan_PostPhysics` is the worked example (after behavior
  update, before `DmgMan`). Running in step with the frame is what removes the cross-thread
  race — there are no pointer guards or atomics on the game state for this reason.
- **`characters()` yields entries regardless of `chr_load_status`**, so across a
  loading/fast-travel transition you can touch a mid-init/teardown `ChrIns` with unwired
  module pointers. A `PostPhysics`-style phase keeps the window small; the fully robust form
  iterates ChrSet entries and skips any whose status isn't `Active` before dereferencing.

## Logging

File logger via `simplelog`/`log`, initialized in `DllMain`. The DLL runs inside the game's
(Proton) working directory, normally `ELDEN RING/Game/`, so logs land there. Log the actual
cwd at startup (Proton's cwd can differ) and install a panic hook that records panics — with
`panic = "abort"` the process still exits, but the trace survives.

## Reverse-engineering unknown state (diagnostic pattern)

When a behavior isn't a named SDK field, don't hand-diff memory dumps. `er-crit-coop`'s
`src/diagnostic.rs` is the template: a background loop that snapshots candidate byte regions
per `ChrIns` each tick and logs **rising edges** (0→1) of individual bits, suppressing
high-churn bits as noise. Trigger the behavior in-game and the responsible
region/offset/bit names itself; then map it to a typed SDK field. Keep such modes behind a
compile-time `MODE` switch (`Patch` vs `Diagnostic`) so they ship dormant, never as the
default.

## Loading & the run/verify loop (Linux + Proton)

Single `.dll` dropped in `ELDEN RING/Game/mods/`, loaded by **Elden Mod Loader**
(`DINPUT8.dll`); coexists with Seamless Co-op via the exe-swap launch. No ModEngine/me3.
`scripts/deploy.sh` copies the built DLL there.

Driving launch/observe/kill from the shell (from `er-crit-coop/docs/DEVELOPMENT.md`):

- `steam -applaunch 1245620` launches with the ERSC exe-swap so the mod loads.
- Watch `ELDEN RING/Game/<crate>.log` for the install line, then a per-frame heartbeat, then
  effect lines (effect lines need real gameplay).
- Kill with the bracket trick: `pkill -f '[e]ldenring.exe'` (plain `pkill -f eldenring.exe`
  matches its own command line — false positive).
- The log is truncated on DLL load (`File::create`); `rm` it before relaunch so a match means
  a fresh load.
- `WorldChrMan_PostPhysics` doesn't tick at the title screen (no world). To prove a task
  *fires* without loading a save, temporarily switch its phase to
  `CSTaskGroupIndex::FrameBegin` (ticks in menus) and watch the heartbeat, then switch back.
- Solo-verifiable: registration, per-frame firing, stability. **Not** solo-verifiable:
  anything needing a loaded save / co-op session — those require an in-game retest.

## Releases

Tag-driven, modeled on `er-crit-coop/.github/workflows/release.yml`: pushing a `vX.Y.Z` tag
cross-compiles the DLL and publishes a GitHub release with the binary, using the annotated
tag message as the notes. The `er-crit-coop` repo has a `/release` skill that bumps
`Cargo.toml`, writes the notes into the tag, and pushes — replicate it here once CI exists.

## Safety / legitimacy

Seamless Co-op runs **outside EAC**, so these mods are for co-op only. Never take a modded
session onto the official servers. The mod must not touch `regulation.bin` (so it can't block
players from connecting).
