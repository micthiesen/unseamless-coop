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
- **★ TASK #16 LIVE WRITER-TRACE DONE (2026-07-04) — the member-add chain is captured.** In a real
  2-player ERSC session we watch-traced how a member is built and pinned the reproduction target
  ([ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "Writer-trace capture"):
  - **A member is a raw `SteamID64` at `member+0x80`** (a direct scalar, not a handle — corrects the
    aim sheet). Members are a **6-slot pre-alloc pool**; the host is a member of its own session.
  - **The member is added SYNCHRONOUSLY inside the establish handler `0x1423f2820`**, driven by
    `update_step 0x140cafd10`: `…→ 0x1423f2820 → session-create 0x1423f7070 → add-member 0x1423fdf20
    → member ctor 0x142400210 → +0x80 SteamID write`. **Not an async Steam callback.** This confirms
    the aim sheet's static chain and reframes rung-3: **drive `0x1423f2820` with a live connection +
    peer present and let it add the member.**
  - **Member registry root = `0x143dcd758` (container+0x388)**, not the aim sheet's +0x1e8.
- **Both remaining "walls" are now confirmed red herrings.** The `SteamServiceImpl` standup owner is
  the config as charted (factory satisfiable — [STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md)),
  AND the availability field `[[0x143d855c8]+0x10]` reads **0 in a working session** (was 1 offline) —
  so the host-setup gate `0x140de2620` we force via `suppress_leave` is **not on ERSC's establishment
  path**. ERSC forms the session through the establish-handler chain above, not that gate.

## Next

**Seed the establish handler's input descriptor so its builder builds a good socketmgr.** The 2026-07-05
rig run ([SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ REPRODUCTION") proved the driven establish handler
`0x1423f2820` passes every gate and **reaches the builder `0x142637440`** — the builder fails only because
the handler pipes **our zeroed input descriptor** (`[rbx+0..0x34]`) into the socketmgr config region
(`socketmgr[0x58..0xa0]`), clobbering the base-ctor defaults that a real descriptor (and `land_socket_holder`)
carry. (Charted: gate 3 always passes — `local[8]` is a hardcoded `0x1423f2d70`; the earlier "wall = `[+8]==0`"
commit was **wrong** and is corrected in SESSION-DRIVE.) `ADD-MEMBER` (new reach-hook) hasn't fired because
the builder gates the `establish → session-create 0x1423f7070 → add-member 0x1423fdf20` chain.

1. **Path A (try first):** seed the descriptor we hand `0x1423f2820` (`[rbx+0..0x34]`) with the socketmgr
   config defaults `land_socket_holder` already reads off a fresh socketmgr, so the establish-handler builder
   builds a good socketmgr → succeeds → the handler proceeds to session-create → add-member. Watch the
   **`ADD-MEMBER` hook** + member registry `0x143dcd758` fire.
2. **Path B (fallback):** `land_socket_holder` already builds a *working* connection; drive
   `session-create 0x1423f7070 → add-member 0x1423fdf20` directly on it, feeding the host SteamID. Avoids
   reconstructing the descriptor.

- **Why this / why now:** the establish handler is one good descriptor away from letting the game build the
  member itself; `land_socket_holder` already knows the config bytes, so path A is a small seed change.
- **Serial:** rig-owned (drive + watch the `ADD-MEMBER` hook); two-machine roster→2 confirmation later.

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
