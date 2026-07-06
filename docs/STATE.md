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

Last updated: **2026-07-05** (/next: the inbound-path Next stands — static charting of the three suspects
delegated to a fleet lane; the two-machine confirm run stays orchestrator-serial. A second lane for the
seamlessness gate was spawned on a stale premise and torn down: **that gate already shipped 2026-07-04 as
`gameplay.stay_connected`**, see Candidates. Ground state below is the 2026-07-05 late-night wrap: stall B
walked down across NINE two-machine runs to the **joiner's INBOUND path** — the host emits real DLNW3D
SYNs, but the joiner's game never builds the host connection from them.)

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **Host-side rung-3 establishment reproduced; client reaches `Client/WaitInitData`.** Host drives its own
  establish handler to a stable `Host`/`Ingame` with the full member graph; the client's `drive_join` +
  `join_set_established_bit` (direct `container+0x7c0` bit-2 OR) builds the per-peer emitter connection and
  parks at `Client/WaitInitData`, stable, no crash.
- **★ Stall B is now narrowed to the joiner's INBOUND transport.** Host-side `drive_add_peer 0x1423fdc80`
  queues the joiner and the **host's game emits real 14-byte DLNW3D SYNs** to it (`GAME-SENDP2P 0x142640b20`);
  with bidirectional accept the host also receives the joiner's packets, and the host's real
  connection-creator `TAG1-CONNSTATUS 0x1423fe350` fires. **But the JOINER never builds the host connection**
  — its worker `0x142640bc0` drains channel 30 with `connections_span=0` and its find-or-create `0x142640e30`
  never fires on the host's inbound SYNs — so it parks at `WaitInitData` and times out (~30s; the "INIT gate"
  `0x1423fbe10` is a countdown, not a data gate).
- **Two leads killed this session (don't re-tread):** `session+0x5a8` (vtable `0x1431fa918`) is
  `DLNR3D::VoiceChatSteam`, not a connection object — `0x142401e80` is the voice pump, not a peer dial (runs
  5/6). And the joiner must **not** drive add-peer itself: it enqueues but crashes the Client session ~10s
  later (run 9, ACCESS_VIOLATION reading null `+0x5f8`, add-peer in the backtrace).
- **The type-5 completion protocol is banked** (ERSC capture #2): completion = a type-5 DLNW3D message (Steam
  auth ticket via `GetAuthSessionTicket`) the host validates with `BeginAuthSession` (`0x142402ee0`) →
  `member+0x152=1`. Dumps at `~/Documents/ersc-session-ref-{host,client}.txt`.

## Next

**★ Run the decisive joiner-inbound splitter (config-ready; blocked only on a free rig), then act on it.**
The static chart landed (SESSION-DRIVE.md > "★ JOINER INBOUND AIM SHEET"): suspect 1 (channel) is ruled
out, and suspects 2 (P2P accept) + 3 (member registration) are ONE gate — the member-resolve
`[socketmgr+0x40]=0x142639d00` fails for the host on the joiner's Client session, and it guards BOTH the
accept path `0x1426408b0` and find-or-create `0x142640e30`. The host is never registered in the joiner's
transport member collection (`S=[socketmgr+0x48]`, `S+0x170`/`S+0x98`); `drive_add_peer` on the joiner is
the wrong collection (`SessionSteam+0x4f0`) AND crashes (run 9).

**The splitter run needs NO new code — the current `main` seed already runs it:** on the joiner,
`drive_p2p` already force-accepts the host (`unmask` is host-only), and `install_host_accept_trace` installs
the find-or-create entry (`0x142640e30`) + resolve gate-c (`0x142640ecd`) + success (`0x142640ee4`) hooks
**role-independently**, so they observe the JOINER's worker. Just run the two-machine cycle with roles:
`rig.sh cycle --auto-session host` + `deck.sh cycle --auto-session join` (Deck reachable at
`deck@10.10.1.57:2222`, applied), then read the **Deck** log:
- find-or-create entry fires + gate-c logs **REJECT** (rax≠0) ⇒ **suspect 3** (member registration is the
  wall) ⇒ next = register the host in the joiner's `ManagerImpl` member collection (aim-sheet lever 3a:
  chart the member-add that writes `S+0xa0`/`S+0x170`, sibling of the lookup vmethod `[S+0x168]`; 3b =
  latch the Arxan-decoded add target live if it's dispatch-only). Predicted most-likely per the ranking.
- entry fires + gate-c **ACCEPT** + success `0x142640ee4` fires ⇒ **suspect 2** was the wall (accept) ⇒
  the connection builds (`conn+0x138=hostID`), init-data flows, `session+0x3cc→2`, `players=2`.
(`force_gatec_accept` stays OFF for this run — we observe the real verdict; forcing rax=0 alone is
insufficient anyway since the success path then calls the null finisher `[local+0x60]`.)

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
