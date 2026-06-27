# In-Overlay Rig-Testing Guides

A **guide** is an ordered series of on-screen test steps that walks a tester through a rig run
without the orchestrator having to round-trip "do X, tell me the result, now do Y" over the wire.
The current step shows as a pinned banner until its finish signal fires; the engine then advances
(optionally branching on a log/state result), and the guide ends with a hardcoded "done testing"
toast. One shared guide runs on every machine; each shows only the steps tagged for its role, so
two-player testing is "set a role, both follow the same guide."

This is the design + how it's wired. **Authoring** a guide (the API you actually write) is its own
short skill: `.claude/skills/rig-guides/SKILL.md`. The engine is host-tested in
`crates/unseamless-core/src/guide.rs`; the committed guides are in `…/guide/guides.rs`; the
game-side binding is `crates/unseamless-coop/src/features/rig_guide.rs` + `coop/overlay.rs`.

## The point: log/state auto-finish, not manual relay

The reason this system exists is to **stop the orchestrator's "do X, read me the result, now do Y"
loop**, so the load-bearing design rule is: a step should **self-detect its completion from the run log
or live state**, not wait for the tester to confirm it. Author every step you can with a `done_when(...)`
predicate (`log_contains(...)` / `lobby_is` / `protocol_is` / `players_at_least` / `game_state_is` /
`after_secs`, composed with `.and`/`.or`) so the datum lands in the **shareable, host-forwarded log**
instead of in the tester's eyes. Manual done (hold the chord) is the **fallback** — for a genuine human
judgement call where no signal exists.

A corollary, and the thing to reach for before writing a manual step: **if a step needs a datum that
isn't logged yet, make the mod log it** — turn on the matching `[debug.probes]`, extend the diag
snapshot, or add a one-shot milestone line next to the relevant toast. `two-player-join` is the worked
example: the side-channel link and config-adoption were once only ephemeral toasts, so the guide now
auto-finishes on `coop: linked` / `coop: adopted host config` lines added next to those toasts (a real
diagnostics win too — the live log now shows *when* the link happened). When even that isn't available
yet (RE-gated), commit the step as a `.stub(...)` noting what's missing — never a manual "tell me it
worked" relay.

## Debug-only — zero release cost

The **entire** subsystem is gated behind `#[cfg(debug_assertions)]` (on for the `dev`/test and
`diag` profiles, off for `release`), because there is no rig in a player's hands. That means the
core `guide` module, the `[debug] guide` / `[debug] rig_role` config fields, the `guide_log` tee,
the `rig_guide` banner channel, the `features::rig_guide` feature, and the overlay's banner draw are
**all** stripped from a `--release` build. Verified: a `cargo build --release` DLL contains none of
the subsystem's strings (`rig-guide: running`, `Rig guide complete`), while a `cargo build --profile
diag` DLL contains them. The shipping build pays nothing.

## Architecture (core vs. coop, the project split)

All decision logic is **pure and host-tested** in `unseamless_core::guide` — advance, branch, skip,
finish-predicate evaluation, role filtering, the done terminal, the hold-to-confirm timer, and the
auto-colouring. It's unit-tested by feeding synthetic log lines + button events, no game needed
(`scripts/test-core.sh`). The cdylib binding is a thin shell that, each frame:

1. gathers a read-only snapshot — `game_state` (from `playstate`), the session FSM
   (`lobby_state`/`protocol_state`/`players` via `session::read`), the new log lines since last tick
   (drained from `guide_log`), and the two control chords (from the pad snapshot);
2. ticks the `GuideRunner` with that `GuideInput`;
3. publishes the returned banner (text + auto colour) to `crate::rig_guide` for the overlay to draw,
   and fires the "done testing" toast on the completing tick.

```
                       crate::guide_log (log TEE → drain queue)
                                   │ new log lines / tick
 game thread                       ▼
 features::rig_guide ── GuideInput ─▶ unseamless_core::guide::GuideRunner::tick ─▶ TickResult
   (gather snapshot)                                                                  │
   playstate / session::read / pad_snapshot                                           │ publish
                                                                                      ▼
 Present thread:  coop::overlay::draw_rig_guide_banner ◀── crate::rig_guide (pinned banner cell)
```

The split is the same one the rest of the mod follows (`docs/ARCHITECTURE.md` > core-vs-coop):
logic is *verified* in core, the binding just samples and renders.

### The model

- **Step** — instruction text + defaults `{manual-finish, serial-next, all-roles, executable}`.
  Opt-in modifiers: `.role(...)`, `.done_when(predicate)`, `.branch(|ctx| …)`,
  `.default_branch(advance)`, `.stub(reason)`.
- **Predicate** — a closure over a read-only `PredicateCtx` (the snapshot + per-step elapsed time +
  the log lines seen since the step started). Ready-made composable ones: `log_contains`,
  `lobby_is` / `protocol_is`, `game_state_is`, `players_at_least`, `after_secs`, plus `.and`/`.or`.
  `Predicate::new` takes any closure, so the set is trivial to extend.
- **Branch / skip** — a step finishing normally takes its `.branch` (default: serial `Next`); a
  **skip** takes its `.default_branch` (default: `Next`, which becomes `Done` past the last step).
  Skip is best-effort per the brief: serial → next, branching → its declared default, dead-end →
  done. Unknown `To(id)` targets degrade to `Done` rather than panicking.
- **Roles** — `Host` / `Join` / `Solo`, resolved from `[debug] rig_role` (default `Solo`). An
  untagged step shows to all; a tagged step shows only to its role, and the engine skips over
  non-matching steps when advancing. This is what makes two-player testing easy.
- **Stub steps** — `.stub(reason)` marks a not-yet-executable step that renders as committed
  documentation (a `[PENDING: reason]` banner) until the work behind it lands. A stub has no auto
  finish; it advances on done/skip like any manual step, so a partially-built (or all-stub) guide
  can be committed now and revived later **without ever trapping the tester** — skip walks it
  straight to the done toast.

### Controls (the one TBD — picked, documented, swappable)

Two deliberately-awkward, **standard-bit** chords (no Guide/Home button, so they survive Steam
Input — same reasoning as the overlay toggle), read from the pad snapshot the overlay's XInput hook
already captures. Defined in `features::rig_guide`, trivially swappable:

- **DONE = hold `L3 + D-pad Up`** — held-to-confirm (a `0.75 s` hold, so a fat-finger never advances
  by accident). Always available, even on an auto-finish step, as a manual override if a signal
  never fires.
- **SKIP = press `L3 + D-pad Down`** — fires on the rising edge (a tap).

The banner **auto-appends** the hint line (`(hold L3 + Up = done, L3 + Down = skip)`) — authors
write only the instruction text.

### Auto-colouring

Banner colours are assigned by the engine, **never** in a guide. A regular step gets a deterministic
per-step palette hue (keyed off its id via `crate::palette`, so it's stable frame-to-frame yet
consecutive steps read as visibly distinct); a **stub** gets one fixed, muted "pending" colour so
documentation banners read as dim/secondary. The colour rides on `TickResult` and the overlay draws
the banner text in it.

### Rendering — a dedicated pinned slot

The step banner is its **own** top-center surface (`draw_rig_guide_banner`), distinct from the
rotating, capped notification banners — it doesn't consume a `MAX_BANNERS` slot and stays put while
toasts come and go. It reuses the passive-surface rendering primitives (borderless,
input-transparent, the crisp menu font) but is a separate window, always visible while a guide runs
(independent of the utility window). The done toast goes through the normal notifications model.

## Committed guides

Selected via `[debug] guide = "<name>"` (empty = off). Add one by writing a builder function and a
one-line `by_name`/`NAMES` entry in `guide/guides.rs`.

| Name | What it does |
|---|---|
| `rung3-create-chart` | **Flagship / dogfood** for the rung-3 create-session RE (`docs/SESSION-RE-FINDINGS.md`). Boot to a loaded save → host via a multiplayer item; auto-finishes on `lobby = TryToCreateSession` (or the `session-probe:` transition line) and **branches**: transition seen → a "captured" terminal, else → a "try a summon sign" retry step. Drives the human steps + auto-detects the log signal; the orchestrator-side ptrace write-watch is separate. Run it with `[debug.probes] session_probe = true`. |
| `overlay-smoke` | A tiny self-test of the guide system: banner renders, controls work, an `after_secs(3)` auto-advance fires, the done toast shows. Needs no session/RE state. |
| `two-player-join` | **The canonical role-tagged two-player guide** and the showcase of log-driven auto-finish: it drives the full friend-connect flow (rungs 4 + 2) and every connect step **auto-finishes off the run log** — the lobby-discovery resolve line (per role), the rung-2 link milestone (`coop: linked`), the client's `coop: adopted host config` — so the result is captured in the (forwarded) log, not relayed. Host/joiner/shared steps from one guide (set `[debug] rig_role` per machine). Ends with a committed **stub** (settings take *effect* in-world, pending the apply layer + rung 3). The driving doc is `docs/FRIEND-TEST-RUNBOOK.md`. |
| `rig-observation` | The **rig observation run** (`docs/RIG-RUNBOOK.md`): drive the session observer through the states to chart and read the `session change @frame …` snapshots. Solo legs auto-finish off the observer log line / live FSM where a fresh signal lands in-window (else the manual advance covers it — the first `session change` may fire at the title, and `TryToCreateSession` is transient; run with `[debug.probes] session_probe = true` for the FSM log signals); the 2-player legs (player count, in-combat scaling, area-boundary persistence) are committed **stubs**, revived during the friend test. Points at `rung3-create-chart` for the FSM capture rather than duplicating it. |

## How to run one

1. Set `[debug] guide = "<name>"` in the install's `unseamless-coop/unseamless_coop.toml` (or the rig
   seed at `scripts/rig/seed-config.toml`). For a two-player guide, set `[debug] rig_role` to `host`
   on the hosting machine and `join` on the joiner (default `solo`).
2. If the guide's predicates read probe output (the flagship reads `session-probe:` lines), enable
   the matching probe (e.g. `[debug.probes] session_probe = true`).
3. Launch (diag/dev build only). The pinned banner appears top-center; follow it, using the DONE
   chord to advance a manual step and SKIP to move past one. The guide auto-advances where it can.

## Friend / prerelease sharing

When a friend or prerelease session is meant to validate **something specific**, ship a guide in the
shared bundle config so the friend(s) **and** you are walked through the same on-screen steps — set
`[debug] guide = "<name>"` in the bundle's `unseamless_coop.toml`, and set each machine's `[debug]
rig_role`. The guide is debug-only, so this only takes effect on a diag/prerelease build. See
`docs/FRIEND-TEST-RUNBOOK.md` > "Stage it up front".

## Extending

- **A new finish condition** the ready-made predicates don't cover: add a constructor in `guide.rs`
  next to `log_contains`/`lobby_is`/… returning a `Predicate`, reading whatever `PredicateCtx`
  already exposes. If it needs new state, add a field to `RigState` and fill it in the binding's
  snapshot gather.
- **A new control** or chord: change `DONE_CHORD`/`SKIP_CHORD` + `HINTS` in `features::rig_guide`
  (the engine is control-agnostic — it takes raw held bools and the hint labels).
- **A new guide**: a builder function + a `by_name`/`NAMES` arm. Stubs let you commit a guide before
  the steps are executable.
