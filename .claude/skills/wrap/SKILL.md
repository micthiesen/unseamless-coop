---
name: wrap
description: >
  Conclude the current session so a fresh one can continue seamlessly: sweep un-encoded learnings
  into the right repo docs, decide or confirm the next step (via /next when open), rewrite
  docs/STATE.md from ground truth (worker-ls, git status, rig state), commit, and print the
  restart instruction. Use when ending an orchestrator or solo-worker session, before killing a
  long-context session, or on "wrap up", "conclude the session", "save state and restart",
  "/wrap".
user_invocable: true
---

# Wrap (Conclude The Session)

The conclude-and-handoff ritual. After a wrap, this session is disposable: everything it learned
is in the repo, the next step is recorded with its why, and a fresh `orch-start` boots straight
into continuing. Never let a session's value live only in its context window.

## 1. Sweep Learnings Into Their Homes

Review the whole session for anything durable that isn't yet written down, and write each piece
to its proper home (per CLAUDE.md > "Project knowledge lives in the repo"):

| Kind of learning | Home |
|---|---|
| Rig conventions, launch/verify gotchas | `docs/RIG-RUNBOOK.md` |
| RE findings (offsets, gates, charted functions, dead ends) | the relevant `docs/*-FINDINGS.md` / design doc |
| How an address/AOB/result was derived | a comment next to the code that uses it |
| Fleet/orchestration lessons | `docs/ORCHESTRATION.md` / the `/fleet` skill |
| A repeatable procedure | the matching `.claude/skills/` skill |
| Cross-cutting rules, preferences | `CLAUDE.md` |

Write the **content** there — STATE.md gets only pointers. A dead end is a finding too: recording
why something was ruled out is what stops the next session from re-treading it.

## 2. Decide Or Confirm Next

- Previous Next still the plan (done partially, or untouched)? Confirm and carry it forward,
  updated to reflect progress.
- Previous Next completed, or the session changed the picture? Run **`/next`** to decide and
  record properly. Don't freehand a big direction change here — that's what `/next`'s candidate
  analysis is for.

## 3. Rewrite STATE.md From Ground Truth

Rewrite `docs/STATE.md` **wholesale** (overwrite, never append — git holds history). Fill each
section from live evidence, not memory:

- **Now** — the current true state in 3–6 bullets. Fold in what this session proved/landed.
- **Next** — from step 2, with the two-line why and plan-doc pointer.
- **Candidates Not Chosen** — carry forward, pruning ones that landed or died.
- **In-Flight** — from `scripts/fleet/worker-ls` (live workers + their lanes), `git status`
  (anything uncommitted), unintegrated `worker/*` branches, and the rig state (mod applied? game
  running? which save?).
- **Learned This Session** — pointers to what step 1 wrote, one line each.
- Update the "Last rewritten" date.

## 4. Commit

- **Orchestrator:** commit the doc updates + STATE.md to `main` per the repo's commit
  conventions (keep it green first if code changed).
- **Solo worker:** commit to your `worker/<name>` branch as usual; note in the done-message /
  handoff that STATE.md was updated so the orchestrator integrates it.

## 5. Sign Off

Print, roughly:

> Session wrapped: learnings encoded, STATE.md rewritten, committed. Safe to end this session.
> Restart with `scripts/fleet/orch-start` — it boots the fresh orchestrator with a prompt that
> reads STATE.md, verifies In-Flight against ground truth, and briefs you, then waits for
> direction (it won't start work on its own).

A wrap does **not** change the normal fleet lifecycle. A worker that has already signaled done
still gets integrated (+ `worker-rm`'d) exactly as usual — do that *before* rewriting STATE.md so
In-Flight reflects it, rather than leaving a finished lane parked in the file. What a wrap must
never do is proactively *conclude the fleet*: don't tear down still-working workers, don't touch
solo workers (they're left alone until Michael or the worker asks for integration), and don't
restore the rig. A wrap ends the *session*; the fleet and the rig keep running.
