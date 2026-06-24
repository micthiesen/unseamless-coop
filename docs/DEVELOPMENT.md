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

**Clean-room rule:** never paste decompiler/disassembler output into source or commits, and
never redistribute upstream bytes (`reference/` stays gitignored). Read to understand, record
behavior in your own words, implement from that. See CLAUDE.md > "Clean-room hygiene".

When a behavior isn't a named SDK field, use a diagnostic loop that snapshots candidate byte
regions per `ChrIns` and logs rising-edge bit flips (see `er-crit-coop/src/diagnostic.rs` for
the pattern), then map the located region/offset/bit to a typed SDK field.

## Run + verify loop (Linux + Proton rig)

Single `.dll` dropped in `ELDEN RING/Game/mods/`, loaded by **Elden Mod Loader**
(`DINPUT8.dll`); coexists with Seamless Co-op via the exe-swap launch. `scripts/deploy.sh`
copies the built DLL there. Driving launch/observe/kill from the shell:

```bash
steam -applaunch 1245620                 # launches with the ersc exe-swap so the mod loads
# watch ELDEN RING/Game/unseamless_coop.log for: "hook installed" -> "frame task live"
pkill -f '[e]ldenring.exe'               # bracket trick avoids matching the pkill itself
```

Gotchas (from the er-crit-coop rig):

- The log is truncated on DLL load (`File::create`); `rm` it before relaunch so a match means a
  fresh load.
- `FrameBegin` ticks in menus/title; world-phase tasks (e.g. `WorldChrMan_PostPhysics`) don't
  tick until a save is loaded.
- Solo-verifiable from the title screen: registration, per-frame firing, stability. Anything
  needing a loaded save / co-op session needs real gameplay.

## Releasing

Push a `vX.Y.Z` tag (use the `/release` skill, which bumps `Cargo.toml`, writes notes into the
annotated tag, and pushes). `.github/workflows/release.yml` then cross-compiles the DLL and
publishes a GitHub release with the binary, using the tag message as the notes.
