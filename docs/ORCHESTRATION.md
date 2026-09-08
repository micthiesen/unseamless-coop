# Orchestration

How we run concurrent development on this repo: a single **orchestrator** Codex session
plus N **worker** sessions, each in its own [rift](https://github.com/anomalyco/rift) workspace,
coordinated over tmux. This is the contract and the naming; the operational procedure lives in the
`/fleet` skill, the way the global `/triage` skill documents `wt`.

It exists because two things in this project are **inherently serial** and one thing is not:

- The **rig** (one Elden Ring install, one `unseamless-coop/` config+log dir, one Steam) and
  **`main`** can only be driven by one actor at a time. See [RIG-RUNBOOK.md](RIG-RUNBOOK.md).
- **Feature coding** is not. Several features can be built in parallel even when they touch the
  same files, as long as integration is funneled through one actor.

So: workers build in parallel; the orchestrator owns the serial parts (rig, integration, the
commit to `main`) and helps the human decide what to build next. **Delegate by default:** any chunk
of buildable work that doesn't need the rig goes to a worker; the orchestrator does work itself
only when it's serial (rig/RE/validation/integration), takes under ~15 minutes, or *is* the
decision itself. **Core/live RE counts as serial even when it's big** — it's rig-coupled and needs
Michael in the loop, so don't spin up and integrate workers for it; the delegable RE exception is a
genuinely independent *static* search (offline binary triage, decompile sweeps, call-site
charting). The heads-down test: an orchestrator that's been building for a long stretch without
touching the rig is holding a chunk that should have been a worker.

## Roles

Roles are injected at **launch**, never by mutating tracked files (a rift workspace is a full git
repo, so editing `AGENTS.md` there would be a tracked diff that pollutes integration).

- **Orchestrator** is the **default**. `AGENTS.md` states "you are the orchestrator unless a
  worker role is injected," so a normal interactive session in the canonical repo *is* the
  orchestrator with no special flag. It owns: planning with the human, the rig, RE/validation,
  integration, the only commits to `main`, and the worker lifecycle (create, message, remove).
- **Worker** is an overlay. A worker session is launched with
  a seed prompt that reads `docs/roles/worker.md`, which overrides the default
  framing. A worker owns: one lane of feature work, WIP commits to its own
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

## Codex

The fleet runs Codex using native `AGENTS.md`, `.agents/skills/`, and
`.codex/config.toml`. Edit those files directly. No generated copies or AI-config
symlinks are needed.

`worker-new --model <id>` pins a user-requested model; otherwise the user's Codex
default applies. `scripts/fleet/models` lists the local Codex model cache.
`worker-open` resumes the workspace's last Codex session and preserves the model pin.
The role is delivered in the initial prompt and remains in conversation history.

`scripts/fleet/harness` prints `codex`. Old `toggle` shortcuts are harmless no-ops.
Workers with missing or retired harness markers fail with an explanation instead of
resuming an unrelated Codex conversation. Finish or recreate those workers first.
A live orchestrator predating this migration must be restarted with `orch-stop`
and `orch-start`; new launches record `orchestrator.harness` before attaching.

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

- `~/Code/unseamless-coop`: canonical repo and `usc-orch` tmux session.
- `~/Code/.rifts/unseamless-coop/<name>`: worker workspace and `usc-worker-<name>` session.
- `~/.local/share/unseamless-fleet/assignments/<name>.md`: assignment.
- Beside it, `.role`, `.harness`, and optional `.model` record the worker's launch settings.

The shared fleet directory stays outside workspaces so copy-on-write snapshots do
not diverge. `UNSEAMLESS_FLEET_DIR` overrides its location.

## Messaging

`scripts/fleet/msg <session> -` reads stdin and submits a tmux bracketed paste
followed by Enter. It permits only `usc-orch` and `usc-worker-<name>` targets and
fails when the session is offline. Use a quoted heredoc for multiline text.

A paste appends to any draft already in the composer, and a booting TUI can miss
it. Keep the destination composer empty and retry if a freshly launched worker
does not respond. A message is ordinary user input; it queues while Codex works.
Each call uses its own tmux buffer so overlapping senders cannot overwrite it.

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
   commit conventions in [AGENTS.md](../AGENTS.md)).

This is why "workers never commit" is really "workers never commit **to `main`**": git's 3-way
merge and `rerere` need commits to operate on, so workers must commit to their own branch.

### Review happens here — light, and only when warranted

Review in this project is deliberately light (AGENTS.md > "Review is light here"). Most lanes are
*experiments* — RE probes, rig instrumentation, diagnostic levers — and get **no formal review**: the
worker keeps the build green, eyeballs its diff, and says "no review — experiment" in its done message.
The orchestrator integrates those on the strength of the diff and the rig result.

For a lane that lands **something solid** (a real feature or subsystem), the worker runs a light
`/check` or `/tricheck` on its own lane before handoff and names which. The orchestrator then:

- Glances at each lane's diff **rebased onto current `main`** (so it sees interactions with
  already-landed lanes) for fit; it does **not** re-review a lane the worker already reviewed.
- For a nontrivial *integrated* surface — several solid lanes touching shared files (`diag.rs` /
  `features/mod.rs` / `config.rs`) or a refactor meeting another lane's additions — runs **one
  `/tricheck` over the combined result**. That cross-lane pass is the review only the orchestrator can
  do; skip it when the integrated surface is trivial or all-experiment.

**Run any review in the background** (`Agent` with `run_in_background: true`, or `/tricheck` which
backgrounds its agents). A review can take minutes; blocking on it stalls the fleet. Kick it off, keep
serving workers and rig requests, collect the findings when it notifies you. A squash-merge you want to
gate on a review stays staged-not-committed meanwhile, so nothing lands until you've read it.

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

## Permissions And Directories

`_codex` persists workspace trust in the user's Codex config before launch and
removes the worker trust entry on teardown. The existing worker launch policy is
`workspace-write`, network access enabled, and `approval_policy="never"`. Writable
roots include the workspace `.git`, `~/.cargo`, and the shared fleet directory so
workers can commit, build, and coordinate. The orchestrator retains its existing
`danger-full-access` launch policy for rig and integration work.

## Scripts (`scripts/fleet/`)

| Script | Purpose |
| --- | --- |
| `worker-new [--solo] [--model <id>] <name> [guidance]` | Create the rift workspace and branch, persist trust, write markers, launch Codex with role and assignment, open Alacritty. |
| `worker-open <name>` | Attach to a live session or run `codex resume --last` in its workspace with the model pin. |
| `worker-ls` | Inspect rift workspaces and tmux sessions, including branch, dirtiness, commits, and role. |
| `msg <session> <text>` | Submit a normal user turn through tmux; `-` reads stdin. |
| `worker-integrate <name>` | Fetch and squash-merge a first handoff for the orchestrator's main commit. For later deltas, cherry-pick new commits. |
| `worker-rm <name> [-f]` | Remove a worker; refuse unintegrated commits unless explicitly forced. |
| `worker-prune [--all] [-n]` | Clean abandoned solo workers; `-n` previews and `--all` includes live sessions. |
| `rig-verify <worker>... [-- <options>]` | Build the combined verification branch and run the rig cycle. |
| `harness [codex]` | Print the supported harness. |
| `models [codex]` | List locally cached Codex model IDs. |
| `orch-start [--no-seed] [--continue]` | Launch or attach the orchestrator; fresh sessions receive the STATE.md orientation prompt. |
| `orch-stop` | Stop the orchestrator session; workers remain running. |
| `notify-human <reason>` | Send the explicitly authorized stopping notification. |

Detached tmux sessions survive closing their Alacritty window.

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

## Session Continuity (STATE.md, /wrap, /next)

Orchestrator sessions are disposable; the project's fast-moving state is not. The contract that
makes stop-and-restart cheap:

- **[`STATE.md`](STATE.md)** is the single fast-moving "what we're working on / what's next" file —
  **overwritten, never appended** (git holds history). It is **about the work**: the current picture
  (Now), the chosen next step with its why (Next), the runners-up (Candidates Not Chosen), and
  pointers to what a session learned. It does **not** track machine state — no fleet/rig/git
  snapshot. Live workers are `worker-ls` (live, can't drift); rig/Deck state is cheap to re-derive
  and re-apply so it's not worth recording; workers integrate before a wrap so there's nothing
  uncommitted to note. Durable knowledge goes to the proper doc (AGENTS.md > "Project knowledge
  lives in the repo"); STATE.md holds pointers and decisions, never the content.
- **`/wrap`** concludes a session: sweep un-encoded learnings into their homes, decide/confirm Next
  (via `/next` when open), rewrite STATE.md to reflect the current work, commit. Kill a session only
  after a wrap — a session's value must never live only in its context window.
- **`/next`** decides the next step when it's open: 2–4 candidates with a gating analysis (what
  each unblocks, rig-serial vs delegable, size, risk), a recommendation, and the decision recorded
  in STATE.md — with ready-to-paste worker briefs for the delegable candidates (this is what makes
  delegate-by-default cheap to act on).
- **`orch-start`** seeds a fresh orchestrator with a boot prompt that reads STATE.md and briefs
  Michael without auditing machine state first. The boot prompt alone is orientation and leaves the
  session idle. When the user launches it with an instruction to continue the project, it begins the
  recorded Next without asking again. Otherwise Michael may continue Next, run `/next`, or choose
  something else. Restarting remains `/wrap` → kill the session → `orch-start`.

## Away Notifications (Pushover)

`scripts/fleet/notify-human` sends the orchestrator's explicitly authorized stopping
notification through `scripts/notify/pushover`. Configure keys in
`~/.config/unseamless-notify/pushover.env`. Sending fails softly when keys are absent.

The retired harness's activity hooks and fleet-quiet polling timer are no longer
installed. On a Linux rig with the old timer, disable it with
`systemctl --user disable --now unseamless-quiet-check.timer`.
