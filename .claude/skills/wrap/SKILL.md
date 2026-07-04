---
name: wrap
description: >
  Conclude the current session so a fresh one can continue seamlessly: sweep un-encoded learnings
  into the right repo docs, decide or confirm the next step (via /next when open), rewrite
  docs/STATE.md so it reflects the current work, commit, and print the restart instruction. Use
  when ending an orchestrator or solo-worker session, before killing a long-context session, or on
  "wrap up", "conclude the session", "save state and restart", "/wrap".
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

## 3. Rewrite STATE.md To Reflect The Work

Rewrite `docs/STATE.md` **wholesale** (overwrite, never append — git holds history). STATE is about
**the work**, not machine state — keep it to:

- **Now** — the current work picture in 3–6 bullets. Fold in what this session proved/landed.
- **Next** — from step 2, with the two-line why and plan-doc pointer.
- **Candidates Not Chosen** — carry forward, pruning ones that landed or died.
- **Learned Recently** — pointers to what step 1 wrote, one line each.
- Update the "Last updated" date.

**Do not** add a fleet/rig/git snapshot. Live workers are `worker-ls` (live, can't drift); rig/Deck
state is cheap to re-derive and re-apply, so it's not worth recording; workers integrate before a
wrap so there's nothing uncommitted to note. Recording any of it is busy-work that just goes stale.

## 4. Commit

- **Orchestrator:** commit the doc updates + STATE.md to `main` per the repo's commit
  conventions (keep it green first if code changed).
- **Solo worker:** commit to your `worker/<name>` branch as usual; note in the done-message /
  handoff that STATE.md was updated so the orchestrator integrates it.

## 5. Sign Off

Print, roughly:

> Session wrapped: learnings encoded, STATE.md rewritten, committed. Safe to end this session.
> Restart with `scripts/fleet/orch-start` — it boots the fresh orchestrator with a prompt that
> reads STATE.md and briefs you, then waits for direction (it won't start work on its own).

A wrap does **not** change the normal fleet lifecycle. A worker that has already signaled done
still gets integrated (+ `worker-rm`'d) exactly as usual. What a wrap must never do is proactively
*conclude the fleet*: don't tear down still-working workers, and don't touch solo workers (they're
left alone until Michael or the worker asks for integration). A wrap ends the *session*; the fleet
keeps running. (The rig is not a wrap concern at all — leave it as it is; re-applying is cheap and
Michael handles any restore himself.)
