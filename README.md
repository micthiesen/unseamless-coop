# unseamless-coop

A from-scratch **Rust rewrite of the Elden Ring [Seamless Co-op](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
mod** (ERSC), built on the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.

The point of the rewrite is to turn ERSC's behavior into something you can read, extend, and
build on in Rust, rather than a single opaque C++ DLL. It reuses the toolchain and runtime
patterns proven in the sibling project [`er-crit-coop`](https://github.com/micthiesen/er-crit-coop).

> Status: **early skeleton.** The DLL currently loads, installs a recurring frame task, and
> logs a heartbeat — proving the harness works. Reverse-engineered ERSC behavior is built out
> from there. Not yet a functional Seamless Co-op replacement.

## Build

Cross-compiles to a Windows DLL — no Windows host needed:

```bash
# needs mingw-w64 (macOS: brew install mingw-w64). The Rust target is pinned in
# rust-toolchain.toml and installed automatically.
cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/unseamless_coop.dll
```

See [`CLAUDE.md`](CLAUDE.md) for architecture, the SDK, safety invariants, and the
build-here / verify-in-game-elsewhere split.

## Safety

Seamless Co-op runs outside EAC, so this is for co-op only. Don't take a modded session onto
the official servers.

## License

MIT — see [LICENSE](LICENSE).
