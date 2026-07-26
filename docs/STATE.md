# State

Fast-moving work state and chosen next step. This records the work, not live workers, rig state, or
uncommitted changes. Durable findings live in the linked docs.

Last updated: **2026-07-13** (native auth and two-player game roster proven on rig plus Deck).

## Now

- **Rungs 1, 2, and 4 plus the Steam P2P transport are shipped and two-machine-proven.** Password-keyed
  lobby discovery resolves the peer, the private side-channel authenticates, and legacy game P2P exchanges
  packets bidirectionally by SteamID64.
- **Rung 3 reaches the two-player game roster on both real machines.** Rig and Deck each held
  `Host`/`Ingame`, validated the other peer's Steam ticket, posted the native type-1 roster event, and
  changed from `players=1` to `players=2` within the same second. Repeated diagnostics held at two and both
  processes stayed alive.
- **The native type-5 path is solved end to end.** The frame task prepares plaintext type 5; manager worker
  `0x142640bc0` sends it through `FsdpConnection` slot 6 only in connected state 3. Native delivery
  `0x142644600`, route adapter `0x14263cf50`, endpoint receiver `0x14203f850`, pump dispatch, and
  `BeginAuthSession` are all runtime-proven.
- **Synthetic descriptor reuse is retired.** The native descriptor owns a session-lifetime Fsdp context;
  retaining it after native teardown dereferences cleared state. Once the native descriptor appears, the
  probe suppresses its synthetic registrar. The endpoint callback's five-argument ABI also cannot be used
  directly by the generic four-argument worker.
- **The last roster blocker was the add-peer suppress flag.** `flag=1` copied to `member+0x151`, causing
  completion phase `0x142400f40` to skip the type-1 session event even though auth set `member+0x152=1`.
  `flag=0` enables the event and immediately grows the roster.

## Next

**Verify in-world presence and control in a two-machine session — and make that check objective
instead of eyeball-only.** This is the last gate before rung 3 counts as playable: does the proven
`players=2` roster actually put a remote character in the world, and does its movement replicate? Two
observation gaps blocked answering that autonomously, and both are being closed now:

- *(delegated, worker `presence-probe`)* a `[debug.probes]` phantom-presence probe that logs the
  `player_chr_set` roster — count, handle, `chr_load_status`, position — plus spawn/despawn edges.
  Position precision matters: the movement check is "inject input on one machine, diff the remote
  phantom's position on the other".
- *(serial, orchestrator)* screenshot capture off gamescope's nested Xwayland in `rig.sh`/`deck.sh`,
  so a visual claim is an image rather than a request for Michael's eyes.

Then the two-machine run itself (serial: rig + Deck). If presence works, the next implementation chunk
is replacing configured debug peer ids and symmetric probe roles with the peer and Open/Join lifecycle
supplied by rungs 4 and 2. If the roster is present but the character is not, instrument the post-roster
game packets and the `join_wait`/ChrIns spawn transition rather than revisiting transport or auth.
Evidence: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Native Transport and Two-Player Roster Result
(2026-07-13)".

## Candidates Not Chosen

- **Productize the proven rung-3 path immediately.** Highest-value implementation after the presence check,
  but doing it before knowing whether the roster produces a visible peer would hide the remaining gate.
- **Harden orphaned lobby cleanup after a killed test process.** A forced-kill cycle left the private
  side-channel reporting an old world still open, while rung 3 proceeded through configured peer ids. This
  is a real lifecycle issue, but it does not invalidate the native session result and is smaller than
  Open/Join integration. See
  [COOP-CONNECTION.md](COOP-CONNECTION.md) > "Rung 4".
- **Stay-connected and area-transition validation.** Now unblocked and suitable for the same two-player rig,
  but only after basic presence and movement are confirmed.
- **Remove the dense RE probes.** Keep them until Open/Join productization reproduces `players=2`; then retain
  only bounded milestone logging and the re-derivation comments.

## Learned Recently

- **Native descriptor, Fsdp sender, receive chain, auth result, roster flag correction, and two-machine
  success** -> [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Native Transport and Two-Player Roster Result
  (2026-07-13)".
- **Current rung-3 gating map and historical dead ends** -> [ROADMAP.md](ROADMAP.md) > "Rung 3".
- **Forced-kill lobby cleanup residual** -> [COOP-CONNECTION.md](COOP-CONNECTION.md) > "Rung 4".
- **Reproducible runtime inspection** -> `scripts/re/peek-socketmgr.py`, `catch-endpoint.py`,
  `watch-write.py`, and `watch-bt.py`; the watchpoint helpers stop every traced thread before clearing DR7.
- **Prior code on `main`** -> commit `4371c3c` (`register_peer_connection`, worker-thread registrar,
  diagnostics, RE helper hardening, and seed configuration).
