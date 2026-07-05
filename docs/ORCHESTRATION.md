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
repo, so editing `CLAUDE.md` there would be a tracked diff that pollutes integration).

- **Orchestrator** is the **default**. `CLAUDE.md` states "you are the orchestrator unless a
  worker role is injected," so a normal interactive session in the canonical repo *is* the
  orchestrator with no special flag. It owns: planning with the human, the rig, RE/validation,
  integration, the only commits to `main`, and the worker lifecycle (create, message, remove).
- **Worker** is an overlay. A worker session is launched with
  `--append-system-prompt-file docs/roles/worker.md` (claude; under codex the same file is instead
  delivered as the first thing in the seed prompt — see Harness), which overrides the default
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

## Harness (Claude Code Or Codex)

The fleet runs on **Claude Code** (`claude`, the default) or **Codex** (`codex`) — same scripts,
same lifecycle, same roles. The **default** is a single machine-global state file,
`$UNSEAMLESS_FLEET_DIR/harness` (shared fleet dir, outside every workspace, for the same
no-COW-divergence reason as the `.role` markers; missing file == claude):

```
scripts/fleet/harness              # print current default: claude | codex
scripts/fleet/harness codex        # set
scripts/fleet/harness toggle       # flip (also the unseamless-toggle-harness .desktop item)
```

Every set/toggle fires a desktop notification with the new value (the .desktop item runs with no
terminal, so that's its feedback). The **orchestrator always runs the default** (it has no
override), and sessions already running keep the harness they launched with.

**Per-worker overrides (opt-in).** A single worker can be spawned off-default with
`worker-new --harness <claude|codex>`, and/or on a specific model with `worker-new --model <id>`
(passed through unvalidated — `claude --model` / `codex -m`, both plain flags on a normal
interactive session; `scripts/fleet/models` lists known-good IDs from local data only). What makes
the mixed fleet safe is that **everything downstream resolves per worker, not globally**:
`worker-new` pins each worker's spawn harness in `assignments/<name>.harness` (and its model, if
overridden, in `assignments/<name>.model`), and `msg` (transport choice), `worker-open` (revive
CLI), and `worker-ls` (HARNESS column, SOCK logic) all read the harness marker via
`fleet_worker_harness`, falling back to the global default only for markerless pre-marker spawns
(`worker-open` additionally re-applies the separate `.model` pin on revive).
The same pinning is why a global toggle can't strand running workers (it can't make `worker-open`
`codex resume` a claude worker's workspace, which would have no codex session to continue).

What differs per harness (all encapsulated in the scripts; verified end to end with a live codex
ping worker, both message directions):

| | claude | codex |
|---|---|---|
| Role injection | `--append-system-prompt-file` overlay at launch | the role instruction is the **first thing in the seed prompt** (no system-prompt flag exists; being turn 1, it survives `resume` revives for free) |
| Messaging (`msg`) | inspector-socket injection (`_inject`): instant, draft-preserving | tmux **bracketed paste + Enter** into the pane: lands as a user turn, queues mid-turn like typing, but a draft in the composer is **not** preserved and a still-booting TUI can drop the paste |
| Workspace trust | `~/.claude.json` `hasTrustDialogAccepted` (jq edit) | `[projects."<ws>"] trust_level = "trusted"` **persisted into `$CODEX_HOME/config.toml`** by `fleet_codex_trust_add` (`scripts/fleet/_codex`), removed again by `worker-rm`. A session `-c projects...trust_level` flag parses but the "do you trust this folder?" gate ignores it (verified on codex-cli 0.142.5), so the file write — the same entry codex persists on a manual "Yes" — is the only way to skip the prompt. The config is a stowed dotfiles symlink; the helper writes *through* it (`>>`/`cat >`, never `mv`) |
| Sandbox | `.claude/settings.json` allowlist | workspace-write **+ `-c sandbox_workspace_write.network_access=true`** — without it codex's seccomp blocks unix sockets, so the session's own `msg` (a tmux client) can't reach the tmux server (verified; the `network.*` keys do NOT lift it) |
| Pre-approved work (no prompts) | `.claude/settings.json` `allow` rules + `additionalDirectories`, unlocked by the trust write | **`-c approval_policy="never"`** (a worker must never sit on an approval modal — nobody watches the pane, and a `msg` paste + Enter would answer it blind; out-of-sandbox commands fail back to the model, which surfaces the blocker) **+ `-c sandbox_workspace_write.writable_roots=["<ws>/.git", "~/.cargo", FLEET_DIR]`** (all verified with `codex sandbox`: workspace-write **carves the workspace's own `.git` out of the writable cwd** — git config/hooks anti-tampering; there's no `allow_git_writes`-style key — so without the explicit `.git` root every `git add/commit` dies with `Unable to create .git/index.lock: Read-only file system` and the worker can't WIP-commit to its branch; `~/.cargo` is mounted read-only, so `cargo fetch/build` registry writes otherwise fail/escalate; the sandbox, not approvals, is the permission boundary, so widen `writable_roots` rather than loosening the policy) |
| Revive (`worker-open`) | `claude -c` + re-overlay + re-`BUN_INSPECT` | `codex resume --last` (cwd-filters to the workspace, so it continues that worker's own conversation) |
| `/color`, `/rc` remote control | yes | not available (Claude Code features; codex sessions go uncolored) |
| Repo instructions & skills | `CLAUDE.md`, `.claude/skills/` | same content via tracked symlinks: `AGENTS.md -> CLAUDE.md`, `.codex/skills -> .claude/skills` (codex does **not** read `.claude/skills` at project level; it does pick up user-level `~/.claude/skills` natively, and it reads only `name`+`description` frontmatter, tolerating the extra claude fields) |

**Workers vs. `Agent`/`Task` subagents.** Fanning out a *chunk of buildable work* (a feature lane, a
big *static* RE search, a migration — anything whose result is a branch to integrate) is **always** a fleet
worker, never an `Agent`-tool subagent, even for a single lane. A worker is visible (`worker-ls`),
watchable by the human, branch-isolated, and integrated through the normal review path; a subagent is an
invisible black box that produces no integrable branch and can't be watched or reviewed. Subagents stay
valid only for **supporting** tasks that feed the orchestrator's own work and return *findings, not a
deliverable*: running tests, locating code (`Explore`), grep-and-summarize research, review agents
(`/check`, `/tricheck`). Litmus: *would the result be a branch merged to `main`?* → worker; *just
informing your own work?* → subagent. This is the orchestrator-specific override of the global "spawn
subagents aggressively" guidance.

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
  ├─ assignments/<name>.harness             harness marker: what the worker was SPAWNED with (msg/revive/ls pin)
  ├─ assignments/<name>.model               model marker: --model override at spawn, if any (revive pin)
  ├─ harness                                fleet DEFAULT harness: "claude" (default) | "codex" (see Harness)
  └─ insp/<session>.sock                    per-session inspector socket (messaging endpoint, 0700 dir)
```

The shared dir must live **outside** every rift workspace. Anything inside a workspace is
COW-copied per worker and diverges, so a shared endpoint in the tree would fork. Its absolute path
goes in `.claude/settings.json` under `additionalDirectories`, which COW-propagates read/write
access to every worker.

## Messaging

*(This section describes the **claude** transport. For a **codex** target, `msg` instead does a
tmux bracketed paste + Enter into the target pane — see Harness above for the differences. `msg`
picks the transport per target: the worker's `.harness` marker for `usc-worker-*`, the fleet
default for `usc-orch`. The conventions — source prefixes, `usc-*`-only targets, no interrupting a
busy session — apply to both.)*

The claude transport is **direct in-process injection through each session's inspector socket** — not
typing into the target's TTY, and not a polled mailbox. Every fleet session launches under
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

### Review happens here — light, and only when warranted

Review in this project is deliberately light (CLAUDE.md > "Review is light here"). Most lanes are
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
| `worker-new [--solo] [--harness claude\|codex] [--model <id>] <name> "<guidance>"` | `rift create` the workspace, run postcreate setup, branch `worker/<name>`, trust the path in `~/.claude.json`, write `.role`/`.harness` (and `.model`, if overridden) markers, launch the harness CLI in `tmux usc-worker-<name>` (claude: under `BUN_INSPECT` with the worker overlay), then pop an Alacritty window. Default: orchestrator-driven — writes an assignment file + seeds the session to read it. `--solo`: user-driven (`worker-solo.md`) — no assignment file; guidance (if any) is the first prompt directly, else launches waiting. `--harness`/`--model`: opt-in per-worker overrides of the fleet default (see Harness). |
| `msg <session> "<text>"` | deliver the message as a live **user turn** in the target via its inspector socket (see Messaging): instant, queues if the target is mid-turn, preserves any draft in its box. Target restricted to `usc-*` sessions; fails loudly if the target has no live socket. |
| `_inject` | internal: the one complex piece. Connects to a session's inspector socket, walks the live React tree to the prompt component, and calls `onSubmit` to submit a message (or a `/slash` command), saving+restoring the draft. `--selftest <session>` checks the structural anchors and exits nonzero if a Claude Code update moved them. |
| `_color-inject <session> <color>` | internal, best-effort, detached: waits for the session's socket + prompt to be ready, then sends `/color <name>` through `_inject`. |
| `worker-ls` | list workers, derived live from `rift list` + tmux (no registry file to drift); flags orphan sessions. A **ROLE** column (`worker`/`solo`/`-`, from the `.role` marker) shows which are solo; a **HARNESS** column (from the `.harness` marker, default fallback) shows each worker's CLI in a mixed fleet. |
| `worker-open <name>` | reopen a worker's window: attach if the session is live, or revive a dead session with `claude -c` (re-applies the overlay, re-trusts the path) / `codex resume --last`, per the worker's `.harness` marker, re-applying its `.model` pin if one was set. |
| `worker-rm <name> [-f]` | `tmux kill-session`, trash the workspace (`rift remove --force` + `gc`), drop the registry (assignment + `.role`/`.harness`/`.model` markers + inspector socket). Refuses without `-f` only if `worker/<name>` has a commit whose patch isn't on `main` (a `git cherry` check, so a squash-landed lane is recognized as integrated and needs **no** `-f`). `-f` is for abandoning unintegrated work, or a lane handed off as several commits squashed into one (workers consolidate to one commit before done, per the overlay). |
| `worker-integrate <name>` | fetch the worker branch into `refs/fleet/<name>`, squash-merge, leave it staged for the orchestrator's `main` commit (fetch-only if the canonical tree is dirty). **First integration only** — for a follow-up on an already-landed lane, `git cherry-pick` the new commits instead (re-running this re-applies the squashed commits and conflicts). |
| `worker-prune [--all] [-n]` | bulk-clean abandoned **solo** workers (Michael `ctrl+d`-exits and forgets them): trash workspace + kill tmux + drop registry (assignment + markers + inspector socket), for solo workers whose session is **dead** (spares live ones; `--all` includes live, `-n` dry-runs). Only ever touches `solo`-role workers; orchestrator-driven lanes use `worker-rm`. Low-safety bulk path — force-discards with no commit check. Also kills orphan `usc-worker-*` sessions whose workspace is gone. |
| `rig-verify <worker>… [-- <cycle opts>]` | build `rig/verify` = `main` + the named lanes, then `rig.sh cycle` — the orchestrator's one-command multi-lane rig check. Don't hand-roll branch+merge+apply+launch. |
| `harness [claude\|codex\|toggle]` | print or switch the DEFAULT CLI harness the fleet spawns (see Harness above; `worker-new --harness` overrides it per worker). Always fires a desktop notification on a switch; live sessions keep the harness they launched with. |
| `models [claude\|codex]` | list known-good model IDs for `worker-new --model`, per harness, from local data only (claude: aliases + full IDs grepped from the installed binary, newest per family; codex: `~/.codex/models_cache.json` slugs). Informational — the flag is pass-through, so unlisted IDs the CLI accepts still work. |
| `orch-start` (optional) | launch the orchestrator session with the `--add-dir` flag set, seeded with the STATE.md boot prompt (read STATE → brief Michael and wait, no auto-start and no machine-state audit; skip with `--no-seed`, auto-skipped on resume flags: `--continue`/`--resume`, plus `-c` on claude only — codex's `-c` is its config-override flag). |
| `orch-stop` | fully tear down the orchestrator: kill the `usc-orch` tmux session (closing the window only detaches) + remove its inspector socket. Workers untouched. Terminal-less friendly (desktop notification is the feedback) — it backs the `unseamless-orch-stop.desktop` item and the OliveTin button. |
| `notify-human "<reason>"` | high-priority Pushover push to Michael's phone — run once when *stopping*: done, giving up, or blocked on something only he can do (see "Away Notifications" below). Same-stop dedup vs the fleet-quiet backstop ping; fails soft without keys. |

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

## Session Continuity (STATE.md, /wrap, /next)

Orchestrator sessions are disposable; the project's fast-moving state is not. The contract that
makes stop-and-restart cheap:

- **[`STATE.md`](STATE.md)** is the single fast-moving "what we're working on / what's next" file —
  **overwritten, never appended** (git holds history). It is **about the work**: the current picture
  (Now), the chosen next step with its why (Next), the runners-up (Candidates Not Chosen), and
  pointers to what a session learned. It does **not** track machine state — no fleet/rig/git
  snapshot. Live workers are `worker-ls` (live, can't drift); rig/Deck state is cheap to re-derive
  and re-apply so it's not worth recording; workers integrate before a wrap so there's nothing
  uncommitted to note. Durable knowledge goes to the proper doc (CLAUDE.md > "Project knowledge
  lives in the repo"); STATE.md holds pointers and decisions, never the content.
- **`/wrap`** concludes a session: sweep un-encoded learnings into their homes, decide/confirm Next
  (via `/next` when open), rewrite STATE.md to reflect the current work, commit. Kill a session only
  after a wrap — a session's value must never live only in its context window.
- **`/next`** decides the next step when it's open: 2–4 candidates with a gating analysis (what
  each unblocks, rig-serial vs delegable, size, risk), a recommendation, and the decision recorded
  in STATE.md — with ready-to-paste worker briefs for the delegable candidates (this is what makes
  delegate-by-default cheap to act on).
- **`orch-start`** seeds a fresh orchestrator with a boot prompt: read STATE.md, then **brief
  Michael and wait**. The boot orients from the work picture and gets moving; it doesn't audit
  machine state first (that's `worker-ls` on demand, and re-applying the rig if a rig task comes
  up). It never auto-starts work — Michael may continue Next, run `/next`, or do something else
  entirely. Restarting the orchestrator is therefore three motions: `/wrap` → kill the session →
  `orch-start`, with the new session landing oriented but idle.

## Away Notifications (Pushover)

Michael gets a push on his phone when the fleet **stops needing to run without him** — the
orchestrator finished or gave up, a session is blocked on a permission prompt, or everything has
gone quiet. Three layers, from precise to guaranteed (all in `scripts/notify/`, plus
`scripts/fleet/notify-human`):

1. **Explicit — `scripts/fleet/notify-human "<one-line reason>"`.** The orchestrator runs this
   once when it's *stopping* — work done, giving up, or blocked on something only Michael can do
   (rig/Deck/in-game validation, a judgment call) — never for progress updates. High-priority push
   with a real reason — this is the message you *want* to receive. Its `.human-notified` marker
   gives layer 3 **same-stop dedup**: the generic "fleet quiet" ping is skipped when the fleet's
   last activity falls within `HUMAN_GRACE_SECS` (default 5 min) of the marker — a grace band
   that absorbs the pinging turn's own tail (remaining tool calls, wrap-up commits, the Stop
   hook). Honestly stated: a short work burst that starts *and* settles inside that band is also
   absorbed; work that settles later pings normally. Concurrent explicit pings are allowed (set
   `NOTIFY_HUMAN_RATE_SECS` > 0 to opt into a minimum gap), and a *failed* explicit push doesn't
   write the marker, so the backstop still covers that stop. Model-dependent, hence layers 2–3.
   Deliberately the ONLY layer agents are told about (one CLAUDE.md bullet); layers 2–3 are pure
   infrastructure and stay agent-invisible.
2. **Blocked-on-approval — the `Notification` hook** (`scripts/notify/notification-hook`). A
   permission request means a session is stuck on Michael right now → immediate push, rate-limited
   to one per session per 10 minutes. The per-session 60s idle nag is deliberately ignored (that
   signal is aggregated fleet-wide by layer 3 instead).
3. **Fleet-quiet backstop — hook sensors + a polling decider.** Claude Code hooks in
   `.claude/settings.json` (inherited by every session launched in the repo or a rift workspace:
   orchestrator, workers, solo workers) write per-session `busy`/`idle` state files to
   `$UNSEAMLESS_FLEET_DIR/state/activity/` via `scripts/notify/activity-hook`. The
   `unseamless-quiet-check` systemd **user timer** runs `scripts/notify/quiet-check` every 30s and
   pushes **once** when every tracked session is idle and has been for ≥2 minutes (debounce covers
   normal worker→orchestrator `msg` wake gaps; new work re-arms it). The quiet epoch is a
   **monotonic high-water mark** of observed activity (persisted), so tearing down a worker after
   a ping can't re-ping the same stop, and work by a session that later dies without a `Stop`
   still re-arms the next one. A freshly opened session that never ran a turn is tracked but not
   counted, so it can't trigger a "work stopped" push by itself, and a failed Pushover send is
   retried on the next tick rather than dropped. The decider is a poller, not a pure hook chain,
   for one reliability reason: a session killed mid-turn never fires its `Stop` hook, so stale
   `busy` state must be cleared by cross-checking tmux liveness (`usc-*` keys, which also sweeps
   the dead session's transcript stash) and by a 15-minute staleness cutoff (`PreToolUse`
   refreshes the timestamp on every tool call, so a genuinely working session never goes stale).

   The push **body** is enriched best-effort (the ping itself never depends on it): the `Stop`
   hook stashes each session's `transcript_path`, and at push time quiet-check takes the last
   assistant message of the lead session (`usc-orch` if tracked — the orchestrator narrates the
   fleet — else the most recently active) and summarizes it to a one-liner via the local
   **LM Studio** OpenAI-compatible server (`QUIET_LLM_URL`, default `127.0.0.1:1234`; model =
   `QUIET_LLM_MODEL` or auto-picked as the first gemma in `/v1/models`; `QUIET_LLM_DISABLE=1`
   turns it off). LM Studio down/slow → a raw truncated snippet of that message; no transcript →
   the generic "fleet quiet: N sessions idle" line.

**Setup (once per machine):** put keys in `~/.config/unseamless-notify/pushover.env`
(`PUSHOVER_TOKEN`/`PUSHOVER_USER`, never committed), then run `scripts/notify/install` **from the
main clone** (it copies the decider to `$UNSEAMLESS_FLEET_DIR/bin` — a stable path outside any
worker workspace — and enables the timer; re-run it to roll out changes to `quiet-check`/
`pushover`). Everything fails soft without keys, so hooks are inert on machines that don't want
pushes. Claude Code only for now; a Codex sensor adapter can later hang off Codex's native
`notify` config hook writing the same state files.

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
role preamble, and the `/fleet` orchestrator skill. Dual-harness support (`_harness`/`harness`, the
codex branches of spawn/revive/msg/ls, the `AGENTS.md` + `.codex/skills` symlinks) landed 2026-07
and was verified end to end with a live codex ping worker: spawn with role+assignment seed, trusted
launch with no dialog, `worker-open` revive via `codex resume --last`, and `msg` delivery in both
directions (including the sandbox `network_access=true` fix that makes a worker's own `msg` work).
Per-worker overrides (`worker-new --harness`/`--model`, the `.model` marker, per-target `msg`
transport resolution via `fleet_worker_harness`, the `models` lister) landed 2026-07-03, turning
the all-one-harness rule into a default-plus-opt-outs model. Also 2026-07-03: codex workers got
pre-approved lane work (`approval_policy="never"` + `writable_roots` for the workspace `.git`
— workspace-write carves it out of the writable cwd, which broke `git commit` — plus `~/.cargo`
and the fleet dir; the analog of the claude allowlist), and workspace trust moved from the
launch-line `-c` flag (which codex 0.142.5's trust gate ignores, re-prompting on every spawn) to
a persisted config entry via `scripts/fleet/_codex`, cleaned up by `worker-rm`.

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
