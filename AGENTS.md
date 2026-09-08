# AGENTS.md

This file provides guidance to Codex when working in this repository.

## What this is

A from-scratch **Rust rewrite of the Elden Ring Seamless Co-op mod** (ERSC, originally a C++
DLL). The goal is to reverse-engineer ERSC's behavior and re-implement it on top of the
[`fromsoftware-rs`](https://github.com/vswarte/fromsoftware-rs) SDK so it can be built on and
extended in Rust rather than patched as opaque C++.

> **Scope:** interoperability work on software the developer owns, run on his machines. The mod is
> co-op-only and loads outside anti-cheat. It never defeats EAC or DRM or touches another person's
> system, account, or server. Follow **Clean-room hygiene** and **Safety / legitimacy** below.

The proven scaffolding, toolchain, and runtime patterns come from the sibling project
**`../er-crit-coop`** (same author, same SDK, a single small DLL mod). When in doubt about
how to build, structure, load, or safely hook the game, read that repo first — its
`docs/DEVELOPMENT.md` and `src/patch.rs` module docs are the reference for everything below.

> Read [`docs/STATE.md`](docs/STATE.md) first for current state and Next. The gating map is
> [`docs/ROADMAP.md`](docs/ROADMAP.md); the in-world session plan is in `docs/SESSION-DRIVE.md`.

## Clean-room hygiene (one hard rule)

This is an independent reimplementation. The upstream `ersc.dll` (under `reference/`,
gitignored) is **all-rights-reserved** — there's no license to copy its code. The legal line
is **ideas/behavior vs. expression**: studying *what* ERSC does and writing your own
implementation is fine; copying *its code* is not. So:

- **Never paste decompiler/disassembler output (Ghidra/IDA/radare2 pseudocode) into source,
  comments, or commits.** Read it to understand behavior, then close it and write your own.
- When you need a record of a behavior, write it **in your own words** as an observation
  ("on event X the mod does Y"), then implement from that note — a soft clean-room. Don't
  transcribe their structure verbatim.
- **Don't redistribute any upstream bytes:** not `ersc.dll`, the launcher, locale JSON, or any
  FromSoft asset. `reference/` is gitignored to enforce this; keep it that way. Reading their
  `.ini`/config *format* for compatibility is functional interop and fine.
- Lean on the `fromsoftware-rs` SDK for "how the game works" — that's public knowledge and
  keeps the work naturally on the reimplement side rather than the copy side.

This costs nothing during development and keeps the project on solid ground. (Not legal
advice, just the working rule.)

`ersc.dll` is also **Themida-packed** (virtualized logic, 8 stub imports), so static
decompilation is a dead end — the rewrite is necessarily **behavioral**: reimplement from
observed behavior + the public SDK. For the RE workflow (triage, the diagnostic pattern, rig
tools) use the **`/reverse-engineer`** skill; the feature surface is in
[`docs/FEATURES.md`](docs/FEATURES.md).

## Where things run (read this first)

Development and testing both happen on this **Linux gaming PC** — it builds the mod *and* runs
Elden Ring. We cross-compile a **Windows DLL** from Linux (the `x86_64-pc-windows-gnu` target, the
correct build approach regardless of host); the toolchain is mingw-w64 (`pacman -S mingw-w64-gcc`).
The only split is build-vs-run, both on this one machine:

- **Build/check:** edit code, cross-compile the DLL, run `cargo check`/`clippy`, reason about the
  SDK and the reference `ersc.dll`, and run `unseamless-core`'s tests natively on the host
  (`scripts/test-core.sh`).
- **Run/verify:** install the DLL, launch the game, and watch the log to verify behavior, all via
  `scripts/rig.sh` and the `/test-loop` skill.

> **Install with `scripts/rig.sh apply`, NEVER `scripts/deploy.sh`.** This PC runs the user's
> *real* ERSC + Elden Mod Loader + own-mods stack. `rig.sh` snapshots that stack to a safe backup
> before standing in for it; `deploy.sh` is the bare install primitive with **no backup safety**, so
> running it directly clobbers the real `dinput8.dll` (Elden Mod Loader) and launcher with no way
> back. Drive launch/log/kill through `rig.sh` too (see the `/test-loop` skill, layer 4).
>
> **`apply`/`cycle` freely; `restore` when a task genuinely needs it — neither needs permission or
> tracking.** Re-applying is cheap and repeatable (`cycle` reaches in-game autonomously); never track
> or reason about "is the mod applied" — if in doubt, just apply. Restore is for real needs (a live
> ERSC capture, a bad rig state), never routine ceremony between cycles. Full rig policy: the
> `/test-loop` skill, layer 4.
>
> **Drive the rig — and the Deck — yourself by default.** Build, apply, launch, auto-session
> in-world, read logs/memory, kill: all of it is yours to run without asking or waiting, on both
> machines (`rig.sh`, `deck.sh`). Pull Michael in only when a test genuinely needs a human playing —
> something auto-sessions + getting-in-world can't cover (real gameplay, a judgment call on feel,
> hardware he must touch). "Serial" work is orchestrator-serial, not Michael-gated.
>
> **A running game is NOT a blocker — kill it and cycle.** You're free to kill/restart/start the local
> *and* Deck game to run tests, even one you didn't launch (Michael, 2026-07-06: "you're always free to
> kill the game"). Don't stop and "come back later" just because ELDEN RING is up, and don't infer
> "Michael is playing" from a live process. The one real hazard is applying *over* a live process (it
> overwrites the mapped `dinput8.dll` in place, and `rig.sh apply`/`cycle` refuse when the game is up):
> so **kill first, then apply/cycle** — which is the normal flow anyway (`cycle` = apply→kill→launch).

The log-line contract (install → heartbeat → effect lines) keeps behavior legible — write code so
its effects show up in the log.

## Code layout (workspace)

Two crates, split by what can be verified where (full design in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)):

- **`crates/unseamless-core`** — pure Rust, **no game/OS deps**. Config, scaling math, and
  (later) the sync model + protocol types. **Runs its tests natively on the host (Linux)** — this
  is where logic is *verified*, not just hoped. Keep decision logic here.
- **`crates/unseamless-coop`** — the `cdylib`. Thin binding layer: `DllMain` → `app::install`
  loads config and registers `Feature`s as recurring tasks. Binds core to the live game via the
  SDK. Its correctness needs the rig.

## Workflow

This is a personal project: **commit and push directly to `main` as needed** — no draft PRs, no
stacked branches, no PR descriptions. Keep commits small and well-described, keep `main` green
(`cargo build --release`, `scripts/test-core.sh`, `cargo clippy --release -- -D warnings` all
pass before pushing). This overrides the global draft-PR/stacking workflow.

**Session state & continuity.** The fast-moving "what we're working on / what's next" lives in
[`docs/STATE.md`](docs/STATE.md) — **about the work**, not machine state (overwritten, never
appended; pointers only — content goes to the proper doc). It deliberately does **not** track live
workers (that's `scripts/fleet/worker-ls`), rig/Deck state (cheap to re-apply, never to remember or
restore), or uncommitted git state. End a work session with **`/wrap`** (sweeps un-encoded learnings
into their homes, rewrites STATE.md to reflect the work, commits); decide an open next step with
**`/next`** (candidates + gating analysis, recorded in STATE.md, worker briefs drafted for delegable
ones). A fresh orchestrator started via `scripts/fleet/orch-start` is auto-seeded with a boot prompt
that reads STATE.md, briefs Michael, and starts the recorded Next when launched to continue the
project. It waits only when no work was requested or Next contains a real human gate. Full contract:
[`docs/ORCHESTRATION.md`](docs/ORCHESTRATION.md) > "Session Continuity".

**Review is proportional.** Experiments and RE probes need a green build and diff inspection. Solid
features use `/check` when small and `/tricheck` when larger or logic-heavy. Build a coherent chunk
before review, apply surviving findings, then commit it to `main`.

When a new capability lands, update the call sites that should use it and align its docs or skill in
the same work.

**Concurrent sessions.** There are often other Codex sessions building in this repo at the same
time. Michael tries to scope each session to independent work so they don't collide, so by default
stay in your lane. But if you *do* hit a conflict — uncommitted changes you didn't make, a dirty
working tree, a file another session is clearly mid-edit on — **preserve their work, don't clobber
it.** Don't `git checkout`/`stash`/reset away changes you didn't make or blindly overwrite a file
that's diverged from what you expected. Integrate alongside them, keep both sets of changes, and if
the two genuinely conflict, stop and surface it rather than picking a winner. Work together
gracefully.

Committing unrelated concurrent changes is acceptable when preserved deliberately; never destroy
them with reset, stash, checkout, or overwrite.

## Orchestrator / worker fleet

This repo can be developed as a one-orchestrator / many-worker fleet of Codex sessions over
[rift](https://github.com/anomalyco/rift) copy-on-write workspaces, coordinated over tmux. **You are
the orchestrator unless a worker role is injected** (workers launch with
a seed prompt reading `docs/roles/worker.md`, or `docs/roles/worker-solo.md` for a user-driven
worker, which overrides this). The **`fleet` skill is the orchestrator's operational playbook**
(spawn/message/integrate/teardown via `scripts/fleet/`, briefs, review offloading); the full design
is [docs/ORCHESTRATION.md](docs/ORCHESTRATION.md). The always-on rules:

- **The split:** the orchestrator owns the rig, RE, in-game validation, integration, and the only
  commits to `main`. A worker is one lane of feature work WIP-committing to `worker/<name>`; it
  never drives the rig and never commits to `main` — anything serial goes to the orchestrator by
  message (`scripts/fleet/msg usc-orch "[worker:<name>] ..."`). The rig is single, so all
  rig/RE/validation serializes through the orchestrator (the structured form of the
  concurrent-sessions guidance above).
- **Delegate buildable work:** use a worker unless it needs the rig/RE/integration, is under about
  15 minutes, or is the decision itself. Only independent static RE is delegable.
- **Buildable chunks go to fleet workers, not ephemeral coding subagents.** The litmus test: *would the
  result be a branch you'd merge to `main`?* → fleet worker (visible, watchable, integrable —
  even for a single lane). *Just informing your own work* (running tests, `Explore`, research,
  read-only review agents)? → a subagent is fine. Keep those reviewers read-only so they do not
  disrupt branch ownership, integration, or the serialized rig.
- **Review ownership:** each worker reviews its own solid lane and names the check at handoff. The
  orchestrator reviews only a nontrivial integrated cross-lane surface.
- **Orchestrator only — ping Michael's phone when stopping:** run
  `scripts/fleet/notify-human "<one-line reason>"` once when you're done, giving up, or blocked on
  something only he can do; never for progress updates or per-worker milestones.

Edit `AGENTS.md` and `.agents/skills/` directly. Codex reads these native files;
there are no compatibility links or generation steps.

## Docs & naming

- The project name is always **`unseamless-coop`** — lowercase, hyphenated. Never title-case or
  capitalize it (not "Unseamless-Coop", not "Unseamless Coop"), including in Markdown headers.
- Otherwise use **title case for Markdown headers** in the README/docs (e.g. "Install & Play",
  "What's in the Bundle"). Keep `ELDEN RING` in caps (the game's own styling).

## Project knowledge lives in the repo, not personal memory

Do not use personal memory for project knowledge. Put rig facts in `docs/RIG-RUNBOOK.md`, RE results
in the relevant findings/design doc, orchestration in `docs/ORCHESTRATION.md`, reusable procedures in
skills, and cross-cutting rules here so every worker sees them.

## Deliberate divergences from ERSC (don't "fix" back)

We reimplement ERSC's *effect*, not its design, and intentionally differ. Full list in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) > Divergences; the load-bearing ones:

- **Config = TOML + serde** (`unseamless-core/config.rs`), not ERSC's `.ini`. Don't add an INI parser.
- **One settings registry** (`unseamless-core/settings.rs`) drives both the config file and the
  menu — declare an option once. Don't hand-wire per-option UI.
- **Session actions = an overlay menu** (`unseamless-core/menu.rs` model + a shipping imgui/DX12
  overlay via hudhook, `coop/overlay.rs`), **not** ERSC's custom in-game items (`MODGOODS_*`) or a
  native pause-menu entry (no
  SDK API for that). The `MODGOODS_*` surface in FEATURES.md is reference, not a build target.
- **Networking = drive the game's own session layer + a private side-channel**
  (`unseamless-core/protocol.rs`); no bespoke transport, no vanilla-ERSC interop.

## Build & test

```bash
cargo build --release        # default target is windows-gnu -> the DLL (see .cargo/config.toml)
cargo build --profile diag   # debugging build: keeps symbols + debug-assertions for readable
                             #   panic backtraces (the shipping build is stripped). Use when
                             #   chasing a crash; not for release.
scripts/test-core.sh         # run unseamless-core's tests on the host triple (Linux-runnable)
python3 scripts/test-fleet.py # isolated fleet launch, resume, and migration guards
```

Testing beyond unit tests (the host harness, the rig) is the **`/test-loop`** skill.

**Logging rule:** verbosity is `[debug]` config, **off by default** (milestone lines only) — so
hot-path logs must use `log::debug!`/`trace!` to stay silent when off, never `info!`. The
self-describing, shareable log model is `unseamless-core/diagnostics.rs`.

The shippable artifact is `target/x86_64-pc-windows-gnu/release/unseamless_coop.dll`. The
default cargo target is the cross target, so a bare `cargo build`/`cargo check`/`cargo clippy`
cross-compiles the DLL. The core crate has no windows deps, so `scripts/test-core.sh` compiles and
runs its unit tests natively (a bare `cargo test` would target windows-gnu and can't execute on the
Linux host).

- The cross target is pinned in `rust-toolchain.toml` (`channel = "stable"`,
  `targets = ["x86_64-pc-windows-gnu"]`) so `rustup` installs it automatically.
- The GNU target links with **mingw-w64**. Install it (`pacman -S mingw-w64-gcc` on Arch); cargo
  finds `x86_64-w64-mingw32-gcc` on PATH.
- Release profile: `panic = "unwind"`, `opt-level = "z"`, `codegen-units = 1`, `lto = true`,
  `strip = true`. **`unwind`, not `abort`** (the one divergence from `er-crit-coop`'s release
  profile): it's what makes the per-feature `catch_unwind` firewall real in the player's build (a
  feature panic is caught + disabled + toasted, not a game crash). Safe only because *every* game→us
  FFI entry point is firewalled so a panic can't unwind across an `extern` boundary into vkd3d/the
  game — see [`docs/FFI-UNWIND-AUDIT.md`](docs/FFI-UNWIND-AUDIT.md).
- Builds are **not** bit-reproducible across hosts (CI mingw vs local mingw differ); the
  release `.dll` won't sha-match a local build. Compare `.text` size with
  `x86_64-w64-mingw32-objdump -h` to sanity-check equivalence.

The run/verify loop on the rig is the **`/test-loop`** skill (see "Where things run").

## Document how to re-derive RE results (AOBs, addresses, debug methods)

Game updates shift addresses and can break AOB patterns. Whenever a patch, hook, or debug/RE
method **works**, leave a concise comment next to it on **how the address/pattern/result was found**,
so a future session can re-derive it fast after an update instead of rediscovering it from scratch.
Aim for "enough that you'd know what to do," not a tutorial — but include exact values (FMG ids,
offsets, AOB bytes, which `diag` probe was used and what it showed, why a landmark is unique) when
having them on hand speeds up the re-find. This applies to the `coop/patch.rs` patches, the
`coop/diag.rs` probes, and any hooked function we located by RE.

## The SDK (the "pointer mappings" library)

Built on `fromsoftware-rs` (`eldenring` + `fromsoftware-shared`), **pinned by exact git commit**
in `Cargo.toml`. It exposes game structs as **named typed fields** instead of raw offsets, so
most of ERSC's pointer bookkeeping is already done; prefer named fields over offsets always.

**Hard rule:** pin both crates to the **same commit** — layouts are read against a specific
revision, and mixing them is silent UB. Re-verify field accesses when bumping the pin. What the
SDK already charts vs. what needs RE is in [`docs/SDK-COVERAGE.md`](docs/SDK-COVERAGE.md).

## Architecture & hard safety invariants

These are load-bearing rules learned in `er-crit-coop`, not style preferences. Violating them
is a use-after-free or a data race in someone's game.

- **`DllMain` handles only `DLL_PROCESS_ATTACH`.** It inits logging and spawns a short-lived
  init thread, then returns. Do real work off the loader lock and off the main thread —
  `CSTaskImp::wait_for_instance` blocks on main-thread init and will deadlock if called on it.
- **Hook the game by registering a recurring task on its own scheduler**, not a free-running
  background thread:
  ```rust
  let cs_task = CSTaskImp::wait_for_instance(timeout)?;   // on the init thread, NOT main
  let handle = cs_task.run_recurring(|_: &FD4TaskData| on_frame(), PHASE);
  std::mem::forget(handle);                               // registration is permanent
  ```
- **`std::mem::forget` the task handle.** The SDK never unregisters; its `cancel()` is a
  no-op stub and the task self-references. Dropping the handle (or adding a
  `DLL_PROCESS_DETACH` cleanup) frees an image the still-registered task points into →
  use-after-free. The DLL must stay resident for the process lifetime.
- **Safety is frame-ordering, not thread exclusivity.** Pick the task phase
  (`CSTaskGroupIndex`) so your code runs after the game writes the state you read and before
  the game consumes it. `WorldChrMan_PostPhysics` is the worked example (after behavior
  update, before `DmgMan`). Running in step with the frame is what removes the cross-thread
  race — there are no pointer guards or atomics on the game state for this reason.
- **`characters()` yields entries regardless of `chr_load_status`**, so across a
  loading/fast-travel transition you can touch a mid-init/teardown `ChrIns` with unwired
  module pointers. A `PostPhysics`-style phase keeps the window small; the fully robust form
  iterates ChrSet entries and skips any whose status isn't `Active` before dereferencing.

## Surfacing errors (fatal popup vs in-session toast/banner)

One rule for how the mod tells the user something went wrong, split by *whether the mod can run at
all*:

- **Startup failure → `guard::fatal` (modal message box, then close the game).** Use the shared
  [`guard::fatal`] util for any condition where the mod cannot install or continuing would be wrong,
  so there's no half-working state to limp along in. Current fatal conditions, all in `coop/guard.rs`
  / `coop/app.rs` before features register: **not launched by our launcher** (the EAC guard),
  **co-op password too short**, and **the game's task system never coming up** (`app::install` can't
  get `CSTaskImp`). Fail loudly and close rather than leave the game running silently unmodded.
- **In-session problem → toast/banner (+ log), never fatal.** Once we're installed and ticking,
  anything that goes wrong degrades gracefully and informs via the notifications model
  (`unseamless-core/notifications.rs`): config-clamp warnings, a peer **version mismatch**, a feature
  **panicking** (it's caught by the per-task `catch_unwind` firewall, disabled for the session, and a
  plain-voice toast tells the player — the game keeps running), **connection lost**, etc. Never kill
  the player's game for these. This degrade-don't-crash guarantee is why the shipping profile builds
  with `panic = "unwind"` (the firewall is a no-op under `abort`) and why every game→us FFI boundary
  is itself firewalled so a panic can't unwind across it ([`docs/FFI-UNWIND-AUDIT.md`](docs/FFI-UNWIND-AUDIT.md)).
  (The toast/banner model is host-tested; the renderer is the in-game overlay, `coop/overlay.rs` —
  hudhook DX12 + imgui, shipping.)
- **Rule of thumb: if we can't install, close loudly; if we're installed and something goes wrong,
  degrade and notify.** Don't reach for `guard::fatal` from inside a feature's `on_frame` — by then
  we're past install, so it's a toast/banner.

**Message voice: ER tone for gameplay, plain for diagnostics.** A user-facing message about an
in-world *effect* (death debuffs, rune-arc, PvP, player join/leave) is worded in FromSoft's register —
terse, weighty, a little archaic — and **never shows raw mechanical values** (no `(×2.0)`; convey
intensity with a word: `Afflicted by Hopelessness`, `Your afflictions deepen`, `Afflictions cleansed`).
A **diagnostic/technical** message (version mismatch, connection lost, a feature disabled by a panic,
config-clamp warning) stays plain and literal — dressing those up obscures a real problem. Lore voice
for effects, plain voice for diagnostics.

**Toasts are a valued, first-class surface — keep and expand them.** Michael likes the in-game toasts
(the `unseamless-core/notifications.rs` model rendered by `coop/overlay.rs`) and called them out
unprompted on the rig; treat the notifications model as first-class, not a debug afterthought. When a
feature has a notable state change, consider whether a toast/banner fits, and don't strip or quiet them.
The side-channel already toasts connect/version/liveness, and the ER-voiced **player join/leave/return**
presence toasts shipped; keep adding useful session-event toasts as the co-op layer grows.

[`guard::fatal`]: crates/unseamless-coop/src/guard.rs

## On-demand procedures live in skills

These are loaded only when relevant (not in this always-on file):

- **Testing / verifying** — the host harness, the rig, and all test layers: the **`/test-loop`**
  skill (+ [`docs/RIG-RUNBOOK.md`](docs/RIG-RUNBOOK.md)). This is also where the run/verify loop
  (deploy, `steam -applaunch`, the `pkill` bracket trick, the `FrameBegin` firing check) lives.
- **Reverse-engineering** behavior — static triage, the diagnostic rising-edge pattern, and the
  rig tools (rizin / Ghidra wrapper / Frida): the **`/reverse-engineer`** skill (+
  [`docs/RUNTIME-RE.md`](docs/RUNTIME-RE.md)).
- **Cutting a release** — tag-driven CI: the **`/release`** skill.
- **Driving a Steam Deck / second machine as player 2** (over SSH, for two-player networking tests):
  the **`/steam-deck`** skill (`scripts/deck.sh` + `scripts/deck/deck-remote.sh`). The local rig is
  player 1; the Deck is a near-stateless player 2 we build-here-and-push-to.
- **Validating the overlay's DX12 present-hook on real Windows** (the native-Windows crash in
  [`docs/OVERLAY-RENDERING.md`](docs/OVERLAY-RENDERING.md)) — the **`/windows-test`** skill
  (`crates/dx12-harness` + `scripts/win.sh`): runs the same hudhook hook + imgui font bake on a real
  Windows loader in the local quickemu Win11 VM, no game. A cheap filter (WARP, not NVIDIA), with the
  friend's machine as the super-validated gate.

## Safety / legitimacy

unseamless-coop loads **outside EAC**, so it's for co-op only. Never take a modded session onto the
official servers. The mod must not touch `regulation.bin` (so it can't block players from
connecting).

We own the whole install — no Elden Mod Loader, no ERSC launcher: the cdylib ships as the game's
`dinput8.dll` proxy (auto-loaded; also the parent loader for `mods/`), and our `launcher` crate
ships as `start_protected_game.exe`, which starts the game directly (outside EAC) with the
`UNSEAMLESS_LAUNCH` marker. The DLL **aborts** if that marker is absent (`coop/guard.rs`), so a
game update that reverts the launcher can't run the mod under anti-cheat. Config and logs live in
our own `unseamless-coop/` folder, never ERSC's `SeamlessCoop/`.
