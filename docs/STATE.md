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

Last updated: **2026-07-05 late night** (★★ stall B narrowed to the joiner's INBOUND path across NINE
two-machine runs. Host-side `drive_add_peer` makes the HOST emit real DLNW3D SYNs to the joiner (runs 7/8);
the joiner-side symmetric add-peer CRASHES its Client session (run 9, null `+0x5f8`). So the joiner must build
the host connection from the host's real inbound SYNs via its own find-or-create `0x142640e30` — which isn't
firing. Next = chart the joiner's inbound path (channel / P2P-accept / admit gate), not another driver.)

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

**★ Stall B, narrowed to the joiner's INBOUND path.** The host now emits real DLNW3D SYNs to the joiner
(host-side `drive_add_peer` — runs 7/8); what's missing is the joiner's game building the host connection
from those inbound SYNs via its own find-or-create `0x142640e30` (inbound-only, writes `conn+0x138=hostID`) —
which never fires on the joiner (run 8: worker drains channel 30 with `connections_span=0`). Fix the joiner's
inbound path and the host's already-firing tag-1 handler `0x1423fe350` promotes the connection → `players=2`.
(NB: `session+0x5a8` is the VOICE object, not a connection — that run-5/6 lead was a dead end; and the joiner
must NOT drive add-peer itself: run 9 proved it crashes the Client session, null `+0x5f8`.)

- **★★ NINE two-machine runs (2026-07-05) walked it down** — full chain + all corrections + the eight
  "▶ RIG RESULT" addenda in [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ STALL-B HANDSHAKE AIM SHEET" and the
  two following aim-sheet sections. What's PROVEN live: host-side `drive_add_peer 0x1423fdc80` queues the
  joiner and the host's game emits **real 14-byte DLNW3D SYNs** to the joiner (`GAME-SENDP2P 0x142640b20`,
  runs 7/8); with bidirectional accept the host also RECEIVES the joiner's packets (`host-admit`, run 8);
  the host's real connection-creator `TAG1-CONNSTATUS 0x1423fe350` fires. What's NOT happening: the JOINER
  never builds a host connection — its worker drains channel 30 with `connections_span=0` and its
  find-or-create `0x142640e30` never fires on the host's inbound SYNs, so it parks at `Client/WaitInitData`
  and times out (~30s; the "INIT gate" `0x1423fbe10` is a countdown, not a data gate).
- **What to do next (static + serial):** chart the **joiner's INBOUND path** — why the host's real SYNs
  don't reach/pass the joiner's find-or-create `0x142640e30`. Three suspects: (a) **channel** — does the
  host's `SendP2PPacket` go out on the channel the joiner's worker `0x142640bc0` drains (30)? (b) **P2P
  accept** — does the joiner accept the host's Steam P2P session so Steam delivers the packets at all? (c)
  the joiner-side **admit shape/identity gate**. This is the joiner's *inbound* mechanism (the same
  inbound-only find-or-create that works on the host), NOT another outbound driver — run 9 proved the joiner
  must NOT drive add-peer (it crashes the Client session, null `+0x5f8`; `drive_add_peer_joiner` is OFF).
- **Instrument + config state (on `main`):** B0/B1/B4/B5 probes wired (`session_probe.rs`). Seed
  (`scripts/rig/seed-config.toml`): `drive_add_peer` ON (host queues joiner → host sends real SYNs),
  `host_skip_p2p_accept` **OFF** (run 8 flipped it — the host must accept the joiner so the return leg
  isn't dropped), `drive_add_peer_joiner` **OFF** (crashes the joiner), `drive_join` + `join_set_established_bit`
  ON (client reaches `Client/WaitInitData`). Do NOT hook `0x1423fb684` or the drain `0x1423ff446` (unaligned
  mid-region in the client update path — crash the client; session state is POLLED via `stall-B poll`).
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
