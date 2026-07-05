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
- **★★★ HOST-SIDE JOINER-MEMBER + ENDPOINT SOLVED — two-machine (2026-07-05).** `[debug.probes]
  drive_add_peer` (throttled re-fire, gated on `lobby_state==Host`) keeps a Deck member in the session's
  pending-conn queue; with the Deck connected two-machine, **the host's own per-frame pump built the Deck
  member's transport endpoint** — `member[4]+0x130` = a live `MTInternalThreadSteamConnection` (vtable
  `0x143277750`), the member persisted, and a handshake flag advanced (`+0x151 0→1`). This is the piece that
  blocked rung-3 for months. Mechanism (ERSC-capture-confirmed): the game's pump `0x1424007e0 → 0x1423ffd00
  → 0x142401110` builds the endpoint once the peer's packets arrive — no endpoint-bind driver needed, just a
  member in the queue. See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★★ HOST BUILDS THE JOINER ENDPOINT" and
  "★★ ENDPOINT CAPTURED".

## Next

**Complete + stabilise the JOINER (Deck) side — its mirror of the host solution.** The host now builds the
Deck member+endpoint; the full 2-player session is blocked only by the joiner: **the Deck crashes ~90s in**
(`eldenring.exe+0x3f4860`, a null-`this` getter reading `[rcx+0x1c5]`; the null is `[r14+0x1e508]`), before
the host-side handshake completes. Root cause: the joiner's session is incompletely built — `drive_join`
bypasses the blob-parse, so the joiner never fully reaches `Client` and leaves game session/network
sub-objects null that the per-frame update later dereferences.

1. **Mirror the host fix on the joiner:** instead of the fragile `drive_join` + bypasses, have the joiner
   drive the establish path (like the host) and `drive_add_peer` for the **host's** SteamID, so the joiner's
   own pump builds the host-member endpoint from the host's packets (symmetric). This likely also builds the
   sub-objects whose absence crashes it.
2. **Or fix the crash directly:** chart what builds `[r14+0x1e508]` on a real ERSC joiner (identify `r14`'s
   system + the null sub-object) and ensure the joiner's driven session builds it, so the getter has a valid
   `this`.
3. **Then verify roster → 2:** with both sides stable, the host handshake completes → `member[4]` endpoint
   fully wired (`+0x8`/`+0x50` populated, flags → `(0,0,1,0)`) → both players in each other's world.

- **Why this / why now:** the host side of the joiner-member is done; the only thing between us and a real
  2-player session is the joiner completing its side without crashing.
- **Serial + Michael-gated:** two-machine (rig host + Deck joiner); the crash/joiner charting is solo/delegable.
  Both machines are currently on our mod (ERSC not restored, per Michael).

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
