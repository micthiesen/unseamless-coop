# unseamless-coop

A from-scratch **Rust rewrite of the Elden Ring [Seamless Co-op](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
mod** (ERSC), built on the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.

The point of the rewrite is to turn ERSC's behavior into something you can read, extend, and
build on in Rust, rather than a single opaque C++ DLL. It reuses the toolchain and runtime
patterns proven in the sibling project [`er-crit-coop`](https://github.com/micthiesen/er-crit-coop).

> Status: **framework in place, co-op core pending a play session.** A Cargo workspace with a
> host-tested `unseamless-core` (config parsing + scaling math + the mod side-channel protocol,
> all unit-tested) and the `unseamless-coop` cdylib (loads config, runs `Feature`s as frame
> tasks, ships a read-only session observer). The actual co-op layer is reverse-engineering-gated
> and waits on a live observation run. Not yet a functional Seamless Co-op replacement.

## Build & test

Cross-compiles to a Windows DLL — no Windows host needed:

```bash
# needs mingw-w64 (macOS: brew install mingw-w64). The cross target is pinned in
# rust-toolchain.toml and is the default (see .cargo/config.toml).
cargo build --release      # -> target/x86_64-pc-windows-gnu/release/unseamless_coop.dll
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
official servers.

## License

MIT — see [LICENSE](LICENSE).
