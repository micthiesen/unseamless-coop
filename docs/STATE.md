# State

Fast-moving work state and chosen next step. This records the work, not live workers, rig state, or
uncommitted changes. Durable findings live in the linked docs.

Last updated: **2026-07-25** (presence check run: roster reaches 2, no character spawns — the gap is
join-wait -> spawn).

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

**Chart the post-roster `join_wait` -> `ChrIns` spawn transition.** The presence check ran and came
back negative in a useful way: `players=2` on both machines, both characters in the same block ~4.6m
apart, and **zero phantoms in `player_chr_set` on either side** for 90+ seconds, no spawn edges. The
remote roster entry reads `join_wait=true`, so the game admits and authenticates the peer but never
leaves join-wait — which is exactly why no character spawns. Transport, auth, and the roster are each
now proven twice on two machines; **don't re-litigate them.** The lever is whatever clears `join_wait`
and drives the spawn: find what the game sends/does after roster-add in a real session, and what reads
that flag. Serial (rig + Deck + live RE). Evidence:
[SESSION-DRIVE.md](SESSION-DRIVE.md) > "In-World Presence Result (2026-07-25)".

Rig cost is ~5 minutes per two-machine run before the roster converges (see the operational note in
that section) — batch probes into a single session rather than cycling per question.

## Candidates Not Chosen

- **Productize the proven rung-3 path immediately** (replace debug peer ids + symmetric probe roles with
  the rung-4 peer and Open/Join lifecycle). Still the highest-value implementation chunk, and now
  *unblocked* in the sense that the roster is twice-proven — but a session that reaches `players=2` with
  nobody in the world isn't yet worth productizing. Do it once join-wait clears.
- **Unify the two unsafe `ChrSetEntry` walks.** `presence_probe::walk_entries` and
  `native_nameplates::active_characters` are now two implementations of the same unsafe walk with
  different safety properties (raw `u8` status vs. materialized discriminants). Worth one shared audited
  helper; deliberately not done mid-run, and it touches nameplates' unsafe. Delegable.
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

- **Roster 2 does not imply presence; `join_wait=true` is the stuck flag** ->
  [SESSION-DRIVE.md](SESSION-DRIVE.md) > "In-World Presence Result (2026-07-25)". Same section carries
  the ~4-minute convergence note (a 2-minute poll wrongly reads as failure).
- **`rig.sh shot` screenshots the game** (gamescope's own `gamescopectl screenshot`; an X11 grab of the
  nested display returns black by construction) -> [RIG-RUNBOOK.md](RIG-RUNBOOK.md) > "Screenshotting the
  Game".
- **`docs/SPECTATE.md` > "Rig asks" #1 is still unanswered.** The look-at-phantom lever shipped but never
  fired this run — it needs a loaded phantom to aim at, and there wasn't one.

- **Native descriptor, Fsdp sender, receive chain, auth result, roster flag correction, and two-machine
  success** -> [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Native Transport and Two-Player Roster Result
  (2026-07-13)".
- **Current rung-3 gating map and historical dead ends** -> [ROADMAP.md](ROADMAP.md) > "Rung 3".
- **Forced-kill lobby cleanup residual** -> [COOP-CONNECTION.md](COOP-CONNECTION.md) > "Rung 4".
- **Reproducible runtime inspection** -> `scripts/re/peek-socketmgr.py`, `catch-endpoint.py`,
  `watch-write.py`, and `watch-bt.py`; the watchpoint helpers stop every traced thread before clearing DR7.
- **Prior code on `main`** -> commit `4371c3c` (`register_peer_connection`, worker-thread registrar,
  diagnostics, RE helper hardening, and seed configuration).
