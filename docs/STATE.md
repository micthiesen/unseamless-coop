# State

The single **fast-moving** "what we're working on / what's next" file. It is **about the work** —
the current picture, the chosen next step, and the runners-up — **not** a snapshot of machine state.
**Overwritten, never appended** (git holds history). Owned by the orchestrator: `/wrap` rewrites it
when a session concludes, `/next` records next-step decisions in it, and `orch-start` seeds a fresh
orchestrator to read it and brief from it. Durable knowledge does **not** live here — findings go to
their proper doc (CLAUDE.md > "Project knowledge lives in the repo"); this file holds the current
picture, the chosen next step, and pointers.

> **Deliberately not tracked here:** live workers (use `scripts/fleet/worker-ls`), rig/Deck state (cheap
> to re-apply, never to remember or restore), and uncommitted git state (workers integrate before a wrap).

Last updated: **2026-07-05** (member-pipeline chart + drive_add_peer).

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the
  side-channel, and exchange game-P2P packets by SteamID64 alone.
- **★★ HOST-SIDE rung-3 ESTABLISHMENT IS REPRODUCED — solo AND two-machine** (stable `Host`/`Ingame`, full
  member graph, `member[5]+0x80` = host SteamID64). Details: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★
  HOST-SIDE ESTABLISHMENT REPRODUCED".
- **★★ THE JOINER-MEMBER PIPELINE IS FULLY CHARTED + LIVE-VALIDATED.** Per frame the host runs
  `update_step → 0x1423f6bf0 (SessionManagerSteam.update) → 0x1423fb690 (per-session update)`, which pumps
  a **pending-conn queue** `[session+0x4f0..+0x4f8]` (handshake pump `0x1424007e0` reading DLNW3D msgs from
  each conn's `+0x130` endpoint) and then **drains a lock-free event queue** (`SessionSteam` vt[28]
  `0x1423ff440`); a **type-1 event → `0x1423fe350` → add-peer `0x1423fdc80`** pops an empty member from the
  pool, sets `member+0x80` = the peer SteamID64 (`peerInfo[0]`, via `0x142402d70`→`0x142400480`), and
  enqueues it. Validated against the live object. See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★ MEMBER
  PIPELINE CHARTED".
- **★ `drive_add_peer` lever built + working (mechanically).** `[debug.probes] drive_add_peer` drives
  `0x1423fdc80` host-side (gated on `lobby_state==Host`) for the two-machine peer: it **creates a correct
  member for the Deck's SteamID** (ret=1, pool popped, queue grown, `member+0x80` = Deck ID). Two-machine
  confirmed **no natural producer** posts the add-peer event for the Deck (its SYN reaches host-admit + is
  rejected; `add-peer` never fires). Transport admit re-confirmed a dead end at the instruction level
  (gate-c's `[context+0x168]` stub short-circuits `cmp eax,1; je reject` before find-or-create).

## Next

**Wire the driven member's transport endpoint (`+0x130`) so its handshake completes.** `drive_add_peer`
builds a correct member except for `+0x130` (the transient handshake endpoint) — so the per-frame pump
`0x1424007e0` reads nothing and the session **drops the member** (solo; session survives). `+0x130` is set
only while handshaking (the live ERSC capture shows even a working remote member reads `+0x130=0` in steady
state). Two ways in:

1. **★ New ERSC capture, watching the writers (highest-leverage, Michael-gated ~10 min, Deck still set up):**
   on real ERSC, arm `scripts/re/watch-write.py` on a fresh remote member's `+0x130` **and** the session
   event queue `[session+0x578]` during a live Deck **join** — catch the RIP that sets `+0x130` (the endpoint
   source) and what posts the add-peer event (the producer). This pins both unknowns directly.
2. **Build the endpoint ourselves (static-first, then rig):** chart the holder endpoint-open call (the
   `0x14203f2xx` family the pump uses on `conn+0x130`), then after `drive_add_peer` pops the member, bind its
   `+0x130` to a transport endpoint on the stood-up holder keyed by the Deck SteamID, so the Deck's real P2P
   packets feed the pump. Two-machine verify: a Deck SteamID persists in a `member+0x80` slot past the
   handshake (roster → 2).
3. **Stabilize the Deck joiner** — crashes ~30–60s into the join drive (limits two-machine windows).

- **Why this / why now:** the whole consumer pipeline is charted + validated and the member is driveable;
  the *only* missing piece is the peer's transport endpoint. The capture (1) is the fast way to source it;
  (2) is the offline build if the capture stalls.
- **Serial + Michael-gated:** the capture and the two-machine verify need the rig host + Deck; the endpoint
  charting is solo/delegable.

## Candidates Not Chosen

- **Forcing / pre-seeding the transport admit** (`force_gatec_accept`, gate-c, `0x142640e30`) — **RULED OUT
  at the instruction level**: the identity callback `0x142639d00` rejects on the `[context+0x168]` stub
  (`cmp eax,1; je reject`) *before* find-or-create, so a pre-created member can't unblock it; the stub is
  present in real ERSC too. Lever kept OFF.
- **Finding the add-peer producer purely statically** — the enqueue helpers `0x1423fda40/b00/bc0` + the
  identity callback have **no static pointer refs at all** (runtime/Steam-callback-installed). Chase it via
  the capture (Next #1), not static.
- **Offline synthesis of the session graph** / the **`+0x168` "real member-lookup"** — long-dead ends.

## Learned Recently (Pointers Only)

- [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★ MEMBER PIPELINE CHARTED" — the full per-frame consumer pipeline
  (event drain → add-peer → pool pop → identity populate → enqueue → handshake pump), the empirical results
  (model validated live; `drive_add_peer` works; the `+0x130` endpoint gap), and the two ways to source the
  endpoint. Also "★★ HOST-SIDE ESTABLISHMENT REPRODUCED" and "★ JOINER-ADMIT".
- Code: `session_probe.rs` — new read-only `add-peer` hook (`0x1423fdc80`, under `instrument_host_accept`)
  and the `drive_add_peer` lever (`try_drive_add_peer`, gated on `lobby_state==Host`); `LIVE_SESSION` global
  captured off the `ADD-MEMBER` hook. Config: `[debug.probes] drive_add_peer` (seed default on for the rig).
- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "Writer-trace capture" — the member layout
  (`+0x80` = SteamID64), the add-member chain, the registry root, both red herrings.
- [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS" — footgun-safe two-machine
  procedure (pass `--auto-session` on `cycle`, not `apply`) + the Deck-crash gotcha.
