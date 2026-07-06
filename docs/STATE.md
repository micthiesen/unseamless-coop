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

Last updated: **2026-07-06** (★ COURSE-CORRECTION + run 12. Cross-checking the ERSC captures (Michael: "use
the info I captured") revealed runs 4–11 — this session's 10/11 chasing the transport gate-c / `[S+0x168]`
member-resolve — were a **red herring** (correction #1: that stub is present *even in working ERSC*; the
connection is built by the session-layer pump, not find-or-create). Re-anchored on the **real frontier**
(charted 2026-07-05): the endpoint `member+0x130` is already built by the game's pump; the sole gap is
**completing the DLNW3D handshake** (`member+0x152=1`). Run 12 disproved option (a): with `suppress_syn` the
endpoint still builds (SYN was noise) but `+0x152` never sets — it needs a real **type-5 auth-ticket** message.
Next = option (b): produce the type-5 ourselves (`GetAuthSessionTicket` → framed message → host validator).
The gate-c path stays DO-NOT-RE-LITIGATE.)

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **Host-side rung-3 establishment reproduced; client reaches `Client/WaitInitData`.** Host drives its own
  establish handler to a stable `Host`/`Ingame` with the full member graph; the client's `drive_join` +
  `join_set_established_bit` (direct `container+0x7c0` bit-2 OR) builds the per-peer emitter connection and
  parks at `Client/WaitInitData`, stable, no crash.
- **★★ REAL FRONTIER = complete the DLNW3D handshake (`member+0x152=1`). The endpoint is already built.**
  `drive_add_peer` queues the peer member and **the game's own per-frame pump `0x1424007e0 → 0x142401110 →
  0x14203ef70` builds `member+0x130`** (a live `MTInternalThreadSteamConnection`, vtable `0x143277750`).
  **Run 12 (2026-07-06) confirmed this builds even with our SYN suppressed** — the fabricated 14-byte SYN was
  pure noise. The one gap: member flags reach `(0,1,0,0)` (`+0x151`, set by the endpoint build) and never
  reach working ERSC's `(0,0,1,0)` (`+0x152`); the endpoint cycles build → ~30s drop → rebuild, `players`
  stuck at 1. `+0x152` is set only by a **type-5** message = a real Steam **auth ticket** the host validates;
  the game's own connect flow never produces it in our driven setup (and symmetric mode has no joiner to
  produce it). ⇒ we produce it ourselves (Next).
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

**★ Option (b) — PRODUCE a real type-5 auth-ticket message ourselves to set `member+0x152=1`.** Run 12
(2026-07-06) disproved option (a): with `suppress_syn` on both machines the endpoint `member+0x130` **still
builds** (the game's pump builds it from `drive_add_peer` queuing — our SYN was pure noise), but flags stay
`(0,1,0,0)` and never reach the working-ERSC `(0,0,1,0)` (`+0x152`). The endpoint just cycles build → ~30s
drop → rebuild. So the SYN wasn't blocking anything; the gap is squarely the missing **type-5**. Full: SESSION-DRIVE.md > "▶ RIG RESULT (run 12)".

**Decision (/next, 2026-07-06): split A (delegated static chart) + B (serial scaffold), injection-path first.**
The biggest unknown is the **injection path** — run 12 proved the pump reads type messages from `conn+0x130`
via holder API `0x14203f250`, but *what transport/channel feeds that endpoint's recv* (where we inject a
type-5) is unknown; if a type-5 can only arrive via the game's own connect flow, "produce it ourselves"
becomes "drive that flow." So: **lane `type5-chart`** charts (a) the pump-read `0x14203f250` + endpoint recv
source (the injection point), (b) the validator `0x142402ee0` (token semantics at `member+0x148`, whether
`BeginAuthSession` success is enforced, what the blob must be), (c) the game's own type-5 SEND site (does the
connect flow call `GetAuthSessionTicket`? framing?). Orchestrator **scaffolds B in parallel** (Steam
`GetAuthSessionTicket` binding + a config-gated type-5-producer feature) so the chart's answers drop straight
in. Both land → serial two-machine confirm run (watch `member+0x152`, `players→2`).

**The concrete build (serial, no ERSC restore — everything charted):**
1. On the peer, call Steam **`GetAuthSessionTicket`** (we already bind Steam interfaces for rung-4 in
   `coop/steam.rs`) to get a real auth ticket blob.
2. Construct the type-5 message `{buf[0]=5, 8B token, 4B len (1..0x400), len·blob=ticket}` (payload shape
   charted, ERSC-LIVE-CAPTURE > "★ The DLNW3D connect protocol").
3. Deliver it to the host's pump — send on the built endpoint / the channel the pump reads (`0x14203f250` on
   `conn+0x130`). Host type-5 case `0x142400924` → validator `0x142402ee0` (`BeginAuthSession`) → `conn+0x152=1`
   → member completes → roster → `players=2`.
- **Open sub-questions:** the 8B token semantics (`member+0x148` — echoed value or a session nonce?), and
  **who sends** — a real joiner produces the type-5, so this likely needs one machine in a **joiner role**
  (not the symmetric both-host mode, which has no joiner) OR we relay. First chart the game's own type-5
  *send* path (does its connect flow call `GetAuthSessionTicket`? where?) to copy the exact token/framing,
  rather than guess.
- **Retire the SYN:** `suppress_syn` proved the fabricated 14-byte SYN is unnecessary (endpoint builds
  without it); drop it from the drive to cut channel-30 noise.

**Do NOT re-run the transport gate-c / `[S+0x168]` path** (⚠️ RED HERRING bullet in Now). `players` stuck at
1 is the *handshake type-5* not completing, not a connection failing to build.

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
