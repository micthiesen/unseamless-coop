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

**The two-machine JOINER test — needs Michael + the Deck.** Host-side establishment is reproduced solo
(above). The remaining rung-3 step: a real Deck joiner over the DLNW3D Steam P2P transport (rig-proven
two-machine) should make the host's establish/admit flow **populate one of the 5 empty member slots with
the Deck's SteamID64** → `SessionManagerSteam` roster grows past 1 → both players in each other's world.

- **What to run — TURNKEY procedure:** [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ READY-TO-RUN".
  Reproduction config is now the seed default (both machines get it). Deck: `deck.sh apply --auto-session
  join` → `cycle`. Rig: `rig.sh cycle`. Verify (orchestrator, from memory): a Deck SteamID64 in a
  previously-empty `member+0x80` slot = roster grew = joiner admitted. **Prep done 2026-07-05 (seed synced,
  Deck artifacts built); blocked only on the Deck being awake** (it's asleep + won't wake over the network).
- **Why this / why now:** the host graph is proven to build the game's way; the only untested leg is whether
  an inbound peer gets added as a member (the joiner half of "let the game establish it").
- **Serial + Michael-gated:** the actual in-world verification needs eyes on both machines; the orchestrator
  can prep the Deck (apply our mod, seed) but the co-op confirmation is human.

**Solo follow-ups (no Deck needed), if useful before the two-machine run:**
- The `ADD-MEMBER` fired 7× solo — chart why (retry loop? one per slot?) and confirm only the host member
  populates solo (it does: `member[5]` = rig, rest empty).
- Confirm the reproduced session survives without `suppress_leave` (the capture showed that gate isn't on
  the establishment path, so it may no longer be needed to hold `Host`).

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
