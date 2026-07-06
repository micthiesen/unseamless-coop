---
name: next
description: >
  Decide what the project's next step should be. Reads docs/STATE.md, docs/ROADMAP.md, and recent
  git history, enumerates 2-4 candidate next steps with a gating analysis (what each unblocks,
  rig-serial vs delegable, size, risk), recommends one, and records the decision + runners-up in
  docs/STATE.md — spawning workers for the delegable ones once the direction is set. Use when unsure
  what to work on next, when the previous Next step completed, or from /wrap when concluding a
  session. TRIGGER on "what's next", "what should we do next", "pick the next step", "/next".
user_invocable: true
---

# Next (Decide The Next Step)

Turn "what should we do now?" from an open-ended re-derivation into a short, recorded decision.
The output is always two things: a **recommendation with its why**, and an **updated
`docs/STATE.md`** so the decision survives this session.

## 1. Gather Ground Truth (Cheap, Parallel)

- Read [`docs/STATE.md`](../../../docs/STATE.md) — the previous Next and the candidates not chosen
  (your shortlist starts here).
- Read [`docs/ROADMAP.md`](../../../docs/ROADMAP.md) for the gating structure (what's
  solo-doable vs 2-player-gated vs rung-3-gated), and the plan doc STATE's Next points at.
- `git log --oneline` since STATE's "Last updated" date — what actually landed since the
  decision was recorded.
- `scripts/fleet/worker-ls` — lanes already running (don't recommend work a live worker owns). This
  is the live source for what's in flight; STATE doesn't track it.

Don't re-read the deep RE docs unless a candidate genuinely hinges on a detail; the point of
STATE.md is that you shouldn't have to.

**STATE.md/ROADMAP candidates can be stale.** Before delegating a "build this charted thing" lane,
`git grep`/`git log` `main` to confirm it isn't already implemented — a candidate marked "charted and
buildable" may have shipped since it was written (a whole worker lane was spawned once for a gate that
was already on `main`). Fix the stale entry as part of the decision.

## 2. Enumerate Candidates (2–4)

For each candidate, one tight block:

- **What it is** — one sentence, concrete enough to brief.
- **What it unblocks** — its place in the gating chain (ROADMAP). Unblocking the headline
  (currently rung 3) outranks polish.
- **Serial or delegable** — needs the rig / a real session / integration → orchestrator-only.
  Pure code/docs/host-testable → delegable to a worker. **Core/live RE is serial even when big**
  (rig-coupled); only a genuinely independent *static* RE search (offline triage, decompile
  sweeps, call-site charting) is delegable. **Serial ≠ Michael-gated:** the orchestrator drives
  the rig and the Deck itself (apply, launch, auto-session in-world, read logs/memory) — tag a
  step Michael-gated only when it needs a human actually playing beyond what auto-sessions reach.
- **Size + risk** — rough effort, and the biggest unknown that could sink it.

## 3. Recommend One

Pick with these biases, in order:

1. **Unblock the headline.** Work that advances the current critical path (STATE's Now/Next)
   beats work that widens the surface.
2. **De-risk the biggest unknown early.** A cheap probe that could invalidate a plan beats
   building on the unproven plan.
3. **Keep the rig batched.** If several candidates need the rig, prefer the one that can absorb
   the others' probes in a single play session (ORCHESTRATION.md > "Batch rig passes").
4. **Parallelize the delegable.** The recommendation can be "start X on the rig AND spawn workers
   for Y, Z" — serial and delegable candidates aren't mutually exclusive.

State the recommendation in two sentences: what, and why it beats the runner-up. For a genuinely
contentious or expensive direction (a pivot, a multi-session bet), escalate to `/devils-advocate`
before recording it.

## 4. Record It

Rewrite `docs/STATE.md`:

- **Next** — the chosen step, its two-line why, the plan-doc pointer, and its serial/delegable tag.
- **Candidates Not Chosen** — the runners-up and the one-line reason each waits. This is what makes
  the *next* run of this skill cheap.

Commit if you're the orchestrator mid-session; leave it for `/wrap` if wrapping anyway.

## 5. Spawn The Delegable

In the proposal itself, keep each delegable candidate to a one-line "what the lane would do" —
don't print full worker briefs before the direction is chosen (they bloat the decision and
usually get revised anyway). Once the recommendation is confirmed (or you're proceeding
autonomously), write the `worker-new` heredoc brief per ORCHESTRATION.md > "Writing a worker
assignment" (lane + boundary, where to look, collision map, review depth) and **spawn it
directly** — the delegate-by-default posture (CLAUDE.md > "Orchestrator / worker fleet") means
spawning, not pasting instructions for Michael to relay.

## If You're A Worker

You can still run the analysis and propose, but say so: your STATE.md edit lands on your worker
branch (not main) and you can't verify rig/fleet ground truth. Hand the proposal to Michael or the
orchestrator rather than treating it as recorded.
