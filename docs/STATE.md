# State

The single **fast-moving** "where we are / what's next" file. **Overwritten, never appended** — no
history, no superseded callouts; git holds the past. Owned by the orchestrator: `/wrap` rewrites it
when a session concludes, `/next` records next-step decisions in it, and `orch-start` seeds a fresh
orchestrator with a boot prompt that reads it. Durable knowledge does **not** live here — findings
go to their proper doc (CLAUDE.md > "Project knowledge lives in the repo"); this file holds the
current picture, the chosen next step, and pointers.

> A session that dies without `/wrap` leaves this stale. Treat it as a map, not gospel: verify the
> In-Flight section against ground truth (`scripts/fleet/worker-ls`, `git status`, `git log`,
> `rig.sh status`) before acting on it.

Last rewritten: **2026-07-04** (live-capture session).

## Now

- Rungs **1/2/4** (identity, password-authed side-channel, lobby discovery) shipped, **confirmed
  across two real machines**. The **DLNW3D Steam P2P transport is rig-proven two-machine** (rig +
  Deck exchange packets by SteamID64 alone, no matchmaking). **Solo host reaches + sticks**
  (`Host`/`Ingame`, `players=1`, warped into map `1800001`).
- **★ LIVE CAPTURE DONE (2026-07-04)** — captured a real working 2-player ERSC session in memory
  (rig host + Deck joiner, both real ERSC). See [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md).
  Two corrections that redirect the effort:
  1. **`[context+0x168]` is the reject-stub even in a fully working session** — the gate-c /
     "install a real member-lookup" theory (avenue a) is **dead**. Members come from the session
     layer (`SessionSteam` → `SessionMemberSteam`), not the transport admit gate.
  2. **The full DLNR3D/DLNW3D graph is native game objects, all present + enumerated with offsets**
     (`SessionSteam`, 6 `SessionMemberSteam` slots, the context, live `SteamConnectionManager`, and
     **`SteamServiceImpl`** — the standup that returns *null* offline is a real object here). Keyed by
     real SteamID64 (`SteamConnection+0x138`), exactly what rung-4 discovers. add-member's two args are
     decoded (arg1 = ref to the host `SocketManagerHolder`, arg2 = peer-identity handle).
- Rung-3 direction (unchanged, now sharpened): **"let the game establish it"** — drive the game's own
  establishment, fed the rung-4 peer, not hand-synthesize the graph.
- Two shipped-this-session lanes on `main`: overlay session **status row + connect-gate hint**, and the
  opt-in **stay-connected** disconnect-suppression gate (default off; rig validation still owed).

## Next

**Reproduce the ERSC establishment — start by resolving the one concrete wall: why `SteamServiceImpl`
standup `0x142638b40` returns null offline.** Use the live capture as the known-good reference: chart
what the standup reads/requires, diff against an offline `stand_up_transport` attempt, then drive the
game to build `SessionSteam` + members + the transport keyed to the rung-4 peer SteamID64 →
`host-admit-success 0x142640ee4` → roster `players=2`, validated two-machine.

- **Why this / why now:** the capture proved the mechanism (native DLNR3D/DLNW3D graph, not the
  gate-c/`+0x168` path we chased) and that everything needed exists in a real session; the standup
  null is the specific gap between our offline state and the known-good one.
- **Plan:** [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ DECISION (2026-07-04)" +
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) (offsets + reproduce target). Tasks
  #14 (reproduce) and #15 (standup null).
- **Serial:** rig-owned (needs our mod re-applied — see In-Flight).

## Candidates Not Chosen

- **Offline synthesis of the session graph** — the live capture confirmed it's a dead end at the object
  level (runtime-built native objects), not just "harder." Re-open only if the standup / establishment
  reduces to a small reproducible field/call set.
- **The gate-c / `+0x168` "real member-lookup"** avenue — **DEBUNKED** (stub even in a working session);
  do not re-chase.
- **Writer-trace live follow-up** (task #16) — arm a watchpoint on a stable anchor during a fresh join to
  catch the establishment RIPs; valuable but needs another live 2-player session (setup in
  ERSC-LIVE-CAPTURE-FINDINGS.md > "Re-running this capture"). Do it if static charting of the establishment
  stalls.
- **Single-source the duplicated `leave_session` offset/prologue** (task #13) — fold into the pivot churn
  on `session_probe.rs`, not standalone.
- **stay-connected rig validation** — solo-drivable; do on a cycle when the rig's free (needs our mod
  re-applied).

## In-Flight

- **Workers:** none (`worker-ls` empty). This session's three lanes (overlay-connect, disconnect-suppress,
  workflow/session-continuity) are all integrated to `main` and removed.
- **Rig:** **running the user's REAL ERSC stack (our mod NOT applied)** — restored for the live capture; ER
  not running. **To resume mod dev, `scripts/rig.sh apply` first.** Snapshot intact.
- **Deck:** provisioned as a real ERSC peer (account `testthiesen`, save `ER0000.co2`, password `salmon`);
  its prior our-mod files are backed up at `~/deck-mod-backup-*` on the Deck. To use the Deck as our-mod
  player 2 again, restore that backup (or re-push via `deck.sh`).
- **Git:** `/wrap` commits the doc sweep here; two earlier commits (live-capture findings; session-continuity
  system) plus this one need a push.
- **Raw capture dumps:** `~/Documents/ersc-live-capture*.txt` (not committed — distilled into the findings
  doc).

## Learned This Session (Pointers Only)

- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) — the live capture: the two corrections,
  the object graph + offsets, add-member args, and how to re-run the capture.
- [SESSION-DRIVE.md](SESSION-DRIVE.md) — decision block updated with the live-capture result; the DLNR3D
  reframe + Lanes A/B/C + reachability chain remain the RE reference.
- `scripts/re/scan-vtable.py` — fixed to chunk large regions (was silently missing the `0x7fff…` Wine heap).
- This session's doc labels corrected to **2026-07-04** (were mislabeled 2026-07-05).
