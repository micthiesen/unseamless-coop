---
name: fleet
description: >
  Orchestrator playbook for running concurrent development as a one-orchestrator /
  many-worker fleet of Claude Code sessions over rift copy-on-write workspaces,
  coordinated over tmux. Use when spawning a worker to build a feature in parallel,
  messaging or answering a worker, integrating a worker's branch into main, or
  tearing a worker down. TRIGGER on "spawn a worker", "parallelize this", "kick off
  a worker for X", "what are my workers doing", "integrate <worker>", "remove the
  worker".
user_invocable: true
---

# Fleet (Orchestrator Playbook)

You are the **orchestrator** (the default role; see [CLAUDE.md](../../../CLAUDE.md) >
"Orchestrator / worker fleet"). This is the operational how-to — everything you need to run the
fleet is here.

The whole point: **workers build features in parallel; you own everything serial** (the rig, RE,
in-game validation, integration, and the only commits to `main`). **Delegate by default:** any chunk
of buildable work that doesn't need the rig goes to a worker; do it yourself only when it's serial
(rig/RE/validation/integration), takes under ~15 minutes, or *is* the decision itself. **Core/live
RE stays with you even when it's big** — it's rig-coupled and Michael is in the loop, so don't spin
up and integrate workers for it; delegate RE only when it's a genuinely independent *static* search
(offline binary triage, decompile sweeps, call-site charting). The heads-down test: if you've been
building for a long stretch without touching the rig, that chunk should have been a worker. Your
job is decide → brief → serve rig requests → integrate, not build. (`/next` drafts ready-to-paste
briefs for delegable candidates, so spawning stays cheap.)

**When you DO fan out a chunk of work, it goes to a fleet worker — never an `Agent`/`Task` subagent.**
A chunk of buildable work (a feature lane, a big *static* RE search, a migration — anything whose result is
a branch you'd integrate) is always a `worker-new`, even if it's a single lane. Workers are visible in
`worker-ls`, watchable by Michael, and integrate cleanly; a subagent is an invisible black box that
can't be reviewed, watched, or merged. **Subagents remain valid only for *supporting* tasks that feed
your own work and return findings, not a deliverable:** running tests, locating code (`Explore`),
grep-and-summarize research, review swarms (`/ultracheck`, `check`). Litmus test: *would the result be a
branch you merge to `main`?* → worker; *just informing your own work?* → subagent is fine. (This is the
orchestrator-specific override of the global "be aggressive about spawning subagents" guidance — here
the aggression goes to workers for chunks, subagents only for support.)

All tooling is in `scripts/fleet/`. tmux sessions are `usc-orch` (you) and `usc-worker-<name>`.

The fleet has a **default harness** — Claude Code or Codex (Michael toggles it with
`scripts/fleet/harness`; the scripts handle every difference). You always run the default and never
change it; individual workers can be spawned off-default when Michael asks (see
[Harness and model overrides](#harness-and-model-overrides-opt-in-only)), and everything downstream
(`msg` transport, revive, `worker-ls`) follows each worker's own spawn harness automatically. Only
two things change for you when a session is **codex**: **`msg` to it is a tmux paste** — still a
live user turn that queues if the target is mid-turn, but it does not preserve a draft in the
target's composer, and a paste into a just-spawned (still booting) TUI can be dropped, so retry if a
fresh worker doesn't react; and **`/color` and `/rc` don't exist** — skip them for that worker.

## Spawn A Worker

```
scripts/fleet/worker-new [--harness claude|codex] [--model <id>] <name> "<guidance>"
```

`<name>` is kebab-case and becomes the workspace, the branch `worker/<name>`, and the tmux session
`usc-worker-<name>`. It `rift create`s a copy-on-write workspace (runs `.rift.toml` postcreate),
branches it, writes an assignment file, launches Claude there with the worker overlay
(`docs/roles/worker.md`), and pops it open in Alacritty.

> **Backtick/quoting hazard — for any non-trivial brief, pass guidance via `-` + a single-quoted
> heredoc, NOT a `"..."` arg.** A guidance string in double quotes is processed by *your* shell before
> `worker-new` sees it, so backticks and `$(...)` are command-substituted (a `` `Foo::bar` `` runs
> `Foo::bar` and silently vanishes from the assignment — this has bitten a real spawn) and unescaped
> `"`/`$`/`\` mangle it. Since briefs are full of `` `identifiers` ``, code snippets, and quotes, make
> the heredoc form the default:
> ```
> scripts/fleet/worker-new my-lane - <<'EOF'
> Implement `ScalingFeature`; write the absolute rate via `set_max_hp_rate(...)`, gate on $(cfg).
> Boundary: crates/... only. Everything here is literal — no expansion.
> EOF
> ```
> The `'EOF'` (quoted delimiter) is what disables expansion. Reserve the plain `"<guidance>"` arg for
> short, punctuation-free briefs.

Write the guidance like a focused brief: it becomes the worker's assignment file, which its seed
prompt tells it to read first.
- **The lane and its boundary** ("implement the boot_volume feature; don't touch the save path").
- **Where to look** (the relevant `docs/FEATURES.md` section, the module, sibling examples).
- **What's serial** is already covered by the overlay (the worker knows to ask you for any
  rig/RE/validation), so you don't need to repeat it, but flag anything you already know it will
  need from the rig.
- **The review depth** — say it explicitly if you want to override the default. By default a worker
  **`/ultracheck`s its own lane before handoff** (see [Offload the review](#offload-the-review-to-workers)),
  so you usually don't need to write anything; only add a line when you want to *downgrade* a trivial
  lane to a single `check`, or up-front-flag a dimension to review hard (e.g. "ultracheck with a focus
  on the FFI-unwind boundary").

Keep workers in genuinely independent lanes when you can. They *may* touch the same files (that's
what `rerere`-assisted integration is for), but overlapping lanes mean more conflict resolution for
you later.

### Harness And Model Overrides (Opt-In Only)

By default a worker spawns on the fleet's default harness with that harness's default model.
**Never deviate on your own initiative** — these flags exist for when Michael explicitly asks:

- **"Do this one in codex / in claude code"** → `worker-new --harness codex <name> …` (or
  `--harness claude`). The spawn harness is pinned per worker, so a mixed fleet just works: `msg`
  picks the right transport, `worker-open` revives with the right CLI, and `worker-ls` shows a
  HARNESS column.
- **"Use haiku / gpt-5.4-mini for this"** → `worker-new --model <id> <name> …`. The value is passed
  through unvalidated (`claude --model` / `codex -m` — still a normal interactive session, never
  print/exec mode), and revives keep it. A model implies its harness: a claude alias or `claude-*`
  ID (`haiku`, `claude-sonnet-5`) needs the claude harness, a `gpt-*` slug needs codex — add
  `--harness` too when that isn't the fleet default.
- **`scripts/fleet/models`** lists known-good IDs for both harnesses (from local data — the claude
  binary and codex's model cache; anything the CLI accepts works even if unlisted).

The orchestrator itself is not configurable: you run the fleet default, full stop.

### Solo Workers

Michael sometimes spins up his own **solo** (user-driven) workers alongside yours. You don't manage
these — they stay silent toward you and just show up in `worker-ls` with ROLE `solo`. Leave them be
until one hands off (it'll `msg` you that its branch is ready, or Michael will point you at it); then
integrate it **exactly like any other worker** (`worker-integrate <name>` → review → commit to `main`)
and then **immediately `worker-rm` it in the same step** — a solo lane is done the moment its work
lands. **Auto-teardown is the default for solo workers: do not leave an integrated solo worker
standing or wait to be told to remove it.** The only exception is if Michael explicitly says to keep
it alive (e.g. he's still iterating in that workspace). Integrate-then-`worker-rm` is one motion, not
two decisions.

## See What's Running

```
scripts/fleet/worker-ls
```

Live from `rift list` + tmux: name, whether the session is live, branch, dirty/clean, path. Flags
orphan tmux sessions whose workspace is gone.

## Reopen Or Revive A Worker

```
scripts/fleet/worker-open <name>
```

If you (or Michael) closed a worker's window, the tmux session is still alive: this pops a fresh
Alacritty attached to it. If the worker's session actually died (workspace still present, `TMUX`
shows `-` in `worker-ls`), this revives it with `claude -c` so it continues its last conversation
with context intact, re-applying the worker overlay and re-trusting the workspace path. (Detach from
any session without killing it via `F10`/`F11`/`F12`, or `Ctrl-b d`.)

## Message A Worker (And Answer Their Requests)

```
scripts/fleet/msg usc-worker-<name> "[orchestrator] <text>"
```

- Always prefix `[orchestrator]` so the worker knows it's you and not Michael typing.
- Just use the CLI; `msg` injects the message as a live turn in the target through its inspector
  socket (you never manage waking anything). To an idle worker it arrives instantly; to a busy one it
  queues and runs at the end of its current turn. A draft sitting in the target's input box is
  preserved.
- Overview / who's running: `scripts/fleet/worker-ls`. There is no command to read another session's
  messages — a message is delivered into the target as a turn, not parked in a mailbox.
- Don't *interrupt/redirect* a busy worker by message. For a hard redirect, attach
  (`tmux attach -t usc-worker-<name>`) and do it by hand.

**Receiving a worker's reply.** A worker's `msg` to you arrives **as a turn in your `usc-orch`
session** — `[worker:<name>] ...` shows up as user input, exactly as if it were typed. If you're
mid-turn it queues and runs when you finish; if you're idle it starts a turn right away. So the normal
flow is just *do your work and end your turn* — the reply lands on its own. There's nothing to poll and
no explicit receive command; running autonomously is the same (end the turn, the reply comes in as the
next one).

**Answering a worker's serial request is your core job.** When a worker messages you (it arrives in
your `usc-orch` session as `[worker:<name>] ...`) asking for a rig run, an RE probe, or in-game
validation: run it yourself, serialized against the single rig (see the `/test-loop` and
`/reverse-engineer` skills), then reply with `msg usc-worker-<name> "[orchestrator] <result>"`. Never
hand the rig to a worker.

**Let Michael watch/control a worker from his phone.** When Michael asks to *watch*, *view*, *follow*,
or *remote-control* a worker (especially "from my phone"), inject the remote-control command into that
worker so he doesn't have to type a slash command on mobile:

```
scripts/fleet/msg usc-worker-<name> "/rc"
```

- **No `[orchestrator]` prefix** — this is the one exception to the always-prefix rule. `/rc` (alias
  `/remote-control`) must arrive verbatim as the worker's input so it runs as a slash command; a prefix
  turns it into plain text and it won't trigger. Send exactly `/rc` (or `/remote-control`), nothing else.
- That command is what opens the worker for viewing/control from Michael's phone. Trigger on phrases
  like "let me watch <name>", "I want to view the worker", "follow it from my phone", "remote into it".
- It's just another `msg` injection (live turn), so the usual delivery rules apply (idle → instant,
  busy → queued).
- **Claude workers only.** A codex worker has no `/rc`; tell Michael that instead of sending it.

## Offload The Review To Workers

**The deep per-lane review is the worker's job, not yours.** A worker `/ultracheck`s its own branch
before handoff (its overlay makes that the default), applies the surviving findings, and tells you
which review it ran in its done message. So you inherit a lane that's *already* been through a heavy
fresh-context pass while its author still had full context — the best time to run one. Don't re-run a
full `/ultracheck` on each incoming lane yourself; that's duplicated spend on work already reviewed.

Choose intelligently, and lean on the workers:
- **Trust the worker's `/ultracheck`** as the lane's deep review. At integration, glance at its diff
  for fit and the done-message's review summary; only escalate to your own focused review if something
  looks off, the lane is unusually load-bearing, or the worker says it ran only a light `check`.
- **Downgrade in the brief when a lane is trivial** — a one-file mechanical change doesn't need the
  swarm; tell that worker "a single `check` is enough" so it doesn't over-spend.
- **Your heaviest pass is best spent *holistically*, after integration** — once several lanes are
  merged, run one `/ultracheck` (or a rig validation) over the *combined* result to catch **cross-lane**
  issues (interacting changes, duplicated helpers, contract drift) that no single-lane review could see.
  That's the review only you can do; do it when the integrated surface is nontrivial, skip it when it's
  a clean single lane.

Net: workers cover *within-lane* depth in parallel; you cover *cross-lane* integration review once, and
only when it's worth it. (Subagent reviewers — `check` / `/ultracheck`'s swarm — are still fine as your
*own* support when you do review; see the litmus test in the intro.)

## Integrate A Worker's Branch

When a worker says it's done:

```
scripts/fleet/worker-integrate <name>
```

Fetches `worker/<name>` into `refs/fleet/<name>` and squash-merges it into your current branch, left
**staged but uncommitted** so you write one clean commit. If your own tree is dirty it fetches only
and prints the command to run after you commit/stash. On a genuine conflict it stops (exit 1) and
tells you to resolve, `git add`, and `git commit` (`rerere` replays repeats; `git reset --merge`
abandons). Then commit to `main` per the repo's commit conventions. The worker already deep-reviewed
its own lane (see [Offload the review](#offload-the-review-to-workers)), so here just sanity-check the
diff for fit; save a full `/ultracheck` for a *holistic* pass once multiple lanes are integrated, and
(if appropriate) run the rig to validate the combined result.

## Tear A Worker Down

After its work is integrated (or abandoned):

```
scripts/fleet/worker-rm <name>
```

Kills the tmux/Claude session, trashes the rift workspace, `gc`s, and removes the assignment file.
It **refuses** (exit 1) only if the worker has commits whose patch isn't already on `main` (a
`git cherry` check), since the workspace is the only copy of that branch; pass `-f` to discard them
anyway. A worker you just integrated normally tears down **without `-f`**: its squash-integrated
commit is patch-equal to what's now on `main`, so the check sees it as landed. (`-f` is only needed
when you abandon unintegrated work, or when a worker handed off several commits squashed into one —
which is why the overlay tells workers to consolidate to a single clean commit before done.) It also
warns on uncommitted/untracked working-tree changes, which are usually just the inherited
orchestrator tree. Workers live until you remove them — so **make the removal part of integrating**,
especially for **solo** workers, where teardown-after-integration is the default (see
[Solo Workers](#solo-workers)), not a separate later chore.

## Start A Fresh Orchestrator Session

If you need the orchestrator in its own tmux session (so workers can reach `usc-orch`), Michael runs:

```
scripts/fleet/orch-start
```

It launches Claude in tmux `usc-orch` with `--add-dir` over the rifts tree (so you can fetch worker
branches), no worker overlay (so it's the orchestrator by default), and attaches. A fresh start is
**seeded with the STATE.md boot prompt** — read [`docs/STATE.md`](../../../docs/STATE.md) (which is
about the work, not machine state), then **brief Michael and wait**. The boot orients from the work
picture; it doesn't audit machine state first. It never auto-starts work — Michael decides whether
to continue Next, run `/next`, or do something else. (`--no-seed` skips it; a resumed start —
`--continue`/`--resume`, or `-c` on claude — is never seeded, the context is already there.)

## End A Session Cleanly (/wrap), Decide What's Next (/next)

- **Ending a session** (context is long, work concluded, Michael says wrap up): run **`/wrap`** —
  it sweeps un-encoded learnings into the right docs, rewrites `docs/STATE.md` to reflect the
  current work, and commits, so the session becomes disposable. Then it's safe to kill and
  `orch-start` fresh.
- **Unsure what to work on** (Next completed, plans changed, a new wave): run **`/next`** — it
  enumerates candidates with a gating analysis, records the decision in STATE.md, and drafts
  worker briefs for the delegable ones.
- Contract details: [ORCHESTRATION.md](../../../docs/ORCHESTRATION.md) > "Session Continuity".

## Lifecycle, At A Glance

1. You + Michael pick a lane -> `worker-new <name> "<guidance>"`.
2. Worker builds, WIP-commits to `worker/<name>`, messages you for anything serial.
3. You serve rig/RE requests in order and reply.
4. Worker `/ultracheck`s its own lane, consolidates to one commit, signals done (naming the review it ran).
5. `worker-integrate <name>` -> sanity-check the diff for fit -> commit to `main`.
6. Once several lanes are in, run one *holistic* `/ultracheck` / rig validation over the combined result if warranted.
7. `worker-rm <name>`.

## Gotchas

- **The rig is single and yours.** All rig/RE/validation serializes through you. A worker that tries
  to drive the rig is a bug in its guidance or overlay.
- **Only you commit to `main`.** Workers commit to their own branch; you integrate.
- **Preserve concurrent work** (CLAUDE.md): when integrating, don't clobber a diverged file you
  didn't expect; surface it.
- **Seeded prompt:** `worker-new` seeds the worker by passing its assignment pointer as Claude's
  first prompt, which auto-submits (confirmed by a live test). If a future Claude version ever
  pre-fills instead, the popped Alacritty window shows it ready to send.
