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

Last updated: **2026-07-06** (run 16). **The type-5 producer is SOLVED — the hand-frame sender works and the type-5
crosses the wire bidirectionally** (rig 255B ↔ Deck 249B, each reaching the other's DLNW3D recv, `send_type5`, commit
`2ceddb0`). But the handshake still doesn't complete, and **the wall has moved and is now pinned**: the type-5 keeps
hitting the receiver's **find-or-create admit `0x142640e30`** — the "no connection found" branch — so **no transport
`SteamConnection` is registered for the peer** and the type-5 has nowhere to be *delivered* (→ never reaches the pump →
`+0x152` stays 0, `players=1`). The remaining gap is no longer producing the type-5; it's **registering the receiver's
transport connection** so recv delivers the type-5 into it. Full evidence: SESSION-DRIVE.md > "▶ RIG RESULT (run 16)".

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **The session graph is reproduced up to the handshake.** `drive_add_peer` queues the peer member; the
  game's own per-frame pump (`0x1424007e0 → 0x142401110 → 0x14203ef70`) **builds the peer's endpoint**
  `member+0x130` (a real `MTInternalThreadSteamConnection`, vtable `0x143277750`) — on both machines, stable,
  no crash. Run 12 confirmed it builds even with our fabricated SYN suppressed (the SYN was pure noise).
- **✅ THE TYPE-5 PRODUCER IS SOLVED (run 16).** The hand-frame sender (`send_type5`, commit `2ceddb0`) frames
  `[len-hdr][5][8B token=0][4B ticket_len][ticket]` (ticket = a real cached `GetAuthSessionTicket`) and
  `SendP2PPacket`s it unreliable on ch30 — never touching the Arxan-locked game send. **Rig-proven two-machine,
  bidirectionally:** rig sends 255B (240B ticket), Deck sends 249B (234B ticket), and **each machine's
  `instrument_host_accept` hook logs the other's type-5 arriving** (`host-admit ... msgSize=249`/`255`). It passes
  Steam delivery + the recv frame-length gate `0x1426425d0` on both peers — the 11-bit length header is correct.
  No crash. The send side (produce + cross the wire) is done.
- **★★ THE MOVED, PINNED GAP: no transport `SteamConnection` is registered for the peer, so the type-5 can't be
  DELIVERED.** The arriving type-5 keeps hitting **find-or-create `0x142640e30`** — per the recv chart (Q1) that's
  called **only when the sender-id search of `[socketmgr+0xb8..0xc0]` (for a `SteamConnection` with
  `+0x138==senderID`) finds nothing**. So recv has no connection to deliver into (`0x142643db0`→`0x142642860`),
  the bytes never reach the endpoint queue → never reach the pump's type-5 case → `+0x152` never sets. Live
  (`capture-endpoint.py`): the peer member has `+0x152=0` (flags `(0,1,0,0)`), `+0x130`(endpoint)=0 (still
  cycling), `players=1`. **Reconciles with correction #1** (gate-c `[context+0x168]` is a stub even in working
  ERSC): real ERSC registers the connection via the establishment/join flow, so recv *finds* it; our driven setup
  skips that flow, so the table has no entry. Full evidence: SESSION-DRIVE.md > "▶ RIG RESULT (run 16)".
- **The type-5 wire format is fully charted + now wire-validated.** Payload `{[5], 8B token, 4B len (1..0x400),
  len·ticket}`; validator `0x142402ee0` gates ONLY on the ticket (`BeginAuthSession` vs `member+0x80`, result ∈
  {0,2}) — the 8B token is stored unvalidated. Transport wrap = the 11-bit len header (`byte0=total&0xff`,
  `byte1=0x40|((total>>8)&7)`), confirmed correct by the packets reaching recv. Chart: SESSION-DRIVE.md > "★ TYPE-5
  PRODUCER + INJECTION"; framing code is host-tested in `core::dlnw3d`.
- **Producer approaches now RULED OUT (don't retry):** (a) *let the game's endpoints talk* (run 12); (b) *drive
  the game's own send `0x142400df0` cold* (runs 13–15: Arxan-locked vtable slot, crashes — OBSERVE-ONLY on `main`,
  do not re-enable). (c) is the shipped hand-frame path — it works; the block is downstream (delivery, above).
- **⚠️ RED HERRING (runs 4–11) — DO NOT RE-LITIGATE:** the transport gate-c / find-or-create `0x142640e30` /
  `[S+0x168]` member-resolve is **not** the admission mechanism — ERSC-LIVE-CAPTURE correction #1 proved
  `[context+0x168]` is the stub `0x1423fdf00` **even in a fully working ERSC session**. The connection is
  built by the session-layer pump, not find-or-create. (Banner atop SESSION-DRIVE.md > "★ JOINER INBOUND AIM
  SHEET".)

## Next

**★ Register the receiver's transport `SteamConnection` for the peer** so recv delivers the (already-arriving)
type-5 into it instead of falling through to find-or-create admit. The recv path (`0x142640bc0`) routes a packet
by searching `[socketmgr+0xb8..0xc0]` for a `SteamConnection` with `+0x138==senderSteamID64`; on a hit it delivers
(`0x142643db0`→`0x142642860`) into the endpoint queue → pump → type-5 case → validator → `member+0x152=1` →
`players=2`. Today that search finds nothing (every type-5 hits find-or-create `0x142640e30`), so **the job is to
get a connection keyed to the peer id into that table.** `stand_up_transport` already *builds* a `SteamConnection`
off the game heap but never registers it in the socket-manager connection table. Serial (orchestrator drives rig +
Deck). Plan doc: SESSION-DRIVE.md > "▶ RIG RESULT (run 16)" > "► NEXT".

**First, a cheap de-risking diagnostic (do this before building the register):** hook the receive-side pump
`0x1424007e0` and validator `0x142402ee0` to positively confirm the type-5 **never reaches the pump today** (vs.
reaching it and failing the ticket validate). This tells us whether the wall is purely delivery-side (the run-16
read) or also validation-side (e.g. `member+0x80` mismatch, ticket not yet valid). If the type-5 never reaches the
pump, the register-the-connection work is the whole job; if it reaches the pump but the validator rejects, the ticket
timing / `member+0x80` identity is the issue instead.

### The register work — the questions to answer (in order)

1. **The connection-table shape.** Confirm `[socketmgr+0xb8..0xc0]` is the array recv searches and `+0x138` is the
   `SteamConnection`'s peer-id key (both from the Q1 chart — re-verify live with `scan-vtable.py` / a read of the
   socketmgr). Find where `socketmgr` is reachable from what `stand_up_transport` already holds.
2. **The register primitive.** Is there a game fn that inserts a `SteamConnection` into the table (the natural
   join flow's registrar), or do we write the array slot + `+0x138` ourselves? Prefer the game fn if charted.
3. **Which connection object.** The one `stand_up_transport` builds, or the DLNW3D endpoint's own transport
   (`endpoint+0x18`)? They must be the same object recv's deliver path (`0x142642860`) expects.
4. **Symmetric vs one-sided.** Both machines already send + receive; registering on both (symmetric) should
   complete both members. Keep `symmetric_peer` + `send_type5` on.

**Ticket-timing note (still relevant):** `GetAuthSessionTicket` fills bytes synchronously but a remote
`BeginAuthSession` only accepts after Steam fires `GetAuthSessionTicketResponse_t` (~ms). The sender already
fetches once + caches + retries on a throttle, so once delivery is fixed the retries cover the validity window
(first sends may bounce `InvalidTicket`; accept set is `{0,2}`).

**Watch item (latent, fix when the handshake lands):** the client's self-identity
`[[member+0x58]+0x7f8]==0` under the bit-2-OR lever misroutes the client's *self* member; set `+0x7f8`
directly alongside the bit-2 OR once a session establishes.

## Candidates Not Chosen

- **Seamlessness disconnect-suppression gate — ALREADY SHIPPED (2026-07-04), don't rebuild.**
  `gameplay.stay_connected` (default off): hooks both charted sites, settings toggle, host-tested core, toast,
  install+arm rig-validated solo (`7ad9000`/`38917c0`/`1981f70`). Remaining is rig-gated only: live 2-player
  validation + caller→cause attribution in the suppression log — both unblock the moment co-op is 2-player.
- **Post-rung-3 wiring** (inert overlay toggles, nameplate color-by-SteamID, session-roster event toasts) —
  gated on 2-player co-op by definition; unblocked the moment `players=2` lands.
- **Build/produce the type-5 (any variant)** — DONE (run 16). The hand-frame sender works and the type-5 crosses
  bidirectionally; don't re-open production. Ruled-out production sub-paths: drive the game's own send
  `0x142400df0` (runs 13–15, Arxan-locked, observe-only on `main`, do not re-enable); let the game's endpoints
  talk (run 12).
- **Capture a real ERSC type-5's on-wire bytes** — NOT NEEDED anymore. Our hand frame passes the recv
  frame-length gate `0x1426425d0` on both peers (the 11-bit len header is correct), so the "inner transport frame"
  worry (decision-1 fallback) did not materialize. The block is delivery-side, not framing-side.
- **Post-rung-3 wiring** (inert overlay toggles, nameplate color-by-SteamID, session-roster event toasts) —
  gated on 2-player co-op by definition; unblocked the moment `players=2` lands.
- **Seamlessness disconnect-suppression live 2-player validation** — rig-gated on 2-player, same as above.
- **The transport gate-c / `[S+0x168]` member-resolve** — still a RED HERRING for *admission*, but note run 16
  refines the picture: the real receive-side gap is the missing **connection registration** in
  `[socketmgr+0xb8..0xc0]` (recv's routing table), upstream of any admit gate.

## Learned Recently (Pointers Only)

- **The whole type-5 / handshake trail** → [SESSION-DRIVE.md](SESSION-DRIVE.md): "▶ RIG RESULT (run 16)" (the
  hand-frame sender proven bidirectional; the delivery-side wall pinned to the missing connection registration) is
  the current head; then "★ TYPE-5 PRODUCER + INJECTION" (validator `0x142402ee0`, the recv chart Q1 with the
  `[socketmgr+0xb8..0xc0]` routing table), "★★ ENDPOINT CAPTURED" + "★★★ HOST BUILDS THE JOINER ENDPOINT" (the pump
  builds `member+0x130`). The 8-type protocol + type-5 payload:
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★ The DLNW3D connect protocol" +
  correction #1 (the `[context+0x168]` stub is stub-in-working-ERSC too).
- **Arxan gotcha** → the [`/reverse-engineer`](../.claude/skills/reverse-engineer/SKILL.md) skill >
  "Capturing Arxan-Decoded Call Targets": you **can't drive (call cold)** a game function whose own dispatch
  goes through an Arxan slot; the tell is a call target below the image base, identical across ASLR launches —
  observe-log the slot before calling, and replicate the effect without the dispatch instead.
- **Code on `main`:** `core::dlnw3d` (host-tested type-5 framing + `wrap_transport_frame`, `2ceddb0`) +
  `steam::get_auth_session_ticket` (`be7f64e`); `session_probe.rs` — the **hand-frame sender** `send_type5` +
  `prepare_type5_packet` (`2ceddb0`, the working producer), the `drive_type5` observe-only send-drive (`ec691cc`,
  retired — do not re-enable), the endpoint-set latch `0x14203ef70`, `drive_add_peer`, `symmetric_peer`/`suppress_syn`
  levers, and `instrument_host_accept` (the receive-side admit hook that logged the type-5 arriving). Config gates in
  `unseamless-core/config.rs`; the seed (`scripts/rig/seed-config.toml`) documents each flag inline.
- **Current seed** (`scripts/rig/seed-config.toml`): `symmetric_peer` + `drive_add_peer` + **`send_type5` ON**
  (both build endpoints + send their type-5); `suppress_syn` ON; `instrument_host_accept` ON (the receive-side
  signal); `drive_type5` OFF (retired); both machines `--auto-session host`. This is the run-16 config; the next
  run adds the connection-register lever.
- **Workflow/rig learnings:** solo-precheck a new sender/producer on the local rig *before* pulling Michael in
  for the Deck — run 16's send half (ticket fetch 240B, framing, `SendP2PPacket=true`, no crash) was fully
  confirmed solo (`rig.sh cycle --auto-session host` + grep the log) while the Deck booted, so the two-machine
  run was clean; a running game is NOT a blocker (kill-first then cycle); don't chain `sleep N; cmd` (poll or
  `run_in_background`); `instrument_host_accept` is the receive-side signal (it logs the peer's type-5 arriving).
- **Ground-truth ERSC captures** (persistent, uncommitted): `~/Documents/ersc-session-ref-{host,client}.txt`
  + `ersc-live-capture*.txt` — the real 2-player member graph, the type-5 payload, the mirrored endpoints.
