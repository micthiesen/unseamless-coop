---
name: sync
description: >-
  One-stop shop for syncing shared AI tooling and platform files between this
  project and its declared peer projects - skills, helper scripts, agent
  configs, CLAUDE.md guidance, build/release infra. Smart, bidirectional, and
  judgment-driven: ports genuine improvements both ways while preserving each
  project's intentional differences. Use when the user runs /sync or asks to
  sync projects' AI tooling, AND whenever you copy or port a skill, script, or
  any AI tooling from one project to another (route the copy through this
  skill so both sync maps stay current).
---

# Sync AI Tooling Between Projects

Keep the AI tooling and platform files this project shares with its peers
consistent, without flattening what is intentionally different. **This is a
judgment task, not a `cp`**: read both sides, decide what genuinely drifted,
port the better version in the right direction, and translate project-specific
tokens as you go. A hunk that differs because two projects legitimately *do*
different things is not drift - leave it alone.

## The skill files

Every participating project carries this skill:

- `SKILL.md` (this file) - the **shared body**. Identical in every participant
  below the frontmatter (frontmatter may differ per environment; rulesync
  sources need `targets`).
- `sync-status.py` - read-only drift detector. Identical everywhere.
- `sync-map.json` - **this project's** peers, shared resources, and (when the
  project derives outputs from synced sources) its `generate` command. Owned
  per project, never synced. This is the single place a project describes its
  own environment, so the syncing side never needs to know how each
  environment works.

In a plain project the skill lives at `.claude/skills/sync/`. In a
rulesync-managed project (like the dotfiles) the canonical copy is the
*source* at `.rulesync/skills/sync/` and the generated output is derived -
always sync the source, then run that project's `generate` command. Projects
that expose skills to other harnesses via symlinks
(`.agents -> .claude`) need nothing extra.

## The map (`sync-map.json`)

Keep the map **coarse**. `notes` describe standing relationships and permanent
per-project differences ("command is `pnpm browser` there", "identity tokens
only"), never current diffs - those rot immediately and git already records
them. A routine sync should not require touching the map; change it only when
the actual sharing relationship changes (resource added/dropped, new peer,
a difference becomes permanent).

Schema by example:

```jsonc
{
  "project": "this-project",
  "root": "~/.dotfiles",        // optional; only when the installed skill dir
                                // is not under the project root (the dotfiles
                                // copy is generated out to ~/.claude)
  "generate": "bash scripts/rulesync.sh",   // optional; how THIS project
                                // regenerates derived outputs after its synced
                                // sources change. Run from the project root.
                                // Omit when nothing is derived.
  "peers": {
    "sibling-project": {
      "path": "../sibling-project",   // relative to root, or absolute
      "tokens": { "this-project": "sibling-project",
                  "this_project": "sibling_project" },
      "notes": "what the relationship is"
    },
    "dotfiles": { "path": "~/.dotfiles", "layout": "rulesync" }
  },
  "resources": [
    { "path": "rust-toolchain.toml", "peers": ["sibling-project"] },
    { "path": "src/logger.rs", "peers": ["sibling-project"],
      "peerPath": { "sibling-project": "crates/foo/src/logger.rs" } },
    { "path": "CLAUDE.md", "peers": ["sibling-project"], "mode": "judgment",
      "notes": "only the safety-invariants section is shared" }
  ]
}
```

- `peers.<name>.path` - where the peer is checked out. Peers are **best
  effort**: sibling projects live at `../<name>`; the dotfiles/home
  exceptions use absolute paths. If the directory is missing, skip that peer
  (report it, don't fail, don't clone anything).
- `peers.<name>.tokens` - identity-token map, local spelling -> peer
  spelling. Applied when comparing and when porting content.
- `peers.<name>.layout: "rulesync"` - the peer's skills live under
  `.rulesync/skills/` (sync the source; the peer regenerates).
- `resources[].mode` - `"exact"` (default): byte-identical after token
  mapping; the script diffs it mechanically and drift means someone should
  reconcile. `"judgment"`: only part of the resource is shared (a pattern, a
  section, a shape); the script merely flags it for review and `notes` say
  what is shared vs owned.
- `resources[].peerPath` - per-peer path override when the resource lives at
  a different path over there.
- The sync skill itself is implicitly shared with **every** peer - never
  list it as a resource.
- **No transitive syncing.** Sync only this project <-> its direct peers. If
  a resource travels A <-> B <-> C, each map lists its own direct
  relationships; a change reaches C when B next syncs.

## Workflow

Don't re-learn peer projects each run: a peer's own `sync-map.json` (in its
sync skill dir) is the authority on its environment - its `generate` command,
its token spellings, its view of what's shared vs owned. Read that instead of
exploring the repo.

1. **Status**: `python3 <this dir>/sync-status.py` - per peer, per resource:
   `ok` / `DRIFT` (exact resource differs) / `review` (judgment resource
   differs - often fine) / `MISSING-*` / `skipped` (peer not checked out).
   Exit 0 means nothing mechanical to do.
2. **Self-sync first.** If the skill body or `sync-status.py` drifted from a
   peer, reconcile *that* before anything else (pick the direction via git
   history), write it to all affected peers, then re-run status. An
   improvement to this skill must never live in one copy only.
3. **Reconcile each drifted resource.**
   - `sync-status.py diff <peer> <path>` shows a token-normalized diff (only
     real drift; identity tokens are factored out).
   - Judge direction with git history on both sides
     (`git log --oneline -- <path>` here, `git -C <peer> ...` there): one
     side changed since they last matched -> port it, translating tokens;
     both changed -> merge both improvements; genuinely conflicting -> stop
     and ask the user.
   - Legitimate per-project behavior is not drift: leave it, and if the
     difference is permanent, record it coarsely in `notes` or downgrade the
     resource to `judgment`.
4. **Judgment resources**: read both sides and reconcile semantically per
   their `notes`. Only act when something shared actually improved.
5. **Regenerate + verify.** For every project you wrote into, run the
   `generate` command from *that project's own* `sync-map.json` (from its
   root), if declared. For code resources, run the owning repo's own
   build/tests.
6. **Commit each changed repo** per its own conventions (the dotfiles repo
   auto-commits and pushes; personal repos commit to main; work repos follow
   their PR flow), with a message naming what synced. Report per repo: what
   moved, what stayed intentionally divergent.

## Porting tooling between projects (always via this skill)

Whenever you copy a skill, helper script, agent config, or a reusable
CLAUDE.md pattern from project A to project B - even outside an explicit
/sync run:

1. Make the copy, adapting tokens/paths for B.
2. Register the resource in **both** A's and B's `sync-map.json` (adding the
   peer entries too if this is a new relationship).
3. If B doesn't carry the sync skill yet, bootstrap it (next section).
4. Commit both sides.

A copy without the map updates is how tooling silently forks; don't.

## Bootstrapping a new project

1. Copy `SKILL.md` + `sync-status.py` from here into the new project's
   `.claude/skills/sync/` - verbatim (or `.rulesync/skills/sync/`, if it
   uses rulesync).
2. Write it a fresh `sync-map.json`: project name, its peers, the shared
   resources - and a `generate` command if it derives outputs from synced
   sources (rulesync etc.); run it once to prove it works.
3. Add the reciprocal peer + resource entries to this project's map.

## Maintaining this skill (self-updating, self-healing)

- **Any improvement to this body or to the script propagates to all peers in
  the same change** - the map's peer list says exactly where the other
  copies live, and status will nag about it anyway.
- If a sync run reveals this procedure itself is wrong or awkward, fix this
  SKILL.md first, propagate it, then continue the run.
- If a peer's copy is missing, stale, or its map is malformed, repair it as
  part of the run - that is expected maintenance, not scope creep.
- Keep maps coarse; if `notes` start describing specific diffs, that detail
  belongs in git history, not here.
