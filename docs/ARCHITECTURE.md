# Architecture

The design of the rewrite. This is the holistic view a step-by-step build wouldn't force us to
write down; it's also where the load-bearing decisions live. Read [FEATURES.md](FEATURES.md)
for *what* we're reproducing and [DEVELOPMENT.md](DEVELOPMENT.md) for the toolchain.

## Shape: a workspace split by verifiability

```
unseamless-core   (lib, pure Rust, NO game/OS deps)   -> host-testable on macOS
unseamless-coop   (cdylib, binds core to the game via fromsoftware-rs)
```

The split is deliberate and is the main rearchitecture vs ERSC's single C++ DLL: **push every
decision that can be expressed without the game into `unseamless-core`**, where `cargo test`
runs natively on the dev Mac. Config parsing, scaling math, the session/sync state model, and
(later) protocol message types all live there with real unit tests. The cdylib stays a thin,
mostly-mechanical binding layer: read SDK singletons, call core, write back.

Why it matters here specifically: we develop on a machine that can't run the game, so the more
logic is host-testable, the more of the project is *verified* rather than *hoped*. The cdylib's
correctness still needs the rig; the core's doesn't.

## Runtime spine (cdylib)

`DllMain` (attach only) → init thread → `app::install`:
1. `CSTaskImp::wait_for_instance` (off the main thread).
2. Load `Config` from `SeamlessCoop/unseamless_coop.ini` (writes defaults if absent).
3. Build the `Vec<Box<dyn Feature>>` and register each as a recurring task in its `phase()`.

A [`Feature`] is one unit of behavior with a `name`, a `phase` (`CSTaskGroupIndex`), and
`on_frame`. Features sit behind a single global `Mutex<App>`; each registered task locks it and
ticks one feature. Tasks run on the game's main thread, so the lock is effectively uncontended —
it exists to satisfy the scheduler's `Fn + 'static` bounds, not for real concurrency. The
no-DETACH / `mem::forget(handle)` invariants from er-crit-coop carry over unchanged (see
CLAUDE.md > "safety invariants").

This gives clean, independent feature modules instead of one monolith, and lets each feature run
in the frame phase ordered against the state it touches.

## The two layers

**Layer 1 — host-charted, buildable now.** Features whose game effect is a typed SDK
read/write: scaling (params via `SoloParamRepository::get_mut`), splash-skip, summons, event
flags, world time. The SDK *is* the contract; we build them on the Mac and batch-verify on the
rig. Risk is bounded and per-feature.

**Layer 2 — RE-gated, needs the rig.** The co-op core: relaxing session limits, persisting
sessions across area transitions, getting players into one another's worlds, and state sync.
We can't write this blind — not for lack of tools, but because we must *observe* how the game's
session machine behaves. The [session observer](../crates/unseamless-coop/src/features/observer.rs)
is Layer 2's first step: it logs the state machine live so the rig run produces the spec.

## Key decision: we drive the game's networking, not our own

The SDK inventory was decisive. The game already has a full P2P stack: `NetworkSessionVmt`
exposes `broadcast_packet` / `receive_packet` / `kick` / `remote_identity`; `CSSessionManager`
holds the lobby/protocol FSM, the player roster, the session player limit, and an AES
cipher; there are dedicated `TaskLineIdx_FrpgNet_*` and `NetFlushSendData` task phases.

So ERSC almost certainly does **not** implement its own transport — it **drives the game's
existing session/matchmaking** with the restrictions relaxed and the session made persistent.
We follow the same model:

- **Transport = the game's** (Steam P2P, already encrypted). We do not reinvent it.
- **What the mod adds:** relax `session_player_limit` and area-boundary teardown; keep sessions
  alive across map transitions; coordinate mod state (config sync, session actions like
  open/lock/break-in) over a **small mod-specific side-channel** — our own packet type(s) sent
  via `broadcast_packet` and read in a `receive_packet` task.
- **Interop scope:** because base networking is the game's own, vanilla multiplayer mechanisms
  come along for free. We make **no attempt** to interoperate with *vanilla ERSC's* side-channel
  packet format — every player runs *our* mod, so our side-channel is ours to define cleanly.
  (Recorded here so a future change of mind is a conscious one.)

This shrinks the hard RE surface from "an entire netcode" to "how the game's session FSM
behaves and where ERSC relaxes/persists it" — observable on the rig.

## Module map (current + planned)

| Path | Layer | Status |
|---|---|---|
| `unseamless-core/config.rs` | 1 | done, tested |
| `unseamless-core/scaling.rs` | 1 | done, tested |
| `unseamless-core/` sync model + packet types | 2 | planned (host-testable once shape is known) |
| `coop/app.rs`, `feature.rs` | — | done |
| `coop/config.rs` (disk load) | 1 | done |
| `coop/features/observer.rs` | 2 | done (read-only) |
| `coop/features/scaling.rs` (apply) | 1/2 | gated: application mechanism needs rig confirm |
| `coop/net/*` (session relax, side-channel, sync) | 2 | gated on observer findings |

## Where Mac-side work ends

When a piece needs to *observe or affect a live session* to proceed — the scaling application
mechanism, session-limit relaxation, the side-channel, sync — it's Layer 2 and moves to the rig.
The handoff artifact is the observer log; the plan for producing it is [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
