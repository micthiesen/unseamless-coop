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

Last updated: **2026-07-05 evening** (★★ stall B instrumented across FOUR two-machine runs — the game's real
P2P event door `0x1423fd560` found + FIRED on the host via the accept-unmask lever; stall B is now two
precisely-separated walls: the client never engages its transport, and the host's event handler doesn't
create the joiner connection).

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

**★ Stall B: complete the DLNW3D connect handshake so the client leaves `WaitInitData` → the type-5 send →
host `player=2`.** The emitter connection is built and both machines are stable — the last gap is that the
client's emitter phase machine doesn't advance past the initial wait and the host never answers with init-data.

- **★★ B0/B1 INSTRUMENTED + RUN (2026-07-05 evening, four two-machine runs)** — full results + three aim-sheet
  corrections in [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ STALL-B HANDSHAKE AIM SHEET" > "▶ RIG RESULTS". The
  headline: the charted "P2P socket callbacks" were the REGISTRARS; the real runtime callbacks are
  **`0x1423fd550/560/570`**, and with the host kept fully silent on legacy P2P (the new
  `[debug.probes] host_skip_p2p_accept` lever — our probe's own Accept/pings were eating the session request),
  **`0x1423fd560` FIRES on the host with rdx = its own `SessionSteam`** as the Deck's packets arrive — the
  farthest a real inbound packet has ever reached. It does not (yet) create the joiner connection.
- **Stall B is now two separated walls:** (1) **client**: the joined session never engages its transport — the
  pending-conn span stays 0 all session (nothing pumps, nothing sent; state 4 is a park, and the "INIT gate"
  `0x1423fbe10` is really a ~30s countdown-to-give-up, correction 2); (2) **host**: `0x1423fd560` fires on
  inbound but bails before creating the `SteamConnection`/pending entry. Also: `0x1423fb684` is NOT hookable
  (mid-function; crashed the Deck client — correction 1; state is now POLLED via `stall-B poll`).
- **What to do next (static lane, delegable):** chart **`0x1423fd560`'s body** (what it does with the event
  object rcx / r8=`0x546adb80`, and which gate stops connection-creation for an unknown peer — the member
  lookup again?), its siblings `d550/d570`, and **what enqueues a joiner's FIRST pending-conn entry /
  initiates the client's real transport connect** (the client-side missing link). Then a serial run drives
  whatever lever that chart exposes.
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
