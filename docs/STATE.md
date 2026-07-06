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

Last updated: **2026-07-06** (RUN 11 — real-vs-stub SETTLED live. The member-lookup vmethod **`[S+0x168]`
IS the stub `0x1423fdf00`** (always not-found); the collection is NON-empty (has members); the live resolve
callback is `0x142639810` (not the statically-charted `0x142639d00`). So **lever 3a (register a member) is
INERT** — the stub ignores the collection — and **lever 3b (install a REAL `[S+0x168]` lookup) is the path.**
Prior: run 10 confirmed the member-resolve is the wall (gate-c REJECT ×6, host-admit-SUCCESS never fires) +
a 2nd wall (host→joiner delivery dead). Full: SESSION-DRIVE.md > "▶ RIG RESULT (run 10/11)".)

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **Host-side rung-3 establishment reproduced; client reaches `Client/WaitInitData`.** Host drives its own
  establish handler to a stable `Host`/`Ingame` with the full member graph; the client's `drive_join` +
  `join_set_established_bit` (direct `container+0x7c0` bit-2 OR) builds the per-peer emitter connection and
  parks at `Client/WaitInitData`, stable, no crash.
- **★ Stall B root = a STUB member-lookup on our stood-up context (run 11 settled it).** The joiner's real
  SYNs reach the host's find-or-create `0x142640e30`; it hits the member-resolve `[mgr+0x40]=0x142639810`,
  which dispatches to the lookup vmethod **`[S+0x168]` = the stub `0x1423fdf00`** (`mov eax,1; ret`, always
  not-found), so the resolve REJECTS (gate-c returns 1) and `host-admit-SUCCESS 0x142640ee4` never fires ⇒ no
  connection ⇒ `players` stays 1. **The member collection is NON-empty** (`[S+0x98..0xa0]` span 0x28, `[S+0x170]`
  non-null) — so the wall is the stub lookup, not a missing member. `drive_add_peer` (writes `SessionSteam+0x4f0`)
  AND aim-sheet lever 3a (register in `S+0x170`) are BOTH inert against a stub that ignores the collection.
  **Second wall (run 10):** host→joiner P2P delivery is dead — the joiner force-accepts but its worker
  `0x142640bc0` drains channel 30 empty and find-or-create never fires (0 RECV), so it parks at `WaitInitData`
  and times out (~30s). See SESSION-DRIVE.md > "★ JOINER INBOUND AIM SHEET" + "▶ RIG RESULT (run 10/11)".
- **Two leads killed this session (don't re-tread):** `session+0x5a8` (vtable `0x1431fa918`) is
  `DLNR3D::VoiceChatSteam`, not a connection object — `0x142401e80` is the voice pump, not a peer dial (runs
  5/6). And the joiner must **not** drive add-peer itself: it enqueues but crashes the Client session ~10s
  later (run 9, ACCESS_VIOLATION reading null `+0x5f8`, add-peer in the backtrace).
- **The type-5 completion protocol is banked** (ERSC capture #2): completion = a type-5 DLNW3D message (Steam
  auth ticket via `GetAuthSessionTicket`) the host validates with `BeginAuthSession` (`0x142402ee0`) →
  `member+0x152=1`. Dumps at `~/Documents/ersc-session-ref-{host,client}.txt`.

## Next

**★ Lever 3b — install a REAL `[S+0x168]` member-lookup on our stood-up socket-manager context.** Run 11
settled real-vs-stub: `[S+0x168]` is the stub `0x1423fdf00`, so 3a is dead. Our context `S` (built by
`stand_up_transport` + the establish handler) never got the real member-lookup wired because the online
member-service init that installs it doesn't run offline. Make `[S+0x168]` a real lookup over the
already-populated collection → resolve finds the peer → gate-c ACCEPT → `host-admit-SUCCESS 0x142640ee4`
fires → host builds the joiner connection → roster-add `0x140cb31b0` grows `players` 1→2.

**Structure charted (member-add-chart lane, integrated):** `S = [socketmgr+0x48]` is a
`DLNR3D::ManagerImplSteam`; the registry is a **delegate pair** `[S+0x168]` (lookup fn-ptr) + `[S+0x170]`
(bound container), **both zeroed at ctor and bound at session-establish** — the step our driven join skips.
Members are `SessionMemberSteam` (0x170B) minted **only** by the Arxan factory `0x1423fdf20` from two
refcounted identity handles — so there is **no cheap `insert(S, u64)`** and no static writer of `[S+0x168]`
(the bind is Arxan/off-image). Full: SESSION-DRIVE.md > "★ MEMBER-ADD WRITER".

**The concrete lever-3b step (serial, needs a real-ERSC capture — the strategic fork; checkpoint with
Michael before restoring his real stack):** the real `[S+0x168]` delegate + its bind are only observable at
runtime on a real ERSC session. Capture plan (same shape as the banked type-5 capture):
1. `rig.sh restore` the real ERSC stack; run real ERSC and open a co-op session (solo host may suffice if
   the member-service binds at host-open; else Deck as an ERSC peer).
2. Walk `CSSessionManager → container → [container+0x708]=socketmgr → [socketmgr+0x48]=S` via `/proc/mem`
   peek (same static singleton RVA), and read the **real `[S+0x168]`** (a `.text` fn — a fixed RVA we can
   then install) + the shape of `[S+0x170]`.
3. Latch the bind call site / the factory `0x1423fdf20` args live (the `/reverse-engineer` "Arxan-decoded
   call targets" pattern) to learn *how* the delegate is installed and how a member is minted.
4. Back on our mod: install the captured real `[S+0x168]` (+ a valid `[S+0x170]`) on our stood-up `S`, or
   drive the captured bind — watch gate-c flip 1→0, `host-admit-SUCCESS 0x142640ee4` fire, `players` 1→2.
- **Wall 2 (host→joiner delivery, run 10):** still open (joiner's find-or-create never fires, 0 RECV despite
  force-accept). Likely downstream of the member-lookup fix; re-test after 3b lands, else chart the joiner's
  `P2PSessionRequest_t`/accept path. `force_gatec_accept` stays OFF (the stub never fills `[local+0x60]`).
  Deck at `deck@10.10.1.57:2222`.

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
