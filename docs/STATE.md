# State

The single **fast-moving** "where we are / what's next" file. **Overwritten, never appended** — no
history, no superseded callouts; git holds the past. Owned by the orchestrator: `/wrap` rewrites it
when a session concludes, `/next` records next-step decisions in it, and `orch-start` seeds a fresh
orchestrator with a boot prompt that reads it. Durable knowledge does **not** live here — findings
go to their proper doc (CLAUDE.md > "Project knowledge lives in the repo"); this file holds the
current picture, the chosen next step, and pointers.

> A session that dies without `/wrap` leaves this stale. Treat it as a map, not gospel: verify the
> In-Flight section against ground truth (`scripts/fleet/worker-ls`, `git status`, `git log`) before
> acting on it.

Last rewritten: **2026-07-04** (seeded by the `workflow` solo lane that built this system).

## Now

- Rungs **1/2/4** (identity, password-authed side-channel, lobby discovery) shipped, **confirmed
  across two real machines** (2026-06-27 friend test).
- The **DLNW3D Steam P2P transport is rig-proven two-machine** (rig + Deck): both machines exchange
  packets by SteamID64 alone, no matchmaking.
- **Solo host reached + sticks**: `Host`/`Ingame`, `players=1`, warped into the co-op map
  (`1800001`), session holds. Joiner SYN reaches the host admit path but is rejected at gate c (the
  synthesized context's member-lookup is a stub) — roster stays 1.
- **★ Rung-3 PIVOT (2026-07-05):** offline hand-synthesis of the session graph is ruled out (3-lane
  RE pass: capacity-0 array, handle-object add-member, no static installer for the accept callback).
  New model: **"let the game establish it"** — drive the game's own establishment entry points, fed
  the rung-4 peer, reproduced from a live capture of a real ERSC session.
- Overlay native-Windows crash: root-caused (inline-hook collision) and fixed (IAT hook), validated
  on a real Windows loader. Pre-release blocker retired.

## Next

**Live-capture a real ERSC establishment, then reproduce it.** Run `watch-write.py` against a real
working ERSC session at the charted offsets (`SessionManagerSteam+0x18/0x20/0x24`, the add-member
handle objects, `MTInternalThreadSteamSocket+0x168`, `[container+0x48]`), record the establishment
sequence, then drive the same sequence from our mod → host-admit success (`0x142640ee4`) → roster
`players=2`.

- **Why this over the alternatives:** the 3-lane RE proved every piece missing offline is a runtime
  object the establishment flow builds; capturing and replaying the flow beats forging the graph
  field-by-field (whack-a-mole, many sessions). The capture also arbitrates whether offline
  synthesis is worth re-opening.
- **Plan:** [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ DECISION (2026-07-05)"; procedure:
  [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md).
- **Serial:** needs the rig + a real ERSC session → orchestrator-owned, not delegable.

## Candidates Not Chosen

- **Offline synthesis of the session graph** — paused, not killed. Re-open only if the capture shows
  establishment reduces to a small reproducible field/call set.
- **Hand-registering the joiner in the member collection** (`[context+0x170]` via register thunk
  `0x14263b7c0`) — same hand-synthesis class the pivot rules out; subsumed by the capture.
- **Seamlessness re-sync layer** (across area transitions) — independent and additive; the armed
  disconnect-suppression gate (stay-connected) already landed. Delegable, but pointless to polish
  before roster=2 exists.
- **Wiring the inert overlay toggles / menu state bits** — rung-3-gated; unblocks the moment the
  session core lands.

## In-Flight

*(Seeded from the `workflow` lane, which cannot see the orchestrator's fleet — verify on first use.)*

- **worker/workflow (solo):** the session-continuity system itself (this file, `/next`, `/wrap`,
  the delegate-by-default posture, the `orch-start` boot seed) — ready to integrate, then
  `worker-rm`.
- **Rig state:** unknown at seed time — check whether the mod is applied before assuming.

## Learned This Session (Pointers Only)

- Session-continuity contract (this file + `/wrap` + `/next` + the `orch-start` seed) — written to
  [ORCHESTRATION.md](ORCHESTRATION.md) > "Session Continuity".
- Delegation posture flipped to delegate-by-default — CLAUDE.md > "Orchestrator / worker fleet" and
  the `/fleet` skill intro.
