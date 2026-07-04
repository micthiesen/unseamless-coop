# State

The single **fast-moving** "what we're working on / what's next" file. It is **about the work** —
the current picture, the chosen next step, and the runners-up — **not** a snapshot of machine state.
**Overwritten, never appended** (git holds history). Owned by the orchestrator: `/wrap` rewrites it
when a session concludes, `/next` records next-step decisions in it, and `orch-start` seeds a fresh
orchestrator to read it and brief from it. Durable knowledge does **not** live here — findings go to
their proper doc (CLAUDE.md > "Project knowledge lives in the repo"); this file holds the current
picture, the chosen next step, and pointers.

> **Deliberately not tracked here:** live workers (use `scripts/fleet/worker-ls` — it's live and
> can't drift), rig/Deck state (the mod is cheap and safe to re-apply, so we just do it; never a
> thing to remember or restore), and uncommitted git state (workers integrate before a wrap). A
> fresh orchestrator reads this file for *the work* and gets moving; it doesn't audit machine state
> first.

Last updated: **2026-07-04**.

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the
  side-channel, and exchange game-P2P packets by SteamID64 alone (no matchmaking). Solo host reaches
  and *sticks* (`Host`/`Ingame`, warped into the co-op map).
- **The rung-3 headline** (two players in each other's world) rides on the game's own session
  establishment. Direction (2026-07-04): **"let the game establish it"** — drive the game's own
  establishment machinery fed our rung-4 peer, don't hand-synthesize the object graph. Validated by a
  **live capture of a real 2-player ERSC session** (all the native DLNR3D/DLNW3D objects present +
  enumerated).
- **The "`SteamServiceImpl` standup returns null offline" wall is DEAD as a framing** — it was a
  red herring. Charted statically ([STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md)) and
  rig-confirmed this session: the factory `0x142638b40` is *satisfiable offline* (its one content
  gate is unconditionally true; the sub-init guarantees a non-null owner), and driving it offline
  builds real transport objects (rig read: 2/2/1 live under our drivers, 0/0/0 undriven). The only
  genuine offline≠live delta on the chain is the **runtime online-availability signal** (the
  singleton `0x144842d40` availability query, read below the standup by gate `0x140de2620`) — which
  we already bypass for the solo host via the `suppress_leave` gate-force.

## Next

**Task #16 — the live writer-trace, now tightly scoped.** The static path is exhausted; the one
field that differs offline-vs-live is a *runtime* value, so it can only be pinned live. Watch, during
a real ERSC host+join (Deck as peer 2), the two things that actually build the session:

1. **The member-add writers** — how a `SessionMemberSteam` gets added to `SessionSteam` (the live
   capture named the objects; catch the RIPs + values that populate a member on a fresh join). Anchor
   on a stable field (`[csm+0xc]` lobby_state, or the member-registry root `container+0x1e8`).
2. **The online-availability signal** — the `0x144842d40` singleton's state and its `[vt+0x18]`
   availability-query result, plus `[[0x143d855c8]+0x10]`, in a real session vs offline.

- **Why this / why now:** the standup red herring is closed and offline synthesis is a proven dead
  end, so the only way forward is to observe a working establishment and reproduce its sequence. The
  offline side of the diff is already captured this session (0/0/0 transport objects undriven;
  `[[0x143d855c8]+0x10]` read as `1` at menu — so the differ is *below* that field, at the singleton
  query), so the live session is a quick grab-and-diff, not open-ended.
- **Plan:** [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "Re-running this
  capture" (setup) + its dump/writer-trace list; [STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md)
  §4 (the ranked offline-vs-live reads); [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ DECISION".
- **Serial:** needs Michael + a live 2-player ERSC session (rig on real ERSC + Deck as ERSC peer);
  orchestrator drives the watcher. **Gated on Michael's availability**, so while it waits, the
  static-prep worker below aims it.
- **Being prepped now (delegable):** a worker is statically charting the two live-trace targets — the
  session-member *writers* (`container+0x1e8` registry root / `SessionSteam` member slots, roster-add
  `0x140cb31b0` call graph) and the availability-singleton query (`0x144842d40` `[vt+0x18]`, and who
  writes its state) — so the live pass arms known anchors with expected values instead of discovering
  them. Turns the scarce live session into a fast confirm.

## Candidates Not Chosen

- **Offline synthesis of the session graph** — dead end at the object level (runtime-built native
  objects). Re-open only if the live capture shows establishment reduces to a small reproducible
  field/call set.
- **The gate-c / `+0x168` "real member-lookup"** avenue — **DEBUNKED** (stub even in a working
  session); members come from the session layer, not the transport admit gate. Do not re-chase.
- **stay-connected behavioral validation** (risks #1–#3: boss/area/death route through the gate +
  stay playable) — needs a live 2-player session; install+arm is already rig-validated. Fold into the
  next live co-op run, or just notice it in play.
- **Static-prep of the live-trace targets** — *chosen as the delegable companion to Next* (see the
  worker note under Next); not a runner-up, listed here so it isn't re-proposed.

(Done since this decision: task #13 — the duplicated `leave_session` offset is now single-sourced
from `stay_connected::LEAVE_SESSION_OFFSET`, referenced by `session_probe`.)

## Learned Recently (Pointers Only)

- [STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md) — the standup factory is satisfiable offline;
  the offline wall is flow-non-entry + the downstream runtime availability signal, not the factory.
  §4 has the ranked offline-vs-live dump list; §5 the minimal driven-standup recipe.
- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) — the live 2-player ERSC object
  graph + offsets, the two corrections, and the re-run/writer-trace procedure.
- [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md) — stay-connected install+arm
  rig-validated; the site-A prologue was missing its leading REX `0x40` (now corrected + live-read),
  and the probe leave-tracer is gated so it stops stealing the gate's site.
