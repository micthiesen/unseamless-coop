# State

Fast-moving work state and chosen next step. This records the work, not live workers, rig state, or
uncommitted changes. Durable findings live in the linked docs.

Last updated: **2026-07-25** (presence check run: the roster reaches 2, no character spawns — the gap
is join-wait -> spawn).

## Now

- **Rungs 1, 2, and 4 plus the Steam P2P transport are shipped and two-machine-proven.** Password-keyed
  lobby discovery resolves the peer, the private side-channel authenticates, and legacy game P2P exchanges
  packets bidirectionally by SteamID64.
- **Rung 3 reaches the two-player game roster, now reproduced twice on two real machines** (2026-07-13
  and 2026-07-25). Rig and Deck each hold `Host`/`Ingame`, validate the other peer's Steam ticket, post
  the native type-1 roster event, and change `players=1` -> `players=2`; scaling switches to two-player
  rates and both processes stay alive.
- **`players=2` does NOT mean a player is in the world — that's this session's headline.** With both
  characters in the same block ~4.6m apart, `player_chr_set` held **zero phantoms on both machines** for
  90+ seconds with no spawn edges, and the remote roster entry read `join_wait=true`. The peer is admitted
  and authenticated but never leaves join-wait.
- **The native type-5 path is solved end to end.** The frame task prepares plaintext type 5; manager worker
  `0x142640bc0` sends it through `FsdpConnection` slot 6 only in connected state 3. Native delivery
  `0x142644600`, route adapter `0x14263cf50`, endpoint receiver `0x14203f850`, pump dispatch, and
  `BeginAuthSession` are all runtime-proven.
- **The rig can now see and be seen.** `rig.sh shot` captures the game's frame, and the `presence-probe`
  probes report the in-world phantom roster — so presence questions are answered from logs and images
  instead of from someone watching the screen.

## Next

**Chart the post-roster `join_wait` -> `ChrIns` spawn transition.** This is the one thing between a
session that *counts* two players and a session where you can *see* the other player, and the presence
run localized it precisely: transport, auth, and the roster are each proven twice on two machines, so
**don't re-litigate them.** The lever is whatever clears `join_wait` and drives the spawn — what the game
does after roster-add in a real session, and what reads that flag. Serial (rig + Deck + live RE).
Evidence: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "In-World Presence Result (2026-07-25)".

Batch probes into one rig session: a two-machine run costs ~5 minutes before the roster converges (see
the operational note in that section — a short polling window reads as a false failure).

## Candidates Not Chosen

- **Productize the proven rung-3 path** (replace debug peer ids + symmetric probe roles with the rung-4
  peer and Open/Join lifecycle). Still the highest-value implementation chunk, but a session that reaches
  `players=2` with nobody in the world isn't worth productizing yet. Do it once join-wait clears.
- **Unify the two unsafe `ChrSetEntry` walks.** `presence_probe::walk_entries` and
  `native_nameplates::active_characters` are two implementations of the same unsafe walk with different
  safety properties (raw `u8` status vs. materialized discriminants). Worth one shared audited helper;
  touches nameplates' unsafe, so it wasn't done mid-run. Delegable.
- **Harden orphaned lobby cleanup after a killed test process.** A forced-kill cycle left the private
  side-channel reporting an old world still open. A real lifecycle issue, but it doesn't invalidate the
  native session result. See [COOP-CONNECTION.md](COOP-CONNECTION.md) > "Rung 4".
- **Stay-connected and area-transition validation.** Suitable for the same two-player rig, but pointless
  until a peer actually appears in the world.
- **Remove the dense RE probes.** Keep them until Open/Join productization reproduces `players=2`; then
  retain only bounded milestone logging and the re-derivation comments.

## Learned Recently

- **Roster 2 does not imply presence; `join_wait=true` is the stuck flag**, plus the ~4-minute roster
  convergence note -> [SESSION-DRIVE.md](SESSION-DRIVE.md) > "In-World Presence Result (2026-07-25)".
- **`rig.sh shot` screenshots the game** via gamescope's own `gamescopectl screenshot` — an X11 grab of
  the nested display returns black *by construction*, since gamescope never paints the composited game
  into the Xwayland root -> [RIG-RUNBOOK.md](RIG-RUNBOOK.md) > "Screenshotting the Game".
- **`docs/SPECTATE.md` > "Rig asks" #1 is still unanswered.** The look-at-phantom lever shipped but never
  fired — it needs a loaded phantom to aim at, and there wasn't one. It answers itself on the first spawn.
- **`scripts/fleet/msg` now takes `-` for stdin**, matching `worker-new`; previously the documented
  heredoc form silently delivered the literal `-` -> the `fleet` skill > "Message A Worker".
- **Native descriptor, Fsdp sender, receive chain, auth result, and the roster flag correction** ->
  [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Native Transport and Two-Player Roster Result (2026-07-13)".
- **Current rung-3 gating map and historical dead ends** -> [ROADMAP.md](ROADMAP.md) > "Rung 3".
- **Reproducible runtime inspection** -> `scripts/re/peek-socketmgr.py`, `catch-endpoint.py`,
  `watch-write.py`, and `watch-bt.py`; the watchpoint helpers stop every traced thread before clearing DR7.
