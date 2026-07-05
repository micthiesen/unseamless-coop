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

Last updated: **2026-07-05 night** (★★ stall B walked down to its root across SIX two-machine runs: the
client's connect chain RUNS and the host's real connection-creator `0x1423fe350` FIRES, but the client's
connection object `session+0x5a8` carries **no peer identity** — the join builds the object shell and never
wires the host's SteamID64 into it, so the client dials nobody and the host has no real inbound connection to
promote. The missing wire is the peer-identity write on the joiner).

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **★★ HOST-SIDE rung-3 ESTABLISHMENT REPRODUCED** (solo + two-machine): stable `Host`/`Ingame`, full member
  graph (`member[5]`=host SteamID64); the host receives the joiner's SYN at admit `0x142640e30` and
  `drive_add_peer 0x1423fdc80` enqueues a pending member for the joiner, held for handshake completion.
- **★★★ CLIENT EMITTER CONNECTION NOW BUILDS (2026-07-05, two-machine) — the piece that blocked rung-3 for
  months.** The corrected **asymmetric** shape works: host establishes (host-gated `drive_establish_handler`),
  client `drive_join`s, no FSM-conflict crash. The wall was the join's readiness gate `0x1423fd7a0` (container
  predicate `0x1423f4330` needs `container+0x7c0` bit 2). Setting it (`join_set_established_bit`, a **direct
  bit-2 OR** — NOT the handler, which crashes) passes readiness → `join-blob-parse 0x1423fb260` runs → the
  per-peer emitter connection is created (`[G+0x28]=1`), and the Deck reaches `Client/WaitInitData`, **stable,
  no crash.** See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ CLIENT-JOIN AIM SHEET" > "▶ RIG RESULT".
- **The remaining gap is stall B (the handshake).** Client parks at `WaitInitData` waiting for the host's
  init-data; it never comes (the real DLNW3D connect handshake between the two connection objects doesn't run),
  so the session times out gracefully to `None` ~30s later. Both local objects exist; they don't yet talk.
- **The type-5 completion protocol is banked** (ERSC capture #2): completion = a type-5 DLNW3D message (Steam
  auth ticket via `GetAuthSessionTicket`) the host validates with `BeginAuthSession` (`0x142402ee0`) →
  `member+0x152=1`. Dumps at `~/Documents/ersc-session-ref-{host,client}.txt`.

## Next

**★ Stall B, root cause found: the joiner's connection object has no peer identity, so the client dials
nobody.** Get the host's SteamID64 wired into the client's `session+0x5a8` connection object (or drive the
real peer-directed connect call) → the client opens a P2P connection to the host → the host's already-firing
tag-1 handler `0x1423fe350` promotes it → `players=2`.

- **★★ SIX two-machine runs (2026-07-05) walked it down** — full chain + all corrections in
  [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ STALL-B HANDSHAKE AIM SHEET" (+ the "P2P-EVENT + CLIENT-CONNECT
  AIM SHEET" section and its three "▶ RIG RESULT" addenda, runs 4/5/6). Confirmed live: the client's state-4
  phase chain fully walks and `CLIENT-CONNECT-INIT 0x142401e80` runs; the host's real connection-creator
  `TAG1-CONNSTATUS 0x1423fe350` fires. **But** the client's embedded connection object
  (`session+0x5a8`, vtable `0x1431fa918`) dumps **no peer SteamID64** — and `0x142401e80` calls
  `iface_vtable[0x40]` with no peer arg (it's a per-frame pump/status step, not a peer-directed dial). So the
  join builds the object shell and never populates the host identity; the client connects to nothing, the
  host has no real inbound connection to promote, and it times out (~30s, the "INIT gate" `0x1423fbe10`).
- **What to do next (static lane, delegable — spawned):** decode the `+0x5a8` connection object (vtable
  `0x1431fa918`): its layout, where a peer SteamID64 belongs, and which fn WRITES it on a genuine joiner
  (start from the join-blob parser `0x1423fb260` with `[conn+0x58]` populated — follow the 8-byte host blob).
  Resolve `iface_vtable[0x40]` (ctx `0x143b48a00`, method 8) and find the joiner's REAL "connect to peer"
  call (the one that takes a SteamID64) — that, not `0x142401e80`, is the lever to drive. Then a serial run
  drives it.
- **Instrument state:** B0/B1/B4 probes are wired + on `main` (`session_probe.rs`); `[debug.probes]
  host_skip_p2p_accept` seeded ON (host stays silent on legacy P2P so the game's own event door fires). Do
  NOT hook `0x1423fb684` or the drain `0x1423ff446` (both unaligned mid-region in the client update path —
  crash the client; state is POLLED via `stall-B poll` instead).
- **Watch item (latent, not the stall):** client self-identity `[[member+0x58]+0x7f8]==0` under the bit-2-OR
  lever misroutes the client's **self** member once a session establishes. Fix when the handshake lands: set
  `+0x7f8` directly alongside the bit-2 OR. See aim sheet Task 5.

## Candidates Not Chosen

- **Seamlessness disconnect-suppression gate** (`leave_session 0x140cae730` armed flag,
  [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md)) — charted and buildable as code, but its
  validation needs a real 2-player session, so it queues directly behind stall B; building it now widens
  surface without proof.
- **Post-rung-3 wiring** (the inert overlay toggles, nameplate color-by-SteamID, event toasts on the session
  roster) — rung-3-gated by definition; unblocked the moment stall B completes.
- **`symmetric_peer` as the shipping design** — proven diagnostic (both build endpoints, both stable) but
  leaves both at `Host(3)` and completes no handshake (no real type-5). Superseded by the asymmetric
  client-join shape (host `Host`, client `Client`), which is now emitter-proven. Keep only as a diagnostic.
- **Calling the session-established handler `0x1423f4870` / `drive_session_established` for the client** —
  RULED OUT: it passes readiness + builds the emitter but also builds a `+0x708` establish-session artifact
  that crashes the joiner ~30s later at `eldenring.exe+0x3f4860` (null-session signature). The direct bit-2 OR
  is the safe lever.
- **Fabricating the type-5 completion message** — the token is a per-connection nonce + a Steam auth ticket
  (`GetAuthSessionTicket`) validated by `BeginAuthSession` against the client's SteamID64; it can't be
  precomputed. It must come from the client's real connect flow.
- **Forcing the transport admit** (`force_gatec_accept`, gate-c `0x142640e30`) — RULED OUT at the instruction
  level (the identity callback `0x142639d00` rejects on the `[context+0x168]` stub before find-or-create; the
  stub is present in real ERSC too). The joiner-member is a session-layer problem, not transport admit.

## Learned Recently (Pointers Only)

- [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ CLIENT-JOIN AIM SHEET" — the join→emitter pipeline (readiness gate
  `0x1423fd7a0`, container predicate `0x1423f4330` bit 2, blob parser, type-5 send `0x142400df0`), the minimal
  blob (any non-empty bytes), and the **▶ RIG RESULT** (2026-07-05): emitter built two-machine, the bit-2-OR
  vs handler-crash finding, and the stall-B picture.
- Code: `session_probe.rs` `SessionJoinDriver` — the `join_set_established_bit` lever (OR-in `container+0x7c0`
  bit 2 before `drive_join`, no handler) + registry init; host-gating of `drive_establish_handler` on
  `do_create` in `install_create_gate_trace`. Config: `[debug.probes] join_set_established_bit` (+ `drive_join`,
  `symmetric_peer` off, `stand_up_transport`/`land_socket_holder` on). Seed: `scripts/rig/seed-config.toml`.
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) > "Rig constraints & gotchas" — re-cycling onto a new build: `kill` before
  `cycle` (apply refuses over a live game → silent no-op), and don't trust a log grep that can match the
  *previous* run's FSM line (the cdylib appends to one log). New this session.
- [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS" — the footgun-safe two-machine
  procedure (rig host `cycle --auto-session host`, then Deck `apply --auto-session join` + `launch` + `dismiss`);
  pass the role on every cycle.
- Ground-truth ERSC dumps (persistent, uncommitted): `~/Documents/ersc-session-ref-{host,client}.txt`; the full
  8-type DLNW3D protocol + the type-5 completion path in
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★★ REFERENCE DUMP #2".
