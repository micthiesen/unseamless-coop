# unseamless-coop

> ⚠️ **Not ready for use — framework in place, co-op core pending a play session.** A Cargo
> workspace with a host-tested `unseamless-core` (config parsing + scaling math + the mod
> side-channel protocol, all unit-tested) and the `unseamless-coop` cdylib (loads config, runs
> `Feature`s as frame tasks, ships a read-only session observer). It installs as a self-owned
> `dinput8.dll` proxy + launcher that also loads other DLL mods. The actual co-op layer is
> reverse-engineering-gated and waits on a live observation run, so it does **not** connect you to
> friends yet — don't install it expecting a working Seamless Co-op replacement.

A from-scratch **Rust rewrite of the Elden Ring [Seamless Co-op](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
mod** (ERSC), built on the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.

The point of the rewrite is to turn Seamless Co-op's behavior into something you can read,
extend, and build on in Rust. It reuses the toolchain and runtime patterns proven in the sibling
project [`er-crit-coop`](https://github.com/micthiesen/er-crit-coop).

## Install & Play

A drop-in, no-installer bundle — same on Windows, Linux/Proton, and Steam Deck. The short version:

1. **Get the files.** Extract a [release](../../releases) zip into your `ELDEN RING/Game/` folder
   (next to `eldenring.exe`). You're adding `dinput8.dll`, `start_protected_game.exe`, and a `mods/`
   folder.
2. **Launch once.** Press **Play** in Steam. The mod boots outside EasyAntiCheat and writes its
   config — including a **random co-op password** — under `Game/unseamless-coop/`.
3. **Match passwords with your group.** Co-op pairs players by a shared password (≥ 5 characters):
   everyone uses the *same* one. Save and relaunch.
4. **Play.** Anyone running the mod with the same password joins your session.

See **[docs/USAGE.md](docs/USAGE.md)** for the full walkthrough: what's in the bundle, configuring
(who sets what), uninstalling / playing vanilla online, and what to do after an ELDEN RING update.

> Note: co-op itself isn't wired up yet (see the status note above), so today this installs the
> framework, boots outside EAC, and creates/validates your config — it does **not** connect you to
> friends. The flow above is the intended end-to-end experience and how to set up for it now.

## Build & Test

Cross-compiles to a Windows DLL — no Windows host needed:

```bash
# needs mingw-w64 (pacman -S mingw-w64-gcc on Arch). The cross target is pinned in
# rust-toolchain.toml and is the default (see .cargo/config.toml).
cargo build --release      # -> target/x86_64-pc-windows-gnu/release/unseamless_coop.dll
                           #    (installed as dinput8.dll) + start_protected_game.exe
scripts/test-core.sh       # run the platform-independent core's unit tests on the host
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the design and [`CLAUDE.md`](CLAUDE.md)
for the SDK, safety invariants, and the build-and-verify-in-game workflow.

## Independent Reimplementation

Seamless Co-op by [LukeYui](https://github.com/LukeYui/EldenRingSeamlessCoopRelease) is the
original that made co-op ELDEN RING what it is, and the inspiration for this project. This is an
independent, from-scratch reimplementation written against the public `fromsoftware-rs` SDK to hack
on that behavior in Rust. It is **not affiliated with, endorsed by, or derived from the source code
of** the original mod, and contains **no upstream code or assets** — behavior was reimplemented from
observation, not by copying. Full credit to the original authors for the design it learns from.

## Safety

unseamless-coop loads outside EAC, so it's for co-op only. Don't take a modded session onto the
official servers. The mod self-aborts if it wasn't started by our launcher, but **re-copy the files
after any game update** before pressing Play — see [After an ELDEN RING
Update](docs/USAGE.md#after-an-elden-ring-update).

## License

MIT — see [LICENSE](LICENSE).
