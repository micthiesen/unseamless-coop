---
name: sync
description: Reconcile shared AI tooling with declared peer projects; use for sync requests and cross-project ports.
---

# Sync shared tooling

Reconcile shared instructions, skills, scripts, and platform files with declared
peers. Preserve each project's behavior and identity; differences are not
necessarily drift. Existing user authorization applies throughout the workflow.

## Ownership

- `SKILL.md` body and `sync-status.py` are shared identically across peers.
  Frontmatter may differ for each harness.
- `sync-map.json` belongs to its project. It records peers, resources, and permanent
  differences. Never copy another project's map over it or put transient diff
  history in its notes.
- Personal skills use `.agents/skills/`. Edit the canonical files directly. For
  Stow-managed installations, edit the source package in its owning repository,
  preserving discovery symlinks. Peer `skillPath` records a different source path.

## Reconcile

1. Run `python3 <skill-dir>/sync-status.py`. Read each present peer's map for its
   paths and ownership. Missing checkouts are reported and skipped, not cloned.
2. Resolve drift in this skill and its script first. Use history on both sides
   (`git log --oneline -- <path>`) to identify improvements and merge compatible
   changes. Update each affected direct peer's canonical copy within the authorized
   scope. Report excluded peers without modifying them.
3. Inspect resource differences with
   `python3 <skill-dir>/sync-status.py diff <peer> <path>`; this normalizes identity
   tokens. Port useful changes in either direction. For `judgment` resources,
   reconcile only the shared behavior named in the map.
4. Decide routine conflicts from project context. Ask only when incompatible
   intent creates a consequential choice the existing instructions do not settle;
   continue unaffected resources. Record permanent divergence in map notes or
   `judgment` mode, not as an unresolved mechanical drift.
5. Recheck sync status and review the complete diffs. Run relevant code checks
   when executable resources changed; prose-only changes need frontmatter,
   reference, and consistency checks. This workflow edits native files directly
   and does not generate derived copies.
6. Commit and push under each repository's conventions and current authorization.
   Report changed repos, meaningful intentional differences, and missing or
   excluded peers.

Sync direct peers only. A multi-project request may cover several peer pairs,
but a normal sync does not recursively modify unrelated projects. An explicit
exclusion takes precedence over the map and shared-copy propagation.

## Map fields

`project` names this project. Optional `root` resolves installed skills outside
its checkout (for example `~/.dotfiles`). Without it, the script derives the root
from its canonical `.agents/skills/sync/` directory.

`peers` maps names to `{path, tokens?, skillPath?, notes?}`. Paths are relative to
the project root or absolute/home-relative. `tokens` maps local identity strings
to peer spellings. `skillPath` is relative to the peer root or absolute/home-relative;
for a Stow package, use `codex/.agents/skills/sync`. Without `skillPath`, discovery
prefers `.agents/skills/sync`, falls back to an existing `.claude/skills/sync` for
unmigrated peers, and reports the native path as missing if neither exists.

`resources` contains `{path, peers, mode?, peerPath?, notes?}` entries:

- `mode: "exact"` (default) requires equality after token normalization.
- `mode: "judgment"` shares only the behavior described in `notes`.
- `peerPath` maps peer names to alternate paths when layouts differ.

The sync skill and script are implicitly shared with every peer; do not list them
as resources. Statuses are `ok`, `DRIFT`, `review`, `MISSING-*`, or `skipped`.
Exit 0 means no mechanical reconciliation remains; `review` still needs judgment.
The status and diff commands are read-only and never update either checkout.

## Add or repair a sharing relationship

When porting tooling, register reciprocal peers and resources in both maps,
adapting paths and identity tokens. Do not change a map for a routine sync that
leaves the sharing relationship unchanged.

If the receiving project lacks this skill, install its `SKILL.md` and
`sync-status.py` in `.agents/skills/sync/`, create a project-owned map, add the
reciprocal relationship, and verify status. Repair missing or malformed maps
within the requested scope. Preserve unrelated entries.

An improvement to this skill or script belongs in all its checked-out direct
peers within the authorized scope. Commit both sides of a port; report unavailable
or excluded peers so a future sync can complete propagation.
