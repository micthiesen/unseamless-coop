# Rung-3 Two-Machine Create-Drive Runbook

> **SUPERSEDED (2026-07-05) — the create-drive/offline-synthesis framing is retired; rung-3 has PIVOTED to
> "let the game establish it" (true ERSC model).** This experiment tested whether a real peer sizes leg B's
> slot array; that (and the whole offline hand-synthesis avenue) is now a proven dead end (3-lane RE: array
> capacity 0 → instant destroy, add-member needs real game handle objects, the `+0x168` accept-callback has
> no static installer). **The current next action is a LIVE-READ capture, not a drive:** attach the
> standalone ptrace watcher `scripts/re/watch-write.py` to a real working **ERSC** host during a genuine
> host+join, and read what the game's own establishment writes at the charted offsets
> (`SessionManagerSteam+0x18/0x20/0x24`, add-member handles, `MTInternalThreadSteamSocket+0x168`,
> `[container+0x48]`), then reproduce that sequence. Full plan: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★
> DECISION (2026-07-05)"; map: [ROADMAP.md](ROADMAP.md) > Wave 2. **The two-machine *mechanics* below
> (`deck.sh`, the config, reading both logs) are still the right harness** for the reproduce-and-validate
> step; only the `drive_create`/slot-array framing is superseded.

The exact, copy-pasteable procedure for the **rung-3 create-drive experiment on two machines** — the
run that answers the one question the solo rig fundamentally can't. Staged now so that the moment a
second machine is available (a friend or the Steam Deck), the orchestrator sets the config below,
launches both sides, and reads the verdict out of the logs; nothing is left to design on the day.

> **Run 1 done (2026-07-03, rig host + Steam Deck joiner): FAIL, unchanged, symmetric.** Rungs 4+2
> linked cleanly (first rig↔Deck link; no Steam-friends requirement), the re-timed drive fired
> post-link on both machines, and both still read `cap=0` / `returned false` /
> `None->FailedToCreateSession` — a live lobby + linked peer does **not** size the slot array (it
> also leaves reject #1's `NetworkSession+0x10` at 0). Full verdict + corollaries:
> [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Two-machine result". **Run 2, the fabricate+peer combo
> (same day): also FAIL** — array fabricated to cap 16 with the peer linked, create still
> `FailedToCreateSession`, symmetric. Every charted gate is clear at the failure point, so the live
> reject is **past the tail's store-capacity check**; the next move is charting that post-store tail
> ([SESSION-DRIVE.md](SESSION-DRIVE.md) > "Combo result"), not another config permutation of this
> runbook.

> **Scope & legitimacy.** Two machines the developer owns (or a friend who owns the game), running a
> co-op mod over a private Steam side-channel, *outside* anti-cheat and never on the official servers.
> "Bypassed gate" = a check flipped in our own in-memory copy so our own co-op create can proceed
> offline — not an anti-cheat defeat and not redistributed. See CLAUDE.md > Safety / legitimacy +
> Clean-room hygiene.

**Why this run exists (the confirmed root cause).** A solo driven create fires and passes *every*
charted gate — the Arxan leg-A availability gate (bypassed), leg B's rejects #1/#2/#3, and the 4th
gate — then dies in **leg B's tail capacity check**: the session-slot array on the `NetworkSession`
is **capacity 0** offline (`cmp count,[rbx+0x20]; jae fail` with `[rbx+0x20]==0`), because no real
match/lobby ever allocated it, so the finished session object has nowhere to be stored. A real peer
(a live rung-4 lobby) is the hypothesized allocator. Full trace + tombstoned dead ends:
[SESSION-DRIVE.md](SESSION-DRIVE.md) > "Why a direct create fails offline"; current status:
[ROADMAP.md](ROADMAP.md) > Rung 3.

**The question the run answers:** does a live rung-4 lobby + a real connected peer size the slot
array (capacity > 0) so the driven create walks `None -> TryToCreateSession -> Host`? Secondary: if
yes, is the lobby *alone* enough, or did the game's own match setup do the sizing (read *when* the
capacity became nonzero, see the log table).

> **This supersedes [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) > Part B for the rung-3 leg.**
> Part B predates the root-cause confirmation (it still frames the blocker as leg B's "registry
> lookup", since corrected to the slot-array capacity check). Parts A (connect) and C (overlay
> trace) stand; for the create-drive procedure and its signatures, use this doc.

## Prerequisite — re-time the driver (code, not staging; do this FIRST)

As of this writing the experiment **cannot** be run meaningfully with the shipped driver:
`SessionCreateDriver::on_frame` (`crates/unseamless-coop/src/session_probe.rs`) is a one-shot that
fires the moment the machine is in-game with an active main player and `lobby_state == None` — i.e.
the **first frame after the save loads**. But the Open/Join actions (and `auto_session`) are
themselves gated on being in-game, so the rung-4 lobby and rung-2 link cannot exist yet when the
drive fires. Run as-is, both machines just reproduce the solo capacity-0 failure and the run answers
nothing.

**Before the run, gate the drive on the connection being up**: keep the one-shot latch, but hold
fire until the side-channel is linked (the `coop: linked` edge / the coop link state), ideally plus a
short settle delay so the Steam lobby + roster are fully live. That is feature code (the
orchestrator's lane or a code worker), not staging. This runbook and the `rung3-create-drive` guide
are written against the corrected ordering; the guide's `drive-watch` step degrades (skip ->
`inspect`) rather than trapping if the drive fired early, and the "drive fired before `coop:
linked`" signature below detects an un-retimed driver in the logs.

## The config (identical on BOTH machines)

Both machines run a **diag build** (the guide engine is debug-only; release strips it) with the same
password and the same probe set. Boot-time patches (the leg-A bypass) read each machine's **local
boot config** — ConfigSync later doesn't apply them retroactively — so every key below must be set on
both sides before launch, not synced after.

| Key | Value | `rig.sh package` flag | Notes |
|---|---|---|---|
| `[session] password` | same on both, >= 8 chars | `--password <pw>` | the only pairing input; a mismatch names itself ("Authentication failed") |
| `[gameplay] bypass_session_create_gate` | `true` | **none — hand-set** | keep **ON**: confirmed prerequisite (the leg-A bypass; without it create dies before leg B) |
| `[gameplay] enable_offline_multiplayer` | `true` | none needed | already the default; leave it on (the create builder consults `is_offline()`) |
| `[debug.probes] session_probe` | `true` | `--session-probe` (but see the append block below) | the FSM rising-edge log (`session-probe:` prefix) |
| `[debug.probes] drive_create` | `true` | **none — hand-set** | the one-shot driven create **and** the leg-B gate tracers |
| `[debug.probes] force_netsession_ready` | `true` | **none — hand-set** | satisfies leg B's reject #1 just before the call |
| `[debug.probes] fabricate_slot_array` | run 1: `false` / combo run: `true` | **none — hand-set** | sizes the slot array at leg-B entry if still empty (the combo variant) |
| `[debug.probes] drive_fire_solo` | `false` | **none — hand-set** | keep the drive holding for the rung-2 link; `true` is only for peerless solo proofs |
| `[debug] guide` | `"rung3-create-drive"` | `--guide rung3-create-drive` | the committed on-screen procedure (below) |
| `[debug] rig_role` | leave default `solo` | none | the guide's connect step **derives** each machine's role from its Open/Join action |
| `[debug] enabled` | `true` | baked by `package` | all the signal lines are `info!`, so the default level suffices (`debug` adds nothing here — the SteamID-bearing register-dump hooks are inert, their sites uncharted) |

Optional / fallback: `[debug] auto_session` starts the connection without the overlay menu. It is
**per-machine, never a seed edit** — the shared seed carries no `auto_session` key (each machine's
cycle re-applies the seed, so a role written there gets clobbered by the other machine's next
cycle). On the rig/Deck pair it's a per-invocation flag: `scripts/rig.sh cycle --auto-session host`
/ `scripts/deck.sh cycle --auto-session join`. For a friend (Windows) bundle it's a package flag
(`rig.sh package --auto-session join`) — the fallback for a machine that must run `--no-overlay`
(`[debug] overlay = false`), which also loses the guide and the Export button (collect its
`unseamless-coop/logs/` by hand — see the privacy note at the bottom).

> **Password hygiene: use a disposable password for this run.** The friend bundle embeds it in its
> seed config and `rig.sh share` publishes that zip on a **public** GitHub prerelease (the zip
> password only gates AV scanners; it's printed in the release notes). And
> `scripts/rig/seed-config.toml` is git-tracked in a public repo — don't commit a real password
> there.

### Machine 1 — the local rig (always one of the two)

Edit `scripts/rig/seed-config.toml`:

```toml
[session]
password = "<the shared password>"   # must equal the other machine's

[gameplay]
bypass_session_create_gate = true    # flip: the seed ships it false

[debug]
guide = "rung3-create-drive"         # the [debug] section's guide key

[debug.probes]
session_probe = true
drive_create = true
force_netsession_ready = true
```

(Those keys all exist in the seed file — flip them in place rather than appending duplicate
sections; a duplicate TOML table header fails the whole parse and the mod falls back to defaults,
which trips the startup password guard.)

Then apply + launch through the rig as usual (see [RIG-RUNBOOK.md](RIG-RUNBOOK.md)):

```bash
scripts/rig.sh apply            # snapshots the real stack, installs the mod + seed config
scripts/rig.sh cycle --in-world # launch and land in a loaded save unattended (~33s)
# headless / two-machine variant: add the per-machine role as a flag (never a seed edit):
#   scripts/rig.sh cycle --in-world --auto-session host
scripts/rig.sh log -f           # watch this machine's log live
```

### Machine 2, option A — a friend (Windows)

Build the bundle **without** `--session-probe` (the append block below writes its own
`[debug.probes]` table; combining both would emit a duplicate header and fail the parse):

```bash
scripts/rig.sh package --guide rung3-create-drive --password <the shared password>
scripts/rig.sh share            # rolling GitHub prerelease; send the friend the link
```

The friend installs per README-FRIENDS.txt, then **appends this block** to
`<ELDEN RING\Game>\unseamless-coop\unseamless_coop.toml` (the probes have no package flags):

```toml
# --- rung-3 create-drive experiment (docs/RUNG3-DRIVE-RUNBOOK.md) ---
[gameplay]
bypass_session_create_gate = true

[debug.probes]
session_probe = true
drive_create = true
force_netsession_ready = true
```

### Machine 2, option B — the Steam Deck

The Deck rides the **same seed config** as the rig (`scripts/deck.sh apply` pushes
`scripts/rig/seed-config.toml`), so the edits from Machine 1 cover both sides automatically —
password included. The per-machine **role** is the exception: it's a per-invocation flag, never a
seed edit (a role in the shared seed gets clobbered when the other machine cycles). Per the
[`/steam-deck`](../.claude/skills/steam-deck/SKILL.md) skill:

```bash
scripts/deck.sh seed-save                   # if the Deck needs a save
scripts/deck.sh cycle --auto-session join   # apply + launch + click into gameplay, join role as a flag
scripts/deck.sh pull-logs                   # collect its logs afterward
```

Order is forgiving: the rig host's lobby stays open once created (only the setup is time-boxed), so
cycle the Deck any time after the host side is up — and re-cycle it freely, passing the role flag
each time.

## The in-game procedure (the guide drives it — don't hand-relay)

The ordered steps live **only** in the committed `rung3-create-drive` guide
(`crates/unseamless-core/src/guide/guides.rs`; authoring conventions:
[RIG-GUIDES.md](RIG-GUIDES.md) + the `rig-guides` skill) — never duplicated here. Both machines run
the same guide; each machine's role is **derived** by the standard connect step from its own action
(one picks **Open World**, the other **Join world**, in the overlay Actions tab), the connect/link
steps auto-finish off the run log, and the drive itself is hands-off — the (re-timed) one-shot
driver fires on its own once linked and in-world, and the guide's watch step auto-branches on the
captured outcome, so the verdict lands in the shareable/forwarded log instead of being relayed. At
the end both machines hit **Actions tab -> Export diagnostics** and send the file back.

Two staging notes:

- **Identical-config runs (the Deck path):** the joiner's "settings synced" step only auto-fires
  when the host's synced settings actually *differ* from the joiner's own; with both machines on
  the same seed config the line may never log, and that step is skipped manually (it never traps).
- **Host vs joiner:** the config is identical; the machines differ only in the overlay action they
  pick (which the guide turns into its role). With `drive_create` on both sides, **both machines
  drive create** — each logs its own outcome, and both logs matter (a host-side pass with a
  joiner-side fail is itself a datum: role/lobby-ownership asymmetry in what sizes the array).

## Log lines to watch (each machine; `grep session-probe:` gets the whole story)

| When | Line (stable fragment) | Meaning |
|---|---|---|
| boot | `patched 'bypass_session_create_gate':` | the leg-A veto bypass took. **Absent, or a `patch 'bypass_session_create_gate': landmark not found` warning ⇒ AOB drift — abort the run** |
| boot | `session-probe: gate-trace hooked legb-entry at 0x…` (+ `create-gate4`) | the leg-B tracers are in place (`drive_create` on) |
| in-world | `session-probe: FSM live @frame N — CSSessionManager @0x… lobby=None` | the FSM logger sees the live manager |
| connect | `coop: linked` (both) / `coop: adopted host config` (joiner) | rungs 4+2 up — the drive precondition |
| drive | `session-probe: drive-create — NetworkSession+0x10 (reject#1 flag) = N before create` then `forced NetworkSession+0x10 = 1` | the reject-#1 lever fired |
| drive | `session-probe: drive-create @frame N — calling create wrapper 0x…` | the one-shot call is going out |
| in-call | `session-probe: gate-trace legb-entry REACHED — … slot-array [+0x20]cap=C [+0x24]count=N` | **the headline datum: `cap`.** Solo baseline is `cap=0`; `cap>0` with a peer = the hypothesis confirmed |
| in-call | `session-probe: gate-trace create-gate4 REACHED (rejects #1-3 passed) — …` | the 4th gate's config fields, populated in-world (illustrative: the 2026-06-29 solo run read `35000/5000/[6,30000,…]`, the `6` being that rig config's `max_players`) |
| result | `session-probe: drive-create returned <bool> — lobby_state now <state>` | the driver's verdict line (the guide branches on it) |
| result | `session-probe: FSM @frame N lobby None-><state> protocol …` | the FSM transition trace; on success expect `None->TryToCreateSession` then `->Host`, with `protocol` advancing toward `Ingame` |
| anomaly | `session-probe: drive-create skipped — lobby_state is <state>, need None` | something already moved the FSM before the drive |

## Success vs failure signatures

- **PASS** — `legb-entry … cap>0`, `drive-create returned true`, lobby `None -> TryToCreateSession`
  (then `Host`), `protocol_state` advancing toward `Ingame`. A real peer sizes the slot array;
  rung-3 create is **unblocked**. Next: the join leg (a join-side driver), the password-derived AES
  key, wiring the driver into the real Open/Join actions, and sourcing the menu's `SessionContext`
  bits from the FSM ([SESSION-DRIVE.md](SESSION-DRIVE.md) > "Ordering").
- **FAIL, unchanged** — `cap=0`, `returned false`, `None -> FailedToCreateSession` on both machines:
  a live rung-4 lobby + peer alone does **not** size the array. Next: chart what does (the game's
  own match setup — protocol reference: vswarte's `waygate-server`; the local-clone pointer is in
  [COOP-CONNECTION.md](COOP-CONNECTION.md) > Rung 3), or weigh the fabricate-the-array fallback
  (risky — SESSION-DRIVE.md > "Paths forward").
- **PARTIAL** — `cap>0` but `returned false`: the peer sized the array yet the tail still failed
  elsewhere; that's a *new* reject past everything charted. Capture both machines' full logs and
  hand back for a fresh leg-B tail pass.
- **VOID (driver not re-timed)** — the `drive-create` lines appear **before** `coop: linked` in the
  log: the drive fired at world-load, pre-lobby (see the Prerequisite section). The run tested
  nothing; fix the driver timing and re-run.
- **Asymmetry** — one machine passes, the other doesn't: record which role passed; it localizes the
  sizing to lobby ownership vs membership. Also a real result.

Whatever the outcome, **collect everything**: the friend's Export file (or `deck.sh pull-logs`) +
the rig's own log, then record the verdict + any new finding in SESSION-DRIVE.md per
[CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results".

> **Privacy:** a raw log carries your **own** SteamID64 even at the default `info` level (the
> `steam: own SteamID …` line), so hand-collected raw logs (`deck.sh pull-logs`, the `--no-overlay`
> fallback) are **not** scrubbed — prefer the **Export** bundle, which redacts the password and
> scrubs decimal-form SteamIDs. (The peer-SteamID register dumps
> [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) warns about cannot occur in this run: the
> create/join entry-hook sites are uncharted (`None`), so no such line is emitted at any level.)

## Cross-references

- [SESSION-DRIVE.md](SESSION-DRIVE.md) — the drive call spec, the capacity-0 root cause, paths
  forward; the doc this runbook operationalizes.
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — the (done) initiation-function RE; kept for
  re-derivation after a game update, and the source of the `session-probe:` line meanings.
- [FRIEND-TEST-RUNBOOK.md](FRIEND-TEST-RUNBOOK.md) — the friend-session frame (Part A connect flow,
  the `coop_connect` stage table, Export); its Part B is superseded by this doc.
- [RIG-GUIDES.md](RIG-GUIDES.md) + the `rig-guides` skill — the guide engine + authoring API behind
  `rung3-create-drive`.
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) / the [`/steam-deck`](../.claude/skills/steam-deck/SKILL.md)
  skill — driving the local rig and the Deck.
