# unseamless-coop

> ⚠️ **Not ready for use.** This is a work in progress: the framework is in place but the actual
> co-op layer isn't wired up yet, so it does **not** connect you to friends. Don't install it
> expecting a working Seamless Co-op replacement. See the status note below.

A from-scratch **Rust rewrite of the Elden Ring [Seamless Co-op](https://github.com/LukeYui/EldenRingSeamlessCoopRelease)
mod** (ERSC), built on the [`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK.

The point of the rewrite is to turn Seamless Co-op's behavior into something you can read,
extend, and build on in Rust. It reuses the toolchain and runtime patterns proven in the sibling
project [`er-crit-coop`](https://github.com/micthiesen/er-crit-coop).

> Status: **framework in place, co-op core pending a play session.** A Cargo workspace with a
> host-tested `unseamless-core` (config parsing + scaling math + the mod side-channel protocol,
> all unit-tested) and the `unseamless-coop` cdylib (loads config, runs `Feature`s as frame
> tasks, ships a read-only session observer). It installs as a self-owned `dinput8.dll` proxy +
> launcher (below) that also loads other DLL mods. The actual co-op layer is
> reverse-engineering-gated and waits on a live observation run, so this is **not yet a functional
> Seamless Co-op replacement.**

## Install & Play

> Heads up: **co-op itself isn't wired up yet** (see the status note above). Today this installs the
> framework, boots outside EAC, and creates/validates your config — it does **not** connect you to
> friends. The flow below is the intended end-to-end experience and how to set up for it now.

A drop-in, no-installer bundle. Getting from download to a co-op session with friends:

1. **Get the files.** From a [release](../../releases), extract the zip's contents into your
   `ELDEN RING/Game/` folder — the one next to `eldenring.exe`. (To find it: in Steam, right-click
   **ELDEN RING → Manage → Browse local files**, then open the `Game` folder.) You're adding
   `dinput8.dll`, `start_protected_game.exe`, and a `mods/` folder — see
   [What's in the bundle](#whats-in-the-bundle).

2. **Launch once.** Press **Play** in Steam. The mod boots outside EasyAntiCheat and, on this first
   run, writes its config — including a **random co-op password** — to
   `ELDEN RING/Game/unseamless-coop/unseamless_coop.toml`.

3. **Match passwords with your group.** Co-op pairs players by a **shared password**: everyone must
   use the *same* one. The easy path is to use the generated default — one person opens their config,
   copies the `password = "…"` value, and everyone else pastes it into theirs. (You can change it to
   any shared phrase instead; it just has to match across the group, and must be **at least 5
   characters** — the mod won't launch with an empty or too-short password.) Save and relaunch.

4. **Play.** Host or join as usual — anyone running the mod with the same password joins your session.

> Want to set the password (or anything else) *before* your first session? Launch once, quit at the
> title screen, edit the config, then relaunch.

This is the same install on Windows, Linux/Proton, and Steam Deck.

### What's in the Bundle

- `dinput8.dll` — the mod itself. The game auto-loads it (it's a proxy for the system `dinput8`),
  so there's **no separate mod loader**. It's also the parent loader: other DLL mods you drop in
  `mods/` are loaded too.
- `start_protected_game.exe` — the launcher, which **replaces** the game's EasyAntiCheat
  bootstrapper of the same name, so Steam's "Play" starts the game outside EAC with the mod loaded.
- `mods/` — the (optional) folder other DLL mods go in. A broken or incompatible DLL here can stop
  the game from launching — if that happens, remove it.

### Configuring

Settings live at `ELDEN RING/Game/unseamless-coop/unseamless_coop.toml` (logs sit beside it under
`unseamless-coop/logs/`). It's created with sensible defaults on first launch; edit it and relaunch
to apply changes.

**Who sets what** (once co-op is wired up):

- **Everyone must match** — `[session] password`, the matchmaking key. It has to be identical for
  the whole party (≥ 5 characters).
- **The host decides for everyone** — the host's values for these are pushed to the party and
  override each client's own:
  - `[scaling]` — per-extra-player enemy/boss health, damage, and posture percentages.
  - the `[gameplay]` *rules*: `allow_invaders`, `allow_summons`, `death_debuffs`.
- **Per-player** — each person sets their own; not shared:
  - the rest of `[gameplay]` — overhead display, skip splash screens, append steam id,
    spectate-on-death, boot volume.
  - `[loader]` (your own `mods/`), `[debug]` (your logging), `[save]` (save-file extension),
    `[language]` (locale).

So in practice: everyone agrees on the password, the host dials in the difficulty/rules, and
everything else is personal preference.

The config isn't part of the install bundle, so re-copying the mod after a game update never
overwrites your settings.

### Uninstalling / Playing Vanilla Online

While installed, every launch is modded/no-EAC, so you can't reach the official servers. To go
back to vanilla online, restore the original launcher: Steam → ELDEN RING → Properties →
Installed Files → **Verify integrity of game files** (this re-downloads the real
`start_protected_game.exe`). Delete `dinput8.dll` to fully remove the mod.

### After an ELDEN RING Update

A game update can restore the original `start_protected_game.exe` while leaving `dinput8.dll` in
place — which would boot **EAC with a mod present**, risking your account. The mod guards against
this common case (it refuses to run and closes the game if it wasn't started by our launcher), but
you should still **re-copy the mod files after any update before pressing Play.** Use at your own risk; this
mod is for co-op only and must never touch the official servers.

The guard works off a launch marker the launcher sets. **Never set `UNSEAMLESS_LAUNCH` as a
permanent environment variable** — doing so disarms the guard and would let the game boot under EAC
with the mod loaded. It's meant to be set only per-launch, by our launcher.

## Build & Test

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

## Independent Reimplementation

Seamless Co-op by [LukeYui](https://github.com/LukeYui/EldenRingSeamlessCoopRelease) is the
original that made co-op ELDEN RING what it is, and the inspiration for this project. This is an
independent, from-scratch reimplementation built to hack on that behavior in Rust; full credit to
the original authors for the design it learns from.

This is a clean reimplementation written against the public `fromsoftware-rs` SDK. It is **not
affiliated with, endorsed by, or derived from the source code of** the original Seamless Co-op
mod, and contains **no upstream code or assets**. Behavior was reimplemented from observation,
not by copying. The upstream mod is referenced locally only for study and is never
redistributed here.

## Safety

unseamless-coop loads outside EAC, so it's for co-op only. Don't take a modded session onto the
official servers. See the [post-update warning](#after-an-elden-ring-update) — the mod self-aborts
if it wasn't started by our launcher, but re-copy the files after any game update.

## License

MIT — see [LICENSE](LICENSE).
