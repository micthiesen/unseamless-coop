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

Last updated: **2026-07-06** (★ COURSE-CORRECTION. Cross-checking the ERSC captures (Michael: "use the info
I captured") revealed runs 4–11 — including this session's runs 10/11 chasing the transport gate-c /
`[S+0x168]` member-resolve — were a **red herring**: ERSC-LIVE-CAPTURE correction #1 already proved that stub
is present *even in working ERSC*, and the connection is built by the session-layer pump, not find-or-create.
The **real frontier** (charted 2026-07-05, ahead of the stall-B trail): the endpoint `member+0x130` is
already built by the game's pump; the sole gap is **completing the DLNW3D handshake** (`member+0x152=1` via a
real type-5 message — our 14-byte SYN is out of the pump's type range). STATE re-anchored on that; the
gate-c path is now marked DO-NOT-RE-LITIGATE.)

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **Host-side rung-3 establishment reproduced; client reaches `Client/WaitInitData`.** Host drives its own
  establish handler to a stable `Host`/`Ingame` with the full member graph; the client's `drive_join` +
  `join_set_established_bit` (direct `container+0x7c0` bit-2 OR) builds the per-peer emitter connection and
  parks at `Client/WaitInitData`, stable, no crash.
- **★★ REAL FRONTIER = complete the DLNW3D handshake (`member+0x152=1`). The endpoint is already built.**
  The captured 2026-07-05 state (SESSION-DRIVE > "★★★ HOST BUILDS THE JOINER ENDPOINT" + "SYMMETRIC PEER"):
  `drive_add_peer` queues the peer member and **the game's own per-frame pump `0x1424007e0 → 0x142401110 →
  0x14203ef70` builds `member+0x130`** (a live `MTInternalThreadSteamConnection`, vtable `0x143277750`) — on
  both machines, stable, no crash. The one gap: member flags reach `(0,1,0,0)` (`+0x151`, set by the endpoint
  build) vs working ERSC `(0,0,1,0)` (`+0x152`). `+0x152` is set only by a **type-5** pump message; our
  fabricated 14-byte SYN (`buf[0]=0x0e`=14) is **out of the pump's type-1..8 range → ignored**, so the
  handshake never completes and the member is dropped/re-added, `players` stuck at 1.
- **⚠️ RED HERRING (runs 4–11, incl. this session's 10/11) — DO NOT RE-LITIGATE:** the transport
  gate-c / find-or-create `0x142640e30` / `[S+0x168]` member-resolve path is **NOT** the admission mechanism.
  ERSC-LIVE-CAPTURE-FINDINGS correction #1 proved `[context+0x168]` = the stub `0x1423fdf00` **even in a fully
  working 2-player ERSC session**; run 11 only re-confirmed the stub is `mov eax,1; ret` on our side too. The
  connection is built by the **session-layer pump** (above), not the transport worker's find-or-create. Levers
  3a/3b (make `[S+0x168]` real) are moot — a real session doesn't have it real either. `drive_add_peer_joiner`
  and `session+0x5a8` (voice, `0x142401e80`) remain killed leads.
- **The type-5 completion protocol is fully charted** (ERSC capture #2). Pump `0x1424007e0`, `buf[0]`=type,
  jump table `0x1424009f8`; **type 5** (`0x142400924`) reads 8B token, calls conn `vtable[0x88]` validator
  `0x142402ee0` → `conn+0x148`=token, **`conn+0x152`=1 (COMPLETE)**. Type-5 payload = `{8B token, 4B len
  (1..0x400), len·blob}` = a real **Steam auth ticket** (`GetAuthSessionTicket`) validated with
  `BeginAuthSession` against `member+0x80`. Full 8-message table: ERSC-LIVE-CAPTURE-FINDINGS > "★ The DLNW3D
  connect protocol". Dumps at `~/Documents/ersc-session-ref-{host,client}.txt` + `ersc-live-capture*.txt`.

## Next

**★ Complete the DLNW3D handshake — get `member+0x152=1`.** The endpoint (`member+0x130`) is already built
by the game's own pump; the sole gap is that a **valid type-5 completion message** never reaches the pump in
our driven setup. Two approaches, cheapest first:

**(a) FIRST — cheap experiment, no ERSC restore, no new capture: let the game's own endpoints talk.** Our
fabricated 14-byte SYN spam on channel 30 is ignored by the pump (out of type range) and may be *interfering*
with / crowding out the game's own connect flow, which owns the built endpoint's real send/recv
(`endpoint+0x20/+0x28`). Test in `symmetric_peer` mode (both build endpoints):
1. **Stop sending the fabricated SYN once `member+0x130` is built** (gate the `drive_p2p` SYN on
   endpoint-null) — small change in `session_probe.rs`.
2. **Hold the member longer** — extend the ~30s drop / keep `drive_add_peer` re-firing so the endpoint
   persists long enough for the game's connect flow to run.
3. Two-machine (both on our mod); watch `member+0x152` / flags `(0,0,1,0)` and `players→2`. Read `+0x152`
   with `capture-endpoint.py` (or a probe). Serial — I drive both machines, no ERSC restore.
If the game's own flow then emits a real type-5 → done. If not, (b).

**(b) If the game never emits a type-5 on its own:** the joiner's connect flow must produce a real Steam auth
ticket (`GetAuthSessionTicket`) type-5 = `{8B token, 4B len, len·blob}` that the host validates
(`0x142402ee0`/`BeginAuthSession`) → `conn+0x152=1`. Chart where the game's connect flow *would* call
`GetAuthSessionTicket` and why it doesn't fire in our driven setup (gated on a step we bypass?), then
drive/relay it. The hard mile — but everything up to a persistent, endpoint-wired joiner member already works.

**Do NOT re-run the transport gate-c / `[S+0x168]` path** (see the ⚠️ RED HERRING bullet in Now). `players`
stuck at 1 is the *handshake* not completing, not a connection failing to build.

- **Decision trail (2026-07-05, /next): chart delegated → landed → integrated; confirm run is serial and
  now config-ready.** The static charting lane (`inbound-chart`) completed and integrated (commit
  `07ca862`, the aim sheet), then was torn down. The three suspects collapsed to one (member-resolve gate);
  full map + all nine prior runs: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ JOINER INBOUND AIM SHEET" (and
  the "★ STALL-B HANDSHAKE" / "P2P-EVENT" / "JOINER PEER-WIRE" sheets it builds on, with their runs-4–9
  "▶ RIG RESULT" addenda).
- **Instrument + config state (on `main`):** B0/B1/B4/B5 probes wired (`session_probe.rs`). Seed
  (`scripts/rig/seed-config.toml`): `drive_add_peer` ON (host → real SYNs), `host_skip_p2p_accept` **OFF**
  (host must accept the joiner so its return leg isn't dropped), `drive_add_peer_joiner` **OFF** (crashes the
  joiner), `drive_join` + `join_set_established_bit` ON. Do NOT hook `0x1423fb684` or the drain `0x1423ff446`
  (unaligned mid-region in the client update path — crash the client; session state is POLLED via `stall-B
  poll`).
- **Watch item (latent, not the stall):** client self-identity `[[member+0x58]+0x7f8]==0` under the bit-2-OR
  lever misroutes the client's **self** member once a session establishes. Fix when the handshake lands: set
  `+0x7f8` directly alongside the bit-2 OR.

## Candidates Not Chosen

- **Seamlessness disconnect-suppression gate — ALREADY SHIPPED (2026-07-04), don't rebuild.** It landed as
  `gameplay.stay_connected` (default off): `coop/stay_connected` hooks both charted sites (leave_session
  `0x140cae730` + the inlined twin), settings-registry toggle, host-tested `SuppressAnnouncer` core, toast +
  milestone log, install+arm rig-validated solo (commits `7ad9000`/`38917c0`/`1981f70`;
  [SESSION-LIFECYCLE-FINDINGS.md](SESSION-LIFECYCLE-FINDINGS.md) addendum). A /next lane spawned for it on
  this stale entry was torn down as a duplicate. Remaining work is rig-gated only: live 2-player validation,
  and caller→cause attribution in the suppression log — both queue directly behind stall B.
- **Post-rung-3 wiring** (inert overlay toggles, nameplate color-by-SteamID, session-roster event toasts) —
  rung-3-gated by definition; unblocked the moment stall B completes.
- **Joiner-side `drive_add_peer` (symmetric add-peer)** — RULED OUT (run 9): enqueues fine on the Client
  session but crashes it ~10s later (null `+0x5f8`; a Host-session sub-object is null on a Client one). The
  joiner's connection must come from inbound find-or-create, not an outbound driver.
- **`symmetric_peer` as the shipping design** — diagnostic only; leaves both at `Host(3)`, no real type-5.
- **Calling the session-established handler `0x1423f4870` for the client** — builds a `+0x708` artifact that
  crashes the joiner ~30s later at `eldenring.exe+0x3f4860`. The direct bit-2 OR is the safe lever.
- **Fabricating the type-5 message / forcing transport admit** (`force_gatec_accept`) — both ruled out; the
  type-5 needs a real Steam auth ticket, and the admit identity callback rejects the same way in real ERSC.

## Learned Recently (Pointers Only)

- [SESSION-DRIVE.md](SESSION-DRIVE.md) — the full stall-B trail: "★ STALL-B HANDSHAKE AIM SHEET" (two phase
  machines; `WaitInitData` = the session machine), "★ P2P-EVENT + CLIENT-CONNECT AIM SHEET" (the event ring;
  `+0x5a8` = voice), "★ JOINER PEER-WIRE AIM SHEET" (connectionless `ISteamNetworking006`; SteamConnections
  are inbound-only), and the nine "▶ RIG RESULT" addenda (runs 4–9: the P2P callbacks, the accept-unmask, the
  host emitting real SYNs, the joiner add-peer crash).
- Code: `session_probe.rs` — the B0/B1/B4/B5 read-only probes (session-state poll, P2P-callback/event
  tracers, client-connect chain, host send-phase + game `SendP2PPacket`); the `drive_add_peer` driver (host +
  the OFF joiner path); the `host_skip_p2p_accept` accept-unmask lever. Config gates in `unseamless-core/config.rs`.
- **Review is now light (this session).** CLAUDE.md > "Review is light here": experiments get no formal
  review (eyeball); solid work gets `/check` (1 agent) or the new **`/tricheck`** skill (3 agents). Ultracheck
  is not used here. Applied across CLAUDE.md, ORCHESTRATION.md, the fleet skill, and both worker roles.
- **Deck get-into-world footgun fixed (this session).** `deck.sh cycle` now verifies the Deck reached the
  world (waits for the `auto-session` fire line) and re-dismisses if not, instead of firing blind dismiss taps
  and stranding the join at a menu. See the `/steam-deck` skill (`wait-inworld`, `DECK_INWORLD_*`).
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) > "Rig constraints & gotchas" — `kill` before `cycle` (apply refuses over a
  live game → **silent no-op**; hit again this session on run 9 — always kill first).
- [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS" — the two-machine procedure; pass
  the role on every cycle.
- Ground-truth ERSC dumps (persistent, uncommitted): `~/Documents/ersc-session-ref-{host,client}.txt`; the
  full 8-type DLNW3D protocol + type-5 path in
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★★ REFERENCE DUMP #2".
