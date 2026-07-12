# State

Fast-moving work state and chosen next step. This records the work, not live workers, rig state, or
uncommitted changes. Durable findings live in the linked docs.

Last updated: **2026-07-12** (worker-thread registrar implemented and solo-proven).

## Now

- **Rungs 1, 2, and 4 plus the Steam P2P transport are shipped and two-machine-proven.** Password-keyed
  lobby discovery resolves the peer, the private side-channel authenticates, and legacy game P2P exchanges
  packets bidirectionally by SteamID64.
- **The rung-3 session graph reaches a stable `Host`/`Ingame` session with one remote member endpoint.**
  `drive_add_peer` queues the peer; the game's pump builds the real `MTInternalThreadSteamConnection` at
  `member+0x130`; the synthetic SYN is suppressed because it is unnecessary noise.
- **The type-5 producer and wire format are solved.** The hand-frame sender uses a real cached Steam auth
  ticket and the charted 11-bit transport header. Prior two-machine runs proved each peer's type-5 crosses
  Steam and reaches the receiver's channel-30 admit path. Driving the game's protected sender remains retired.
- **The delivery-side registrar is implemented behind `register_peer_connection` and passes locally.** At
  the transient endpoint set, the probe forms the descriptor using the native builder's field mapping; at
  the next `0x142640bc0` worker entry it calls `0x14263fd10`. Runtime result: free `5 -> 4`, active `0 -> 1`,
  correct connection vtable/owner/peer key, and no crash. A raw contiguous endpoint copy is invalid because
  it puts `endpoint+0x58` in callable descriptor `+0x38`.
- **The only solo retirement is expected remote timeout.** With the Deck offline, the manager removes the
  valid connection after 6.57 seconds with `EP2PSessionError::Timeout` (reason 4). The remaining unknown is
  downstream behavior with a responding peer, not local construction or manager insertion.

## Next

**Run the registrar lever with the Deck online.** This is orchestrator-serial and directly gates rung 3;
no independent code lane outranks it. Use the existing symmetric two-machine configuration with
`register_peer_connection`, `send_type5`, and `instrument_type5_recv` enabled on both machines.

Acceptance on each receiver: `peer-register SUCCESS`; active connection remains while peer traffic is live;
`TYPE5-VALIDATOR FIRED`; then `member+0x152=1` and the session observer reports `players=2`. If an online peer
still retires with reason 4, inspect legacy P2P accept state before changing the descriptor. If the validator
fires without completion, inspect `BeginAuthSession` ticket timing and identity. Plan and evidence:
[SESSION-DRIVE.md](SESSION-DRIVE.md) > "Worker-Thread Registrar Result (2026-07-12)".

## Candidates Not Chosen

- **Batch stay-connected two-player validation into the same Deck session after `players=2`.** Small and
  serial, but it cannot precede the registrar acceptance chain.
- **Post-rung-3 adoption sweep** (session toggles, identity-keyed nameplate color, presence surfaces). This is
  delegable once real two-player state exists, but implementing against `players=1` would not validate it.
- **More static registrar/descriptor RE.** The native call contract, endpoint mapping, insertion, service loop,
  and timeout cleanup are already observed. More static work does not answer the responding-peer question.
- **ERSC factory capture or gate-c/member-lookup work.** Ruled out: `0x14263b720` creates the manager, not a
  peer connection, and `[context+0x168]` is the reject stub even in a working ERSC session.

## Learned Recently

- **Registrar chart, descriptor failure/correction, local success, and timeout cleanup** ->
  [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Correction and Local Runtime Lead (2026-07-12)" and
  "Worker-Thread Registrar Result (2026-07-12)".
- **Current rung-3 gating map and historical dead ends** -> [ROADMAP.md](ROADMAP.md) > "Rung 3".
- **Reproducible runtime inspection** -> `scripts/re/peek-socketmgr.py`, `catch-endpoint.py`,
  `watch-write.py`, and `watch-bt.py`; the watchpoint helpers stop every traced thread before clearing DR7.
- **Code on `main`** -> commit `4371c3c` (`register_peer_connection`, worker-thread registrar, diagnostics,
  RE helper hardening, and seed configuration).
