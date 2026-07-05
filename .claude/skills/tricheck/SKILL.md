---
name: tricheck
description: Lightweight three-agent code review of the current branch — one general reviewer plus two focused lenses you pick for the change. The heaviest review this project uses; reach for it when landing solid work into the mod, not for experiments.
---

A fast, three-agent review for **landing solid work** into the project. It's a step up from a single
`/check` and the heaviest review used here — three fresh-context reviewers, findings applied, done.

**When to run it — and when not to.** Most work in this repo is *experiments*: RE probes, rig
instrumentation, throwaway drivers, diagnostic levers. **Experiments get no formal review** — keep the
build green (`cargo build` / `cargo clippy --release -- -D warnings` / `scripts/test-core.sh`), eyeball
the diff, ship. Minor bugs and rough edges are fine there; they get corrected when noticed. Save
`/tricheck` for when a **real feature or subsystem is going into the mod as something solid** — the
point where a second set of eyes actually pays for itself. For a small, localized solid change, a
single `/check` is enough; step up to `/tricheck` when the change is larger or logic-heavy.

## How To Run It

Spawn **three fresh-context reviewers in parallel over the current branch diff**, in the background
(`Agent` with `run_in_background: true`) so you keep working while they run:

1. One **general** reviewer — `subagent_type: "check"`. Give it a one-paragraph summary of the
   session's goal; it reads the diff itself.
2. Two **focused** reviewers — `subagent_type: "check-focused"`, each with ONE lens chosen for *this*
   change. Pick the two lenses that fit what landed. Common ones for this codebase:
   - **correctness** — logic bugs, off-by-one, wrong offsets/constants.
   - **safety** — the load-bearing invariants: no unwind across an FFI boundary, no use-after-free
     from a dropped task handle, frame-ordering vs thread-exclusivity, `ChrIns` load-status. (See
     CLAUDE.md > "Architecture & hard safety invariants" and FFI-UNWIND-AUDIT.md.)
   - **concurrency / frame-ordering** — task phase choice, shared-state reads.
   - **error-handling** — degrade-don't-crash, the fatal-vs-toast split.
   - **API-shape / simplification / test-coverage** — for core-crate logic.

   Default when unsure: **correctness + safety** — this is a game-mod cdylib, so a use-after-free or an
   unwind across `extern` is the class of bug that actually matters.

If the invocation carries an arg, treat it as scope ("staged only", "the last N commits", a file set)
or lens guidance ("focus on the scaling math") and fold it in.

## When The Reviewers Return

1. Validate each finding against the code, deduping overlaps across the three.
2. Apply every valid finding, including minor ones that genuinely improve the code. A fix may extend
   slightly past the diff (rename + update callers, extract a nearby helper). Don't apply sprawling
   cross-module restructures — reject those with a one-line note and let the human decide.
3. Summarize what you changed, and surface rejected findings with reasons so they can be sanity-checked.

That's the whole ladder for this project: **eyeball → `/check` → `/tricheck`**. If a change feels too
risky even for `/tricheck`, that's a signal to split it smaller or validate it on the rig, not to reach
for something heavier.
