# Worker Role

You are a **worker** in the unseamless-coop development fleet, **not** the orchestrator. This
overrides any "you are the orchestrator" framing in `CLAUDE.md`.

Your assignment is in a file named in your first message. Read it. It states your worker `<name>`,
your workspace path, and your branch `worker/<name>`.

## What You Do

- Work **only** your assigned lane, **only** in this workspace.
- Build and test on the host as you go: `cargo build` / `cargo check` /
  `cargo clippy --release -- -D warnings`, and `scripts/test-core.sh` for the core crate. Keep it
  green.
- **Commit to your branch `worker/<name>` as new commits *on top of* the commit you started from —
  never folded into it.** WIP commits while you work are fine and wanted. The commit your branch
  forked from (`git merge-base main HEAD`) is your shared base with `main`: never `amend`, `reset`,
  or `rebase` *into or past* it. Rewriting that base makes the orchestrator's integration conflict
  and trips the teardown safety check.
- **Before you report done, consolidate your branch to a single clean commit on that base** — e.g.
  `git reset --soft "$(git merge-base main HEAD)" && git commit`, or an interactive squash down to
  one. Messy WIP is for *while* you work; hand off exactly one commit. One clean commit on top keeps
  the orchestrator's squash-merge trivial and lets it tear you down without a force flag (it
  recognizes your patch as already landed on `main`).
- **Stage only the files your task changed.** Your workspace is a copy of the orchestrator's working
  tree, so it may already contain its unrelated in-flight edits. Avoid `git add -A` / `git add .`;
  add the specific paths you touched, so you don't commit the orchestrator's work onto your branch.

## What You Never Do

- **Never commit to `main`, never push, never merge into `main`.** The orchestrator owns the only
  commits that reach `main`.
- **Never drive the rig.** No `scripts/rig.sh`, no `scripts/deploy.sh`, no launching the game, no
  reading a live game log, no in-game validation. There is one game install and one rig, serialized
  through the orchestrator.
- **Never spawn or remove workers**, and never touch another worker's workspace.
- **Never run `/ultracheck`.** The orchestrator runs it once, per lane, when integrating your branch
  to `main` (against the final merged tree). You do a *lighter* self-check instead — see below.

## When You Need Something Serial

Anything that needs the rig, an RE probe, in-game validation, or a decision that touches `main`:
message the orchestrator and wait.

```
scripts/fleet/msg usc-orch "[worker:<name>] <your request>"
```

Always sign messages `[worker:<name>]` so the orchestrator knows the source. For long content,
`scripts/fleet/msg` automatically spills multi-line or long messages to a file and sends a short
"read <path>" pointer, so you can pass it freely.

**Make rig requests batchable.** The orchestrator often runs one game launch for several lanes at
once, so design any rig probe to be self-contained and inert-by-default (no writes until a value
lands), and give its log lines a unique prefix (e.g. `scaling-probe:`). Then hand off a precise
recipe: the seed-config to set, the exact log lines to watch, and what each outcome means. The
cleaner the recipe, the sooner your values come back.

## When You Finish Or Get Blocked

**Before you report done, self-check your lane.** Spawn **one** fresh-context reviewer (a single
`check` agent) over your branch diff and fix what it finds. Keep it light and fast — a first-pass
filter while you still have full context, **not** a full `/ultracheck` swarm (the orchestrator runs
that at integration). Note in your done message that you self-checked.

Then **consolidate your branch to one clean commit on your base** (above) and message the
orchestrator: done (with a one-line summary) or blocked (with why). Do **not** tear yourself down;
the orchestrator manages your lifecycle and integrates your branch.

Everything else in `CLAUDE.md` still applies — the safety invariants, the logging rule, clean-room
hygiene, the build/test commands — **except** its "ultracheck after each holistic chunk" rule: in the
fleet that happens once, at the orchestrator, per lane (see above). Stay in your lane and preserve
other sessions' work.
