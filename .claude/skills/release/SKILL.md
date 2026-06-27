---
name: release
description: Mint a new GitHub release for unseamless-coop. Determines the next version, writes clean release notes as a delta since the last tag, creates an annotated vX.Y.Z tag, and pushes it — CI then cross-compiles the DLL and publishes the release. Use when the user asks to cut/mint/publish a release or run /release.
---

# Minting a release

This repo releases via an annotated tag: `.github/workflows/release.yml` triggers on a
`v*` tag, builds the Windows DLL, and runs `gh release create --notes-from-tag`, so the
**tag's annotation message becomes the release notes**. Your job is to author those notes
and create the tag. CI does the build + publish.

Tagging/pushing happens locally; the actual DLL build + publish runs in CI. No game launch is
involved in releasing.

Optional argument: an explicit version (e.g. `v0.2.0`). If omitted, propose one.

## Steps

1. **Preflight.**
   - Ensure `gh` is installed and authed: `gh auth status`. If `gh` is missing, install it
     (`pacman -S github-cli`).
   - Confirm we're on `main` with a clean tree (`git status --porcelain`). If there are
     uncommitted changes, stop and tell the user.
   - Sanity-build so we never tag something that doesn't compile:
     `cargo build --release --target x86_64-pc-windows-gnu`.

2. **Find the last tag.** `git describe --tags --abbrev=0` (none yet → this is the first
   release; base notes on the whole history and start at `v0.1.0`).

3. **Pick the version.** Use the argument if given. Otherwise infer a semver bump from the
   changes since the last tag (behavior change/new feature → minor; fixes only → patch;
   breaking install/usage change → major) and state your choice. Call it `X.Y.Z` (tag `vX.Y.Z`).
   - **A major bump (`X` increase) requires Michael's explicit approval.** Never cut one on
     your own inference. If the changes look major, default to a minor bump and ask first.
   - **`1.0.0` is reserved for when the mod is actually functional** (real co-op working, not
     just framework/scaffold). Until then stay in `0.x` even for breaking changes. (Remove this
     note once `1.0.0` ships.)

4. **Bump the version in `Cargo.toml` and commit it.** Set `version = "X.Y.Z"` in `Cargo.toml`,
   then `cargo build --release --target x86_64-pc-windows-gnu` to refresh `Cargo.lock`. Commit
   both (`git commit -m "Release vX.Y.Z"`) and `git push origin main`. The tag must point at a
   commit whose `Cargo.toml` already carries the released version, since CI builds from the tag.

5. **Write the release notes — a clean delta, not a commit dump.** Read
   `git log <last-tag>..HEAD` for raw material, then write for a user installing the mod:
   - Lead with what changed in behavior or how you use it.
   - Group related changes; drop refactors/internal churn that don't affect users.
   - Keep it tight (a short intro line + a few bullets). No em dashes.
   - For the first release, describe what the mod does and the one-file install.

6. **Create the annotated tag with those notes as the message, and push:**
   ```bash
   git tag -a vX.Y.Z -F <notes-file>   # write the notes to a temp file first
   git push origin vX.Y.Z
   ```
   (Annotated tag is required — `--notes-from-tag` reads this message.)

7. **Watch and report.** `gh run watch` (or poll `gh run list --workflow=release.yml`)
   until the release job finishes, then give the user the release URL
   (`gh release view vX.Y.Z --json url -q .url`). If CI fails, surface the log and stop.

## Notes
- Don't call `gh release create` yourself — CI owns publishing, so the binary and notes land
  atomically. You only create and push the tag.
- If you need to redo a release, delete the tag locally and remotely
  (`git tag -d vX; git push --delete origin vX`) and the release
  (`gh release delete vX`), then re-run.
