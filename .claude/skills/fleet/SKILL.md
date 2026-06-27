---
name: fleet
description: >
  Orchestrator playbook for running concurrent development as a one-orchestrator /
  many-worker fleet of Claude Code sessions over rift copy-on-write workspaces,
  coordinated over tmux. Use when spawning a worker to build a feature in parallel,
  messaging or answering a worker, integrating a worker's branch into main, or
  tearing a worker down. TRIGGER on "spawn a worker", "parallelize this", "kick off
  a worker for X", "what are my workers doing", "integrate <worker>", "remove the
  worker". Design + rationale live in docs/ORCHESTRATION.md.
user_invocable: true
---

# Fleet (Orchestrator Playbook)

You are the **orchestrator** (the default role; see [CLAUDE.md](../../../CLAUDE.md) >
"Orchestrator / worker fleet"). This is the operational how-to — everything you need to run the
fleet is here.

The whole point: **workers build features in parallel; you own everything serial** (the rig, RE,
in-game validation, integration, and the only commits to `main`). Spawning workers is optional, not
required: do small or rig-coupled work yourself; reach for a worker when a feature is independent
enough to run on its own for a while.

All tooling is in `scripts/fleet/`. tmux sessions are `usc-orch` (you) and `usc-worker-<name>`.

## Spawn A Worker

```
scripts/fleet/worker-new <name> "<guidance>"
```

`<name>` is kebab-case and becomes the workspace, the branch `worker/<name>`, and the tmux session
`usc-worker-<name>`. It `rift create`s a copy-on-write workspace (runs `.rift.toml` postcreate),
branches it, writes an assignment file, launches Claude there with the worker overlay
(`docs/roles/worker.md`), and pops it open in Alacritty.

Write the guidance like a focused brief: it becomes the worker's assignment file, which its seed
prompt tells it to read first.
- **The lane and its boundary** ("implement the boot_volume feature; don't touch the save path").
- **Where to look** (the relevant `docs/FEATURES.md` section, the module, sibling examples).
- **What's serial** is already covered by the overlay (the worker knows to ask you for any
  rig/RE/validation), so you don't need to repeat it, but flag anything you already know it will
  need from the rig.

Keep workers in genuinely independent lanes when you can. They *may* touch the same files (that's
what `rerere`-assisted integration is for), but overlapping lanes mean more conflict resolution for
you later.

### Solo Workers

Michael sometimes spins up his own **solo** (user-driven) workers alongside yours. You don't manage
these — they stay silent toward you and just show up in `worker-ls` with ROLE `solo`. Leave them be
until one hands off (it'll `msg` you that its branch is ready, or Michael will point you at it); then
integrate it **exactly like any other worker** (`worker-integrate <name>` → review → commit to `main`)
and **`worker-rm` it once integrated** — a solo lane is done when its work lands, so tear it down
automatically unless Michael says to keep it.

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

## Integrate A Worker's Branch

When a worker says it's done:

```
scripts/fleet/worker-integrate <name>
```

Fetches `worker/<name>` into `refs/fleet/<name>` and squash-merges it into your current branch, left
**staged but uncommitted** so you write one clean commit. If your own tree is dirty it fetches only
and prints the command to run after you commit/stash. On a genuine conflict it stops (exit 1) and
tells you to resolve, `git add`, and `git commit` (`rerere` replays repeats; `git reset --merge`
abandons). Then review and commit to `main` per the repo's commit conventions, and (if appropriate)
run the rig to validate the integrated result.

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
orchestrator tree. Workers live until you remove them.

## Start A Fresh Orchestrator Session

If you need the orchestrator in its own tmux session (so workers can reach `usc-orch`), Michael runs:

```
scripts/fleet/orch-start
```

It launches Claude in tmux `usc-orch` with `--add-dir` over the rifts tree (so you can fetch worker
branches), no worker overlay (so it's the orchestrator by default), and attaches.

## Lifecycle, At A Glance

1. You + Michael pick a lane -> `worker-new <name> "<guidance>"`.
2. Worker builds, WIP-commits to `worker/<name>`, messages you for anything serial.
3. You serve rig/RE requests in order and reply.
4. Worker signals done -> `worker-integrate <name>` -> review -> commit to `main` -> validate.
5. `worker-rm <name>`.

## Gotchas

- **The rig is single and yours.** All rig/RE/validation serializes through you. A worker that tries
  to drive the rig is a bug in its guidance or overlay.
- **Only you commit to `main`.** Workers commit to their own branch; you integrate.
- **Preserve concurrent work** (CLAUDE.md): when integrating, don't clobber a diverged file you
  didn't expect; surface it.
- **Seeded prompt:** `worker-new` seeds the worker by passing its assignment pointer as Claude's
  first prompt, which auto-submits (confirmed by a live test). If a future Claude version ever
  pre-fills instead, the popped Alacritty window shows it ready to send.
