---
name: sync
description: Reconcile shared platform code, docs, and skills between this FromSoftware DLL-mod repo and its sibling (er-crit-coop <-> unseamless-coop). Smart, bidirectional, AI-driven — ports genuine improvements both ways while preserving each repo's intentional differences (crate/dll name, version, and the actual mod behavior). Use when the user runs /sync or asks to sync the two mod projects.
---

# Syncing the two mod repos

`er-crit-coop` and `unseamless-coop` are siblings: two Elden Ring DLL mods on the same
`fromsoftware-rs` SDK, sharing a **platform** (toolchain, build/release infra, the DLL entry
+ task-hook pattern, logging, safety conventions, and these skills). They diverge on identity
and behavior. This skill keeps the *platform* in sync without flattening the *differences*.

This is a **judgment task, not a `cp`.** Read both sides, decide what genuinely drifted, port
the better version each way, and rewrite repo-specific tokens as you go. Never blindly
overwrite — a hunk that differs because the two mods *do* different things is not drift.

## The two repos

Both are cloned as siblings. From whichever repo you're in:
- **this repo** = the cwd.
- **peer repo** = the sibling dir: if cwd is `er-crit-coop`, peer is `../unseamless-coop`;
  if cwd is `unseamless-coop`, peer is `../er-crit-coop`. Confirm it exists before starting;
  if missing, stop and tell the user.

Default direction is **bidirectional reconcile** (best version of each shared artifact wins,
ported both ways). If the user names a direction ("push my changes to the other", "pull from
er-crit-coop"), respect it.

## Token map (rewrite these when porting between repos)

Shared files are near-identical *except* for identity tokens. Translate every occurrence when
moving a hunk across:

| er-crit-coop            | unseamless-coop          | appears in                          |
| ----------------------- | ------------------------ | ----------------------------------- |
| `er-crit-coop`          | `unseamless-coop`        | repo name, Cargo `name`, CI, docs   |
| `er_crit_coop`          | `unseamless_coop`        | lib name, `.dll`, `.log` filename   |
| crit/riposte behavior   | Seamless Co-op behavior  | descriptions, README, module docs   |

Identity tokens (`name`, `version`, `description` in `Cargo.toml`; README prose) are **owned
per repo** — translate them when a *shared* file mentions them, but never sync the values
themselves.

## In scope (sync these)

The shared platform. For each, reconcile drift and apply the token map:

- **Toolchain:** `rust-toolchain.toml`; the `[profile.release]` block and the shared dep lines
  (`eldenring`, `fromsoftware-shared` pinned to the **same commit** in both, `windows`, `log`,
  `simplelog`) in `Cargo.toml`. **Do not** touch `name`/`version`/`description`.
- **Build/release infra:** `.github/workflows/release.yml`, `scripts/deploy.sh`, `.gitignore`,
  `LICENSE`.
- **The harness pattern:** `src/logger.rs` (whole file, token-mapped) and the *structure* of
  `src/lib.rs`'s `DllMain` (entry pattern, no-DETACH invariant, init-thread spawn) and the
  task-registration block in `src/hook.rs` / `src/patch.rs` (`wait_for_instance` →
  `run_recurring` → `mem::forget`, phase-ordering safety). Sync the **pattern and its safety
  comments**, not the per-frame body.
- **Docs:** `docs/DEVELOPMENT.md` and the shared sections of `CLAUDE.md` — "Where things run"
  (the single Linux build+run machine), the SDK notes, and the **hard safety invariants**.
- **Skills:** `.claude/skills/release/SKILL.md` and **this** `.claude/skills/sync/SKILL.md`
  (self-sync — if you improve the sync skill, propagate it).

## Out of scope (never sync)

- Mod behavior: `er-crit-coop`'s `src/patch.rs` + `src/diagnostic.rs` vs `unseamless-coop`'s
  `src/hook.rs` and whatever it grows. The *pattern* is shared; the logic is each repo's own.
- Package identity (`Cargo.toml` `name`/`version`/`description`), `README.md` prose,
  `Cargo.lock` (let the build refresh it), `reference/` (gitignored, repo-local), and any
  repo-specific notes (e.g. `docs/session.md`).

## Procedure

1. **Locate the peer** and confirm both trees are clean (`git status --porcelain` in each). If
   either is dirty, surface it and ask before proceeding — sync rewrites tracked files.
2. **Diff the in-scope set** between the two repos (read both versions; `git -C <peer> ...` and
   local reads). For each artifact, classify each hunk: shared-drift (reconcile) vs
   intentional-divergence (leave) vs identity-token (translate).
3. **Reconcile.** Port the better version of each shared hunk both directions, applying the
   token map. When two shared changes genuinely conflict and you can't tell which is the
   improvement, stop and ask rather than guess.
4. **Verify each repo still builds:** `cargo build --release --target x86_64-pc-windows-gnu`
   in both (this is the host-doable check; no game needed). Fix any token-rewrite slips.
5. **Commit on each changed repo** with a message naming what synced (e.g.
   `Sync platform from unseamless-coop: release.yml + logger`). Then **push both**
   (`git push origin main` in each) — it's safe to push both.
6. **Report** a short per-repo summary of what moved and what you deliberately left divergent.

## Notes
- The pinned `fromsoftware-rs` commit MUST match in both `Cargo.toml`s — a sync is a natural
  time to bump them together. If they differ, reconcile to one and note it.
- If you find yourself wanting to sync something not listed in scope, that's a signal the two
  repos are sharing something new — tell the user and propose adding it to the scope list here
  (then it gets self-synced).
