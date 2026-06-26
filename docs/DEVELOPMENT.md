# Development notes

How this mod is built, run, and verified. It's a Rust rewrite of Seamless Co-op (ERSC) on the
`fromsoftware-rs` SDK, and it inherits its toolchain and runtime patterns from the sibling
project `../er-crit-coop` — read that repo's `docs/DEVELOPMENT.md` for the original, more
detailed write-up of the Linux/Proton tricks.

## Two machines, one workflow

Work is split across two hosts on purpose:

- **macOS laptop (primary dev host).** Edit, cross-compile, `cargo check`/`clippy`, study the
  SDK and the reference `ersc.dll` under `reference/`. The full build toolchain runs here. It
  **cannot run Elden Ring** — and that's fine; nothing in normal development needs to.
- **Linux + Proton rig (async verification).** Deploy the DLL, launch the game, watch the log.
  Done out of band, not from the Mac. `scripts/deploy.sh` and the run/verify loop below target
  this rig.

The handoff between them is the **log**: install line → per-frame heartbeat → effect lines.
Write behavior so it's legible from the log, since that's what the remote verifier sees.

## Toolchain: cross-compiling a Windows DLL

No Windows host needed. The mod is a `cdylib` built for `x86_64-pc-windows-gnu`:

- `rust-toolchain.toml` pins `channel = "stable"` and `targets = ["x86_64-pc-windows-gnu"]`,
  so `rustup` installs the target automatically.
- The GNU target links with **mingw-w64** (`brew install mingw-w64` on macOS,
  `pacman -S mingw-w64-gcc` on Arch); cargo finds `x86_64-w64-mingw32-gcc` on PATH.
- `cargo build --release --target x86_64-pc-windows-gnu` →
  `target/.../release/unseamless_coop.dll`.

Builds are not bit-reproducible across hosts (CI's mingw vs local mingw differ), so a release
`.dll` won't sha-match a local build. They're equivalent; compare `.text` size
(`x86_64-w64-mingw32-objdump -h`) to sanity-check.

## The SDK and the hook design

Built on [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) (`eldenring` +
`fromsoftware-shared`), pinned by commit in `Cargo.toml`. The high-value part: it exposes game
structs as **named typed fields** instead of raw offsets, so most of ERSC's pointer/offset
bookkeeping is already done. Pin both crates to the **same** commit; struct layouts are read
against a specific revision.

The runtime harness (`src/hook.rs`) registers a per-frame task on the game's own scheduler
rather than running a free thread:

```rust
let cs_task = CSTaskImp::wait_for_instance(timeout)?;   // off the main thread
let handle = cs_task.run_recurring(|_: &FD4TaskData| on_frame(), PHASE);
std::mem::forget(handle);                               // registration is permanent
```

`DllMain` only spawns the short init thread (avoids loader-lock issues; `wait_for_instance`
must not run on the main thread). **Phase choice matters**: run in a `CSTaskGroupIndex` phase
ordered against the state you touch — the safety is frame-ordering, not thread exclusivity. The
skeleton heartbeats in `FrameBegin` (ticks even at the title screen, so firing is observable
without a save); real game-state work moves to an appropriate phase as it's built.

## Reverse-engineering ERSC

The upstream mod lives under `reference/seamless-coop-v1.9.9/` (gitignored, not redistributable).
A static triage (`rz-bin -l/-S/-E/-i ersc.dll`) found the decisive fact up front:

**`ersc.dll` is Themida-packed.** ~5.5MB of its ~7.4MB is a single `.themida` section
(`-rwx`, self-modifying); the real code is virtualized and the import table is a stub (only
**8** visible imports, one per linked library). So **static decompilation is mostly a dead
end** — Ghidra/IDA will show you the unpacker and the stub IAT, not the logic. Don't sink time
into "decompile ersc.dll." A disassembler is still worth having for `eldenring.exe`, for any
runtime-unpacked memory dump, for strings, and for checking our own builds.

What the triage *does* tell us (factual metadata, safe to use):
- **Linked libraries** name the architecture: `steam_api64.dll` (Steam P2P), `ws2_32.dll`
  (Winsock), `crypt32`/`wldap32`/`normaliz` (TLS/crypto), plus `user32` (`GetAsyncKeyState` →
  hotkeys). So: Steam-transport networking with a crypto layer.
- **Export `modengine_ext_init`** → it's a ModEngine2 extension (a public, documented load API).
- `ersc_settings.ini` + `english.json` are plain text — the cheapest map of the feature set
  (catalogued in [FEATURES.md](FEATURES.md)).

So the realistic RE path is **behavioral, not static**: observe what it does to game memory,
the network, and save files, and reimplement from the public `fromsoftware-rs` SDK + the ER
modding community's knowledge. This happens to fit the clean-room posture perfectly — you
can't copy code you can't read.

### RE toolchain (all headless / CLI, no GUI)

- **rizin** (`brew install rizin`) — primary static triage and disasm. Headless and scriptable;
  every command has a JSON form for piping to `jq`. Examples:
  ```bash
  rizin -q -c 'iSj' bin | jq -r '.[] | "\(.name)\t\(.size)\t\(.perm)"'   # sections
  rizin -q -c 'ilj' bin                                                  # linked libraries
  rizin -q -c 'iij' bin | jq                                             # imports
  rizin -q -c 'aa; s entry0; pd 20' bin                                  # analyze + disasm
  ```
  This subsumes `pefile`/`capstone` for our purposes, so we don't install those.
- **Ghidra headless** — **optional / not installed by default.** A C decompiler is rarely
  needed here: `ersc.dll` is Themida-packed (undecompilable) and `eldenring.exe` is already
  charted by the `fromsoftware-rs` SDK, so we seldom decompile anything ourselves. If a clean
  target ever needs it (a game function the SDK doesn't name, an unpacked dump), install on
  demand (`brew install --cask ghidra`) and run `scripts/re/ghidra-decompile.sh <binary>
  [function]` — a CLI wrapper around `analyzeHeadless` + the `DumpDecomp.py` Jython post-script
  that prints decompiled C to stdout (no GUI, no MCP). The wrapper is committed and ready; it
  errors cleanly if Ghidra isn't present. A lighter alternative is a rizin decompiler plugin
  (`rz-ghidra`/`jsdec`). Project cache: `.ghidra-projects/` (gitignored).
- **Frida** (dynamic instrumentation) — deferred. It belongs on the Linux + Proton rig where
  the game actually runs (hook/trace at runtime), not on the macOS dev host. Set it up there
  when M2/M3 behavioral work starts.

**Clean-room rule:** never paste decompiler/disassembler output into source or commits, and
never redistribute upstream bytes (`reference/` stays gitignored). Read to understand, record
behavior in your own words, implement from that. See CLAUDE.md > "Clean-room hygiene".

When a behavior isn't a named SDK field, use a diagnostic loop that snapshots candidate byte
regions per `ChrIns` and logs rising-edge bit flips (see `er-crit-coop/src/diagnostic.rs` for
the pattern), then map the located region/offset/bit to a typed SDK field.

## Logging

File logger via `simplelog`/`log`, set up by `logger::init` on the init thread. The DLL runs
inside the game's (Proton) working directory (normally `ELDEN RING/Game/`), so the run log lands
under `unseamless-coop/logs/`; the startup line records the actual cwd (Proton's can differ) and a
panic hook records panics (with `panic = "abort"` the process still exits, but the trace and a
backtrace survive). The self-describing, shareable log model is `unseamless-core/diagnostics.rs`.
Verbosity is `[debug]` config, **off by default** — hot-path logs must use `log::debug!`/`trace!`.

## Run + verify loop (Linux + Proton rig)

> The first-rig-session procedure (deploy → observe the session FSM to unblock the co-op core) is
> **canonical in [RIG-RUNBOOK.md](RIG-RUNBOOK.md)** and wrapped by the `/test-loop` skill. This
> section is the general dev quick-reference for the same rig.

We are our **own** loader and launcher — no Elden Mod Loader, no ERSC launcher. The cdylib ships as
the game's `dinput8.dll` (a proxy the game auto-loads via DLL search order; `src/proxy.rs`), which
on load also acts as the parent loader for other DLL mods in `mods/` (`src/mods.rs`). Our launcher
(`crates/launcher`, shipped as `start_protected_game.exe`) starts `eldenring.exe` directly — outside
EAC — with `UNSEAMLESS_LAUNCH=1` set; the DLL aborts if that marker is absent (`src/guard.rs`), so
a game update that reverts the launcher can't run the mod under anti-cheat. `scripts/rig.sh apply`
installs both (and snapshots the original stack first); `scripts/deploy.sh` is the bare primitive it
wraps, gated behind `UNSEAMLESS_DEPLOY_STANDALONE=1` so it can't clobber a real stack by accident.
Driving launch/observe/kill from the shell:

```bash
steam -applaunch 1245620                 # runs our start_protected_game.exe -> eldenring.exe (no EAC)
# watch ELDEN RING/Game/unseamless-coop/logs/*.log for the install + frame-task lines
pkill -f '[e]ldenring.exe'               # bracket trick avoids matching the pkill itself
```

Gotchas (from the er-crit-coop rig):

- Each run writes a fresh timestamped log under `unseamless-coop/logs/` (old runs are kept, not
  truncated), so "the one from when it broke" survives; no need to `rm` before relaunch.
- `FrameBegin` ticks in menus/title; world-phase tasks (e.g. `WorldChrMan_PostPhysics`) don't
  tick until a save is loaded.
- Solo-verifiable from the title screen: registration, per-frame firing, stability. Anything
  needing a loaded save / co-op session needs real gameplay.

## Releasing

Push a `vX.Y.Z` tag (use the `/release` skill, which bumps `Cargo.toml`, writes notes into the
annotated tag, and pushes). `.github/workflows/release.yml` then cross-compiles the DLL and
publishes a GitHub release with the binary, using the tag message as the notes.
