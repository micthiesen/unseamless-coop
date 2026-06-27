# Two-Player Friend-Test Runbook

The recipe for the **one** thing the rig can't do alone: a real second player. It is built to move
three rungs in a single friend session —

- **Rung 4** — password-keyed lobby discovery (the joiner-finds-host leg; `CreateLobby` is already
  rig-proven solo).
- **Rung 2** — the private Steam P2P side-channel links over the discovered peer (`coop_connect`
  walks `linking → linked`).
- **Rung 3** — capture the game's own create/join initiation while the friend hosts once and joins
  once, so the session-FSM RE can proceed.

One good friend session pays for all three. Stage everything below **up front** so a single launch
captures the lot; the friend is a low-effort peer (set a password, press Play, host once, join once,
send one file).

Read alongside [COOP-CONNECTION.md](COOP-CONNECTION.md) (the rung model + the Steam decisions) and
[SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) (the rung-3 create/join RE in full). The connection
itself is driven by lobby discovery — there are **no SteamIDs to copy**; both players share the same
password, then one picks **Open World** and the other picks **Join world** from the overlay menu.

## What's already shipped (lean on it)

The prep batch we deferred for this test has landed — don't rebuild it, just turn it on:

- **Per-stage connect report** (`coop_connect` section of the diag dump). Replaces the coarse
  off/linking/linked/lost phase atomic with a timestamped, per-stage `ConnectReport`, so a *failed*
  link is diagnosable from one log without a re-run: own-SteamID resolved? session accepted? messages
  sent vs received (`sent>0, recv=0` = one-way NAT)? handshake landed? version match? and — for rung
  4 — lobby created / found (`candidates`) / joined / host-ID resolved. Identities are `peer_tag`
  pseudonyms, no raw SteamIDs, so the block is safe to share.
- **One-click "Export diagnostics"** (overlay Actions tab). Writes a single shareable file —
  `<ELDEN RING\Game>\unseamless-coop\unseamless-coop-diagnostics.txt` — with the run header + a live
  snapshot + a recent log tail. It reads only local, present-thread-safe sources (never the co-op
  transport), so it captures exactly the **never-connected** case that log-forwarding can't (forward
  needs the link up). Password redacted, every SteamID64 scrubbed to a `peer_tag` — safe to post
  publicly. [README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) already leads the friend to this
  button.

So the friend's whole reporting duty is: press the Export button, send back one file.

## Stage it up front (one seed config, both players)

This run rides the **lobby-discovery build** (the one where rung 4's discovery path is live). In
`scripts/rig/seed-config.toml` (and in the bundle that ships to the friend), set:

- `[session] password`: the **same** value on both players, and now **at least 8 characters**
  (`MIN_PASSWORD_LEN`; the startup guard refuses to launch a shorter one). This is the only pairing
  input: it keys the lobby (`SetLobbyData`/filter via the verbatim SHA-256 `lobby_discovery_token`),
  it **authenticates the peer** (the side-channel handshake now proves password knowledge before
  linking, see below), **and** it derives the session AES key later. (The lobby key/filter is
  `lobby_discovery_token`, a domain-separated SHA-256 over the verbatim password, truncated to 32 hex
  chars, KAT-pinned so the DLL and the harness agree.) Nothing else identifies the peer.

> **Both players MUST set the identical password.** The side-channel now authenticates the peer: each
> side presents a password-keyed proof (`Auth`) and is **not linked** until the other side verifies it.
> A mismatch shows a plain-voice **"Authentication failed … (wrong co-op password)"** banner and the
> peers never link: no config sync, no session actions, no log forwarding. A wrong password is no
> longer a silent non-connect; it names itself.
- `[debug] enabled = true`, `level = "debug"` — so the `session-probe:` / `coop` lines (and the
  rung-3 register dumps, which are `debug!`) are captured.
- `[debug] forward_to_host = true` — once linked, the client tees its log to the host's `LogBundle`
  (client→host only; the manual export is the fallback until the link is up).
- `[debug.probes] session_probe = true` — turns on the rung-3 FSM rising-edge logger + the create/join
  entry hooks (`session-probe:` prefix). See [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md).
- The **friend-capture export** is always available (the overlay button); no flag needed.
- `[debug] guide = "<name>"` — **when this session is to validate something specific, always ship a
  guide.** An in-overlay guide pins the test steps on-screen so the friend(s) **and** you follow the
  exact same sequence (advance with `L3 + D-pad Up`, skip with `L3 + D-pad Down`), instead of a chat
  round-trip. Add it **by hand** to each machine's `unseamless-coop/unseamless_coop.toml` after
  extracting (the `rig.sh package` step doesn't emit these debug keys, and ConfigSync doesn't carry
  them). For `two-player-join` each machine's **role is derived** from its Open/Join action by the
  guide's connect step, so **leave `[debug] rig_role` at the default `solo`** — you no longer set it per
  machine. (It remains an override/solo fallback: set it `host` / `join` only to force a role for a
  guide without a connect step, or to run one leg solo.) For this connect test, `two-player-join` fits;
  for the create RE, `rung3-create-chart`. Their steps **auto-finish off the run log** (`coop: linked`,
  `coop: adopted host config`, the `session-probe:` FSM lines), so the result lands in the
  shareable/forwarded log and the tester is never asked "did it connect?" — that's the whole point of
  staging a guide over a chat round-trip. The whole subsystem is debug-only, so **every machine must
  run a diag build** (release strips it entirely). Authoring + the committed list:
  [RIG-GUIDES.md](RIG-GUIDES.md) + the `rig-guides` skill.

> The `lobby_callback_probe` (rung-4 gate) has already served its purpose solo — it confirmed ER pumps
> via `RunCallbacks` and `CreateLobby` succeeds. It can stay off for the friend test; the live
> discovery driver is what we're exercising now, not the probe.

## Part A — Connect via lobby discovery (rungs 4 + 2)

The connection model: **both share the same password; one opens a world, the other joins.** Co-op is
triggered on demand from the overlay menu, not at launch — the actions stay disabled until Steam
networking is ready and the player is in-game. The host (Open World) creates the password-keyed lobby;
the joiner (Join world) filters the list by the password, finds it, and joins. The role is the user's
**choice**, never derived (only the host creates a lobby). The connection is **repeatable**: a failed
attempt can be retried (Leave, then Open/Join again), and each Open/Join **resets the `coop_connect`
report** so the per-stage diagnostics reflect only the latest attempt, not a stale earlier one.

**The in-game procedure is the `two-player-join` guide — don't hand-relay it.** Stage `[debug] guide =
"two-player-join"` (no per-machine `rig_role` — the connect step derives each machine's role from its
Open/Join action), and both machines are walked through the connect on-screen, each step
**auto-finishing off the run log** so the result is captured, not relayed: the connect step resolves on
the Open/Join action itself, then the rung-2 link milestone (`coop: linked`) and the client's config
adoption (`coop: adopted host config`). The host (Open World) sees the host steps, the joiner (Join
world) the joiner steps, both the shared ones — derived per machine. Just watch the rig side with
`scripts/rig.sh log -f` and have the friend hit Export at the end; the guide drives the rest. The
ordered steps live only in the guide (`crates/unseamless-core/src/guide/guides.rs`), never duplicated
here.

**What the logged stages mean (the guide auto-detects these; this table is the diagnostic reference for
reading the `coop_connect` section of a diag dump, or the live log):**

| Stage | Success looks like | A failure here means |
|---|---|---|
| Own SteamID (rung 1) | `self_id_at` set | rung-1 resolution broke; nothing downstream can run |
| Lobby create (host) | `lobby.role = host`, `created_at` set | `CreateLobby` poll failed (rig-proven solo, so suspect the build) |
| Lobby find (joiner) | `candidates = Some(N≥1)`, `list_returned_at` set | `Some(0)` = empty filter (password mismatch / host not up / version-tag mismatch); `None` = list never returned |
| Lobby join (joiner) | `joined_at` set, `host_id_resolved = true` | joined but couldn't read the owner SteamID (the value that seeds rung 2) |
| Session accept (rung 2) | `session_accepted_at` set | we never `AcceptSessionWithUser`'d the resolved peer |
| Messages | `messages_sent > 0` **and** `messages_received > 0` | `sent>0, recv=0` = one-way NAT (the classic) |
| Handshake | handshake stamped, `coop_connect` → `linked` | partner's `Hello` never landed |
| Auth | peer linked (proof verified) | an **"Authentication failed with <peer> (wrong co-op password)"** banner = the two passwords differ; fix them to match |
| Version | match | version mismatch (confirm both ran the same `build_id`) |

A clean link shows `coop_connect` walking `linking → linked`, an overlay **"Co-op partner connected"**
toast, the client **adopting the host's config**, and (with `forward_to_host`) the host's `LogBundle`
picking up the client's lines.

**Watch the one open Steam question:** messaging two arbitrary SteamIDs over `ISteamNetworkingMessages`
may require the accounts to be **Steam friends** for NAT-punch/auth. If `sent>0, recv=0` persists,
have the two accounts friend each other and retry — that's the most likely fix, and confirming it is
itself a result worth recording.

**Regardless of outcome, have the friend hit Export and send the one file** — the connect report makes
even a *failed* attempt fully diagnosable without a second session.

## Part B — Capture the rung-3 create/join initiation (same session)

The same two connected instances are exactly what rung 3 needs. While you have the friend, chart the
two session-initiation functions — **instrument our side; the friend is just the peer.**

**The on-screen procedure for this leg is the `rung3-create-chart` guide** (the flagship; switch
`[debug] guide` to it for the host-once/join-once leg, or run the friend test in two passes). With
`session_probe = true` staged, its steps drive the host/join and **auto-finish on the FSM signal** —
the rising-edge logger logging the host walking `None → TryToCreateSession → Host` and the joiner
`None → TryToJoinSession → Client`, each frame-stamped, with the live `CSSessionManager @0x…` base
printed once. Have the friend **host once, then join once** so both initiation paths fire on *our*
machine. The guide's ordered steps live only in `guides.rs`; the procedure and outcomes are
[SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — don't duplicate either here.

The orchestrator-side capture is separate from the guide: set a **Frida write-watch on
`&CSSessionManager.lobby_state` (`base + 0xc`)** — the base comes from the `session-probe: FSM live …`
line. The watch reports the instruction that writes `1` (create) / `4` (join); walk up to the
enclosing function prologue → that entry is the hook site. (Full strategy, the store-site AOB fallback,
and the scaffold to fill are in [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) > "Find the two
initiation functions".)

> **Privacy:** the create/join hook register dumps (`rdx`/`r8`/`r9`) may carry a raw peer SteamID64,
> which resolves straight to a Steam profile. Keep those `debug!` lines out of any shared log — scrub
> or `peer_tag` them, exactly as [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) warns. The
> `coop_connect` report and the Export bundle are already scrubbed; the raw `session-probe:` register
> lines are not.

This is behavioral RE — watch *what* writes the state and *which* function the game runs, then
implement our own driver from that. Never paste decompiler output into source/commits
([CLAUDE.md](../CLAUDE.md) > Clean-room hygiene).

## After the session

- **Collect:** the friend's `unseamless-coop-diagnostics.txt` (Export button) + our own rig log.
- **Rung 4/2 verdict:** did `coop_connect` reach `linked`? If not, the per-stage report names the
  failing stage (use the table above). Record whether peers had to be Steam friends.
- **Rung 3 verdict:** did the FSM logger capture both transitions, and did the write-watch point at
  the two initiation entries? If so, fill the `SESSION_CREATE_SITE` / `SESSION_JOIN_SITE` scaffold
  (per SESSION-RE-RUNBOOK) with the landmark AOBs + the confirmed register→meaning mapping, documented
  inline per [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results".
- **Then:** flip on lobby discovery for keeps (rung 4 done), and hand the charted initiation entries
  to the co-op core to *drive* a session (rung 3).

## Cross-references

- [COOP-CONNECTION.md](COOP-CONNECTION.md) — the rung model, the poll-not-pump Steam decisions, the
  lobby-discovery connection model.
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — the rung-3 create/join RE in full (the probe, the
  AOB strategies, the scaffold to fill, the outcomes).
- [RUNTIME-RE.md](RUNTIME-RE.md) — Frida-under-Proton + the diagnostic-DLL tooling for the write-watch.
- [README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) — what the friend sees (install + the
  Export button + the zip-your-logs fallback).
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) — the broader rig procedures + the two-player live verifications
  this session can also knock out (scaling in-combat, player count, session persistence).
