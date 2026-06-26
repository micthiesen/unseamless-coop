# Orchestration

How we run concurrent development on this repo: a single **orchestrator** Claude Code session
plus N **worker** sessions, each in its own [rift](https://github.com/anomalyco/rift) workspace,
coordinated over tmux. This is the contract and the naming; the operational procedure lives in the
`/fleet` skill, the way the global `/triage` skill documents `wt`.

It exists because two things in this project are **inherently serial** and one thing is not:

- The **rig** (one Elden Ring install, one `unseamless-coop/` config+log dir, one Steam) and
  **`main`** can only be driven by one actor at a time. See [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
- **Feature coding** is not. Several features can be built in parallel even when they touch the
  same files, as long as integration is funneled through one actor.

So: workers build in parallel; the orchestrator owns the serial parts (rig, integration, the
commit to `main`) and helps the human decide what to build next. The orchestrator can also just
do work itself; spawning workers is a tool, not a requirement.

## Roles

Roles are injected at **launch**, never by mutating tracked files (a rift workspace is a full git
repo, so editing `CLAUDE.md` there would be a tracked diff that pollutes integration).

- **Orchestrator** is the **default**. `CLAUDE.md` states "you are the orchestrator unless a
  worker role is injected," so a normal interactive session in the canonical repo *is* the
  orchestrator with no special flag. It owns: planning with the human, the rig, RE/validation,
  integration, the only commits to `main`, and the worker lifecycle (create, message, remove).
- **Worker** is an overlay. A worker session is launched with
  `--append-system-prompt-file docs/roles/worker.md`, which appends after `CLAUDE.md` and
  overrides the default framing. A worker owns: one lane of feature work, WIP commits to its own
  branch, and asking the orchestrator (by message) for anything serial. A worker **never** drives
  the rig and **never** commits to `main`.

The overlay files (`docs/roles/worker.md`, and any orchestrator-specific notes) are **tracked and
read-only at runtime** (consumed via `--append-system-prompt-file`), so they COW into a workspace
without ever being mutated there.

## Why rift, Not Git Worktrees

rift gives copy-on-write workspaces (btrfs reflinks on this machine), so a workspace clones in
under a tenth of a second at near-zero disk cost. Verified properties that the design leans on:

- Each workspace is a **full independent git repo** (its own `.git` directory, not a worktree
  gitlink), starting on **detached HEAD at the orchestrator's commit**, with the orchestrator's
  uncommitted working tree copied in.
- Integration is therefore **git over a filesystem path**:
  `git fetch ~/Code/.rifts/unseamless-coop/<name> worker/<name>`. Cheap, because the workspaces
  share COW history.
- `rift create` **excludes `target/` by default** (the ~4 GB Rust build dir). We do **not** try to
  carry it: cargo embeds absolute paths in fingerprints and build-script output, so a reflinked
  `target/` at a new path tends to invalidate anyway. Workers are long-lived, so one cold build
  per worker amortizes to nothing. Revisit only if it bites.

## Layout

```
~/Code/unseamless-coop                      canonical repo  -> orchestrator (tmux: usc-orch)
~/Code/.rifts/unseamless-coop/<name>        worker workspace -> worker (tmux: usc-worker-<name>)
~/.local/share/unseamless-fleet/            shared dir (OUTSIDE all workspaces)
  ├─ assignments/<name>.md                  per-worker assignment (read at launch)
  ├─ payloads/                              long message payloads (temp files)
  └─ .msg.lock                              flock for serialized sends
```

The shared dir must live **outside** every rift workspace. Anything inside a workspace is
COW-copied per worker and diverges, so a shared mailbox in the tree would fork. Its absolute path
goes in `.claude/settings.json` under `additionalDirectories`, which COW-propagates read/write
access to every worker.

## Messaging

The transport is **tmux `send-keys`** into a session's pane, i.e. the same mechanism as
`wt claude send`. It delivers to an ongoing conversation, not just an idle prompt: a message to an
**idle** worker starts a new turn (wakes it); a message to a **busy** worker queues as its next
input. Both are fine and are how a human types into a session anyway.

A `msg` wrapper standardizes it:

- `tmux send-keys -t <session> -l '<text>'` then a separate `send-keys -t <session> Enter`. The
  `-l` (literal) flag stops tmux interpreting key names inside the message.
- Wrap in `flock` so two simultaneous sends to one pane can't interleave and garble input.
- **Prefix every cross-session message** with its source, e.g. `[orchestrator] ...` or
  `[worker:auth] ...`. send-keys input is indistinguishable from the human typing, so the
  attribution is how the receiver (and the human watching) knows where it came from.
- **Long guidance goes to a file** in `~/.local/share/unseamless-fleet/payloads/`; send only a
  short `read <path>`.

We do **not** script interrupting a busy worker (send-keys + `Esc` to interrupt is racy). For the
rare hard redirect, attach to the worker and do it by hand. Note `usc-orch` is the pane you may be
typing in live: a worker message arriving mid-keystroke interleaves with your input, and a message to
a busy orchestrator queues silently (no receipt), so a worker's "wait for the orchestrator" can stall
if you don't notice it.

## Integration

The only path code reaches `main`:

1. Worker WIP-commits freely to `worker/<name>` (messy commits are fine; they are not the final
   history).
2. Worker signals done by message.
3. Orchestrator `git fetch`es the worker branch by path into `refs/fleet/<name>` and squash-merges
   it. `rerere` is enabled in this repo, so recurring conflicts across workers resolve once and
   replay.
4. Orchestrator squashes to one clean commit on `main` with a proper message (per the repo's
   commit conventions in [CLAUDE.md](../CLAUDE.md)).

This is why "workers never commit" is really "workers never commit **to `main`**": git's 3-way
merge and `rerere` need commits to operate on, so workers must commit to their own branch.

## The Rig Is Single and Orchestrator-Owned

A worker that needs a rig run, an RE probe, or in-game validation **asks the orchestrator** by
message and waits. The orchestrator serializes these against the one game install. No worker drives
`scripts/rig.sh`, launches the game, or reads a live log. This is the core reason the role split
exists; see [RIG-RUNBOOK.md](RIG-RUNBOOK.md) and the `/test-loop` skill (orchestrator-only).

## Permissions and Directories

- **Shared, COW-propagated** (checked-in `.claude/settings.json`, so every worker inherits it):
  - `additionalDirectories`: the absolute shared-dir path (`~/.local/share/unseamless-fleet`).
  - a build-loop allowlist so workers don't prompt on every cycle: `cargo build`/`check`/`clippy`/
    `test`/`fetch`, `scripts/test-core.sh`, `scripts/fleet/msg`, `scripts/fleet/worker-ls`, and git
    incl. `add`/`commit`/`switch`/`stash`/`fetch`/`merge` (workers commit to their own branch;
    `git push` is deliberately omitted so it still prompts).
- **Workspace trust.** Claude Code drops a project's `allow`/`additionalDirectories` on an untrusted
  path, and each new rift workspace path is untrusted by default, so `worker-new`/`worker-open` set
  `projects["<ws>"].hasTrustDialogAccepted = true` in `~/.claude.json` (live, not git-tracked) before
  launching the worker. Without it a worker silently loses its permissions.
- **Orchestrator-only** (launch flag, kept OUT of settings files so workers stay isolated):
  `--add-dir ~/Code/.rifts/unseamless-coop` so the orchestrator can reach worker repos to
  integrate. Workers must not see each other's workspaces.

## Scripts (`scripts/fleet/`)

| Script | Does |
|--------|------|
| `worker-new <name> "<guidance>"` | `rift create` the workspace, run postcreate setup, branch `worker/<name>`, trust the path in `~/.claude.json`, write an assignment file, launch `claude` in `tmux usc-worker-<name>` with the worker overlay seeded to read the assignment, then pop an Alacritty window. |
| `msg <session> "<text>"` | the flock'd `send-keys` wrapper above (target restricted to `usc-*` sessions). |
| `worker-ls` | list workers, derived live from `rift list` + tmux (no registry file to drift); flags orphan sessions. |
| `worker-open <name>` | reopen a worker's window: attach if the session is live, or revive a dead session with `claude -c` (re-applies the overlay, re-trusts the path). |
| `worker-rm <name> [-f]` | `tmux kill-session`, trash the workspace (`rift remove --force` + `gc`), drop the assignment file. Refuses without `-f` if `worker/<name>` has unintegrated commits. |
| `worker-integrate <name>` | fetch the worker branch into `refs/fleet/<name>`, squash-merge, leave it staged for the orchestrator's `main` commit (fetch-only if the canonical tree is dirty). |
| `orch-start` (optional) | launch the orchestrator session with the `--add-dir` flag set. |

Detached-first tmux (`new-session -d`) is what makes "a worker lives until the orchestrator removes
it" true: closing the Alacritty window detaches but does not kill the session, and the CC session
inside stays resumable.

## rift Postcreate Hooks

A `.rift.toml` at the source root (committed) drives per-workspace setup. `rift create` runs its
`[[hooks.postcreate]]` entries **in the new workspace root** after the copy (skip with `--no-hooks`;
a failing hook fails the create). Ours warms the dependency cache and repairs the copy:

```toml
version = 1

[[hooks.postcreate]]
run = "cargo fetch --locked"

[[hooks.postcreate]]
run = "git ls-files -d -z | xargs -0 -r git checkout --"
```

The cargo registry lives in `$HOME`, shared across workspaces, so the fetch is near-instant. Do
**not** run `cargo build` here (cold, blocks session start) and do not try to copy `target/`.

**The restore hook (first-run gotcha).** rift's COW copy omits build-output dir *names* — it skips
`target/`, but that also catches our **force-tracked `scripts/dist/`** (git does not ignore it, yet
rift drops it). The workspace index still has those files, so they show as spurious deletions, and a
stray `git add -A` on a worker branch would commit the drop. The second hook restores any tracked
path missing from the worktree (`ls-files -d` → `checkout`); it only touches missing files, never
clobbering real edits. Do **not** use `rift create --copy-all` for this — it would also re-copy
`target/`, the very thing we avoid.

## Worker Lifecycle

1. Orchestrator and human agree on a lane.
2. `worker-new` materializes workspace + branch + tmux + window + initial guidance.
3. Worker builds, WIP-commits to `worker/<name>`, messages the orchestrator for anything serial.
4. Orchestrator serves rig/RE requests in serial order and answers.
5. Worker signals done; orchestrator integrates to `main`.
6. `worker-rm` tears the worker down.

## Open Items

- **Agent Teams.** If Claude Code's experimental Agent Teams matures, its native lead/teammate
  messaging could replace the tmux transport; rift already supplies the isolation Teams lacks. Pilot
  separately before betting the workflow on an experimental flag.
- **Warm-cache measurement.** Confirm whether a copied `target/` ever gives cargo a usable cache
  before reconsidering the "one cold build per worker" stance.

## Status

Implemented: the worker overlay (`docs/roles/worker.md`), `scripts/fleet/`
(`worker-new`/`worker-ls`/`worker-open`/`worker-rm`/`worker-integrate`/`msg`/`orch-start`),
`.rift.toml`, the `.claude/settings.json` allowlist + `additionalDirectories`, workspace-trust
wiring, the `CLAUDE.md` role preamble, and the `/fleet` orchestrator skill.

Exercised end to end: a live ping worker confirmed spawn, the seeded prompt auto-submitting, the
worker overlay applying, bidirectional `msg` (orchestrator <-> worker), and teardown.

**First real run (2026-06, Wave 1 — 5 concurrent feature/polish workers).** Confirmed: parallel
lanes build green in isolation; a worker handed back a precise rig recipe over `msg` (the spill-to-
file path triggers for long messages). Fixes that came out of it: the restore postcreate hook
(above), and a `worker-ls` **AHEAD** column = commits on `worker/<name>` beyond the workspace's own
`main` (computed per-workspace, since the branches live in the independent clones, not the
orchestrator repo) — the at-a-glance "has this worker produced anything yet?" signal. Still to
prove: a feature worker integrated to `main` and a batched rig pass feeding values back to multiple
lanes.
