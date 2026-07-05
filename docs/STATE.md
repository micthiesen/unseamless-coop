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

Last updated: **2026-07-05** (host builds joiner endpoint; symmetric-peer crash fix; ERSC #2 architecture).

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **★★ HOST-SIDE rung-3 ESTABLISHMENT IS REPRODUCED** (solo + two-machine): stable `Host`/`Ingame`, full
  member graph, `member[5]` = host SteamID64.
- **★★ THE JOINER-MEMBER PIPELINE IS FULLY CHARTED + LIVE-VALIDATED.** Per frame: `update_step →
  SessionManagerSteam.update 0x1423f6bf0 → per-session update 0x1423fb690`, which pumps a **pending-conn
  queue** `[session+0x4f0..+0x4f8]` (handshake pump `0x1424007e0`) and drains a lock-free **event queue**
  (`SessionSteam` vt[28] `0x1423ff440`); a **type-1 event → add-peer `0x1423fdc80`** pops an empty member and
  sets `member+0x80` = the peer SteamID64. `[debug.probes] drive_add_peer` drives that directly.
- **★★★ HOST BUILDS THE JOINER MEMBER + ENDPOINT — two-machine.** With `drive_add_peer` (throttled re-fire,
  gated on `lobby_state==Host`) keeping a member in the pending queue and the peer connected, the host's own
  per-frame pump (`0x1424007e0 → 0x1423ffd00 → 0x142401110`) builds `member+0x130` = a live
  `MTInternalThreadSteamConnection` — no endpoint-bind driver needed. This is the piece that blocked rung-3
  for months. See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★★ HOST BUILDS THE JOINER ENDPOINT".
- **★★ JOINER CRASH FIXED (`symmetric_peer`); BOTH machines stable + build each other's endpoint.** The
  crash was an FSM conflict (our joiner drove establish→`Host` AND join→`Client` at once → teardown → null
  crash). `symmetric_peer` makes both host-style peers (both send SYNs), and two-machine both build the
  other's fully-initialised endpoint. But it leaves both at `Host(3)` and the handshake never *completes*
  (member flags reach `(0,1,0,0)`, not the done `(0,0,1,0)`) because our 14-byte SYN isn't a real DLNW3D
  message — so the member churns and `players` stays 1. (Behavioral proof it's a real session: the host
  can't rest at a grace.) `symmetric_peer` is a proven diagnostic, not the shipping shape.
- **★★★ ERSC CAPTURE #2 settled the architecture + banked the handshake protocol.** Verified on BOTH
  machines: asymmetric in **FSM state only** (host `lobby_state=3` `Host`, client `=6` `Client`); the DLNR3D
  graph is built on both sides and **mirrors** (each side's *remote* member holds the endpoint). Completion =
  a **type-5 DLNW3D message** (`{8B token, 4B len, len·blob}`; token is a per-connection nonce) that the
  pump validates (member vtable[0x88] `0x142402ee0`) and sets `member+0x152`=done. Full transitive dumps of
  both machines at `~/Documents/ersc-session-ref-{host,client}.txt`. See
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★★ REFERENCE DUMP #2".

## Next

**Client = drive_join to `Client(6)` WITHOUT the conflicting establish; let its join emit the type-5.**
ERSC capture #2 (2026-07-05, verified on BOTH machines) settled the architecture: it's asymmetric in **FSM
state only** — host `lobby_state=3` (`Host`), client `lobby_state=6` (`Client`) — but the DLNR3D session
graph is built on BOTH sides and **mirrors** (each side's *remote* member holds the endpoint). So the member
machinery is symmetric (`symmetric_peer` was right about that); the divergence is the FSM role. **The joiner
crash was an FSM conflict:** our joiner drove `drive_establish_handler` (→ Host/3) AND `drive_join` (→
Client/6) at once → teardown → null-session crash. Fix:

1. **Client = join only:** `drive_join` → `TryToJoinSession(4)` → `Client(6)`, with **`drive_establish_handler`
   OFF** on the client so the FSM doesn't fight (the crash). The client's own join flow builds its graph AND
   emits the DLNW3D connect handshake — including the **type-5 message** (`{8B token, 4B len, len·blob}`) that
   the host's pump needs to set `member+0x152` and complete the host's member. (`symmetric_peer` stays a proven
   diagnostic — both build endpoints, both stable — but leaves both at `Host(3)` and sends no type-5, so it's
   not the shipping shape.)
2. **Host unchanged:** `drive_establish` + `drive_add_peer` builds the client member; the pump completes it
   when the client's type-5 arrives.
3. **Open question:** whether the client's `drive_join` reaches `Client(6)` + emits the handshake against our
   `drive_add_peer` host — i.e. does it need the host connection blob we previously bypassed (the blob-parse)?
   That bypass is likely why the earlier join left the session incomplete. Two-machine verify: client join →
   host `member[4]` completes (`flags (0,0,1,0)`, `+0x148` token) → `players=2` → client visible.

- **Ground-truth reference (banked):** full both-sides transitive dumps at
  `~/Documents/ersc-session-ref-{host,client}.txt`; the mirror structure, endpoint→context→connmgr→
  `SteamConnection(+0x138 peer id)` wiring, the per-connection nonce token (`member+0x148`), and the full
  8-type protocol + handlers are in [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) >
  "★★ REFERENCE DUMP #2". Completion = pump `0x1424007e0` type-5 case `0x142400924` → member vtable[0x88]
  validator `0x142402ee0`.
- **Why this / why now:** the host side of the joiner-member is solved and the crash is fixed; the only thing
  between us and a real 2-player session is the client completing its side (reaching `Client(6)` + emitting
  the type-5) without the establish/join FSM conflict.
- **Serial + Michael-gated:** two-machine (rig + Deck). Both machines were on ERSC for capture #2 at wrap;
  rig/Deck state is cheap to re-derive (re-apply our mod with `symmetric_peer` off + `drive_join` on the
  client, or the corrected client-join config, for the next test).

## Candidates Not Chosen

- **`symmetric_peer` as the shipping design** — proven diagnostic (both build endpoints, both stable) but
  leaves both at `Host(3)` and can't complete the handshake (sends no real type-5). Keep as a lever; the
  endgame is host `Host(3)` + client `Client(6)`.
- **Fabricating the type-5 completion message** — the token (`member+0x148`) is a per-connection nonce (it
  changes every rejoin) + a length-prefixed blob validated against the peer identity, so it can't be
  precomputed. The type-5 must come from the peer's real connect flow, not a synthesized packet.

- **Forcing / pre-seeding the transport admit** (`force_gatec_accept`, gate-c, `0x142640e30`) — **RULED OUT
  at the instruction level**: the identity callback `0x142639d00` rejects on the `[context+0x168]` stub
  (`cmp eax,1; je reject`) *before* find-or-create, so a pre-created member can't unblock it; the stub is
  present in real ERSC too. Lever kept OFF.
- **Finding the add-peer producer purely statically** — the enqueue helpers `0x1423fda40/b00/bc0` + the
  identity callback have **no static pointer refs at all** (runtime/Steam-callback-installed). Chase it via
  the capture (Next #1), not static.
- **Offline synthesis of the session graph** / the **`+0x168` "real member-lookup"** — long-dead ends.

## Learned Recently (Pointers Only)

- [SESSION-DRIVE.md](SESSION-DRIVE.md) — "★★ MEMBER PIPELINE CHARTED" (the per-frame consumer pipeline),
  "★★★ HOST BUILDS THE JOINER ENDPOINT" (`drive_add_peer` + pump builds `member+0x130`), "★★ SYMMETRIC PEER"
  (crash fix + both build endpoints; the handshake-completion gap), and "★★★ ERSC CAPTURE #2" (the corrected
  architecture: FSM-only asymmetry, mirrored graph, the type-5 protocol).
- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★★ REFERENCE DUMP #2" — the both-sides
  transitive graph, endpoint→context→connmgr→`SteamConnection` wiring, the nonce token, and the full 8-type
  DLNW3D protocol + resolved handler methods + the type-5 payload shape.
- Ground-truth dumps (persistent, uncommitted): `~/Documents/ersc-session-ref-{host,client}.txt`. Re-dump
  with `scripts/re/dump-session-ref.py` (host-local; push it + `scan-vtable.py` to the Deck for the client).
- Code: `session_probe.rs` — the `drive_add_peer` lever (`try_drive_add_peer`, throttled re-fire, gated on
  `lobby_state==Host`) + read-only `add-peer` hook; `symmetric_peer` mode (`rung3_role` forces host role,
  both send the DLNW3D SYN). Config: `[debug.probes] drive_add_peer` + `symmetric_peer`.
- [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS" — footgun-safe two-machine
  procedure. ERSC capture recipe + the Deck ERSC swap (move our `dinput8.dll` aside, push the rig's ERSC
  launcher) in `capture-endpoint.py` / `dump-session-ref.py` headers + ERSC-LIVE-CAPTURE-FINDINGS.
