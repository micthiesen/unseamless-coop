---
name: test-loop
description: How to test/verify the unseamless-coop mod — the layered test loops, when to use each, and how to drive them. Use when verifying a change, reproducing a bug, choosing a test strategy, or building out the next loop. Covers the host harness (no game), the local PC rig (scripts/rig.sh — backup/apply/restore + launch/log), the live-mod debug bridge (harness bridge-host), and real co-op. TRIGGER on "test the mod", "verify this", "how do I check X works", "run the harness", "deploy/apply/restore the mod", "set up the rig".
---

# Test loops for unseamless-coop

Testing this mod is layered, because most of it can't run on the dev Mac (no game) and full co-op
can't be automated at all (it needs two real Elden Ring instances in a real Steam session). The
layers form a pyramid: fast/cheap/narrow at the bottom, slow/real/broad at the top. Use the
lowest layer that can answer your question.

| # | Loop | Where | Verifies | Who drives | Status |
|---|------|-------|----------|-----------|--------|
| 1 | Unit tests | Mac | pure core logic | assistant | **DONE** |
| 2 | Two-peer harness | Mac, no game | side-channel coordination + convergence under loss | assistant, fast | **DONE** |
| 2b | TCP two-process harness | Mac, no game | the same logic over real sockets (host half of L3) | assistant | **DONE** |
| 3 | Debug bridge | one game + harness | the side-channel `Session` against the live mod | assistant | **DONE** (`rig.sh` + `harness bridge-host`) |
| 4 | Local PC rig | this machine | game binding (load/register/observe/stability) | assistant (solo subset) | **tooling DONE** (`scripts/rig.sh`); first run pending |
| 5 | Real co-op | friends | actual co-op behavior | you, manual | ongoing; logs handed back |

**The hard limit:** no harness can join the *real game's* Steam P2P session without reimplementing
Elden Ring's encrypted netcode + matchmaking. So we never mock "a second player talking to the
game over Steam." Instead we test *our* logic over a stub transport (layer 2), and test the
*binding* against a real game separately (layers 3-5). Player/world sync is the game's job and is
RE-gated — see `docs/RIG-RUNBOOK.md`.

---

## Layer 1 — Unit tests (DONE)

```bash
scripts/test-core.sh            # runs unseamless-core's tests on the host triple
```
Pure logic: config parsing, scaling math, settings/menu models, the wire protocol (incl. hostile
input), diagnostics, notifications, util, and the peer coordination logic. First thing to run for
any core change.

## Layer 2 — Two-peer harness (DONE) — the fast loop the assistant drives

```bash
scripts/harness.sh [scenario]   # handshake | version-mismatch | config-sync | session-action |
                                # log-forward | lossy | all (default)
```
Wires a host + client (both running the real `unseamless_core::peer::Peer`) over an in-memory
`Loopback` transport (`unseamless_core::transport`) and prints what happened. No game, no Steam,
runs on the Mac in milliseconds.

**Covers:** the version handshake, host→client `ConfigSync` convergence (`SharedSettings::apply_to`),
session-action authorization by **sender role** (`SessionAction::is_host_only`), client→host log
forwarding into the `LogBundle`, and — the `lossy` scenario — **convergence over a dropping/
reordering channel**. The side-channel is designed to self-heal regardless of the transport's
delivery guarantees (the game's P2P broadcast may drop/duplicate/reorder): the host re-asserts a
versioned `ConfigSync` each `maintain()` tick, and actions/logs dedup by per-sender sequence. The
`lossy` scenario drops 85% of frames and still converges via re-assertion.

**Fault injection:** `Loopback::mesh_with_faults(ids, FaultModel{drop_rate, duplicate_rate,
reorder}, seed)` gives a reproducible adversarial channel. The unit suite uses it to prove
convergence under 60% loss and under drop+duplicate+reorder together; a failing soak replays from
its seed.

**Does NOT cover:** the game's own player/world sync, anything that reads/writes live game state,
or whether `broadcast_packet` actually carries our bytes. Those are layers 3-5.

**Adding a scenario:** add a `fn scenario_x()` in `crates/harness/src/main.rs` and an entry in the
`scenarios` table. Add a matching `#[test]` in `crates/unseamless-core/src/peer.rs` so the behavior
is also guarded by the unit suite (the harness is for *driving/observing*, the tests for *pinning*).

## Layer 2b — TCP two-process harness (DONE) — real sockets, no game

```bash
scripts/harness-tcp.sh [port]   # spawns tcp-listen (host) + tcp-connect (client) over localhost
```
The same `Peer`/`Session` logic, but the two peers are **separate OS processes** talking over a
localhost TCP socket (`crates/harness/src/tcp.rs`, `TcpTransport`). This is a higher-fidelity rung
than `Loopback`: real serialization, a real socket, cross-process concurrency, and partial
reads/writes (length-prefixed framing with a `MAX_FRAME` cap). The client exits non-zero if it
never syncs the host's config, so the script is a CI gate, not just a demo.

It is also the **host half of the layer-3 debug bridge**: swap `TcpTransport` for one that speaks to
a debug listener inside the live mod and the same scenarios drive a real game (see Layer 3).

---

## Layer 3 — Debug bridge (DONE) — the side-channel against the live mod

Lets the harness act as a second mod-peer to **one** running game, exercising the real `Session`
side-channel (handshake, `ConfigSync`, actions, log-forward) against the live mod — no second game,
no Steam. It proves the side-channel runs in-process before we bind it to the game's P2P.

**How to drive it:**
```bash
scripts/rig.sh apply && scripts/rig.sh launch --wait   # --wait blocks until the framework is up
scripts/harness.sh bridge-host 47700                   # acts as host, pushes a config into the mod
```
The mod runs as the **client**; `bridge-host` is the authoritative host and pushes a config (it sets
`max_players=4`). The mod applies the received `ConfigSync` into its live config (`coop/state.rs`),
and on the game thread `session-limit` re-applies the override — visible in the **mod's** log as
`session player limit override set to 4` (the seed is 6, so 4 can only come from the pushed sync).
That's a received side-channel message changing **live game state**, solo. (Wine maps the in-game
listener to host loopback, so the native harness reaches it.)

**Shape:** `coop/bridge.rs` (behind the `bridge` cargo feature + `[debug] bridge_port`) binds a
loopback listener and runs a `Session<BridgeTransport>` as a client on a background thread, applying
any received config into the process-global live config (`coop/state.rs`, a `Mutex<Config>`) that the
game-thread features read. `BridgeTransport` is the socket I/O; the wire framing is the shared
host-tested `unseamless_core::framing` codec. The only cross-thread state is that `Mutex`. The
`bridge` feature is `compile_error!`-guarded out of release builds, binds `127.0.0.1` only, and is
off unless `bridge_port > 0`.

**Limits:** drives config-shaped effects (the session-limit override) but not yet richer ones (a
synced `ConfigSync` re-scaling params, an action firing a game call) — those land as more apply
features do — nor the game's own P2P player sync (still needs two real games, layer 5).

## Layer 4 — Local PC rig (`scripts/rig.sh`) — the game-binding loop

This is the "Linux + Proton rig" from `docs/DEVELOPMENT.md` / `RIG-RUNBOOK.md`, and it's now **this
gaming PC** (which both builds and runs the game — the Mac/PC split collapses when you're working
here). `scripts/rig.sh` is the driver; it builds, installs, launches, and reads logs, and — the
load-bearing part — does it **without destroying the machine's real ERSC + Elden Mod Loader + own-mods
setup**.

**The safety model.** The game folder normally runs the *real* mod stack (Seamless Co-op's
`ersc.dll` via an ersc-launcher copy at `start_protected_game.exe`, Elden Mod Loader as `dinput8.dll`,
the user's own DLL mods in `mods/`). Testing unseamless-coop means standing in for all of that, so:

- **`backup`** snapshots that original stack **once**, to `~/.local/share/unseamless-coop/rig-backup/`
  (outside the game folder, safe from Steam "verify integrity" and game updates). It's idempotent and
  guarded: it refuses to re-snapshot over an existing one, and refuses to snapshot at all if our mod
  is already installed (detected via the install marker) so it can never record *our* DLL as "the
  original".
- **`apply`** installs our `dinput8.dll` + `start_protected_game.exe` + a seed config over the stack.
  **Safe to run as often as you like; it never restores.** Auto-snapshots first if needed.
- **`restore`** is **explicit only** — the one command that puts the original stack back. Nothing
  auto-reverts. (This is the user's rule: apply freely, restore only when told.)

**Commands** (env overrides: `GAME_DIR`, `BACKUP_DIR`, `APPID`):
- `rig.sh status` — snapshot state, what's installed (ours vs original), latest log.
- `rig.sh apply [--release] [--no-build] [--with-mods a,b] [--keep-config]` — build (default
  **`diag`**: symbols + debug-assertions for readable panic backtraces) and install. `--with-mods`
  pulls named mods out of the snapshot to test the parent-loader; default leaves `mods/` empty so the
  observer log is unambiguous (`no extra mods …`).
- `rig.sh launch [--wait]` — `steam -applaunch 1245620` (uses the configured gamescope launch options;
  our launcher sets `UNSEAMLESS_LAUNCH`, so the EAC guard passes and the game starts outside EAC).
  **`--wait` blocks until the framework comes up and prints the install lines** — use it instead of
  hand-rolling a log-poll loop.
- `rig.sh log [-f]` — print/follow the latest `unseamless-coop/logs/unseamless_coop-*.log`.
- `rig.sh kill` — stops the game **and** the launcher, escalating SIGTERM→SIGKILL and verifying (Wine
  ignores SIGTERM, so this is reliable — don't add your own `pkill -9`).
- `rig.sh cycle [apply-opts]` — apply → launch → wait for the install/heartbeat lines. The solo
  smoke test in one shot.
- `rig.sh seed-save [src-ext]` — copy a real save into the rig's isolated test extension (default
  `co2` → the configured `file_extension`, e.g. `uco`) so you can test on a real character. Backs up
  the existing test save first; never touches the source or the vanilla `.sl2`; game must be closed.
  **Not a per-run step** — the test save already on disk is usually fine, so only run this on the
  *initial* apply on a fresh rig, or when you deliberately want to reset/refresh the test character.
  Day-to-day `apply`/`launch`/`cycle` leave it alone.
- `rig.sh restore` — roll back to the original stack (explicit).

The seed config (`scripts/rig/seed-config.toml`) sets `[debug] enabled = true` so the run captures
verbose lines; otherwise the CLAUDE.md logging rule keeps them silent. (`scripts/deploy.sh` is the
bare install primitive `rig.sh apply` is built on — kept for the Mac-builds-elsewhere handoff in
RIG-RUNBOOK; on this machine prefer `rig.sh`.)

**Solo-verifiable here (assistant drives end to end):** the DLL loads, registers its feature task,
fires per frame (the `FrameBegin` heartbeat ticks even at the title screen), writes + reads config,
runs the session observer, and stays stable — i.e. the RIG-RUNBOOK "first rig" checklist. **Not**
solo-verifiable: anything needing a loaded save / co-op session — those need layer 5. Handoff is the
log file.

## Layer 5 — Real co-op (ongoing, manual)

Two or more real players. Can't be automated. To make it useful to the assistant afterward: set
`[debug] enabled = true` and (on clients) `forward_to_host = true`, so the host machine aggregates
everyone's logs into one `LogBundle`; then hand over the host's `unseamless-coop/logs/` folder. The
self-describing `RunInfo` header (version, role, session id, config) lets the assistant reconstruct
the session without context. This is the acceptance loop and the only one that proves real co-op.

---

## Picking a loop

- Changed pure logic (config/scaling/protocol/peer)? → **layer 1**, then **layer 2** if it touches
  the side-channel flow.
- Changed the side-channel coordination (handshake/sync/actions/forwarding)? → **layer 2**.
- Need to drive the side-channel against the **live mod** (no second game)? → **layer 3**
  (`rig.sh` + `harness bridge-host`).
- Need to know it actually affects the game (params, session state, loading)? → **layer 4**
  (`rig.sh`, this machine).
- Co-op behavior with real partners? → **layer 5**.

What's left: the side-channel now runs live (layer 3 done), but it doesn't yet drive game *effects*
— wiring a received `ConfigSync`/`SessionAction` to real game calls is the apply-layer, and binding
the side-channel to the game's own P2P (the `GameTransport` over `broadcast_packet`/`receive_packet`)
is the step that replaces the bridge for real multiplayer. Both are the next build-outs.
