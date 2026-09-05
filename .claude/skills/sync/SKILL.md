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
  Frontmatter may differ for each harness, including rulesync `targets`.
- `sync-map.json` belongs to its project. It records peers, resources, permanent
  differences, and an optional generation command. Never copy another project's
  map over it or put transient diff history in its notes.
- In rulesync projects edit `.rulesync/skills/sync/`; otherwise use the existing
  canonical skill directory, usually `.claude/skills/sync/`. Preserve discovery
  symlinks such as `.agents -> .claude` and regenerate derived outputs.

## Reconcile

1. Run `python3 <skill-dir>/sync-status.py`. Read each present peer's map for its
   paths and generator. Missing checkouts are reported and skipped, not cloned.
2. Resolve drift in this skill and its script first. Use history on both sides
   (`git log --oneline -- <path>`) to identify improvements and merge compatible
   changes. Update every affected direct peer's canonical copy.
3. Inspect resource differences with
   `python3 <skill-dir>/sync-status.py diff <peer> <path>`; this normalizes identity
   tokens. Port useful changes in either direction. For `judgment` resources,
   reconcile only the shared behavior named in the map.
4. Decide routine conflicts from project context. Ask only when incompatible
   intent creates a consequential choice the existing instructions do not settle;
   continue unaffected resources. Record permanent divergence in map notes or
   `judgment` mode, not as an unresolved mechanical drift.
5. Run each changed project's declared `generate` command from that project's
   root, then recheck sync status and review source and generated diffs. Run
   relevant code checks when executable resources changed; prose-only changes
   need frontmatter, reference, and consistency checks.
6. Commit and push under each repository's conventions and current authorization.
   Report changed repos, meaningful intentional differences, and missing peers.

Sync direct peers only. A multi-project request may cover several peer pairs,
but a normal sync does not recursively modify unrelated projects.

## Map fields

`project` names this project. Optional `root` resolves installed skills outside
its checkout (for example `~/.dotfiles`); optional `generate` is its generator.
`peers` maps names to `{path, tokens?, layout?, notes?}`. Paths are relative to
the project root or absolute/home-relative. `tokens` maps local identity strings
to peer spellings. Use `layout: "rulesync"` for a rulesync peer.

`resources` contains `{path, peers, mode?, peerPath?, notes?}` entries:

- `mode: "exact"` (default) requires equality after token normalization.
- `mode: "judgment"` shares only the behavior described in `notes`.
- `peerPath` maps peer names to alternate paths when layouts differ.

The sync skill and script are implicitly shared with every peer; do not list them
as resources. Statuses are `ok`, `DRIFT`, `review`, `MISSING-*`, or `skipped`.
Exit 0 means no mechanical reconciliation remains; `review` still needs judgment.

## Add or repair a sharing relationship

When porting tooling, register reciprocal peers and resources in both maps,
adapting paths and identity tokens. Do not change a map for a routine sync that
leaves the sharing relationship unchanged.

If the receiving project lacks this skill, install its `SKILL.md` and
`sync-status.py` in the canonical directory, create a project-owned map, add the
reciprocal relationship, and verify status and any generation command. Repair
missing or malformed maps within the requested scope. Preserve unrelated entries.

An improvement to this skill or script belongs in all its checked-out direct
peers in the same change. Commit both sides of a port; report unavailable peers
so a future sync can complete propagation.
