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
- **Solo worker** is the same overlay mechanism with a different file
  (`docs/roles/worker-solo.md`, via `worker-new --solo`). It's **user-driven**: Michael drives the
  session interactively (no assignment file; guidance, if any, is its first prompt directly), and it
  **stays silent toward the orchestrator until he hands off** — then it's integrated like any other
  worker. Same isolation, branch, and lifecycle (`worker-ls`/`open`/`integrate`/`rm`/`prune` all work
  on it). Which overlay a worker spawned with is recorded in `assignments/<name>.role` (in the shared
  fleet dir, outside every workspace, so it never COW-diverges) — `worker-open` reads it to revive
  with the right overlay; `worker-rm`/`worker-prune` clean it.

The overlay files (`docs/roles/worker.md`, `docs/roles/worker-solo.md`, and any orchestrator-specific
notes) are **tracked and read-only at runtime** (consumed via `--append-system-prompt-file`), so they
COW into a workspace without ever being mutated there.

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
  ├─ assignments/<name>.md                  per-worker assignment (orchestrator-driven; read at launch)
  ├─ assignments/<name>.role                role marker: "worker" | "solo" (picks the overlay on revive)
  └─ insp/<session>.sock                    per-session inspector socket (messaging endpoint, 0700 dir)
```

The shared dir must live **outside** every rift workspace. Anything inside a workspace is
COW-copied per worker and diverges, so a shared endpoint in the tree would fork. Its absolute path
goes in `.claude/settings.json` under `additionalDirectories`, which COW-propagates read/write
access to every worker.

## Messaging

The transport is **direct in-process injection through each session's inspector socket** — not typing
into the target's TTY, and not a polled mailbox. Every fleet session launches under
`BUN_INSPECT=ws+unix://…/insp/<session>.sock` (worker-new / orch-start / worker-open), which exposes a
JSC/WebKit inspector on a per-session unix socket. `msg <session> "<text>"` calls
`scripts/fleet/_inject`, which connects to that socket, walks the live Ink/React fiber tree to the
prompt-input component, and calls its `onSubmit` — so the message lands as a normal **user turn**,
instantly, with nothing typed over the TTY. `_inject` is the one complex piece; `msg` is a thin
wrapper over it.

What this buys:

- **Instant.** Delivery happens the moment `msg` runs. A *mid-turn* target is fine: `onSubmit` queues
  the message and it runs when the current turn ends, exactly like typing would.
- **Never clobbers a draft.** Before submitting, `_inject` reads the target's input-box draft (the
  prompt's `value`) and re-asserts it via `onChange` after `onSubmit` clears the box, so a draft you
  (or a worker's human) left sitting survives. This holds for **every session, the human-attended
  `usc-orch` included** — there's no "empty box" gate and no special-casing the orchestrator.
- **Receive is visible for free.** The message arrives as a user turn in the target, so anyone
  watching that session sees it (and the response) appear — no separate notification layer.
- **Resilient by structure.** `_inject`'s anchors are semantic React props (`onSubmit` +
  `messagesRef`/`commands`; `value` + `onChange`), never minified names or offsets, so they survive
  minifier churn. `_inject --selftest <session>` verifies them and exits nonzero if a Claude Code
  update moved them — wire it into the smoke test. The `_inject` header documents the mechanism and how
  to re-derive the anchors after an update.

Still true: **prefix every cross-session message** with its source (`[orchestrator] ...` /
`[worker:<name>] ...`) for attribution, and we don't script *interrupting* a busy session — for a hard
redirect, attach and do it by hand. There is intentionally no "read another session's messages"
command: a message is delivered into the target as a turn, not parked in a mailbox someone could rifle.
A target with no live socket (offline, or mid-restart) can't be reached — `msg` fails loudly rather
than queuing, since an offline fleet session is being torn down anyway.

The same socket carries prompt-bar coloring: `_color-inject` sends `/color <name>` through `_inject`
(a slash command is just a message whose text starts with `/`), so there's a single delivery path and
no `send-keys` anywhere. The socket is an in-process code-exec surface, so its dir is mode `0700`.

## Writing a worker assignment

The first-run workers rated the brief the highest-leverage artifact. Make the parts that worked
standard:

- **Own-these-files list + a numbered per-task spec + an explicit SCOPE-GUARD / NEVER list.** Zero
  lane ambiguity.
- **A cross-lane collision map** — for *every* file the lane touches that another lane also touches,
  name the sibling lane, whether it's landed or in-flight, who's authoritative, and the integration
  order. Workers can't see other branches, so this is the one thing they can't self-serve, and it's
  what lets them write merge-friendly diffs instead of guessing. (Both first-run workers' #1 ask.)
- **Approximate pointers are fine — the deep dive is the worker's job.** Cite likely file / line /
  symbol with a "grep to confirm" caveat; they don't need to be exact. A worker finding the real
  location (e.g. a bit-check that's actually in `pad.rs`, not `input.rs`) is the **intended** flow,
  not a brief defect — don't over-research the brief, and **reject** that class of worker feedback
  ("you under-specified the location/type"). Only a wrong *behavioral* instruction (what the feature
  should do) is a real brief error worth correcting.

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

### Ultracheck happens here, per lane

`CLAUDE.md`'s "ultracheck after each holistic chunk" maps onto the fleet as **one ultracheck per
lane, run by the orchestrator at integration** — each lane *is* the holistic chunk and lands as its
own squashed commit. **Workers do not run `/ultracheck`** (a full swarm nested in a rift workspace is
wasteful and reviews a stale base); they run a *lighter* one-shot `check` self-review before
reporting done (see [roles/worker.md](roles/worker.md)). The orchestrator then:

- Reviews each lane **rebased onto current `main`** (so it sees interactions with already-landed
  lanes), not the worker's original fork point.
- Scales depth to the lane: a full `/ultracheck` swarm for logic-heavy lanes; a single `check` agent
  is enough for trivial or already-rig-verified ones. Apply surviving findings before the commit.
- After the whole wave lands, runs **one final seam pass** focused on the cross-lane integration
  points (shared files like `diag.rs` / `features/mod.rs` / `config.rs`, and any refactor that meets
  another lane's additions) — the class of bug per-lane review structurally can't catch.

**Always run review agents in the background** (`Agent` with `run_in_background: true`) — every
`/ultracheck` swarm and every standalone `check`. A review can take minutes; blocking on it stalls
the whole fleet. Kick it off, keep serving workers and rig requests, and collect the findings when
the task notifies you. The squash-merge stays staged-not-committed meanwhile, so nothing lands until
you've read the review.

### Follow-up deltas, lockfiles, and acks

- **Re-integrating a lane after its first squash-merge conflicts.** The worker branch still carries the
  commits you already squashed onto `main`, so `worker-integrate` re-applies them and collides. For a
  *follow-up* commit on an already-landed lane (the iterate-after-review loop), **cherry-pick just the
  new commits** (`git cherry-pick <sha>…`) onto `main` — don't re-run `worker-integrate`.
- **Tell the worker the integration SHA** ("integrated through `<sha>`") when a landed lane may get
  follow-ups. The worker can't see `main`, so on a re-touch of the same file it's otherwise trusting
  its branch base blindly.
- **Lockfile / dep bumps are orchestrator-owned at integration.** A worker adding a dependency mutates
  `Cargo.lock` (shared artifact) — a latent cross-lane conflict. Don't hand-merge `Cargo.lock`;
  regenerate it (a plain `cargo build`) after merging the lanes.

## The Rig Is Single and Orchestrator-Owned

A worker that needs a rig run, an RE probe, or in-game validation **asks the orchestrator** by
message and waits. The orchestrator serializes these against the one game install. No worker drives
`scripts/rig.sh`, launches the game, or reads a live log. This is the core reason the role split
exists; see [RIG-RUNBOOK.md](RIG-RUNBOOK.md) and the `/test-loop` skill (orchestrator-only).

**Batch rig passes when you can.** A game launch is the expensive, serial step, so when several
lanes have rig-dependent probes pending, prefer combining their probe branches into one diag build
and observing them in a single play session over launching per-lane. It costs one early seam-merge
(rerere caches it for final integration) but collapses N launches into one and lets you feed every
lane its values together. Probes are designed inert-by-default, so they coexist safely in one build.

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
- **Inspector socket** (launch flag on *both* orch and workers): `BUN_INSPECT=ws+unix://…/insp/<session>.sock`
  opens the per-session inspector that `msg`/`_inject`/coloring deliver through (see Messaging). `env
  UNSEAMLESS_FLEET_DIR=<dir>` is propagated so the session and `msg` agree on the socket dir. Opening
  the inspector is inert until something connects, and the socket dir is mode `0700` (the socket is an
  in-process code-exec surface). No `--settings` and no lifecycle hooks: the socket is the whole
  transport.

## Scripts (`scripts/fleet/`)

| Script | Does |
|--------|------|
| `worker-new [--solo] <name> "<guidance>"` | `rift create` the workspace, run postcreate setup, branch `worker/<name>`, trust the path in `~/.claude.json`, write a `.role` marker, launch `claude` in `tmux usc-worker-<name>` under `BUN_INSPECT` with the worker overlay, then pop an Alacritty window. Default: orchestrator-driven — writes an assignment file + seeds the session to read it. `--solo`: user-driven (`worker-solo.md`) — no assignment file; guidance (if any) is the first prompt directly, else launches waiting. |
| `msg <session> "<text>"` | deliver the message as a live **user turn** in the target via its inspector socket (see Messaging): instant, queues if the target is mid-turn, preserves any draft in its box. Target restricted to `usc-*` sessions; fails loudly if the target has no live socket. |
| `_inject` | internal: the one complex piece. Connects to a session's inspector socket, walks the live React tree to the prompt component, and calls `onSubmit` to submit a message (or a `/slash` command), saving+restoring the draft. `--selftest <session>` checks the structural anchors and exits nonzero if a Claude Code update moved them. |
| `_color-inject <session> <color>` | internal, best-effort, detached: waits for the session's socket + prompt to be ready, then sends `/color <name>` through `_inject`. |
| `worker-ls` | list workers, derived live from `rift list` + tmux (no registry file to drift); flags orphan sessions. A **ROLE** column (`worker`/`solo`/`-`, from the `.role` marker) shows which are solo. |
| `worker-open <name>` | reopen a worker's window: attach if the session is live, or revive a dead session with `claude -c` (re-applies the overlay, re-trusts the path). |
| `worker-rm <name> [-f]` | `tmux kill-session`, trash the workspace (`rift remove --force` + `gc`), drop the registry (assignment + `.role` marker + inspector socket). Refuses without `-f` only if `worker/<name>` has a commit whose patch isn't on `main` (a `git cherry` check, so a squash-landed lane is recognized as integrated and needs **no** `-f`). `-f` is for abandoning unintegrated work, or a lane handed off as several commits squashed into one (workers consolidate to one commit before done, per the overlay). |
| `worker-integrate <name>` | fetch the worker branch into `refs/fleet/<name>`, squash-merge, leave it staged for the orchestrator's `main` commit (fetch-only if the canonical tree is dirty). **First integration only** — for a follow-up on an already-landed lane, `git cherry-pick` the new commits instead (re-running this re-applies the squashed commits and conflicts). |
| `worker-prune [--all] [-n]` | bulk-clean abandoned **solo** workers (Michael `ctrl+d`-exits and forgets them): trash workspace + kill tmux + drop registry (assignment + `.role` + inspector socket), for solo workers whose session is **dead** (spares live ones; `--all` includes live, `-n` dry-runs). Only ever touches `solo`-role workers; orchestrator-driven lanes use `worker-rm`. Low-safety bulk path — force-discards with no commit check. Also kills orphan `usc-worker-*` sessions whose workspace is gone. |
| `rig-verify <worker>… [-- <cycle opts>]` | build `rig/verify` = `main` + the named lanes, then `rig.sh cycle` — the orchestrator's one-command multi-lane rig check. Don't hand-roll branch+merge+apply+launch. |
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
  messaging could replace our inspector-injection transport; rift already supplies the isolation
  Teams lacks. Pilot separately before betting the workflow on an experimental flag.
- **Warm-cache measurement.** Confirm whether a copied `target/` ever gives cargo a usable cache
  before reconsidering the "one cold build per worker" stance.

## Status

Implemented: the worker overlay (`docs/roles/worker.md`), `scripts/fleet/`
(`worker-new`/`worker-ls`/`worker-open`/`worker-rm`/`worker-integrate`/`msg`/`orch-start`, plus the
inspector-injection transport: `_inject`/`_color-inject`), `.rift.toml`, the
`.claude/settings.json` allowlist + `additionalDirectories`, workspace-trust wiring, the `CLAUDE.md`
role preamble, and the `/fleet` orchestrator skill.

Exercised end to end: a live ping worker confirmed spawn, the seeded prompt auto-submitting, the
worker overlay applying, bidirectional `msg` (orchestrator <-> worker), and teardown.

**First real run (2026-06, Wave 1 — 5 concurrent feature/polish workers).** Confirmed: parallel
lanes build green in isolation; a worker handed back a precise rig recipe over `msg`. Fixes that
came out of it: the restore postcreate hook
(above), and a `worker-ls` **AHEAD** column = commits on `worker/<name>` beyond the workspace's own
`main` (computed per-workspace, since the branches live in the independent clones, not the
orchestrator repo) — the at-a-glance "has this worker produced anything yet?" signal. Still to
prove: a feature worker integrated to `main` and a batched rig pass feeding values back to multiple
lanes.
