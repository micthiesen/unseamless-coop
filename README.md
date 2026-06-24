# unseamless-coop

A from-scratch **Rust rewrite of the Elden Ring [Seamless Co-op](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
mod** (ERSC), built on the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.

The point of the rewrite is to turn ERSC's behavior into something you can read, extend, and
build on in Rust, rather than a single opaque C++ DLL. It reuses the toolchain and runtime
patterns proven in the sibling project [`er-crit-coop`](https://github.com/micthiesen/er-crit-coop).

> Status: **framework in place, co-op core pending a play session.** A Cargo workspace with a
> host-tested `unseamless-core` (config parsing + scaling math + the mod side-channel protocol,
> all unit-tested) and the `unseamless-coop` cdylib (loads config, runs `Feature`s as frame
> tasks, ships a read-only session observer). It installs as a self-owned `dinput8.dll` proxy +
> launcher (below) that also loads other DLL mods. The actual co-op layer is
> reverse-engineering-gated and waits on a live observation run, so this is **not yet a functional
> Seamless Co-op replacement.**

## Install

A drop-in, no-installer bundle. From a release zip, copy into your `ELDEN RING/Game/` folder
(next to `eldenring.exe`):

- `dinput8.dll` — the mod itself. The game auto-loads it (it's a proxy for the system `dinput8`),
  so there's **no separate mod loader**. It's also the parent loader: drop other simple DLL mods in
  `mods/` and it loads them too (order them with the `[loader]` list in the generated
  `unseamless-coop/unseamless_coop.toml`).
- `start_protected_game.exe` — our launcher, which **replaces** the game's EasyAntiCheat
  bootstrapper of the same name. Steam's "Play" then starts the game outside EAC with the mod
  loaded. (Same install on Windows, Linux/Proton, and Steam Deck.)
- `mods/` — the (optional) folder other DLL mods go in; the mod creates it on first run if absent,
  and writes its config + logs to a `unseamless-coop/` folder next to the game.

Then just press **Play** in Steam.

### Uninstalling / playing vanilla online

While installed, every launch is modded/no-EAC, so you can't reach the official servers. To go
back to vanilla online, restore the original launcher: Steam → ELDEN RING → Properties →
Installed Files → **Verify integrity of game files** (this re-downloads the real
`start_protected_game.exe`). Delete `dinput8.dll` to fully remove the mod.

### ⚠️ After an ELDEN RING update

A game update can restore the original `start_protected_game.exe` while leaving `dinput8.dll` in
place — which would boot **EAC with a mod present**, risking your account. The mod guards against
this (it refuses to run and closes the game if it wasn't started by our launcher), but you should
still **re-copy the mod files after any update before pressing Play.** Use at your own risk; this
mod is for co-op only and must never touch the official servers.

The guard works off a launch marker the launcher sets. **Never set `UNSEAMLESS_LAUNCH` as a
permanent environment variable** — doing so disarms the guard and would let the game boot under EAC
with the mod loaded. It's meant to be set only per-launch, by our launcher.

## Build & test

Cross-compiles to a Windows DLL — no Windows host needed:

```bash
# needs mingw-w64 (macOS: brew install mingw-w64). The cross target is pinned in
# rust-toolchain.toml and is the default (see .cargo/config.toml).
cargo build --release      # -> target/x86_64-pc-windows-gnu/release/unseamless_coop.dll
                           #    (installed as dinput8.dll) + start_protected_game.exe
scripts/test-core.sh       # run the platform-independent core's unit tests on the host
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design and [`CLAUDE.md`](CLAUDE.md)
for the SDK, safety invariants, and the build-here / verify-in-game-elsewhere split.

## Independent reimplementation

This is a clean reimplementation written against the public `fromsoftware-rs` SDK. It is **not
affiliated with, endorsed by, or derived from the source code of** the original Seamless Co-op
mod, and contains **no upstream code or assets**. Behavior was reimplemented from observation,
not by copying. The upstream mod is referenced locally only for study and is never
redistributed here.

## Safety

unseamless-coop loads outside EAC, so it's for co-op only. Don't take a modded session onto the
official servers. See the post-update warning under [Install](#-after-an-elden-ring-update) — the
mod self-aborts if it wasn't started by our launcher, but re-copy the files after any game update.

## License

MIT — see [LICENSE](LICENSE).
