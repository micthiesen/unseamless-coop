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
- **★★ HOST-SIDE ESTABLISHMENT REPRODUCED (2026-07-05) — solo, offline, via the game's own flow.**
  Seeding the establish handler's input descriptor from the stood-up socketmgr's config defaults made
  `0x1423f2820` **succeed offline**: it builds the connection, its own `add-member 0x1423fdf20` fires,
  and the result is a **stable `Host`/`Ingame` session with the full member graph** — 1 SessionSteam +
  6 SessionMemberSteam, `member[5]+0x80` = the rig's own SteamID64 (host is a member of its own session),
  5 empty slots. This EXACTLY matches the live ERSC host-side capture. The "let the game establish it"
  model is proven. Details: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★ HOST-SIDE ESTABLISHMENT REPRODUCED".
- **Task #16 live capture (2026-07-04)** pinned the reproduction target that made this possible: a member
  is a raw `SteamID64` at `member+0x80`; the member-add is synchronous inside `0x1423f2820` (`→
  session-create 0x1423f7070 → add-member 0x1423fdf20 → member ctor 0x142400210 → +0x80`), not an async
  callback ([ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "Writer-trace capture").
- **Both prior "walls" were red herrings** (confirmed live): the `SteamServiceImpl` standup is satisfiable
  (owner=config), and the availability field `[[0x143d855c8]+0x10]` reads **0 in a working session** — the
  gate `0x140de2620` isn't on the establishment path.

## Next

**Chart how the host's establishment incorporates a CONNECTING PEER — the joiner-member add.** The
2026-07-05 two-machine run ([SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ JOINER-ADMIT") proved the transport
admit path is the wrong door: the Deck's SYN reaches host-admit `0x142640e30` and the side-channel links,
but gate-c rejects, and **forcing gate-c accept doesn't help** (a 2nd gate `0x142640ed5` bails on the null
peer connection, and the capture shows gate-c rejects in real ERSC too). The joiner becomes a member via
the **session layer** (`add-member 0x1423fdf20`, same as the host's `member[5]`), using the joiner's **arg2
identity handle** derived from its connection.

1. **Chart the joiner path in the live capture data** — how did real ERSC's `member[4]`=Deck get added?
   Where did its `+0x70`/`+0x78` handles + `+0x80` SteamID come from (the joiner's connection object), and
   what triggered `add-member` for the peer (vs the host's own)? Use ERSC-LIVE-CAPTURE + a fresh
   writer-trace on the host during a Deck join if needed.
2. **Reproduce it:** when the Deck connects (we know its SteamID from rung-4/`coop: linked`), drive/allow
   the host to add it as a member — likely build the joiner's connection + identity handle, then
   `add-member`. Verify a Deck SteamID64 lands in an empty `member+0x80` slot (roster → 2).
3. **Stabilize the Deck joiner** — it crashes ~30–60s into the join drive; needs fixing for sustained tests.

- **Why this / why now:** host-side is done; the joiner-member is the last rung-3 leg, now correctly scoped
  to the session layer (not the transport admit rabbit hole we just ruled out).
- **Serial + Michael-gated:** two-machine (rig host + Deck joiner); orchestrator drives, verifies from memory.
- **Charted lever, OFF:** `force_gatec_accept` (forces host gate-c accept) — kept in code, not the path.

## Candidates Not Chosen

- **Offline synthesis of the session graph** — dead end at the object level (runtime-built native
  objects). Re-open only if the live capture shows establishment reduces to a small reproducible
  field/call set.
- **The gate-c / `+0x168` "real member-lookup"** avenue — **DEBUNKED** (stub even in a working
  session); members come from the session layer, not the transport admit gate. Do not re-chase.
- **stay-connected behavioral validation** (risks #1–#3: boss/area/death route through the gate +
  stay playable) — needs a live 2-player session; install+arm is already rig-validated. Fold into the
  next live co-op run, or just notice it in play.
(Done this session, no longer candidates: **static-prep of the live-trace targets** — landed as
[WRITER-TRACE-TARGETS.md](WRITER-TRACE-TARGETS.md), now folded into Next above; **task #13** — the
duplicated `leave_session` offset is single-sourced from `stay_connected::LEAVE_SESSION_OFFSET`.)

## Learned Recently (Pointers Only)

- [WRITER-TRACE-TARGETS.md](WRITER-TRACE-TARGETS.md) — the static aim sheet for the task-#16 live
  capture: four watchpoints (member registry, session-create, availability singleton, gate input),
  each with a backtrace-verified writer chain, arm recipes, and re-derive rules. Both writers sit
  behind vtable dispatch, so the live pass uses `watch-bt` backtraces to prove the chain.
- [STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md) — the standup factory is satisfiable offline;
  the offline wall is flow-non-entry + the downstream runtime availability signal, not the factory.
  §4 has the ranked offline-vs-live dump list; §5 the minimal driven-standup recipe.
- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) — the live 2-player ERSC object
  graph + offsets, the two corrections, and the re-run/writer-trace procedure.
- [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md) — stay-connected install+arm
  rig-validated; the site-A prologue was missing its leading REX `0x40` (now corrected + live-read),
  and the probe leave-tracer is gated so it stops stealing the gate's site.
