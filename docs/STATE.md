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

**Complete the DLNW3D connect handshake so a driven member persists + gets roster-promoted.** The joiner
crash is FIXED (`symmetric_peer` mode) and — two-machine — **both peers now build each other's
fully-initialised `member+0x130` `MTInternalThreadSteamConnection`**, both machines stable. The *only*
remaining gap is that the handshake never completes: member flags reach `(0,1,0,0)` (`+0x151`, set by the
endpoint build) vs the working ERSC `(0,0,1,0)` (`+0x152`, set only by a real handshake message). So the
member times out ~30s, is dropped + re-added (endpoint rebuilds transiently), and `players` stays 1.
Root-caused: **our 14-byte SYN isn't a valid pump message** — the pump `0x1424007e0` dispatches `buf[0]` as a
type 1..8 (jump table `0x1424009f8`); `0x0e`=14 is out of range → ignored. The type that sets `+0x152` is a
real DLNW3D handshake message we don't send. See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★ SYMMETRIC PEER".

1. **Let the built endpoints drive the handshake:** once `member+0x130` (`MTInternalThreadSteamConnection`)
   exists it has its own send/recv (callbacks `endpoint+0x20/+0x28`). Try (a) **stop our SYN-spam once the
   endpoint is built** (channel-30 collision may block the game's endpoint traffic), and (b) **extend the
   ~30s member timeout** (`0x1424004e0` finalize) so the endpoint persists long enough to finish — the churn
   drops it before the handshake can complete (a poke of `+0x152` mid-churn didn't stick).
2. **Or chart + drive the real DLNW3D connect sequence:** the type-1..8 messages the pump dispatches (esp.
   the type whose `conn vtable[0x88]` sets `+0x152`) — likely a quick **ERSC capture** watching what the pump
   reads/writes on a member's `conn+0x130` during a live join (both machines still set up on our mod; ERSC
   swap-in is `rig.sh restore` + the Deck ERSC files are aside).
3. **Verify roster → 2:** completed member (`flags (0,0,1,0)`, persists) → promoted via `0x140cb31b0` →
   `players=2` → both players see each other.

- **Why this / why now:** everything up to a persistent, roster-promoted joiner member now works
  (host+joiner build each other's endpoint, no crash). The final mile is the connect-handshake completion.
- **Serial + Michael-gated:** two-machine; the handshake charting/capture is Michael-gated. Both machines are
  on our mod with `symmetric_peer=true` (ERSC not restored, per Michael).

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
