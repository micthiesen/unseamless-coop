# In-Overlay Rig-Testing Guides

A **guide** is an ordered series of on-screen test steps that walks a tester through a rig run
without the orchestrator having to round-trip "do X, tell me the result, now do Y" over the wire.
The current step shows as a pinned banner until its finish signal fires; the engine then advances
(optionally branching on a log/state result), and the guide ends with a hardcoded "done testing"
toast. One shared guide runs on every machine; each shows only the steps tagged for its role, and a
machine's role is **derived** from what the tester does (Open World ⇒ host, Join world ⇒ join) via a
standard connect step — so two-player testing is "both run the same guide; each just opens or joins."

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
   (drained from `guide_log`), the two control chords (from the pad snapshot), **and the choice-modal
   input the overlay pushed back** (menu nav up/down/confirm + the keyboard note buffer, drained from
   `rig_guide`);
2. ticks the `GuideRunner` with that `GuideInput`;
3. publishes the returned view to `crate::rig_guide` for the overlay to draw — a **pinned banner**
   (`RigView::Banner`, text + auto colour) for a normal/stub step, or a **choice modal**
   (`RigView::Choice`, a `ChoiceView`) for a choice step — **logs a resolved choice**
   (`TickResult::choice_made`), and fires the "done testing" toast on the completing tick.

A choice modal needs input *back* from the overlay (the menu nav layer + the keyboard note field can
only be read on the Present thread), so `rig_guide` carries a second cell in the opposite direction —
the overlay pushes `ModalInput`, the game thread drains it into the next `GuideInput::choice`. The
selection index, nav (wrap), confirm-resolution, and the captured note all live in the **engine**
(host-tested); the overlay only renders the modal and pushes raw nav/confirm/note.

```
                       crate::guide_log (log TEE → drain queue)
                                   │ new log lines / tick
 game thread                       ▼
 features::rig_guide ── GuideInput ─▶ unseamless_core::guide::GuideRunner::tick ─▶ TickResult
   (gather snapshot)        ▲                                                          │
   playstate/session/pad    │ ModalInput (nav/confirm/note)                            │ publish
                            │  drain ◀───────────────────────────┐          banner OR choice
                                                                  │                    ▼
 Present thread:  coop::overlay::draw_rig_guide{_banner,_choice} ─┴── crate::rig_guide (view + modal-input cells)
```

The split is the same one the rest of the mod follows (`docs/ARCHITECTURE.md` > core-vs-coop):
logic is *verified* in core, the binding just samples and renders.

### The model

- **Step** — instruction text + defaults `{manual-finish, serial-next, all-roles, executable}`.
  Opt-in modifiers: `.role(...)`, `.done_when(predicate)`, `.branch(|ctx| …)`,
  `.default_branch(advance)`, `.stub(reason)`. Plus the composable `.connect_step()` (below) that
  appends a standard role-deriving step.
- **Predicate** — a closure over a read-only `PredicateCtx` (the snapshot + per-step elapsed time +
  the log lines seen since the step started). Ready-made composable ones: `log_contains`,
  `lobby_is` / `protocol_is`, `game_state_is`, `players_at_least`, `after_secs`, plus `.and`/`.or`.
  `Predicate::new` takes any closure, so the set is trivial to extend.
- **Branch / skip** — a step finishing normally takes its `.branch` (default: serial `Next`); a
  **skip** takes its `.default_branch` (default: `Next`, which becomes `Done` past the last step).
  Skip is best-effort per the brief: serial → next, branching → its declared default, dead-end →
  done. Unknown `To(id)` targets degrade to `Done` rather than panicking.
- **Roles** — `Host` / `Join` / `Solo`. An untagged step shows to all; a tagged step shows only to
  its role, and the engine skips over non-matching steps when advancing. This is what makes two-player
  testing easy. **A role is normally DERIVED, not hand-set** (see *Connect step* below): the runner
  starts unresolved (`Solo`) and the connect step sets it from the tester's Open/Join action. The
  `[debug] rig_role` field is only an **override / solo fallback** — an explicit non-`Solo` value pins
  the role and suppresses derivation; left at the default `Solo`, the connect step derives it.
- **Connect step** — `.connect_step()` appends a standard, shown-to-everyone step that **derives this
  machine's role from what the tester does**: Open World ⇒ `Host`, Join world ⇒ `Join` (read off
  `RigState::lobby_intent`, mapped in the binding from the live session flags). It auto-finishes the
  moment the intent resolves, and every role-tagged step *after* it then filters by the derived role.
  Drop it into a two-player guide once, before the role-tagged steps; guide writers never hand-assign
  a role. Role-tagged steps placed *before* it run with the role still unresolved (`Solo`) — only
  untagged steps show until it resolves. It's manually finishable + skippable like any step, so a solo
  run with no peer degrades sensibly (pick an intent to derive, or skip/finish before acting to stay `Solo`) and an
  explicit non-`Solo` `rig_role` override wins over the derived intent.
- **Stub steps** — `.stub(reason)` marks a not-yet-executable step that renders as committed
  documentation (a `[PENDING: reason]` banner) until the work behind it lands. A stub has no auto
  finish; it advances on done/skip like any manual step, so a partially-built (or all-stub) guide
  can be committed now and revived later **without ever trapping the tester** — skip walks it
  straight to the done toast.
- **Choice steps** — `.choice(&[(label, advance)])` (optionally `.note()`) marks a step that renders
  as a focused **modal** of preset options instead of a pinned banner. Selecting one captures the
  answer as a `ChoiceMade`, **logs it** (`rig-guide: '<id>' -> '<label>'`, plus `note = "..."` for a
  free-form annotation), and advances per that option's `Advance`. It's the **last resort after
  logging** (see below): the modal exists for the one source logging can't reach — the tester's
  eyes/judgement — and turns it into captured, logged, branchable data instead of throwing it away.
  Skip still escapes it (logged as `skipped`, taking the default branch), so the never-trap rule holds.

#### Choice modal: the last resort after logging

The whole point of the system (above) is **capture data, never verbal relay**: `done_when(...)`
auto-finishes off the log/live state. A choice modal is the *capstone* of that principle, not an
escape from it — it's only for an **irreducibly human-perceptual** signal where the answer **matters**
(it branches, or is worth recording): does the peer render in-world, is the nameplate placed right,
does the log show the expected snapshot. A plain "press to continue" stays a normal manual step, **not**
a choice. The ordering is: a `done_when(...)` predicate first; if the datum isn't logged, log it; only
when the signal is irreducibly in the tester's judgement reach for `.choice(...)` — and even then the
answer is logged, so it's captured and shareable like every other guide signal.

**Controls.** Preset options are navigated with the **overlay menu input layer** (d-pad / left-stick /
arrows to move, A / Enter to confirm) — *not* the done/skip chords, which stay the normal-step
controls. The skip chord still escapes. The optional free-form **note** is **keyboard-only** (the rig
has a keyboard; there's no controller text entry / virtual keyboard) — a controller-only tester uses
the presets + skip, free-form needs a keyboard.

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

### Rendering — a dedicated pinned slot (and a focused modal)

The step banner is its **own** top-center surface (`draw_rig_guide_banner`), distinct from the
rotating, capped notification banners — it doesn't consume a `MAX_BANNERS` slot and stays put while
toasts come and go. It reuses the passive-surface rendering primitives (borderless,
input-transparent, the crisp menu font) but is a separate window, always visible while a guide runs
(independent of the utility window). The done toast goes through the normal notifications model.

A **choice step** renders instead as a centered, near-opaque **modal** (`draw_rig_guide_choice`) over
a dim full-screen scrim — visually distinct from the pinned banner, and *focused/blocking*: it takes
input focus (the utility window yields, and `message_filter` + `set_blocked` suppress game input the
same way an open utility window does — we can't freeze ER's own loop, so "blocking" = modal focus +
the guide waits), and the guide doesn't advance until the tester confirms an option or skips. The
modal lists the engine-held options (the `selected` one highlighted), an optional keyboard note field
(`input_text`), and the controls; it's the one interactive overlay surface besides the utility window.

## Committed guides

Selected via `[debug] guide = "<name>"` (empty = off). Add one by writing a builder function and a
one-line `by_name`/`NAMES` entry in `guide/guides.rs`.

| Name | What it does |
|---|---|
| `rung3-create-chart` | **Flagship / dogfood** for the rung-3 create-session RE (`docs/SESSION-RE-FINDINGS.md`). Boot to a loaded save → host via a multiplayer item; auto-finishes on `lobby = TryToCreateSession` (or the `session-probe:` transition line) and **branches**: transition seen → a "captured" terminal, else → a "try a summon sign" retry step. Drives the human steps + auto-detects the log signal; the orchestrator-side ptrace write-watch is separate. Run it with `[debug.probes] session_probe = true`. |
| `overlay-smoke` | A tiny self-test of the guide system: banner renders, controls work, an `after_secs(3)` auto-advance fires, the **choice modal** renders + captures an answer (with a free-form note), the done toast shows. Needs no session/RE state — the worked example of the choice capability. |
| `two-player-join` | **The canonical role-tagged two-player guide** and the showcase of log-driven auto-finish: it drives the full friend-connect flow (rungs 4 + 2). The standard **connect step** derives each machine's role from its Open/Join action (no per-machine `rig_role` needed), then the rest **auto-finishes off the run log** — the rung-2 link milestone (`coop: linked`) and the client's `coop: adopted host config` — so the result is captured in the (forwarded) log, not relayed. Host/joiner/shared steps come from one guide. Ends with a committed **stub** (settings take *effect* in-world, pending the apply layer + rung 3). The driving doc is `docs/FRIEND-TEST-RUNBOOK.md`. |
| `rig-observation` | The **rig observation run** (`docs/RIG-RUNBOOK.md`): drive the session observer through the states to chart and read the `session change @frame …` snapshots. Solo legs auto-finish off the observer log line / live FSM where a fresh signal lands in-window (else the manual advance covers it — the first `session change` may fire at the title, and `TryToCreateSession` is transient; run with `[debug.probes] session_probe = true` for the FSM log signals); the 2-player legs (player count, in-combat scaling, area-boundary persistence) are committed **stubs**, revived during the friend test. Points at `rung3-create-chart` for the FSM capture rather than duplicating it. |

## How to run one

1. Set `[debug] guide = "<name>"` in the install's `unseamless-coop/unseamless_coop.toml` (or the rig
   seed at `scripts/rig/seed-config.toml`). For a two-player guide built around a connect step (e.g.
   `two-player-join`), the role is **derived** from each machine's Open/Join action — leave `[debug]
   rig_role` at the default `solo`. Only set it (`host` / `join`) to **force** a role for a guide
   without a connect step, or to run one leg solo.
2. If the guide's predicates read probe output (the flagship reads `session-probe:` lines), enable
   the matching probe (e.g. `[debug.probes] session_probe = true`).
3. Launch (diag/dev build only). The pinned banner appears top-center; follow it, using the DONE
   chord to advance a manual step and SKIP to move past one. The guide auto-advances where it can.

## Friend / prerelease sharing

When a friend or prerelease session is meant to validate **something specific**, ship a guide in the
shared bundle config so the friend(s) **and** you are walked through the same on-screen steps — set
`[debug] guide = "<name>"` in the bundle's `unseamless_coop.toml`. A connect-step guide derives each
machine's role from its Open/Join action, so no per-machine `[debug] rig_role` is needed (it's only an
override/solo fallback). The guide is debug-only, so this only takes effect on a diag/prerelease build.
See `docs/FRIEND-TEST-RUNBOOK.md` > "Stage it up front".

## Extending

- **A new finish condition** the ready-made predicates don't cover: add a constructor in `guide.rs`
  next to `log_contains`/`lobby_is`/… returning a `Predicate`, reading whatever `PredicateCtx`
  already exposes. If it needs new state, add a field to `RigState` and fill it in the binding's
  snapshot gather.
- **A new control** or chord: change `DONE_CHORD`/`SKIP_CHORD` + `HINTS` in `features::rig_guide`
  (the engine is control-agnostic — it takes raw held bools and the hint labels).
- **A new guide**: a builder function + a `by_name`/`NAMES` arm. Stubs let you commit a guide before
  the steps are executable.
- **A choice step**: `.choice(&[(label, advance)])` (+ `.note()` for a free-form field) on any step —
  but only as the **last resort after logging** (a human-perceptual signal whose answer matters). The
  engine surfaces it as `TickResult::choice`/`choice_made`; if you change the modal's controls or
  styling, that's `draw_rig_guide_choice` + the `ModalInput` channel in `coop/{overlay,rig_guide}.rs`,
  not the engine.
